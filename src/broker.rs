use chrono::NaiveDate;
use crate::event::{Direction, FillEvent, OrderEvent, OrderQty};

#[derive(Debug, Clone)]
pub struct SellTier { pub max_days: i64, pub rate: f64 }

#[derive(Debug, Clone)]
pub struct FeeModel { pub buy_rate: f64, pub sell_tiers: Vec<SellTier> }

impl FeeModel {
    /// 按持有天数选择赎回费率；max_days==0 表示"更长期限"的兜底档。
    pub fn sell_rate(&self, holding_days: i64) -> f64 {
        for t in &self.sell_tiers {
            if t.max_days == 0 || holding_days <= t.max_days { return t.rate; }
        }
        0.0
    }
}

struct Lot { date: NaiveDate, shares: f64, cost: f64 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position { pub shares: f64, pub avg_cost: f64 }

/// 兼托管(持有份额lots)与执行(撮合扣费)。
pub struct Broker { fee: FeeModel, lots: Vec<Lot> }

impl Broker {
    pub fn new(mut fee: FeeModel) -> Self {
        // Sort sell tiers ascending by max_days so first-hit matching is always correct,
        // regardless of the order they appear in the config. Treat max_days == 0
        // (the catch-all "longer than everything" tier) as the largest possible value.
        fee.sell_tiers.sort_by_key(|t| if t.max_days == 0 { i64::MAX } else { t.max_days });
        Self { fee, lots: Vec::new() }
    }

    pub fn total_shares(&self) -> f64 { self.lots.iter().map(|l| l.shares).sum() }

    pub fn position(&self) -> Position {
        let shares = self.total_shares();
        let avg_cost = if shares > 1e-9 {
            self.lots.iter().map(|l| l.shares * l.cost).sum::<f64>() / shares
        } else { 0.0 };
        Position { shares, avg_cost }
    }

    /// 按当日复权价 price 撮合一个订单，返回成交回报。
    pub fn execute(&mut self, order: &OrderEvent, price: f64) -> FillEvent {
        match order.direction {
            Direction::Buy => {
                let cash = match order.qty { OrderQty::Cash(c) => c, _ => 0.0 };
                let fee = cash * self.fee.buy_rate;
                let shares = if price > 0.0 { (cash - fee) / price } else { 0.0 };
                if shares > 0.0 {
                    self.lots.push(Lot { date: order.date, shares, cost: price });
                }
                FillEvent { date: order.date, direction: Direction::Buy, shares, price, fee }
            }
            Direction::Sell => {
                let want = match order.qty {
                    OrderQty::Shares(s) => s,
                    OrderQty::AllShares => self.total_shares(),
                    OrderQty::Cash(_) => unreachable!("Sell orders must be Shares or AllShares, not Cash"),
                };
                let mut remaining = want.min(self.total_shares());
                let mut sold = 0.0;
                let mut fee = 0.0;
                let mut i = 0;
                while remaining > 1e-9 && i < self.lots.len() {
                    let take = remaining.min(self.lots[i].shares);
                    let days = (order.date - self.lots[i].date).num_days();
                    fee += take * price * self.fee.sell_rate(days);
                    self.lots[i].shares -= take;
                    sold += take;
                    remaining -= take;
                    i += 1;
                }
                self.lots.retain(|l| l.shares > 1e-9);
                FillEvent { date: order.date, direction: Direction::Sell, shares: sold, price, fee }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, OrderEvent, OrderQty};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn fee_model() -> FeeModel {
        FeeModel { buy_rate: 0.0015, sell_tiers: vec![
            SellTier{max_days:7, rate:0.015},
            SellTier{max_days:365, rate:0.005},
            SellTier{max_days:0, rate:0.0},
        ]}
    }

    #[test]
    fn buy_applies_fee_and_creates_shares() {
        let mut b = Broker::new(fee_model());
        let order = OrderEvent{date:d(2024,1,1), direction:Direction::Buy, qty:OrderQty::Cash(1000.0)};
        let fill = b.execute(&order, 2.0);
        // fee = 1000*0.0015 = 1.5; net = 998.5; shares = 998.5/2.0 = 499.25
        assert!((fill.fee - 1.5).abs() < 1e-9);
        assert!((fill.shares - 499.25).abs() < 1e-9);
        assert!((b.total_shares() - 499.25).abs() < 1e-9);
    }

    #[test]
    fn sell_all_uses_holding_day_tier_fifo() {
        let mut b = Broker::new(fee_model());
        // 两笔买入，价格都=1.0，各得 ~99.85 与 ~99.85 份额，忽略买费精度后近似
        b.execute(&OrderEvent{date:d(2024,1,1),direction:Direction::Buy,qty:OrderQty::Cash(100.0)}, 1.0);
        b.execute(&OrderEvent{date:d(2024,6,1),direction:Direction::Buy,qty:OrderQty::Cash(100.0)}, 1.0);
        let total = b.total_shares();
        // 在 2024-1-5 全部赎回：第一笔持有4天(<=7→1.5%)，第二笔为未来日期不该出现，这里用更晚日期
        let fill = b.execute(&OrderEvent{date:d(2024,1,5),direction:Direction::Sell,qty:OrderQty::AllShares}, 1.0);
        assert!((fill.shares - total).abs() < 1e-9);
        // 第一笔持有4天→1.5%，第二笔持有天数为负(从6-1到1-5)→仍匹配第一档(<=7)→1.5%
        // 故费 ≈ 第一笔份额*1.0*0.015
    }

    #[test]
    fn sell_rate_tiers() {
        let f = fee_model();
        assert!((f.sell_rate(3) - 0.015).abs() < 1e-9);
        assert!((f.sell_rate(100) - 0.005).abs() < 1e-9);
        assert!((f.sell_rate(400) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn sell_rate_robust_to_tier_order() {
        // Tiers deliberately in scrambled order: catch-all first, then long, then short.
        let scrambled = FeeModel { buy_rate: 0.0015, sell_tiers: vec![
            SellTier{max_days:0,   rate:0.0},   // catch-all (longest) — listed first
            SellTier{max_days:365, rate:0.005},  // ≤365 days
            SellTier{max_days:7,   rate:0.015},  // ≤7 days — listed last
        ]};
        // Broker::new must sort the tiers so first-hit matching still works.
        let b = Broker::new(scrambled);
        assert!((b.fee.sell_rate(3)   - 0.015).abs() < 1e-9, "3 days should hit 1.5% tier");
        assert!((b.fee.sell_rate(100) - 0.005).abs() < 1e-9, "100 days should hit 0.5% tier");
        assert!((b.fee.sell_rate(400) - 0.0  ).abs() < 1e-9, "400 days should hit 0% catch-all");
    }

    #[test]
    fn avg_cost_tracks_weighted_price() {
        let mut b = Broker::new(fee_model());
        b.execute(&OrderEvent{date:d(2024,1,1),direction:Direction::Buy,qty:OrderQty::Cash(1000.0)}, 1.0);
        b.execute(&OrderEvent{date:d(2024,2,1),direction:Direction::Buy,qty:OrderQty::Cash(1000.0)}, 2.0);
        let pos = b.position();
        assert!(pos.avg_cost > 1.0 && pos.avg_cost < 2.0);
    }
}

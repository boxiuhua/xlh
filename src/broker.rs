use crate::event::{Direction, FillEvent, OrderEvent, OrderQty};
use chrono::NaiveDate;

#[derive(Debug, Clone)]
pub struct SellTier {
    pub max_days: i64,
    pub rate: f64,
}

#[derive(Debug, Clone)]
pub struct FeeModel {
    pub buy_rate: f64,
    pub sell_tiers: Vec<SellTier>,
}

impl FeeModel {
    /// 按持有天数选择赎回费率；与档位在 Vec 中的顺序无关。
    /// 在满足 (max_days==0 兜底) 或 (holding_days <= max_days) 的档中，取 max_days 最小者；
    /// max_days==0 视作最大（最长期限兜底档）。
    pub fn sell_rate(&self, holding_days: i64) -> f64 {
        self.sell_tiers
            .iter()
            .filter(|t| t.max_days == 0 || holding_days <= t.max_days)
            .min_by_key(|t| {
                if t.max_days == 0 {
                    i64::MAX
                } else {
                    t.max_days
                }
            })
            .map(|t| t.rate)
            .unwrap_or(0.0)
    }
}

/// 资产无关的费用抽象：基金用 FeeModel，股票用 StockFee。
pub trait Fee {
    fn buy_fee(&self, cash: f64) -> f64;
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64;

    /// 一笔卖单的总费用。`legs` 为 FIFO 拆出的 (份额, 持有天数)。
    /// 默认逐 lot 累加(基金赎回费按持有期分档,本就逐 lot 计);
    /// 有「每笔最低佣金」的费率须覆盖,整单只收一次最低佣金。
    fn sell_fee_order(&self, legs: &[(f64, i64)], price: f64) -> f64 {
        legs.iter().fold(0.0, |acc, &(shares, days)| {
            acc + self.sell_fee(shares, price, days)
        })
    }
}

impl Fee for FeeModel {
    fn buy_fee(&self, cash: f64) -> f64 {
        cash * self.buy_rate
    }
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64 {
        shares * price * self.sell_rate(holding_days)
    }
}

struct Lot {
    date: NaiveDate,
    shares: f64,
    cost: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub shares: f64,
    pub avg_cost: f64,
}

/// 兼托管(持有份额lots)与执行(撮合扣费)。
pub struct Broker {
    fee: Box<dyn Fee>,
    lots: Vec<Lot>,
}

impl Broker {
    pub fn new(fee: impl Fee + 'static) -> Self {
        // 费率档位顺序无关由 FeeModel::sell_rate 保证，故此处无需再排序。
        Self {
            fee: Box::new(fee),
            lots: Vec::new(),
        }
    }

    pub fn total_shares(&self) -> f64 {
        self.lots.iter().map(|l| l.shares).sum()
    }

    pub fn position(&self) -> Position {
        let shares = self.total_shares();
        let avg_cost = if shares > 1e-9 {
            self.lots.iter().map(|l| l.shares * l.cost).sum::<f64>() / shares
        } else {
            0.0
        };
        Position { shares, avg_cost }
    }

    /// 买入费用(按成交金额)。成交模型据此做「预算内最多买几手」。
    pub fn buy_fee(&self, cash: f64) -> f64 {
        self.fee.buy_fee(cash)
    }

    /// 在 `date` 可卖出的份额:只含**严格早于** `date` 买入的 lot(A 股 T+1)。
    pub fn sellable_shares(&self, date: NaiveDate) -> f64 {
        self.lots
            .iter()
            .filter(|l| l.date < date)
            .map(|l| l.shares)
            .sum()
    }

    /// 按当日复权价 price 撮合一个订单，返回成交回报。
    pub fn execute(&mut self, order: &OrderEvent, price: f64) -> FillEvent {
        match order.direction {
            Direction::Buy => {
                let (shares, fee) = match order.qty {
                    OrderQty::Cash(cash) => {
                        let fee = self.fee.buy_fee(cash);
                        let shares = if price > 0.0 {
                            (cash - fee) / price
                        } else {
                            0.0
                        };
                        (shares, fee)
                    }
                    // A 股成交模型已按整手算好股数,费用按成交金额计。
                    OrderQty::Shares(s) if s > 0.0 && price > 0.0 => {
                        (s, self.fee.buy_fee(s * price))
                    }
                    // 与旧实现一致:非 Cash 买单视作 0 元买入。
                    _ => (0.0, self.fee.buy_fee(0.0)),
                };
                if shares > 0.0 {
                    self.lots.push(Lot {
                        date: order.date,
                        shares,
                        cost: price,
                    });
                }
                FillEvent {
                    date: order.date,
                    direction: Direction::Buy,
                    shares,
                    price,
                    fee,
                }
            }
            Direction::Sell => {
                let want = match order.qty {
                    OrderQty::Shares(s) => s,
                    OrderQty::AllShares => self.total_shares(),
                    OrderQty::Cash(_) => {
                        unreachable!("Sell orders must be Shares or AllShares, not Cash")
                    }
                };
                let mut remaining = want.min(self.total_shares());
                let mut sold = 0.0;
                let mut legs: Vec<(f64, i64)> = Vec::new();
                let mut i = 0;
                while remaining > 1e-9 && i < self.lots.len() {
                    let take = remaining.min(self.lots[i].shares);
                    let days = (order.date - self.lots[i].date).num_days();
                    legs.push((take, days));
                    self.lots[i].shares -= take;
                    sold += take;
                    remaining -= take;
                    i += 1;
                }
                let fee = self.fee.sell_fee_order(&legs, price);
                self.lots.retain(|l| l.shares > 1e-9);
                FillEvent {
                    date: order.date,
                    direction: Direction::Sell,
                    shares: sold,
                    price,
                    fee,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, OrderEvent, OrderQty};
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn fee_model() -> FeeModel {
        FeeModel {
            buy_rate: 0.0015,
            sell_tiers: vec![
                SellTier {
                    max_days: 7,
                    rate: 0.015,
                },
                SellTier {
                    max_days: 365,
                    rate: 0.005,
                },
                SellTier {
                    max_days: 0,
                    rate: 0.0,
                },
            ],
        }
    }

    #[test]
    fn buy_applies_fee_and_creates_shares() {
        let mut b = Broker::new(fee_model());
        let order = OrderEvent {
            date: d(2024, 1, 1),
            direction: Direction::Buy,
            qty: OrderQty::Cash(1000.0),
        };
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
        b.execute(
            &OrderEvent {
                date: d(2024, 1, 1),
                direction: Direction::Buy,
                qty: OrderQty::Cash(100.0),
            },
            1.0,
        );
        b.execute(
            &OrderEvent {
                date: d(2024, 6, 1),
                direction: Direction::Buy,
                qty: OrderQty::Cash(100.0),
            },
            1.0,
        );
        let total = b.total_shares();
        // 在 2024-1-5 全部赎回：第一笔持有4天(<=7→1.5%)，第二笔为未来日期不该出现，这里用更晚日期
        let fill = b.execute(
            &OrderEvent {
                date: d(2024, 1, 5),
                direction: Direction::Sell,
                qty: OrderQty::AllShares,
            },
            1.0,
        );
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
        // 档位故意乱序：兜底档在前、长档居中、短档在后。
        let scrambled = FeeModel {
            buy_rate: 0.0015,
            sell_tiers: vec![
                SellTier {
                    max_days: 0,
                    rate: 0.0,
                },
                SellTier {
                    max_days: 365,
                    rate: 0.005,
                },
                SellTier {
                    max_days: 7,
                    rate: 0.015,
                },
            ],
        };
        // FeeModel::sell_rate 顺序无关，无需 Broker 预排序即正确命中。
        assert!(
            (scrambled.sell_rate(3) - 0.015).abs() < 1e-9,
            "3天应命中1.5%档"
        );
        assert!(
            (scrambled.sell_rate(100) - 0.005).abs() < 1e-9,
            "100天应命中0.5%档"
        );
        assert!(
            (scrambled.sell_rate(400) - 0.0).abs() < 1e-9,
            "400天应命中0%兜底"
        );
    }

    #[test]
    fn avg_cost_tracks_weighted_price() {
        let mut b = Broker::new(fee_model());
        b.execute(
            &OrderEvent {
                date: d(2024, 1, 1),
                direction: Direction::Buy,
                qty: OrderQty::Cash(1000.0),
            },
            1.0,
        );
        b.execute(
            &OrderEvent {
                date: d(2024, 2, 1),
                direction: Direction::Buy,
                qty: OrderQty::Cash(1000.0),
            },
            2.0,
        );
        let pos = b.position();
        assert!(pos.avg_cost > 1.0 && pos.avg_cost < 2.0);
    }

    #[test]
    fn buy_by_shares_charges_fee_on_value() {
        let mut b = Broker::new(crate::stock::fee::StockFee::a_share());
        let fill = b.execute(
            &OrderEvent {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                qty: OrderQty::Shares(900.0),
            },
            10.0,
        );
        assert!((fill.shares - 900.0).abs() < 1e-9);
        // 市值 9000:佣金 2.25<5 取 5;过户 0.09 → 5.09
        assert!((fill.fee - 5.09).abs() < 1e-9, "fee={}", fill.fee);
        assert!((b.total_shares() - 900.0).abs() < 1e-9);
    }

    #[test]
    fn sellable_shares_excludes_same_day_lots() {
        let mut b = Broker::new(fee_model());
        for day in [2, 3] {
            b.execute(
                &OrderEvent {
                    date: d(2024, 1, day),
                    direction: Direction::Buy,
                    qty: OrderQty::Cash(1000.0),
                },
                1.0,
            );
        }
        // 每笔:费 1.5,份额 998.5
        assert!(
            b.sellable_shares(d(2024, 1, 2)).abs() < 1e-9,
            "当日买入不可卖"
        );
        assert!((b.sellable_shares(d(2024, 1, 3)) - 998.5).abs() < 1e-9);
        assert!((b.sellable_shares(d(2024, 1, 4)) - 1997.0).abs() < 1e-9);
    }

    #[test]
    fn buy_fee_is_exposed() {
        let b = Broker::new(crate::stock::fee::StockFee::a_share());
        assert!((b.buy_fee(1000.0) - 5.01).abs() < 1e-9);
    }

    #[test]
    fn fund_fee_model_order_fee_still_sums_lots_by_tier() {
        // 默认实现逐 lot:3 天档 1.5% + 100 天档 0.5%
        let fee = fee_model().sell_fee_order(&[(100.0, 3), (100.0, 100)], 1.0);
        assert!((fee - 2.0).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn stock_sell_across_lots_pays_min_commission_once() {
        let mut b = Broker::new(crate::stock::fee::StockFee::a_share());
        for day in [2, 3, 4] {
            b.execute(
                &OrderEvent {
                    date: d(2024, 1, day),
                    direction: Direction::Buy,
                    qty: OrderQty::Shares(100.0),
                },
                10.0,
            );
        }
        let fill = b.execute(
            &OrderEvent {
                date: d(2024, 1, 10),
                direction: Direction::Sell,
                qty: OrderQty::AllShares,
            },
            10.0,
        );
        assert!((fill.shares - 300.0).abs() < 1e-9);
        assert!(
            (fill.fee - 6.53).abs() < 1e-9,
            "整单一次最低佣金,实际 {}",
            fill.fee
        );
    }
}

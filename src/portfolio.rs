use chrono::NaiveDate;
use crate::broker::Position;
use crate::event::{Direction, FillEvent, MarketEvent, OrderEvent, OrderQty, SignalAmount, SignalEvent};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquityPoint { pub date: NaiveDate, pub equity: f64, pub contribution: f64 }

pub struct Portfolio {
    pub cash: f64,
    pub total_contributed: f64,
    pub curve: Vec<EquityPoint>,
    /// 外部现金流(投入为负、期末市值为正)，供 XIRR。
    pub flows: Vec<(NaiveDate, f64)>,
    initial_cash: f64,
    last_recorded_contributed: f64,
}

impl Portfolio {
    pub fn new(initial_cash: f64) -> Self {
        Self {
            cash: initial_cash,
            total_contributed: initial_cash,
            curve: Vec::new(),
            flows: Vec::new(),
            initial_cash,
            last_recorded_contributed: 0.0,
        }
    }

    /// 在首个交易日登记初始投入现金流。
    pub fn seed(&mut self, date: NaiveDate) {
        if self.initial_cash > 0.0 {
            self.flows.push((date, -self.initial_cash));
        }
    }

    fn contribute(&mut self, amount: f64, date: NaiveDate) {
        self.cash += amount;
        self.total_contributed += amount;
        self.flows.push((date, -amount));
    }

    /// 将策略信号转为确定订单，做基本风控；无效/无仓返回 None。
    pub fn on_signal(&self, sig: &SignalEvent, pos: &Position, today: &MarketEvent) -> Option<OrderEvent> {
        match (sig.direction, sig.amount) {
            (Direction::Buy, SignalAmount::Cash(c)) if c > 0.0 =>
                Some(OrderEvent { date: sig.date, direction: Direction::Buy, qty: OrderQty::Cash(c) }),
            (Direction::Sell, SignalAmount::AllOut) if pos.shares > 1e-9 =>
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::AllShares }),
            (Direction::Sell, SignalAmount::Ratio(r)) if pos.shares > 1e-9 && r > 0.0 => {
                let s = pos.shares * r.min(1.0);
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::Shares(s) })
            }
            (Direction::Sell, SignalAmount::Cash(c)) if pos.shares > 1e-9 && c > 0.0 && today.adj_nav > 0.0 => {
                let s = (c / today.adj_nav).min(pos.shares);
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::Shares(s) })
            }
            _ => None,
        }
    }

    /// 应用成交：买入不足现金自动注入投入；卖出加回现金。
    pub fn apply_fill(&mut self, fill: &FillEvent) {
        match fill.direction {
            Direction::Buy => {
                let needed = fill.shares * fill.price + fill.fee;
                if self.cash < needed {
                    let deficit = needed - self.cash;
                    self.contribute(deficit, fill.date);
                }
                self.cash -= needed;
            }
            Direction::Sell => {
                self.cash += fill.shares * fill.price - fill.fee;
            }
        }
    }

    /// 记录当日权益与"当日新增投入"。
    pub fn record_equity(&mut self, date: NaiveDate, shares: f64, price: f64) {
        let equity = self.cash + shares * price;
        let contribution = self.total_contributed - self.last_recorded_contributed;
        self.last_recorded_contributed = self.total_contributed;
        self.curve.push(EquityPoint { date, equity, contribution });
    }

    /// 回测结束登记期末市值为正向现金流（XIRR 用）。
    pub fn finalize(&mut self, date: NaiveDate, equity: f64) {
        self.flows.push((date, equity));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Position;
    use crate::event::{Direction, FillEvent, MarketEvent, SignalAmount, SignalEvent, OrderQty};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn buy_with_zero_cash_auto_contributes() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        // 一次买入成交：100 份额 @ 价1.0，费 1.5
        let fill = FillEvent{date:d(2024,1,1), direction:Direction::Buy, shares:100.0, price:1.0, fee:1.5};
        p.apply_fill(&fill);
        // 需要 100*1.0+1.5 = 101.5 现金，初始0 → 注入101.5
        assert!((p.total_contributed - 101.5).abs() < 1e-9);
        assert!((p.cash).abs() < 1e-9);
    }

    #[test]
    fn sell_adds_proceeds_to_cash() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        p.apply_fill(&FillEvent{date:d(2024,1,1),direction:Direction::Buy,shares:100.0,price:1.0,fee:0.0});
        p.apply_fill(&FillEvent{date:d(2024,2,1),direction:Direction::Sell,shares:100.0,price:1.5,fee:0.0});
        // 卖得 150 现金
        assert!((p.cash - 150.0).abs() < 1e-9);
    }

    #[test]
    fn signal_to_order_conversion() {
        let p = Portfolio::new(0.0);
        let today = MarketEvent{date:d(2024,1,1), nav:1.0, adj_nav:1.0};
        let pos = Position{shares:100.0, avg_cost:1.0};
        let buy = p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Buy,amount:SignalAmount::Cash(500.0)}, &pos, &today).unwrap();
        assert_eq!(buy.qty, OrderQty::Cash(500.0));
        let sell = p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Sell,amount:SignalAmount::AllOut}, &pos, &today).unwrap();
        assert_eq!(sell.qty, OrderQty::AllShares);
        // 无持仓时卖出 → None
        let empty = Position{shares:0.0, avg_cost:0.0};
        assert!(p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Sell,amount:SignalAmount::AllOut}, &empty, &today).is_none());
    }

    #[test]
    fn equity_curve_records_value_and_contribution() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        p.apply_fill(&FillEvent{date:d(2024,1,1),direction:Direction::Buy,shares:100.0,price:1.0,fee:0.0});
        p.record_equity(d(2024,1,1), 100.0, 1.0);
        let pt = p.curve.last().unwrap();
        assert!((pt.equity - 100.0).abs() < 1e-9);   // cash0 + 100*1.0
        assert!((pt.contribution - 100.0).abs() < 1e-9); // 当日注入100
    }
}

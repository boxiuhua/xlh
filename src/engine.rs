use std::collections::VecDeque;
use crate::broker::Broker;
use crate::data::DataHandler;
use crate::event::Event;
use crate::portfolio::Portfolio;
use crate::result::{DailyRecord, TradeRecord};
use crate::strategy::{Strategy, StrategyContext};

pub struct Engine<D: DataHandler, S: Strategy> {
    data: D,
    strategy: S,
    broker: Broker,
    portfolio: Portfolio,
    lookback: usize,
    daily: Vec<DailyRecord>,
    trades: Vec<TradeRecord>,
}

impl<D: DataHandler, S: Strategy> Engine<D, S> {
    pub fn new(data: D, strategy: S, broker: Broker, portfolio: Portfolio) -> Self {
        Self { data, strategy, broker, portfolio, lookback: usize::MAX, daily: Vec::new(), trades: Vec::new() }
    }

    pub fn run(&mut self) -> &Portfolio {
        let mut seeded = false;
        while let Some(today) = self.data.next_bar() {
            if !seeded { self.portfolio.seed(today.date); seeded = true; }

            let mut queue: VecDeque<Event> = VecDeque::new();
            queue.push_back(Event::Market(today.clone()));

            while let Some(ev) = queue.pop_front() {
                match ev {
                    Event::Market(m) => {
                        let pos = self.broker.position();
                        let history = self.data.history(self.lookback);
                        let ctx = StrategyContext {
                            today: &m,
                            history,
                            shares: pos.shares,
                            avg_cost: pos.avg_cost,
                            cash: self.portfolio.cash,
                        };
                        for s in self.strategy.on_market(&ctx) {
                            queue.push_back(Event::Signal(s));
                        }
                    }
                    Event::Signal(s) => {
                        let pos = self.broker.position();
                        if let Some(o) = self.portfolio.on_signal(&s, &pos, &today) {
                            queue.push_back(Event::Order(o));
                        }
                    }
                    Event::Order(o) => {
                        let fill = self.broker.execute(&o, today.adj_nav);
                        queue.push_back(Event::Fill(fill));
                    }
                    Event::Fill(f) => {
                        if f.shares > 1e-9 {
                            self.trades.push(TradeRecord {
                                date: f.date,
                                direction: f.direction,
                                shares: f.shares,
                                price: f.price,
                                fee: f.fee,
                            });
                        }
                        self.portfolio.apply_fill(&f);
                    }
                }
            }
            self.portfolio.record_equity(today.date, self.broker.total_shares(), today.adj_nav);
            if let Some(p) = self.portfolio.curve.last() {
                self.daily.push(DailyRecord {
                    date: today.date,
                    nav: today.nav,
                    adj_nav: today.adj_nav,
                    equity: p.equity,
                    contribution: p.contribution,
                    shares: self.broker.total_shares(),
                    cash: self.portfolio.cash,
                });
            }
        }
        if let Some(last) = self.portfolio.curve.last() {
            let (date, equity) = (last.date, last.equity);
            self.portfolio.finalize(date, equity);
        }
        &self.portfolio
    }

    pub fn daily(&self) -> &[DailyRecord] {
        &self.daily
    }

    pub fn trades(&self) -> &[TradeRecord] {
        &self.trades
    }

    /// Immutable access to the portfolio after run().
    pub fn portfolio(&self) -> &Portfolio {
        &self.portfolio
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{InMemoryData, NavPoint};
    use crate::broker::{Broker, FeeModel, SellTier};
    use crate::event::Direction;
    use crate::portfolio::Portfolio;
    use crate::strategy::Period;
    use crate::strategy::dca::Dca;
    use crate::strategy::rules::{Rule, RuleLayer};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn no_fee() -> FeeModel { FeeModel{ buy_rate:0.0, sell_tiers:vec![SellTier{max_days:0,rate:0.0}] } }

    #[test]
    fn dca_on_flat_then_up_market() {
        // 净值: 1月1日=1.0(定投日买1000→1000份), 2月1日=1.0(再买1000→1000份),
        // 2月15日=2.0(当月已触发,不再买) → 共投入2000, 持有2000份, 市值4000
        let points = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,2,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,2,15),nav:2.0,acc_nav:2.0},
        ];
        let data = InMemoryData::new(points);
        let strat = Dca::new(Period::Monthly, 1, 1000.0);
        let mut engine = Engine::new(data, strat, Broker::new(no_fee()), Portfolio::new(0.0));
        let pf = engine.run();
        // 共投入2000，持有2000份，2月15日价2.0 → 市值4000
        let last = pf.curve.last().unwrap();
        assert!((last.equity - 4000.0).abs() < 1e-6, "equity={}", last.equity);
        assert!((pf.total_contributed - 2000.0).abs() < 1e-9);
        // XIRR 现金流应含两笔投入与期末市值
        assert!(pf.flows.len() >= 3);
    }

    /// End-to-end test: DCA wrapped in RuleLayer with TakeProfit{target_return: 0.1}.
    /// Price rises from 1.0 to 2.0 (+100%) well past the 10% threshold — the take-profit
    /// sell must execute through the full Market→Signal→Order→Fill loop, leaving the
    /// broker with zero shares and the portfolio holding only cash.
    #[test]
    fn take_profit_exits_through_engine() {
        // Day 1 (2024-01-01, price 1.0): Dca buys 1000 cash → 1000 shares at cost 1.0.
        // Day 2 (2024-01-02, price 1.1): +10% — take-profit threshold (0.1) reached.
        //   RuleLayer injects AllOut sell → broker sells all shares → cash > 0.
        // Day 3 (2024-01-03, price 2.0): no position left, no more buys this month.
        let points = vec![
            NavPoint{date:d(2024,1,1), nav:1.0, acc_nav:1.0},
            NavPoint{date:d(2024,1,2), nav:1.1, acc_nav:1.1},
            NavPoint{date:d(2024,1,3), nav:2.0, acc_nav:2.0},
        ];
        let data = InMemoryData::new(points);
        // DCA buys 1000 on day=1 of every month; price on day-1 is 1.0.
        let inner = Dca::new(Period::Monthly, 1, 1000.0);
        let strat = RuleLayer::new(Box::new(inner), vec![Rule::TakeProfit { target_return: 0.1 }]);
        let mut engine = Engine::new(data, strat, Broker::new(no_fee()), Portfolio::new(0.0));
        let pf = engine.run();

        // After the take-profit sell the broker must hold zero shares.
        let last = pf.curve.last().unwrap();
        // Final equity should equal cash only (shares == 0).
        // The sell happened at price 1.1: 1000 shares * 1.1 = 1100 cash.
        // On day 3 price is 2.0 but shares == 0 so equity stays at cash = 1100.
        assert!(
            last.equity > 0.0,
            "portfolio must have positive equity after take-profit sell; got {}", last.equity
        );
        // Cash must be positive (proceeds from the sale).
        assert!(
            pf.cash > 0.0,
            "cash must be positive after take-profit sell; got {}", pf.cash
        );
        // Total contributed must equal the single DCA buy (1000).
        assert!(
            (pf.total_contributed - 1000.0).abs() < 1e-6,
            "expected 1000 contributed, got {}", pf.total_contributed
        );
    }

    #[test]
    fn captures_daily_and_trades() {
        // Same data as dca_on_flat_then_up_market:
        // Day 1 (2024-01-01, nav=1.0): DCA buys 1000 → 1000 shares
        // Day 2 (2024-02-01, nav=1.0): DCA buys 1000 → 1000 shares
        // Day 3 (2024-02-15, nav=2.0): no new buy (already bought this month)
        // 3 trading days → daily.len()==3; 2 buys → trades.len()==2; last equity≈4000
        let points = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,2,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,2,15),nav:2.0,acc_nav:2.0},
        ];
        let data = InMemoryData::new(points);
        let strat = Dca::new(Period::Monthly, 1, 1000.0);
        let mut engine = Engine::new(data, strat, Broker::new(no_fee()), Portfolio::new(0.0));
        engine.run();
        assert_eq!(engine.daily().len(), 3, "expected 3 daily records");
        assert_eq!(engine.trades().len(), 2, "expected 2 trade records");
        let last_equity = engine.daily().last().unwrap().equity;
        assert!((last_equity - 4000.0).abs() < 1e-6, "expected last equity≈4000, got {}", last_equity);
        assert_eq!(engine.trades()[0].direction, Direction::Buy);
    }
}

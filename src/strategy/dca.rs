use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{Period, Schedule, Strategy, StrategyContext};

pub struct Dca { schedule: Schedule, amount: f64 }

impl Dca {
    pub fn new(period: Period, day: u32, amount: f64) -> Self {
        Self { schedule: Schedule::new(period, day), amount }
    }
}

impl Strategy for Dca {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        if self.schedule.due(ctx.today.date) {
            vec![SignalEvent {
                date: ctx.today.date,
                direction: Direction::Buy,
                amount: SignalAmount::Cash(self.amount),
            }]
        } else { Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount};
    use crate::strategy::{Period, StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn buys_fixed_amount_on_schedule_day() {
        let mut s = Dca::new(Period::Monthly, 1, 1000.0);
        let today = MarketEvent{date:d(2024,1,1),nav:1.0,adj_nav:1.0};
        let ctx = StrategyContext{today:&today, history:std::slice::from_ref(&today), shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].direction, Direction::Buy);
        assert_eq!(sigs[0].amount, SignalAmount::Cash(1000.0));
    }

    #[test]
    fn no_signal_off_schedule() {
        let mut s = Dca::new(Period::Monthly, 15, 1000.0);
        let today = MarketEvent{date:d(2024,1,3),nav:1.0,adj_nav:1.0};
        let ctx = StrategyContext{today:&today, history:std::slice::from_ref(&today), shares:0.0, avg_cost:0.0, cash:0.0};
        assert!(s.on_market(&ctx).is_empty());
    }
}

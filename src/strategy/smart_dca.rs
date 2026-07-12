use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{moving_average, Period, Schedule, Strategy, StrategyContext};

pub struct SmartDca {
    schedule: Schedule,
    base: f64,
    ma_window: usize,
    k: f64, // 偏离敏感系数：越跌买越多
}

impl SmartDca {
    pub fn new(period: Period, day: u32, base: f64, ma_window: usize, k: f64) -> Self {
        Self { schedule: Schedule::new(period, day), base, ma_window, k }
    }
}

impl Strategy for SmartDca {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        if !self.schedule.due(ctx.today) { return Vec::new(); }
        // 偏离度用**最近已公布净值(T-1)**，不是当日净值 —— 下单时今天的净值还没出来。
        let amount = match (moving_average(ctx.history, self.ma_window), ctx.last_nav()) {
            (Some(ma), Some(last)) if ma > 0.0 => {
                let dev = last / ma - 1.0;                    // 低于均线为负
                (self.base * (1.0 - self.k * dev)).clamp(self.base * 0.5, self.base * 2.0)
            }
            _ => self.base,
        };
        vec![SignalEvent { date: ctx.today, direction: Direction::Buy, amount: SignalAmount::Cash(amount) }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{MarketEvent, SignalAmount};
    use crate::strategy::{Period, StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn bars(prices: &[f64]) -> Vec<MarketEvent> {
        prices.iter().enumerate().map(|(i,p)| MarketEvent{date:d(2024,1,(i+1) as u32),nav:*p,adj_nav:*p}).collect()
    }

    #[test]
    fn buys_more_when_below_ma() {
        // 均线≈中枢，当前价低于均线 → 金额 > base
        let mut s = SmartDca::new(Period::Monthly, 1, 1000.0, 3, 1.0);
        let hist = bars(&[1.2, 1.0, 0.8]); // ma=1.0, 当前0.8, dev=-0.2 → 1000*(1+0.2)=1200
        let today = hist.last().unwrap().clone();
        // 把定投日设到当天(1月3日? day=1 已在1月触发? 用首个bar触发)。改用 day=1, 当月首bar即触发
        let ctx = StrategyContext{today: today.date, history:&hist, shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs.len(), 1);
        if let SignalAmount::Cash(amt) = sigs[0].amount {
            assert!(amt > 1000.0, "expected >base, got {amt}");
            assert!(amt <= 2000.0);
        } else { panic!("expected cash"); }
    }

    #[test]
    fn falls_back_to_base_without_enough_history() {
        let mut s = SmartDca::new(Period::Monthly, 1, 1000.0, 250, 1.0);
        let hist = bars(&[1.0]);
        let today = hist.last().unwrap().clone();
        let ctx = StrategyContext{today: today.date, history:&hist, shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs[0].amount, SignalAmount::Cash(1000.0));
    }
}

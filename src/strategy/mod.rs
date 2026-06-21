use chrono::{Datelike, NaiveDate};
use crate::event::{MarketEvent, SignalEvent};

pub mod dca;
pub mod smart_dca;
pub mod trend;
pub mod rsi;
pub mod adaptive;
pub mod rules;

/// 策略每日可见的上下文（历史只到当日，防偷看未来）。
pub struct StrategyContext<'a> {
    pub today: &'a MarketEvent,
    pub history: &'a [MarketEvent],
    pub shares: f64,
    pub avg_cost: f64,
    pub cash: f64,
}

pub trait Strategy {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent>;
}

/// 让 Box<dyn Strategy> 也满足 Strategy，便于 RuleLayer 与引擎组合。
impl Strategy for Box<dyn Strategy> {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        (**self).on_market(ctx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Period { Monthly, Weekly }

/// 定投日历：每周期(月/周)在到达目标日时触发一次。
pub struct Schedule { period: Period, day: u32, last_key: Option<i64> }

impl Schedule {
    pub fn new(period: Period, day: u32) -> Self { Self { period, day, last_key: None } }

    pub fn due(&mut self, date: NaiveDate) -> bool {
        let (key, reached) = match self.period {
            Period::Monthly => {
                let key = date.year() as i64 * 12 + date.month() as i64;
                (key, date.day() >= self.day)
            }
            Period::Weekly => {
                let iso = date.iso_week();
                let key = iso.year() as i64 * 53 + iso.week() as i64;
                (key, date.weekday().number_from_monday() >= self.day)
            }
        };
        if reached && self.last_key != Some(key) {
            self.last_key = Some(key);
            true
        } else { false }
    }
}

/// 最近 window 根 bar 的复权净值均线（含当日）。
pub fn moving_average(history: &[MarketEvent], window: usize) -> Option<f64> {
    if window == 0 || history.len() < window { return None; }
    let slice = &history[history.len() - window..];
    Some(slice.iter().map(|b| b.adj_nav).sum::<f64>() / window as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MarketEvent;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn monthly_schedule_fires_once_per_month_on_or_after_day() {
        let mut s = Schedule::new(Period::Monthly, 5);
        assert!(!s.due(d(2024,1,3)));  // 早于5号
        assert!(s.due(d(2024,1,5)));   // 当月首次到达5号
        assert!(!s.due(d(2024,1,8)));  // 当月已触发
        assert!(s.due(d(2024,2,6)));   // 新月份
    }

    #[test]
    fn moving_average_needs_full_window() {
        let bars = vec![
            MarketEvent{date:d(2024,1,1),nav:1.0,adj_nav:1.0},
            MarketEvent{date:d(2024,1,2),nav:1.0,adj_nav:2.0},
            MarketEvent{date:d(2024,1,3),nav:1.0,adj_nav:3.0},
        ];
        assert_eq!(moving_average(&bars, 4), None);
        assert_eq!(moving_average(&bars, 3), Some(2.0));
        assert_eq!(moving_average(&bars[..2], 2), Some(1.5));
    }
}

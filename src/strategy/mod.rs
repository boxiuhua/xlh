use chrono::{Datelike, NaiveDate};
use crate::event::{MarketEvent, SignalEvent};

pub mod dca;
pub mod smart_dca;
pub mod trend;
pub mod rsi;
pub mod adaptive;
pub mod rules;

/// 策略在决策日 T 可见的上下文。
///
/// ## 为什么 `today` 只有日期、没有净值
///
/// 场外基金的 T 日净值要**收盘后**才公布，而申购/赎回的下单截止时间是**当日 15:00**。
/// 也就是说：下单的那一刻，你不可能知道今天的净值。
/// 「看到今天净值跌破均线 → 按今天的净值买入」在现实中物理上做不到。
///
/// 早先的实现把当日 `MarketEvent`（含 `adj_nav`）直接交给策略，且 `history` 也**含当日**，
/// 于是 `smart_dca`/`trend`/`rsi`/`adaptive`/止盈止损 全都在用「今天的收盘净值」做今天的决策 ——
/// 一天的未来函数。它不会让回测崩溃，只会**持续单向地美化所有择时策略**：
/// 信号总是在已经知道今天涨跌之后才触发。而纯日历触发的 `dca` 不受影响 ——
/// 结果是「5 个策略选最优」那场比赛里，4 个选手作弊、1 个不作弊。
///
/// 现在从结构上杜绝：**当日净值根本不出现在上下文里**，策略拿不到就无从偷看。
/// 决策只能基于 `history`（截止 T-1 的已公布净值），成交则发生在 T 日净值上 ——
/// 这正是真实的申赎流程：T 日 15:00 前提交，按 T 日净值确认。
pub struct StrategyContext<'a> {
    /// 决策日 T。**只有日期** —— T 日净值此刻尚未公布。
    pub today: NaiveDate,
    /// 已公布的历史净值，**截止 T-1**（不含当日）。
    pub history: &'a [MarketEvent],
    pub shares: f64,
    pub avg_cost: f64,
    pub cash: f64,
}

impl StrategyContext<'_> {
    /// 最近一个**已公布**的复权净值（T-1）。策略要用价格做决策，只能用这个。
    /// 首个交易日无历史 → `None`。
    pub fn last_nav(&self) -> Option<f64> {
        self.history.last().map(|b| b.adj_nav)
    }
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

/// 最近 window 根 bar 的复权净值均线。
///
/// 传入的 `history` 已截止 T-1（见 `StrategyContext`），故此均线**不含当日**。
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

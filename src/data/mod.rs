pub mod eastmoney;
pub mod cache;
pub mod fundlist;
pub mod sync;

use chrono::NaiveDate;
use crate::event::MarketEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavPoint { pub date: NaiveDate, pub nav: f64, pub acc_nav: f64 }

/// 由单位净值+累计净值推导复权净值（隐含红利再投）。
/// 当日分红 = 累计净值增量 - 单位净值增量（截断为非负）。
pub fn compute_adjusted(points: &[NavPoint]) -> Vec<f64> {
    let mut adj = Vec::with_capacity(points.len());
    if points.is_empty() { return adj; }
    adj.push(points[0].nav);
    for i in 1..points.len() {
        let prev = &points[i - 1];
        let cur = &points[i];
        let dividend = ((cur.acc_nav - prev.acc_nav) - (cur.nav - prev.nav)).max(0.0);
        let factor = if prev.nav > 0.0 { (cur.nav + dividend) / prev.nav } else { 1.0 };
        adj.push(adj[i - 1] * factor);
    }
    adj
}

pub trait DataHandler {
    /// 推进到下一交易日；数据耗尽返回 None。
    fn next_bar(&mut self) -> Option<MarketEvent>;
    /// 截至当前已发出 bar 的历史窗口（只含过去与当日，绝不含未来）。
    fn history(&self, lookback: usize) -> &[MarketEvent];
}

/// 内存数据源（测试与已抓取数据回放共用）。
pub struct InMemoryData { bars: Vec<MarketEvent>, cursor: usize }

impl InMemoryData {
    pub fn new(points: Vec<NavPoint>) -> Self {
        let adj = compute_adjusted(&points);
        let bars = points
            .iter()
            .zip(adj)
            .map(|(p, a)| MarketEvent { date: p.date, nav: p.nav, adj_nav: a })
            .collect();
        Self { bars, cursor: 0 }
    }
}

impl DataHandler for InMemoryData {
    fn next_bar(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.bars.len() {
            let b = self.bars[self.cursor].clone();
            self.cursor += 1;
            Some(b)
        } else { None }
    }
    fn history(&self, lookback: usize) -> &[MarketEvent] {
        let end = self.cursor;
        let start = end.saturating_sub(lookback);
        &self.bars[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    #[test]
    fn adjusted_reinvests_dividend() {
        // 第二天单位净值不变(1.0->1.0)但累计净值+0.1，说明分红0.1 → 复权应+10%
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.0, acc_nav: 1.1 },
        ];
        let adj = compute_adjusted(&pts);
        assert!((adj[0] - 1.0).abs() < 1e-9);
        assert!((adj[1] - 1.1).abs() < 1e-9);
    }

    #[test]
    fn adjusted_no_dividend_tracks_nav_growth() {
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.2, acc_nav: 1.2 },
        ];
        let adj = compute_adjusted(&pts);
        assert!((adj[1] - 1.2).abs() < 1e-9);
    }

    #[test]
    fn history_never_returns_future() {
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.1, acc_nav: 1.1 },
            NavPoint { date: d(2024,1,3), nav: 1.2, acc_nav: 1.2 },
        ];
        let mut h = InMemoryData::new(pts);
        let b1 = h.next_bar().unwrap();
        assert_eq!(b1.date, d(2024,1,1));
        assert_eq!(h.history(10).len(), 1); // 只含已发出的当日
        h.next_bar();
        assert_eq!(h.history(10).len(), 2);
        assert_eq!(h.history(1).len(), 1); // lookback 截断
    }
}

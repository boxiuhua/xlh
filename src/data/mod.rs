pub mod eastmoney;
pub mod cache;
pub mod fundlist;
pub mod sync;

use chrono::NaiveDate;
use crate::event::MarketEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavPoint { pub date: NaiveDate, pub nav: f64, pub acc_nav: f64 }

/// 由累计净值推导复权净值（隐含红利再投）。
/// 累计净值（累计单位净值）本身即已包含分红再投，故每日复权因子 = 累计净值之比。
/// 注意：不能用 `(单位净值增量 与 累计净值增量 之差)` 估算分红——一旦基金发生过
/// 份额折算（单位净值被重置而累计净值不重置），两者将处于不同量纲，该差值会在每个
/// 上涨日制造虚假"分红"并逐日复利放大，导致复权净值爆炸性失真。
pub fn compute_adjusted(points: &[NavPoint]) -> Vec<f64> {
    let mut adj = Vec::with_capacity(points.len());
    if points.is_empty() { return adj; }
    adj.push(points[0].nav);
    for i in 1..points.len() {
        let prev = &points[i - 1];
        let cur = &points[i];
        let factor = if prev.acc_nav > 0.0 { cur.acc_nav / prev.acc_nav } else { 1.0 };
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
    fn adjusted_handles_acc_nav_scale_gap() {
        // 份额折算后：单位净值被重置(从~2.6→1.0)，但累计净值不重置，二者从此处于不同量纲。
        // 之后某日纯价格上涨 10%（无分红），nav 与 acc 同步 +10%。
        // 复权因子应为真实涨幅 1.10，而非旧公式因量纲差制造的虚假"分红"导致的 1.40。
        let pts = vec![
            NavPoint { date: d(2024, 1, 1), nav: 1.0, acc_nav: 4.0 },
            NavPoint { date: d(2024, 1, 2), nav: 1.1, acc_nav: 4.4 },
        ];
        let adj = compute_adjusted(&pts);
        let factor = adj[1] / adj[0];
        assert!((factor - 1.10).abs() < 1e-9,
            "量纲不一致时复权因子应等于真实涨幅 1.10，实际 {factor:.4}");
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

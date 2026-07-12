use chrono::NaiveDate;
use crate::event::MarketEvent;
use crate::data::DataHandler;

pub mod secid;
pub mod kline;
pub mod tencent;
pub mod search;
pub mod cache;
pub mod sync;
pub mod fundamentals;
pub mod universe;
pub mod valuation;

/// 组合入口：A股/港股离线解析；美股经 suggest 搜索解析 secid。
pub fn resolve_secid(input: &str) -> anyhow::Result<secid::Secid> {
    match secid::resolve_offline(input)? {
        secid::Resolved::Ready(s) => Ok(s),
        secid::Resolved::NeedSearch(t) => search::resolve_us(&t),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StockBar {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub adj_close: f64,
}

pub struct StockData { bars: Vec<MarketEvent>, cursor: usize }

impl StockData {
    pub fn new(bars: Vec<StockBar>) -> Self {
        let bars = bars.into_iter()
            .map(|b| MarketEvent { date: b.date, nav: b.close, adj_nav: b.adj_close })
            .collect();
        Self { bars, cursor: 0 }
    }
}

impl DataHandler for StockData {
    fn next_bar(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.bars.len() {
            let b = self.bars[self.cursor].clone();
            self.cursor += 1;
            Some(b)
        } else { None }
    }
    fn history(&self, lookback: usize) -> &[MarketEvent] {
        // 截止 T-1，不含当日（cursor 在 next_bar() 中已自增）。见 data::DataHandler 的说明。
        // 股票虽可盘中看价、近似按收盘成交，但「用当日收盘价决策、又按当日收盘价成交」
        // 同样是理想化的；与基金侧统一为 T-1 决策 / T 日成交，口径一致且更保守。
        let end = self.cursor.saturating_sub(1);
        let start = end.saturating_sub(lookback);
        &self.bars[start..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, close: f64, adj: f64) -> StockBar {
        StockBar { date: dt, open: close, high: close, low: close, close, volume: 0.0, adj_close: adj }
    }

    #[test]
    fn maps_close_and_adj_into_market_event() {
        let bars = vec![bar(d(2024,1,2), 100.0, 200.0)];
        let mut h = StockData::new(bars);
        let ev = h.next_bar().unwrap();
        assert_eq!(ev.date, d(2024,1,2));
        assert!((ev.nav - 100.0).abs() < 1e-9, "nav 应为不复权 close");
        assert!((ev.adj_nav - 200.0).abs() < 1e-9, "adj_nav 应为后复权 adj_close");
        assert!(h.next_bar().is_none());
    }

    /// history 必须截止 T-1，绝不含当日 —— 与基金侧同一契约（见 data::DataHandler）。
    #[test]
    fn history_never_returns_future_nor_today() {
        let bars = vec![bar(d(2024,1,1),1.0,1.0), bar(d(2024,1,2),1.1,1.1), bar(d(2024,1,3),1.2,1.2)];
        let mut h = StockData::new(bars);

        let b1 = h.next_bar().unwrap();
        assert!(h.history(10).is_empty(), "首日无「昨天」");

        let b2 = h.next_bar().unwrap();
        let h2 = h.history(10);
        assert_eq!(h2.len(), 1);
        assert_eq!(h2[0].date, b1.date);
        assert!(h2.iter().all(|b| b.date < b2.date), "不得含当日");

        h.next_bar();
        assert_eq!(h.history(10).len(), 2);
        assert_eq!(h.history(1).len(), 1, "lookback 截断");
    }
}

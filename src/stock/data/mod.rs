use crate::data::DataHandler;
use crate::event::MarketEvent;
use crate::execution::ExecBar;
use chrono::NaiveDate;

pub mod cache;
pub mod fundamentals;
pub mod kline;
pub mod search;
pub mod secid;
pub mod sync;
pub mod tencent;
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

pub struct StockData {
    bars: Vec<MarketEvent>,
    /// 原始 bar,仅供成交模型(开盘价、前收)使用,不进入策略上下文。
    raw: Vec<StockBar>,
    /// 区间起始前一交易日的 bar,仅用于给 bar 0 提供 prev_close/prev_adj_close;
    /// 不进入 `bars`/`raw` 的可迭代序列,不影响 `next_bar()`/`history()`。
    prev: Option<StockBar>,
    cursor: usize,
}

impl StockData {
    pub fn new(raw: Vec<StockBar>) -> Self {
        Self::with_prev_bar(raw, None)
    }

    /// `prev` 是 `bars[0]` 前一交易日的 bar,仅用于其 `exec_bar().prev_close`/`prev_adj_close`。
    pub fn with_prev_bar(raw: Vec<StockBar>, prev: Option<StockBar>) -> Self {
        let bars = raw
            .iter()
            .map(|b| MarketEvent {
                date: b.date,
                nav: b.close,
                adj_nav: b.adj_close,
            })
            .collect();
        Self {
            bars,
            raw,
            prev,
            cursor: 0,
        }
    }

    /// 全部 bar 的策略视图(不受游标影响)。
    pub fn events(&self) -> &[MarketEvent] {
        &self.bars
    }
}

impl DataHandler for StockData {
    fn next_bar(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.bars.len() {
            let b = self.bars[self.cursor].clone();
            self.cursor += 1;
            Some(b)
        } else {
            None
        }
    }
    fn history(&self, lookback: usize) -> &[MarketEvent] {
        // 截止 T-1，不含当日（cursor 在 next_bar() 中已自增）。见 data::DataHandler 的说明。
        // 股票虽可盘中看价、近似按收盘成交，但「用当日收盘价决策、又按当日收盘价成交」
        // 同样是理想化的；与基金侧统一为 T-1 决策 / T 日成交，口径一致且更保守。
        let end = self.cursor.saturating_sub(1);
        let start = end.saturating_sub(lookback);
        &self.bars[start..end]
    }

    fn exec_bar(&self) -> Option<ExecBar> {
        let i = self.cursor.checked_sub(1)?;
        let b = self.raw.get(i)?;
        let (prev_close, prev_adj_close) = match i.checked_sub(1) {
            Some(j) => (Some(self.raw[j].close), Some(self.raw[j].adj_close)),
            None => (self.prev.map(|p| p.close), self.prev.map(|p| p.adj_close)),
        };
        Some(ExecBar {
            open: b.open,
            close: b.close,
            adj_close: b.adj_close,
            prev_close,
            prev_adj_close,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn bar(dt: NaiveDate, close: f64, adj: f64) -> StockBar {
        StockBar {
            date: dt,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            adj_close: adj,
        }
    }

    #[test]
    fn maps_close_and_adj_into_market_event() {
        let bars = vec![bar(d(2024, 1, 2), 100.0, 200.0)];
        let mut h = StockData::new(bars);
        let ev = h.next_bar().unwrap();
        assert_eq!(ev.date, d(2024, 1, 2));
        assert!((ev.nav - 100.0).abs() < 1e-9, "nav 应为不复权 close");
        assert!(
            (ev.adj_nav - 200.0).abs() < 1e-9,
            "adj_nav 应为后复权 adj_close"
        );
        assert!(h.next_bar().is_none());
    }

    /// history 必须截止 T-1，绝不含当日 —— 与基金侧同一契约（见 data::DataHandler）。
    #[test]
    fn history_never_returns_future_nor_today() {
        let bars = vec![
            bar(d(2024, 1, 1), 1.0, 1.0),
            bar(d(2024, 1, 2), 1.1, 1.1),
            bar(d(2024, 1, 3), 1.2, 1.2),
        ];
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

    #[test]
    fn with_prev_bar_only_feeds_exec_bar_not_iteration() {
        let prev = bar(d(2023, 12, 29), 9.0, 18.0);
        let b1 = bar(d(2024, 1, 2), 10.0, 20.0);
        let b2 = bar(d(2024, 1, 3), 11.0, 22.0);
        let mut h = StockData::with_prev_bar(vec![b1, b2], Some(prev));

        h.next_bar();
        let e1 = h.exec_bar().unwrap();
        assert_eq!(e1.prev_close, Some(9.0), "bar 0 应用 prev 的不复权 close");
        assert_eq!(
            e1.prev_adj_close,
            Some(18.0),
            "bar 0 应用 prev 的复权 close"
        );
        assert!(
            h.history(10).is_empty(),
            "首根 bar 无「昨天」，prev 不计入 history（与不传 prev 时行为一致）"
        );

        h.next_bar();
        assert!(h.next_bar().is_none(), "prev 不增加可迭代 bar 数，共 2 根");
        let h2 = h.history(10);
        assert_eq!(h2.len(), 1, "history 截止 T-1，只含 bars[0]，不含 prev");
        assert_eq!(h2[0].date, d(2024, 1, 2), "prev 的日期不应出现在 history");
    }

    #[test]
    fn exec_bar_exposes_raw_ohlc_and_prev_close() {
        let mut b1 = bar(d(2024, 1, 1), 10.0, 20.0);
        b1.open = 9.5;
        let mut b2 = bar(d(2024, 1, 2), 11.0, 22.0);
        b2.open = 10.5;
        let mut h = StockData::new(vec![b1, b2]);
        assert!(h.exec_bar().is_none(), "未推进时无当日");
        h.next_bar();
        let e1 = h.exec_bar().unwrap();
        assert!((e1.open - 9.5).abs() < 1e-9);
        assert!(e1.prev_close.is_none(), "首根无前收");
        h.next_bar();
        let e2 = h.exec_bar().unwrap();
        assert!((e2.open - 10.5).abs() < 1e-9);
        assert!((e2.close - 11.0).abs() < 1e-9);
        assert!((e2.adj_close - 22.0).abs() < 1e-9);
        assert_eq!(e2.prev_close, Some(10.0), "前收为不复权 close");
    }
}

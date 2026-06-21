use crate::event::{Direction, SignalAmount, SignalEvent, MarketEvent};
use crate::strategy::{Strategy, StrategyContext};

/// 最近 window 根 bar 的 RSI（简单平均法），基于 adj_nav 日涨跌。
/// 需 history.len() >= window+1；不足返回 None。平均跌幅为 0 → 100.0。
fn rsi(history: &[MarketEvent], window: usize) -> Option<f64> {
    if window == 0 || history.len() < window + 1 { return None; }
    let slice = &history[history.len() - (window + 1)..];
    let mut gain = 0.0;
    let mut loss = 0.0;
    for w in slice.windows(2) {
        let d = w[1].adj_nav - w[0].adj_nav;
        if d >= 0.0 { gain += d; } else { loss += -d; }
    }
    let avg_gain = gain / window as f64;
    let avg_loss = loss / window as f64;
    if avg_loss == 0.0 { return Some(100.0); }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}

/// RSI 超买超卖：RSI 下穿超卖线买入固定金额；上穿超买线清仓。
pub struct Rsi {
    window: usize,
    oversold: f64,
    overbought: f64,
    amount: f64,
    prev_rsi: Option<f64>,
}

impl Rsi {
    pub fn new(window: usize, oversold: f64, overbought: f64, amount: f64) -> Self {
        Self { window, oversold, overbought, amount, prev_rsi: None }
    }
}

impl Strategy for Rsi {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let cur = match rsi(ctx.history, self.window) {
            Some(v) => v,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        if let Some(prev) = self.prev_rsi {
            if prev >= self.oversold && cur < self.oversold {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Buy, amount: SignalAmount::Cash(self.amount) });
            } else if prev <= self.overbought && cur > self.overbought && ctx.shares > 1e-9 {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Sell, amount: SignalAmount::AllOut });
            }
        }
        self.prev_rsi = Some(cur);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day.min(28)).unwrap() }

    fn bars(prices: &[f64]) -> Vec<MarketEvent> {
        prices.iter().enumerate()
            .map(|(i, p)| MarketEvent { date: d(2024, 1, (i + 1) as u32), nav: *p, adj_nav: *p })
            .collect()
    }

    #[test]
    fn rsi_values() {
        let b = bars(&[1.0, 2.0, 1.0]);
        assert!((rsi(&b, 2).unwrap() - 50.0).abs() < 1e-9, "涨1跌1 → RSI 50");
        let up = bars(&[1.0, 2.0, 3.0]);
        assert!((rsi(&up, 2).unwrap() - 100.0).abs() < 1e-9, "全涨 → RSI 100");
        let short = bars(&[1.0, 2.0]);
        assert!(rsi(&short, 2).is_none(), "不足 window+1 → None");
    }

    fn run(prices: &[f64], window: usize, oversold: f64, overbought: f64) -> Vec<SignalEvent> {
        let mut s = Rsi::new(window, oversold, overbought, 1000.0);
        let bs = bars(prices);
        let mut out = Vec::new();
        for i in 0..bs.len() {
            let ctx = StrategyContext { today: &bs[i], history: &bs[..=i], shares: if i > 0 { 100.0 } else { 0.0 }, avg_cost: 1.0, cash: 0.0 };
            out.extend(s.on_market(&ctx));
        }
        out
    }

    #[test]
    fn buys_on_oversold_cross_sells_on_overbought_cross() {
        // window=2, os=30, ob=70。价格 [3,3,3,2,1,4,5,1,5]：
        // i3 RSI 100→0 跌破30→Buy；i5 0→75 升破70→Sell(AllOut)；i7 100→20 跌破30→Buy
        let sigs = run(&[3.0, 3.0, 3.0, 2.0, 1.0, 4.0, 5.0, 1.0, 5.0], 2, 30.0, 70.0);
        assert!(sigs.iter().any(|s| s.direction == Direction::Buy), "应有买入");
        assert!(sigs.iter().any(|s| s.direction == Direction::Sell && s.amount == SignalAmount::AllOut), "应有清仓");
    }
}

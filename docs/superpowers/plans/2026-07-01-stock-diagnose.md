# 股票技术分析/诊断 Implementation Plan（子项目 3）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 对单只股票 OHLCV 计算 MA/MACD/布林/RSI，给出趋势与综合买卖信号，产出可序列化 `StockDiagnosis`。

**Architecture:** 纯函数指标（`indicators.rs`）+ 组装诊断（`diagnose.rs`），基于后复权价 `adj_close`。风格参照基金 `analyze.rs` 但独立实现、互不 `use`。

**Tech Stack:** Rust；无新增依赖。

## Global Constraints

- 隔离：`src/stock/indicators.rs`、`src/stock/diagnose.rs` 只 `use` std/chrono/serde/anyhow 与 `crate::stock::data::StockBar`，禁止 `use` 任何基金专属模块。
- 指标一律基于 `adj_close` 序列。
- 测试全离线，`cargo test` 全绿。
- 提交信息结尾追加：`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`。

---

## File Structure

```
src/stock/mod.rs         —— 修改：新增 pub mod indicators; diagnose;
src/stock/indicators.rs  —— 新建：sma/ema_series/macd/bollinger/rsi
src/stock/diagnose.rs    —— 新建：DiagnoseParams/StockDiagnosis/diagnose
```

---

## Task 1: 技术指标 indicators

**Files:**
- Create: `src/stock/indicators.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod indicators;`）

**Interfaces:**
- Produces:
  - `sma(prices: &[f64], n: usize) -> Option<f64>`
  - `ema_series(prices: &[f64], n: usize) -> Vec<f64>`
  - `struct Macd { macd, signal, hist: f64 }`（Debug, Clone, Copy, PartialEq）+ `macd(prices, fast, slow, signal) -> Option<Macd>`
  - `struct Boll { mid, upper, lower, std: f64 }`（Debug, Clone, Copy, PartialEq）+ `bollinger(prices, n, k) -> Option<Boll>`
  - `rsi(prices: &[f64], n: usize) -> Option<f64>`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod indicators;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/indicators.rs`：
```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Macd { pub macd: f64, pub signal: f64, pub hist: f64 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Boll { pub mid: f64, pub upper: f64, pub lower: f64, pub std: f64 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_latest_window() {
        assert_eq!(sma(&[1.0,2.0,3.0,4.0], 2), Some(3.5));
        assert_eq!(sma(&[1.0], 2), None);
        assert_eq!(sma(&[1.0,2.0], 0), None);
    }

    #[test]
    fn ema_series_seeds_first_and_tracks() {
        let e = ema_series(&[1.0, 2.0, 3.0], 2); // alpha=2/3
        assert_eq!(e.len(), 3);
        assert!((e[0] - 1.0).abs() < 1e-9, "seed=首值");
        // e1 = 1 + 2/3*(2-1) = 1.6667; e2 = 1.6667 + 2/3*(3-1.6667)=2.5556
        assert!((e[1] - 1.6666667).abs() < 1e-6);
        assert!((e[2] - 2.5555556).abs() < 1e-6);
        assert!(ema_series(&[], 2).is_empty());
    }

    #[test]
    fn macd_hist_positive_on_uptrend() {
        let prices: Vec<f64> = (0..60).map(|i| 100.0 + i as f64).collect();
        let m = macd(&prices, 12, 26, 9).unwrap();
        assert!(m.hist > 0.0, "上升序列 MACD 柱应为正: {m:?}");
    }

    #[test]
    fn macd_hist_negative_on_downtrend() {
        let prices: Vec<f64> = (0..60).map(|i| 200.0 - i as f64).collect();
        let m = macd(&prices, 12, 26, 9).unwrap();
        assert!(m.hist < 0.0, "下降序列 MACD 柱应为负: {m:?}");
    }

    #[test]
    fn macd_none_when_insufficient() {
        assert!(macd(&[1.0,2.0,3.0], 12, 26, 9).is_none());
    }

    #[test]
    fn bollinger_constant_series_zero_std() {
        let b = bollinger(&[5.0; 20], 20, 2.0).unwrap();
        assert!((b.std).abs() < 1e-9);
        assert!((b.mid - 5.0).abs() < 1e-9);
        assert!((b.upper - 5.0).abs() < 1e-9 && (b.lower - 5.0).abs() < 1e-9);
    }

    #[test]
    fn bollinger_bands_symmetric() {
        let prices: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        let b = bollinger(&prices, 20, 2.0).unwrap();
        assert!((b.mid - 10.5).abs() < 1e-9, "1..20 均值=10.5");
        assert!((b.upper - b.mid - (b.mid - b.lower)).abs() < 1e-9, "上下带对称");
        assert!(bollinger(&prices, 21, 2.0).is_none());
    }

    #[test]
    fn rsi_all_up_is_100_all_down_is_0() {
        let up: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        assert!((rsi(&up, 14).unwrap() - 100.0).abs() < 1e-9);
        let down: Vec<f64> = (0..20).map(|i| 100.0 - i as f64).collect();
        assert!((rsi(&down, 14).unwrap()).abs() < 1e-9);
    }

    #[test]
    fn rsi_mixed_in_range_and_insufficient_none() {
        let mixed: Vec<f64> = (0..20).map(|i| 100.0 + if i % 2 == 0 { 1.0 } else { -0.8 } * i as f64).collect();
        let r = rsi(&mixed, 14).unwrap();
        assert!(r > 0.0 && r < 100.0, "混合序列 RSI 应在(0,100): {r}");
        assert!(rsi(&[1.0,2.0], 14).is_none());
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::indicators`
Expected: 编译失败 —— `sma`/`ema_series`/`macd`/`bollinger`/`rsi` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/indicators.rs` 的 `struct Boll` 之后加入：
```rust
pub fn sma(prices: &[f64], n: usize) -> Option<f64> {
    if n == 0 || prices.len() < n { return None; }
    Some(prices[prices.len() - n..].iter().sum::<f64>() / n as f64)
}

/// EMA 序列：seed=首值，alpha=2/(n+1)。空输入→空。
pub fn ema_series(prices: &[f64], n: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(prices.len());
    if prices.is_empty() || n == 0 { return out; }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut prev = prices[0];
    out.push(prev);
    for &p in &prices[1..] {
        prev += alpha * (p - prev);
        out.push(prev);
    }
    out
}

pub fn macd(prices: &[f64], fast: usize, slow: usize, signal: usize) -> Option<Macd> {
    if prices.len() < slow.max(1) { return None; }
    let ef = ema_series(prices, fast);
    let es = ema_series(prices, slow);
    let line: Vec<f64> = ef.iter().zip(&es).map(|(a, b)| a - b).collect();
    let sig = ema_series(&line, signal);
    let m = *line.last()?;
    let s = *sig.last()?;
    Some(Macd { macd: m, signal: s, hist: m - s })
}

pub fn bollinger(prices: &[f64], n: usize, k: f64) -> Option<Boll> {
    if n == 0 || prices.len() < n { return None; }
    let s = &prices[prices.len() - n..];
    let mid = s.iter().sum::<f64>() / n as f64;
    let var = s.iter().map(|x| (x - mid).powi(2)).sum::<f64>() / n as f64; // 总体方差
    let std = var.sqrt();
    Some(Boll { mid, upper: mid + k * std, lower: mid - k * std, std })
}

/// 简单均值口径 RSI：最近 n 个日变动的平均涨/跌幅。全涨→100，全跌→0。
pub fn rsi(prices: &[f64], n: usize) -> Option<f64> {
    if n == 0 || prices.len() < n + 1 { return None; }
    let start = prices.len() - n;
    let (mut gain, mut loss) = (0.0, 0.0);
    for i in start..prices.len() {
        let ch = prices[i] - prices[i - 1];
        if ch >= 0.0 { gain += ch; } else { loss -= ch; }
    }
    let avg_gain = gain / n as f64;
    let avg_loss = loss / n as f64;
    if avg_loss < 1e-12 { return Some(100.0); }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::indicators`
Expected: 9 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/indicators.rs src/stock/mod.rs
git commit -m "feat(stock): 技术指标 sma/ema/macd/bollinger/rsi"
```

---

## Task 2: 诊断 diagnose

**Files:**
- Create: `src/stock/diagnose.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod diagnose;`）

**Interfaces:**
- Consumes: `crate::stock::data::StockBar`、`crate::stock::indicators::{self, Macd, Boll}`
- Produces:
  - `struct DiagnoseParams { ... }` + `impl Default`
  - `struct StockDiagnosis { ... }`（Debug, Clone, Serialize）
  - `diagnose(code: String, name: String, bars: &[StockBar], p: &DiagnoseParams) -> anyhow::Result<StockDiagnosis>`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod diagnose;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/diagnose.rs`：
```rust
use anyhow::{anyhow, Result};
use serde::Serialize;
use crate::stock::data::StockBar;
use crate::stock::indicators;

pub struct DiagnoseParams {
    pub ma_short: usize, pub ma_long: usize,
    pub boll_window: usize, pub boll_k: f64,
    pub rsi_period: usize,
    pub macd_fast: usize, pub macd_slow: usize, pub macd_signal: usize,
    pub trend_window: usize,
    pub up_threshold: f64, pub down_threshold: f64,
}

impl Default for DiagnoseParams {
    fn default() -> Self {
        Self {
            ma_short: 20, ma_long: 60,
            boll_window: 20, boll_k: 2.0,
            rsi_period: 14,
            macd_fast: 12, macd_slow: 26, macd_signal: 9,
            trend_window: 60,
            up_threshold: 0.10, down_threshold: -0.10,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StockDiagnosis {
    pub code: String, pub name: String, pub date: String,
    pub price: f64, pub adj_price: f64,
    pub ma_short: f64, pub ma_long: f64, pub ma_relation: String,
    pub macd: f64, pub macd_signal: f64, pub macd_hist: f64,
    pub boll_mid: f64, pub boll_upper: f64, pub boll_lower: f64, pub boll_z: f64,
    pub rsi: f64,
    pub trend: String,
    pub signal: String,
    pub score: i32,
    pub rationale: String,
    pub caveat: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<StockBar> {
        vals.iter().enumerate().map(|(i, v)| StockBar {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            open: *v, high: *v, low: *v, close: *v, volume: 0.0, adj_close: *v,
        }).collect()
    }

    #[test]
    fn uptrend_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + i as f64).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "上涨");
        assert_eq!(dgn.ma_relation, "多头排列");
    }

    #[test]
    fn downtrend_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 200.0 - i as f64).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "下跌");
        assert_eq!(dgn.ma_relation, "空头排列");
    }

    #[test]
    fn range_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + if i % 2 == 0 { 0.0 } else { 0.5 }).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "震荡");
    }

    #[test]
    fn spike_down_last_point_leans_buy() {
        // 80 点窄幅震荡后末点深跌 → 布林 z 很负、RSI 低 → score 偏买
        let mut vals: Vec<f64> = (0..79).map(|i| 100.0 + if i % 2 == 0 { -0.5 } else { 0.5 }).collect();
        vals.push(80.0);
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert!(dgn.score > 0, "深跌末点应偏买: score={}", dgn.score);
        assert!(dgn.signal.contains("买入"), "信号应含买入: {}", dgn.signal);
    }

    #[test]
    fn spike_up_last_point_leans_sell() {
        let mut vals: Vec<f64> = (0..79).map(|i| 100.0 + if i % 2 == 0 { -0.5 } else { 0.5 }).collect();
        vals.push(140.0);
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert!(dgn.score < 0, "冲高末点应偏卖: score={}", dgn.score);
        assert!(dgn.signal.contains("卖出"), "信号应含卖出: {}", dgn.signal);
    }

    #[test]
    fn insufficient_data_errors() {
        let vals: Vec<f64> = (0..30).map(|_| 100.0).collect();
        let err = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn serializes_frontend_keys() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + 0.1 * i as f64).collect();
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        let j = serde_json::to_string(&dgn).unwrap();
        for key in ["\"trend\"", "\"signal\"", "\"macd_hist\"", "\"boll_z\"", "\"rsi\"", "\"ma_relation\"", "\"score\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::diagnose`
Expected: 编译失败 —— `diagnose` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/diagnose.rs` 的 `struct StockDiagnosis` 之后加入：
```rust
/// 对单股 OHLCV 做技术诊断（基于后复权价）。纯函数。
pub fn diagnose(code: String, name: String, bars: &[StockBar], p: &DiagnoseParams) -> Result<StockDiagnosis> {
    let need = p.ma_long.max(p.boll_window).max(p.rsi_period + 1).max(p.macd_slow).max(p.trend_window);
    if bars.len() < need {
        return Err(anyhow!("数据不足: 诊断需要至少 {} 个交易日，当前 {}", need, bars.len()));
    }
    let adj: Vec<f64> = bars.iter().map(|b| b.adj_close).collect();
    let last = bars.last().unwrap();

    let ma_short = indicators::sma(&adj, p.ma_short).unwrap();
    let ma_long = indicators::sma(&adj, p.ma_long).unwrap();
    let eps = 0.005;
    let ma_relation = if ma_short > ma_long * (1.0 + eps) { "多头排列" }
        else if ma_short < ma_long * (1.0 - eps) { "空头排列" }
        else { "纠缠" };

    let m = indicators::macd(&adj, p.macd_fast, p.macd_slow, p.macd_signal).unwrap();
    let b = indicators::bollinger(&adj, p.boll_window, p.boll_k).unwrap();
    let rsi = indicators::rsi(&adj, p.rsi_period).unwrap();

    let price_adj = *adj.last().unwrap();
    let boll_z = if b.std > 1e-12 { (price_adj - b.mid) / b.std } else { 0.0 };

    // 趋势
    let w = &adj[adj.len() - p.trend_window..];
    let window_return = if w[0] > 0.0 { w[w.len() - 1] / w[0] - 1.0 } else { 0.0 };
    let trend = if window_return > p.up_threshold && ma_short > ma_long { "上涨" }
        else if window_return < p.down_threshold && ma_short < ma_long { "下跌" }
        else { "震荡" };

    // 综合打分（正=偏买）
    let boll_sig = if boll_z <= -2.0 { 2 } else if boll_z <= -1.0 { 1 }
        else if boll_z < 1.0 { 0 } else if boll_z < 2.0 { -1 } else { -2 };
    let rsi_sig = if rsi <= 30.0 { 1 } else if rsi >= 70.0 { -1 } else { 0 };
    let macd_sig = if m.hist > 0.0 { 1 } else if m.hist < 0.0 { -1 } else { 0 };
    let score = boll_sig + rsi_sig + macd_sig;
    let signal = if score >= 2 { "强力买入" } else if score == 1 { "买入" }
        else if score == 0 { "观望" } else if score == -1 { "卖出" } else { "强力卖出" };

    let rationale = format!(
        "趋势{trend}（近{}日{:+.1}%，{ma_relation}）；布林 z={:.2}；RSI={:.1}；MACD 柱={:+.4}",
        p.trend_window, window_return * 100.0, boll_z, rsi, m.hist);
    let caveat = match trend {
        "上涨" => "上涨趋势：顺势持有，回踩布林下轨可低吸，勿追高。",
        "下跌" => "下跌趋势：反弹至上轨谨慎减仓，抄底只在超卖小额试探。",
        _ => "震荡：区间内沿布林上下轨高抛低吸，按信号分档执行。",
    }.to_string();

    Ok(StockDiagnosis {
        code, name, date: last.date.to_string(),
        price: last.close, adj_price: price_adj,
        ma_short, ma_long, ma_relation: ma_relation.to_string(),
        macd: m.macd, macd_signal: m.signal, macd_hist: m.hist,
        boll_mid: b.mid, boll_upper: b.upper, boll_lower: b.lower, boll_z,
        rsi,
        trend: trend.to_string(),
        signal: signal.to_string(),
        score,
        rationale,
        caveat,
    })
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::diagnose`
Expected: 7 个测试 PASS。

- [ ] **Step 6: 全量回归 + 提交**

Run: `cargo test`
Expected: 全绿。
```bash
git add src/stock/diagnose.rs src/stock/mod.rs
git commit -m "feat(stock): 单股技术诊断 diagnose（趋势+综合买卖信号）"
```

---

## Self-Review

**1. Spec 覆盖：** MA/MACD/布林/RSI→Task1；趋势判断+综合信号+StockDiagnosis→Task2；adj_close 口径→Task2；数据不足报错→Task2；序列化键→Task2 测试。✅

**2. 占位符扫描：** 无 TBD/TODO；每步含完整代码。✅

**3. 类型一致性：** `Macd`/`Boll` 在 indicators 定义、diagnose 使用一致；`indicators::{sma,ema_series,macd,bollinger,rsi}` 签名一致；`StockBar` 字段（date/open/high/low/close/volume/adj_close，子项目1）一致；`DiagnoseParams`/`StockDiagnosis` 字段自洽。✅

**说明：** 诊断的趋势判断与信号打分均只读 `adj_close`；`price` 展示用不复权 `close`。`spike_down/up` 测试中末点极端值使布林 z 越界 ±2、并使 RSI 与 MACD 柱同向，确保 score 明确偏买/偏卖、signal 含"买入"/"卖出"。

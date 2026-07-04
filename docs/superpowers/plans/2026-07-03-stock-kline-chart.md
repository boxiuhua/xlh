# 股票专业 K线图 + 投资参考 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Web 股票诊断页展示服务端渲染的多面板 K线图（蜡烛+成交量+MACD+RSI，主图叠 MA/布林），与已有文字诊断并排构成「投资参考」。

**Architecture:** 复用 `stock/indicators`（新增 series 版）算指标序列 → 新 `stock/chart` 用 plotters 画多面板 SVG → 新 `/api/stock/chart` 端点返回 SVG → `page.rs` 诊断视图 `<img>` 嵌入。分析引擎（indicators/diagnose）逻辑不变。

**Tech Stack:** Rust, plotters 0.3（SVGBackend，已在依赖中）, axum 0.7, 前端 vanilla JS。

## Global Constraints

- **不新增任何 crate 依赖** —— 用现有 `plotters` 的 `SVGBackend`（PNG 内存编码需引入 `image` crate，故改用 SVG）。
- **蜡烛红涨绿跌（A股惯例）** —— 涨 `RGBColor(0xd6,0x3a,0x2f)`，跌 `RGBColor(0x1a,0x9c,0x5b)`。
- **图用原始 OHLC/close**；文字诊断沿用后复权（不改）。
- **默认窗口 250 交易日**，`days` 参数 `clamp(30, 2000)`。
- **series 末值一致性**：每个新 `*_series` 的最后一个有效值须与现有 last-value 版（`sma`/`bollinger`/`macd`/`rsi`）一致，作为回归约束。
- 提交信息结尾带 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`。
- 偏离 spec 说明：spec 写的是 PNG，实现改为 SVG（同为服务端 plotters 静态图、`<img>` 嵌入；SVG 内存直出、零新依赖、可缩放）。

---

## File Structure

- `src/stock/indicators.rs`（改）—— 新增 `sma_series`/`bollinger_series`/`macd_series`/`rsi_series`。
- `src/stock/chart.rs`（新）—— `ChartParams` + `render(&[StockBar], title, &ChartParams) -> String`（SVG）。
- `src/stock/mod.rs`（改）—— `pub mod chart;`。
- `src/web/stock.rs`（改）—— `ChartQuery` + `chart_handler` + `chart_blocking`。
- `src/web/mod.rs`（改）—— 注册 `/api/stock/chart` 路由。
- `src/web/page.rs`（改）—— `renderStockDiag` 顶部插 `<img>`。

---

## Task 1: indicators series 版

**Files:**
- Modify: `src/stock/indicators.rs`（在 `rsi` 之后、`#[cfg(test)]` 之前追加函数；测试追加到 tests 模块）

**Interfaces:**
- Consumes: 现有 `ema_series(prices, n) -> Vec<f64>`。
- Produces:
  - `sma_series(prices: &[f64], n: usize) -> Vec<Option<f64>>`
  - `bollinger_series(prices: &[f64], n: usize, k: f64) -> Vec<Option<(f64, f64, f64)>>` —— (mid, upper, lower)
  - `macd_series(prices: &[f64], fast: usize, slow: usize, signal: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>)` —— (dif, dea, hist)，长度均等于 `prices.len()`
  - `rsi_series(prices: &[f64], n: usize) -> Vec<Option<f64>>`
  - 所有 `Vec` 长度等于 `prices.len()`，预热段为 `None`。

- [ ] **Step 1: 写失败测试**（追加到 `src/stock/indicators.rs` 的 `mod tests`）

```rust
    #[test]
    fn series_lengths_and_warmup() {
        let p: Vec<f64> = (1..=30).map(|i| i as f64).collect();
        assert_eq!(sma_series(&p, 5).len(), p.len());
        assert!(sma_series(&p, 5)[..4].iter().all(|x| x.is_none()), "前 n-1 应为 None");
        assert!(sma_series(&p, 5)[4].is_some());
        assert_eq!(bollinger_series(&p, 5, 2.0).len(), p.len());
        assert_eq!(rsi_series(&p, 14).len(), p.len());
        let (dif, dea, hist) = macd_series(&p, 12, 26, 9);
        assert_eq!(dif.len(), p.len());
        assert_eq!(dea.len(), p.len());
        assert_eq!(hist.len(), p.len());
    }

    #[test]
    fn series_last_value_matches_scalar() {
        let p: Vec<f64> = (0..80).map(|i| 100.0 + (i as f64 * 0.7).sin() * 5.0 + i as f64 * 0.3).collect();
        // SMA
        assert!((sma_series(&p, 20).last().unwrap().unwrap() - sma(&p, 20).unwrap()).abs() < 1e-9);
        // Bollinger
        let (m, u, l) = bollinger_series(&p, 20, 2.0).last().unwrap().unwrap();
        let b = bollinger(&p, 20, 2.0).unwrap();
        assert!((m - b.mid).abs() < 1e-9 && (u - b.upper).abs() < 1e-9 && (l - b.lower).abs() < 1e-9);
        // MACD
        let (_dif, _dea, hist) = macd_series(&p, 12, 26, 9);
        assert!((hist.last().unwrap() - macd(&p, 12, 26, 9).unwrap().hist).abs() < 1e-9);
        // RSI
        assert!((rsi_series(&p, 14).last().unwrap().unwrap() - rsi(&p, 14).unwrap()).abs() < 1e-9);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xlh indicators::tests::series_ -- --nocapture`
Expected: 编译失败（`sma_series` 等未定义）。

- [ ] **Step 3: 实现（追加到 `rsi` 之后）**

```rust
/// 滚动 SMA 序列，长度对齐输入，前 n-1 项为 None。
pub fn sma_series(prices: &[f64], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; prices.len()];
    if n == 0 || prices.len() < n { return out; }
    let mut sum: f64 = prices[..n].iter().sum();
    out[n - 1] = Some(sum / n as f64);
    for i in n..prices.len() {
        sum += prices[i] - prices[i - n];
        out[i] = Some(sum / n as f64);
    }
    out
}

/// 滚动布林带序列：(mid, upper, lower)，总体方差口径（与 `bollinger` 一致）。
pub fn bollinger_series(prices: &[f64], n: usize, k: f64) -> Vec<Option<(f64, f64, f64)>> {
    let mut out = vec![None; prices.len()];
    if n == 0 || prices.len() < n { return out; }
    for i in (n - 1)..prices.len() {
        let s = &prices[i + 1 - n..=i];
        let mid = s.iter().sum::<f64>() / n as f64;
        let var = s.iter().map(|x| (x - mid).powi(2)).sum::<f64>() / n as f64;
        let std = var.sqrt();
        out[i] = Some((mid, mid + k * std, mid - k * std));
    }
    out
}

/// MACD 序列：(DIF, DEA, 柱)，均与输入等长（复用 `ema_series` 的全长序列）。
pub fn macd_series(prices: &[f64], fast: usize, slow: usize, signal: usize) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let ef = ema_series(prices, fast);
    let es = ema_series(prices, slow);
    let dif: Vec<f64> = ef.iter().zip(&es).map(|(a, b)| a - b).collect();
    let dea = ema_series(&dif, signal);
    let hist: Vec<f64> = dif.iter().zip(&dea).map(|(a, b)| a - b).collect();
    (dif, dea, hist)
}

/// 滚动 RSI 序列（简单均值口径，与 `rsi` 一致），前 n 项为 None。
pub fn rsi_series(prices: &[f64], n: usize) -> Vec<Option<f64>> {
    let mut out = vec![None; prices.len()];
    if n == 0 || prices.len() < n + 1 { return out; }
    for i in n..prices.len() {
        let (mut gain, mut loss) = (0.0, 0.0);
        for j in (i - n + 1)..=i {
            let ch = prices[j] - prices[j - 1];
            if ch >= 0.0 { gain += ch; } else { loss -= ch; }
        }
        let avg_gain = gain / n as f64;
        let avg_loss = loss / n as f64;
        out[i] = Some(if avg_loss < 1e-12 { 100.0 } else {
            let rs = avg_gain / avg_loss;
            100.0 - 100.0 / (1.0 + rs)
        });
    }
    out
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p xlh indicators`
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/stock/indicators.rs
git commit -m "feat(stock): 指标 series 版(sma/bollinger/macd/rsi)供绘图用

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: stock/chart.rs 多面板 SVG 渲染

**Files:**
- Create: `src/stock/chart.rs`
- Modify: `src/stock/mod.rs`（追加 `pub mod chart;`，放在 `pub mod diagnose;` 之后）

**Interfaces:**
- Consumes: `crate::stock::data::StockBar`（字段 `date: NaiveDate, open/high/low/close/volume/adj_close: f64`）；Task 1 的 `indicators::{sma_series, bollinger_series, macd_series, rsi_series}`。
- Produces:
  - `pub struct ChartParams { ma_short, ma_long, boll_window: usize, boll_k: f64, macd_fast, macd_slow, macd_signal, rsi_period: usize, width, height: u32 }` + `Default`（MA20/60、Boll(20,2)、MACD(12,26,9)、RSI14、1000×900）。
  - `pub fn render(bars: &[StockBar], title: &str, p: &ChartParams) -> String`（SVG 文本；数据不足时返回占位 SVG）。
  - `pub fn min_bars_needed(p: &ChartParams) -> usize`。

- [ ] **Step 1: 先加模块声明**

在 `src/stock/mod.rs` 的 `pub mod diagnose;` 后追加一行：

```rust
pub mod chart;
```

- [ ] **Step 2: 写失败测试**（写入新文件 `src/stock/chart.rs` 末尾的 tests 模块；先只放测试与空 render 会编译失败）

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<StockBar> {
        vals.iter().enumerate().map(|(i, v)| StockBar {
            date: NaiveDate::from_ymd_opt(2022, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            open: *v - 1.0, high: *v + 2.0, low: *v - 2.0, close: *v, volume: 1000.0 + i as f64, adj_close: *v,
        }).collect()
    }

    #[test]
    fn renders_svg_document() {
        let vals: Vec<f64> = (0..300).map(|i| 100.0 + (i as f64 * 0.1).sin() * 8.0 + i as f64 * 0.2).collect();
        let svg = render(&series(&vals), "600519", &ChartParams::default());
        assert!(!svg.is_empty());
        assert!(svg.contains("<svg"), "应是 SVG 文档");
        assert!(svg.contains("</svg>"));
    }

    #[test]
    fn insufficient_data_returns_placeholder() {
        let svg = render(&series(&[1.0, 2.0, 3.0]), "x", &ChartParams::default());
        assert!(svg.contains("<svg"));
        assert!(svg.contains("数据不足"), "数据不足应出占位提示");
    }

    #[test]
    fn threshold_exactly_enough_does_not_panic() {
        let p = ChartParams::default();
        let vals: Vec<f64> = (0..min_bars_needed(&p)).map(|i| 100.0 + i as f64).collect();
        let _ = render(&series(&vals), "x", &p); // 不 panic 即可
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test -p xlh chart::tests -- --nocapture`
Expected: 编译失败（`render`/`ChartParams`/`min_bars_needed` 未定义）。

- [ ] **Step 4: 实现 `src/stock/chart.rs` 主体**（放在文件顶部、tests 模块之前）

```rust
//! 股票多面板 K线图（SVG，plotters）。纯渲染，无 IO。
//! 面板自上而下：主图(蜡烛+MA+布林) / 成交量 / MACD / RSI，共享 X 轴(交易日索引)。
use plotters::prelude::*;
use plotters::coord::Shift;
use crate::stock::data::StockBar;
use crate::stock::indicators;

type Area<'a> = DrawingArea<SVGBackend<'a>, Shift>;

const UP: RGBColor = RGBColor(0xd6, 0x3a, 0x2f);   // 涨(红)
const DOWN: RGBColor = RGBColor(0x1a, 0x9c, 0x5b); // 跌(绿)
const MA_S: RGBColor = RGBColor(0x1f, 0x77, 0xb4); // MA短(蓝)
const MA_L: RGBColor = RGBColor(0xd6, 0x7f, 0x0e); // MA长(橙)
const BOLL: RGBColor = RGBColor(0x8e, 0x6f, 0xb8); // 布林(紫)
const GRID: RGBColor = RGBColor(0xee, 0xee, 0xee);

pub struct ChartParams {
    pub ma_short: usize, pub ma_long: usize,
    pub boll_window: usize, pub boll_k: f64,
    pub macd_fast: usize, pub macd_slow: usize, pub macd_signal: usize,
    pub rsi_period: usize,
    pub width: u32, pub height: u32,
}

impl Default for ChartParams {
    fn default() -> Self {
        Self {
            ma_short: 20, ma_long: 60, boll_window: 20, boll_k: 2.0,
            macd_fast: 12, macd_slow: 26, macd_signal: 9, rsi_period: 14,
            width: 1000, height: 900,
        }
    }
}

/// 指标预热所需的最少 bar 数。
pub fn min_bars_needed(p: &ChartParams) -> usize {
    p.ma_long.max(p.boll_window).max(p.macd_slow).max(p.rsi_period + 1)
}

/// 渲染多面板 K线图为 SVG。数据不足时返回带提示文字的占位 SVG。
pub fn render(bars: &[StockBar], title: &str, p: &ChartParams) -> String {
    if bars.len() < min_bars_needed(p) {
        return placeholder(title, p, bars.len());
    }
    let mut buf = String::new();
    {
        let root = SVGBackend::with_string(&mut buf, (p.width, p.height)).into_drawing_area();
        root.fill(&WHITE).ok();
        let h = p.height as i32;
        let (main, r1) = root.split_vertically(h * 50 / 100);
        let (vol, r2) = r1.split_vertically(h * 15 / 100);
        let (macd_a, rsi_a) = r2.split_vertically(h * 17 / 100);
        draw_main(&main, bars, title, p);
        draw_volume(&vol, bars);
        draw_macd(&macd_a, bars, p);
        draw_rsi(&rsi_a, bars, p);
    }
    buf
}

fn placeholder(title: &str, p: &ChartParams, have: usize) -> String {
    let mut buf = String::new();
    {
        let root = SVGBackend::with_string(&mut buf, (p.width, p.height)).into_drawing_area();
        root.fill(&WHITE).ok();
        let style = ("sans-serif", 26).into_font().color(&RGBColor(0x88, 0x88, 0x88));
        root.draw_text(
            &format!("{title}：数据不足（{have} 条，至少需 {}）", min_bars_needed(p)),
            &style, (60, p.height as i32 / 2),
        ).ok();
    }
    buf
}

fn draw_main(area: &Area<'_>, bars: &[StockBar], title: &str, p: &ChartParams) {
    let n = bars.len();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let ma_s = indicators::sma_series(&closes, p.ma_short);
    let ma_l = indicators::sma_series(&closes, p.ma_long);
    let boll = indicators::bollinger_series(&closes, p.boll_window, p.boll_k);

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for b in bars { lo = lo.min(b.low); hi = hi.max(b.high); }
    for v in boll.iter().flatten() { lo = lo.min(v.2); hi = hi.max(v.1); }
    let pad = (hi - lo).max(1e-6) * 0.05;
    let (lo, hi) = (lo - pad, hi + pad);

    let mut chart = ChartBuilder::on(area)
        .caption(title, ("sans-serif", 18))
        .margin(6).x_label_area_size(0).y_label_area_size(52)
        .build_cartesian_2d(0f64..(n as f64 - 1.0), lo..hi).unwrap();
    chart.configure_mesh().disable_x_mesh().light_line_style(GRID).draw().ok();

    // 蜡烛
    chart.draw_series(bars.iter().enumerate().map(|(i, b)|
        CandleStick::new(i as f64, b.open, b.high, b.low, b.close, UP.filled(), DOWN.filled(), 4)
    )).ok();
    // 叠加线
    line_opt(&mut chart, &ma_s, MA_S);
    line_opt(&mut chart, &ma_l, MA_L);
    line_opt(&mut chart, &boll.iter().map(|o| o.map(|v| v.0)).collect::<Vec<_>>(), BOLL);
    line_opt(&mut chart, &boll.iter().map(|o| o.map(|v| v.1)).collect::<Vec<_>>(), BOLL.mix(0.5));
    line_opt(&mut chart, &boll.iter().map(|o| o.map(|v| v.2)).collect::<Vec<_>>(), BOLL.mix(0.5));
}

fn draw_volume(area: &Area<'_>, bars: &[StockBar]) {
    let n = bars.len();
    let vmax = bars.iter().map(|b| b.volume).fold(1.0_f64, f64::max);
    let mut chart = ChartBuilder::on(area)
        .margin(6).x_label_area_size(0).y_label_area_size(52)
        .build_cartesian_2d(0f64..(n as f64 - 1.0), 0f64..vmax * 1.1).unwrap();
    chart.configure_mesh().disable_x_mesh().light_line_style(GRID).draw().ok();
    chart.draw_series(bars.iter().enumerate().map(|(i, b)| {
        let color = if b.close >= b.open { UP } else { DOWN };
        Rectangle::new([(i as f64 - 0.3, 0.0), (i as f64 + 0.3, b.volume)], color.filled())
    })).ok();
}

fn draw_macd(area: &Area<'_>, bars: &[StockBar], p: &ChartParams) {
    let n = bars.len();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let (dif, dea, hist) = indicators::macd_series(&closes, p.macd_fast, p.macd_slow, p.macd_signal);
    let mut lo = 0.0_f64;
    let mut hi = 0.0_f64;
    for v in dif.iter().chain(&dea).chain(&hist) { lo = lo.min(*v); hi = hi.max(*v); }
    let pad = (hi - lo).max(1e-6) * 0.1;
    let mut chart = ChartBuilder::on(area)
        .margin(6).x_label_area_size(0).y_label_area_size(52)
        .build_cartesian_2d(0f64..(n as f64 - 1.0), (lo - pad)..(hi + pad)).unwrap();
    chart.configure_mesh().disable_x_mesh().light_line_style(GRID).draw().ok();
    // 柱
    chart.draw_series(hist.iter().enumerate().map(|(i, &v)| {
        let color = if v >= 0.0 { UP } else { DOWN };
        Rectangle::new([(i as f64 - 0.3, 0.0), (i as f64 + 0.3, v)], color.filled())
    })).ok();
    // DIF / DEA
    chart.draw_series(LineSeries::new(dif.iter().enumerate().map(|(i, &v)| (i as f64, v)), MA_S)).ok();
    chart.draw_series(LineSeries::new(dea.iter().enumerate().map(|(i, &v)| (i as f64, v)), MA_L)).ok();
}

fn draw_rsi(area: &Area<'_>, bars: &[StockBar], p: &ChartParams) {
    let n = bars.len();
    let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
    let rsi = indicators::rsi_series(&closes, p.rsi_period);
    let mut chart = ChartBuilder::on(area)
        .margin(6).x_label_area_size(22).y_label_area_size(52)
        .build_cartesian_2d(0f64..(n as f64 - 1.0), 0f64..100f64).unwrap();
    chart.configure_mesh()
        .disable_x_mesh()
        .light_line_style(GRID)
        .x_labels(6)
        .x_label_formatter(&|x| {
            let i = *x as usize;
            bars.get(i).map(|b| b.date.format("%y-%m").to_string()).unwrap_or_default()
        })
        .draw().ok();
    // 30 / 70 参考线
    for y in [30.0_f64, 70.0] {
        chart.draw_series(LineSeries::new(
            [(0f64, y), (n as f64 - 1.0, y)], RGBColor(0xcc, 0xcc, 0xcc))).ok();
    }
    line_opt(&mut chart, &rsi, RGBColor(0x9b, 0x30, 0xa0));
}

/// 在图上画一条来自 `Vec<Option<f64>>` 的折线（跳过 None 预热段）。
fn line_opt<DB, X>(chart: &mut ChartContext<DB, X>, ys: &[Option<f64>], color: RGBColor)
where
    DB: DrawingBackend,
    X: plotters::coord::CoordTranslate<From = (f64, f64)> + plotters::coord::ranged1d::Ranged,
{
    // 见下方 Step 5：line_opt 用泛型不便，改为宏/闭包内联。占位说明。
    let _ = (chart, ys, color);
}
```

> 注：上面的 `line_opt` 泛型签名对 plotters 的 `ChartContext` 类型约束很啰嗦、易错。改为**内联闭包**避免泛型（见 Step 5 修正）。

- [ ] **Step 5: 用内联闭包替换 `line_opt`（修正 draw_main/draw_rsi）**

删除 `line_opt` 函数，并把各处 `line_opt(&mut chart, &xs, C);` 调用替换为内联绘制。为避免重复，在每个 draw 函数内部定义一个本地闭包宏式写法——直接内联如下（以 draw_main 为例，draw_rsi 同理）：

在 `draw_main` 末尾把 5 个 `line_opt(...)` 替换为：

```rust
    let draw_line = |chart: &mut ChartContext<SVGBackend, plotters::coord::types::RangedCoordf64>, _: ()| {};
    // 直接内联，不用闭包：
    for (ys, color) in [
        (&ma_s, MA_S),
        (&ma_l, MA_L),
        (&boll.iter().map(|o| o.map(|v| v.0)).collect::<Vec<_>>(), BOLL),
    ] {
        chart.draw_series(LineSeries::new(
            ys.iter().enumerate().filter_map(|(i, o)| o.map(|v| (i as f64, v))), color)).ok();
    }
    // 布林上下轨(淡色)
    for sel in [1usize, 2] {
        let band: Vec<Option<f64>> = boll.iter().map(|o| o.map(|v| if sel == 1 { v.1 } else { v.2 })).collect();
        chart.draw_series(LineSeries::new(
            band.iter().enumerate().filter_map(|(i, o)| o.map(|v| (i as f64, v))), BOLL.mix(0.5))).ok();
    }
    let _ = draw_line; // 删除该行；仅示意不要保留占位
```

> 简化说明：`ChartContext` 的第二个类型参数是 `Cartesian2d<RangedCoordf64, RangedCoordf64>`。若上面写法的类型标注太繁，**最稳妥**是不抽公共函数，直接在 draw_main / draw_rsi 里对每条线各写一句 `chart.draw_series(LineSeries::new(iter, color)).ok();`（`iter` = `xs.iter().enumerate().filter_map(|(i,o)| o.map(|v| (i as f64, v)))`）。draw_rsi 的 RSI 线同法内联。删除 `line_opt` 及其 `where` 约束。

- [ ] **Step 6: 运行确认通过**

Run: `cargo test -p xlh chart`
Expected: 3 测试 PASS。若 plotters API 有细节报错（如 `CandleStick::new` 参数、`split_vertically` 像素类型），按编译器提示微调，不改变行为与断言。

- [ ] **Step 7: 提交**

```bash
git add src/stock/chart.rs src/stock/mod.rs
git commit -m "feat(stock): 多面板 K线图 SVG 渲染(蜡烛+量+MACD+RSI)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: /api/stock/chart 端点

**Files:**
- Modify: `src/web/stock.rs`（追加 `ChartQuery`、`chart_handler`、`chart_blocking`，及测试）
- Modify: `src/web/mod.rs:277` 附近路由表（新增一行）

**Interfaces:**
- Consumes: `crate::stock::chart::{render, ChartParams}`；现有 `validate_stock_code`、`cache::load_or_fetch`、`stock_cache()`、`AppError`。
- Produces: `pub async fn chart_handler(Query<ChartQuery>) -> Result<Response, AppError>`，返回 `Content-Type: image/svg+xml`。

- [ ] **Step 1: 写失败测试**（追加到 `src/web/stock.rs` 的 `mod tests`）

```rust
    #[tokio::test]
    async fn chart_bad_code_is_400() {
        let resp = crate::web::router()
            .oneshot(Request::builder().uri("/api/stock/chart?code=bad!!code").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xlh chart_bad_code_is_400`
Expected: 失败（路由不存在 → 404，断言 400 失败）。

- [ ] **Step 3: 实现 handler**（追加到 `src/web/stock.rs`；文件顶部 `use` 增加 `axum::response::{IntoResponse, Response}` 与 `axum::http::header`）

```rust
#[derive(Debug, Deserialize)]
pub struct ChartQuery { pub code: String, #[serde(default)] pub days: Option<usize> }

pub async fn chart_handler(Query(q): Query<ChartQuery>) -> std::result::Result<Response, AppError> {
    let svg = tokio::task::spawn_blocking(move || chart_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(([(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")], svg).into_response())
}

fn chart_blocking(q: ChartQuery) -> Result<String> {
    validate_stock_code(&q.code)?;
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(800);
    let mut bars = cache::load_or_fetch(&q.code, stock_cache(), start, end)
        .map_err(|e| anyhow!("加载行情失败: {e}"))?;
    let days = q.days.unwrap_or(250).clamp(30, 2000);
    if bars.len() > days { bars = bars[bars.len() - days..].to_vec(); }
    Ok(crate::stock::chart::render(&bars, &q.code, &crate::stock::chart::ChartParams::default()))
}
```

- [ ] **Step 4: 注册路由**（`src/web/mod.rs`，在 `.route("/api/stock/sync", ...)` 那一行之后追加）

```rust
        .route("/api/stock/chart", get(stock::chart_handler))
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test -p xlh chart_bad_code_is_400`
Expected: PASS。再跑 `cargo build` 确认整体编译。

- [ ] **Step 6: 提交**

```bash
git add src/web/stock.rs src/web/mod.rs
git commit -m "feat(web): /api/stock/chart 返回 K线图 SVG

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: page.rs 诊断视图嵌入图表

**Files:**
- Modify: `src/web/page.rs`（`renderStockDiag` 函数，约 832-844 行）
- Modify: `src/web/stock.rs`（tests 追加接线测试）

**Interfaces:**
- Consumes: Task 3 的 `/api/stock/chart` 端点；诊断对象字段 `d.code`。
- Produces: 诊断结果框顶部一张 `<img>`，`onerror` 占位。

- [ ] **Step 1: 写失败测试**（追加到 `src/web/stock.rs` 的 `mod tests`）

```rust
    #[tokio::test]
    async fn index_wires_chart_endpoint() {
        let resp = crate::web::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8_lossy(&bytes);
        assert!(html.contains("/api/stock/chart"), "股诊断视图应引用图表端点");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p xlh index_wires_chart_endpoint`
Expected: 失败（页面尚未引用该端点）。

- [ ] **Step 3: 实现**（修改 `src/web/page.rs` 的 `renderStockDiag`，在 `box.innerHTML =` 的字符串最前面拼接图表 img）

把：

```javascript
  box.innerHTML =
    '<div style="display:flex;gap:12px;align-items:baseline;flex-wrap:wrap">...
```

改为在 `box.innerHTML =` 后先接一段 img（其余不变）：

```javascript
  var chartSrc = '/api/stock/chart?code=' + encodeURIComponent(d.code) + '&days=250';
  box.innerHTML =
    '<img src="'+chartSrc+'" alt="K线图" style="width:100%;max-width:1000px;border:1px solid #eee;border-radius:6px;margin-bottom:10px" onerror="this.style.display=\'none\'"/>'
    + '<div style="display:flex;gap:12px;align-items:baseline;flex-wrap:wrap"><span style="font-size:1.3rem;font-weight:700;color:'+tc+'">'+esc(d.trend)+'</span>'
```

（即把原来 `box.innerHTML =` 之后的第一段 `'<div style="display:flex;...'` 前面加上 `var chartSrc=...;` 与 `'<img .../>' +`，其余行保持原样。）

- [ ] **Step 4: 运行确认通过**

Run: `cargo test -p xlh index_wires_chart_endpoint`
Expected: PASS。

- [ ] **Step 5: 手动验证（真实渲染）**

```bash
cargo run -- serve --port 8080
```
浏览器打开 `http://localhost:8080` → 「股诊断」Tab → 输入 `600519` → 诊断。预期：文字结论上方出现多面板 K线图（蜡烛红涨绿跌 + 量 + MACD + RSI）。数据不足的代码应显示「数据不足」占位图。

- [ ] **Step 6: 提交**

```bash
git add src/web/page.rs src/web/stock.rs
git commit -m "feat(web): 股诊断页嵌入 K线图

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review 备注

- **Spec 覆盖**：indicators series(Task1)、chart 渲染+占位(Task2)、端点+错误处理(Task3)、页面嵌入(Task4)、测试贯穿每任务。红涨绿跌、250 天 clamp、原始价、末值一致性均落到约束或代码。
- **已知风险**：plotters 具体 API（`CandleStick::new` 参数顺序/样式类型、`split_vertically` 像素类型、`ChartContext` 泛型标注）可能需按编译器提示微调；Task2 Step 5 已指出「不抽公共折线函数、直接内联」为最稳路径，避免泛型签名。
- **偏离**：SVG 代替 PNG（见 Global Constraints）。

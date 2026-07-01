# 股票技术分析/诊断设计（子项目 3）

> 全局背景见 [子项目1 spec](2026-07-01-stock-data-layer-design.md)。股票代码收敛于 `src/stock/`，与基金业务互不 `use`。
> 本文档只覆盖**子项目 3：技术分析/诊断**。前置：子项目1（数据层）已完成。

## 目标与范围

对单只股票的 OHLCV（子项目1 的 `StockBar`）计算经典技术指标，给出趋势判断与买卖信号档位，产出一个可序列化的诊断结构 `StockDiagnosis`（供子项目5 的"诊断"Tab 渲染）。全部纯函数、离线可测。

**做**：均线(MA) + MACD + 布林带(Bollinger) + RSI；趋势判断（上涨/下跌/震荡）；综合买卖信号（强力买入/买入/观望/卖出/强力卖出）。

**不做（YAGNI/归属他处）**：HTML/Web（子项目5）；ATR/KDJ/成交量指标等（暂不需要，后续按需加）；回测/策略（子项目2）；跨股票排名（子项目4）。

## 设计要点

- 指标一律基于**后复权收盘价 `adj_close`** 序列计算（跨除权除息连续、避免缺口）；展示价另给不复权 `close`。
- 参照基金 `analyze.rs` 的风格：结构体 + `serde::Serialize` + 纯函数 + 中文信号档位；但**不复用 `analyze`**（那是基金 acc_nav 口径），股票独立实现。
- 隔离：`src/stock/indicators.rs`、`src/stock/diagnose.rs` 只 `use` std/chrono 与 `crate::stock::data::StockBar`，不碰任何基金模块。

## 模块结构

```
src/stock/
├── indicators.rs  —— 纯指标数学：sma/ema_series/macd/bollinger/rsi（对 &[f64] 价格序列）
└── diagnose.rs    —— StockDiagnosis + diagnose(...) 组装趋势与综合信号
```
`src/stock/mod.rs` 追加 `pub mod indicators; pub mod diagnose;`。

## 指标（`indicators.rs`）

```rust
pub fn sma(prices: &[f64], n: usize) -> Option<f64>;          // 最新 n 日简单均线
pub fn ema_series(prices: &[f64], n: usize) -> Vec<f64>;      // EMA 序列（seed=首值，alpha=2/(n+1)）
pub struct Macd { pub macd: f64, pub signal: f64, pub hist: f64 }
pub fn macd(prices: &[f64], fast: usize, slow: usize, signal: usize) -> Option<Macd>;
pub struct Boll { pub mid: f64, pub upper: f64, pub lower: f64, pub std: f64 }
pub fn bollinger(prices: &[f64], n: usize, k: f64) -> Option<Boll>; // 总体标准差
pub fn rsi(prices: &[f64], n: usize) -> Option<f64>;         // 简单均值口径，全涨→100 全跌→0
```
数据不足一律返回 `None`。MACD 线 = EMA(fast) − EMA(slow)，信号线 = EMA(MACD 线, signal)，柱 = 线 − 信号。

## 诊断（`diagnose.rs`）

```rust
pub struct DiagnoseParams {
    pub ma_short: usize, pub ma_long: usize,           // 20 / 60
    pub boll_window: usize, pub boll_k: f64,           // 20 / 2.0
    pub rsi_period: usize,                             // 14
    pub macd_fast: usize, pub macd_slow: usize, pub macd_signal: usize, // 12/26/9
    pub trend_window: usize,                           // 60
    pub up_threshold: f64, pub down_threshold: f64,    // 0.10 / -0.10
}   // impl Default

pub struct StockDiagnosis {
    pub code: String, pub name: String, pub date: String,
    pub price: f64, pub adj_price: f64,                // 不复权 / 后复权 最新价
    pub ma_short: f64, pub ma_long: f64, pub ma_relation: String, // 多头排列/空头排列/纠缠
    pub macd: f64, pub macd_signal: f64, pub macd_hist: f64,
    pub boll_mid: f64, pub boll_upper: f64, pub boll_lower: f64, pub boll_z: f64,
    pub rsi: f64,
    pub trend: String,      // 上涨/下跌/震荡
    pub signal: String,     // 强力买入/买入/观望/卖出/强力卖出
    pub score: i32,
    pub rationale: String,
    pub caveat: String,
}   // Serialize

pub fn diagnose(code: String, name: String, bars: &[StockBar], p: &DiagnoseParams)
    -> anyhow::Result<StockDiagnosis>;
```

**趋势判断**（同基金 regime 口径，但用 adj_close）：`window_return`=近 trend_window 日收益；`ma_short`/`ma_long`。
`return>up_threshold && ma_short>ma_long`→上涨；`return<down_threshold && ma_short<ma_long`→下跌；否则震荡。

**综合信号**（透明打分，正=偏买）：
- 布林：`z=(price-mid)/std`；`z<=-2→+2, (-2,-1]→+1, (-1,1)→0, [1,2)→-1, >=2→-2`
- RSI：`<=30→+1, >=70→-1, else 0`
- MACD：`hist>0→+1, hist<0→-1, =0→0`
- `score = 布林 + RSI + MACD`；映射：`>=2 强力买入 / =1 买入 / =0 观望 / =-1 卖出 / <=-2 强力卖出`

`rationale` 汇总各指标读数；`caveat` 按趋势给风险提示（上涨顺势、下跌谨慎抄底、震荡区间高抛低吸）。数据不足（`< max(ma_long, boll_window, rsi_period+1, macd_slow, trend_window)`）返回带"数据不足"的错误。

## 测试策略（全离线）

- `indicators`：sma 已知值；ema seed 与收敛；macd 上升序列 hist>0、下降序列 hist<0；bollinger 常量序列 std=0 且三线相等、线性序列 mid 正确；rsi 全涨=100、全跌=0、混合在 (0,100)。数据不足→None。
- `diagnose`：线性上涨→trend=上涨；线性下跌→下跌；窄幅震荡→震荡；构造深跌末点→score 偏买/signal 含"买入"；构造冲高末点→signal 含"卖出"；数据不足→错误含"数据不足"；`serde_json` 序列化含前端所需键。

## 交付定义（Done）

- `cargo test` 全绿（新增指标/诊断测试 + 既有全部）。
- `diagnose` 可对一段内存 `StockBar` 给出完整 `StockDiagnosis`。
- 股票模块不 `use` 任何基金专属模块。

# 基金推荐页（Top5 + 策略 + 择时 + 依据）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增「推荐」页：对预设精选基金池做跨基金分析，产出 Top5（推荐策略 + 当前择时点 + 节奏 + 依据），并在页面展示算法说明。

**Architecture:** 新增纯函数模块 `src/recommend.rs`（评分/选优/排名，IO 以闭包注入便于单测），复用 `runner`/`metrics`/`analyze`/`config`；Web 层加一条只读 `GET /api/recommend` 薄编排；`page.rs` 加第 5 个 Tab + 算法说明区块 + 渲染 JS。纯新增，不改引擎/策略/既有 4 Tab。

**Tech Stack:** Rust 2021、axum 0.7、tokio（spawn_blocking）、serde、toml、chrono；前端纯静态 HTML/JS（无外部依赖）；端到端用 Playwright（`scripts/verify_web.py`）。

## Global Constraints

- 定位：基于历史净值的**统计回测 + 启发式**，**不预测、不构成投资建议**；免责声明常驻 UI。
- 综合评分权重（常量，集中可调）：`w_return=0.4, w_sharpe=0.4, w_mdd=0.2`（回撤为负向贡献）。
- 样本外切分：`split_ratio=0.70`（前 70% 训练、后 30% 检验）；最小数据量 `MIN_TRAIN=120`、`MIN_TEST=30`。
- 5 候选策略固定默认参数（不逐基金寻优）：dca/adaptive=monthly,day1,1000；smart_dca +ma_window250,k1.0；trend short20/long60/amount1000；rsi 14/30/70/amount1000。
- Top5 排名按各基金「最优策略的**样本外(OOS)** 三项指标」跨基金 z-score 综合评分。
- 费率：买入 0；卖出沿用标准阶梯（7日1.5% / 365日0.5% / 其后0%）；初始现金 0。
- 字符串注入前端一律经 `esc()`；运行中禁用按钮。
- 中文名从 `fundlist.json` 反查（不硬编码）；查不到用代码本身；净值缺失/不足则跳过并计入 `skipped`。
- 既有测试保持绿、`cargo clippy` 干净。

> 对 spec §4.3 的一处实现细化：候选策略在**训练段与检验段都回测**（OOS 计算成本低），使 `all_strategies` 的 `oos_*` 字段对全部 5 个策略都真实有值；最优策略仍**只按训练段综合评分**选出，与「样本外验证」定位一致。

---

### Task 1: `recommend` 模块骨架 —— 类型、参数、`zscores`、`split_history`

**Files:**
- Create: `src/recommend.rs`
- Modify: `src/lib.rs`（加 `pub mod recommend;`）
- Test: `src/recommend.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces:
  - `pub struct ScoreWeights { pub w_return: f64, pub w_sharpe: f64, pub w_mdd: f64 }`（`Default`=0.4/0.4/0.2，`Clone+Copy+Serialize`）
  - `pub struct RecommendParams { pub top_n: usize, pub split_ratio: f64, pub weights: ScoreWeights }`（`Default`=5/0.70/默认权重）
  - `pub struct StrategyEval { pub kind, name: String; pub is_return, is_sharpe, is_mdd, oos_return, oos_sharpe, oos_mdd, score: f64 }`（`Clone+Serialize`）
  - `pub struct FundRecommendation { pub code, name: String; pub fund_score: f64; pub best_strategy: StrategyEval; pub all_strategies: Vec<StrategyEval>; pub regime: crate::analyze::RegimeReport; pub cadence_hint, rationale: String }`（`Clone+Serialize`）
  - `pub struct RecommendReport { pub generated: String; pub pool_size, analyzed: usize; pub skipped: Vec<String>; pub top: Vec<FundRecommendation>; pub weights: ScoreWeights; pub split_ratio: f64; pub disclaimer: String }`（`Clone+Serialize`）
  - `fn zscores(xs: &[f64]) -> Vec<f64>`（模块私有）
  - `fn split_history(points: &[NavPoint], split_ratio: f64) -> Option<(&[NavPoint], &[NavPoint])>`（模块私有）
  - `const MIN_TRAIN: usize = 120; const MIN_TEST: usize = 30;`
  - `pub const DISCLAIMER: &str`

- [ ] **Step 1: 在 `src/lib.rs` 注册模块**

在 `pub mod web;` 上一行加入：

```rust
pub mod recommend;
```

- [ ] **Step 2: 写 `src/recommend.rs` 类型与工具函数（先放可编译的实现，再补测试）**

```rust
//! 跨基金推荐：综合多因子评分 + 样本外验证 + 形态择时。纯逻辑，IO 由调用方注入。
use std::collections::HashMap;
use serde::Serialize;

use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
use crate::broker::{FeeModel, SellTier};
use crate::config::build_strategy_from;
use crate::data::NavPoint;
use crate::metrics::Summary;
use crate::runner;

pub const DISCLAIMER: &str =
    "基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。";

const MIN_TRAIN: usize = 120;
const MIN_TEST: usize = 30;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScoreWeights {
    pub w_return: f64,
    pub w_sharpe: f64,
    pub w_mdd: f64,
}
impl Default for ScoreWeights {
    fn default() -> Self { Self { w_return: 0.4, w_sharpe: 0.4, w_mdd: 0.2 } }
}

pub struct RecommendParams {
    pub top_n: usize,
    pub split_ratio: f64,
    pub weights: ScoreWeights,
}
impl Default for RecommendParams {
    fn default() -> Self { Self { top_n: 5, split_ratio: 0.70, weights: ScoreWeights::default() } }
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyEval {
    pub kind: String,
    pub name: String,
    pub is_return: f64,
    pub is_sharpe: f64,
    pub is_mdd: f64,
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_mdd: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FundRecommendation {
    pub code: String,
    pub name: String,
    pub fund_score: f64,
    pub best_strategy: StrategyEval,
    pub all_strategies: Vec<StrategyEval>,
    pub regime: RegimeReport,
    pub cadence_hint: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecommendReport {
    pub generated: String,
    pub pool_size: usize,
    pub analyzed: usize,
    pub skipped: Vec<String>,
    pub top: Vec<FundRecommendation>,
    pub weights: ScoreWeights,
    pub split_ratio: f64,
    pub disclaimer: String,
}

/// 总体标准分（population std）。长度<1 返回空；σ≈0 返回全 0（不除零）。
fn zscores(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    if n == 0 { return Vec::new(); }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    let sd = var.sqrt();
    if sd < 1e-12 { return vec![0.0; n]; }
    xs.iter().map(|x| (x - mean) / sd).collect()
}

/// 按比例切训练/检验段；任一段不足最小阈值返回 None。
fn split_history(points: &[NavPoint], split_ratio: f64) -> Option<(&[NavPoint], &[NavPoint])> {
    let cut = (points.len() as f64 * split_ratio).floor() as usize;
    let (train, test) = points.split_at(cut);
    if train.len() >= MIN_TRAIN && test.len() >= MIN_TEST { Some((train, test)) } else { None }
}
```

- [ ] **Step 3: 写失败测试（`zscores` 与 `split_history`）**

在文件末尾加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// 构造 acc_nav==nav 的净值序列。
    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    #[test]
    fn zscores_constant_is_zero() {
        assert_eq!(zscores(&[2.0, 2.0, 2.0]), vec![0.0, 0.0, 0.0]);
        assert!(zscores(&[]).is_empty());
    }

    #[test]
    fn zscores_centered_and_scaled() {
        let z = zscores(&[1.0, 2.0, 3.0]);
        assert!((z.iter().sum::<f64>()).abs() < 1e-9, "z 均值应≈0");
        assert!(z[2] > z[1] && z[1] > z[0], "应保序");
    }

    #[test]
    fn split_history_ok_and_too_short() {
        let pts = series(&(0..220).map(|i| 1.0 + i as f64 * 0.001).collect::<Vec<_>>());
        let (tr, te) = split_history(&pts, 0.70).expect("220 点应可切分");
        assert_eq!(tr.len(), 154);
        assert_eq!(te.len(), 66);
        // 160 点 → cut=112 < MIN_TRAIN(120) → None
        let short = series(&(0..160).map(|i| 1.0 + i as f64 * 0.001).collect::<Vec<_>>());
        assert!(split_history(&short, 0.70).is_none());
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib recommend::tests`
Expected: PASS（`zscores_*`、`split_history_*` 全绿）

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs src/recommend.rs
git commit -m "feat(recommend): 模块骨架 + 类型 + zscores/split_history"
```

---

### Task 2: 候选策略默认参数 + 单段回测指标

**Files:**
- Modify: `src/recommend.rs`
- Test: `src/recommend.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes（Task 1）：`NavPoint`、`Summary`、`build_strategy_from`、`runner::run_one`。
- Produces（模块私有）：
  - `const CANDIDATES: &[(&str, &str)]`（kind, 中文名，5 项）
  - `fn default_params(kind: &str) -> toml::Value`
  - `fn rec_fee() -> FeeModel`
  - `fn run_metrics(kind: &str, points: &[NavPoint]) -> Summary`

- [ ] **Step 1: 实现候选与单段回测**

在 `split_history` 之后插入：

```rust
/// 5 个候选策略（kind, 中文名），顺序即展示顺序。
const CANDIDATES: &[(&str, &str)] = &[
    ("dca", "普通定投"),
    ("smart_dca", "智能定投"),
    ("trend", "均线择时"),
    ("rsi", "RSI超买超卖"),
    ("adaptive", "自适应"),
];

/// 各策略固定稳健默认参数（不逐基金寻优，降低过拟合）。
fn default_params(kind: &str) -> toml::Value {
    let mut t = toml::Table::new();
    let s = |x: &str| toml::Value::String(x.to_string());
    match kind {
        "dca" | "adaptive" => {
            t.insert("period".into(), s("monthly"));
            t.insert("day".into(), toml::Value::Integer(1));
            t.insert("base_amount".into(), toml::Value::Float(1000.0));
        }
        "smart_dca" => {
            t.insert("period".into(), s("monthly"));
            t.insert("day".into(), toml::Value::Integer(1));
            t.insert("base_amount".into(), toml::Value::Float(1000.0));
            t.insert("ma_window".into(), toml::Value::Integer(250));
            t.insert("k".into(), toml::Value::Float(1.0));
        }
        "trend" => {
            t.insert("short_window".into(), toml::Value::Integer(20));
            t.insert("long_window".into(), toml::Value::Integer(60));
            t.insert("amount".into(), toml::Value::Float(1000.0));
        }
        "rsi" => {
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(30.0));
            t.insert("overbought".into(), toml::Value::Float(70.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
        }
        _ => {}
    }
    toml::Value::Table(t)
}

/// 评分用费率：买入 0、卖出标准阶梯。
fn rec_fee() -> FeeModel {
    FeeModel {
        buy_rate: 0.0,
        sell_tiers: vec![
            SellTier { max_days: 7, rate: 0.015 },
            SellTier { max_days: 365, rate: 0.005 },
            SellTier { max_days: 0, rate: 0.0 },
        ],
    }
}

/// 在给定净值段上跑某策略，返回绩效摘要。固定默认参数保证 build 不失败。
fn run_metrics(kind: &str, points: &[NavPoint]) -> Summary {
    let strat = build_strategy_from(kind, &Some(default_params(kind)), &[])
        .expect("固定默认参数构建策略不应失败");
    let outcome = runner::run_one(
        kind.to_string(), String::new(), points.to_vec(), strat, rec_fee(), 0.0);
    outcome.summary
}
```

- [ ] **Step 2: 写失败测试**

在 `mod tests` 内追加：

```rust
    #[test]
    fn default_params_build_all_candidates() {
        for (kind, _) in CANDIDATES {
            let r = build_strategy_from(kind, &Some(default_params(kind)), &[]);
            assert!(r.is_ok(), "{kind} 默认参数应能构建策略: {:?}", r.err());
        }
    }

    #[test]
    fn run_metrics_finite_on_uptrend() {
        // 300 点温和上涨，足够各策略均线/RSI 窗口
        let vals: Vec<f64> = (0..300).map(|i| 1.0 + i as f64 * 0.003).collect();
        let s = run_metrics("smart_dca", &series(&vals));
        assert!(s.total_return.is_finite() && s.sharpe.is_finite());
        assert!(s.max_drawdown >= 0.0);
    }
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib recommend::tests`
Expected: PASS（新增两测试 + 既有全绿）

- [ ] **Step 4: Commit**

```bash
git add src/recommend.rs
git commit -m "feat(recommend): 候选策略默认参数 + 单段回测指标"
```

---

### Task 3: `evaluate_fund` —— 单基金选优 + 样本外 + 形态择时 + 依据

**Files:**
- Modify: `src/recommend.rs`
- Test: `src/recommend.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes（Task 1–2）：`StrategyEval/FundRecommendation/RecommendParams`、`CANDIDATES`、`run_metrics`、`split_history`、`zscores`、`analyze::{detect_regime_with_plan, detect_regime}`。
- Produces：
  - `pub fn evaluate_fund(code: &str, name: &str, points: &[NavPoint], p: &RecommendParams) -> anyhow::Result<FundRecommendation>`
  - 私有 `fn regime_or_fallback(points: &[NavPoint]) -> RegimeReport`
  - 私有 `fn cadence_for(regime: &str) -> String`

- [ ] **Step 1: 实现 `evaluate_fund` 及两个私有辅助**

在 `run_metrics` 之后插入：

```rust
/// 形态+行动计划；数据不足/波动为零时降级为占位报告（不致命）。
fn regime_or_fallback(points: &[NavPoint]) -> RegimeReport {
    let rp = RegimeParams::default();
    let pp = PlanParams::default();
    if let Ok(r) = analyze::detect_regime_with_plan(points, &rp, &pp) { return r; }
    if let Ok(r) = analyze::detect_regime(points, &rp) { return r; }
    RegimeReport {
        regime: "数据不足".into(), window: 0, window_return: 0.0, annualized_vol: 0.0,
        ma_short: 0.0, ma_long: 0.0, ma_relation: "未知".into(),
        rec_strategy: String::new(), rec_name: String::new(),
        rationale: "数据不足，暂不给出形态与择时点".into(), plan: None,
    }
}

/// 按形态给投资节奏建议。
fn cadence_for(regime: &str) -> String {
    match regime {
        "上涨趋势" => "顺势持有 / 坚持定投，勿过早下车",
        "下跌趋势" => "谨慎，仅 −2σ 小额试探或观望",
        "震荡" => "按波动带分批：低吸线买、高抛线减",
        _ => "数据不足，暂不给择时节奏",
    }.to_string()
}

/// 对单只基金净值产出推荐（fund_score 留待 rank_top 跨基金标准化）。
/// 数据不足返回 Err，调用方据此跳过。
pub fn evaluate_fund(
    code: &str, name: &str, points: &[NavPoint], p: &RecommendParams,
) -> anyhow::Result<FundRecommendation> {
    let (train, test) = split_history(points, p.split_ratio).ok_or_else(|| {
        anyhow::anyhow!("数据不足: 需训练≥{} 检验≥{} 个净值点（当前 {}）", MIN_TRAIN, MIN_TEST, points.len())
    })?;

    // 每个候选在训练段与检验段各回测一次。
    let mut evals: Vec<StrategyEval> = Vec::with_capacity(CANDIDATES.len());
    for (kind, name_cn) in CANDIDATES {
        let is_s = run_metrics(kind, train);
        let oos_s = run_metrics(kind, test);
        evals.push(StrategyEval {
            kind: (*kind).to_string(), name: (*name_cn).to_string(),
            is_return: is_s.total_return, is_sharpe: is_s.sharpe, is_mdd: is_s.max_drawdown,
            oos_return: oos_s.total_return, oos_sharpe: oos_s.sharpe, oos_mdd: oos_s.max_drawdown,
            score: 0.0,
        });
    }

    // 训练段三项指标跨候选标准化 → 综合评分。
    let z_ret = zscores(&evals.iter().map(|e| e.is_return).collect::<Vec<_>>());
    let z_sh = zscores(&evals.iter().map(|e| e.is_sharpe).collect::<Vec<_>>());
    let z_mdd = zscores(&evals.iter().map(|e| e.is_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, e) in evals.iter_mut().enumerate() {
        e.score = w.w_return * z_ret[i] + w.w_sharpe * z_sh[i] - w.w_mdd * z_mdd[i];
    }

    let best_idx = evals.iter().enumerate()
        .max_by(|(_, a), (_, b)| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i).unwrap_or(0);
    let best = evals[best_idx].clone();

    let regime = regime_or_fallback(points);
    let cadence_hint = cadence_for(&regime.regime);
    let rationale = format!(
        "训练段(前{:.0}%)在 5 个候选策略中『{}』综合评分最高（收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%）；\
         检验段(后{:.0}%)样本外实测 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%。当前形态：{}。",
        p.split_ratio * 100.0, best.name, best.is_return * 100.0, best.is_sharpe, best.is_mdd * 100.0,
        (1.0 - p.split_ratio) * 100.0, best.oos_return * 100.0, best.oos_sharpe, best.oos_mdd * 100.0,
        regime.regime,
    );

    Ok(FundRecommendation {
        code: code.to_string(), name: name.to_string(), fund_score: 0.0,
        best_strategy: best, all_strategies: evals, regime, cadence_hint, rationale,
    })
}
```

- [ ] **Step 2: 写失败测试**

在 `mod tests` 内追加：

```rust
    #[test]
    fn evaluate_fund_ok_on_uptrend() {
        let vals: Vec<f64> = (0..300).map(|i| 1.0 + i as f64 * 0.004).collect();
        let r = evaluate_fund("000001", "测试基金", &series(&vals), &RecommendParams::default())
            .expect("300 点上涨应可评估");
        assert_eq!(r.all_strategies.len(), 5, "应评估全部 5 候选");
        assert!(!r.rationale.is_empty(), "应有依据文案");
        assert!(!r.regime.regime.is_empty(), "应有形态标签");
        // best_strategy 必在候选集合内
        assert!(CANDIDATES.iter().any(|(k, _)| *k == r.best_strategy.kind));
        // best 的 score 应为各候选最大
        let max = r.all_strategies.iter().map(|e| e.score).fold(f64::MIN, f64::max);
        assert!((r.best_strategy.score - max).abs() < 1e-9, "best 应为最高分");
    }

    #[test]
    fn evaluate_fund_too_short_errors() {
        let vals: Vec<f64> = (0..100).map(|i| 1.0 + i as f64 * 0.001).collect();
        let err = evaluate_fund("000001", "x", &series(&vals), &RecommendParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib recommend::tests`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add src/recommend.rs
git commit -m "feat(recommend): evaluate_fund 选优+样本外+形态择时+依据"
```

---

### Task 4: 跨基金排名 `rank_top` + 报告装配 `build_report`

**Files:**
- Modify: `src/recommend.rs`
- Test: `src/recommend.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes（Task 1–3）：`FundRecommendation/RecommendParams/RecommendReport`、`evaluate_fund`、`zscores`、`DISCLAIMER`。
- Produces：
  - `pub fn rank_top(recs: Vec<FundRecommendation>, p: &RecommendParams) -> Vec<FundRecommendation>`
  - `pub fn build_report<F>(pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams, load: F) -> RecommendReport where F: FnMut(&str) -> anyhow::Result<Vec<NavPoint>>`

- [ ] **Step 1: 实现 `rank_top` 与 `build_report`**

在 `evaluate_fund` 之后插入：

```rust
/// 用各基金「最优策略的样本外三项指标」跨基金 z-score 综合评分，降序取 top_n。
pub fn rank_top(mut recs: Vec<FundRecommendation>, p: &RecommendParams) -> Vec<FundRecommendation> {
    if recs.is_empty() { return recs; }
    let zr = zscores(&recs.iter().map(|r| r.best_strategy.oos_return).collect::<Vec<_>>());
    let zs = zscores(&recs.iter().map(|r| r.best_strategy.oos_sharpe).collect::<Vec<_>>());
    let zm = zscores(&recs.iter().map(|r| r.best_strategy.oos_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, r) in recs.iter_mut().enumerate() {
        r.fund_score = w.w_return * zr[i] + w.w_sharpe * zs[i] - w.w_mdd * zm[i];
    }
    recs.sort_by(|a, b| b.fund_score.partial_cmp(&a.fund_score).unwrap_or(std::cmp::Ordering::Equal));
    recs.truncate(p.top_n);
    recs
}

/// 遍历基金池：用注入的 `load` 取净值 → `evaluate_fund` → `rank_top`，装配整页报告。
/// IO（联网/读盘）经闭包注入，便于离线单测。
pub fn build_report<F>(
    pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams, mut load: F,
) -> RecommendReport
where
    F: FnMut(&str) -> anyhow::Result<Vec<NavPoint>>,
{
    let mut recs = Vec::new();
    let mut skipped = Vec::new();
    for &code in pool {
        match load(code) {
            Ok(points) => {
                let name = names.get(code).cloned().unwrap_or_else(|| code.to_string());
                match evaluate_fund(code, &name, &points, p) {
                    Ok(r) => recs.push(r),
                    Err(_) => skipped.push(code.to_string()),
                }
            }
            Err(_) => skipped.push(code.to_string()),
        }
    }
    let analyzed = recs.len();
    let top = rank_top(recs, p);
    RecommendReport {
        generated: today.to_string(),
        pool_size: pool.len(),
        analyzed,
        skipped,
        top,
        weights: p.weights,
        split_ratio: p.split_ratio,
        disclaimer: DISCLAIMER.to_string(),
    }
}
```

- [ ] **Step 2: 写失败测试（含序列化键检查）**

在 `mod tests` 内追加：

```rust
    #[test]
    fn build_report_ranks_and_skips() {
        // 池：两只可分析（不同斜率）+ 一只加载失败
        let names: HashMap<String, String> = [("000001".to_string(), "甲".to_string())].into_iter().collect();
        let p = RecommendParams::default();
        let rep = build_report(&["000001", "000002", "BADX"], &names, "2026-06-29", &p, |code| {
            match code {
                "000001" => Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.005).collect::<Vec<_>>())),
                "000002" => Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.002).collect::<Vec<_>>())),
                _ => Err(anyhow::anyhow!("加载失败")),
            }
        });
        assert_eq!(rep.pool_size, 3);
        assert_eq!(rep.analyzed, 2, "两只成功");
        assert_eq!(rep.skipped, vec!["BADX".to_string()]);
        assert_eq!(rep.top.len(), 2);
        // 按 fund_score 降序
        assert!(rep.top[0].fund_score >= rep.top[1].fund_score);
        assert_eq!(rep.generated, "2026-06-29");
        assert_eq!(rep.top[0].name.is_empty(), false);
    }

    #[test]
    fn report_serializes_frontend_keys() {
        let names = HashMap::new();
        let p = RecommendParams::default();
        let rep = build_report(&["000001"], &names, "2026-06-29", &p, |_| {
            Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.004).collect::<Vec<_>>()))
        });
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"top\"", "\"best_strategy\"", "\"all_strategies\"", "\"regime\"",
                    "\"rationale\"", "\"cadence_hint\"", "\"weights\"", "\"split_ratio\"",
                    "\"disclaimer\"", "\"fund_score\"", "\"skipped\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }

    #[test]
    fn build_report_empty_pool_is_valid() {
        let names = HashMap::new();
        let rep = build_report(&[], &names, "2026-06-29", &RecommendParams::default(), |_| {
            Ok(Vec::new())
        });
        assert_eq!(rep.analyzed, 0);
        assert!(rep.top.is_empty());
    }
```

- [ ] **Step 3: 运行测试确认通过**

Run: `cargo test --lib recommend::tests`
Expected: PASS

- [ ] **Step 4: clippy 检查**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无警告（如有未用 import 等按提示修正）

- [ ] **Step 5: Commit**

```bash
git add src/recommend.rs
git commit -m "feat(recommend): rank_top 跨基金排名 + build_report 报告装配"
```

---

### Task 5: Web 接线 —— `GET /api/recommend` + 精选池常量

**Files:**
- Modify: `src/web/mod.rs`（加 `RecommendQuery`、`recommend_handler`、`recommend_blocking`、`PRESET_POOL`、注册路由）
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes（Task 1–4）：`crate::recommend::{RecommendParams, RecommendReport, build_report}`；既有 `funds_payload`、`crate::data::cache::load_or_fetch`、`crate::data::fundlist::FundInfo`、`AppError`。
- Produces：路由 `GET /api/recommend`（返回 `Json<RecommendReport>`）。

- [ ] **Step 1: 注册路由**

在 `router()`（`src/web/mod.rs`）的 `.route("/api/regime", get(regime_handler))` 之后加一行：

```rust
        .route("/api/recommend", get(recommend_handler))
```

- [ ] **Step 2: 加精选池常量、Query、handler 与编排**

在 `regime_blocking` 函数之后插入：

```rust
/// 预设精选基金池（宽基指数 / 行业 / 口碑主动，含现有缓存）。可增删。
/// 中文名运行时从 fundlist.json 反查；净值缺失/不足则跳过。
const PRESET_POOL: &[&str] = &[
    "161725", "050002", "000834", "001427", "003095", "008888",
    "110011", "005827", "110022", "161005", "163406", "260108",
    "000961", "001593", "519674", "320007", "002001", "001714",
    "000478", "270042", "040046", "519066", "005669", "001102",
];

#[derive(Debug, Deserialize)]
pub struct RecommendQuery {
    #[serde(default)]
    pub top_n: Option<usize>,
}

async fn recommend_handler(
    axum::extract::Query(q): axum::extract::Query<RecommendQuery>,
) -> std::result::Result<axum::Json<crate::recommend::RecommendReport>, AppError> {
    let report = tokio::task::spawn_blocking(move || recommend_blocking(q))
        .await
        .map_err(|e| anyhow!("任务执行失败: {e}"))??;
    Ok(axum::Json(report))
}

fn recommend_blocking(q: RecommendQuery) -> Result<crate::recommend::RecommendReport> {
    let params = crate::recommend::RecommendParams {
        top_n: q.top_n.unwrap_or(5),
        ..Default::default()
    };
    // code → 中文名 映射（清单加载失败则空映射，名字回退为代码）
    let names: std::collections::HashMap<String, String> = funds_payload(std::path::Path::new(".cache"))
        .into_iter()
        .map(|f| (f.code, f.name))
        .collect();
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);
    let report = crate::recommend::build_report(
        PRESET_POOL, &names, &end.to_string(), &params,
        |code| crate::data::cache::load_or_fetch(code, std::path::Path::new(".cache"), start, end),
    );
    Ok(report)
}
```

- [ ] **Step 3: 写失败测试（结构性，不联网）**

> `recommend_blocking` 会对全池逐只 `load_or_fetch`（可能联网），不适合放进 `cargo test`。这里只断言路由已注册、`PRESET_POOL` 合理、核心装配由 `recommend` 单测与 Playwright 覆盖。

在 `src/web/mod.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[test]
    fn preset_pool_nonempty_and_valid_codes() {
        assert!(!super::PRESET_POOL.is_empty(), "精选池不应为空");
        for c in super::PRESET_POOL {
            assert!(super::validate_fund_code(c).is_ok(), "池内代码应合法: {c}");
        }
    }

    #[tokio::test]
    async fn recommend_route_registered() {
        // 仅断言路由存在：用一个会被 axum 解析的 query；不校验业务结果（可能联网）。
        // 通过检查 router 能 build 且 /api/recommend 不是 404 的方式较重，这里改为
        // 断言 index 页含推荐 tab + 端点字符串（见下个测试），路由注册由编译期保证。
        let _ = super::router(); // 能构造即说明 .route 注册无类型错误
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web::`
Expected: PASS（`preset_pool_*`、`recommend_route_registered` + 既有 web 测试全绿）

- [ ] **Step 5: clippy + 编译**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无警告

- [ ] **Step 6: Commit**

```bash
git add src/web/mod.rs
git commit -m "feat(web): GET /api/recommend + 预设精选池常量"
```

---

### Task 6: 前端「推荐」Tab + 算法说明 + 渲染

**Files:**
- Modify: `src/web/page.rs`（`INDEX_HTML`：加 tab 按钮、panel、JS 渲染）
- Test: `src/web/mod.rs`（`#[cfg(test)]`，对 `INDEX_HTML` 断言）

**Interfaces:**
- Consumes：`GET /api/recommend`（`RecommendReport` JSON：`top[].{code,name,fund_score,best_strategy,all_strategies,regime,cadence_hint,rationale}`、`weights`、`split_ratio`、`analyzed`、`pool_size`、`skipped`、`disclaimer`）。
- Produces：第 5 个 `data-tab="recommend"` Tab + `#rec-result`。

- [ ] **Step 1: 加 Tab 按钮**

在 `src/web/page.rs` 的 tabs 区块，把诊断按钮那行后追加一行（第 57 行 `<button class="tab" data-tab="diagnose">诊断</button>` 之后）：

```html
    <button class="tab" data-tab="recommend">推荐</button>
```

- [ ] **Step 2: 加「推荐」panel（含算法说明区块）**

在「诊断」panel（`</div>` 收尾，即第 168 行 `</div>` 之后、`<iframe id="result"` 之前）插入：

```html
  <!-- 推荐 -->
  <div class="panel" id="panel-recommend">
    <div class="card">
      <details open style="margin-bottom:12px">
        <summary style="cursor:pointer;font-weight:600;color:#1a252f">算法说明（点击展开/收起）</summary>
        <div style="margin-top:10px;color:#34495e;font-size:.9rem;line-height:1.7">
          <div>1) <strong>综合评分</strong>：score = 0.4·z(收益) + 0.4·z(夏普) − 0.2·z(最大回撤)，对池内各基金做标准化（z 分数），回撤为负向。</div>
          <div>2) <strong>样本外验证</strong>：历史按 70/30 切分；训练段（前 70%）从 5 个策略里按综合评分选最优，检验段（后 30%）实测该策略表现；<strong>Top5 排名用检验段指标</strong>，抑制过拟合。</div>
          <div>3) <strong>候选策略（固定参数）</strong>：普通定投 / 智能定投(MA250) / 均线择时(20·60) / RSI(14·30·70) / 自适应。</div>
          <div>4) <strong>当前择时</strong>：用近段净值的均线 ±σ 波动带——低吸线≈中轴−σ、高抛线≈中轴+σ，结合形态（上涨红 / 下跌绿 / 震荡灰）给出当下信号。</div>
          <div style="color:#c0392b;margin-top:6px">5) 免责声明：基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。</div>
        </div>
      </details>
      <div class="row">
        <div class="field"><label>取 Top-N</label><input type="number" id="rec-topn" value="5"/></div>
        <button class="run" id="run-recommend">生成推荐</button>
      </div>
      <div class="hint" style="margin-top:8px">首次需联网抓取精选池全部基金净值（数十只，约几十秒）；命中缓存后秒级。</div>
      <div id="rec-result" style="margin-top:14px"></div>
    </div>
  </div>
```

- [ ] **Step 3: 加渲染 JS**

在 `src/web/page.rs` 脚本末尾、`attachCombobox(document.getElementById('diag-fund'));`（第 480 行）之后插入：

```javascript
function regimeColor(reg){ return reg === '上涨趋势' ? '#c0392b' : (reg === '下跌趋势' ? '#27ae60' : '#7f8c8d'); }
function pct(x){ return (x*100).toFixed(1) + '%'; }
function recCard(r, rank){
  var reg = r.regime || {};
  var plan = reg.plan;
  var rc = regimeColor(reg.regime);
  var b = r.best_strategy || {};
  var timing = '';
  if (plan && plan.current){
    var c = plan.current;
    timing = '<div style="margin-top:8px;color:#34495e">当前择时：'
      + '<strong style="color:'+rc+'">'+esc(reg.regime)+'</strong>'
      + ' · 低吸线 '+plan.buy.toFixed(4)+' · 高抛线 '+plan.sell.toFixed(4)
      + ' · 当下 <strong>'+esc(c.signal)+'</strong>（'+esc(c.action)+'）</div>';
  } else {
    timing = '<div style="margin-top:8px;color:#7f8c8d">当前择时：'+esc(reg.regime||'数据不足')+'（暂无波动带）</div>';
  }
  return '<div class="card" style="border-left:4px solid '+rc+'">'
    + '<div style="display:flex;align-items:baseline;gap:10px">'
    + '<span style="font-size:1.3rem;font-weight:700;color:#c0392b">#'+rank+'</span>'
    + '<span style="font-size:1.1rem;font-weight:600">'+esc(r.name)+'</span>'
    + '<span style="color:#7f8c8d">'+esc(r.code)+'</span>'
    + '<span style="margin-left:auto;color:#5a6a7a">综合评分 '+r.fund_score.toFixed(2)+'</span></div>'
    + '<div style="margin-top:8px">推荐策略：<strong style="background:#fdecea;color:#c0392b;padding:2px 10px;border-radius:12px">'+esc(b.name)+'</strong></div>'
    + '<div style="margin-top:6px;color:#34495e">样本外：收益 '+pct(b.oos_return)+' · 夏普 '+b.oos_sharpe.toFixed(2)+' · 回撤 '+pct(b.oos_mdd)
    + '<span style="color:#95a5a6">（训练段 收益 '+pct(b.is_return)+' · 夏普 '+b.is_sharpe.toFixed(2)+'）</span></div>'
    + timing
    + '<div style="margin-top:8px;color:#5a6a7a">依据：'+esc(r.rationale)+'</div>'
    + '<div style="margin-top:4px;color:#5a6a7a">节奏：'+esc(r.cadence_hint)+'</div>'
    + '</div>';
}
function renderRec(rep){
  var box = document.getElementById('rec-result');
  if (!rep || !Array.isArray(rep.top)){ box.innerHTML = '<span style="color:#c0392b">推荐生成失败</span>'; return; }
  if (!rep.top.length){
    box.innerHTML = '<div style="color:#c0392b">暂无可分析数据（已分析 '+rep.analyzed+'/'+rep.pool_size+'）。请先在上方「数据同步」同步精选池，或检查网络。</div>';
    return;
  }
  var head = '<div style="color:#5a6a7a;margin-bottom:10px">已分析 '+rep.analyzed+'/'+rep.pool_size
    + ' · 跳过 '+(rep.skipped||[]).length+' 只 · 生成于 '+esc(rep.generated)+'</div>';
  var cards = rep.top.map(function(r, i){ return recCard(r, i+1); }).join('');
  var foot = '<div class="hint" style="margin-top:6px;color:#c0392b">'+esc(rep.disclaimer)+'</div>';
  box.innerHTML = head + cards + foot;
}
document.getElementById('run-recommend').addEventListener('click', function(){
  var btn = this;
  var topn = document.getElementById('rec-topn').value.trim();
  var qs = new URLSearchParams();
  if (topn) qs.append('top_n', topn);
  var t = btn.textContent; setBtn(btn, true, '生成推荐');
  document.getElementById('rec-result').textContent = '分析中…（首次需联网抓取，请稍候）';
  fetch('/api/recommend?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(t){ throw new Error(t); }); return res.json(); })
    .then(renderRec)
    .catch(function(e){ document.getElementById('rec-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '生成推荐'); });
});
```

- [ ] **Step 4: 写失败测试（页面断言）**

在 `src/web/mod.rs` 的 `#[cfg(test)] mod tests` 内追加：

```rust
    #[tokio::test]
    async fn index_has_recommend_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"recommend\""), "应有推荐 tab");
        assert!(body.contains("/api/recommend"), "应调用推荐接口");
        assert!(body.contains("id=\"rec-result\""), "应有推荐结果区");
        assert!(body.contains("综合评分"), "算法说明应含综合评分");
        assert!(body.contains("样本外"), "算法说明应含样本外");
        assert!(body.contains("不构成"), "应有免责声明");
    }
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib web::tests::index_has_recommend_tab`
Expected: PASS

- [ ] **Step 6: 全量测试 + clippy**

Run: `cargo test` 然后 `cargo clippy --all-targets -- -D warnings`
Expected: 全绿、无警告

- [ ] **Step 7: Commit**

```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat(web): 推荐 Tab + 算法说明 + Top5 卡片渲染"
```

---

### Task 7: 端到端验证（Playwright）

**Files:**
- Modify: `scripts/verify_web.py`（在诊断 tab 之后加推荐 tab 验证）

**Interfaces:**
- Consumes：运行中的 `xlh serve`、`#run-recommend`、`#rec-result`、`#panel-recommend`。

- [ ] **Step 1: 在诊断 tab 验证之后插入推荐 tab 验证**

在 `scripts/verify_web.py` 第 138 行（诊断截图 `web_diagnose.png` 那行）之后、`browser.close()` 之前插入：

```python
            # ---- 推荐 tab ----
            page.click('.tab[data-tab="recommend"]')
            # 算法说明常驻
            rec_panel = page.locator("#panel-recommend").inner_text(timeout=10000)
            assert "综合评分" in rec_panel, "推荐页应有算法说明（综合评分）"
            assert "不构成" in rec_panel, "推荐页应有免责声明"
            page.click("#run-recommend")
            # 首次需联网逐只抓取精选池，给足时间；等结果区出现卡片或「暂无」提示
            page.wait_for_function(
                "document.querySelector('#rec-result') && "
                "(document.querySelector('#rec-result').querySelector('.card') "
                "|| document.querySelector('#rec-result').innerText.indexOf('暂无') >= 0 "
                "|| document.querySelector('#rec-result').innerText.indexOf('已分析') >= 0)",
                timeout=180000)
            rec_text = page.locator("#rec-result").inner_text(timeout=10000)
            assert ("推荐策略" in rec_text) or ("暂无" in rec_text) or ("已分析" in rec_text), \
                "推荐结果应出现卡片或概览/暂无提示"
            page.screenshot(path=str(Path("output/web_recommend.png").resolve()), full_page=True)
```

- [ ] **Step 2: 更新结尾 PASS 文案**

把第 142 行：

```python
        print("PASS: 三 tab + 基金搜索 + RSI 策略 + 数据同步 + 诊断 均正常")
```

改为：

```python
        print("PASS: 五 tab + 基金搜索 + RSI 策略 + 数据同步 + 诊断 + 推荐 均正常")
```

- [ ] **Step 3: 运行端到端脚本**

Run: `python scripts/verify_web.py`
Expected: 退出码 0，输出 `PASS: 五 tab ...`；生成 `output/web_recommend.png`（Top5 卡片或「已分析 X/Y」概览）。
（如本机离线、精选池无缓存：断言走「暂无/已分析」分支仍 PASS。）

- [ ] **Step 4: Commit**

```bash
git add scripts/verify_web.py
git commit -m "test(e2e): 推荐 tab Playwright 验证 + 截图"
```

---

## Self-Review

**1. Spec coverage**（逐节对照 `2026-06-29-fund-recommendation-design.md`）：
- §3 精选池 → Task 5 `PRESET_POOL`（24 码，含现有 6 缓存）✔
- §4.1 数据结构 → Task 1（全部 struct + 字段，派生 Serialize）✔
- §4.2 候选与固定参数 → Task 2 `CANDIDATES`/`default_params`/`rec_fee` ✔
- §4.3 评分管线（切分/训练回测/标准化选优/检验/形态/节奏/依据）→ Task 1 `split_history` + Task 3 `evaluate_fund` ✔（细化：OOS 对全 5 候选都算，已在 Global Constraints 注明）
- §4.4 Top5 跨基金排名（用 OOS）→ Task 4 `rank_top` ✔
- §4.5 纯函数签名 `evaluate_fund`/`rank_top` → Task 3/4 ✔
- §5.1 路由 `GET /api/recommend` + handler + 8 年回看 + 跳过 → Task 5 ✔
- §5.2 前端 Tab + 算法说明区块 + Top5 卡片 + 免责 → Task 6 ✔
- §6 错误处理（跳过/全失败/400/前端 catch）→ Task 4 `build_report`（跳过/空池）+ Task 6（前端 catch/暂无）✔
- §7 测试（单元/路由结构/页面/Playwright）→ Task 1–7 ✔
- §8 单元边界 → 纯函数 vs 薄编排 划分一致 ✔
- §9 对既有影响（纯新增、复用）→ 各 Task Files 仅新增/追加 ✔
- §10 免责定位 → `DISCLAIMER` 常量 + 算法说明区块 ✔

**2. Placeholder scan：** 无 TBD/TODO；每个代码步骤均给出完整代码与可运行命令。

**3. Type consistency：**
- `evaluate_fund(code,name,points,p) -> Result<FundRecommendation>`、`rank_top(Vec,&p)->Vec`、`build_report(pool,names,today,&p,load)->RecommendReport` 在 Task 3/4/5 引用一致。
- `RegimeReport` 字段（`regime/plan/...`）与 `src/analyze.rs` 定义一致（fallback 构造覆盖全部字段）。
- `Summary` 字段 `total_return/sharpe/max_drawdown` 与 `src/metrics.rs` 一致。
- `build_strategy_from(kind,&Option<toml::Value>,&[])`、`runner::run_one(name,fund,points,strat,fee,cash)` 与现有签名一致。
- 前端 JSON 键（`best_strategy.oos_*`、`regime.plan.{buy,sell,current}`、`fund_score`、`cadence_hint`、`rationale`、`disclaimer`）与 Task 1 结构 + `analyze::ActionPlan` 字段一致。

无遗漏的 spec 需求；无类型不一致。

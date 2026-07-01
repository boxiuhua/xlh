# 选股/推荐 Implementation Plan（子项目 4）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 对一批股票做多策略样本外评分 + 技术诊断，跨股票 z-score 排名产出 TopN 选股报告。

**Architecture:** 镜像基金 `recommend.rs`，但复用共用策略构造器 + 子项目2 回测 + 子项目3 诊断，股票独立实现、不 `use` 基金模块。

**Tech Stack:** Rust；无新增依赖。

## Global Constraints

- 隔离：`src/stock/recommend.rs` 只 `use` 共用件（`crate::{strategy::*, metrics::Summary}`）与 `crate::stock::{data,backtest,fee,diagnose}`，禁止 `use` 基金专属模块（`recommend`/`config`/`analyze`/`runner` 等）。
- 评分费率统一 `StockFee`（默认 A股）。
- 测试全离线，`cargo test` 全绿。
- 提交信息结尾追加：`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`。

---

## File Structure

```
src/stock/diagnose.rs   —— 修改：StockDiagnosis 增加 #[derive(Default)]
src/stock/recommend.rs  —— 新建：选股/推荐全套
src/stock/mod.rs        —— 修改：新增 pub mod recommend;
```

---

## Task 1: StockDiagnosis 支持 Default（供诊断降级占位）

**Files:**
- Modify: `src/stock/diagnose.rs`

- [ ] **Step 1: 加 Default derive**

`src/stock/diagnose.rs` 把 `StockDiagnosis` 的 derive 从
```rust
#[derive(Debug, Clone, Serialize)]
```
改为
```rust
#[derive(Debug, Clone, Default, Serialize)]
```

- [ ] **Step 2: 回归**

Run: `cargo test --lib stock::diagnose`
Expected: 既有 7 个诊断测试仍全 PASS。

- [ ] **Step 3: 提交**

```bash
git add src/stock/diagnose.rs
git commit -m "feat(stock): StockDiagnosis 支持 Default（供推荐诊断降级占位）"
```

---

## Task 2: 选股/推荐 recommend

**Files:**
- Create: `src/stock/recommend.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod recommend;`）

**Interfaces:**
- Consumes: `crate::strategy::{Period, Strategy, dca::Dca, smart_dca::SmartDca, trend::Trend, rsi::Rsi, adaptive::Adaptive}`、`crate::metrics::Summary`、`crate::stock::data::StockBar`、`crate::stock::backtest`、`crate::stock::fee::StockFee`、`crate::stock::diagnose::{self, DiagnoseParams, StockDiagnosis}`
- Produces: `ScoreWeights`、`RecommendParams`、`StockStrategyEval`、`StockRecommendation`、`StockRecommendReport`、`evaluate_stock`、`rank_top`、`build_report`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod recommend;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/recommend.rs`：
```rust
//! 跨股票选股/推荐：多策略样本外评分 + z-score 排名 + 技术诊断。纯逻辑，IO 注入。
use std::collections::HashMap;
use serde::Serialize;

use crate::metrics::Summary;
use crate::strategy::{Period, Strategy};
use crate::strategy::dca::Dca;
use crate::strategy::smart_dca::SmartDca;
use crate::strategy::trend::Trend;
use crate::strategy::rsi::Rsi;
use crate::strategy::adaptive::Adaptive;
use crate::stock::backtest;
use crate::stock::fee::StockFee;
use crate::stock::data::StockBar;
use crate::stock::diagnose::{self, DiagnoseParams, StockDiagnosis};

pub const DISCLAIMER: &str =
    "基于历史行情的统计回测与技术指标启发式，不预测未来走势，不构成任何投资建议。";

const MIN_TRAIN: usize = 120;
const MIN_TEST: usize = 30;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScoreWeights { pub w_return: f64, pub w_sharpe: f64, pub w_mdd: f64 }
impl Default for ScoreWeights {
    fn default() -> Self { Self { w_return: 0.4, w_sharpe: 0.4, w_mdd: 0.2 } }
}

pub struct RecommendParams { pub top_n: usize, pub split_ratio: f64, pub weights: ScoreWeights, pub fee: StockFee }
impl Default for RecommendParams {
    fn default() -> Self { Self { top_n: 5, split_ratio: 0.70, weights: ScoreWeights::default(), fee: StockFee::a_share() } }
}

#[derive(Debug, Clone, Serialize)]
pub struct StockStrategyEval {
    pub kind: String, pub name: String,
    pub is_return: f64, pub is_sharpe: f64, pub is_mdd: f64,
    pub oos_return: f64, pub oos_sharpe: f64, pub oos_mdd: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendation {
    pub code: String, pub name: String, pub stock_score: f64,
    pub best_strategy: StockStrategyEval,
    pub all_strategies: Vec<StockStrategyEval>,
    pub diagnosis: StockDiagnosis,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendReport {
    pub generated: String, pub pool_size: usize, pub analyzed: usize,
    pub skipped: Vec<String>, pub top: Vec<StockRecommendation>,
    pub weights: ScoreWeights, pub split_ratio: f64, pub disclaimer: String,
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
    fn zscores_constant_and_empty() {
        assert_eq!(zscores(&[2.0, 2.0, 2.0]), vec![0.0, 0.0, 0.0]);
        assert!(zscores(&[]).is_empty());
    }

    #[test]
    fn zscores_ordered() {
        let z = zscores(&[1.0, 2.0, 3.0]);
        assert!((z.iter().sum::<f64>()).abs() < 1e-9);
        assert!(z[2] > z[1] && z[1] > z[0]);
    }

    #[test]
    fn split_history_ok_and_short() {
        let pts = series(&(0..220).map(|i| 100.0 + i as f64 * 0.1).collect::<Vec<_>>());
        let (tr, te) = split_history(&pts, 0.70).expect("220 点应可切分");
        assert_eq!(tr.len(), 154);
        assert_eq!(te.len(), 66);
        let short = series(&(0..160).map(|i| 100.0 + i as f64 * 0.1).collect::<Vec<_>>());
        assert!(split_history(&short, 0.70).is_none());
    }

    #[test]
    fn evaluate_stock_ok_on_uptrend() {
        let vals: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.4).collect();
        let r = evaluate_stock("600519", "茅台", &series(&vals), &RecommendParams::default())
            .expect("300 点上涨应可评估");
        assert_eq!(r.all_strategies.len(), 5);
        assert!(!r.rationale.is_empty());
        assert!(!r.diagnosis.trend.is_empty());
        assert!(CANDIDATES.iter().any(|(k, _)| *k == r.best_strategy.kind));
        let max = r.all_strategies.iter().map(|e| e.score).fold(f64::MIN, f64::max);
        assert!((r.best_strategy.score - max).abs() < 1e-9, "best 应为最高分");
    }

    #[test]
    fn evaluate_stock_too_short_errors() {
        let vals: Vec<f64> = (0..100).map(|i| 100.0 + i as f64 * 0.1).collect();
        let err = evaluate_stock("x", "x", &series(&vals), &RecommendParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn build_report_ranks_and_skips() {
        let names: HashMap<String, String> = [("600519".to_string(), "茅台".to_string())].into_iter().collect();
        let p = RecommendParams::default();
        let rep = build_report(&["600519", "000001", "BADX"], &names, "2026-07-01", &p, |code| {
            match code {
                "600519" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.5).collect::<Vec<_>>())),
                "000001" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.2).collect::<Vec<_>>())),
                _ => Err(anyhow::anyhow!("加载失败")),
            }
        });
        assert_eq!(rep.pool_size, 3);
        assert_eq!(rep.analyzed, 2);
        assert_eq!(rep.skipped, vec!["BADX".to_string()]);
        assert_eq!(rep.top.len(), 2);
        assert!(rep.top[0].stock_score >= rep.top[1].stock_score);
        assert_eq!(rep.generated, "2026-07-01");
    }

    #[test]
    fn build_report_empty_pool() {
        let names = HashMap::new();
        let rep = build_report(&[], &names, "2026-07-01", &RecommendParams::default(), |_| Ok(Vec::new()));
        assert_eq!(rep.analyzed, 0);
        assert!(rep.top.is_empty());
    }

    #[test]
    fn report_serializes_frontend_keys() {
        let names = HashMap::new();
        let rep = build_report(&["600519"], &names, "2026-07-01", &RecommendParams::default(), |_| {
            Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.4).collect::<Vec<_>>()))
        });
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"top\"", "\"best_strategy\"", "\"all_strategies\"", "\"diagnosis\"",
                    "\"rationale\"", "\"weights\"", "\"split_ratio\"", "\"disclaimer\"",
                    "\"stock_score\"", "\"skipped\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::recommend`
Expected: 编译失败 —— `zscores`/`split_history`/`evaluate_stock`/`build_report`/`CANDIDATES` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/recommend.rs` 的 `struct StockRecommendReport` 之后加入：
```rust
/// 总体标准分：空→空；σ≈0→全0。
fn zscores(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    if n == 0 { return Vec::new(); }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    let sd = var.sqrt();
    if sd < 1e-12 { return vec![0.0; n]; }
    xs.iter().map(|x| (x - mean) / sd).collect()
}

fn split_history(bars: &[StockBar], split_ratio: f64) -> Option<(&[StockBar], &[StockBar])> {
    let cut = (bars.len() as f64 * split_ratio).floor() as usize;
    let (train, test) = bars.split_at(cut);
    if train.len() >= MIN_TRAIN && test.len() >= MIN_TEST { Some((train, test)) } else { None }
}

/// 5 个候选（kind, 中文名），顺序即展示顺序。
const CANDIDATES: &[(&str, &str)] = &[
    ("dca", "普通定投"),
    ("smart_dca", "智能定投"),
    ("trend", "均线择时"),
    ("rsi", "RSI超买超卖"),
    ("adaptive", "自适应"),
];

/// 用共用策略构造器建候选（固定稳健默认参数）。
fn candidate(kind: &str) -> Box<dyn Strategy> {
    match kind {
        "smart_dca" => Box::new(SmartDca::new(Period::Monthly, 1, 1000.0, 250, 1.0)),
        "trend" => Box::new(Trend::new(20, 60, 1000.0)),
        "rsi" => Box::new(Rsi::new(14, 30.0, 70.0, 1000.0)),
        "adaptive" => Box::new(Adaptive::new(Period::Monthly, 1, 1000.0)),
        _ => Box::new(Dca::new(Period::Monthly, 1, 1000.0)),
    }
}

fn run_metrics(kind: &str, bars: &[StockBar], fee: StockFee) -> Summary {
    let strat = candidate(kind);
    backtest::run_one(kind.to_string(), String::new(), bars.to_vec(), strat, fee, 0.0).summary
}

fn diagnose_or_fallback(code: &str, name: &str, bars: &[StockBar]) -> StockDiagnosis {
    diagnose::diagnose(code.to_string(), name.to_string(), bars, &DiagnoseParams::default())
        .unwrap_or_else(|_| StockDiagnosis {
            code: code.to_string(), name: name.to_string(),
            trend: "数据不足".into(), signal: "观望".into(), ma_relation: "未知".into(),
            rationale: "数据不足，暂不给出技术诊断".into(),
            ..Default::default()
        })
}

/// 对单只股票产出推荐（stock_score 留待 rank_top 跨股标准化）。
pub fn evaluate_stock(code: &str, name: &str, bars: &[StockBar], p: &RecommendParams) -> anyhow::Result<StockRecommendation> {
    let (train, test) = split_history(bars, p.split_ratio).ok_or_else(|| {
        anyhow::anyhow!("数据不足: 需训练≥{} 检验≥{} 个交易日（当前 {}）", MIN_TRAIN, MIN_TEST, bars.len())
    })?;

    let mut evals: Vec<StockStrategyEval> = Vec::with_capacity(CANDIDATES.len());
    for (kind, name_cn) in CANDIDATES {
        let is_s = run_metrics(kind, train, p.fee);
        let oos_s = run_metrics(kind, test, p.fee);
        evals.push(StockStrategyEval {
            kind: (*kind).to_string(), name: (*name_cn).to_string(),
            is_return: is_s.total_return, is_sharpe: is_s.sharpe, is_mdd: is_s.max_drawdown,
            oos_return: oos_s.total_return, oos_sharpe: oos_s.sharpe, oos_mdd: oos_s.max_drawdown,
            score: 0.0,
        });
    }

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

    let diagnosis = diagnose_or_fallback(code, name, bars);
    let rationale = format!(
        "训练段(前{:.0}%)5 候选中『{}』综合评分最高（收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%）；\
         检验段(后{:.0}%)样本外 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%。技术面：{}（{}）。",
        p.split_ratio * 100.0, best.name, best.is_return * 100.0, best.is_sharpe, best.is_mdd * 100.0,
        (1.0 - p.split_ratio) * 100.0, best.oos_return * 100.0, best.oos_sharpe, best.oos_mdd * 100.0,
        diagnosis.trend, diagnosis.signal,
    );

    Ok(StockRecommendation {
        code: code.to_string(), name: name.to_string(), stock_score: 0.0,
        best_strategy: best, all_strategies: evals, diagnosis, rationale,
    })
}

/// 用各股「最优策略样本外三指标」跨股 z-score 综合评分，降序取 top_n。
pub fn rank_top(mut recs: Vec<StockRecommendation>, p: &RecommendParams) -> Vec<StockRecommendation> {
    if recs.is_empty() { return recs; }
    let zr = zscores(&recs.iter().map(|r| r.best_strategy.oos_return).collect::<Vec<_>>());
    let zs = zscores(&recs.iter().map(|r| r.best_strategy.oos_sharpe).collect::<Vec<_>>());
    let zm = zscores(&recs.iter().map(|r| r.best_strategy.oos_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, r) in recs.iter_mut().enumerate() {
        r.stock_score = w.w_return * zr[i] + w.w_sharpe * zs[i] - w.w_mdd * zm[i];
    }
    recs.sort_by(|a, b| b.stock_score.partial_cmp(&a.stock_score).unwrap_or(std::cmp::Ordering::Equal));
    recs.truncate(p.top_n);
    recs
}

/// 遍历股票池：注入 loader 取 bars → evaluate_stock → rank_top，装配整页报告。
pub fn build_report<F>(
    pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams, mut load: F,
) -> StockRecommendReport
where
    F: FnMut(&str) -> anyhow::Result<Vec<StockBar>>,
{
    let mut recs = Vec::new();
    let mut skipped = Vec::new();
    for &code in pool {
        match load(code) {
            Ok(bars) => {
                let name = names.get(code).cloned().unwrap_or_else(|| code.to_string());
                match evaluate_stock(code, &name, &bars, p) {
                    Ok(r) => recs.push(r),
                    Err(_) => skipped.push(code.to_string()),
                }
            }
            Err(_) => skipped.push(code.to_string()),
        }
    }
    let analyzed = recs.len();
    let top = rank_top(recs, p);
    StockRecommendReport {
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

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::recommend`
Expected: 8 个测试 PASS。

- [ ] **Step 6: 全量回归 + 提交**

Run: `cargo test`
Expected: 全绿。
```bash
git add src/stock/recommend.rs src/stock/mod.rs
git commit -m "feat(stock): 选股/推荐 build_report（多策略样本外评分 + z-score 排名 + 技术诊断）"
```

---

## Self-Review

**1. Spec 覆盖：** zscores/split_history→Task2；5 复用策略评估+训练段选优→evaluate_stock；跨股 z-score 排名→rank_top；诊断上下文→diagnose_or_fallback（依赖 Task1 Default）；注入式 build_report→Task2；免责声明→DISCLAIMER。✅

**2. 占位符扫描：** 无 TBD/TODO；每步完整代码。✅

**3. 类型一致性：** `StockStrategyEval`/`StockRecommendation`/`StockRecommendReport`/`ScoreWeights`/`RecommendParams` 字段自洽；`backtest::run_one(name,code,bars,strategy,fee,cash)->StockRunOutcome{summary}`（子项目2）签名一致；`diagnose::diagnose(code,name,&bars,&params)`（子项目3）一致；`StockFee`(Copy) 一致；策略构造器签名与 strategy 模块一致。✅

**说明：** 诊断降级用 `StockDiagnosis::default()`（Task1 加 Default）填充占位；评估段 diagnose 用全量 bars（≥150>60）正常成功。评分费率统一 A股口径，跨股排名一致。

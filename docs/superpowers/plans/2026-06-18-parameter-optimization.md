# 参数寻优（Grid Search）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 对单基金、单策略种类在参数网格上批量回测并按指标排序，生成 `output/optimize.html`（全组合排名表 + Top-N 收益率叠图）。

**Architecture:** 新增 `[optimize]` 配置；新增 `src/optimize.rs`（纯函数 `expand_grid` 笛卡尔积 + 编排函数 `run_optimize`）；新增 `src/report/optimize.rs` 渲染；`main.rs` 检测到 `[optimize]` 即进寻优模式（优先于 compare）。每个组合复用现有 `config::build_strategy_from` + `runner::run_one`，引擎/runner 零改动。

**Tech Stack:** Rust（serde/toml/serde_json）+ ECharts + Playwright 自测。

依附既有 API（已核对）：
- `config::build_strategy_from(kind: &str, params: &Option<toml::Value>, rules: &[RuleCfg]) -> Result<Box<dyn Strategy>>`
- `config::build_fee(cfg: &Config) -> FeeModel`（`FeeModel: Clone`）
- `runner::run_one(name: String, fund_code: String, points: Vec<NavPoint>, strategy: Box<dyn Strategy>, fee: FeeModel, initial_cash: f64) -> RunOutcome`
- `runner::RunOutcome { name, fund_code, summary: metrics::Summary, daily: Vec<DailyRecord> }`
- `metrics::Summary { total_contributed, final_equity, total_return, annualized, max_drawdown, sharpe, trade_count }`（`Clone + Serialize`）
- `report::html_escape(s: &str) -> String`（crate 内 `pub(crate)`）
- `NavPoint`（`Copy`）、`DailyRecord`（`Clone + Serialize`，字段 date/nav/adj_nav/equity/contribution/shares/cash）

## Global Constraints

- 不改动 `engine.rs` / `runner.rs` / `portfolio.rs` / 既有策略；只新增模块与配置。
- 不破坏既有 53 测试与 clippy 干净。
- 排序指标合法集合：`total_return` / `annualized` / `sharpe`（越大越优）、`max_drawdown`（越小越优）。
- `toml::Table` 默认 BTreeMap 支撑，迭代按键名字典序——`param_keys` 与展开顺序均以此为稳定序。
- 寻优模式与 compare 同时存在时：寻优优先，`eprintln!` 提示忽略 compare。
- 提交信息使用仓库现有中文/英文风格；每个 Task 末尾提交一次。

---

## Task 1: 配置 OptimizeCfg

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`（`#[cfg(test)]` 内新增）

**Interfaces:**
- Produces: `pub struct OptimizeCfg { pub strategy: String, pub metric: String, pub top_n: usize, pub grid: toml::Table, pub rules: Vec<RuleCfg> }`；`Config` 新增 `pub optimize: Option<OptimizeCfg>`。

- [ ] **Step 1: 写失败测试**

在 `src/config.rs` 的 `mod tests` 内新增：

```rust
    #[test]
    fn parses_optimize_section() {
        let s = r#"
[data]
fund_code = "161725"
start = "2020-01-01"
end = "2024-12-31"
cache_dir = ".cache"
[fees]
buy_rate = 0.0015
sell_tiers = [{max_days = 0, rate = 0.0}]
[strategy]
kind = "dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
[report]
chart = false
out_dir = "output"

[optimize]
strategy = "smart_dca"
metric = "sharpe"
[optimize.grid]
period = ["monthly"]
day = [1]
base_amount = [1000.0]
ma_window = [120, 250, 500]
k = [0.5, 1.0, 1.5]
"#;
        let cfg: Config = toml::from_str(s).unwrap();
        let opt = cfg.optimize.expect("optimize section should parse");
        assert_eq!(opt.strategy, "smart_dca");
        assert_eq!(opt.metric, "sharpe");
        assert_eq!(opt.top_n, 5, "top_n should default to 5");
        assert_eq!(opt.grid.len(), 5, "grid should have 5 params");
        assert!(opt.grid.contains_key("ma_window"));
    }

    #[test]
    fn optimize_absent_is_none() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert!(cfg.optimize.is_none());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib config::tests::parses_optimize_section`
Expected: 编译失败（`OptimizeCfg`/`optimize` 字段不存在）。

- [ ] **Step 3: 实现**

在 `src/config.rs` 的 `Config` 结构体末尾（`compare` 之后）加字段：

```rust
    #[serde(default)]
    pub optimize: Option<OptimizeCfg>,
```

在 `CompareRun` 定义之后新增：

```rust
#[derive(Debug, Deserialize)]
pub struct OptimizeCfg {
    pub strategy: String,
    pub metric: String,
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    pub grid: toml::Table,
    #[serde(default)]
    pub rules: Vec<RuleCfg>,
}

fn default_top_n() -> usize { 5 }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib config`
Expected: PASS（含两个新测试，既有 config 测试不变）。

- [ ] **Step 5: 提交**

```bash
git add src/config.rs
git commit -m "feat: [optimize] 配置段 OptimizeCfg"
```

---

## Task 2: 网格展开 optimize::expand_grid

**Files:**
- Create: `src/optimize.rs`
- Modify: `src/lib.rs`（加 `pub mod optimize;`）
- Test: `src/optimize.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: 无（纯函数，输入 `&toml::Table`）。
- Produces: `pub fn expand_grid(grid: &toml::Table) -> anyhow::Result<Vec<toml::Value>>`，每个元素是 `toml::Value::Table` 的一个参数组合。

- [ ] **Step 1: 在 `src/lib.rs` 注册模块**

在 `src/lib.rs` 现有 `pub mod runner;` 附近加一行：

```rust
pub mod optimize;
```

- [ ] **Step 2: 写失败测试**

创建 `src/optimize.rs`，先只放测试与函数签名占位：

```rust
use anyhow::{anyhow, Result};

/// 把 {参数名 -> 值数组} 的网格展开成每个组合一个 `toml::Value::Table`。
/// 笛卡尔积按 grid 键的字典序（BTreeMap 迭代序）稳定展开。
pub fn expand_grid(grid: &toml::Table) -> Result<Vec<toml::Value>> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr_table() -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]));
        t.insert("b".into(), toml::Value::Array(vec![toml::Value::String("x".into())]));
        t
    }

    #[test]
    fn expands_cartesian_product() {
        let combos = expand_grid(&arr_table()).unwrap();
        assert_eq!(combos.len(), 2, "2x1 = 2 combos");
        // 每个组合含 a 与 b
        for c in &combos {
            let t = c.as_table().unwrap();
            assert!(t.contains_key("a") && t.contains_key("b"));
            assert_eq!(t["b"].as_str(), Some("x"));
        }
        // a 取值覆盖 1 和 2
        let a_vals: Vec<i64> = combos.iter().map(|c| c.as_table().unwrap()["a"].as_integer().unwrap()).collect();
        assert!(a_vals.contains(&1) && a_vals.contains(&2));
    }

    #[test]
    fn rejects_non_array_value() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Integer(1)); // 非数组
        let err = expand_grid(&t).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name param a: {err}");
    }

    #[test]
    fn rejects_empty_array() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Array(vec![]));
        let err = expand_grid(&t).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name param a: {err}");
    }

    #[test]
    fn rejects_empty_grid() {
        let err = expand_grid(&toml::Table::new()).unwrap_err();
        assert!(err.to_string().contains("grid"), "error should mention grid: {err}");
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib optimize::tests::expands_cartesian_product`
Expected: panic（`todo!()`）。

- [ ] **Step 4: 实现 expand_grid**

把 `expand_grid` 的 `todo!()` 替换为：

```rust
pub fn expand_grid(grid: &toml::Table) -> Result<Vec<toml::Value>> {
    if grid.is_empty() {
        return Err(anyhow!("optimize.grid 不能为空"));
    }
    // 收集各维度 (键, 取值数组)，按 grid 迭代序（字典序）
    let mut dims: Vec<(&String, &Vec<toml::Value>)> = Vec::new();
    for (k, v) in grid {
        match v {
            toml::Value::Array(arr) if !arr.is_empty() => dims.push((k, arr)),
            toml::Value::Array(_) => return Err(anyhow!("optimize.grid 参数 {k} 的取值数组为空")),
            _ => return Err(anyhow!("optimize.grid 参数 {k} 必须是数组，例如 {k} = [..]")),
        }
    }
    // 笛卡尔积
    let mut combos: Vec<toml::Table> = vec![toml::Table::new()];
    for (k, arr) in &dims {
        let mut next = Vec::with_capacity(combos.len() * arr.len());
        for base in &combos {
            for val in arr.iter() {
                let mut t = base.clone();
                t.insert((*k).clone(), val.clone());
                next.push(t);
            }
        }
        combos = next;
    }
    Ok(combos.into_iter().map(toml::Value::Table).collect())
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib optimize`
Expected: 4 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/lib.rs src/optimize.rs
git commit -m "feat: optimize::expand_grid 笛卡尔积展开"
```

---

## Task 3: 寻优编排 optimize::run_optimize

**Files:**
- Modify: `src/optimize.rs`
- Test: `src/optimize.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `expand_grid`（Task 2）、`config::build_strategy_from`、`config::OptimizeCfg`、`runner::run_one`、`broker::FeeModel`、`data::NavPoint`。
- Produces:
  - `pub struct OptOutcome { pub params: toml::Value, pub label: String, pub outcome: crate::runner::RunOutcome }`
  - `pub struct OptReport { pub strategy: String, pub metric: String, pub top_n: usize, pub ranked: Vec<OptOutcome>, pub param_keys: Vec<String> }`
  - `pub fn run_optimize(cfg: &crate::config::OptimizeCfg, fund_code: &str, points: &[crate::data::NavPoint], fee: crate::broker::FeeModel, initial_cash: f64) -> Result<OptReport>`

- [ ] **Step 1: 写失败测试**

在 `src/optimize.rs` 的 `mod tests` 内新增（顶部补 `use` 见实现步骤）：

```rust
    use crate::broker::{FeeModel, SellTier};
    use crate::config::OptimizeCfg;
    use crate::data::NavPoint;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn no_fee() -> FeeModel { FeeModel { buy_rate: 0.0, sell_tiers: vec![SellTier { max_days: 0, rate: 0.0 }] } }

    fn sample_points() -> Vec<NavPoint> {
        vec![
            NavPoint { date: d(2024, 1, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 15), nav: 2.0, acc_nav: 2.0 },
        ]
    }

    // grid: smart_dca，ma_window 取两值（其余固定单值）
    fn smart_dca_cfg(metric: &str) -> OptimizeCfg {
        let toml_text = format!(r#"
strategy = "smart_dca"
metric = "{metric}"
[grid]
period = ["monthly"]
day = [1]
base_amount = [1000.0]
ma_window = [1, 2]
k = [1.0]
"#);
        toml::from_str(&toml_text).unwrap()
    }

    #[test]
    fn runs_all_combos_and_ranks() {
        let cfg = smart_dca_cfg("total_return");
        let report = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap();
        assert_eq!(report.ranked.len(), 2, "ma_window 两值 → 2 组合");
        assert_eq!(report.param_keys, vec!["base_amount", "day", "k", "ma_window", "period"],
            "param_keys 按字典序");
        // total_return 降序：第一个 >= 第二个
        assert!(report.ranked[0].outcome.summary.total_return
            >= report.ranked[1].outcome.summary.total_return);
        // label 只含变化维度 ma_window
        assert!(report.ranked[0].label.contains("ma_window"), "label: {}", report.ranked[0].label);
        assert!(!report.ranked[0].label.contains("period"), "固定维度不入 label: {}", report.ranked[0].label);
    }

    #[test]
    fn max_drawdown_sorts_ascending() {
        let cfg = smart_dca_cfg("max_drawdown");
        let report = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap();
        // max_drawdown 越小越优 → 升序：第一个 <= 第二个
        assert!(report.ranked[0].outcome.summary.max_drawdown
            <= report.ranked[1].outcome.summary.max_drawdown);
    }

    #[test]
    fn rejects_bad_metric() {
        let cfg = smart_dca_cfg("bogus");
        let err = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap_err();
        assert!(err.to_string().contains("metric"), "error should mention metric: {err}");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib optimize::tests::runs_all_combos_and_ranks`
Expected: 编译失败（`run_optimize`/`OptReport`/`OptOutcome` 不存在）。

- [ ] **Step 3: 实现**

在 `src/optimize.rs` 顶部 `use` 区补：

```rust
use crate::config::{build_strategy_from, OptimizeCfg};
use crate::data::NavPoint;
use crate::runner::{run_one, RunOutcome};
use crate::broker::FeeModel;
```

在 `expand_grid` 之后新增：

```rust
pub struct OptOutcome {
    pub params: toml::Value,
    pub label: String,
    pub outcome: RunOutcome,
}

pub struct OptReport {
    pub strategy: String,
    pub metric: String,
    pub top_n: usize,
    pub ranked: Vec<OptOutcome>,
    pub param_keys: Vec<String>,
}

const METRICS: [&str; 4] = ["total_return", "annualized", "sharpe", "max_drawdown"];

fn metric_value(s: &crate::metrics::Summary, metric: &str) -> f64 {
    match metric {
        "total_return" => s.total_return,
        "annualized" => s.annualized,
        "sharpe" => s.sharpe,
        "max_drawdown" => s.max_drawdown,
        _ => 0.0,
    }
}

/// 由变化维度（取值多于一个的参数）拼出紧凑标签；全固定时用序号。
fn make_label(combo: &toml::Value, varying: &[String], idx: usize) -> String {
    let t = combo.as_table().expect("combo 应为 table");
    if varying.is_empty() {
        return format!("#{}", idx + 1);
    }
    varying.iter()
        .map(|k| format!("{}={}", k, t.get(k).map(|v| v.to_string()).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn run_optimize(
    cfg: &OptimizeCfg,
    fund_code: &str,
    points: &[NavPoint],
    fee: FeeModel,
    initial_cash: f64,
) -> Result<OptReport> {
    if !METRICS.contains(&cfg.metric.as_str()) {
        return Err(anyhow!("未知排序 metric: {}，合法取值: {:?}", cfg.metric, METRICS));
    }
    let combos = expand_grid(&cfg.grid)?;
    if combos.len() > 200 {
        eprintln!("⚠ 参数组合数 {} 较多，回测可能较慢", combos.len());
    }
    let param_keys: Vec<String> = cfg.grid.keys().cloned().collect();
    let varying: Vec<String> = param_keys.iter()
        .filter(|k| matches!(cfg.grid.get(k.as_str()), Some(toml::Value::Array(a)) if a.len() > 1))
        .cloned()
        .collect();

    let mut ranked = Vec::with_capacity(combos.len());
    for (i, combo) in combos.into_iter().enumerate() {
        let label = make_label(&combo, &varying, i);
        let strategy = build_strategy_from(&cfg.strategy, &Some(combo.clone()), &cfg.rules)
            .map_err(|e| anyhow!("组合 [{label}] 构建策略失败: {e}"))?;
        let outcome = run_one(label.clone(), fund_code.to_string(), points.to_vec(), strategy, fee.clone(), initial_cash);
        ranked.push(OptOutcome { params: combo, label, outcome });
    }

    let descending = cfg.metric != "max_drawdown";
    ranked.sort_by(|a, b| {
        let (va, vb) = (metric_value(&a.outcome.summary, &cfg.metric), metric_value(&b.outcome.summary, &cfg.metric));
        if descending { vb.partial_cmp(&va).unwrap() } else { va.partial_cmp(&vb).unwrap() }
    });

    Ok(OptReport {
        strategy: cfg.strategy.clone(),
        metric: cfg.metric.clone(),
        top_n: cfg.top_n,
        ranked,
        param_keys,
    })
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib optimize`
Expected: 全部 PASS（含 Task 2 的 4 个 + 本任务 3 个）。

- [ ] **Step 5: clippy**

Run: `cargo clippy --all-targets`
Expected: 无 warning。

- [ ] **Step 6: 提交**

```bash
git add src/optimize.rs
git commit -m "feat: optimize::run_optimize 跑全网格并按指标排序"
```

---

## Task 4: 寻优报告 report::optimize

**Files:**
- Create: `src/report/optimize.rs`
- Modify: `src/report/mod.rs`（加 `pub mod optimize;`）
- Test: `src/report/optimize.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `optimize::OptReport` / `OptOutcome`（Task 3）、`report::html_escape`、`metrics::Summary`、`DailyRecord`。
- Produces:
  - `pub struct OptMeta { pub start: NaiveDate, pub end: NaiveDate, pub fund_code: String }`
  - `pub fn render_optimize(meta: &OptMeta, report: &crate::optimize::OptReport, out_dir: &Path) -> Result<PathBuf>`（写 `optimize.html`，返回其路径）

- [ ] **Step 1: 在 `src/report/mod.rs` 注册模块**

在 `pub mod compare;` 下新增：

```rust
pub mod optimize;
```

- [ ] **Step 2: 写失败测试**

创建 `src/report/optimize.rs`，先放函数签名占位与测试：

```rust
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use crate::optimize::OptReport;

pub struct OptMeta {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub fund_code: String,
}

pub fn render_optimize(meta: &OptMeta, report: &OptReport, out_dir: &Path) -> Result<PathBuf> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::metrics::Summary;
    use crate::result::DailyRecord;
    use crate::runner::RunOutcome;
    use crate::optimize::{OptOutcome, OptReport};

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    fn daily() -> Vec<DailyRecord> {
        vec![
            DailyRecord { date: d(2024, 1, 1), nav: 1.0, adj_nav: 1.0, equity: 1000.0, contribution: 1000.0, shares: 1000.0, cash: 0.0 },
            DailyRecord { date: d(2024, 2, 15), nav: 2.0, adj_nav: 2.0, equity: 2000.0, contribution: 0.0, shares: 1000.0, cash: 0.0 },
        ]
    }

    fn outcome(label: &str, total_return: f64, ma: i64) -> OptOutcome {
        let mut params = toml::Table::new();
        params.insert("ma_window".into(), toml::Value::Integer(ma));
        OptOutcome {
            params: toml::Value::Table(params),
            label: label.to_string(),
            outcome: RunOutcome {
                name: label.to_string(),
                fund_code: "161725".to_string(),
                summary: Summary { total_contributed: 1000.0, final_equity: 2000.0, total_return, annualized: 0.4, max_drawdown: 0.1, sharpe: 1.2, trade_count: 1 },
                daily: daily(),
            },
        }
    }

    #[test]
    fn renders_optimize_report() {
        let report = OptReport {
            strategy: "smart_dca".to_string(),
            metric: "total_return".to_string(),
            top_n: 5,
            ranked: vec![outcome("ma_window=250", 1.0, 250), outcome("ma_window=120", 0.5, 120)],
            param_keys: vec!["ma_window".to_string()],
        };
        let meta = OptMeta { start: d(2024, 1, 1), end: d(2024, 2, 15), fund_code: "161725".to_string() };
        let tmp = std::env::temp_dir().join("xlh_optimize_test");
        let path = render_optimize(&meta, &report, &tmp).unwrap();

        assert!(path.exists(), "optimize.html should exist");
        let html = std::fs::read_to_string(&path).unwrap();
        assert!(html.contains("参数寻优"), "标题");
        assert!(html.contains("排名"), "排名表头");
        assert!(html.contains("ma_window"), "参数列名");
        assert!(html.contains("ma_window=250"), "组合标签");
        assert!(html.contains("const DATA"), "内嵌数据");
        assert!(html.contains("echarts"), "图表库");
        assert!(html.contains("<table"), "表格");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib report::optimize::tests::renders_optimize_report`
Expected: panic（`todo!()`）。

- [ ] **Step 4: 实现 render_optimize + build_html**

把 `render_optimize` 的 `todo!()` 替换为实现，并补 `build_html` 与序列化结构。完整代码：

```rust
#[derive(Serialize)]
struct RunJson<'a> {
    label: String,
    summary: &'a crate::metrics::Summary,
    daily: &'a [crate::result::DailyRecord],
}

#[derive(Serialize)]
struct Payload<'a> {
    start: String,
    end: String,
    // 仅 Top-N 进图，避免曲线过多
    runs: Vec<RunJson<'a>>,
}

fn fmt_pct(v: f64) -> String { format!("{:.2}%", v * 100.0) }
fn sign_class(v: f64) -> &'static str { if v >= 0.0 { "pos" } else { "neg" } }

/// 取 toml 值的紧凑显示（去掉字符串引号，整数/浮点原样）。
fn cell(v: Option<&toml::Value>) -> String {
    match v {
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

pub fn render_optimize(meta: &OptMeta, report: &OptReport, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;

    let top_n = report.top_n.min(report.ranked.len());
    let payload = Payload {
        start: meta.start.to_string(),
        end: meta.end.to_string(),
        runs: report.ranked.iter().take(top_n).map(|o| RunJson {
            label: o.label.clone(),
            summary: &o.outcome.summary,
            daily: &o.outcome.daily,
        }).collect(),
    };
    let data_json = serde_json::to_string(&payload)
        .context("序列化寻优数据失败")?
        .replace("</", "<\\/");
    let html = build_html(meta, report, &data_json);
    let out = out_dir.join("optimize.html");
    std::fs::write(&out, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out.display()))?;
    Ok(out)
}

fn build_html(meta: &OptMeta, report: &OptReport, data_json: &str) -> String {
    let fund_esc = crate::report::html_escape(&meta.fund_code);
    let strat_esc = crate::report::html_escape(&report.strategy);
    let metric_esc = crate::report::html_escape(&report.metric);
    let total = report.ranked.len();
    let top_n = report.top_n.min(total);

    // 表头参数列
    let param_th: String = report.param_keys.iter()
        .map(|k| format!("<th>{}</th>", crate::report::html_escape(k)))
        .collect();

    // 排序所依指标对应的列名（用于整列高亮 class）
    let metric_col = report.metric.as_str();

    let rows: String = report.ranked.iter().enumerate().map(|(i, o)| {
        let s = &o.outcome.summary;
        let t = o.params.as_table();
        let param_tds: String = report.param_keys.iter()
            .map(|k| format!("<td>{}</td>", crate::report::html_escape(&cell(t.and_then(|tt| tt.get(k))))))
            .collect();
        let rank_best = if i == 0 { " best" } else { "" };
        let hl = |col: &str| if col == metric_col { " best" } else { "" };
        format!(
            "<tr>\
<td class=\"{rank_best}\">{rank}</td>\
{params}\
<td class=\"{cret}{h_ret}\">{ret}</td>\
<td class=\"{cann}{h_ann}\">{ann}</td>\
<td class=\"neg{h_mdd}\">{mdd}</td>\
<td class=\"{h_sharpe}\">{sharpe:.2}</td>\
<td>{eq:.2}</td>\
<td>{tc}</td>\
</tr>\n",
            rank_best = rank_best,
            rank = i + 1,
            params = param_tds,
            cret = sign_class(s.total_return), h_ret = hl("total_return"), ret = fmt_pct(s.total_return),
            cann = sign_class(s.annualized), h_ann = hl("annualized"), ann = fmt_pct(s.annualized),
            h_mdd = hl("max_drawdown"), mdd = fmt_pct(s.max_drawdown),
            h_sharpe = hl("sharpe"), sharpe = s.sharpe,
            eq = s.final_equity,
            tc = s.trade_count,
        )
    }).collect();

    format!(r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>参数寻优报告</title>
<style>
*,*::before,*::after{{box-sizing:border-box;margin:0;padding:0}}
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;background:#f5f6fa;color:#2c3e50;line-height:1.5}}
.container{{max-width:1200px;margin:0 auto;padding:24px 16px}}
header{{margin-bottom:24px}}
header h1{{font-size:1.8rem;font-weight:700;color:#1a252f}}
header .subtitle{{color:#7f8c8d;margin-top:4px;font-size:.95rem}}
.card{{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:20px;margin-bottom:20px;box-shadow:0 1px 4px rgba(0,0,0,.06)}}
.card h2{{font-size:1rem;font-weight:600;color:#34495e;margin-bottom:14px;border-bottom:1px solid #eaecef;padding-bottom:8px}}
.pos{{color:#c0392b}}
.neg{{color:#27ae60}}
.chart{{width:100%;height:380px}}
table{{width:100%;border-collapse:collapse;font-size:.88rem}}
th{{background:#f0f2f5;text-align:left;padding:8px 10px;font-weight:600;color:#5a6a7a;position:sticky;top:0}}
td{{padding:7px 10px;border-bottom:1px solid #f0f2f5}}
tr:last-child td{{border-bottom:none}}
.best{{font-weight:700;background:#fffbe6}}
.scrollable{{overflow-x:auto}}
</style>
</head>
<body>
<div class="container">
<header>
  <h1>参数寻优报告</h1>
  <div class="subtitle">基金 {fund} &nbsp;|&nbsp; 策略 {strat} &nbsp;|&nbsp; 区间 {start} ~ {end} &nbsp;|&nbsp; 排序指标 {metric} &nbsp;|&nbsp; 共 {total} 组合（图示 Top {top_n}）</div>
</header>

<div class="card">
  <h2>组合排名</h2>
  <div class="scrollable">
  <table>
    <thead><tr><th>排名</th>{param_th}<th>总收益</th><th>年化</th><th>最大回撤</th><th>夏普</th><th>期末市值</th><th>交易次数</th></tr></thead>
    <tbody>
{rows}    </tbody>
  </table>
  </div>
</div>

<div class="card">
  <h2>Top {top_n} 收益率对比</h2>
  <div id="chart-return" class="chart"></div>
</div>

</div><!-- /container -->

<script>const DATA = {data_json};</script>
<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>
<script>
(function(){{
  if (typeof echarts === 'undefined') {{
    document.querySelectorAll('.chart').forEach(function(e) {{
      e.style.display='flex';e.style.alignItems='center';e.style.justifyContent='center';
      e.style.color='#888';e.style.fontSize='14px';
      e.innerHTML='图表库加载失败（需联网加载 ECharts）';
    }});
    return;
  }}
  var COLORS = ['#c0392b','#2980b9','#27ae60','#8e44ad','#e67e22','#16a085'];
  var retSeries = DATA.runs.map(function(run, idx) {{
    var cum = 0;
    var data = run.daily.map(function(r) {{
      cum += r.contribution;
      var ret = cum > 0 ? (r.equity / cum - 1) * 100 : 0;
      return [r.date, +ret.toFixed(4)];
    }});
    return {{ name: run.label, type:'line', data:data, smooth:true, symbol:'none',
      lineStyle:{{color:COLORS[idx % COLORS.length]}}, itemStyle:{{color:COLORS[idx % COLORS.length]}} }};
  }});
  var _dateSet = new Set();
  DATA.runs.forEach(function(run) {{ run.daily.forEach(function(r) {{ _dateSet.add(r.date); }}); }});
  var allDates = Array.from(_dateSet).sort();
  var c1 = echarts.init(document.getElementById('chart-return'));
  c1.setOption({{
    tooltip: {{ trigger:'axis', valueFormatter:function(v){{ return (typeof v==='number'?v.toFixed(2)+'%':v); }} }},
    legend: {{ data: DATA.runs.map(function(r){{ return r.label; }}), bottom:36 }},
    grid: {{ left:60, right:20, top:40, bottom:80 }},
    xAxis: {{ type:'category', data:allDates, boundaryGap:false }},
    yAxis: {{ type:'value', name:'%', axisLabel:{{ formatter:function(v){{return v+'%';}} }} }},
    dataZoom: [{{ type:'slider', xAxisIndex:0, bottom:8, height:20 }}, {{ type:'inside', xAxisIndex:0 }}],
    series: retSeries
  }});
  window.addEventListener('resize', function() {{ c1.resize(); }});
}})();
</script>
</body>
</html>
"#,
        fund = fund_esc,
        strat = strat_esc,
        metric = metric_esc,
        start = meta.start,
        end = meta.end,
        total = total,
        top_n = top_n,
        param_th = param_th,
        rows = rows,
        data_json = data_json,
    )
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib report::optimize`
Expected: PASS。

- [ ] **Step 6: clippy + 全量测试**

Run: `cargo clippy --all-targets` 然后 `cargo test`
Expected: clippy 无 warning；测试全绿。

- [ ] **Step 7: 提交**

```bash
git add src/report/mod.rs src/report/optimize.rs
git commit -m "feat: 参数寻优 HTML 报告 render_optimize"
```

---

## Task 5: CLI 接线 + 示例 optimize.toml

**Files:**
- Modify: `src/main.rs`
- Create: `optimize.toml`

**Interfaces:**
- Consumes: `optimize::run_optimize`、`report::optimize::{OptMeta, render_optimize}`、`config` 各项。

- [ ] **Step 1: main.rs 加寻优分支**

在 `src/main.rs` 的 `let cfg = config::load(&cli.config)?;` 之后、`if !cfg.compare.is_empty() {` 之前插入：

```rust
    if let Some(opt) = &cfg.optimize {
        if !cfg.compare.is_empty() {
            eprintln!("⚠ 同时存在 [optimize] 与 [[compare]]，本次按寻优模式执行，忽略 compare。");
        }
        let points = cache::load_or_fetch(&cfg.data.fund_code, &cfg.data.cache_dir, cfg.data.start, cfg.data.end)?;
        println!("加载 {} 条净值（{} ~ {}）", points.len(), cfg.data.start, cfg.data.end);
        let fee = config::build_fee(&cfg);
        let report = xlh::optimize::run_optimize(opt, &cfg.data.fund_code, &points, fee, cfg.portfolio.initial_cash)?;
        let show = report.top_n.min(report.ranked.len());
        println!("== 寻优 Top {} （按 {}）==", show, report.metric);
        for (i, o) in report.ranked.iter().take(show).enumerate() {
            println!("  {}. {}  总收益 {:.2}%  夏普 {:.2}  最大回撤 {:.2}%",
                i + 1, o.label,
                o.outcome.summary.total_return * 100.0,
                o.outcome.summary.sharpe,
                o.outcome.summary.max_drawdown * 100.0);
        }
        let meta = xlh::report::optimize::OptMeta {
            start: cfg.data.start, end: cfg.data.end, fund_code: cfg.data.fund_code.clone(),
        };
        let path = xlh::report::optimize::render_optimize(&meta, &report, &cfg.report.out_dir)?;
        println!("寻优报告已生成：{}", path.display());
        return Ok(());
    }

```

- [ ] **Step 2: 创建 optimize.toml**

创建 `optimize.toml`：

```toml
[data]
fund_code = "161725"
start = "2020-01-01"
end   = "2024-12-31"
cache_dir = ".cache"

[fees]
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7,   rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0,   rate = 0.000 },
]

# 单次路径要求 [strategy] 可解析（寻优模式下忽略内容）
[strategy]
kind = "dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0

[report]
chart = false
html = false
out_dir = "output"

[portfolio]
initial_cash = 0.0

[optimize]
strategy = "smart_dca"
metric   = "sharpe"
top_n    = 5

[optimize.grid]
period      = ["monthly"]
day         = [1]
base_amount = [1000.0]
ma_window   = [120, 250, 500]
k           = [0.5, 1.0, 1.5]
```

- [ ] **Step 3: 构建 + 全量测试 + clippy**

Run: `cargo build` 然后 `cargo test` 然后 `cargo clippy --all-targets`
Expected: 构建成功；测试全绿；clippy 无 warning。

- [ ] **Step 4: 提交**

```bash
git add src/main.rs optimize.toml
git commit -m "feat: CLI 寻优模式接线 + 示例 optimize.toml"
```

---

## Task 6: 端到端自测（真实数据 + Playwright）

**Files:**
- Create: `scripts/verify_optimize.py`

- [ ] **Step 1: 真实运行寻优**

Run: `cargo run -- --config optimize.toml`
Expected: 打印加载净值条数、`== 寻优 Top 5 ==` 列表、`寻优报告已生成：output/optimize.html`；`output/optimize.html` 存在。
（若联网抓取失败，先确认 `.cache` 已有 161725 数据；本仓库前序任务已缓存。）

- [ ] **Step 2: 写 Playwright 自测脚本**

参照 `scripts/verify_compare.py` 思路创建 `scripts/verify_optimize.py`：

```python
#!/usr/bin/env python3
"""用 Playwright 加载 optimize.html，校验排名表与 Top-N 叠图渲染。"""
import sys
from pathlib import Path
from playwright.sync_api import sync_playwright

HTML = Path("output/optimize.html").resolve()
SHOT = Path("output/optimize_screenshot.png").resolve()

def main():
    if not HTML.exists():
        print(f"FAIL: {HTML} 不存在，请先 cargo run -- --config optimize.toml")
        return 1
    errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page()
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        page.goto(HTML.as_uri())
        page.wait_for_load_state("networkidle")

        # 排名表至少有 1 行
        rows = page.locator("table tbody tr").count()
        assert rows >= 1, f"排名表应有数据行，实际 {rows}"

        # 收益率图含 canvas 且像素面积 > 0
        canvas = page.locator("#chart-return canvas").first
        box = canvas.bounding_box()
        assert box and box["width"] > 0 and box["height"] > 0, "图表 canvas 面积应 > 0"

        page.screenshot(path=str(SHOT), full_page=True)
        browser.close()

    assert not errors, f"页面有 console error: {errors}"
    print(f"PASS: 排名表 {rows} 行；叠图已渲染；截图 {SHOT}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

- [ ] **Step 3: 运行 Playwright 自测**

Run: `python scripts/verify_optimize.py`
Expected: 打印 `PASS: ...`；`output/optimize_screenshot.png` 生成。

- [ ] **Step 4: 肉眼核对截图**

打开 `output/optimize_screenshot.png` 确认：排名表多行、metric 列高亮、第 1 名行高亮、收益率图有多条曲线且图例为组合标签。异常则按 systematic-debugging 修复后重跑。

- [ ] **Step 5: 全量测试 + 提交**

Run: `cargo test`
Expected: 全绿。

```bash
git add scripts/verify_optimize.py
git commit -m "test: Playwright 自测寻优报告渲染"
```

交付报告：optimize.html 路径、截图、各断言结果、Top-5 参数组合的对比数字。

---

## Self-Review

- **Spec 覆盖**：§3 配置→T1；§4 expand_grid→T2；§5 run_optimize→T3；§6 render_optimize→T4；§7 CLI→T5；§9 自测→T6。§8 错误处理散落于 T2/T3（带参数/组合上下文报错）与组合数警告（T3 Step 3）。§2/§10 决策与单元边界由各 Task 文件划分落实。
- **占位符**：无 TBD/TODO；每个改代码的 Step 均给出完整代码。
- **类型一致**：`expand_grid -> Vec<toml::Value>` 与 `build_strategy_from(&Some(combo))` 一致；`OptReport.ranked: Vec<OptOutcome>`、`param_keys: Vec<String>` 在 T3 定义、T4/T5 一致引用；`metrics::Summary` 字段名与 metrics.rs 一致；`DailyRecord` 字段（date/nav/adj_nav/equity/contribution/shares/cash）与 result.rs 一致；`FeeModel: Clone` 已核实，run_optimize 内 `fee.clone()` 合法。
- **不破坏既有路径**：main.rs 寻优分支在 compare 分支之前 `return`，二者互斥；config 仅新增可选字段；既有 53 测试不动。
- **YAGNI**：不做并行、跨基金、规则扫描、智能搜索。

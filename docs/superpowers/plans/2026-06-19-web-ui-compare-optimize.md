# Web 界面加对比/寻优 Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有 `xlh serve` Web 界面加"对比"和"寻优"两个 Tab：对比动态增删多策略行、寻优逗号分隔参数网格，POST JSON 到后端实时跑回测，结果报告刷新到共用 iframe。

**Architecture:** 抽 `render_compare_html`/`render_optimize_html`(返回 String)；web 层抽共享纯函数（校验 + 拼策略参数表 + CSV 解析 + 构建 OptimizeCfg），单次/对比/寻优三处复用；新增 `POST /api/compare`、`POST /api/optimize` handler（spawn_blocking 内建/用策略，Send 安全）；page.rs 重写为三 Tab。

**Tech Stack:** Rust（axum 0.7 / serde / toml）+ 现有 ECharts 报告 + Playwright。

## Global Constraints

- 只新增/改 web 与 report 渲染抽取；不改 engine/runner/portfolio/broker/策略/optimize 算法逻辑。
- 不破坏既有 72 测试与 clippy 干净。
- 对比/寻优用 POST + JSON；单次保持 GET（不变）。
- 非 Send 的 `Box<dyn Strategy>` 必须在 spawn_blocking 闭包内创建并消费，不跨 `.await`。
- fund_code 校验：非空、≤12、全 ASCII 字母数字（防路径穿越）；对比每个 run 的 fund 都校验。
- buy_rate ∈ [0.0,1.0)；start < end；卖出费率固定 `standard_sell_tiers()`（已存在）。
- 缓存目录固定 `.cache`。
- DRY：单次现有"拼策略参数表"逻辑抽成共享函数，三处复用，RunQuery 保持扁平字段（其既有测试不改）。
- 路由单测走 hermetic 错误路径（不依赖网络/缓存）；happy-path 完整回测由 Playwright e2e 验证。
- edition 2021；提交含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `report::compare::{CompareMeta{start,end}, render_compare(meta,&[RunOutcome],out_dir)->Result<PathBuf>}`，内部 `Payload{start,end,runs:Vec<RunJson>}` + `RunJson{name,fund_code,summary:&Summary,daily:&[DailyRecord]}` + `build_html(meta,runs,data_json)`。
- `report::optimize::{OptMeta{start,end,fund_code}, render_optimize(meta,&OptReport,out_dir)->Result<PathBuf>}`，内部 `Payload{start,end,runs}` + `build_html(meta,report,data_json)`。
- `optimize::{run_optimize(&OptimizeCfg, fund:&str, &[NavPoint], FeeModel, f64)->Result<OptReport>, OptReport}`；`config::OptimizeCfg{strategy:String, metric:String, top_n:usize, grid:toml::Table, rules:Vec<RuleCfg>}`。
- `runner::{run_one(name:String, fund:String, points:Vec<NavPoint>, Box<dyn Strategy>, FeeModel, f64)->RunOutcome, RunOutcome{name,fund_code,summary,daily}}`。
- `config::build_strategy_from(kind:&str,&Option<toml::Value>,&[RuleCfg])->Result<Box<dyn Strategy>>`。
- `data::cache::load_or_fetch(fund:&str,&Path,NaiveDate,NaiveDate)->Result<Vec<NavPoint>>`；`broker::{FeeModel,SellTier}`。
- web/mod.rs 现有：`RunQuery`(扁平策略字段)、`RunSpec`、`build_run_from_query`、`standard_sell_tiers()`、`router()`、`serve`、`run_blocking`、`AppError`、`page::INDEX_HTML`。

---

## Task 1: 抽 render_compare_html / render_optimize_html

**Files:**
- Modify: `src/report/compare.rs`, `src/report/optimize.rs`
- Test: 两文件各自 `#[cfg(test)]`

**Interfaces:**
- Produces: `report::compare::render_compare_html(meta:&CompareMeta, runs:&[RunOutcome])->String`；`report::optimize::render_optimize_html(meta:&OptMeta, report:&OptReport)->String`。
- `render_compare`/`render_optimize`（写文件）改为委托。

- [ ] **Step 1: 写失败测试（compare）**

在 `src/report/compare.rs` 的 `mod tests` 内，复用已有 `make_summary`/`make_daily`/`d` 工具新增：

```rust
    #[test]
    fn render_compare_html_returns_markup() {
        let runs = vec![RunOutcome {
            name: "甲".to_string(), fund_code: "161725".to_string(),
            summary: make_summary(), daily: make_daily(),
        }];
        let meta = CompareMeta { start: d(2024,1,1), end: d(2024,2,15) };
        let html = render_compare_html(&meta, &runs);
        assert!(html.contains("const DATA"));
        assert!(html.contains("甲"));
        assert!(html.contains("echarts"));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib report::compare::tests::render_compare_html_returns_markup`
Expected: 编译失败（`render_compare_html` 未定义）。

- [ ] **Step 3: 实现（compare）**

替换 `src/report/compare.rs` 的 `render_compare` 为：

```rust
pub fn render_compare_html(meta: &CompareMeta, runs: &[RunOutcome]) -> String {
    let payload = Payload {
        start: meta.start.to_string(),
        end: meta.end.to_string(),
        runs: runs.iter().map(|r| RunJson {
            name: r.name.clone(),
            fund_code: r.fund_code.clone(),
            summary: &r.summary,
            daily: &r.daily,
        }).collect(),
    };
    let data_json = serde_json::to_string(&payload)
        .expect("序列化对比数据失败")
        .replace("</", "<\\/");
    build_html(meta, runs, &data_json)
}

pub fn render_compare(meta: &CompareMeta, runs: &[RunOutcome], out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
    let html = render_compare_html(meta, runs);
    let out = out_dir.join("compare.html");
    std::fs::write(&out, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out.display()))?;
    Ok(out)
}
```

- [ ] **Step 4: 写失败测试（optimize）**

在 `src/report/optimize.rs` 的 `mod tests` 内，复用其已有 `outcome(...)`/`d` 工具新增：

```rust
    #[test]
    fn render_optimize_html_returns_markup() {
        let report = OptReport {
            strategy: "smart_dca".to_string(), metric: "total_return".to_string(), top_n: 5,
            ranked: vec![outcome("ma_window=250", 1.0, 250)],
            param_keys: vec!["ma_window".to_string()],
        };
        let meta = OptMeta { start: d(2024,1,1), end: d(2024,2,15), fund_code: "161725".to_string() };
        let html = render_optimize_html(&meta, &report);
        assert!(html.contains("参数寻优"));
        assert!(html.contains("ma_window"));
        assert!(html.contains("const DATA"));
    }
```

- [ ] **Step 5: 实现（optimize）**

替换 `src/report/optimize.rs` 的 `render_optimize` 为：

```rust
pub fn render_optimize_html(meta: &OptMeta, report: &OptReport) -> String {
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
        .expect("序列化寻优数据失败")
        .replace("</", "<\\/");
    build_html(meta, report, &data_json)
}

pub fn render_optimize(meta: &OptMeta, report: &OptReport, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
    let html = render_optimize_html(meta, report);
    let out = out_dir.join("optimize.html");
    std::fs::write(&out, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out.display()))?;
    Ok(out)
}
```

- [ ] **Step 6: 运行测试 + clippy + 提交**

Run: `cargo test --lib report`（新两测 + 既有 compare/optimize 测试全过）然后 `cargo clippy --all-targets`（无 warning）。
```bash
git add src/report/compare.rs src/report/optimize.rs
git commit -m "refactor: 抽出 render_compare_html / render_optimize_html"
```

---

## Task 2: web 共享纯函数（校验 + 策略参数表）DRY 重构

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces:
  - `pub struct StrategyFields { strategy:String, period:Option<String>, day:Option<u32>, base_amount:Option<f64>, ma_window:Option<usize>, k:Option<f64>, short_window:Option<usize>, long_window:Option<usize>, amount:Option<f64> }`（`#[derive(Debug, Deserialize)]`）
  - `fn validate_fund_code(&str)->Result<()>`、`fn validate_common(fund:&str,start:NaiveDate,end:NaiveDate,buy_rate:f64)->Result<()>`
  - `fn strategy_params_table(&StrategyFields)->Result<toml::Table>`、`pub fn build_strategy_from_fields(&StrategyFields)->Result<Box<dyn Strategy>>`
- `build_run_from_query` 改为复用以上（行为不变）。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增：

```rust
    fn sf(strategy: &str) -> StrategyFields {
        StrategyFields {
            strategy: strategy.into(),
            period: Some("monthly".into()), day: Some(1), base_amount: Some(1000.0),
            ma_window: Some(250), k: Some(1.0),
            short_window: Some(20), long_window: Some(60), amount: Some(1000.0),
        }
    }

    #[test]
    fn build_strategy_from_fields_each() {
        for s in ["dca", "smart_dca", "trend"] {
            assert!(build_strategy_from_fields(&sf(s)).is_ok(), "{s} 应成功");
        }
    }

    #[test]
    fn build_strategy_from_fields_missing_param() {
        let mut f = sf("smart_dca");
        f.ma_window = None;
        let err = build_strategy_from_fields(&f).unwrap_err();
        assert!(err.to_string().contains("ma_window"), "应提示缺 ma_window: {err}");
    }

    #[test]
    fn validate_fund_code_rejects_traversal() {
        assert!(validate_fund_code("../etc").is_err());
        assert!(validate_fund_code("161725").is_ok());
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::build_strategy_from_fields_each`
Expected: 编译失败（`StrategyFields`/`build_strategy_from_fields` 未定义）。

- [ ] **Step 3: 实现共享函数**

在 `src/web/mod.rs`（`RunQuery` 定义之后、`build_run_from_query` 之前）加：

```rust
#[derive(Debug, Deserialize)]
pub struct StrategyFields {
    pub strategy: String,
    #[serde(default)] pub period: Option<String>,
    #[serde(default)] pub day: Option<u32>,
    #[serde(default)] pub base_amount: Option<f64>,
    #[serde(default)] pub ma_window: Option<usize>,
    #[serde(default)] pub k: Option<f64>,
    #[serde(default)] pub short_window: Option<usize>,
    #[serde(default)] pub long_window: Option<usize>,
    #[serde(default)] pub amount: Option<f64>,
}

fn validate_fund_code(code: &str) -> Result<()> {
    if code.is_empty() || code.len() > 12 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(anyhow!("基金代码非法: {code} （只允许 1-12 位字母或数字）"));
    }
    Ok(())
}

fn validate_common(fund: &str, start: NaiveDate, end: NaiveDate, buy_rate: f64) -> Result<()> {
    if start >= end {
        return Err(anyhow!("回测区间错误: start ({start}) 必须早于 end ({end})"));
    }
    if !(0.0..1.0).contains(&buy_rate) {
        return Err(anyhow!("buy_rate ({buy_rate}) 必须在 [0.0, 1.0) 范围内"));
    }
    validate_fund_code(fund)
}

/// 按策略把字段拼成 build_strategy_from 所需的 toml 参数表（缺必填字段报错，含字段名）。
fn strategy_params_table(s: &StrategyFields) -> Result<toml::Table> {
    let mut t = toml::Table::new();
    let need = |o: bool, name: &str| -> Result<()> {
        if o { Ok(()) } else { Err(anyhow!("策略 {} 缺少必填参数: {}", s.strategy, name)) }
    };
    match s.strategy.as_str() {
        "dca" => {
            need(s.period.is_some(), "period")?;
            need(s.day.is_some(), "day")?;
            need(s.base_amount.is_some(), "base_amount")?;
            t.insert("period".into(), s.period.clone().unwrap().into());
            t.insert("day".into(), (s.day.unwrap() as i64).into());
            t.insert("base_amount".into(), s.base_amount.unwrap().into());
        }
        "smart_dca" => {
            need(s.period.is_some(), "period")?;
            need(s.day.is_some(), "day")?;
            need(s.base_amount.is_some(), "base_amount")?;
            need(s.ma_window.is_some(), "ma_window")?;
            t.insert("period".into(), s.period.clone().unwrap().into());
            t.insert("day".into(), (s.day.unwrap() as i64).into());
            t.insert("base_amount".into(), s.base_amount.unwrap().into());
            t.insert("ma_window".into(), (s.ma_window.unwrap() as i64).into());
            t.insert("k".into(), s.k.unwrap_or(1.0).into());
        }
        "trend" => {
            need(s.short_window.is_some(), "short_window")?;
            need(s.long_window.is_some(), "long_window")?;
            need(s.amount.is_some(), "amount")?;
            t.insert("short_window".into(), (s.short_window.unwrap() as i64).into());
            t.insert("long_window".into(), (s.long_window.unwrap() as i64).into());
            t.insert("amount".into(), s.amount.unwrap().into());
        }
        other => return Err(anyhow!("未知策略: {other}")),
    }
    Ok(t)
}

pub fn build_strategy_from_fields(s: &StrategyFields) -> Result<Box<dyn Strategy>> {
    let t = strategy_params_table(s)?;
    build_strategy_from(&s.strategy, &Some(toml::Value::Table(t)), &[])
}
```

- [ ] **Step 4: 重构 build_run_from_query 复用共享函数**

把 `build_run_from_query` 函数体替换为：

```rust
pub fn build_run_from_query(q: &RunQuery) -> Result<RunSpec> {
    validate_common(&q.fund_code, q.start, q.end, q.buy_rate)?;
    let sf = StrategyFields {
        strategy: q.strategy.clone(),
        period: q.period.clone(), day: q.day, base_amount: q.base_amount,
        ma_window: q.ma_window, k: q.k,
        short_window: q.short_window, long_window: q.long_window, amount: q.amount,
    };
    let strategy = build_strategy_from_fields(&sf)?;
    let fee = FeeModel { buy_rate: q.buy_rate, sell_tiers: standard_sell_tiers() };
    Ok(RunSpec {
        fund_code: q.fund_code.clone(),
        start: q.start, end: q.end, strategy, fee, initial_cash: q.initial_cash,
    })
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 新 3 测 + 既有 web 测试（含 `rejects_*`、`builds_each_strategy`、`index_serves_form`）全过——既有错误信息断言（"start"/"buy_rate"/"基金代码"/"ma_window"/未知策略）由 validate_common 与 strategy_params_table 保留。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/mod.rs
git commit -m "refactor: web 抽共享校验+策略参数表，单次复用 (DRY)"
```

---

## Task 3: 请求结构 + parse_csv_values + build_optimize_cfg

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `StrategyFields`、`config::OptimizeCfg`（Task 2 / 既有）。
- Produces:
  - `CompareRunReq{name:String, fund_code:Option<String>, #[serde(flatten)] params:StrategyFields}`
  - `CompareRequest{fund_code,start,end,buy_rate,initial_cash, runs:Vec<CompareRunReq>}`
  - `OptimizeRequest{fund_code,start,end,buy_rate,initial_cash, strategy:String, metric:String, top_n:usize, grid:BTreeMap<String,String>}`
  - `pub fn parse_csv_values(&str)->Result<Vec<toml::Value>>`
  - `pub fn build_optimize_cfg(&OptimizeRequest)->Result<config::OptimizeCfg>`

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增：

```rust
    #[test]
    fn parse_csv_values_typed() {
        let ints = parse_csv_values("120,250,500").unwrap();
        assert_eq!(ints.len(), 3);
        assert!(matches!(ints[0], toml::Value::Integer(120)));
        let floats = parse_csv_values("0.5, 1.0").unwrap();
        assert_eq!(floats.len(), 2);
        assert!(matches!(floats[0], toml::Value::Float(_)));
        let strs = parse_csv_values("monthly").unwrap();
        assert!(matches!(strs[0], toml::Value::String(_)));
        assert!(parse_csv_values("").is_err());
        assert!(parse_csv_values("  , ").is_err());
    }

    fn opt_req() -> OptimizeRequest {
        let mut grid = std::collections::BTreeMap::new();
        grid.insert("period".into(), "monthly".into());
        grid.insert("day".into(), "1".into());
        grid.insert("base_amount".into(), "1000".into());
        grid.insert("ma_window".into(), "120,250".into());
        grid.insert("k".into(), "1.0".into());
        OptimizeRequest {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            buy_rate: 0.0015, initial_cash: 0.0,
            strategy: "smart_dca".into(), metric: "sharpe".into(), top_n: 5, grid,
        }
    }

    #[test]
    fn build_optimize_cfg_smart_dca() {
        let cfg = build_optimize_cfg(&opt_req()).unwrap();
        assert_eq!(cfg.strategy, "smart_dca");
        assert_eq!(cfg.metric, "sharpe");
        assert_eq!(cfg.grid.len(), 5, "smart_dca 5 个网格参数");
        match cfg.grid.get("ma_window").unwrap() {
            toml::Value::Array(a) => assert_eq!(a.len(), 2),
            _ => panic!("ma_window 应为数组"),
        }
    }

    #[test]
    fn build_optimize_cfg_missing_param() {
        let mut req = opt_req();
        req.grid.remove("base_amount");
        let err = build_optimize_cfg(&req).unwrap_err();
        assert!(err.to_string().contains("base_amount"), "应提示缺 base_amount: {err}");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::parse_csv_values_typed`
Expected: 编译失败（类型/函数未定义）。

- [ ] **Step 3: 实现结构 + 函数**

在 `src/web/mod.rs` 顶部 `use` 区补 `use std::collections::BTreeMap;`（若未引入）。在 `build_strategy_from_fields` 之后加：

```rust
#[derive(Debug, Deserialize)]
pub struct CompareRunReq {
    pub name: String,
    #[serde(default)] pub fund_code: Option<String>,
    #[serde(flatten)] pub params: StrategyFields,
}

#[derive(Debug, Deserialize)]
pub struct CompareRequest {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    pub runs: Vec<CompareRunReq>,
}

fn default_top_n_web() -> usize { 5 }

#[derive(Debug, Deserialize)]
pub struct OptimizeRequest {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    pub strategy: String,
    pub metric: String,
    #[serde(default = "default_top_n_web")] pub top_n: usize,
    pub grid: BTreeMap<String, String>,
}

/// 把 "120,250,500" 拆成 toml 值数组；每值试 i64→f64→String。空/全空白报错。
pub fn parse_csv_values(s: &str) -> Result<Vec<toml::Value>> {
    let vals: Vec<toml::Value> = s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            if let Ok(i) = p.parse::<i64>() { toml::Value::Integer(i) }
            else if let Ok(f) = p.parse::<f64>() { toml::Value::Float(f) }
            else { toml::Value::String(p.to_string()) }
        })
        .collect();
    if vals.is_empty() {
        return Err(anyhow!("参数取值列表为空"));
    }
    Ok(vals)
}

/// 由寻优请求构建 OptimizeCfg：按策略取所需参数名，各 CSV → toml 数组。
pub fn build_optimize_cfg(req: &OptimizeRequest) -> Result<crate::config::OptimizeCfg> {
    let keys: &[&str] = match req.strategy.as_str() {
        "dca" => &["period", "day", "base_amount"],
        "smart_dca" => &["period", "day", "base_amount", "ma_window", "k"],
        "trend" => &["short_window", "long_window", "amount"],
        other => return Err(anyhow!("未知策略: {other}")),
    };
    let mut grid = toml::Table::new();
    for &name in keys {
        let csv = req.grid.get(name)
            .ok_or_else(|| anyhow!("寻优缺少参数网格: {name}"))?;
        let vals = parse_csv_values(csv)
            .map_err(|e| anyhow!("参数 {name} 取值非法: {e}"))?;
        grid.insert(name.to_string(), toml::Value::Array(vals));
    }
    Ok(crate::config::OptimizeCfg {
        strategy: req.strategy.clone(),
        metric: req.metric.clone(),
        top_n: req.top_n,
        grid,
        rules: Vec::new(),
    })
}
```

注：`OptimizeCfg` 字段须 `pub`（已有 strategy/metric/top_n/grid/rules 均 pub——`rules: Vec<RuleCfg>`）。metric 合法性不在此重复校验（`optimize::run_optimize` 已校验并报清晰错误，避免重复 METRICS 列表）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 新 3 测 + 既有全过。

- [ ] **Step 5: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/mod.rs
git commit -m "feat: web 对比/寻优请求结构 + parse_csv_values + build_optimize_cfg"
```

---

## Task 4: 对比/寻优 handler + 路由

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]` 路由测试）

**Interfaces:**
- Consumes: Task 1 `render_compare_html`/`render_optimize_html`、Task 2 `build_strategy_from_fields`/`validate_common`/`validate_fund_code`、Task 3 请求结构 + `build_optimize_cfg`、`runner::run_one`、`optimize::run_optimize`。
- Produces: `compare_handler`、`optimize_handler`；`router()` 注册 `POST /api/compare`、`POST /api/optimize`。

- [ ] **Step 1: 写失败的路由错误路径测试（hermetic）**

在 `src/web/mod.rs` 的 `mod tests` 内新增（这些路径在 load_or_fetch 之前就报错，不依赖网络/缓存）：

```rust
    async fn post_json(uri: &str, body: serde_json::Value) -> axum::http::StatusCode {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().method("POST").uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())).unwrap())
            .await.unwrap();
        resp.status()
    }

    #[tokio::test]
    async fn compare_empty_runs_is_400() {
        let body = serde_json::json!({
            "fund_code":"161725","start":"2024-01-01","end":"2024-12-31",
            "buy_rate":0.0015,"initial_cash":0.0,"runs":[]
        });
        assert_eq!(post_json("/api/compare", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn optimize_missing_grid_param_is_400() {
        // smart_dca 缺 base_amount → build_optimize_cfg 在加载数据前即报错
        let body = serde_json::json!({
            "fund_code":"161725","start":"2024-01-01","end":"2024-12-31",
            "buy_rate":0.0015,"initial_cash":0.0,"strategy":"smart_dca","metric":"sharpe","top_n":5,
            "grid":{"period":"monthly","day":"1","ma_window":"120,250","k":"1.0"}
        });
        assert_eq!(post_json("/api/optimize", body).await, axum::http::StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::compare_empty_runs_is_400`
Expected: 失败（路由不存在 → 404 ≠ 400，或编译期 handler 未定义）。

- [ ] **Step 3: 实现 handlers + 注册路由**

在 `router()` 里加两条路由（与现有 `.route("/api/run", get(run_handler))` 并列）：

```rust
    .route("/api/compare", axum::routing::post(compare_handler))
    .route("/api/optimize", axum::routing::post(optimize_handler))
```

在 `run_handler`/`run_blocking` 之后加：

```rust
async fn compare_handler(
    axum::Json(req): axum::Json<CompareRequest>,
) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || compare_blocking(req))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

fn compare_blocking(req: CompareRequest) -> Result<String> {
    validate_common(&req.fund_code, req.start, req.end, req.buy_rate)?;
    if req.runs.is_empty() {
        return Err(anyhow!("对比至少需要一个策略"));
    }
    let fee = crate::broker::FeeModel { buy_rate: req.buy_rate, sell_tiers: standard_sell_tiers() };
    let mut outcomes = Vec::with_capacity(req.runs.len());
    for run in &req.runs {
        let fund = run.fund_code.clone().unwrap_or_else(|| req.fund_code.clone());
        validate_fund_code(&fund)?;
        let strategy = build_strategy_from_fields(&run.params)
            .map_err(|e| anyhow!("策略 [{}] 构建失败: {e}", run.name))?;
        let points = crate::data::cache::load_or_fetch(
            &fund, std::path::Path::new(".cache"), req.start, req.end)
            .map_err(|e| anyhow!("策略 [{}] 加载 {fund} 失败: {e}", run.name))?;
        let outcome = crate::runner::run_one(
            run.name.clone(), fund, points, strategy, fee.clone(), req.initial_cash);
        outcomes.push(outcome);
    }
    let meta = crate::report::compare::CompareMeta { start: req.start, end: req.end };
    Ok(crate::report::compare::render_compare_html(&meta, &outcomes))
}

async fn optimize_handler(
    axum::Json(req): axum::Json<OptimizeRequest>,
) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || optimize_blocking(req))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

fn optimize_blocking(req: OptimizeRequest) -> Result<String> {
    validate_common(&req.fund_code, req.start, req.end, req.buy_rate)?;
    let cfg = build_optimize_cfg(&req)?;
    let fee = crate::broker::FeeModel { buy_rate: req.buy_rate, sell_tiers: standard_sell_tiers() };
    let points = crate::data::cache::load_or_fetch(
        &req.fund_code, std::path::Path::new(".cache"), req.start, req.end)
        .map_err(|e| anyhow!("加载净值失败: {}", req.fund_code))?;
    let report = crate::optimize::run_optimize(&cfg, &req.fund_code, &points, fee, req.initial_cash)?;
    let meta = crate::report::optimize::OptMeta {
        start: req.start, end: req.end, fund_code: req.fund_code.clone(),
    };
    Ok(crate::report::optimize::render_optimize_html(&meta, &report))
}
```

注：`compare_empty_runs_is_400` 与 `optimize_missing_grid_param_is_400` 都在 `load_or_fetch` 之前报错（runs 空 / build_optimize_cfg 缺参），故 hermetic。Send 安全：`Box<dyn Strategy>` 仅在 `compare_blocking`/`run_optimize`（blocking 线程内）创建消费。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 两路由测试 PASS（返回 400）+ 既有全过。

- [ ] **Step 5: clippy + 全量测试 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add src/web/mod.rs
git commit -m "feat: POST /api/compare 与 /api/optimize handler"
```

---

## Task 5: 前端三 Tab（重写 page.rs）

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/mod.rs`（在 `index_serves_form` 旁加 tab 标识断言）

**Interfaces:**
- Consumes: `POST /api/compare`、`POST /api/optimize`、`GET /api/run`。

- [ ] **Step 1: 写失败测试（GET / 含三 Tab 标识）**

在 `src/web/mod.rs` 的 `mod tests` 内 `index_serves_form` 之后新增：

```rust
    #[tokio::test]
    async fn index_has_three_tabs() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"single\""), "单次 tab");
        assert!(body.contains("data-tab=\"compare\""), "对比 tab");
        assert!(body.contains("data-tab=\"optimize\""), "寻优 tab");
        assert!(body.contains("/api/compare"), "对比提交端点");
        assert!(body.contains("/api/optimize"), "寻优提交端点");
        assert!(body.contains("id=\"result\""), "结果 iframe");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::index_has_three_tabs`
Expected: 失败（现 INDEX_HTML 无 `data-tab`）。

- [ ] **Step 3: 重写 page.rs**

将 `src/web/page.rs` 整体替换为以下内容（三 Tab + 三表单 + JS；单次表单逻辑保留在 single panel）：

```rust
/// 本地回测界面首页：三 Tab（单次/对比/寻优）+ 结果 iframe（纯静态，无外部依赖）。
pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>xlh 回测</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Arial,sans-serif;background:#f5f6fa;color:#2c3e50}
.wrap{max-width:1200px;margin:0 auto;padding:20px 16px}
h1{font-size:1.5rem;color:#1a252f;margin-bottom:14px}
.tabs{display:flex;gap:6px;margin-bottom:14px;border-bottom:2px solid #e0e4ea}
.tab{padding:9px 18px;cursor:pointer;border:none;background:none;font-size:.95rem;color:#7f8c8d;border-bottom:2px solid transparent;margin-bottom:-2px}
.tab.active{color:#c0392b;border-bottom-color:#c0392b;font-weight:600}
.panel{display:none}
.panel.active{display:block}
.card{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:18px;margin-bottom:16px;box-shadow:0 1px 4px rgba(0,0,0,.06)}
.row{display:flex;flex-wrap:wrap;gap:12px 18px;align-items:flex-end}
.field{display:flex;flex-direction:column;gap:4px}
.field label{font-size:.8rem;color:#5a6a7a}
.field input,.field select{padding:7px 9px;border:1px solid #cfd6e0;border-radius:6px;font-size:.9rem}
.field input[type=number]{width:120px}
button.run{padding:9px 22px;background:#c0392b;color:#fff;border:none;border-radius:6px;font-size:.95rem;cursor:pointer;margin-top:12px}
button.run:disabled{opacity:.5;cursor:wait}
.params.hidden{display:none}
.crow{border:1px solid #eaecef;border-radius:8px;padding:12px;margin-bottom:10px;background:#fafbfc}
.crow .row{align-items:flex-end}
button.small{padding:5px 12px;border:1px solid #cfd6e0;background:#fff;border-radius:6px;cursor:pointer;font-size:.85rem}
button.del{color:#c0392b;border-color:#e8b9b3}
#result{width:100%;height:1400px;border:1px solid #e0e4ea;border-radius:10px;background:#fff;margin-top:8px}
.hint{color:#7f8c8d;font-size:.85rem;margin-top:8px}
</style>
</head>
<body>
<div class="wrap">
  <h1>xlh 基金回测</h1>
  <div class="tabs">
    <button class="tab active" data-tab="single">单次</button>
    <button class="tab" data-tab="compare">对比</button>
    <button class="tab" data-tab="optimize">寻优</button>
  </div>

  <!-- 单次 -->
  <div class="panel active" id="panel-single">
    <div class="card">
      <form id="f-single" class="row">
        <div class="field"><label>基金代码</label><input name="fund_code" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" name="start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" name="end" value="2024-12-31"/></div>
        <div class="field"><label>策略</label>
          <select name="strategy" class="strat">
            <option value="dca">普通定投</option>
            <option value="smart_dca" selected>智能定投</option>
            <option value="trend">均线择时</option>
          </select>
        </div>
        <span class="params" data-for="dca smart_dca" style="display:contents">
          <div class="field"><label>周期</label><select name="period"><option value="monthly">月</option><option value="weekly">周</option></select></div>
          <div class="field"><label>定投日</label><input type="number" name="day" value="1"/></div>
          <div class="field"><label>每期金额</label><input type="number" name="base_amount" value="1000"/></div>
        </span>
        <span class="params" data-for="smart_dca" style="display:contents">
          <div class="field"><label>均线窗口</label><input type="number" name="ma_window" value="250"/></div>
          <div class="field"><label>k 系数</label><input type="number" step="0.1" name="k" value="1.0"/></div>
        </span>
        <span class="params" data-for="trend" style="display:contents">
          <div class="field"><label>短窗口</label><input type="number" name="short_window" value="20"/></div>
          <div class="field"><label>长窗口</label><input type="number" name="long_window" value="60"/></div>
          <div class="field"><label>每次金额</label><input type="number" name="amount" value="1000"/></div>
        </span>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" name="buy_rate" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" name="initial_cash" value="0"/></div>
      </form>
      <button class="run" id="run-single">运行</button>
    </div>
  </div>

  <!-- 对比 -->
  <div class="panel" id="panel-compare">
    <div class="card">
      <div class="row">
        <div class="field"><label>默认基金代码</label><input id="cmp-fund" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" id="cmp-start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" id="cmp-end" value="2024-12-31"/></div>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" id="cmp-buy" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" id="cmp-cash" value="0"/></div>
      </div>
      <div id="compare-rows" style="margin-top:14px"></div>
      <button class="small" id="add-row">+ 添加策略</button>
      <button class="run" id="run-compare">运行对比</button>
      <div class="hint">每行一个命名策略；基金代码留空则用默认基金。</div>
    </div>
  </div>

  <!-- 寻优 -->
  <div class="panel" id="panel-optimize">
    <div class="card">
      <div class="row">
        <div class="field"><label>基金代码</label><input id="opt-fund" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" id="opt-start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" id="opt-end" value="2024-12-31"/></div>
        <div class="field"><label>策略</label>
          <select id="opt-strat" class="strat-opt">
            <option value="dca">普通定投</option>
            <option value="smart_dca" selected>智能定投</option>
            <option value="trend">均线择时</option>
          </select>
        </div>
        <div class="field"><label>排序指标</label>
          <select id="opt-metric">
            <option value="sharpe" selected>夏普</option>
            <option value="total_return">总收益</option>
            <option value="annualized">年化</option>
            <option value="max_drawdown">最大回撤</option>
          </select>
        </div>
        <div class="field"><label>Top-N</label><input type="number" id="opt-topn" value="5"/></div>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" id="opt-buy" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" id="opt-cash" value="0"/></div>
      </div>
      <div class="row" id="opt-grid" style="margin-top:14px"></div>
      <button class="run" id="run-optimize">运行寻优</button>
      <div class="hint">参数填逗号分隔的多个候选值，如 均线窗口 = 120,250,500。取笛卡尔积。</div>
    </div>
  </div>

  <iframe id="result" title="回测报告"></iframe>
</div>

<script>
var GRID_FIELDS = {
  dca: [["period","周期(月/周)","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]],
  smart_dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"],["ma_window","均线窗口(可多值)","120,250,500"],["k","k系数(可多值)","0.5,1.0,1.5"]],
  trend: [["short_window","短窗口(可多值)","10,20"],["long_window","长窗口(可多值)","60,120"],["amount","每次金额","1000"]]
};
var ROW_FIELDS = {
  dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]],
  smart_dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"],["ma_window","均线窗口","250"],["k","k系数","1.0"]],
  trend: [["short_window","短窗口","20"],["long_window","长窗口","60"],["amount","每次金额","1000"]]
};
var iframe = document.getElementById('result');

// Tab 切换
document.querySelectorAll('.tab').forEach(function(t){
  t.addEventListener('click', function(){
    document.querySelectorAll('.tab').forEach(function(x){x.classList.remove('active');});
    document.querySelectorAll('.panel').forEach(function(x){x.classList.remove('active');});
    t.classList.add('active');
    document.getElementById('panel-' + t.getAttribute('data-tab')).classList.add('active');
  });
});

// 单次：随策略显隐参数组
var singleStrat = document.querySelector('#f-single .strat');
function syncSingle(){
  var s = singleStrat.value;
  document.querySelectorAll('#f-single .params').forEach(function(g){
    var on = g.getAttribute('data-for').split(' ').indexOf(s) >= 0;
    g.style.display = on ? 'contents' : 'none';
    g.querySelectorAll('input,select').forEach(function(el){ el.disabled = !on; });
  });
}
singleStrat.addEventListener('change', syncSingle); syncSingle();

function setBtn(btn, busy, label){ btn.disabled = busy; btn.textContent = busy ? '运行中…' : label; }
function showErr(e){ iframe.srcdoc = '<p style="color:#c0392b;padding:20px;font-family:sans-serif">请求失败: ' + e + '</p>'; }

// 单次运行 (GET)
document.getElementById('run-single').addEventListener('click', function(){
  var btn = this; var fd = new FormData(document.getElementById('f-single'));
  var qs = new URLSearchParams();
  for (var pair of fd.entries()) { if (pair[1] !== '') qs.append(pair[0], pair[1]); }
  setBtn(btn, true, '运行');
  fetch('/api/run?' + qs.toString()).then(function(r){return r.text();})
    .then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行'); });
});

// 对比：动态行
function strategySelect(cls){
  return '<select class="' + cls + '"><option value="dca">普通定投</option><option value="smart_dca" selected>智能定投</option><option value="trend">均线择时</option></select>';
}
function buildRowParams(div, strat){
  var holder = div.querySelector('.rowparams');
  holder.innerHTML = '';
  ROW_FIELDS[strat].forEach(function(f){
    holder.insertAdjacentHTML('beforeend',
      '<div class="field"><label>'+f[1]+'</label><input data-k="'+f[0]+'" value="'+f[2]+'"/></div>');
  });
}
function addCompareRow(){
  var div = document.createElement('div');
  div.className = 'crow';
  div.innerHTML = '<div class="row">'
    + '<div class="field"><label>名称</label><input class="rname" value="策略'+(document.querySelectorAll('.crow').length+1)+'"/></div>'
    + '<div class="field"><label>策略</label>'+strategySelect('rstrat')+'</div>'
    + '<div class="field"><label>基金(可空)</label><input class="rfund" placeholder="默认"/></div>'
    + '<span class="rowparams" style="display:contents"></span>'
    + '<button class="small del">删除</button></div>';
  document.getElementById('compare-rows').appendChild(div);
  var sel = div.querySelector('.rstrat');
  buildRowParams(div, sel.value);
  sel.addEventListener('change', function(){ buildRowParams(div, sel.value); });
  div.querySelector('.del').addEventListener('click', function(){ div.remove(); });
}
document.getElementById('add-row').addEventListener('click', addCompareRow);
addCompareRow(); addCompareRow();

document.getElementById('run-compare').addEventListener('click', function(){
  var btn = this;
  var runs = [];
  document.querySelectorAll('#compare-rows .crow').forEach(function(div){
    var run = { name: div.querySelector('.rname').value, strategy: div.querySelector('.rstrat').value };
    var fund = div.querySelector('.rfund').value.trim();
    if (fund) run.fund_code = fund;
    div.querySelectorAll('.rowparams input').forEach(function(inp){
      var k = inp.getAttribute('data-k'); var v = inp.value.trim();
      if (v === '') return;
      run[k] = (k === 'period') ? v : Number(v);
    });
    runs.push(run);
  });
  var payload = {
    fund_code: document.getElementById('cmp-fund').value,
    start: document.getElementById('cmp-start').value,
    end: document.getElementById('cmp-end').value,
    buy_rate: Number(document.getElementById('cmp-buy').value),
    initial_cash: Number(document.getElementById('cmp-cash').value),
    runs: runs
  };
  setBtn(btn, true, '运行对比');
  fetch('/api/compare', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(r){return r.text();}).then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行对比'); });
});

// 寻优：随策略生成 CSV 参数框
var optStrat = document.getElementById('opt-strat');
function buildOptGrid(){
  var holder = document.getElementById('opt-grid'); holder.innerHTML = '';
  GRID_FIELDS[optStrat.value].forEach(function(f){
    holder.insertAdjacentHTML('beforeend',
      '<div class="field"><label>'+f[1]+'</label><input data-k="'+f[0]+'" value="'+f[2]+'"/></div>');
  });
}
optStrat.addEventListener('change', buildOptGrid); buildOptGrid();

document.getElementById('run-optimize').addEventListener('click', function(){
  var btn = this; var grid = {};
  document.querySelectorAll('#opt-grid input').forEach(function(inp){
    var v = inp.value.trim(); if (v !== '') grid[inp.getAttribute('data-k')] = v;
  });
  var payload = {
    fund_code: document.getElementById('opt-fund').value,
    start: document.getElementById('opt-start').value,
    end: document.getElementById('opt-end').value,
    buy_rate: Number(document.getElementById('opt-buy').value),
    initial_cash: Number(document.getElementById('opt-cash').value),
    strategy: optStrat.value,
    metric: document.getElementById('opt-metric').value,
    top_n: Number(document.getElementById('opt-topn').value),
    grid: grid
  };
  setBtn(btn, true, '运行寻优');
  fetch('/api/optimize', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(r){return r.text();}).then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行寻优'); });
});
</script>
</body>
</html>
"##;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: `index_has_three_tabs` + `index_serves_form`（仍含 `name="fund_code"`/`id="result"`/`运行`）+ 既有全过。

- [ ] **Step 5: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning；page.rs 是字符串常量，clippy 无影响）
```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat: Web 界面三 Tab（单次/对比/寻优）前端"
```

---

## Task 6: Playwright 端到端自测（对比 + 寻优）

**Files:**
- Modify: `scripts/verify_web.py`

- [ ] **Step 1: 扩展 verify_web.py 覆盖三 Tab**

在 `scripts/verify_web.py` 现有单次校验之后（同一 `with sync_playwright()` 会话内、`browser.close()` 之前）加入对比与寻优的校验。完整新增片段：

```python
        # ---- 对比 tab ----
        page.click('.tab[data-tab="compare"]')
        page.click("#run-compare")  # 默认两行策略
        cframe = page.frame_locator("#result")
        cframe.locator("canvas").first.wait_for(timeout=60000)
        ctext = cframe.locator("body").inner_text(timeout=10000)
        assert "总收益" in ctext, "对比报告应含 总收益"
        page.screenshot(path=str(Path("output/web_compare.png").resolve()), full_page=True)

        # ---- 寻优 tab ----
        page.click('.tab[data-tab="optimize"]')
        page.click("#run-optimize")  # 默认网格 (smart_dca ma_window/k 多值)
        ofr = page.frame_locator("#result")
        ofr.locator("canvas").first.wait_for(timeout=120000)
        otext = ofr.locator("body").inner_text(timeout=10000)
        assert "参数寻优" in otext, "寻优报告应含 参数寻优标题"
        page.screenshot(path=str(Path("output/web_optimize.png").resolve()), full_page=True)
```

并把最终 PASS 打印改为：

```python
        print("PASS: 单次/对比/寻优三 tab 均渲染成功")
```

（保留脚本顶部已有的 `*_PROXY` 环境变量清除逻辑——chromium 须直连 127.0.0.1。）

- [ ] **Step 2: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 `PASS: 单次/对比/寻优三 tab 均渲染成功`；生成 `output/web_screenshot.png`、`output/web_compare.png`、`output/web_optimize.png`。

- [ ] **Step 3: 肉眼核对截图**

Read `output/web_compare.png`（多条收益曲线 + 对比表）与 `output/web_optimize.png`（排名表 + Top-N 叠图）。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 4: 全量测试 + 提交**

Run: `cargo test`（全绿）。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 覆盖对比/寻优 tab"
```

交付报告：三 tab 截图、各断言结果、对比与寻优的实际数字。

---

## Self-Review

- **Spec 覆盖**：§3 前端三 Tab→T5；§4 路由→T4；§5 请求结构→T3；§6 纯函数（strategy_params_table/build_strategy_from_fields→T2，parse_csv_values/build_optimize_cfg→T3）；§7 数据流→T4 handler；§8 渲染抽取→T1；§9 错误处理→T2/T4（validate_common、AppError）；§10 测试→T2/T3 单元、T4 路由、T5 GET/、T6 e2e；§11 单元边界→各 Task 文件划分；§12 对既有影响→T2(build_run_from_query 复用)、T1(render 委托)、T5(page 重写)。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码。
- **类型一致**：`StrategyFields`(T2) 被 `CompareRunReq.params`(T3) flatten、`build_strategy_from_fields`(T2) 复用；`build_optimize_cfg->OptimizeCfg`(T3) 喂 `run_optimize`(T4)；`render_compare_html(&CompareMeta,&[RunOutcome])`/`render_optimize_html(&OptMeta,&OptReport)`(T1) = T4 调用签名；`run_one`/`run_optimize` 签名与既有一致；`CompareMeta{start,end}`、`OptMeta{start,end,fund_code}` 与 report 实际字段一致。
- **Send 安全**：对比/寻优的 `Box<dyn Strategy>` 仅在 `compare_blocking`/`run_optimize`（spawn_blocking 线程内）创建消费，闭包仅捕获请求结构（Send），返回 String。
- **DRY**：校验与策略参数表逻辑单处（T2），单次/对比/寻优共用；metric 校验不重复（交给 run_optimize）。
- **Hermetic 测试**：T4 两路由测试在 load_or_fetch 前报错（空 runs / 缺网格参数），不依赖网络；happy-path 由 T6 Playwright（用缓存 161725）。
- **不破坏既有**：T1 render 委托、T2 build_run_from_query 复用（既有 web/report 测试断言保留）、T5 单次表单保留在 single panel。
- **YAGNI**：不做保存/分享、per-run 区间、寻优扫规则。

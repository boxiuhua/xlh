# 本地回测 Web 界面（单次回测）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 新增 `xlh serve` 子命令，启动本地 Web 表单：选基金/区间/策略/参数 → 后端实时跑单次回测 → 把现有 ECharts 报告刷新到页面 iframe。

**Architecture:** axum + tokio 起一个本地服务；后端复用现有 `Engine` 跑回测、复用抽出的 `report::html::render_report_html` 渲染；前端是静态表单页，`fetch('/api/run')` 后把返回的报告 HTML 塞进 `iframe.srcdoc`。引擎、config 构建、图表全部不重写。

**Tech Stack:** Rust（axum 0.7 / tokio 1 / serde）+ 现有 ECharts 报告 + Playwright 自测。

## Global Constraints

- 不改动 `engine.rs`/`runner.rs`/`portfolio.rs`/`broker.rs`/既有策略逻辑；只新增 web 模块、CLI 分流、抽 `render_report_html`。
- 不破坏既有 64 测试与 clippy 干净。
- 服务绑定 `127.0.0.1`，端口默认 8080（`--port` 可改）；仅本机，不对外。
- 无子命令时 `--config` 老行为不变（向后兼容）。
- 卖出费率首版固定标准阶梯 `[{max_days:7,rate:0.015},{max_days:365,rate:0.005},{max_days:0,rate:0.0}]`，不进表单；表单只暴露 `buy_rate`、`initial_cash`。
- `buy_rate` 必须在 `[0.0,1.0)`；`start < end`；未知策略报错。
- 非 `Send` 的 `Box<dyn Strategy>` 必须在 `spawn_blocking` 闭包内创建并消费，不得跨 `.await`。
- 缓存目录固定 `.cache`（与 CLI 一致）。
- edition 2021；提交信息用仓库现有风格，每个 Task 末尾提交一次，含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `report::html::ReportMeta { fund_code: String, start: NaiveDate, end: NaiveDate, strategy: String, strategy_desc: String, initial_cash: f64 }`
- `report::html::render_report(meta, pf, daily, trades, out_dir) -> Result<PathBuf>`（内部 `build_html(meta, &MetricsJson, trades, &data_json) -> String`）
- `report::html_escape(&str) -> String`（crate 内 `pub(crate)`）
- `config::build_strategy_from(kind: &str, params: &Option<toml::Value>, rules: &[RuleCfg]) -> Result<Box<dyn Strategy>>`
- `data::cache::load_or_fetch(fund: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> Result<Vec<NavPoint>>`
- `data::InMemoryData::new(Vec<NavPoint>)`；`broker::{Broker::new(FeeModel), FeeModel{buy_rate,sell_tiers}, SellTier{max_days,rate}}`
- `portfolio::Portfolio::new(f64)`；`engine::Engine::new(data, strategy, broker, portfolio)`，`engine.run()`，`engine.portfolio()/daily()/trades()`（均 `&self`）
- `strategy::Strategy`（trait）；`result::{DailyRecord, TradeRecord}`

---

## Task 1: 抽出 render_report_html

**Files:**
- Modify: `src/report/html.rs`
- Test: `src/report/html.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces: `pub fn render_report_html(meta: &ReportMeta, pf: &crate::portfolio::Portfolio, daily: &[crate::result::DailyRecord], trades: &[crate::result::TradeRecord]) -> String`
- `render_report(...)`（写文件）改为内部调用它。

- [ ] **Step 1: 写失败测试**

在 `src/report/html.rs` 的 `mod tests` 内新增（复用其已有的测试构造工具；若该文件测试已有构造 pf/daily/trades 的 helper，直接用，否则用下方自带构造）：

```rust
    #[test]
    fn render_report_html_contains_core_markup() {
        use crate::portfolio::{Portfolio, EquityPoint};
        let mut pf = Portfolio::new(0.0);
        pf.curve.push(EquityPoint { date: d(2024,1,1), equity: 1000.0, contribution: 1000.0 });
        pf.curve.push(EquityPoint { date: d(2024,2,1), equity: 1500.0, contribution: 0.0 });
        pf.total_contributed = 1000.0;
        pf.flows = vec![(d(2024,1,1), -1000.0), (d(2024,2,1), 1500.0)];
        let meta = ReportMeta {
            fund_code: "161725".into(), start: d(2024,1,1), end: d(2024,2,1),
            strategy: "dca".into(), strategy_desc: "dca".into(), initial_cash: 0.0,
        };
        let html = render_report_html(&meta, &pf, &[], &[]);
        assert!(html.contains("const DATA"), "应内嵌 const DATA");
        assert!(html.contains("总收益"), "应含指标标签");
        assert!(html.contains("echarts"), "应引用 echarts");
    }
```

（若 `mod tests` 内尚无 `fn d(...)`，在其顶部加 `fn d(y:i32,m:u32,day:u32)->chrono::NaiveDate{chrono::NaiveDate::from_ymd_opt(y,m,day).unwrap()}`。）

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib report::html::tests::render_report_html_contains_core_markup`
Expected: 编译失败（`render_report_html` 未定义）。

- [ ] **Step 3: 实现 render_report_html，render_report 改为委托**

把 `render_report` 中"计算 payload → data_json → build_html"那段移入新函数。替换现有 `render_report` 为：

```rust
/// 渲染单次回测报告为完整自包含 HTML 字符串。
pub fn render_report_html(
    meta: &ReportMeta,
    pf: &Portfolio,
    daily: &[DailyRecord],
    trades: &[TradeRecord],
) -> String {
    let s = crate::metrics::summarize(pf, trades.len());
    let payload = Payload {
        meta: MetaJson {
            fund_code: &meta.fund_code,
            start: meta.start.to_string(),
            end: meta.end.to_string(),
            strategy_desc: &meta.strategy_desc,
        },
        metrics: MetricsJson {
            total_return: s.total_return,
            annualized: s.annualized,
            max_drawdown: s.max_drawdown,
            sharpe: s.sharpe,
            final_equity: s.final_equity,
            total_contributed: s.total_contributed,
            trade_count: s.trade_count,
        },
        daily,
        trades,
    };
    let data_json = serde_json::to_string(&payload)
        .expect("报告数据序列化失败")
        .replace("</", "<\\/");
    build_html(meta, &payload.metrics, trades, &data_json)
}

pub fn render_report(
    meta: &ReportMeta,
    pf: &Portfolio,
    daily: &[DailyRecord],
    trades: &[TradeRecord],
    out_dir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("创建输出目录 {} 失败", out_dir.display()))?;
    let html = render_report_html(meta, pf, daily, trades);
    let out_path = out_dir.join("report.html");
    std::fs::write(&out_path, html.as_bytes())
        .with_context(|| format!("写入 {} 失败", out_path.display()))?;
    Ok(out_path)
}
```

注意：`Payload`/`MetaJson`/`MetricsJson`/`build_html` 均已存在，保持不动。`serde_json::to_string` 原来用 `?` 返回错误；新纯函数改用 `.expect`（序列化本地结构体不会失败，且签名不返回 Result）。

- [ ] **Step 4: 运行测试确认通过 + 既有报告测试不回归**

Run: `cargo test --lib report::html`
Expected: 新测试 PASS，既有 `renders_self_contained_report` / 注入转义等测试仍 PASS。

- [ ] **Step 5: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/report/html.rs
git commit -m "refactor: 抽出 render_report_html 供 server 复用"
```

---

## Task 2: web 数据层 build_run_from_query

**Files:**
- Create: `src/web/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod web;`）
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `config::build_strategy_from`、`broker::{FeeModel, SellTier}`、`strategy::Strategy`。
- Produces:
  - `pub struct RunQuery { ... }`（见下，serde Deserialize）
  - `pub struct RunSpec { pub fund_code: String, pub start: NaiveDate, pub end: NaiveDate, pub strategy: Box<dyn Strategy>, pub fee: FeeModel, pub initial_cash: f64 }`
  - `pub fn build_run_from_query(q: &RunQuery) -> anyhow::Result<RunSpec>`

- [ ] **Step 1: 在 `src/lib.rs` 注册模块**

在 `src/lib.rs` 现有模块声明区加：

```rust
pub mod web;
```

- [ ] **Step 2: 写失败测试 + 类型/签名占位**

创建 `src/web/mod.rs`：

```rust
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use crate::broker::{FeeModel, SellTier};
use crate::config::build_strategy_from;
use crate::strategy::Strategy;

#[derive(Debug, Deserialize)]
pub struct RunQuery {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: String,
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    #[serde(default)] pub period: Option<String>,
    #[serde(default)] pub day: Option<u32>,
    #[serde(default)] pub base_amount: Option<f64>,
    #[serde(default)] pub ma_window: Option<usize>,
    #[serde(default)] pub k: Option<f64>,
    #[serde(default)] pub short_window: Option<usize>,
    #[serde(default)] pub long_window: Option<usize>,
    #[serde(default)] pub amount: Option<f64>,
}

pub struct RunSpec {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: Box<dyn Strategy>,
    pub fee: FeeModel,
    pub initial_cash: f64,
}

/// 标准 A 股基金卖出费率阶梯（首版固定，不进表单）。
fn standard_sell_tiers() -> Vec<SellTier> {
    vec![
        SellTier { max_days: 7, rate: 0.015 },
        SellTier { max_days: 365, rate: 0.005 },
        SellTier { max_days: 0, rate: 0.0 },
    ]
}

/// 校验 query 并组装回测所需的一切；纯函数，不做任何 IO。
pub fn build_run_from_query(q: &RunQuery) -> Result<RunSpec> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(strategy: &str) -> RunQuery {
        RunQuery {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            strategy: strategy.into(),
            buy_rate: 0.0015,
            initial_cash: 0.0,
            period: Some("monthly".into()), day: Some(1), base_amount: Some(1000.0),
            ma_window: Some(250), k: Some(1.0),
            short_window: Some(20), long_window: Some(60), amount: Some(1000.0),
        }
    }

    #[test]
    fn builds_each_strategy() {
        for s in ["dca", "smart_dca", "trend"] {
            let spec = build_run_from_query(&base(s)).unwrap_or_else(|e| panic!("{s} 应成功: {e}"));
            assert_eq!(spec.fund_code, "161725");
            assert_eq!(spec.fee.sell_tiers.len(), 3);
            assert!((spec.fee.buy_rate - 0.0015).abs() < 1e-9);
        }
    }

    #[test]
    fn rejects_start_after_end() {
        let mut q = base("dca");
        q.start = NaiveDate::from_ymd_opt(2025,1,1).unwrap();
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("start") || err.to_string().contains("区间"),
            "应提示区间错误: {err}");
    }

    #[test]
    fn rejects_unknown_strategy() {
        let mut q = base("bogus");
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("bogus") || err.to_string().contains("策略"),
            "应提示未知策略: {err}");
    }

    #[test]
    fn rejects_bad_buy_rate() {
        let mut q = base("dca");
        q.buy_rate = 1.5;
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("buy_rate"), "应提示 buy_rate: {err}");
    }

    #[test]
    fn rejects_smart_dca_missing_ma_window() {
        let mut q = base("smart_dca");
        q.ma_window = None;
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("ma_window"), "应提示缺 ma_window: {err}");
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib web::tests::builds_each_strategy`
Expected: panic（`todo!()`）。

- [ ] **Step 4: 实现 build_run_from_query**

把 `todo!()` 替换为：

```rust
pub fn build_run_from_query(q: &RunQuery) -> Result<RunSpec> {
    if q.start >= q.end {
        return Err(anyhow!("回测区间错误: start ({}) 必须早于 end ({})", q.start, q.end));
    }
    if !(0.0..1.0).contains(&q.buy_rate) {
        return Err(anyhow!("buy_rate ({}) 必须在 [0.0, 1.0) 范围内", q.buy_rate));
    }

    // 按策略拼参数表（缺必填字段报错，含字段名）
    let mut t = toml::Table::new();
    let need = |o: bool, name: &str| -> Result<()> {
        if o { Ok(()) } else { Err(anyhow!("策略 {} 缺少必填参数: {}", q.strategy, name)) }
    };
    match q.strategy.as_str() {
        "dca" => {
            need(q.period.is_some(), "period")?;
            need(q.day.is_some(), "day")?;
            need(q.base_amount.is_some(), "base_amount")?;
            t.insert("period".into(), q.period.clone().unwrap().into());
            t.insert("day".into(), (q.day.unwrap() as i64).into());
            t.insert("base_amount".into(), q.base_amount.unwrap().into());
        }
        "smart_dca" => {
            need(q.period.is_some(), "period")?;
            need(q.day.is_some(), "day")?;
            need(q.base_amount.is_some(), "base_amount")?;
            need(q.ma_window.is_some(), "ma_window")?;
            t.insert("period".into(), q.period.clone().unwrap().into());
            t.insert("day".into(), (q.day.unwrap() as i64).into());
            t.insert("base_amount".into(), q.base_amount.unwrap().into());
            t.insert("ma_window".into(), (q.ma_window.unwrap() as i64).into());
            t.insert("k".into(), q.k.unwrap_or(1.0).into());
        }
        "trend" => {
            need(q.short_window.is_some(), "short_window")?;
            need(q.long_window.is_some(), "long_window")?;
            need(q.amount.is_some(), "amount")?;
            t.insert("short_window".into(), (q.short_window.unwrap() as i64).into());
            t.insert("long_window".into(), (q.long_window.unwrap() as i64).into());
            t.insert("amount".into(), q.amount.unwrap().into());
        }
        other => return Err(anyhow!("未知策略: {other}")),
    }

    let strategy = build_strategy_from(&q.strategy, &Some(toml::Value::Table(t)), &[])?;
    let fee = FeeModel { buy_rate: q.buy_rate, sell_tiers: standard_sell_tiers() };
    Ok(RunSpec {
        fund_code: q.fund_code.clone(),
        start: q.start,
        end: q.end,
        strategy,
        fee,
        initial_cash: q.initial_cash,
    })
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 5 个测试 PASS。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning；`unused mut` 之类清掉）
```bash
git add src/lib.rs src/web/mod.rs
git commit -m "feat: web::build_run_from_query 表单参数→回测装配"
```

---

## Task 3: axum 服务（路由 + handler + 页面）

**Files:**
- Modify: `Cargo.toml`（加依赖）
- Modify: `src/web/mod.rs`（加 server/handlers/AppError）
- Create: `src/web/page.rs`（INDEX_HTML）
- Test: `src/web/mod.rs`（`#[cfg(test)]` 路由测试）

**Interfaces:**
- Consumes: `build_run_from_query`（Task 2）、`render_report_html`（Task 1）、`Engine`/`cache::load_or_fetch` 等。
- Produces:
  - `pub fn router() -> axum::Router`
  - `pub async fn serve(port: u16) -> anyhow::Result<()>`
  - `src/web/page.rs`：`pub const INDEX_HTML: &str`

- [ ] **Step 1: 加依赖到 Cargo.toml**

在 `[dependencies]` 末尾加：

```toml
axum = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net"] }
```

在文件末尾（若无 `[dev-dependencies]` 则新建）加：

```toml
[dev-dependencies]
tower = { version = "0.5", features = ["util"] }
```

Run: `cargo build`（拉取依赖，确认编译）
Expected: 成功（暂无新代码用到它们）。

- [ ] **Step 2: 创建 page.rs**

创建 `src/web/page.rs`：

```rust
/// 本地回测界面首页：表单 + 结果 iframe（纯静态，无外部依赖）。
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
.card{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:18px;margin-bottom:16px;box-shadow:0 1px 4px rgba(0,0,0,.06)}
form{display:flex;flex-wrap:wrap;gap:12px 18px;align-items:flex-end}
.field{display:flex;flex-direction:column;gap:4px}
.field label{font-size:.8rem;color:#5a6a7a}
.field input,.field select{padding:7px 9px;border:1px solid #cfd6e0;border-radius:6px;font-size:.9rem}
button{padding:9px 22px;background:#c0392b;color:#fff;border:none;border-radius:6px;font-size:.95rem;cursor:pointer}
button:disabled{opacity:.5;cursor:wait}
.params{display:contents}
.params.hidden{display:none}
#result{width:100%;height:1400px;border:1px solid #e0e4ea;border-radius:10px;background:#fff}
.hint{color:#7f8c8d;font-size:.85rem;margin-top:8px}
</style>
</head>
<body>
<div class="wrap">
  <h1>xlh 基金回测</h1>
  <div class="card">
    <form id="f">
      <div class="field"><label>基金代码</label><input name="fund_code" value="161725"/></div>
      <div class="field"><label>起始日</label><input type="date" name="start" value="2020-01-01"/></div>
      <div class="field"><label>结束日</label><input type="date" name="end" value="2024-12-31"/></div>
      <div class="field"><label>策略</label>
        <select name="strategy" id="strategy">
          <option value="dca">普通定投</option>
          <option value="smart_dca" selected>智能定投</option>
          <option value="trend">均线择时</option>
        </select>
      </div>

      <div class="params" data-for="dca smart_dca">
        <div class="field"><label>周期</label><select name="period"><option value="monthly">月</option><option value="weekly">周</option></select></div>
        <div class="field"><label>定投日</label><input type="number" name="day" value="1"/></div>
        <div class="field"><label>每期金额</label><input type="number" name="base_amount" value="1000"/></div>
      </div>
      <div class="params" data-for="smart_dca">
        <div class="field"><label>均线窗口</label><input type="number" name="ma_window" value="250"/></div>
        <div class="field"><label>k 系数</label><input type="number" step="0.1" name="k" value="1.0"/></div>
      </div>
      <div class="params" data-for="trend">
        <div class="field"><label>短窗口</label><input type="number" name="short_window" value="20"/></div>
        <div class="field"><label>长窗口</label><input type="number" name="long_window" value="60"/></div>
        <div class="field"><label>每次金额</label><input type="number" name="amount" value="1000"/></div>
      </div>

      <div class="field"><label>买入费率</label><input type="number" step="0.0001" name="buy_rate" value="0.0015"/></div>
      <div class="field"><label>初始现金</label><input type="number" name="initial_cash" value="0"/></div>
      <button type="submit" id="run">运行</button>
    </form>
    <div class="hint">卖出费率固定为标准阶梯（7天内1.5% / 1年内0.5% / 满1年0%）。</div>
  </div>
  <iframe id="result" title="回测报告"></iframe>
</div>
<script>
var sel = document.getElementById('strategy');
function syncParams(){
  var s = sel.value;
  document.querySelectorAll('.params').forEach(function(g){
    var on = g.getAttribute('data-for').split(' ').indexOf(s) >= 0;
    g.classList.toggle('hidden', !on);
    g.querySelectorAll('input,select').forEach(function(el){ el.disabled = !on; });
  });
}
sel.addEventListener('change', syncParams);
syncParams();

document.getElementById('f').addEventListener('submit', function(e){
  e.preventDefault();
  var btn = document.getElementById('run');
  var fd = new FormData(e.target);
  var qs = new URLSearchParams();
  for (var pair of fd.entries()) { if (pair[1] !== '') qs.append(pair[0], pair[1]); }
  btn.disabled = true; btn.textContent = '运行中…';
  fetch('/api/run?' + qs.toString())
    .then(function(r){ return r.text(); })
    .then(function(html){ document.getElementById('result').srcdoc = html; })
    .catch(function(err){ document.getElementById('result').srcdoc = '<p style="color:#c0392b;padding:20px">请求失败: ' + err + '</p>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = '运行'; });
});
</script>
</body>
</html>
"##;
```

- [ ] **Step 3: 写失败的路由测试**

在 `src/web/mod.rs` 的 `mod tests` 内追加：

```rust
    #[tokio::test]
    async fn index_serves_form() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"fund_code\""), "应含基金代码输入");
        assert!(body.contains("id=\"result\""), "应含结果 iframe");
        assert!(body.contains("运行"), "应含运行按钮");
    }
```

- [ ] **Step 4: 运行测试确认失败**

Run: `cargo test --lib web::tests::index_serves_form`
Expected: 编译失败（`router` 未定义）。

- [ ] **Step 5: 实现 server/handlers/AppError + 注册 page 模块**

在 `src/web/mod.rs` 顶部模块声明处加：

```rust
pub mod page;
```

在 `src/web/mod.rs`（`build_run_from_query` 之后、`mod tests` 之前）加：

```rust
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use anyhow::Context;

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/run", get(run_handler))
}

pub async fn serve(port: u16) -> Result<()> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await
        .with_context(|| format!("绑定 {addr} 失败"))?;
    println!("回测界面已启动：http://{addr}  (Ctrl+C 退出)");
    axum::serve(listener, router()).await.context("服务运行失败")?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(page::INDEX_HTML)
}

async fn run_handler(Query(q): Query<RunQuery>) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || run_blocking(q))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

/// 同步跑回测并渲染报告。在 spawn_blocking 线程内执行，
/// 非 Send 的 Box<dyn Strategy> 在此创建并消费，不跨 await。
fn run_blocking(q: RunQuery) -> Result<String> {
    let spec = build_run_from_query(&q)?;
    let points = crate::data::cache::load_or_fetch(
        &spec.fund_code, std::path::Path::new(".cache"), spec.start, spec.end)
        .with_context(|| format!("加载净值失败: {}", spec.fund_code))?;
    let meta = crate::report::html::ReportMeta {
        fund_code: spec.fund_code.clone(),
        start: spec.start,
        end: spec.end,
        strategy: q.strategy.clone(),
        strategy_desc: q.strategy.clone(),
        initial_cash: spec.initial_cash,
    };
    let data = crate::data::InMemoryData::new(points);
    let broker = crate::broker::Broker::new(spec.fee);
    let portfolio = crate::portfolio::Portfolio::new(spec.initial_cash);
    let mut engine = crate::engine::Engine::new(data, spec.strategy, broker, portfolio);
    engine.run();
    Ok(crate::report::html::render_report_html(
        &meta, engine.portfolio(), engine.daily(), engine.trades()))
}

pub struct AppError(pub anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!(
            "<!doctype html><meta charset=\"utf-8\"><body style=\"font-family:sans-serif;padding:24px;color:#c0392b\"><h3>回测失败</h3><pre>{}</pre>",
            crate::report::html_escape(&self.0.to_string()));
        (StatusCode::BAD_REQUEST, Html(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self { AppError(e.into()) }
}
```

（`anyhow::{anyhow, Result}` 已在文件顶部 import；若 `Context` 重复 import 则合并。）

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: Task 2 的 5 个 + `index_serves_form` 全部 PASS。

- [ ] **Step 7: clippy + 全量测试 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add Cargo.toml Cargo.lock src/web/mod.rs src/web/page.rs
git commit -m "feat: axum 本地回测服务（表单页 + /api/run）"
```

---

## Task 4: CLI serve 子命令

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `xlh::web::serve`。

- [ ] **Step 1: 重构 main.rs 为子命令分流**

把 `src/main.rs` 顶部 `use clap::Parser;` 改为 `use clap::{Parser, Subcommand};`，`Cli` 结构与 `main` 改为：

```rust
#[derive(Parser)]
#[command(name = "xlh", about = "A股基金定投/择时回测")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// 配置文件路径（无子命令时使用）
    #[arg(short, long, default_value = "config.toml", global = true)]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动本地 Web 界面
    Serve {
        /// 监听端口
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Serve { port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(xlh::web::serve(port))?;
            Ok(())
        }
        None => run_cli(&cli.config),
    }
}
```

把原 `main` 函数体（从 `let cfg = config::load(...)` 到结尾的全部回测逻辑）整体移入新函数：

```rust
fn run_cli(config: &std::path::Path) -> Result<()> {
    let cfg = config::load(config)?;
    // …… 原有 main 体（optimize / compare / 单次 三分支）原样粘贴 ……
}
```

（只是把原 `main` 体搬进 `run_cli` 并把 `&cli.config` 换成参数 `config`；逻辑一字不改。）

- [ ] **Step 2: 构建 + 既有路径冒烟**

Run: `cargo build`
Expected: 成功。

Run: `cargo run --quiet -- --config config.toml 2>&1 | head -3`
Expected: 仍打印"加载 … 条净值"等（无子命令路径不变）。

- [ ] **Step 3: serve 启动冒烟（后台起、curl、杀进程）**

Run（bash）:
```bash
cargo run --quiet -- serve --port 18080 &
SVPID=$!
sleep 3
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:18080/
curl -s "http://127.0.0.1:18080/api/run?fund_code=161725&start=2020-01-01&end=2024-12-31&strategy=dca&period=monthly&day=1&base_amount=1000&buy_rate=0.0015&initial_cash=0" | grep -c "总收益"
kill $SVPID
```
Expected: 首行 `200`；第二行 `1`（报告含"总收益"）。

- [ ] **Step 4: 全量测试 + clippy + 提交**

Run: `cargo test` 然后 `cargo clippy --all-targets`
Expected: 全绿、无 warning。
```bash
git add src/main.rs
git commit -m "feat: xlh serve 子命令接入 Web 服务"
```

---

## Task 5: Playwright 端到端自测

**Files:**
- Create: `scripts/verify_web.py`

- [ ] **Step 1: 写 Playwright 自测脚本**

参照 `scripts/verify_compare.py` 思路创建 `scripts/verify_web.py`：

```python
#!/usr/bin/env python3
"""端到端：启动 xlh serve，浏览器填表单点运行，校验 iframe 内报告渲染。"""
import subprocess, sys, time, socket, urllib.request
from pathlib import Path
from playwright.sync_api import sync_playwright

PORT = 18081
SHOT = Path("output/web_screenshot.png").resolve()

def wait_port(port, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True
        except OSError:
            time.sleep(0.5)
    return False

def main():
    srv = subprocess.Popen(["cargo", "run", "--quiet", "--", "serve", "--port", str(PORT)])
    try:
        if not wait_port(PORT):
            print("FAIL: 服务未在超时内就绪")
            return 1
        errors = []
        with sync_playwright() as p:
            browser = p.chromium.launch()
            page = browser.new_page()
            page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
            page.goto(f"http://127.0.0.1:{PORT}/")
            page.wait_for_load_state("networkidle")
            page.click("#run")  # 用默认表单值运行
            # 等 iframe 内出现报告
            frame = page.frame_locator("#result")
            frame.locator("canvas").first.wait_for(timeout=60000)
            # 断言 iframe 内含“总收益”
            body_text = frame.locator("body").inner_text(timeout=10000)
            assert "总收益" in body_text, "报告应含 总收益"
            box = frame.locator("canvas").first.bounding_box()
            assert box and box["width"] > 0 and box["height"] > 0, "图表 canvas 面积应 > 0"
            page.screenshot(path=str(SHOT), full_page=True)
            browser.close()
        assert not errors, f"页面有 console error: {errors}"
        print(f"PASS: 表单运行成功，报告渲染，截图 {SHOT}")
        return 0
    finally:
        srv.terminate()
        try: srv.wait(timeout=10)
        except Exception: srv.kill()

if __name__ == "__main__":
    sys.exit(main())
```

注意：上面 `page.click («#run»)` 是占位排版符号，实现时写成 `page.click("#run")`（双引号，CSS 选择器）。

- [ ] **Step 2: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 `PASS: …`；`output/web_screenshot.png` 生成。

- [ ] **Step 3: 肉眼核对截图**

Read `output/web_screenshot.png`，确认：上方表单完整、下方 iframe 内是收益曲线 + 指标卡片的完整报告。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 4: 全量测试 + 提交**

Run: `cargo test`
Expected: 全绿。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 端到端自测 Web 界面"
```

交付报告：`xlh serve` 启动方式、截图、各断言结果。

---

## Self-Review

- **Spec 覆盖**：§3 CLI→T4；§4 模块结构（render_report_html→T1、web mod→T2/T3、page→T3、lib 注册→T2、依赖→T3）；§5 路由→T3；§6 build_run_from_query→T2；§7/§8 数据流与"直接用 Engine"→T3 `run_blocking`；§9 AppError→T3；§10 前端→T3 page.rs；§11 测试→T2(单元)/T3(路由)/T5(e2e)。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码。
- **类型一致**：`render_report_html(meta,&pf,&daily,&trades)->String`（T1）= T3 `run_blocking` 调用签名；`RunQuery`/`RunSpec`/`build_run_from_query`（T2）= T3 引用；`ReportMeta` 字段与 html.rs 实际一致；`FeeModel{buy_rate,sell_tiers}`、`SellTier{max_days,rate}` 与 broker.rs 一致；`Engine::new(data,strategy,broker,portfolio)` + `portfolio()/daily()/trades()` 与 engine.rs 一致。
- **Send 安全**：`Box<dyn Strategy>` 仅在 `spawn_blocking` 的 `run_blocking(q)` 内创建/消费，闭包只捕获 `RunQuery`（纯数据，Send），返回 `String`（Send）——不跨 await。
- **不破坏既有路径**：T1 仅抽函数、行为不变；T4 把原 main 体搬进 run_cli 不改逻辑；config/engine/runner 全不动。
- **YAGNI**：不做对比/寻优入界面、鉴权、持久化、自定义卖出阶梯。

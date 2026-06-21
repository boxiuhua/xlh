# Web 界面加对比/寻优 Tab —— 设计文档

- 日期：2026-06-19
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + Web 界面（单次回测，已合入 master）

## 1. 目标

在现有本地 Web 界面（`xlh serve`）上，把另外两种回测模式接进来：**对比**（一次跑多个命名策略，出叠加对比报告）与**寻优**（一个策略扫参数网格，出排名报告）。三种模式用顶部 Tab 切换，共用下方结果 iframe。

复用面最大：对比复用 `runner::run_one`，寻优复用 `optimize::run_optimize`，二者正好返回所需类型；各报告渲染只需抽一个返回 String 的版本（与已有 `render_report_html` 同模式）。引擎、对比/寻优算法、ECharts 报告全部不重写。

非目标（YAGNI）：保存/分享结果、per-run 独立区间、寻优扫描规则、并发多用户、鉴权。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 模式切换 | 顶部三 Tab（单次/对比/寻优），共用结果 iframe | 用户选定；三表单互不干扰 |
| 对比策略输入 | 动态增删策略行（名称+策略+参数+删除） | 用户选定；真正任意多策略 |
| 寻优网格输入 | 每参数一个逗号分隔值输入框 | 用户选定；表单友好、直接映射笛卡尔积 |
| 请求方式 | 对比/寻优用 POST + JSON body；单次保持 GET | 嵌套/列表数据用 JSON 干净，serde_urlencoded 不擅长嵌套 |
| 渲染复用 | 抽 `render_compare_html`/`render_optimize_html` -> String | DRY；CLI 写文件路径行为不变 |
| Send 安全 | `Box<dyn Strategy>` 在 spawn_blocking 闭包内建/用 | 沿用单次做法，不跨 await |
| 校验 | 复用 fund_code charset + buy_rate∈[0,1) + start<end | 与单次一致；防路径穿越 |

## 3. 前端（重写 src/web/page.rs INDEX_HTML）

顶部 Tab 条三项；点击切换 `.tab-panel` 显隐。三表单各自独立，共用下方 `<iframe id="result">`。

- **单次 tab**：搬现有单次表单（字段、随策略显隐参数组、buy_rate、initial_cash、运行）。
- **对比 tab**：
  - 公共字段：基金代码、起始日、结束日、买入费率、初始现金。
  - 策略行区 `#compare-rows`：每行 = 名称(text) + 策略(select) + 随策略显隐的参数输入 + 「删除」按钮 + 可选 per-row 基金代码（留空用公共基金）。
  - 「+ 添加策略」按钮 append 一行（用 `<template>` 克隆）；初始 2 行。
  - 「运行对比」按钮。
- **寻优 tab**：公共字段 + 策略(select) + 该策略各参数的**逗号分隔值**输入框（如 ma_window=`120,250,500`）+ 排序指标(select: total_return/annualized/sharpe/max_drawdown) + Top-N(number) + 「运行寻优」按钮。
- JS：
  - Tab 切换：点 tab → toggle active + 显隐 panel。
  - 对比提交：收集公共字段 + 遍历策略行拼 `runs: [{name, fund_code?, strategy, period?, day?, ...}]` → `fetch('/api/compare', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})` → `iframe.srcdoc = text`。
  - 寻优提交：收集公共字段 + strategy + metric + top_n + 各参数 CSV 字段 → JSON → `POST /api/optimize` → iframe。
  - 运行中禁用按钮、显示"运行中…"，失败把错误文本写入 iframe。
  - 纯静态，无外部依赖（图表在 iframe 内的报告里）。

## 4. 后端路由（src/web/mod.rs）

- 保留 `GET /api/run`（单次，不变）。
- 新增 `POST /api/compare` → `Json(CompareRequest)`。
- 新增 `POST /api/optimize` → `Json(OptimizeRequest)`。
- `router()` 注册两条新路由。

## 5. 请求结构

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

#[derive(Debug, Deserialize)]
pub struct CompareRunReq {
    pub name: String,
    #[serde(default)] pub fund_code: Option<String>, // 留空用公共基金
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
    /// 各参数的逗号分隔候选值字符串，键 = 参数名（period/day/base_amount/ma_window/k/short_window/long_window/amount）
    pub grid: std::collections::BTreeMap<String, String>,
}
fn default_top_n_web() -> usize { 5 }
```

## 6. 纯函数（可单测，不碰 IO）

```rust
/// 复用现有的"按策略拼 toml 参数表"逻辑，构建单个策略。
/// 与 build_run_from_query 内部相同的字段→参数表规则，抽成共享函数。
fn strategy_params_table(s: &StrategyFields) -> Result<toml::Table>;
fn build_strategy_from_fields(s: &StrategyFields) -> Result<Box<dyn Strategy>>; // 调 build_strategy_from

/// 把 "120,250,500" 拆成 [Integer(120),...]；每值试 i64→f64→String。空串/全空白报错。
pub fn parse_csv_values(s: &str) -> Result<Vec<toml::Value>>;

/// 由寻优请求构建 OptimizeCfg（grid: 各 CSV 字段 → toml 数组的 Table）。
/// 校验 metric 合法、grid 非空、各 CSV 非空。
pub fn build_optimize_cfg(req: &OptimizeRequest) -> Result<crate::config::OptimizeCfg>;
```

注：单次的 `build_run_from_query` 现有"拼参数表"逻辑抽出共享 `strategy_params_table`，单次/对比/寻优共用，避免三处重复（DRY）。fund_code 校验沿用现有 `^[0-9A-Za-z]{1,12}$` 等价 charset 检查，对比每个 run 的 fund（含 per-run 覆盖）都校验。

## 7. 数据流（均在 spawn_blocking 内，Send 安全）

**对比** `POST /api/compare`：
```
校验公共 fund_code/buy_rate/区间；runs 非空。
spawn_blocking(move || {
  for run in runs:
    fund = run.fund_code.unwrap_or(公共 fund)；校验 fund charset
    strategy = build_strategy_from_fields(&run.params)?
    points = cache::load_or_fetch(&fund, ".cache", start, end)?
    outcome = runner::run_one(run.name, fund, points, strategy, fee, initial_cash)
    收集 outcome
  render_compare_html(&CompareMeta{start,end}, &outcomes)
}) -> String
```

**寻优** `POST /api/optimize`：
```
cfg = build_optimize_cfg(req)?；校验 fund_code/buy_rate/区间
spawn_blocking(move || {
  points = cache::load_or_fetch(&fund, ".cache", start, end)?
  report = optimize::run_optimize(&cfg, &fund, &points, fee, initial_cash)?
  render_optimize_html(&OptMeta{start,end,fund_code:fund}, &report)
}) -> String
```

`fee = FeeModel{ buy_rate, sell_tiers: 标准阶梯 }`（与单次同一 `standard_sell_tiers()`）。

## 8. 渲染复用（各抽 String 版）

- `report::compare`：抽 `pub fn render_compare_html(meta: &CompareMeta, runs: &[RunOutcome]) -> String`（现 `render_compare` 内"payload→data_json→build_html"段）；`render_compare(...)` 改为 `create_dir_all` + 调它 + 写 `compare.html`。
- `report::optimize`：抽 `pub fn render_optimize_html(meta: &OptMeta, report: &OptReport) -> String`；`render_optimize(...)` 改为调它 + 写 `optimize.html`。
- 两处 `build_html`/Payload 结构不动；页面文案与既有测试断言不变。

## 9. 错误处理

复用 `AppError`（→ 400 + `html_escape` 后中文错误）。新增报错点：runs 为空、grid 为空、某 CSV 为空、metric 非法、per-run fund 非法——均带具体原因。`Json` 提取失败由 axum 返回 400（可接受）。

## 10. 测试

- 单元（web）：
  - `parse_csv_values`：`"120,250,500"`→3 个 Integer；`"0.5,1.0"`→2 个 Float；`"monthly"`→1 个 String；`""`/`" , "`→报错。
  - `build_optimize_cfg`：smart_dca 三参数 CSV → grid 维度数正确、metric 透传；空 grid/空 CSV/非法 metric 报错。
  - `build_strategy_from_fields`：三策略 ok；缺必填参数报错（透传 build_strategy_from）。
- 路由：`POST /api/compare`（body 两个 run，fund 161725 已缓存）经 axum oneshot → 200，body 含 `echarts` 与两个 run 名；`POST /api/optimize`（小网格）→ 200，body 含 `参数寻优`。离线可跑（用缓存）。
- Playwright e2e（扩展 scripts/verify_web.py）：切到对比 tab、加两个策略、点运行，断言 iframe 出现 canvas + 两条曲线区；切到寻优 tab、填网格、点运行，断言 iframe 出现排名表 + canvas。各截图。
- 既有 72 测试保持绿、clippy 干净。

## 11. 单元边界

- `strategy_params_table` / `build_strategy_from_fields` / `parse_csv_values` / `build_optimize_cfg`：纯函数，无 IO，独立可测。
- `compare_handler` / `optimize_handler`：编排（校验 → 纯函数 → spawn_blocking 跑引擎 → 渲染），薄。
- `render_compare_html` / `render_optimize_html`：纯渲染。
- `page.rs`：纯静态前端（三 Tab + 三表单 + JS）。
- 各单元经请求结构 / `OptimizeCfg` / `RunOutcome` / `OptReport` / String 通信，可分别理解与测试。

## 12. 对既有的影响

- `build_run_from_query`（单次）改为复用抽出的 `strategy_params_table`——行为不变，既有 web 单测仍过。
- `render_compare`/`render_optimize` 改为委托新 `*_html` 函数——CLI compare/optimize 路径行为不变，既有 report 测试仍过。
- page.rs 重写为多 Tab——单次表单逻辑保留在"单次"tab 内。

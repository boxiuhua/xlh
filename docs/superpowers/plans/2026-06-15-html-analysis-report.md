# HTML 分析报告 Implementation Plan

**Goal:** 让回测引擎生成自包含交互式 HTML 分析报告（report.html），含指标卡片、权益/回撤/净值图表与成交流水。

**Architecture:** 引擎捕获逐日记录与成交流水 → report/html.rs 计算指标并把数据 JSON 内嵌进 HTML 模板（ECharts CDN 渲染图表，文本/表格服务端渲染）→ CLI 按配置输出。

**Tech Stack:** Rust（serde/serde_json 已有）+ ECharts CDN + Playwright 自测。

依附既有代码：event.rs(Direction)、engine.rs、portfolio.rs(curve/flows/total_contributed)、metrics.rs、config.rs(ReportCfg)、main.rs。

---

## Task 1: 数据捕获类型与 Direction 序列化

**Files:** Create `src/result.rs`; Modify `src/event.rs`, `src/lib.rs`.

- [ ] Step 1: 在 `src/event.rs` 给 `Direction` 增加 serde 序列化：
  - 顶部 `use serde::Serialize;`
  - `#[derive(Debug, Clone, Copy, PartialEq, Serialize)]` 且 `#[serde(rename_all = "lowercase")]` 作用于 `enum Direction`。
- [ ] Step 2: 创建 `src/result.rs`：
```rust
use chrono::NaiveDate;
use serde::Serialize;
use crate::event::Direction;

#[derive(Debug, Clone, Serialize)]
pub struct DailyRecord {
    pub date: NaiveDate,
    pub nav: f64,
    pub adj_nav: f64,
    pub equity: f64,
    pub contribution: f64,
    pub shares: f64,
    pub cash: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeRecord {
    pub date: NaiveDate,
    pub direction: Direction,
    pub shares: f64,
    pub price: f64,
    pub fee: f64,
}
```
- [ ] Step 3: `src/lib.rs` 增加 `pub mod result;`。
- [ ] Step 4: 单测（在 result.rs）`direction_serializes_lowercase`：`serde_json::to_string(&Direction::Buy).unwrap() == "\"buy\""`，Sell 同理。运行 `cargo test --lib result` 与 `--lib event` 通过。
- [ ] Step 5: Commit：`feat: report data records and Direction serialization`。

---

## Task 2: 引擎捕获逐日记录与成交流水

**Files:** Modify `src/engine.rs`.

- [ ] Step 1: 在 `Engine` 结构体加字段 `daily: Vec<crate::result::DailyRecord>` 与 `trades: Vec<crate::result::TradeRecord>`；`new()` 初始化为空 `Vec::new()`。
- [ ] Step 2: 在 `run()` 的 `Event::Fill(f)` 分支，应用成交后追加流水（仅非零份额）：
```rust
Event::Fill(f) => {
    if f.shares > 1e-9 {
        self.trades.push(crate::result::TradeRecord {
            date: f.date, direction: f.direction, shares: f.shares, price: f.price, fee: f.fee,
        });
    }
    self.portfolio.apply_fill(&f);
}
```
- [ ] Step 3: 在每日 `record_equity(...)` 之后，追加逐日记录：
```rust
self.portfolio.record_equity(today.date, self.broker.total_shares(), today.adj_nav);
if let Some(p) = self.portfolio.curve.last() {
    self.daily.push(crate::result::DailyRecord {
        date: today.date,
        nav: today.nav,
        adj_nav: today.adj_nav,
        equity: p.equity,
        contribution: p.contribution,
        shares: self.broker.total_shares(),
        cash: self.portfolio.cash,
    });
}
```
- [ ] Step 4: 增加只读访问方法：
```rust
pub fn daily(&self) -> &[crate::result::DailyRecord] { &self.daily }
pub fn trades(&self) -> &[crate::result::TradeRecord] { &self.trades }
```
- [ ] Step 5: 新增引擎测试 `captures_daily_and_trades`：用现有 `dca_on_flat_then_up_market` 同款数据跑，断言 `engine.daily().len() == 3`，`engine.trades().len() == 2`（两次定投买入），且 `engine.daily().last().unwrap().equity ≈ 4000.0`，首条 trade.direction == Direction::Buy。注意：`run()` 借用 `&mut self` 返回 `&Portfolio`，测试里先 `engine.run();` 再分别调用 `engine.daily()`/`engine.trades()`（避免与 run 的借用冲突）。
- [ ] Step 6: `cargo test`（全量）通过。Commit：`feat: engine captures daily records and trade log`。

---

## Task 3: HTML 报告渲染器

**Files:** Create `src/report/html.rs`; Modify `src/report/mod.rs`.

- [ ] Step 1: `src/report/mod.rs` 增加 `pub mod html;`。
- [ ] Step 2: 创建 `src/report/html.rs`，含 `ReportMeta`、`render_report`，以及一个内部可序列化 `Payload`/`MetricsJson`/`MetaJson`。要点：
  - 用 `metrics::{total_return, max_drawdown, xirr, sharpe}` 计算指标；`final_equity = pf.curve.last().equity`；`trade_count = trades.len()`。
  - `Payload { meta, metrics, daily, trades }` 派生 `Serialize`，`serde_json::to_string(&payload)?` 得到 `data_json`。
  - HTML 模板用 `format!` 注入：标题/区间/策略摘要、服务端渲染的指标卡片（数值+正负色类）、服务端渲染的成交表行（遍历 trades）、`<script>const DATA = {data_json};</script>`、ECharts CDN `<script src="https://cdn.jsdelivr.net/npm/echarts@5/dist/echarts.min.js"></script>`、初始化脚本（三块图：权益+累计投入、回撤水下、净值双线+买卖 markPoint）。
  - 内联 `<style>`：浅色、卡片网格、响应式、盈利红/亏损绿。
  - ECharts 缺失保护：初始化脚本开头 `if (typeof echarts === 'undefined') { document.querySelectorAll('.chart').forEach(e=>e.innerHTML='图表库加载失败（需联网加载 ECharts）'); return; }`（包在一个 IIFE 内以便 return）。
  - 路径安全：`out_dir` 用 `std::fs::create_dir_all(out_dir)?`；输出文件 `out_dir.join("report.html")`；写入失败用 `?` 传播 `anyhow` 上下文。返回 `Ok(out_path)`。
- [ ] Step 3: 单测 `renders_self_contained_report`：构造 2~3 天 daily、1~2 条 trades、对应 pf（手填 curve/total_contributed/flows），渲染到 `std::env::temp_dir().join("xlh_html_test")`，断言：文件存在；内容含 `const DATA`；含 `meta.fund_code`；含 `总收益` 与 `最大回撤` 标签；含 `echarts`（脚本引用）；含一行成交（如含 `buy` 或方向中文）。清理临时目录。
- [ ] Step 4: `cargo test --lib report` 通过。Commit：`feat: self-contained HTML analysis report renderer`。

---

## Task 4: 配置与 CLI 接线 + 示例配置

**Files:** Modify `src/config.rs`, `src/main.rs`, `config.toml`.

- [ ] Step 1: `src/config.rs` 的 `ReportCfg` 增加 `#[serde(default)] pub html: bool`。
- [ ] Step 2: `src/main.rs`：在 `engine.run()` 之后（注意借用顺序：先 `let pf = engine.run();` 打印 summary 与 PNG 图；HTML 部分需要 `engine.daily()/trades()`，而 `pf` 是对 `engine` 内 portfolio 的不可变借用——为避免借用冲突，重排为：先用 `pf` 完成 summary 和 PNG，再在这些借用结束后单独处理 HTML：克隆所需 portfolio 数据或在 `pf` 作用域结束后调用 `engine.daily()`）。具体实现：
```rust
let pf = engine.run();
report::print_summary(pf);
if cfg.report.chart {
    report::chart::render_equity(pf, &cfg.report.out_dir)?;
    println!("图表已保存到 {}/equity.png", cfg.report.out_dir.display());
}
let want_html = cfg.report.html;
// pf 借用在此结束；下面重新借用 engine
if want_html {
    let meta = report::html::ReportMeta {
        fund_code: cfg.data.fund_code.clone(),
        start: cfg.data.start,
        end: cfg.data.end,
        strategy: cfg.strategy.kind.clone(),
        strategy_desc: format!("{} {}", cfg.strategy.kind, cfg.strategy.params.as_ref().map(|v| v.to_string()).unwrap_or_default()),
        initial_cash: cfg.portfolio.initial_cash,
    };
    let pf2 = /* 需要再次拿到 portfolio：见下 */;
    let path = report::html::render_report(&meta, pf2, engine.daily(), engine.trades(), &cfg.report.out_dir)?;
    println!("HTML 报告已生成：{}", path.display());
}
```
  借用细节修正：`engine.run()` 返回 `&Portfolio`，其生命周期借用 `engine`。要同时把 `pf`、`engine.daily()`、`engine.trades()` 传进 `render_report`，三者都是对 `engine` 的不可变借用，可同时存在。因此最简实现是：把整段（summary/PNG/HTML）都放在 `let pf = engine.run();` 之后，HTML 直接用 `pf`、`engine.daily()`、`engine.trades()`（全是 `&engine` 不可变借用，互不冲突）。**采用此最简方案**，删除上面的 `want_html`/`pf2` 绕行：
```rust
let pf = engine.run();
report::print_summary(pf);
if cfg.report.chart {
    report::chart::render_equity(pf, &cfg.report.out_dir)?;
    println!("图表已保存到 {}/equity.png", cfg.report.out_dir.display());
}
if cfg.report.html {
    let meta = report::html::ReportMeta {
        fund_code: cfg.data.fund_code.clone(),
        start: cfg.data.start,
        end: cfg.data.end,
        strategy: cfg.strategy.kind.clone(),
        strategy_desc: format!("{} {}", cfg.strategy.kind,
            cfg.strategy.params.as_ref().map(|v| v.to_string()).unwrap_or_default()),
        initial_cash: cfg.portfolio.initial_cash,
    };
    let path = report::html::render_report(&meta, pf, engine.daily(), engine.trades(), &cfg.report.out_dir)?;
    println!("HTML 报告已生成：{}", path.display());
}
```
  （`cfg.strategy.params` 为 `Option<toml::Value>`；`toml::Value` 实现 `Display`。）
- [ ] Step 3: `config.toml` 的 `[report]` 增加 `html = true`。
- [ ] Step 4: `cargo build` + `cargo test` 全绿。Commit：`feat: wire HTML report into CLI and config`。

---

## Task 5: 端到端自测（真实数据 + Playwright 渲染）

**Files:** 无源码改动（除非发现缺陷）。

- [ ] Step 1: 删除可能的过期缓存后跑真实回测生成报告：`rm -f .cache/161725.csv && cargo run -- --config config.toml`，确认打印 “HTML 报告已生成：…/report.html”，文件存在。
- [ ] Step 2: 用 webapp-testing（Playwright）打开 `file://.../output/report.html`：等待加载，收集 console 错误，断言无 error；断言三个 `.chart` 容器内均出现 `<canvas>`（ECharts 已渲染）；断言指标卡片含非空数值；截图保存到 `output/report_screenshot.png`。
- [ ] Step 3: 若渲染有报错或图表空白，按 systematic-debugging 定位修复（常见：ECharts CDN 未加载→确认联网/换 jsDelivr 备用 CDN；日期轴格式；markPoint 数据映射）。修复后重跑 Step 1-2。
- [ ] Step 4: 全量 `cargo test` 通过、Playwright 截图通过后，Commit（若有改动）。报告交付：列出 report.html 路径、截图路径、各项断言结果。

---

## Self-Review

- 覆盖：数据捕获(Task1-2)、渲染(Task3)、接线(Task4)、自测(Task5)。
- 借用陷阱已在 Task4 显式说明并给出最简正确写法（pf 与 engine.daily()/trades() 均为 &engine 不可变借用，可共存）。
- 不破坏既有接口：run()->&Portfolio 不变；既有 42 测试应继续通过。
- YAGNI：不做对比/服务器/在线调参。

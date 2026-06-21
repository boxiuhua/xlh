# 多基金/多策略对比 Implementation Plan

**Goal:** 一次运行多个命名回测并生成对比报告 compare.html（对比指标表 + 收益率叠加图 + 回撤叠加图）。

**Architecture:** 配置新增 `[[compare]]`；重构出 `build_strategy_from` 与 `metrics::summarize`；新增 `runner::run_one` 跑单个 run；新增 `report::compare::render_compare` 出对比页；CLI 检测到 compare 非空即进对比模式。

**Tech Stack:** Rust（serde/serde_json）+ ECharts + Playwright 自测。

依附：config.rs(build_strategy/build_rules/build_fee/validate)、metrics.rs、engine.rs(daily/trades/portfolio 访问器)、result.rs(DailyRecord)、report/html.rs(设计语言参考)。

---

## Task 1: 共享指标 metrics::Summary

**Files:** Modify `src/metrics.rs`, `src/report/html.rs`.

- [ ] Step 1: 在 `src/metrics.rs` 顶部 `use serde::Serialize;`，新增：
```rust
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total_contributed: f64,
    pub final_equity: f64,
    pub total_return: f64,
    pub annualized: f64,
    pub max_drawdown: f64,
    pub sharpe: f64,
    pub trade_count: usize,
}

/// 从组合与成交数汇总绩效指标。
pub fn summarize(pf: &crate::portfolio::Portfolio, trade_count: usize) -> Summary {
    let final_equity = pf.curve.last().map(|p| p.equity).unwrap_or(0.0);
    Summary {
        total_contributed: pf.total_contributed,
        final_equity,
        total_return: total_return(final_equity, pf.total_contributed),
        annualized: xirr(&pf.flows).unwrap_or(0.0),
        max_drawdown: max_drawdown(&pf.curve),
        sharpe: sharpe(&pf.curve, 0.0),
        trade_count,
    }
}
```
- [ ] Step 2: 测试（metrics.rs）`summarize_fields`：构造一个 pf（手填 curve 两点、total_contributed、flows），断言 `summarize(&pf, 3)` 的 total_return/max_drawdown/trade_count 数值正确。
- [ ] Step 3: 重构 `src/report/html.rs` 改用 `metrics::summarize`：把其内部逐项指标计算替换为 `let s = crate::metrics::summarize(pf, trades.len());`，卡片渲染改读 `s.total_return` 等字段。**保持页面中文标签与既有 html 测试断言不变**（`总收益`/`最大回撤`/`echarts`/`const DATA` 仍在）。若 html.rs 原有一个 `MetricsJson` 结构，直接用 `metrics::Summary` 替换并放进 Payload。
- [ ] Step 4: `cargo test`（全量）通过（既有 48 + 1 新）。Commit：`refactor: shared metrics::Summary used by html report`。

---

## Task 2: 配置 build_strategy_from + CompareRun

**Files:** Modify `src/config.rs`.

- [ ] Step 1: 新增 `build_rules_from`，并把 `build_rules` 改为委托：
```rust
fn build_rules_from(rules: &[RuleCfg]) -> Result<Vec<Rule>> {
    rules.iter().map(|r| match r.kind.as_str() {
        "take_profit" => Ok(Rule::TakeProfit { target_return: r.target_return }),
        "stop_loss" => Ok(Rule::StopLoss { max_drawdown: r.max_drawdown }),
        other => Err(anyhow!("未知规则: {other}")),
    }).collect()
}
fn build_rules(cfg: &Config) -> Result<Vec<Rule>> { build_rules_from(&cfg.rules) }
```
- [ ] Step 2: 抽出 `build_strategy_from`，`build_strategy` 委托它：
```rust
pub fn build_strategy_from(
    kind: &str,
    params: &Option<toml::Value>,
    rules: &[RuleCfg],
) -> Result<Box<dyn Strategy>> {
    let params = params.clone().unwrap_or(toml::Value::Table(toml::Table::new()));
    let base: Box<dyn Strategy> = match kind {
        "dca" => { let p: DcaParams = params.try_into()?;
            Box::new(Dca::new(parse_period(&p.period)?, p.day, p.base_amount)) }
        "smart_dca" => { let p: SmartDcaParams = params.try_into()?;
            if p.ma_window < 1 { return Err(anyhow!("配置错误: smart_dca.ma_window 必须 >= 1，当前值: {}", p.ma_window)); }
            Box::new(SmartDca::new(parse_period(&p.period)?, p.day, p.base_amount, p.ma_window, p.k)) }
        "trend" => { let p: TrendParams = params.try_into()?;
            if p.short_window < 1 { return Err(anyhow!("配置错误: trend.short_window 必须 >= 1，当前值: {}", p.short_window)); }
            if p.short_window >= p.long_window { return Err(anyhow!("配置错误: trend.short_window ({}) 必须小于 long_window ({})", p.short_window, p.long_window)); }
            Box::new(Trend::new(p.short_window, p.long_window, p.amount)) }
        other => return Err(anyhow!("未知策略: {other}")),
    };
    let rules = build_rules_from(rules)?;
    if rules.is_empty() { Ok(base) } else { Ok(Box::new(RuleLayer::new(base, rules))) }
}

pub fn build_strategy(cfg: &Config) -> Result<Box<dyn Strategy>> {
    build_strategy_from(&cfg.strategy.kind, &cfg.strategy.params, &cfg.rules)
}
```
- [ ] Step 3: 新增 `CompareRun` 并加到 `Config`：
```rust
#[derive(Debug, Deserialize)]
pub struct CompareRun {
    pub name: String,
    #[serde(default)] pub fund_code: Option<String>,
    pub strategy: StrategyCfg,
    #[serde(default)] pub rules: Vec<RuleCfg>,
    #[serde(default)] pub initial_cash: f64,
}
```
在 `Config` 结构体加 `#[serde(default)] pub compare: Vec<CompareRun>,`。
- [ ] Step 4: 测试：新增 `parses_compare_runs`（一个含 2 个 `[[compare]]` 的 TOML，断言 `cfg.compare.len()==2`、名称与 kind）；`build_strategy_from_dca_ok`（直接调 `build_strategy_from("dca", &Some(params_value), &[])` 成功）。既有 config 测试不变。
- [ ] Step 5: `cargo test --lib config` 通过。Commit：`feat: build_strategy_from and [[compare]] config`。

---

## Task 3: 运行器 runner::run_one

**Files:** Create `src/runner.rs`; Modify `src/lib.rs`.

- [ ] Step 1: `src/lib.rs` 加 `pub mod runner;`。
- [ ] Step 2: 创建 `src/runner.rs`：
```rust
use crate::broker::{Broker, FeeModel};
use crate::data::{InMemoryData, NavPoint};
use crate::engine::Engine;
use crate::metrics::{self, Summary};
use crate::portfolio::Portfolio;
use crate::result::DailyRecord;
use crate::strategy::Strategy;

pub struct RunOutcome {
    pub name: String,
    pub fund_code: String,
    pub summary: Summary,
    pub daily: Vec<DailyRecord>,
}

/// 跑单个命名回测：装配引擎→run→汇总指标。points 已按区间过滤、排序。
pub fn run_one(
    name: String,
    fund_code: String,
    points: Vec<NavPoint>,
    strategy: Box<dyn Strategy>,
    fee: FeeModel,
    initial_cash: f64,
) -> RunOutcome {
    let data = InMemoryData::new(points);
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let daily = engine.daily().to_vec();
    RunOutcome { name, fund_code, summary, daily }
}
```
  （`engine.portfolio()`、`engine.daily()`、`engine.trades()` 均为 Task(前序) 已有的 `&self` 访问器；`DailyRecord` 需 `Clone`——已派生。）
- [ ] Step 3: 测试 `run_one_dca_flat_then_up`：用引擎黄金用例同款 3 点数据 + `Dca::new(Monthly,1,1000)` + 无费 + initial_cash 0，断言 `outcome.daily.len()==3`、`outcome.summary.final_equity≈4000`、`outcome.summary.total_contributed≈2000`、`outcome.summary.trade_count==2`。
- [ ] Step 4: `cargo test --lib runner` 通过。Commit：`feat: runner::run_one for a single named backtest`。

---

## Task 4: 对比报告 report::compare

**Files:** Create `src/report/compare.rs`; Modify `src/report/mod.rs`.

- [ ] Step 1: `src/report/mod.rs` 加 `pub mod compare;`。
- [ ] Step 2: 创建 `src/report/compare.rs`，含：
```rust
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Serialize;
use crate::runner::RunOutcome;

pub struct CompareMeta { pub start: NaiveDate, pub end: NaiveDate }

#[derive(Serialize)]
struct RunJson<'a> { name: String, fund_code: String, summary: &'a crate::metrics::Summary, daily: &'a [crate::result::DailyRecord] }
#[derive(Serialize)]
struct Payload<'a> { start: String, end: String, runs: Vec<RunJson<'a>> }

fn html_escape(s: &str) -> String {
    s.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;").replace('"',"&quot;").replace('\'',"&#39;")
}

pub fn render_compare(meta: &CompareMeta, runs: &[RunOutcome], out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir).with_context(|| format!("创建输出目录失败: {}", out_dir.display()))?;
    let payload = Payload {
        start: meta.start.to_string(),
        end: meta.end.to_string(),
        runs: runs.iter().map(|r| RunJson { name: r.name.clone(), fund_code: r.fund_code.clone(), summary: &r.summary, daily: &r.daily }).collect(),
    };
    let data_json = serde_json::to_string(&payload)?.replace("</", "<\\/");
    let html = build_html(meta, runs, &data_json);
    let out = out_dir.join("compare.html");
    let path_str = out.to_str().context("输出路径含非法字符")?;
    std::fs::write(path_str, html).with_context(|| format!("写入 {path_str} 失败"))?;
    Ok(out)
}
```
  `build_html(meta, runs, data_json)` 生成完整 HTML：
  - 顶部标题「策略对比」、区间、`共 N 个策略`。
  - 对比表：表头 `策略 | 基金 | 总收益 | 年化 | 最大回撤 | 夏普 | 期末市值 | 累计投入 | 交易次数`；每行一个 run（name 与 fund_code 经 `html_escape`，数值格式化，收益/年化按正负红绿）；服务端计算各列最优（总收益/年化/夏普最大、最大回撤最小）并给该单元格加 `best` class 高亮。
  - 两块图（ECharts，从 `DATA` 构建）：
    - 收益率对比：每 run 一条 series，数据 `daily` 上 JS 计算 `cum=running sum(contribution); ret=equity/cum-1`（cum>0 才计），x=date，legend、dataZoom、tooltip。
    - 回撤对比：每 run 一条，JS 由 equity 跑动峰值算 `equity/peak-1`。
  - echarts 缺失保护（IIFE 内 `if(typeof echarts==='undefined'){...return;}`）。
  - 内联 `<style>`，沿用单次报告设计语言（卡片/浅色/红涨绿跌/响应式）。
- [ ] Step 3: 测试 `renders_compare_with_two_runs`：构造 2 个 `RunOutcome`（各 2~3 天 daily + summary），渲染到临时目录，断言：文件存在；含两个 run 名称；含 `const DATA`；含 `总收益`/`最大回撤`；含 `echarts`；含 `<table`。清理。
- [ ] Step 4: `cargo test --lib report` 通过。Commit：`feat: comparison HTML report renderer`。

---

## Task 5: CLI 接线 + 示例 compare.toml

**Files:** Modify `src/main.rs`; Create `compare.toml`.

- [ ] Step 1: `src/main.rs`：在 `let cfg = config::load(...)?;` 之后，优先处理对比模式：
```rust
if !cfg.compare.is_empty() {
    let mut runs = Vec::new();
    for run in &cfg.compare {
        let fund = run.fund_code.clone().unwrap_or_else(|| cfg.data.fund_code.clone());
        let points = cache::load_or_fetch(&fund, &cfg.data.cache_dir, cfg.data.start, cfg.data.end)
            .map_err(|e| anyhow::anyhow!("run [{}] 加载 {} 失败: {e}", run.name, fund))?;
        let strategy = config::build_strategy_from(&run.strategy.kind, &run.strategy.params, &run.rules)
            .map_err(|e| anyhow::anyhow!("run [{}] 构建策略失败: {e}", run.name))?;
        let fee = config::build_fee(&cfg);
        let outcome = xlh::runner::run_one(run.name.clone(), fund, points, strategy, fee, run.initial_cash);
        println!("✓ {}  总收益 {:.2}%  夏普 {:.2}", outcome.name, outcome.summary.total_return*100.0, outcome.summary.sharpe);
        runs.push(outcome);
    }
    let meta = xlh::report::compare::CompareMeta { start: cfg.data.start, end: cfg.data.end };
    let path = xlh::report::compare::render_compare(&meta, &runs, &cfg.report.out_dir)?;
    println!("对比报告已生成：{}", path.display());
    return Ok(());
}
// ……以下为既有单次流程（不变）
```
  （`StrategyCfg.params` 字段为 `pub`？当前 `StrategyCfg{ pub kind, pub params }` —— 确认 `params` 可见；若 `params` 非 pub，将其改 pub。同理 `CompareRun` 各字段 pub。）
- [ ] Step 2: 创建 `compare.toml`（按设计文档第 3 节的示例：3 个 run —— 普通定投/智能定投/均线择时，同一基金 161725，区间 2020-2024，`[report] out_dir="output"`）。
- [ ] Step 3: `cargo build` + `cargo test` 全绿、clippy 干净。Commit：`feat: wire comparison mode into CLI with example compare.toml`。

---

## Task 6: 端到端自测（真实数据 + Playwright）

- [ ] Step 1: `cargo run -- --config compare.toml`，确认打印每个 run 的小结与「对比报告已生成：…/compare.html」，文件存在。
- [ ] Step 2: 复用/改写 `scripts/verify_report.py` 思路新增 `scripts/verify_compare.py`：`file://` 打开 compare.html，等 networkidle，断言：无 console error；两个 `.chart` 容器各含 `<canvas>`；对比表 `tbody tr` 行数 == run 数（3）；图表 canvas 像素面积>0；截图存 `output/compare_screenshot.png`。
- [ ] Step 3: 查看截图肉眼核对（曲线有多条、图例为 run 名、表格高亮最优列）。若异常按 systematic-debugging 修复后重跑。
- [ ] Step 4: 全量 `cargo test` 通过 + Playwright 通过后 Commit（脚本）。交付报告：compare.html 路径、截图、各断言结果、3 个策略的对比数字。

---

## Self-Review

- 覆盖：指标复用(T1)、配置/策略构建(T2)、运行器(T3)、对比渲染(T4)、CLI(T5)、自测(T6)。
- 不破坏单次路径：build_strategy/build_strategy 委托新函数，html.rs 仅换指标来源，既有 48 测试应通过。
- 借用：runner 内 engine.run() 后再读 portfolio()/daily()/trades()，与 HTML 任务同样的安全顺序。
- 可比性口径明确（收益率%，非原始权益）。
- YAGNI：不做寻优/滚动窗口/在线筛选。

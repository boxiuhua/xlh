# 多基金/多策略对比 —— 设计文档

- 日期：2026-06-15
- 状态：已定稿（用户授权自主交付，免审批）
- 依附：xlh 回测引擎 + HTML 报告（feat/backtest-engine 分支）

## 1. 目标

一次运行多个「命名回测」（每个 = 基金 + 策略 + 参数，共享日期区间），生成一份对比报告 `output/compare.html`，并排比较收益、回撤、夏普等。可用于：同一基金不同策略对比，或同一策略不同基金对比。

非目标（YAGNI）：参数自动寻优、滚动窗口、>20 个 run 的性能优化、在线交互筛选。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 配置形态 | TOML 顶层 `[[compare]]` 数组，每项一个命名 run，共享 `[data]` 区间与 `[fees]` | 复用现有结构，单次回测路径不变 |
| 触发 | `compare` 非空即进入对比模式，生成 compare.html（跳过单次报告） | 一个 CLI 入口，零新子命令 |
| 可比口径 | 叠加「相对累计投入的收益率%」曲线：`ret_t = equity_t / cum_contrib_t - 1` | 不同策略投入额/时点不同，用收益率归一才公平 |
| 指标复用 | 抽出共享 `metrics::Summary` + `summarize(pf, trade_count)` | DRY；单次报告与对比报告共用 |
| 自测 | Rust 单测 + Playwright | 数据与渲染都验证 |

## 3. 配置（新增，单独示例 compare.toml，不动 config.toml）

```toml
[data]
start = "2020-01-01"
end   = "2024-12-31"
cache_dir = ".cache"
fund_code = "161725"   # 默认基金（run 未指定时用）

[fees]                 # 所有 run 共享
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7,   rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0,   rate = 0.000 },
]

# 单次回测仍需 [strategy]（对比模式下忽略，但结构要求存在）——
# 为简洁，对比模式下 [strategy] 字段仍需可解析；用一个占位 dca 即可。
[strategy]
kind = "dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0

[report]
chart = false
html = false

[portfolio]
initial_cash = 0.0

[[compare]]
name = "白酒·普通定投"
fund_code = "161725"
[compare.strategy]
kind = "dca"
[compare.strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0

[[compare]]
name = "白酒·智能定投"
fund_code = "161725"
[compare.strategy]
kind = "smart_dca"
[compare.strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
ma_window = 250
k = 1.0

[[compare]]
name = "白酒·均线择时"
fund_code = "161725"
[compare.strategy]
kind = "trend"
[compare.strategy.params]
short_window = 20
long_window = 60
amount = 12000.0
```

`CompareRun` 结构：
```rust
#[derive(Debug, Deserialize)]
pub struct CompareRun {
    pub name: String,
    #[serde(default)] pub fund_code: Option<String>, // 缺省用 data.fund_code
    pub strategy: StrategyCfg,                        // 复用既有
    #[serde(default)] pub rules: Vec<RuleCfg>,        // 可选止盈止损
    #[serde(default)] pub initial_cash: f64,
}
```
`Config` 增加 `#[serde(default)] pub compare: Vec<CompareRun>`。

## 4. 重构（最小、不破坏单次路径）

1. `config.rs`：把 `build_strategy(cfg)` 内部逻辑抽到
   `pub fn build_strategy_from(kind: &str, params: &Option<toml::Value>, rules: &[RuleCfg]) -> Result<Box<dyn Strategy>>`；
   原 `build_strategy(cfg)` 改为调用它（行为不变，既有测试通过）。同样的 trend/smart_dca 参数校验移入 `build_strategy_from`。
2. `metrics.rs`：新增
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
   pub fn summarize(pf: &crate::portfolio::Portfolio, trade_count: usize) -> Summary;
   ```
   `report/html.rs` 改用 `metrics::summarize`（替换其私有指标计算；页面标签与既有测试断言不变）。

## 5. 运行器（新增 src/runner.rs）

```rust
pub struct RunOutcome {
    pub name: String,
    pub fund_code: String,
    pub summary: metrics::Summary,
    pub daily: Vec<crate::result::DailyRecord>, // 含 equity 与每日 contribution
}

/// 跑单个 run：装配引擎→run→汇总。points 已按区间过滤。
pub fn run_one(
    name: String,
    fund_code: String,
    points: Vec<crate::data::NavPoint>,
    strategy: Box<dyn Strategy>,
    fee: FeeModel,
    initial_cash: f64,
) -> RunOutcome;
```

## 6. 对比报告（新增 src/report/compare.rs）

```rust
pub struct CompareMeta { pub start: NaiveDate, pub end: NaiveDate }
pub fn render_compare(meta: &CompareMeta, runs: &[RunOutcome], out_dir: &Path) -> Result<PathBuf>;
```
内容：
- 顶部：标题「策略对比」、区间、参与 run 数。
- 对比指标表：行=run（名称+基金），列=总收益/年化/最大回撤/夏普/期末市值/累计投入/交易次数；每列最优值加粗高亮（收益/年化/夏普取最大，回撤取最小）。
- 收益率叠加图（ECharts）：每个 run 一条「收益率%」曲线（JS 由 daily 的 equity 与 cumsum(contribution) 算 `equity/cumContrib-1`），共用 x=日期、dataZoom、legend 可点选隐藏。
- 回撤叠加图：每个 run 一条由 equity 跑动峰值算的回撤%曲线。
- 健壮性：echarts 缺失时图区显示提示；表格不依赖 JS。
- 数据以 `const DATA = {json};` 内嵌（含 `</` 转义）；run.name 等字符串 HTML 转义。
- 样式复用单次报告的设计语言（卡片/浅色/红涨绿跌）。

## 7. CLI

`main.rs`：load cfg 后，若 `!cfg.compare.is_empty()`：
- 对每个 run：`fund = run.fund_code.unwrap_or(data.fund_code)`；`points = cache::load_or_fetch(fund, cache_dir, start, end)`；`strategy = build_strategy_from(run.strategy.kind, &run.strategy.params, &run.rules)`；`fee = build_fee(cfg)`；`run_one(...)` 收集。
- `render_compare(&meta, &runs, &cfg.report.out_dir)` → 打印 compare.html 路径。
- 否则走既有单次流程（summary/PNG/HTML）。

## 8. 错误处理

任一 run 数据加载/构建失败：返回带 run 名称上下文的 anyhow 错误并终止（明确报错优于静默跳过）。

## 9. 测试

- `config`：解析含 `[[compare]]` 的 TOML，断言 compare.len()、各 run 的 name/kind；`build_strategy_from` 对三类策略可用。
- `metrics`：`summarize` 在已知 pf 上各字段数值正确。
- `runner`：`run_one` 在固定净值+dca 上返回 daily.len()/summary.final_equity 正确。
- `report/compare`：`render_compare` 在 2 个 run 上渲染，断言文件存在、含两个 run 名称、含 `const DATA`、含中文指标标签、含 echarts。
- Playwright：用 compare.toml 真实生成 compare.html，验证两条曲线渲染（图表 canvas）、表格行数=run 数、无 console error、截图。
- 既有 48 测试保持全绿。

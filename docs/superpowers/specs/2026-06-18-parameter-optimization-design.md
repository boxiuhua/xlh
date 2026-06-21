# 参数寻优（Grid Search）—— 设计文档

- 日期：2026-06-18
- 状态：已定稿（用户授权：「全部按照推荐执行」，免逐节审批）
- 依附：xlh 回测引擎 + HTML 报告 + 多策略对比（已合入 master）

## 1. 目标

对**单一基金、单一策略种类**，在一组参数取值上做笛卡尔积，批量回测每个参数组合，按指定指标排序，生成寻优报告 `output/optimize.html`（全组合排名表 + Top-N 收益率叠图）。用于回答「该策略哪组参数在此区间表现最好」。

复用面：grid 展开是唯一的新逻辑；每个组合的回测复用现有 `config::build_strategy_from` + `runner::run_one`，引擎/runner/portfolio 零改动。

非目标（YAGNI）：跨基金寻优、规则参数扫描、并行（rayon）、滚动窗口稳健性、贝叶斯/遗传等智能搜索、过拟合惩罚。仅做朴素 grid + 排序。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 网格语法 | `[optimize.grid]` 下每个参数给一个值数组，取笛卡尔积 | 最直观，可混整数/浮点；解析最简（已选定） |
| 触发 | TOML 出现 `[optimize]` 即进寻优模式（与 compare 同级，互斥优先级见 §7） | 一个 CLI 入口，零新子命令 |
| 排序指标 | `[optimize].metric` ∈ `total_return`/`annualized`/`sharpe`/`max_drawdown`；前三越大越优，`max_drawdown` 越小越优 | 复用 `metrics::Summary` 字段；方向由 metric 名自动决定 |
| 规则 | `[optimize].rules`（可选）为所有组合共用，**不参与扫描** | 扫规则会让组合爆炸；首版固定 |
| Top-N | `[optimize].top_n`（默认 5）控制叠图曲线数；排名表始终全量 | 图例过多不可读，表格无此问题 |
| 组合上限 | 无硬上限；展开后 `> 200` 打印警告但继续 | 不静默截断；用户知情即可 |
| 输出 | 复用 compare 的渲染设计语言，新出 `optimize.html` | 视觉一致，红涨绿跌/卡片沿用 |
| 自测 | Rust 单测 + Playwright | 网格展开、排序、渲染都验证 |

## 3. 配置（新增，单独示例 optimize.toml，不动既有配置）

```toml
[data]
start = "2020-01-01"
end   = "2024-12-31"
cache_dir = ".cache"
fund_code = "161725"

[fees]
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7,   rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0,   rate = 0.000 },
]

# 单次路径仍要求 [strategy] 可解析（寻优模式下忽略其内容，占位即可）
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

[optimize]
strategy = "smart_dca"   # 被扫描的策略种类
metric   = "sharpe"      # total_return | annualized | sharpe | max_drawdown
top_n    = 5             # 叠图取前 N（默认 5）

[optimize.grid]          # 每个参数一个值数组，取笛卡尔积
period      = ["monthly"]
day         = [1]
base_amount = [1000.0]
ma_window   = [120, 250, 500]
k           = [0.5, 1.0, 1.5]
# → 1×1×1×3×3 = 9 组合

# 可选：所有组合共用的规则（不扫描）
# [[optimize.rules]]
# kind = "take_profit"
# target_return = 0.3
```

`OptimizeCfg` 结构（加入 `Config`，`#[serde(default)] pub optimize: Option<OptimizeCfg>`）：
```rust
#[derive(Debug, Deserialize)]
pub struct OptimizeCfg {
    pub strategy: String,
    pub metric: String,
    #[serde(default = "default_top_n")] pub top_n: usize, // 默认 5
    pub grid: toml::Table,                                // 参数名 -> 值数组
    #[serde(default)] pub rules: Vec<RuleCfg>,
}
fn default_top_n() -> usize { 5 }
```

## 4. 网格展开（新增 src/optimize.rs 内的纯函数）

```rust
/// 把 {参数名 -> 值数组} 的 table 展开成每个组合一个 toml::Value::Table。
/// - 每个 value 必须是数组（非数组报错，指明参数名）。
/// - 空数组报错（该维度无取值）。
/// - 笛卡尔积顺序：按 grid 的键序稳定展开。
/// - 单值参数写成单元素数组（如 period = ["monthly"]）。
pub fn expand_grid(grid: &toml::Table) -> Result<Vec<toml::Value>>;
```
返回的每个 `toml::Value::Table` 即可直接作为 `build_strategy_from(strategy, &Some(combo), &rules)` 的 params。

校验：metric 非法名 → 报错列出合法集合；grid 为空 → 报错；任一维度空数组 → 报错。

## 5. 寻优执行（src/optimize.rs）

```rust
pub struct OptOutcome {
    pub params: toml::Value,                 // 该组合参数（用于报告参数列）
    pub label: String,                       // 由参数键值拼成的紧凑标签，如 "ma_window=250,k=1.0"
    pub outcome: crate::runner::RunOutcome,  // 复用：summary + daily
}

pub struct OptReport {
    pub strategy: String,
    pub metric: String,
    pub top_n: usize,
    pub ranked: Vec<OptOutcome>,             // 已按 metric 排好序（最优在前）
    pub param_keys: Vec<String>,             // grid 的参数键序（表格列头用）
}

/// 跑全部组合并排序。points 已按区间过滤、排序（由 CLI 传入，避免重复抓取）。
pub fn run_optimize(
    cfg: &OptimizeCfg,
    fund_code: &str,
    points: &[crate::data::NavPoint],
    fee: crate::broker::FeeModel,
    initial_cash: f64,
) -> Result<OptReport>;
```
实现：`expand_grid` → 对每个 combo `build_strategy_from(cfg.strategy, &Some(combo), &cfg.rules)`（失败带组合标签上下文报错）→ `runner::run_one`（每组合克隆 `points`）→ 收集 → 按 metric 排序（方向由 metric 决定）。`label` 仅由 grid 中**取值多于一个**的参数拼成（固定维度不入标签，保持简洁）；若全固定则用序号。

## 6. 寻优报告（新增 src/report/optimize.rs）

```rust
pub struct OptMeta { pub start: NaiveDate, pub end: NaiveDate, pub fund_code: String }
pub fn render_optimize(meta: &OptMeta, report: &OptReport, out_dir: &Path) -> Result<PathBuf>;
```
内容：
- 顶部：标题「参数寻优」、基金、区间、被扫策略、排序指标、组合总数、Top-N。
- 排名表：列 = 排名 | 各 grid 参数（`param_keys` 顺序）| 总收益 | 年化 | 最大回撤 | 夏普 | 期末市值 | 交易次数；行按 metric 排序，**排序所依指标列整列高亮**，第 1 名行加 `best` class。数值红涨绿跌、HTML 转义。
- 收益率叠加图（ECharts）：仅 Top-N 组合各一条「收益率%」曲线，JS 由 daily 的 equity 与 cumsum(contribution) 算 `equity/cumContrib-1`；legend 用 `label`；共用 x=日期、dataZoom。
- 健壮性：echarts 缺失时图区提示；表格不依赖 JS。数据 `const DATA = {json};`（含 `</` 转义）。
- 复用 compare/html 的样式与 `html_escape`（抽到可共享处或各自保留一份——沿用现状，避免过度重构）。

## 7. CLI（main.rs）

load cfg 后，按优先级：
1. 若 `cfg.optimize.is_some()`：进寻优模式。
   - `fund = cfg.data.fund_code`；`points = cache::load_or_fetch(fund, cache_dir, start, end)`；`fee = build_fee(cfg)`；`initial_cash = cfg.portfolio.initial_cash`。
   - `report = optimize::run_optimize(opt, &fund, &points, fee, initial_cash)?`。
   - 打印 Top-N 小结（排名、label、metric 值）。
   - `render_optimize(&meta, &report, &cfg.report.out_dir)` → 打印 optimize.html 路径；`return`。
2. 否则若 `!cfg.compare.is_empty()`：走既有对比模式（不变）。
3. 否则走既有单次流程（不变）。

`optimize` 与 `compare` 同时存在时：寻优优先，并 `eprintln!` 提示忽略了 compare。

## 8. 错误处理

- 网格/指标校验失败：anyhow 错误，指明问题参数/合法取值。
- 任一组合构建或回测异常：带组合 `label` 上下文报错并终止（明确报错优于静默跳过）。
- 组合数 > 200：`eprintln!` 警告组合数与预计耗时提示，继续执行。

## 9. 测试

- `config`：解析含 `[optimize]` + `[optimize.grid]` 的 TOML，断言 strategy/metric/top_n/grid 维度数；缺省 top_n=5。
- `optimize::expand_grid`：`{a:[1,2], b:["x"]}` → 2 组合且键值正确；非数组值报错；空数组报错。
- `optimize::run_optimize`：固定净值 + smart_dca 小网格（如 ma_window=[1,2]），断言 `ranked.len()==2`、已按 metric 排序（最优在前）、param_keys 正确；`max_drawdown` 指标下顺序方向相反。
- `report::optimize::render_optimize`：2~3 个 OptOutcome 渲染，断言文件存在、含「参数寻优」「排名」、含各参数列名、含 `const DATA`、含 echarts、含 `<table`。
- Playwright（scripts/verify_optimize.py）：用 optimize.toml 真实生成 optimize.html，验证排名表行数 = 组合数、叠图 canvas 像素>0、无 console error、截图。
- 既有 53 测试保持全绿、clippy 干净。

## 10. 单元边界

- `optimize::expand_grid`：纯函数，输入 table 输出组合列表，独立可测，不碰 IO/引擎。
- `optimize::run_optimize`：编排（展开→构建→跑→排序），依赖 config/runner/metrics，不碰渲染与 CLI。
- `report::optimize::render_optimize`：纯渲染，输入 OptReport 输出 HTML 文件，不碰执行。
- `main.rs`：仅装配与分流。
四者经 `OptReport` / `OptOutcome` 等数据结构通信，可分别理解与测试。

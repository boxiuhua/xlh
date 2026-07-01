# 选股/推荐设计（子项目 4）

> 全局背景见 [子项目1 spec](2026-07-01-stock-data-layer-design.md)。股票代码收敛于 `src/stock/`，与基金业务互不 `use`。
> 本文档只覆盖**子项目 4：选股/推荐**。前置：子项目1（数据层）、2（回测）、3（诊断）已完成。

## 目标与范围

对一批股票逐只做「多策略样本外回测评分 + 技术诊断」，再跨股票 z-score 综合排名，产出 TopN 选股报告。镜像基金 `recommend.rs` 的框架，但股票独立实现、不 `use` 基金模块。

**做**：训练/检验切分 → 5 复用策略各段回测 → 训练段 z-score 选最优策略 → 跨股票用「最优策略样本外三指标」z-score 排名取 TopN；每只股票附子项目3 的 `StockDiagnosis` 作为形态上下文；注入式 loader 便于离线测。

**不做（YAGNI/他处）**：HTML/Web/API（子项目5）；股票专属策略（复用现有5种）；参数逐股寻优（固定稳健默认，降过拟合）。

## 设计要点

- **复用而不 `use` 基金**：候选策略用共用构造器 `crate::strategy::{Dca,SmartDca,Trend,Rsi,Adaptive}` 直接构建（不走基金 `config::build_strategy_from`）；回测用子项目2 的 `crate::stock::backtest::run_one`；诊断用子项目3 的 `crate::stock::diagnose::diagnose`。
- `StrategyEval`/`ScoreWeights` 等基金侧数据结构**不复用**（属基金 recommend 模块），股票侧另定同形小结构，保持隔离。
- 评分费率统一用 `StockFee`（默认 A股，`RecommendParams.fee` 可覆盖），保证跨股票口径一致（记录在案：跨市场时口径统一而非逐市场精确）。

## 模块结构

```
src/stock/recommend.rs  —— 新建：zscores/split_history/候选/evaluate_stock/rank_top/build_report + 结构体
```
`src/stock/mod.rs` 追加 `pub mod recommend;`。
另：给子项目3 的 `StockDiagnosis` 增加 `#[derive(Default)]`（便于诊断降级占位），不改其它行为。

## 数据结构（`recommend.rs`）

```rust
pub struct ScoreWeights { pub w_return: f64, pub w_sharpe: f64, pub w_mdd: f64 } // 默认 0.4/0.4/0.2
pub struct RecommendParams { pub top_n: usize, pub split_ratio: f64, pub weights: ScoreWeights, pub fee: StockFee }
// 默认 top_n=5, split_ratio=0.70, weights=默认, fee=StockFee::a_share()

pub struct StockStrategyEval {
    pub kind: String, pub name: String,
    pub is_return: f64, pub is_sharpe: f64, pub is_mdd: f64,
    pub oos_return: f64, pub oos_sharpe: f64, pub oos_mdd: f64,
    pub score: f64,
}
pub struct StockRecommendation {
    pub code: String, pub name: String, pub stock_score: f64,
    pub best_strategy: StockStrategyEval,
    pub all_strategies: Vec<StockStrategyEval>,
    pub diagnosis: StockDiagnosis,   // 子项目3
    pub rationale: String,
}
pub struct StockRecommendReport {
    pub generated: String, pub pool_size: usize, pub analyzed: usize,
    pub skipped: Vec<String>, pub top: Vec<StockRecommendation>,
    pub weights: ScoreWeights, pub split_ratio: f64, pub disclaimer: String,
}
```

## 函数

```rust
fn zscores(xs: &[f64]) -> Vec<f64>;                 // 总体std，σ≈0→全0，空→空
fn split_history(bars: &[StockBar], ratio: f64) -> Option<(&[StockBar], &[StockBar])>; // MIN_TRAIN=120, MIN_TEST=30
pub fn evaluate_stock(code: &str, name: &str, bars: &[StockBar], p: &RecommendParams) -> anyhow::Result<StockRecommendation>;
pub fn rank_top(recs: Vec<StockRecommendation>, p: &RecommendParams) -> Vec<StockRecommendation>;
pub fn build_report<F>(pool: &[&str], names: &HashMap<String,String>, today: &str, p: &RecommendParams, load: F) -> StockRecommendReport
    where F: FnMut(&str) -> anyhow::Result<Vec<StockBar>>;
```

- `evaluate_stock`：切分→5 候选各段回测（`run_one` 取 `summary`）→训练段三指标 z-score 加权（`+ret +sharpe − mdd`）选最优→附 `diagnose` 诊断（失败降级为 Default 占位）→装配 rationale。`stock_score` 留待 `rank_top`。
- `rank_top`：用各股「最优策略样本外三指标」跨股 z-score → `stock_score` 降序取 top_n。
- `build_report`：遍历池，注入 loader 取 bars → `evaluate_stock`；失败/加载错入 `skipped`；`rank_top` 后装配整页。

`DISCLAIMER` 常量：与基金同义的免责声明（股票文案）。

## 测试策略（全离线）

- `zscores`：常量→全0、空→空、保序。
- `split_history`：足量可切分（长度/阈值断言）、过短→None。
- `evaluate_stock`：300 点上涨可评估，`all_strategies.len()==5`，best 分最高且在候选集合，含诊断与 rationale；过短→错误含"数据不足"。
- `rank_top`/`build_report`：两只可分析（不同斜率）+ 一只加载失败 → analyzed=2、skipped=[BADX]、top 按 stock_score 降序；空池合法；序列化含前端键（top/best_strategy/diagnosis/stock_score/skipped/disclaimer 等）。

## 交付定义（Done）

- `cargo test` 全绿（新增推荐测试 + 既有全部）。
- `build_report` 可对注入式池产出 TopN 报告。
- 股票模块不 `use` 任何基金专属模块（策略/回测/诊断均为共用件或 stock 内部）。

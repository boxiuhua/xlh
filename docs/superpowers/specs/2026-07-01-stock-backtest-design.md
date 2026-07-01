# 单股回测设计（子项目 2）

> 全局背景见 [子项目1 spec](2026-07-01-stock-data-layer-design.md) 与工程记忆。整体：xlh 新增完整股票体系，股票代码收敛于顶层 `src/stock/`，与基金业务代码互不 `use`，仅共用资产无关的通用引擎。
> 本文档只覆盖**子项目 2：单股回测**。前置：子项目1（数据层）已完成。

## 目标与范围

让现有回测引擎跑在个股上，产出带真实 A/港/美股费用的回测结果与交易统计。

**做**：
- 复用现有 `Engine`（已是 `Engine<D: DataHandler, S: Strategy>` 泛型，`StockData` 直接可插）、`Portfolio`、`metrics::Summary`、`DailyRecord`/`TradeRecord`。
- 复用现有 5 种策略（DCA/Smart DCA/Trend/RSI/Adaptive）跑个股，本子项目**不新增股票专属策略**（留待后续）。
- 股票费用模型：佣金（买卖，含最低佣金）+ 印花税（卖）+ 过户费，按市场预设。
- 交易统计：FIFO 实现盈亏还原 → 胜率、盈亏比、平均盈/亏。

**不做（YAGNI / 归属其他子项目）**：
- HTML/图表/报告渲染 → 子项目5（Web层）。
- Web API/路由/前端 → 子项目5。
- 技术指标（MACD/布林等）与股票专属策略 → 子项目3。
- 跨股票选股/排名 → 子项目4。
- 做空、保证金、盘中撮合、分钟线。

## 核心架构决策

### 引擎复用：零成本

`Engine<D: DataHandler, S: Strategy>` 已泛型，`StockData`(子项目1，impl DataHandler) 与 `Box<dyn Strategy>` 可直接传入 `Engine::new(data, strategy, broker, portfolio)`。引擎按 `today.adj_nav`（=股票后复权价）撮合与记权益，语义正确，**引擎/组合/指标零改动**。

### 共用 Broker 泛化（行为保持）

现状：`Broker` 固定持有 `FeeModel { buy_rate, sell_tiers }`，`execute` 内买费=`cash*buy_rate`、卖费=`shares*price*sell_rate(days)`；无最低佣金、无印花税。这套"按持有天数分档"是基金语义，不适配股票。

方案：把费用计算抽象为共用 `Fee` trait，`Broker` 改持 `Box<dyn Fee>`：

```rust
// src/broker.rs（共用件，资产无关）
pub trait Fee {
    fn buy_fee(&self, cash: f64) -> f64;
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64;
}
impl Fee for FeeModel { /* 保持原基金逻辑 */ }
pub struct Broker { fee: Box<dyn Fee>, /* lots */ }
impl Broker { pub fn new(fee: impl Fee + 'static) -> Self { /* box 内部 */ } }
```

- **基金行为完全不变**：`FeeModel: Fee`，`Broker::new(fee_model)` 调用点无需改动（`FeeModel` 满足 `impl Fee + 'static`）。
- **顺带**：把 `FeeModel::sell_rate` 改为与档位顺序无关（在满足 `max_days==0 || days<=max_days` 的档中取 `max_days` 最小者，`0` 视作无穷大），等价于原"先排序再首命中"，从而移除 `Broker::new` 里的排序步骤。行为等价，基金测试保持通过。
- `Engine` 持有的仍是唯一具体类型 `Broker`（内含 `Box<dyn Fee>`），**故 `Engine` 无需泛型化、签名不变**。

被否决：为股票复制一套 `StockBroker`（重复 lots/FIFO/持仓逻辑，违反复用）；把 `Broker` 变泛型 `Broker<F>`（会波及 `Engine` 增加泛型参数，改动面更大）。

### 交易统计：股票侧纯函数，不碰共用代码

`TradeRecord { date, direction, shares, price, fee }` 无成本基准，无法直接得单笔实现盈亏。方案：在股票模块内对 `&[TradeRecord]` 做 **FIFO 成本匹配**的纯函数还原每笔卖出的实现盈亏。只读共用 `TradeRecord`，不改共用 event/broker/engine。

## 模块结构

```
src/stock/
├── mod.rs          （已存在，续加 pub mod fee; backtest; trade_stats;）
├── data/           （子项目1，已完成）
├── fee.rs          —— StockFee(impl crate::broker::Fee) + 市场预设(A股/港股/美股)
├── trade_stats.rs  —— TradeStats + trade_stats(&[TradeRecord]) FIFO 还原
└── backtest.rs     —— run_one(...) -> StockRunOutcome
```
共用件改动仅限 `src/broker.rs`（新增 `Fee` trait、`Broker` 持 `Box<dyn Fee>`、`sell_rate` 顺序无关化、更新其自身单测）。

## 费用模型 `StockFee`

```rust
pub struct StockFee {
    pub commission_rate: f64,  // 佣金率（买卖同）
    pub min_commission: f64,   // 最低佣金
    pub stamp_tax_rate: f64,   // 印花税率（仅卖出）
    pub transfer_rate: f64,    // 过户费率（买卖同）
}
```
- `buy_fee(cash) = max(cash*commission_rate, min_commission) + cash*transfer_rate`
- `sell_fee(shares,price,_) = max(v*commission_rate, min_commission) + v*stamp_tax_rate + v*transfer_rate`，其中 `v=shares*price`
- 忽略 `holding_days`（股票费用与持有期无关）。

**市场预设**（`StockFee::a_share()/hk()/us()` 与 `for_market(market: u16)`）：
| 市场 | 佣金率 | 最低佣金 | 印花税(卖) | 过户费 |
|------|--------|----------|-----------|--------|
| A股 (1/0) | 0.025% | 5.0 | 0.05% | 0.001% |
| 港股 (116) | 0.25% | 3.0 | 0.10% | 0 |
| 美股 (105/106/107) | 0 | 0 | 0 | 0 |
| 其他/未知 | 回退 A股 | | | |

**已知近似（记录在案，非缺陷）**：
- 撮合价用后复权价 `adj_close`，故印花税/佣金按后复权成交额计，与真实名义成交额有系统性缩放差异；回测内部前后一致，用于策略比较无偏。
- 忽略港币/美元与人民币汇率差异（回测在价格单位内自洽）。
- 忽略 SEC/交易所零星规费。

## 回测入口 `backtest.rs`

```rust
pub struct StockRunOutcome {
    pub name: String,
    pub code: String,
    pub summary: crate::metrics::Summary,
    pub trade_stats: crate::stock::trade_stats::TradeStats,
    pub daily: Vec<crate::result::DailyRecord>,
    pub trades: Vec<crate::result::TradeRecord>,
}

pub fn run_one(
    name: String,
    code: String,
    bars: Vec<crate::stock::data::StockBar>,
    strategy: Box<dyn crate::strategy::Strategy>,
    fee: StockFee,
    initial_cash: f64,
) -> StockRunOutcome;
```
内部：`Engine::new(StockData::new(bars), strategy, Broker::new(fee), Portfolio::new(initial_cash))` → `run()` → `metrics::summarize` + `trade_stats::trade_stats(engine.trades())`。

## 交易统计 `trade_stats.rs`

```rust
pub struct TradeStats {
    pub round_trips: usize, // 卖出笔数（每次卖出=一次实现事件）
    pub wins: usize,
    pub win_rate: f64,      // wins / round_trips
    pub profit_factor: f64, // 总盈利 / |总亏损|（无亏损→f64::INFINITY；无盈亏→0）
    pub avg_win: f64,
    pub avg_loss: f64,      // 取正值
    pub realized_pnl: f64,  // 累计实现盈亏
}
pub fn trade_stats(trades: &[TradeRecord]) -> TradeStats;
```
FIFO 还原：买入压入队列 `(剩余份额, 每股成本)`，每股成本 = `price + 买费/shares`（把买费摊入成本）。卖出按 FIFO 消耗，`实现盈亏 = Σ(卖价-每股成本)*消耗份额 - 卖费`；每次卖出计一次 round_trip，`>0` 计 win。

## 测试策略（全离线）

- `broker.rs`：新增 `Fee` trait 后，`FeeModel` 经 Broker 的买卖费与原先一致（回归）；`sell_rate` 顺序无关（原 `sell_rate_robust_to_tier_order` 改为直接测 `FeeModel::sell_rate`）；所有既有 broker/engine/runner 基金测试保持通过。
- `stock::fee`：A股买入最低佣金生效、卖出含印花税；港股/美股预设值；`for_market` 映射。
- `stock::trade_stats`：单笔盈利/亏损、FIFO 多笔部分卖出、无卖出→零、无亏损→profit_factor=∞。
- `stock::backtest`：用内存 `StockBar` 序列 + DCA 策略跑通，断言 `summary`/`trade_stats` 合理（对齐 runner 的 `run_one_dca_flat_then_up` 风格）。

## 交付定义（Done）

- `cargo test` 全绿：新增股票测试 + 全部既有基金测试（零回归）。
- `run_one` 可对一段内存 `StockBar` 跑通并给出 `StockRunOutcome`。
- 股票模块不 `use` 任何基金专属模块；仅用共用件（engine/broker/portfolio/metrics/result/strategy）与 `stock::data`。

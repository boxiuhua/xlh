# A股基金定投/择时策略回测引擎 —— 设计文档

- 日期：2026-06-14
- 状态：已评审待实现
- 语言/平台：Rust，CLI

## 1. 目标

构建一个 Rust 命令行回测引擎，针对 A股**场外基金**，支持定投与择时类策略的历史回测，输出关键绩效指标并生成图表。

**非目标（YAGNI）**：实盘下单、盘中交易、多基金组合优化、Web UI、实时行情推送。这些不在本期范围内。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 数据源 | Rust `reqwest` 直接抓天天基金(fund.eastmoney)接口 + 本地缓存 | 全 Rust，无外部语言依赖 |
| 引擎架构 | 事件驱动（事件队列） | 用户选定；T+1 确认、申赎费、分红的建模更自然，防偷看未来 |
| 净值口径 | 用累计净值复权 | 基金分红会使单位净值跳水，直接用单位净值回测虚减收益 |
| 输出 | CLI 指标表格 + `plotters` 生成 PNG 图表 | 纯 Rust，无需浏览器 |
| 配置 | TOML 文件 | 便于保存多个回测方案，复杂策略参数表达清晰 |
| 错误处理 | `anyhow`（应用层）+ `thiserror`（库内） | 回测主循环不 panic |

## 3. 架构与事件流

事件队列驱动，五类组件围绕一个 `VecDeque<Event>` 协作，每个交易日推进一次：

```
                ┌─────────────┐
   每个交易日 →  │ DataHandler │  推送当日净值 → MarketEvent
                └──────┬──────┘
                       ↓
                ┌─────────────┐
                │  Strategy   │  读 MarketEvent → 产生 SignalEvent
                └──────┬──────┘
                       ↓
                ┌─────────────┐
                │  Portfolio  │  读 Signal → 风控/仓位 → OrderEvent
                │             │  读 Fill   → 更新份额、现金、持仓
                └──────┬──────┘
                       ↓
                ┌─────────────┐
                │   Broker    │  读 Order → 按当日净值成交、扣申赎费 → FillEvent
                └─────────────┘
```

主循环：`while let Some(ev) = queue.pop_front()`，按事件类型分发给对应组件，组件可能往队列推新事件。每日先由 DataHandler 推入一个 Market 事件触发整链。

**对"基金"的两个关键好处**：
1. 成交时点明确——下单日 T 入队 Order，Broker 按**当日 T 净值**确认成交（对应 A股基金 15:00 前申购按当日净值），份额计入持仓；时间错位由事件流表达，不偷看未来。
2. 申购费/赎回费/分红集中在 Broker，策略层不关心成本。

## 4. 模块划分

```
xlh/
├── Cargo.toml
└── src/
    ├── main.rs           # CLI 入口、加载 TOML、装配引擎、跑主循环
    ├── event.rs          # Event 枚举 + 四种事件结构体
    ├── data/
    │   ├── mod.rs        # DataHandler trait + NavPoint
    │   ├── eastmoney.rs  # 天天基金 HTTP 抓取与解析
    │   └── cache.rs      # 本地净值缓存（CSV/JSON）
    ├── strategy/
    │   ├── mod.rs        # Strategy trait + StrategyContext
    │   ├── dca.rs        # 普通定投
    │   ├── smart_dca.rs  # 智能定投（择时加减）
    │   ├── trend.rs      # 择时买卖（均线）
    │   └── rules.rs      # 止盈止损/再平衡（可叠加规则层）
    ├── portfolio.rs      # 账户：份额/现金/持仓 + 风控
    ├── broker.rs         # 撮合、申赎费、分红复权
    ├── metrics.rs        # 收益/年化/最大回撤/夏普
    ├── report/
    │   ├── mod.rs        # CLI 表格输出
    │   └── chart.rs      # plotters 生成 PNG 曲线
    └── config.rs         # TOML 配置结构体
```

## 5. 核心类型与 trait

### 5.1 事件与基础数据（event.rs）

```rust
pub enum Event {
    Market(MarketEvent),
    Signal(SignalEvent),
    Order(OrderEvent),
    Fill(FillEvent),
}

pub struct MarketEvent {
    pub date: NaiveDate,
    pub nav: f64,          // 单位净值
    pub acc_nav: f64,      // 累计净值（用于复权）
}

pub enum Direction { Buy, Sell }

pub enum SignalAmount { Cash(f64), SharesRatio(f64), AllOut }

pub struct SignalEvent {       // 策略意图（金额或比例，不含价格）
    pub date: NaiveDate,
    pub direction: Direction,
    pub amount: SignalAmount,
}

pub struct OrderEvent {         // Portfolio 定下的确定指令
    pub date: NaiveDate,
    pub direction: Direction,
    pub cash: f64,             // 申购金额 / 赎回市值
}

pub struct FillEvent {          // Broker 成交回报
    pub date: NaiveDate,
    pub direction: Direction,
    pub shares: f64,           // 实际确认份额
    pub nav: f64,
    pub fee: f64,
}
```

### 5.2 DataHandler trait（data/mod.rs）

```rust
pub struct NavPoint { pub date: NaiveDate, pub nav: f64, pub acc_nav: f64 }

pub trait DataHandler {
    /// 推进到下一交易日；数据耗尽返回 None
    fn next_bar(&mut self) -> Option<MarketEvent>;
    /// 截至当前日的历史净值窗口（只给过去，防偷看未来）
    fn history(&self, lookback: usize) -> &[NavPoint];
}
```

### 5.3 Strategy trait（strategy/mod.rs）

```rust
pub trait Strategy {
    fn on_market(&mut self, ev: &MarketEvent, ctx: &StrategyContext) -> Vec<SignalEvent>;
}
```

`StrategyContext` 提供：历史净值窗口、当前持仓快照、交易日历（判断是否定投日）。

四种策略：
- **普通定投（dca）**：判断当日是否定投日 → 发固定金额 Buy。
- **智能定投（smart_dca）**：定投日按净值偏离均线/估值程度缩放金额（越跌买越多）。
- **择时买卖（trend）**：均线金叉发 Buy、死叉发 AllOut Sell。
- **止盈止损/再平衡（rules）**：`RuleLayer` 包在任意策略外层，每日检查回撤/目标收益，触发即发卖出信号，可与前三者组合。

### 5.4 风控与现金

放在 Portfolio：信号转订单时校验现金充足、赎回份额充足、单日最大投入上限等。

## 6. 指标（metrics.rs）

基于每日权益曲线计算：
- 累计收益率、年化收益率（CAGR）
- 最大回撤 MaxDrawdown
- 夏普比率（无风险利率可配，默认 0）
- 定投专属：累计投入、期末市值、持有成本均价

## 7. 配置文件示例（config.toml）

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
  { max_days = 0,   rate = 0.000 },  # max_days=0 表示更长期限
]

[strategy]
kind = "smart_dca"          # dca | smart_dca | trend | rules
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
ma_window = 250

[[rules]]                   # 可叠加止盈止损（可选、可多条）
kind = "take_profit"
target_return = 0.30

[report]
chart = true
out_dir = "output"
```

策略用 `kind` + serde tagged enum 反序列化；新增策略只加一个分支。

## 8. 错误处理

- `anyhow`（应用层）+ `thiserror`（库内错误类型）。
- 三类错误显式处理：
  1. 网络抓取失败 → 重试 + 回退本地缓存。
  2. 净值数据缺口（停牌日）→ 跳过该日，不前向填充进交易。
  3. 配置非法 → 启动即报错退出。
- 回测主循环绝不 panic。

## 9. 测试策略（TDD）

- `broker`：给定净值与费率，验证份额/费用（含赎回分档、复权）——纯函数，最易测。
- 每个 `strategy`：喂构造净值序列，断言信号序列正确（如金叉确实发 Buy）。
- `metrics`：已知权益曲线断言回撤/年化数值。
- `data/eastmoney`：用录制的样本 HTTP 响应测试解析，不打真实网络。
- 端到端：固定净值 CSV + 简单定投，断言最终市值（黄金用例）。

## 10. 依赖

`reqwest`(blocking) · `serde`/`serde_json` · `toml` · `chrono` · `plotters` · `anyhow` · `thiserror`；测试加 `mockito` 或录制响应。

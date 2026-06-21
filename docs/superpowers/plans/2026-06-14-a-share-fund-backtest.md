# A股基金回测引擎 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 Rust 构建一个事件驱动的 A股基金回测 CLI，支持定投/智能定投/择时/止盈止损四类策略，输出绩效指标并生成 PNG 图表。

**Architecture:** 事件队列驱动（Market→Signal→Order→Fill），DataHandler 喂日频净值，Strategy 产信号，Portfolio 管现金与权益，Broker 管份额/费率/复权。数据从天天基金 pingzhongdata 接口抓取并本地 CSV 缓存。

**Tech Stack:** Rust 2021 · chrono · serde/serde_json · toml · reqwest(blocking) · plotters · clap · anyhow · thiserror

**实现约定（贯穿全程）：**
- 所有交易/估值用**复权净值 `adj_nav`**（由单位净值+累计净值推导，隐含红利再投）；单位净值 `nav` 仅用于图表展示买卖点。
- 现金模型：定投买入时若现金不足，自动注入差额并记为"外部投入"`contribution`（定投初始现金=0 时每次买入都是一次投入；择时可给初始现金池）。
- 浮点份额比较统一用 `1e-9` 容差。
- **范围说明：** `rules` 层实现**止盈/止损**；多资产"再平衡"在单基金场景无意义，本期不做（执行交接时会向用户点明）。

**类型字典（跨任务必须保持一致的命名）：**
- `Direction::{Buy, Sell}`，`SignalAmount::{Cash(f64), Ratio(f64), AllOut}`，`OrderQty::{Cash(f64), Shares(f64), AllShares}`
- `MarketEvent{date, nav, adj_nav}`、`NavPoint{date, nav, acc_nav}`
- `SignalEvent{date, direction, amount}`、`OrderEvent{date, direction, qty}`、`FillEvent{date, direction, shares, price, fee}`
- `Position{shares, avg_cost}`、`EquityPoint{date, equity, contribution}`
- `Strategy::on_market(&mut self, ctx:&StrategyContext)->Vec<SignalEvent>`
- `DataHandler::{next_bar()->Option<MarketEvent>, history(lookback)->&[MarketEvent]}`

---

## File Structure

| 文件 | 职责 |
|------|------|
| `Cargo.toml` | 依赖与包配置 |
| `src/lib.rs` | 模块声明（供集成测试 `use xlh::...`） |
| `src/main.rs` | CLI 入口：解析参数→加载配置→抓数→装配引擎→跑→出报告 |
| `src/event.rs` | 四类事件与方向/数量枚举 |
| `src/data/mod.rs` | `NavPoint`、`compute_adjusted`、`DataHandler` trait、`InMemoryData` |
| `src/data/eastmoney.rs` | 天天基金 pingzhongdata 抓取与解析 |
| `src/data/cache.rs` | 本地 CSV 缓存读写、load_or_fetch |
| `src/broker.rs` | `FeeModel`/`SellTier`/`Broker`：撮合、申赎费、FIFO 份额、持仓快照 |
| `src/portfolio.rs` | 现金、外部投入、权益曲线、信号转订单、应用成交 |
| `src/metrics.rs` | 总收益、XIRR 年化、最大回撤、夏普 |
| `src/strategy/mod.rs` | `Strategy` trait、`StrategyContext`、`Schedule`、`moving_average` |
| `src/strategy/dca.rs` | 普通定投 |
| `src/strategy/smart_dca.rs` | 智能定投（择时加减） |
| `src/strategy/trend.rs` | 择时买卖（双均线金叉死叉） |
| `src/strategy/rules.rs` | 止盈/止损规则层 `RuleLayer` |
| `src/engine.rs` | 事件主循环 `Engine` |
| `src/config.rs` | TOML 配置结构体 + 构建 strategy/fee/portfolio |
| `src/report/mod.rs` | CLI 指标表格 |
| `src/report/chart.rs` | plotters 生成 PNG |

---

## Task 1: 项目脚手架

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`

- [ ] **Step 1: 写 Cargo.toml**

```toml
[package]
name = "xlh"
version = "0.1.0"
edition = "2021"

[dependencies]
chrono = { version = "0.4", features = ["serde"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
reqwest = { version = "0.12", features = ["blocking", "rustls-tls"], default-features = false }
plotters = "0.3"
clap = { version = "4", features = ["derive"] }
anyhow = "1"
thiserror = "1"

[[bin]]
name = "xlh"
path = "src/main.rs"

[lib]
name = "xlh"
path = "src/lib.rs"
```

- [ ] **Step 2: 写 src/lib.rs（先只声明已存在的模块，随任务推进逐步打开注释）**

```rust
pub mod event;
// 后续任务逐个取消注释：
// pub mod data;
// pub mod broker;
// pub mod portfolio;
// pub mod metrics;
// pub mod strategy;
// pub mod engine;
// pub mod config;
// pub mod report;
```

- [ ] **Step 3: 写最小 src/main.rs 与占位 src/event.rs**

`src/main.rs`:
```rust
fn main() {
    println!("xlh backtest");
}
```
`src/event.rs`:
```rust
// 占位，Task 2 填充
```

- [ ] **Step 4: 编译验证**

Run: `cargo build`
Expected: 编译通过（首次会拉取依赖）。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/
git commit -m "chore: scaffold xlh cargo project"
```

---

## Task 2: 事件与基础枚举（event.rs）

**Files:**
- Modify: `src/event.rs`
- Test: 内联 `#[cfg(test)]`

- [ ] **Step 1: 写失败测试（替换 src/event.rs 占位内容，先只放测试 + 空类型会编译失败）**

直接写完整实现更高效，本任务为纯类型定义，测试仅验证可构造与 `Event` 分发。先写实现：

```rust
use chrono::NaiveDate;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction { Buy, Sell }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalAmount { Cash(f64), Ratio(f64), AllOut }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderQty { Cash(f64), Shares(f64), AllShares }

#[derive(Debug, Clone, PartialEq)]
pub struct MarketEvent { pub date: NaiveDate, pub nav: f64, pub adj_nav: f64 }

#[derive(Debug, Clone, PartialEq)]
pub struct SignalEvent { pub date: NaiveDate, pub direction: Direction, pub amount: SignalAmount }

#[derive(Debug, Clone, PartialEq)]
pub struct OrderEvent { pub date: NaiveDate, pub direction: Direction, pub qty: OrderQty }

#[derive(Debug, Clone, PartialEq)]
pub struct FillEvent { pub date: NaiveDate, pub direction: Direction, pub shares: f64, pub price: f64, pub fee: f64 }

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Market(MarketEvent),
    Signal(SignalEvent),
    Order(OrderEvent),
    Fill(FillEvent),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_wraps_market() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let ev = Event::Market(MarketEvent { date: d, nav: 1.0, adj_nav: 1.0 });
        assert!(matches!(ev, Event::Market(_)));
    }
}
```

- [ ] **Step 2: 运行测试确认失败→通过**

Run: `cargo test --lib event`
Expected: 编译并 PASS（若 lib.rs 未声明 `event` 会失败，确保 Task1 的 lib.rs 含 `pub mod event;`）。

- [ ] **Step 3: Commit**

```bash
git add src/event.rs
git commit -m "feat: core event and enum types"
```

---

## Task 3: 数据层与复权（data/mod.rs）

**Files:**
- Create: `src/data/mod.rs`
- Modify: `src/lib.rs`（取消 `pub mod data;` 注释）
- Test: 内联

- [ ] **Step 1: 写失败测试**

在 `src/data/mod.rs` 顶部先放测试（连同将实现的签名）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    #[test]
    fn adjusted_reinvests_dividend() {
        // 第二天单位净值不变(1.0->1.0)但累计净值+0.1，说明分红0.1 → 复权应+10%
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.0, acc_nav: 1.1 },
        ];
        let adj = compute_adjusted(&pts);
        assert!((adj[0] - 1.0).abs() < 1e-9);
        assert!((adj[1] - 1.1).abs() < 1e-9);
    }

    #[test]
    fn adjusted_no_dividend_tracks_nav_growth() {
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.2, acc_nav: 1.2 },
        ];
        let adj = compute_adjusted(&pts);
        assert!((adj[1] - 1.2).abs() < 1e-9);
    }

    #[test]
    fn history_never_returns_future() {
        let pts = vec![
            NavPoint { date: d(2024,1,1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024,1,2), nav: 1.1, acc_nav: 1.1 },
            NavPoint { date: d(2024,1,3), nav: 1.2, acc_nav: 1.2 },
        ];
        let mut h = InMemoryData::new(pts);
        let b1 = h.next_bar().unwrap();
        assert_eq!(b1.date, d(2024,1,1));
        assert_eq!(h.history(10).len(), 1); // 只含已发出的当日
        h.next_bar();
        assert_eq!(h.history(10).len(), 2);
        assert_eq!(h.history(1).len(), 1); // lookback 截断
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib data`
Expected: FAIL（`NavPoint`/`compute_adjusted`/`InMemoryData` 未定义）。

- [ ] **Step 3: 写实现（置于测试模块之上）**

```rust
use chrono::NaiveDate;
use crate::event::MarketEvent;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NavPoint { pub date: NaiveDate, pub nav: f64, pub acc_nav: f64 }

/// 由单位净值+累计净值推导复权净值（隐含红利再投）。
/// 当日分红 = 累计净值增量 - 单位净值增量（截断为非负）。
pub fn compute_adjusted(points: &[NavPoint]) -> Vec<f64> {
    let mut adj = Vec::with_capacity(points.len());
    if points.is_empty() { return adj; }
    adj.push(points[0].nav);
    for i in 1..points.len() {
        let prev = &points[i - 1];
        let cur = &points[i];
        let dividend = ((cur.acc_nav - prev.acc_nav) - (cur.nav - prev.nav)).max(0.0);
        let factor = if prev.nav > 0.0 { (cur.nav + dividend) / prev.nav } else { 1.0 };
        adj.push(adj[i - 1] * factor);
    }
    adj
}

pub trait DataHandler {
    /// 推进到下一交易日；数据耗尽返回 None。
    fn next_bar(&mut self) -> Option<MarketEvent>;
    /// 截至当前已发出 bar 的历史窗口（只含过去与当日，绝不含未来）。
    fn history(&self, lookback: usize) -> &[MarketEvent];
}

/// 内存数据源（测试与已抓取数据回放共用）。
pub struct InMemoryData { bars: Vec<MarketEvent>, cursor: usize }

impl InMemoryData {
    pub fn new(points: Vec<NavPoint>) -> Self {
        let adj = compute_adjusted(&points);
        let bars = points
            .iter()
            .zip(adj)
            .map(|(p, a)| MarketEvent { date: p.date, nav: p.nav, adj_nav: a })
            .collect();
        Self { bars, cursor: 0 }
    }
}

impl DataHandler for InMemoryData {
    fn next_bar(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.bars.len() {
            let b = self.bars[self.cursor].clone();
            self.cursor += 1;
            Some(b)
        } else { None }
    }
    fn history(&self, lookback: usize) -> &[MarketEvent] {
        let end = self.cursor;
        let start = end.saturating_sub(lookback);
        &self.bars[start..end]
    }
}
```
然后在 `src/lib.rs` 取消注释 `pub mod data;`。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib data`
Expected: PASS（3 个测试）。

- [ ] **Step 5: Commit**

```bash
git add src/data/mod.rs src/lib.rs
git commit -m "feat: data handler with dividend-adjusted nav"
```

---

## Task 4: 撮合与费率（broker.rs）

**Files:**
- Create: `src/broker.rs`
- Modify: `src/lib.rs`（加 `pub mod broker;`）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, OrderEvent, OrderQty};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn fee_model() -> FeeModel {
        FeeModel { buy_rate: 0.0015, sell_tiers: vec![
            SellTier{max_days:7, rate:0.015},
            SellTier{max_days:365, rate:0.005},
            SellTier{max_days:0, rate:0.0},
        ]}
    }

    #[test]
    fn buy_applies_fee_and_creates_shares() {
        let mut b = Broker::new(fee_model());
        let order = OrderEvent{date:d(2024,1,1), direction:Direction::Buy, qty:OrderQty::Cash(1000.0)};
        let fill = b.execute(&order, 2.0);
        // fee = 1000*0.0015 = 1.5; net = 998.5; shares = 998.5/2.0 = 499.25
        assert!((fill.fee - 1.5).abs() < 1e-9);
        assert!((fill.shares - 499.25).abs() < 1e-9);
        assert!((b.total_shares() - 499.25).abs() < 1e-9);
    }

    #[test]
    fn sell_all_uses_holding_day_tier_fifo() {
        let mut b = Broker::new(fee_model());
        // 两笔买入，价格都=1.0，各得 ~99.85 与 ~99.85 份额，忽略买费精度后近似
        b.execute(&OrderEvent{date:d(2024,1,1),direction:Direction::Buy,qty:OrderQty::Cash(100.0)}, 1.0);
        b.execute(&OrderEvent{date:d(2024,6,1),direction:Direction::Buy,qty:OrderQty::Cash(100.0)}, 1.0);
        let total = b.total_shares();
        // 在 2024-1-5 全部赎回：第一笔持有4天(<=7→1.5%)，第二笔为未来日期不该出现，这里用更晚日期
        let fill = b.execute(&OrderEvent{date:d(2024,1,5),direction:Direction::Sell,qty:OrderQty::AllShares}, 1.0);
        assert!((fill.shares - total).abs() < 1e-9);
        // 第一笔持有4天→1.5%，第二笔持有(从6-1到1-5为负，num_days为负)→走catch-all 0%
        // 故费 ≈ 第一笔份额*1.0*0.015
    }

    #[test]
    fn sell_rate_tiers() {
        let f = fee_model();
        assert!((f.sell_rate(3) - 0.015).abs() < 1e-9);
        assert!((f.sell_rate(100) - 0.005).abs() < 1e-9);
        assert!((f.sell_rate(400) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn avg_cost_tracks_weighted_price() {
        let mut b = Broker::new(fee_model());
        b.execute(&OrderEvent{date:d(2024,1,1),direction:Direction::Buy,qty:OrderQty::Cash(1000.0)}, 1.0);
        b.execute(&OrderEvent{date:d(2024,2,1),direction:Direction::Buy,qty:OrderQty::Cash(1000.0)}, 2.0);
        let pos = b.position();
        assert!(pos.avg_cost > 1.0 && pos.avg_cost < 2.0);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib broker`
Expected: FAIL（类型未定义）。

- [ ] **Step 3: 写实现**

```rust
use chrono::NaiveDate;
use crate::event::{Direction, FillEvent, OrderEvent, OrderQty};

#[derive(Debug, Clone)]
pub struct SellTier { pub max_days: i64, pub rate: f64 }

#[derive(Debug, Clone)]
pub struct FeeModel { pub buy_rate: f64, pub sell_tiers: Vec<SellTier> }

impl FeeModel {
    /// 按持有天数选择赎回费率；max_days==0 表示"更长期限"的兜底档。
    pub fn sell_rate(&self, holding_days: i64) -> f64 {
        for t in &self.sell_tiers {
            if t.max_days == 0 || holding_days <= t.max_days { return t.rate; }
        }
        0.0
    }
}

struct Lot { date: NaiveDate, shares: f64, cost: f64 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position { pub shares: f64, pub avg_cost: f64 }

/// 兼托管(持有份额lots)与执行(撮合扣费)。
pub struct Broker { fee: FeeModel, lots: Vec<Lot> }

impl Broker {
    pub fn new(fee: FeeModel) -> Self { Self { fee, lots: Vec::new() } }

    pub fn total_shares(&self) -> f64 { self.lots.iter().map(|l| l.shares).sum() }

    pub fn position(&self) -> Position {
        let shares = self.total_shares();
        let avg_cost = if shares > 1e-9 {
            self.lots.iter().map(|l| l.shares * l.cost).sum::<f64>() / shares
        } else { 0.0 };
        Position { shares, avg_cost }
    }

    /// 按当日复权价 price 撮合一个订单，返回成交回报。
    pub fn execute(&mut self, order: &OrderEvent, price: f64) -> FillEvent {
        match order.direction {
            Direction::Buy => {
                let cash = match order.qty { OrderQty::Cash(c) => c, _ => 0.0 };
                let fee = cash * self.fee.buy_rate;
                let shares = if price > 0.0 { (cash - fee) / price } else { 0.0 };
                if shares > 0.0 {
                    self.lots.push(Lot { date: order.date, shares, cost: price });
                }
                FillEvent { date: order.date, direction: Direction::Buy, shares, price, fee }
            }
            Direction::Sell => {
                let want = match order.qty {
                    OrderQty::Shares(s) => s,
                    OrderQty::AllShares => self.total_shares(),
                    OrderQty::Cash(_) => 0.0,
                };
                let mut remaining = want.min(self.total_shares());
                let mut sold = 0.0;
                let mut fee = 0.0;
                let mut i = 0;
                while remaining > 1e-9 && i < self.lots.len() {
                    let take = remaining.min(self.lots[i].shares);
                    let days = (order.date - self.lots[i].date).num_days();
                    fee += take * price * self.fee.sell_rate(days);
                    self.lots[i].shares -= take;
                    sold += take;
                    remaining -= take;
                    i += 1;
                }
                self.lots.retain(|l| l.shares > 1e-9);
                FillEvent { date: order.date, direction: Direction::Sell, shares: sold, price, fee }
            }
        }
    }
}
```
`src/lib.rs` 加 `pub mod broker;`。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib broker`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/broker.rs src/lib.rs
git commit -m "feat: broker with fee tiers and FIFO lots"
```

---

## Task 5: 账户与权益（portfolio.rs）

**Files:**
- Create: `src/portfolio.rs`
- Modify: `src/lib.rs`（加 `pub mod portfolio;`）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::Position;
    use crate::event::{Direction, FillEvent, MarketEvent, SignalAmount, SignalEvent, OrderQty};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn buy_with_zero_cash_auto_contributes() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        // 一次买入成交：100 份额 @ 价1.0，费 1.5
        let fill = FillEvent{date:d(2024,1,1), direction:Direction::Buy, shares:100.0, price:1.0, fee:1.5};
        p.apply_fill(&fill);
        // 需要 100*1.0+1.5 = 101.5 现金，初始0 → 注入101.5
        assert!((p.total_contributed - 101.5).abs() < 1e-9);
        assert!((p.cash).abs() < 1e-9);
    }

    #[test]
    fn sell_adds_proceeds_to_cash() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        p.apply_fill(&FillEvent{date:d(2024,1,1),direction:Direction::Buy,shares:100.0,price:1.0,fee:0.0});
        p.apply_fill(&FillEvent{date:d(2024,2,1),direction:Direction::Sell,shares:100.0,price:1.5,fee:0.0});
        // 卖得 150 现金
        assert!((p.cash - 150.0).abs() < 1e-9);
    }

    #[test]
    fn signal_to_order_conversion() {
        let p = Portfolio::new(0.0);
        let today = MarketEvent{date:d(2024,1,1), nav:1.0, adj_nav:1.0};
        let pos = Position{shares:100.0, avg_cost:1.0};
        let buy = p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Buy,amount:SignalAmount::Cash(500.0)}, &pos, &today).unwrap();
        assert_eq!(buy.qty, OrderQty::Cash(500.0));
        let sell = p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Sell,amount:SignalAmount::AllOut}, &pos, &today).unwrap();
        assert_eq!(sell.qty, OrderQty::AllShares);
        // 无持仓时卖出 → None
        let empty = Position{shares:0.0, avg_cost:0.0};
        assert!(p.on_signal(&SignalEvent{date:d(2024,1,1),direction:Direction::Sell,amount:SignalAmount::AllOut}, &empty, &today).is_none());
    }

    #[test]
    fn equity_curve_records_value_and_contribution() {
        let mut p = Portfolio::new(0.0);
        p.seed(d(2024,1,1));
        p.apply_fill(&FillEvent{date:d(2024,1,1),direction:Direction::Buy,shares:100.0,price:1.0,fee:0.0});
        p.record_equity(d(2024,1,1), 100.0, 1.0);
        let pt = p.curve.last().unwrap();
        assert!((pt.equity - 100.0).abs() < 1e-9);   // cash0 + 100*1.0
        assert!((pt.contribution - 100.0).abs() < 1e-9); // 当日注入100
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib portfolio`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use chrono::NaiveDate;
use crate::broker::Position;
use crate::event::{Direction, FillEvent, MarketEvent, OrderEvent, OrderQty, SignalAmount, SignalEvent};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EquityPoint { pub date: NaiveDate, pub equity: f64, pub contribution: f64 }

pub struct Portfolio {
    pub cash: f64,
    pub total_contributed: f64,
    pub curve: Vec<EquityPoint>,
    /// 外部现金流(投入为负、期末市值为正)，供 XIRR。
    pub flows: Vec<(NaiveDate, f64)>,
    initial_cash: f64,
    last_recorded_contributed: f64,
}

impl Portfolio {
    pub fn new(initial_cash: f64) -> Self {
        Self {
            cash: initial_cash,
            total_contributed: initial_cash,
            curve: Vec::new(),
            flows: Vec::new(),
            initial_cash,
            last_recorded_contributed: 0.0,
        }
    }

    /// 在首个交易日登记初始投入现金流。
    pub fn seed(&mut self, date: NaiveDate) {
        if self.initial_cash > 0.0 {
            self.flows.push((date, -self.initial_cash));
        }
    }

    fn contribute(&mut self, amount: f64, date: NaiveDate) {
        self.cash += amount;
        self.total_contributed += amount;
        self.flows.push((date, -amount));
    }

    /// 将策略信号转为确定订单，做基本风控；无效/无仓返回 None。
    pub fn on_signal(&self, sig: &SignalEvent, pos: &Position, today: &MarketEvent) -> Option<OrderEvent> {
        match (sig.direction, sig.amount) {
            (Direction::Buy, SignalAmount::Cash(c)) if c > 0.0 =>
                Some(OrderEvent { date: sig.date, direction: Direction::Buy, qty: OrderQty::Cash(c) }),
            (Direction::Sell, SignalAmount::AllOut) if pos.shares > 1e-9 =>
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::AllShares }),
            (Direction::Sell, SignalAmount::Ratio(r)) if pos.shares > 1e-9 && r > 0.0 => {
                let s = pos.shares * r.min(1.0);
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::Shares(s) })
            }
            (Direction::Sell, SignalAmount::Cash(c)) if pos.shares > 1e-9 && c > 0.0 && today.adj_nav > 0.0 => {
                let s = (c / today.adj_nav).min(pos.shares);
                Some(OrderEvent { date: sig.date, direction: Direction::Sell, qty: OrderQty::Shares(s) })
            }
            _ => None,
        }
    }

    /// 应用成交：买入不足现金自动注入投入；卖出加回现金。
    pub fn apply_fill(&mut self, fill: &FillEvent) {
        match fill.direction {
            Direction::Buy => {
                let needed = fill.shares * fill.price + fill.fee;
                if self.cash < needed {
                    let deficit = needed - self.cash;
                    self.contribute(deficit, fill.date);
                }
                self.cash -= needed;
            }
            Direction::Sell => {
                self.cash += fill.shares * fill.price - fill.fee;
            }
        }
    }

    /// 记录当日权益与"当日新增投入"。
    pub fn record_equity(&mut self, date: NaiveDate, shares: f64, price: f64) {
        let equity = self.cash + shares * price;
        let contribution = self.total_contributed - self.last_recorded_contributed;
        self.last_recorded_contributed = self.total_contributed;
        self.curve.push(EquityPoint { date, equity, contribution });
    }

    /// 回测结束登记期末市值为正向现金流（XIRR 用）。
    pub fn finalize(&mut self, date: NaiveDate, equity: f64) {
        self.flows.push((date, equity));
    }
}
```
`src/lib.rs` 加 `pub mod portfolio;`。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib portfolio`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/portfolio.rs src/lib.rs
git commit -m "feat: portfolio with contribution model and equity curve"
```

---

## Task 6: 绩效指标（metrics.rs）

**Files:**
- Create: `src/metrics.rs`
- Modify: `src/lib.rs`（加 `pub mod metrics;`）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::EquityPoint;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn total_return_basic() {
        assert!((total_return(150.0, 100.0) - 0.5).abs() < 1e-9);
        assert!((total_return(100.0, 0.0)).abs() < 1e-9); // 防除零
    }

    #[test]
    fn max_drawdown_basic() {
        let curve = vec![
            EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0},
            EquityPoint{date:d(2024,1,2),equity:120.0,contribution:0.0},
            EquityPoint{date:d(2024,1,3),equity:90.0,contribution:0.0},
            EquityPoint{date:d(2024,1,4),equity:110.0,contribution:0.0},
        ];
        // 峰值120→谷底90，回撤 = 1 - 90/120 = 0.25
        assert!((max_drawdown(&curve) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn xirr_one_year_doubling() {
        // 年初投入100，一年后取回200 → 年化≈100%
        let flows = vec![(d(2023,1,1), -100.0), (d(2024,1,1), 200.0)];
        let r = xirr(&flows).unwrap();
        assert!((r - 1.0).abs() < 0.02);
    }

    #[test]
    fn sharpe_runs_on_curve() {
        let curve = vec![
            EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0},
            EquityPoint{date:d(2024,1,2),equity:101.0,contribution:0.0},
            EquityPoint{date:d(2024,1,3),equity:102.0,contribution:0.0},
        ];
        let s = sharpe(&curve, 0.0);
        assert!(s.is_finite());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib metrics`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use chrono::NaiveDate;
use crate::portfolio::EquityPoint;

pub fn total_return(final_equity: f64, total_contributed: f64) -> f64 {
    if total_contributed > 0.0 { final_equity / total_contributed - 1.0 } else { 0.0 }
}

/// 权益曲线上的最大回撤（峰到谷的最大相对跌幅）。
pub fn max_drawdown(curve: &[EquityPoint]) -> f64 {
    let mut peak = f64::MIN;
    let mut mdd = 0.0;
    for p in curve {
        if p.equity > peak { peak = p.equity; }
        if peak > 0.0 {
            let dd = 1.0 - p.equity / peak;
            if dd > mdd { mdd = dd; }
        }
    }
    mdd
}

fn xnpv(rate: f64, flows: &[(NaiveDate, f64)], t0: NaiveDate) -> f64 {
    flows.iter().map(|(date, amt)| {
        let years = (*date - t0).num_days() as f64 / 365.0;
        amt / (1.0 + rate).powf(years)
    }).sum()
}

/// 货币加权年化收益（XIRR），二分法求根。无解返回 None。
pub fn xirr(flows: &[(NaiveDate, f64)]) -> Option<f64> {
    if flows.len() < 2 { return None; }
    let t0 = flows.iter().map(|(d, _)| *d).min()?;
    let (mut lo, mut hi) = (-0.9999_f64, 10.0_f64);
    let f_lo = xnpv(lo, flows, t0);
    let f_hi = xnpv(hi, flows, t0);
    if f_lo * f_hi > 0.0 { return None; } // 同号无法二分
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let f_mid = xnpv(mid, flows, t0);
        if f_mid.abs() < 1e-7 { return Some(mid); }
        if f_lo * f_mid < 0.0 { hi = mid; } else { lo = mid; }
    }
    Some((lo + hi) / 2.0)
}

/// 年化夏普：日收益剔除当日外部投入对权益的抬升。
pub fn sharpe(curve: &[EquityPoint], rf_annual: f64) -> f64 {
    if curve.len() < 2 { return 0.0; }
    let mut rets = Vec::new();
    for w in curve.windows(2) {
        let prev = w[0].equity;
        let cur = w[1].equity;
        if prev > 0.0 {
            // 剔除当日新增投入，只看市值变动带来的收益
            rets.push((cur - w[1].contribution - prev) / prev);
        }
    }
    if rets.is_empty() { return 0.0; }
    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / n;
    let std = var.sqrt();
    if std < 1e-12 { return 0.0; }
    (mean * 252.0 - rf_annual) / (std * 252.0_f64.sqrt())
}
```
`src/lib.rs` 加 `pub mod metrics;`。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib metrics`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/metrics.rs src/lib.rs
git commit -m "feat: performance metrics (return, drawdown, xirr, sharpe)"
```

---

## Task 7: 策略基座（strategy/mod.rs）

**Files:**
- Create: `src/strategy/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod strategy;`）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MarketEvent;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn monthly_schedule_fires_once_per_month_on_or_after_day() {
        let mut s = Schedule::new(Period::Monthly, 5);
        assert!(!s.due(d(2024,1,3)));  // 早于5号
        assert!(s.due(d(2024,1,5)));   // 当月首次到达5号
        assert!(!s.due(d(2024,1,8)));  // 当月已触发
        assert!(s.due(d(2024,2,6)));   // 新月份
    }

    #[test]
    fn moving_average_needs_full_window() {
        let bars = vec![
            MarketEvent{date:d(2024,1,1),nav:1.0,adj_nav:1.0},
            MarketEvent{date:d(2024,1,2),nav:1.0,adj_nav:2.0},
            MarketEvent{date:d(2024,1,3),nav:1.0,adj_nav:3.0},
        ];
        assert_eq!(moving_average(&bars, 4), None);
        assert_eq!(moving_average(&bars, 3), Some(2.0));
        assert_eq!(moving_average(&bars[..2], 2), Some(1.5));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib strategy`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use chrono::{Datelike, NaiveDate};
use crate::event::{MarketEvent, SignalEvent};

pub mod dca;
pub mod smart_dca;
pub mod trend;
pub mod rules;

/// 策略每日可见的上下文（历史只到当日，防偷看未来）。
pub struct StrategyContext<'a> {
    pub today: &'a MarketEvent,
    pub history: &'a [MarketEvent],
    pub shares: f64,
    pub avg_cost: f64,
    pub cash: f64,
}

pub trait Strategy {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent>;
}

/// 让 Box<dyn Strategy> 也满足 Strategy，便于 RuleLayer 与引擎组合。
impl Strategy for Box<dyn Strategy> {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        (**self).on_market(ctx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Period { Monthly, Weekly }

/// 定投日历：每周期(月/周)在到达目标日时触发一次。
pub struct Schedule { period: Period, day: u32, last_key: Option<i64> }

impl Schedule {
    pub fn new(period: Period, day: u32) -> Self { Self { period, day, last_key: None } }

    pub fn due(&mut self, date: NaiveDate) -> bool {
        let (key, reached) = match self.period {
            Period::Monthly => {
                let key = date.year() as i64 * 12 + date.month() as i64;
                (key, date.day() >= self.day)
            }
            Period::Weekly => {
                let iso = date.iso_week();
                let key = iso.year() as i64 * 53 + iso.week() as i64;
                (key, date.weekday().number_from_monday() >= self.day)
            }
        };
        if reached && self.last_key != Some(key) {
            self.last_key = Some(key);
            true
        } else { false }
    }
}

/// 最近 window 根 bar 的复权净值均线（含当日）。
pub fn moving_average(history: &[MarketEvent], window: usize) -> Option<f64> {
    if window == 0 || history.len() < window { return None; }
    let slice = &history[history.len() - window..];
    Some(slice.iter().map(|b| b.adj_nav).sum::<f64>() / window as f64)
}
```
`src/lib.rs` 加 `pub mod strategy;`。注意：本任务声明了 `dca/smart_dca/trend/rules` 子模块，需在 Task 8-11 创建对应文件后才能整体编译；为让本任务可独立 `cargo test`，**先把这 4 行 `pub mod` 注释掉**，Task 8-11 逐个取消注释。

修订 Step 3：先注释子模块声明：
```rust
// pub mod dca;
// pub mod smart_dca;
// pub mod trend;
// pub mod rules;
```

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib strategy`
Expected: PASS（schedule + ma 共 2 测试）。

- [ ] **Step 5: Commit**

```bash
git add src/strategy/mod.rs src/lib.rs
git commit -m "feat: strategy trait, context, schedule and moving average"
```

---

## Task 8: 普通定投（strategy/dca.rs）

**Files:**
- Create: `src/strategy/dca.rs`
- Modify: `src/strategy/mod.rs`（取消 `pub mod dca;` 注释）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount};
    use crate::strategy::{Period, StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn buys_fixed_amount_on_schedule_day() {
        let mut s = Dca::new(Period::Monthly, 1, 1000.0);
        let today = MarketEvent{date:d(2024,1,1),nav:1.0,adj_nav:1.0};
        let ctx = StrategyContext{today:&today, history:&[today.clone()], shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].direction, Direction::Buy);
        assert_eq!(sigs[0].amount, SignalAmount::Cash(1000.0));
    }

    #[test]
    fn no_signal_off_schedule() {
        let mut s = Dca::new(Period::Monthly, 15, 1000.0);
        let today = MarketEvent{date:d(2024,1,3),nav:1.0,adj_nav:1.0};
        let ctx = StrategyContext{today:&today, history:&[today.clone()], shares:0.0, avg_cost:0.0, cash:0.0};
        assert!(s.on_market(&ctx).is_empty());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib strategy::dca`
Expected: FAIL（先在 mod.rs 取消注释 `pub mod dca;` 才能编译到此文件）。

- [ ] **Step 3: 写实现**

```rust
use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{Period, Schedule, Strategy, StrategyContext};

pub struct Dca { schedule: Schedule, amount: f64 }

impl Dca {
    pub fn new(period: Period, day: u32, amount: f64) -> Self {
        Self { schedule: Schedule::new(period, day), amount }
    }
}

impl Strategy for Dca {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        if self.schedule.due(ctx.today.date) {
            vec![SignalEvent {
                date: ctx.today.date,
                direction: Direction::Buy,
                amount: SignalAmount::Cash(self.amount),
            }]
        } else { Vec::new() }
    }
}
```
在 `src/strategy/mod.rs` 取消 `pub mod dca;` 注释。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib strategy::dca`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/strategy/dca.rs src/strategy/mod.rs
git commit -m "feat: plain DCA strategy"
```

---

## Task 9: 智能定投（strategy/smart_dca.rs）

**Files:**
- Create: `src/strategy/smart_dca.rs`
- Modify: `src/strategy/mod.rs`（取消 `pub mod smart_dca;` 注释）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{MarketEvent, SignalAmount};
    use crate::strategy::{Period, StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn bars(prices: &[f64]) -> Vec<MarketEvent> {
        prices.iter().enumerate().map(|(i,p)| MarketEvent{date:d(2024,1,(i+1) as u32),nav:*p,adj_nav:*p}).collect()
    }

    #[test]
    fn buys_more_when_below_ma() {
        // 均线≈中枢，当前价低于均线 → 金额 > base
        let mut s = SmartDca::new(Period::Monthly, 1, 1000.0, 3, 1.0);
        let hist = bars(&[1.2, 1.0, 0.8]); // ma=1.0, 当前0.8, dev=-0.2 → 1000*(1+0.2)=1200
        let today = hist.last().unwrap().clone();
        // 把定投日设到当天(1月3日? day=1 已在1月触发? 用首个bar触发)。改用 day=1, 当月首bar即触发
        let ctx = StrategyContext{today:&today, history:&hist, shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs.len(), 1);
        if let SignalAmount::Cash(amt) = sigs[0].amount {
            assert!(amt > 1000.0, "expected >base, got {amt}");
            assert!(amt <= 2000.0);
        } else { panic!("expected cash"); }
    }

    #[test]
    fn falls_back_to_base_without_enough_history() {
        let mut s = SmartDca::new(Period::Monthly, 1, 1000.0, 250, 1.0);
        let hist = bars(&[1.0]);
        let today = hist.last().unwrap().clone();
        let ctx = StrategyContext{today:&today, history:&hist, shares:0.0, avg_cost:0.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert_eq!(sigs[0].amount, SignalAmount::Cash(1000.0));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib strategy::smart_dca`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{moving_average, Period, Schedule, Strategy, StrategyContext};

pub struct SmartDca {
    schedule: Schedule,
    base: f64,
    ma_window: usize,
    k: f64, // 偏离敏感系数：越跌买越多
}

impl SmartDca {
    pub fn new(period: Period, day: u32, base: f64, ma_window: usize, k: f64) -> Self {
        Self { schedule: Schedule::new(period, day), base, ma_window, k }
    }
}

impl Strategy for SmartDca {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        if !self.schedule.due(ctx.today.date) { return Vec::new(); }
        let amount = match moving_average(ctx.history, self.ma_window) {
            Some(ma) if ma > 0.0 => {
                let dev = ctx.today.adj_nav / ma - 1.0;       // 低于均线为负
                (self.base * (1.0 - self.k * dev)).clamp(self.base * 0.5, self.base * 2.0)
            }
            _ => self.base,
        };
        vec![SignalEvent { date: ctx.today.date, direction: Direction::Buy, amount: SignalAmount::Cash(amount) }]
    }
}
```
在 `src/strategy/mod.rs` 取消 `pub mod smart_dca;` 注释。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib strategy::smart_dca`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/strategy/smart_dca.rs src/strategy/mod.rs
git commit -m "feat: smart DCA scaling by deviation from MA"
```

---

## Task 10: 择时买卖（strategy/trend.rs）

**Files:**
- Create: `src/strategy/trend.rs`
- Modify: `src/strategy/mod.rs`（取消 `pub mod trend;` 注释）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount};
    use crate::strategy::{StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,(day).min(28)).unwrap()}

    fn run(prices: &[f64], short: usize, long: usize) -> Vec<crate::event::SignalEvent> {
        let mut s = Trend::new(short, long, 1000.0);
        let bars: Vec<MarketEvent> = prices.iter().enumerate()
            .map(|(i,p)| MarketEvent{date:d(2024,1,(i+1) as u32),nav:*p,adj_nav:*p}).collect();
        let mut out = Vec::new();
        for i in 0..bars.len() {
            let ctx = StrategyContext{today:&bars[i], history:&bars[..=i], shares: if i>0 {100.0} else {0.0}, avg_cost:1.0, cash:0.0};
            out.extend(s.on_market(&ctx));
        }
        out
    }

    #[test]
    fn golden_cross_buys_dead_cross_sells() {
        // 价格先跌后涨：短均线先在下方后上穿长均线→Buy；再下穿→Sell
        let prices = [3.0,2.0,1.0,1.0,2.0,3.0,4.0, 3.0,2.0,1.0];
        let sigs = run(&prices, 2, 4);
        assert!(sigs.iter().any(|s| s.direction==Direction::Buy));
        assert!(sigs.iter().any(|s| s.direction==Direction::Sell && s.amount==SignalAmount::AllOut));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib strategy::trend`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{moving_average, Strategy, StrategyContext};

/// 双均线：短上穿长(金叉)买入固定金额；短下穿长(死叉)清仓。
pub struct Trend {
    short: usize,
    long: usize,
    amount: f64,
    prev_short_above: Option<bool>,
}

impl Trend {
    pub fn new(short: usize, long: usize, amount: f64) -> Self {
        Self { short, long, amount, prev_short_above: None }
    }
}

impl Strategy for Trend {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let (ms, ml) = match (moving_average(ctx.history, self.short), moving_average(ctx.history, self.long)) {
            (Some(s), Some(l)) => (s, l),
            _ => return Vec::new(),
        };
        let above = ms > ml;
        let mut out = Vec::new();
        if let Some(prev) = self.prev_short_above {
            if above && !prev {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Buy, amount: SignalAmount::Cash(self.amount) });
            } else if !above && prev && ctx.shares > 1e-9 {
                out.push(SignalEvent { date: ctx.today.date, direction: Direction::Sell, amount: SignalAmount::AllOut });
            }
        }
        self.prev_short_above = Some(above);
        out
    }
}
```
在 `src/strategy/mod.rs` 取消 `pub mod trend;` 注释。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib strategy::trend`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/strategy/trend.rs src/strategy/mod.rs
git commit -m "feat: dual-MA trend timing strategy"
```

---

## Task 11: 止盈止损规则层（strategy/rules.rs）

**Files:**
- Create: `src/strategy/rules.rs`
- Modify: `src/strategy/mod.rs`（取消 `pub mod rules;` 注释）
- Test: 内联

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount, SignalEvent};
    use crate::strategy::{StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    struct Noop;
    impl Strategy for Noop { fn on_market(&mut self, _:&StrategyContext)->Vec<SignalEvent>{Vec::new()} }

    #[test]
    fn take_profit_triggers_all_out() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::TakeProfit{target_return:0.3}]);
        let today = MarketEvent{date:d(2024,6,1),nav:1.4,adj_nav:1.4};
        // 持仓成本1.0，现价1.4 → +40% ≥ 30% → 清仓
        let ctx = StrategyContext{today:&today, history:&[today.clone()], shares:100.0, avg_cost:1.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert!(sigs.iter().any(|x| x.direction==Direction::Sell && x.amount==SignalAmount::AllOut));
    }

    #[test]
    fn stop_loss_on_drawdown_from_peak() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::StopLoss{max_drawdown:0.2}]);
        // 先记录高点市值，再跌破20%
        let peak = MarketEvent{date:d(2024,1,1),nav:2.0,adj_nav:2.0};
        let _ = s.on_market(&StrategyContext{today:&peak, history:&[peak.clone()], shares:100.0, avg_cost:1.0, cash:0.0});
        let drop = MarketEvent{date:d(2024,1,2),nav:1.5,adj_nav:1.5}; // 市值150 vs 峰200 → -25%
        let sigs = s.on_market(&StrategyContext{today:&drop, history:&[drop.clone()], shares:100.0, avg_cost:1.0, cash:0.0});
        assert!(sigs.iter().any(|x| x.direction==Direction::Sell));
    }

    #[test]
    fn no_trigger_without_position() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::TakeProfit{target_return:0.1}]);
        let today = MarketEvent{date:d(2024,1,1),nav:5.0,adj_nav:5.0};
        let ctx = StrategyContext{today:&today, history:&[today.clone()], shares:0.0, avg_cost:0.0, cash:0.0};
        assert!(s.on_market(&ctx).is_empty());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib strategy::rules`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{Strategy, StrategyContext};

#[derive(Debug, Clone, Copy)]
pub enum Rule {
    TakeProfit { target_return: f64 }, // 相对持仓均价的收益达标 → 清仓
    StopLoss { max_drawdown: f64 },    // 自持仓市值峰值回撤达标 → 清仓
}

/// 包裹任意内层策略，叠加止盈/止损；触发时追加清仓信号。
pub struct RuleLayer {
    inner: Box<dyn Strategy>,
    rules: Vec<Rule>,
    peak_value: f64,
}

impl RuleLayer {
    pub fn new(inner: Box<dyn Strategy>, rules: Vec<Rule>) -> Self {
        Self { inner, rules, peak_value: 0.0 }
    }
}

impl Strategy for RuleLayer {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let mut sigs = self.inner.on_market(ctx);
        let pos_value = ctx.shares * ctx.today.adj_nav;
        if pos_value > self.peak_value { self.peak_value = pos_value; }

        if ctx.shares <= 1e-9 { return sigs; }

        let mut exit = false;
        for rule in &self.rules {
            match rule {
                Rule::TakeProfit { target_return } => {
                    if ctx.avg_cost > 0.0 && (ctx.today.adj_nav / ctx.avg_cost - 1.0) >= *target_return {
                        exit = true;
                    }
                }
                Rule::StopLoss { max_drawdown } => {
                    if self.peak_value > 0.0 && (1.0 - pos_value / self.peak_value) >= *max_drawdown {
                        exit = true;
                    }
                }
            }
        }
        if exit {
            sigs.push(SignalEvent { date: ctx.today.date, direction: Direction::Sell, amount: SignalAmount::AllOut });
            self.peak_value = 0.0; // 清仓后重置峰值，便于再入场重新计
        }
        sigs
    }
}
```
在 `src/strategy/mod.rs` 取消 `pub mod rules;` 注释。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib strategy::rules`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/strategy/rules.rs src/strategy/mod.rs
git commit -m "feat: take-profit/stop-loss rule layer"
```

---

## Task 12: 事件主循环（engine.rs）+ 端到端黄金用例

**Files:**
- Create: `src/engine.rs`
- Modify: `src/lib.rs`（加 `pub mod engine;`）
- Test: 内联（端到端）

- [ ] **Step 1: 写失败测试（黄金用例：固定净值 + 普通定投，断言期末市值）**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{InMemoryData, NavPoint};
    use crate::broker::{Broker, FeeModel, SellTier};
    use crate::portfolio::Portfolio;
    use crate::strategy::Period;
    use crate::strategy::dca::Dca;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    fn no_fee() -> FeeModel { FeeModel{ buy_rate:0.0, sell_tiers:vec![SellTier{max_days:0,rate:0.0}] } }

    #[test]
    fn dca_on_flat_then_up_market() {
        // 净值: 1月1日=1.0(定投日买1000→1000份), 2月1日=1.0(再买1000→1000份), 3月1日=2.0(不买)
        let points = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,2,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,3,1),nav:2.0,acc_nav:2.0},
        ];
        let data = InMemoryData::new(points);
        let strat = Dca::new(Period::Monthly, 1, 1000.0);
        let mut engine = Engine::new(data, strat, Broker::new(no_fee()), Portfolio::new(0.0));
        let pf = engine.run();
        // 共投入2000，持有2000份，3月1日价2.0 → 市值4000
        let last = pf.curve.last().unwrap();
        assert!((last.equity - 4000.0).abs() < 1e-6, "equity={}", last.equity);
        assert!((pf.total_contributed - 2000.0).abs() < 1e-9);
        // XIRR 现金流应含两笔投入与期末市值
        assert!(pf.flows.len() >= 3);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib engine`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use std::collections::VecDeque;
use crate::broker::Broker;
use crate::data::DataHandler;
use crate::event::Event;
use crate::portfolio::Portfolio;
use crate::strategy::{Strategy, StrategyContext};

pub struct Engine<D: DataHandler, S: Strategy> {
    data: D,
    strategy: S,
    broker: Broker,
    portfolio: Portfolio,
    lookback: usize,
}

impl<D: DataHandler, S: Strategy> Engine<D, S> {
    pub fn new(data: D, strategy: S, broker: Broker, portfolio: Portfolio) -> Self {
        Self { data, strategy, broker, portfolio, lookback: usize::MAX }
    }

    pub fn run(&mut self) -> &Portfolio {
        let mut seeded = false;
        while let Some(today) = self.data.next_bar() {
            if !seeded { self.portfolio.seed(today.date); seeded = true; }

            let mut queue: VecDeque<Event> = VecDeque::new();
            queue.push_back(Event::Market(today.clone()));

            while let Some(ev) = queue.pop_front() {
                match ev {
                    Event::Market(m) => {
                        let pos = self.broker.position();
                        let history = self.data.history(self.lookback);
                        let ctx = StrategyContext {
                            today: &m,
                            history,
                            shares: pos.shares,
                            avg_cost: pos.avg_cost,
                            cash: self.portfolio.cash,
                        };
                        for s in self.strategy.on_market(&ctx) {
                            queue.push_back(Event::Signal(s));
                        }
                    }
                    Event::Signal(s) => {
                        let pos = self.broker.position();
                        if let Some(o) = self.portfolio.on_signal(&s, &pos, &today) {
                            queue.push_back(Event::Order(o));
                        }
                    }
                    Event::Order(o) => {
                        let fill = self.broker.execute(&o, today.adj_nav);
                        queue.push_back(Event::Fill(fill));
                    }
                    Event::Fill(f) => {
                        self.portfolio.apply_fill(&f);
                    }
                }
            }
            self.portfolio.record_equity(today.date, self.broker.total_shares(), today.adj_nav);
        }
        if let Some(last) = self.portfolio.curve.last() {
            let (date, equity) = (last.date, last.equity);
            self.portfolio.finalize(date, equity);
        }
        &self.portfolio
    }
}
```
`src/lib.rs` 加 `pub mod engine;`。

- [ ] **Step 4: 运行测试通过 + 全量回归**

Run: `cargo test`
Expected: 全部 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/engine.rs src/lib.rs
git commit -m "feat: event-driven engine with end-to-end DCA golden test"
```

---

## Task 13: 配置加载与装配（config.rs）

**Files:**
- Create: `src/config.rs`
- Modify: `src/lib.rs`（加 `pub mod config;`）
- Test: 内联（解析 TOML 字符串 + 构建对象）

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[data]
fund_code = "161725"
start = "2020-01-01"
end = "2024-12-31"
cache_dir = ".cache"

[fees]
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7, rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0, rate = 0.0 },
]

[strategy]
kind = "smart_dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
ma_window = 250
k = 1.0

[[rules]]
kind = "take_profit"
target_return = 0.3

[portfolio]
initial_cash = 0.0

[report]
chart = true
out_dir = "output"
"#;

    #[test]
    fn parses_full_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.data.fund_code, "161725");
        assert_eq!(cfg.fees.sell_tiers.len(), 3);
        assert_eq!(cfg.strategy.kind, "smart_dca");
        assert_eq!(cfg.rules.len(), 1);
        assert!(cfg.report.chart);
    }

    #[test]
    fn builds_fee_model_and_strategy() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        let fee = build_fee(&cfg);
        assert!((fee.buy_rate - 0.0015).abs() < 1e-9);
        // 构建策略不应 panic
        let _strat = build_strategy(&cfg).unwrap();
    }

    #[test]
    fn dca_minimal_config() {
        let s = r#"
[data]
fund_code="000001"
start="2020-01-01"
end="2020-12-31"
cache_dir=".cache"
[fees]
buy_rate=0.0
sell_tiers=[{max_days=0, rate=0.0}]
[strategy]
kind="dca"
[strategy.params]
period="monthly"
day=1
base_amount=500.0
[report]
chart=false
out_dir="output"
"#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(build_strategy(&cfg).is_ok());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib config`
Expected: FAIL。

- [ ] **Step 3: 写实现**

```rust
use std::path::PathBuf;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use crate::broker::{FeeModel, SellTier};
use crate::strategy::{Period, Strategy};
use crate::strategy::dca::Dca;
use crate::strategy::smart_dca::SmartDca;
use crate::strategy::trend::Trend;
use crate::strategy::rules::{Rule, RuleLayer};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub data: DataCfg,
    pub fees: FeesCfg,
    pub strategy: StrategyCfg,
    #[serde(default)]
    pub rules: Vec<RuleCfg>,
    #[serde(default)]
    pub portfolio: PortfolioCfg,
    pub report: ReportCfg,
}

#[derive(Debug, Deserialize)]
pub struct DataCfg {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct FeesCfg {
    pub buy_rate: f64,
    pub sell_tiers: Vec<SellTierCfg>,
}

#[derive(Debug, Deserialize)]
pub struct SellTierCfg { pub max_days: i64, pub rate: f64 }

#[derive(Debug, Deserialize)]
pub struct StrategyCfg {
    pub kind: String,
    #[serde(default)]
    pub params: toml::Value,
}

#[derive(Debug, Deserialize)]
pub struct RuleCfg {
    pub kind: String,
    #[serde(default)]
    pub target_return: f64,
    #[serde(default)]
    pub max_drawdown: f64,
}

#[derive(Debug, Default, Deserialize)]
pub struct PortfolioCfg {
    #[serde(default)]
    pub initial_cash: f64,
}

#[derive(Debug, Deserialize)]
pub struct ReportCfg {
    pub chart: bool,
    pub out_dir: PathBuf,
}

// 各策略参数结构
#[derive(Debug, Deserialize)]
struct DcaParams { period: String, day: u32, base_amount: f64 }

#[derive(Debug, Deserialize)]
struct SmartDcaParams { period: String, day: u32, base_amount: f64, ma_window: usize, #[serde(default = "one")] k: f64 }
fn one() -> f64 { 1.0 }

#[derive(Debug, Deserialize)]
struct TrendParams { short_window: usize, long_window: usize, amount: f64 }

fn parse_period(s: &str) -> Result<Period> {
    match s.to_lowercase().as_str() {
        "monthly" => Ok(Period::Monthly),
        "weekly" => Ok(Period::Weekly),
        other => Err(anyhow!("未知定投周期: {other}")),
    }
}

pub fn load(path: &std::path::Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取配置 {} 失败: {e}", path.display()))?;
    let cfg: Config = toml::from_str(&text).map_err(|e| anyhow!("配置解析失败: {e}"))?;
    Ok(cfg)
}

pub fn build_fee(cfg: &Config) -> FeeModel {
    FeeModel {
        buy_rate: cfg.fees.buy_rate,
        sell_tiers: cfg.fees.sell_tiers.iter()
            .map(|t| SellTier { max_days: t.max_days, rate: t.rate })
            .collect(),
    }
}

fn build_rules(cfg: &Config) -> Result<Vec<Rule>> {
    cfg.rules.iter().map(|r| match r.kind.as_str() {
        "take_profit" => Ok(Rule::TakeProfit { target_return: r.target_return }),
        "stop_loss" => Ok(Rule::StopLoss { max_drawdown: r.max_drawdown }),
        other => Err(anyhow!("未知规则: {other}")),
    }).collect()
}

pub fn build_strategy(cfg: &Config) -> Result<Box<dyn Strategy>> {
    let base: Box<dyn Strategy> = match cfg.strategy.kind.as_str() {
        "dca" => {
            let p: DcaParams = cfg.strategy.params.clone().try_into()?;
            Box::new(Dca::new(parse_period(&p.period)?, p.day, p.base_amount))
        }
        "smart_dca" => {
            let p: SmartDcaParams = cfg.strategy.params.clone().try_into()?;
            Box::new(SmartDca::new(parse_period(&p.period)?, p.day, p.base_amount, p.ma_window, p.k))
        }
        "trend" => {
            let p: TrendParams = cfg.strategy.params.clone().try_into()?;
            Box::new(Trend::new(p.short_window, p.long_window, p.amount))
        }
        other => return Err(anyhow!("未知策略: {other}")),
    };
    let rules = build_rules(cfg)?;
    if rules.is_empty() {
        Ok(base)
    } else {
        Ok(Box::new(RuleLayer::new(base, rules)))
    }
}
```
`src/lib.rs` 加 `pub mod config;`。

注意：`Engine<D,S>` 的 `S: Strategy`，而这里返回 `Box<dyn Strategy>`。Task 7 已为 `Box<dyn Strategy>` 实现 `Strategy`，故 `Engine::new(data, strategy_box, ...)` 可用 `S = Box<dyn Strategy>`。

- [ ] **Step 4: 运行测试通过**

Run: `cargo test --lib config`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/lib.rs
git commit -m "feat: TOML config loading and component assembly"
```

---

## Task 14: 数据抓取与缓存（data/eastmoney.rs, data/cache.rs）

**Files:**
- Create: `src/data/eastmoney.rs`
- Create: `src/data/cache.rs`
- Modify: `src/data/mod.rs`（加 `pub mod eastmoney; pub mod cache;`）
- Test: 内联（解析用内嵌样本，不打真实网络）

- [ ] **Step 1: 写解析失败测试（eastmoney）**

在 `src/data/eastmoney.rs`：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    // 精简样本：模拟 pingzhongdata.js 中的两个数组
    const SAMPLE: &str = r#"
var fS_name = "测试基金";
var Data_netWorthTrend = [{"x":1577808000000,"y":1.0,"equityReturn":0,"unitMoney":""},{"x":1577894400000,"y":1.1,"equityReturn":10,"unitMoney":""}];
var Data_ACWorthTrend = [[1577808000000,1.0],[1577894400000,1.2]];
var Data_grandTotal = [];
"#;

    #[test]
    fn parses_nav_and_acc() {
        let pts = parse_pingzhongdata(SAMPLE).unwrap();
        assert_eq!(pts.len(), 2);
        assert!((pts[0].nav - 1.0).abs() < 1e-9);
        assert!((pts[1].nav - 1.1).abs() < 1e-9);
        assert!((pts[1].acc_nav - 1.2).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib data::eastmoney`
Expected: FAIL。

- [ ] **Step 3: 写 eastmoney 实现**

```rust
use anyhow::{anyhow, Result};
use chrono::DateTime;
use serde::Deserialize;
use crate::data::NavPoint;

#[derive(Deserialize)]
struct NetWorth { x: i64, y: f64 }

/// 从 body 中截取 `var <name> = <array>;` 的数组文本。
fn extract_array(body: &str, name: &str) -> Result<String> {
    let key = format!("var {name} = ");
    let start = body.find(&key).ok_or_else(|| anyhow!("未找到 {name}"))? + key.len();
    let rest = &body[start..];
    let end = rest.find("];").ok_or_else(|| anyhow!("{name} 数组未闭合"))?;
    Ok(rest[..=end].to_string()) // 含结尾 ']'
}

pub fn parse_pingzhongdata(body: &str) -> Result<Vec<NavPoint>> {
    let nw_text = extract_array(body, "Data_netWorthTrend")?;
    let ac_text = extract_array(body, "Data_ACWorthTrend")?;
    let nw: Vec<NetWorth> = serde_json::from_str(&nw_text)?;
    let ac: Vec<(i64, f64)> = serde_json::from_str(&ac_text)?;

    let mut acc_map = std::collections::HashMap::new();
    for (ts, v) in ac { acc_map.insert(ts, v); }

    let mut points = Vec::with_capacity(nw.len());
    for n in nw {
        let dt = DateTime::from_timestamp_millis(n.x)
            .ok_or_else(|| anyhow!("非法时间戳 {}", n.x))?;
        let date = dt.date_naive();
        let acc_nav = *acc_map.get(&n.x).unwrap_or(&n.y);
        points.push(NavPoint { date, nav: n.y, acc_nav });
    }
    points.sort_by_key(|p| p.date);
    Ok(points)
}

/// 从天天基金 pingzhongdata 接口抓取全量净值。
pub fn fetch(code: &str) -> Result<Vec<NavPoint>> {
    let url = format!("https://fund.eastmoney.com/pingzhongdata/{code}.js");
    let body = reqwest::blocking::Client::new()
        .get(&url)
        .header("Referer", "https://fund.eastmoney.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .map_err(|e| anyhow!("请求 {url} 失败: {e}"))?
        .text()
        .map_err(|e| anyhow!("读取响应失败: {e}"))?;
    parse_pingzhongdata(&body)
}
```

- [ ] **Step 4: 写缓存失败测试（cache）**

在 `src/data/cache.rs`：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::NavPoint;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn csv_roundtrip() {
        let pts = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,1,2),nav:1.1,acc_nav:1.2},
        ];
        let tmp = std::env::temp_dir().join("xlh_cache_test.csv");
        write_csv(&tmp, &pts).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back.len(), 2);
        assert!((back[1].acc_nav - 1.2).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }
}
```

- [ ] **Step 5: 运行确认失败**

Run: `cargo test --lib data::cache`
Expected: FAIL。

- [ ] **Step 6: 写 cache 实现**

```rust
use std::path::Path;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use crate::data::{eastmoney, NavPoint};

pub fn write_csv(path: &Path, points: &[NavPoint]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from("date,nav,acc_nav\n");
    for p in points {
        s.push_str(&format!("{},{},{}\n", p.date, p.nav, p.acc_nav));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<NavPoint>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; } // 跳过表头
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 { continue; }
        let date = NaiveDate::parse_from_str(cols[0], "%Y-%m-%d")?;
        let nav: f64 = cols[1].parse()?;
        let acc_nav: f64 = cols[2].parse()?;
        out.push(NavPoint { date, nav, acc_nav });
    }
    Ok(out)
}

/// 有缓存读缓存，否则抓取并写缓存；最后按 [start,end] 过滤并排序。
pub fn load_or_fetch(code: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> Result<Vec<NavPoint>> {
    let path = cache_dir.join(format!("{code}.csv"));
    let mut points = if path.exists() {
        read_csv(&path)?
    } else {
        let p = eastmoney::fetch(code)?;
        write_csv(&path, &p)?;
        p
    };
    points.retain(|p| p.date >= start && p.date <= end);
    points.sort_by_key(|p| p.date);
    if points.is_empty() {
        return Err(anyhow!("基金 {code} 在 {start}~{end} 无净值数据"));
    }
    Ok(points)
}
```
在 `src/data/mod.rs` 加：
```rust
pub mod eastmoney;
pub mod cache;
```

- [ ] **Step 7: 运行测试通过**

Run: `cargo test --lib data`
Expected: PASS（含已有 data 测试 + 新增解析/缓存）。

- [ ] **Step 8: Commit**

```bash
git add src/data/eastmoney.rs src/data/cache.rs src/data/mod.rs
git commit -m "feat: eastmoney fetch and CSV cache"
```

---

## Task 15: 报告与图表（report/mod.rs, report/chart.rs）

**Files:**
- Create: `src/report/mod.rs`
- Create: `src/report/chart.rs`
- Modify: `src/lib.rs`（加 `pub mod report;`）
- Test: 内联（指标摘要字符串可断言；图表生成做 smoke test）

- [ ] **Step 1: 写失败测试（report 摘要）**

在 `src/report/mod.rs`：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::Portfolio;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn summary_contains_key_metrics() {
        let mut pf = Portfolio::new(0.0);
        pf.seed(d(2024,1,1));
        pf.record_equity(d(2024,1,1), 0.0, 1.0); // 占位
        // 手工灌入一个简单曲线
        pf.curve.clear();
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2023,1,1),equity:100.0,contribution:100.0});
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,1),equity:150.0,contribution:0.0});
        pf.total_contributed = 100.0;
        pf.flows = vec![(d(2023,1,1),-100.0),(d(2024,1,1),150.0)];
        let s = summary(&pf);
        assert!(s.contains("总收益"));
        assert!(s.contains("最大回撤"));
        assert!(s.contains("年化"));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib report`
Expected: FAIL。

- [ ] **Step 3: 写 report/mod.rs 实现**

```rust
pub mod chart;

use crate::metrics;
use crate::portfolio::Portfolio;

/// 生成可打印的指标摘要文本。
pub fn summary(pf: &Portfolio) -> String {
    let final_equity = pf.curve.last().map(|p| p.equity).unwrap_or(0.0);
    let total_ret = metrics::total_return(final_equity, pf.total_contributed);
    let mdd = metrics::max_drawdown(&pf.curve);
    let ann = metrics::xirr(&pf.flows).unwrap_or(0.0);
    let sharpe = metrics::sharpe(&pf.curve, 0.0);

    format!(
        "==== 回测结果 ====\n\
         累计投入   : {:.2}\n\
         期末市值   : {:.2}\n\
         总收益     : {:.2}%\n\
         年化(XIRR) : {:.2}%\n\
         最大回撤   : {:.2}%\n\
         夏普比率   : {:.2}\n\
         交易日数   : {}\n",
        pf.total_contributed,
        final_equity,
        total_ret * 100.0,
        ann * 100.0,
        mdd * 100.0,
        sharpe,
        pf.curve.len(),
    )
}

/// 打印摘要到 stdout。
pub fn print_summary(pf: &Portfolio) {
    println!("{}", summary(pf));
}
```

- [ ] **Step 4: 运行 report 测试通过**

Run: `cargo test --lib report`
Expected: PASS。

- [ ] **Step 5: 写 chart 实现 + smoke 测试**

在 `src/report/chart.rs`：
```rust
use std::path::Path;
use anyhow::{anyhow, Result};
use plotters::prelude::*;
use crate::portfolio::Portfolio;

/// 把权益曲线画成 PNG，保存到 out_dir/equity.png。
pub fn render_equity(pf: &Portfolio, out_dir: &Path) -> Result<()> {
    if pf.curve.is_empty() { return Err(anyhow!("无数据可画图")); }
    std::fs::create_dir_all(out_dir).ok();
    let path = out_dir.join("equity.png");

    let root = BitMapBackend::new(path.to_str().unwrap(), (1000, 600)).into_drawing_area();
    root.fill(&WHITE)?;

    let n = pf.curve.len();
    let y_min = pf.curve.iter().map(|p| p.equity).fold(f64::MAX, f64::min);
    let y_max = pf.curve.iter().map(|p| p.equity).fold(f64::MIN, f64::max);
    let pad = (y_max - y_min).max(1.0) * 0.05;

    let mut chart = ChartBuilder::on(&root)
        .caption("Equity Curve", ("sans-serif", 28))
        .margin(20)
        .x_label_area_size(40)
        .y_label_area_size(60)
        .build_cartesian_2d(0..n, (y_min - pad)..(y_max + pad))?;

    chart.configure_mesh().draw()?;
    chart.draw_series(LineSeries::new(
        pf.curve.iter().enumerate().map(|(i, p)| (i, p.equity)),
        &BLUE,
    ))?;

    root.present()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn renders_png_file() {
        let mut pf = Portfolio::new(0.0);
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0});
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,2),equity:110.0,contribution:0.0});
        let dir = std::env::temp_dir().join("xlh_chart_test");
        render_equity(&pf, &dir).unwrap();
        assert!(dir.join("equity.png").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```
`src/lib.rs` 加 `pub mod report;`。

- [ ] **Step 6: 运行测试通过**

Run: `cargo test --lib report`
Expected: PASS（summary + chart smoke）。

- [ ] **Step 7: Commit**

```bash
git add src/report/ src/lib.rs
git commit -m "feat: metrics summary and equity chart (plotters)"
```

---

## Task 16: CLI 装配（main.rs）+ 示例配置 + 手动验证

**Files:**
- Modify: `src/main.rs`
- Create: `config.toml`（示例）

- [ ] **Step 1: 写 main.rs**

```rust
use std::path::PathBuf;
use anyhow::Result;
use clap::Parser;

use xlh::broker::Broker;
use xlh::config;
use xlh::data::{cache, InMemoryData};
use xlh::engine::Engine;
use xlh::portfolio::Portfolio;
use xlh::report;

#[derive(Parser)]
#[command(name = "xlh", about = "A股基金定投/择时回测")]
struct Cli {
    /// 配置文件路径
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load(&cli.config)?;

    let points = cache::load_or_fetch(
        &cfg.data.fund_code,
        &cfg.data.cache_dir,
        cfg.data.start,
        cfg.data.end,
    )?;
    println!("加载 {} 条净值（{} ~ {}）", points.len(), cfg.data.start, cfg.data.end);

    let data = InMemoryData::new(points);
    let strategy = config::build_strategy(&cfg)?;
    let broker = Broker::new(config::build_fee(&cfg));
    let portfolio = Portfolio::new(cfg.portfolio.initial_cash);

    let mut engine = Engine::new(data, strategy, broker, portfolio);
    let pf = engine.run();

    report::print_summary(pf);

    if cfg.report.chart {
        report::chart::render_equity(pf, &cfg.report.out_dir)?;
        println!("图表已保存到 {}/equity.png", cfg.report.out_dir.display());
    }
    Ok(())
}
```

- [ ] **Step 2: 写示例 config.toml**

```toml
[data]
fund_code = "161725"        # 招商中证白酒
start = "2020-01-01"
end   = "2024-12-31"
cache_dir = ".cache"

[fees]
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7,   rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0,   rate = 0.000 },
]

[strategy]
kind = "smart_dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
ma_window = 250
k = 1.0

[[rules]]
kind = "take_profit"
target_return = 0.30

[portfolio]
initial_cash = 0.0

[report]
chart = true
out_dir = "output"
```

- [ ] **Step 3: 编译并全量测试**

Run: `cargo build && cargo test`
Expected: 编译通过，所有测试 PASS。

- [ ] **Step 4: 手动联网验证（真实抓取一只基金）**

Run: `cargo run -- --config config.toml`
Expected: 打印"加载 N 条净值"、回测指标摘要，并在 `output/equity.png` 生成图表。若网络受限，先准备一个 `.cache/161725.csv`（date,nav,acc_nav 三列）即可离线跑。

- [ ] **Step 5: Commit**

```bash
git add src/main.rs config.toml
git commit -m "feat: CLI assembly and sample config"
```

---

## Self-Review 结论

- **Spec 覆盖：** 数据源(Task14)、事件驱动(Task12)、复权(Task3)、四类策略(Task8-11)、止盈止损(Task11)、申赎费分档(Task4)、指标(Task6)、TOML 配置(Task13)、图表(Task15)、错误处理(贯穿，anyhow/thiserror)、测试(每任务 TDD) 均有对应任务。
- **再平衡：** Spec 第 5.4/范围处提到"再平衡"，单基金场景无多资产权重，本计划未实现，已在顶部范围说明显式标注，执行交接时会向用户确认是否需要（如需，可作为后续单独 plan）。
- **类型一致性：** `OrderQty`/`SignalAmount`/`FillEvent{shares,price,fee}`/`Position{shares,avg_cost}`/`StrategyContext` 字段在所有任务中统一；`Box<dyn Strategy>: Strategy` 在 Task7 提供，使 Task13 的装配与 Task12 的泛型引擎兼容。
- **占位扫描：** 无 TBD/TODO；每个代码步骤含完整可编译代码与具体测试。
- **依赖一致性：** Task1 的 `thiserror` 已声明但本计划主要用 `anyhow`；保留 `thiserror` 供后续自定义错误类型（Spec 要求），不影响编译。

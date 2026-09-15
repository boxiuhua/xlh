# 量化交易 · 计划 1/4:A 股成交模型(ExecutionModel)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为回测引擎引入可插拔成交模型,新增 A 股成交口径(开盘价 + 滑点、整手、涨跌停、T+1),基金回测结果保持不变,股票单股回测切换到 A 股口径并在页面展示未成交订单。

**Architecture:** 新增 `src/execution.rs` 定义 `ExecutionModel` trait 与保持旧行为的 `CloseExecution`;`Engine` 默认使用 `CloseExecution`,通过 `with_execution` 注入其他模型。`DataHandler` 增加默认返回 `None` 的 `exec_bar()`,只有 `StockData` 提供原始 OHLC 与前收。A 股规则以纯函数形式放在 `src/stock/ashare.rs`,供本计划的 `AShareExecution` 与计划 2 的交易闸门共用。

**Tech Stack:** Rust(edition 同现有 crate)、chrono、serde;测试为 `#[cfg(test)]` 单元测试。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md`(本计划实现 §9 与 §13 第一条;§14 任务 1)

## Global Constraints

- 基金回测结果**逐位不变**:`Engine::new` 行为不变,`Broker::execute` 的 `OrderQty::Cash` 买入路径不得改变任何计算
- 策略上下文契约不变:`history` 截止 T-1,当日价格不暴露给策略
- CI 三件套必须通过:`cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`
- 基线:改动前 `cargo test --lib` 为 **502 passed, 0 failed, 8 ignored**,改动后除新增测试外数量与结果不变
- A 股规则数值(照抄 spec §9):滑点默认 0.1%;整手 100(`688`/`689` 最少 200、步长 1;北交所最少 100、步长 1);涨跌幅 主板 10%、ST 5%、`300`/`301`/`688`/`689` 20%、北交所 30%;涨跌停以**不复权**价判断;费用沿用 `StockFee::a_share()`
- 不引入新依赖

### 相对 spec 的实现细化(执行者照此实现,完成后向用户说明)

1. **首根 bar 前收**:回测数据会裁剪到用户区间,首根 bar 不是上市日;调用方需通过 `StockData::with_prev_bar` 提供区间前一交易日 bar,否则首根 bar 订单以 `NoPrevClose` 拒绝。新股前 5 日判断留给计划 3。
2. **停牌**:腾讯/东财日 K 不含停牌日,「无 bar 不成交」由数据天然满足,不额外判断 volume(现有测试数据 volume 均为 0,按 volume 判断会误伤)。
3. **ST**:回测入口拿不到股票名称,`name` 传 `None` 视为非 ST;交易闸门(计划 2)有名称时传入。
4. **卖出零股**:只有「卖出全部可卖份额」时允许零股;部分卖出按步长向下取整。
5. **股票推荐(`stock/recommend.rs`)暂保持 `CloseExecution`**:其候选策略固定每次 1000 元,A 股整手下高价股一手都买不起,直接切换会使推荐失效。迁移留给计划 3(策略准入的 walk-forward 使用 A 股口径)。
6. **除权除息日**:涨跌停基准用除权参考价 `prev_adj_close × close / adj_close`(后复权数据推导),无复权前收时回退不复权前收。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/broker.rs` | 修改 | 支持按股数买入;暴露 `buy_fee`;新增 `sellable_shares`(T+1) |
| `src/execution.rs` | 新建 | `ExecBar`、`Prepared`、`RejectReason`、`RejectedOrder`、`ExecutionModel`、`CloseExecution` |
| `src/lib.rs` | 修改 | `pub mod execution;` |
| `src/data/mod.rs` | 修改 | `DataHandler::exec_bar()` 默认 `None` |
| `src/engine.rs` | 修改 | 持有 `Box<dyn ExecutionModel>`;`with_execution`;记录 `rejected` |
| `src/stock/ashare.rs` | 新建 | A 股规则纯函数 + `AShareExecution` + `execution_for_market` |
| `src/stock/mod.rs` | 修改 | `pub mod ashare;` |
| `src/stock/data/mod.rs` | 修改 | `StockData` 保存原始 bar 并实现 `exec_bar()` |
| `src/stock/backtest.rs` | 修改 | `run_one` 接收成交模型;输出 `execution`、`rejected` |
| `src/stock/recommend.rs` | 修改 | 显式传 `CloseExecution` |
| `src/web/stock.rs` | 修改 | 按市场选择成交模型 |
| `src/web/page.rs` | 修改 | 回测结果展示成交口径与未成交订单 |

---

### Task 1: Broker 支持按股数买入与 T+1 可卖份额

**Files:**
- Modify: `src/broker.rs:64-153`(`impl Broker`)
- Test: `src/broker.rs` 内 `mod tests`

**Interfaces:**
- Consumes: 无
- Produces:
  - `Broker::buy_fee(&self, cash: f64) -> f64`
  - `Broker::sellable_shares(&self, date: NaiveDate) -> f64` —— 严格早于 `date` 买入的份额之和
  - `Broker::execute` 接受 `Direction::Buy` + `OrderQty::Shares(s)`:成交 `s` 份,费用 `buy_fee(s * price)`

- [ ] **Step 1: 写失败测试**

在 `src/broker.rs` 的 `mod tests` 末尾(最后一个 `}` 之前)追加:

```rust
    #[test]
    fn buy_by_shares_charges_fee_on_value() {
        let mut b = Broker::new(crate::stock::fee::StockFee::a_share());
        let fill = b.execute(
            &OrderEvent {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                qty: OrderQty::Shares(900.0),
            },
            10.0,
        );
        assert!((fill.shares - 900.0).abs() < 1e-9);
        // 市值 9000:佣金 2.25<5 取 5;过户 0.09 → 5.09
        assert!((fill.fee - 5.09).abs() < 1e-9, "fee={}", fill.fee);
        assert!((b.total_shares() - 900.0).abs() < 1e-9);
    }

    #[test]
    fn sellable_shares_excludes_same_day_lots() {
        let mut b = Broker::new(fee_model());
        for day in [2, 3] {
            b.execute(
                &OrderEvent {
                    date: d(2024, 1, day),
                    direction: Direction::Buy,
                    qty: OrderQty::Cash(1000.0),
                },
                1.0,
            );
        }
        // 每笔:费 1.5,份额 998.5
        assert!(b.sellable_shares(d(2024, 1, 2)).abs() < 1e-9, "当日买入不可卖");
        assert!((b.sellable_shares(d(2024, 1, 3)) - 998.5).abs() < 1e-9);
        assert!((b.sellable_shares(d(2024, 1, 4)) - 1997.0).abs() < 1e-9);
    }

    #[test]
    fn buy_fee_is_exposed() {
        let b = Broker::new(crate::stock::fee::StockFee::a_share());
        assert!((b.buy_fee(1000.0) - 5.01).abs() < 1e-9);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib broker::tests`
Expected: 编译失败,`no method named sellable_shares` / `no method named buy_fee`

- [ ] **Step 3: 实现**

在 `impl Broker` 中 `position()` 之后添加:

```rust
    /// 买入费用(按成交金额)。成交模型据此做「预算内最多买几手」。
    pub fn buy_fee(&self, cash: f64) -> f64 {
        self.fee.buy_fee(cash)
    }

    /// 在 `date` 可卖出的份额:只含**严格早于** `date` 买入的 lot(A 股 T+1)。
    pub fn sellable_shares(&self, date: NaiveDate) -> f64 {
        self.lots
            .iter()
            .filter(|l| l.date < date)
            .map(|l| l.shares)
            .sum()
    }
```

将 `execute` 的 `Direction::Buy` 分支整体替换为(`Cash` 路径与原实现计算完全一致):

```rust
            Direction::Buy => {
                let (shares, fee) = match order.qty {
                    OrderQty::Cash(cash) => {
                        let fee = self.fee.buy_fee(cash);
                        let shares = if price > 0.0 {
                            (cash - fee) / price
                        } else {
                            0.0
                        };
                        (shares, fee)
                    }
                    // A 股成交模型已按整手算好股数,费用按成交金额计。
                    OrderQty::Shares(s) if s > 0.0 && price > 0.0 => {
                        (s, self.fee.buy_fee(s * price))
                    }
                    // 与旧实现一致:非 Cash 买单视作 0 元买入。
                    _ => (0.0, self.fee.buy_fee(0.0)),
                };
                if shares > 0.0 {
                    self.lots.push(Lot {
                        date: order.date,
                        shares,
                        cost: price,
                    });
                }
                FillEvent {
                    date: order.date,
                    direction: Direction::Buy,
                    shares,
                    price,
                    fee,
                }
            }
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib broker::tests`
Expected: 全部 PASS(含原有 5 个)

- [ ] **Step 5: 全量回归**

Run: `cargo test --lib`
Expected: `505 passed; 0 failed; 8 ignored`

- [ ] **Step 6: Commit**

```bash
git add src/broker.rs
git commit -m "feat(broker): 支持按股数买入、暴露买入费用、T+1 可卖份额"
```

---

### Task 2: ExecutionModel 抽象与引擎接入(基金行为不变)

**Files:**
- Create: `src/execution.rs`
- Modify: `src/lib.rs`(在 `pub mod event;` 后加 `pub mod execution;`)
- Modify: `src/data/mod.rs:40-51`(`trait DataHandler`)
- Modify: `src/engine.rs:1-117`
- Test: `src/execution.rs`、`src/engine.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 1 的 `Broker`(本任务只传引用)
- Produces:

```rust
// src/execution.rs
pub struct ExecBar { pub open: f64, pub close: f64, pub adj_close: f64, pub prev_close: Option<f64> } // Copy
pub struct Prepared { pub order: OrderEvent, pub price: f64 }
pub enum RejectReason { LimitUp, LimitDown, NoPrevClose, BelowOneLot, NothingSellable, NoPrice, UnsupportedQty } // Copy, Serialize snake_case
pub struct RejectedOrder { pub date: NaiveDate, pub direction: Direction, pub reason: RejectReason } // Serialize
pub trait ExecutionModel {
    fn name(&self) -> &'static str;
    fn prepare(&self, order: &OrderEvent, today: &MarketEvent, bar: Option<&ExecBar>, broker: &Broker) -> Result<Prepared, RejectReason>;
}
pub struct CloseExecution; // name() == "close"
// src/data/mod.rs
trait DataHandler { fn exec_bar(&self) -> Option<ExecBar> { None } }
// src/engine.rs
impl Engine { pub fn with_execution(self, exec: Box<dyn ExecutionModel>) -> Self; pub fn rejected(&self) -> &[RejectedOrder]; pub fn execution_name(&self) -> &'static str; }
```

- [ ] **Step 1: 写失败测试(execution.rs)**

创建 `src/execution.rs`,写入以下完整内容(纯类型定义 + `CloseExecution` + 测试;在 Step 2 注册模块前不会被编译):

```rust
//! 成交模型:把引擎产生的订单变成「以什么价格、成交多少」,或说明为什么不能成交。
//!
//! 基金沿用 `CloseExecution`(按当日复权净值成交,与历史行为逐位一致);
//! A 股使用 `stock::ashare::AShareExecution`(开盘价 + 滑点、整手、涨跌停、T+1)。

use crate::broker::Broker;
use crate::event::{Direction, MarketEvent, OrderEvent};
use chrono::NaiveDate;
use serde::Serialize;

/// 成交模型需要、但**策略不可见**的当日原始行情。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExecBar {
    /// 不复权开盘价
    pub open: f64,
    /// 不复权收盘价
    pub close: f64,
    /// 复权收盘价(与 `MarketEvent::adj_nav` 同尺度)
    pub adj_close: f64,
    /// 前一交易日不复权收盘价;数据首根 bar 为 None
    pub prev_close: Option<f64>,
}

/// 可交给 `Broker::execute` 的订单与成交价(复权尺度)。
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub order: OrderEvent,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// 开盘即涨停,买不进
    LimitUp,
    /// 开盘即跌停,卖不出
    LimitDown,
    /// 无前收,无法计算涨跌停价
    NoPrevClose,
    /// 预算不足一手 / 部分卖出不足一个步长
    BelowOneLot,
    /// 无可卖份额(含 T+1 限制)
    NothingSellable,
    /// 无有效报价
    NoPrice,
    /// 该成交模型不支持的数量类型
    UnsupportedQty,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedOrder {
    pub date: NaiveDate,
    pub direction: Direction,
    pub reason: RejectReason,
}

pub trait ExecutionModel {
    /// 口径名称,输出到回测结果供页面展示。
    fn name(&self) -> &'static str;

    fn prepare(
        &self,
        order: &OrderEvent,
        today: &MarketEvent,
        bar: Option<&ExecBar>,
        broker: &Broker,
    ) -> Result<Prepared, RejectReason>;
}

/// 历史行为:订单原样、按当日复权价成交。
pub struct CloseExecution;

impl ExecutionModel for CloseExecution {
    fn name(&self) -> &'static str {
        "close"
    }

    fn prepare(
        &self,
        order: &OrderEvent,
        today: &MarketEvent,
        _bar: Option<&ExecBar>,
        _broker: &Broker,
    ) -> Result<Prepared, RejectReason> {
        Ok(Prepared {
            order: order.clone(),
            price: today.adj_nav,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::OrderQty;
    use crate::stock::fee::StockFee;

    #[test]
    fn close_execution_passes_order_through_at_adj_nav() {
        let date = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let order = OrderEvent {
            date,
            direction: Direction::Buy,
            qty: OrderQty::Cash(1000.0),
        };
        let today = MarketEvent {
            date,
            nav: 1.0,
            adj_nav: 1.23,
        };
        let broker = Broker::new(StockFee::a_share());
        let p = CloseExecution
            .prepare(&order, &today, None, &broker)
            .expect("CloseExecution 从不拒绝");
        assert_eq!(p.order, order);
        assert!((p.price - 1.23).abs() < 1e-12);
        assert_eq!(CloseExecution.name(), "close");
    }

    #[test]
    fn reject_reason_serializes_snake_case() {
        let j = serde_json::to_string(&RejectReason::BelowOneLot).unwrap();
        assert_eq!(j, "\"below_one_lot\"");
    }
}
```

- [ ] **Step 2: 注册模块并运行确认测试可编译运行**

在 `src/lib.rs` 的 `pub mod event;` 下一行加入 `pub mod execution;`。

Run: `cargo test --lib execution::tests`
Expected: 2 PASS(此步验证类型定义正确;引擎尚未接入)

- [ ] **Step 3: 写引擎失败测试**

在 `src/engine.rs` 的 `mod tests` 末尾追加:

```rust
    /// 注入 CloseExecution 必须与默认构造逐位一致 —— 基金回测结果不变的护栏。
    #[test]
    fn explicit_close_execution_is_identical_to_default() {
        use crate::execution::CloseExecution;
        let pts = || {
            (0..60)
                .map(|i| {
                    let v = 1.0 + (i as f64 * 0.37).sin() * 0.2;
                    NavPoint {
                        date: d(2024, 1, 1) + chrono::Duration::days(i),
                        nav: v,
                        acc_nav: v,
                    }
                })
                .collect::<Vec<_>>()
        };
        let strat = || {
            RuleLayer::new(
                Box::new(Dca::new(Period::Weekly, 1, 1000.0)),
                vec![Rule::TakeProfit { target_return: 0.1 }],
            )
        };
        let mut a = Engine::new(
            InMemoryData::new(pts()),
            strat(),
            Broker::new(no_fee()),
            Portfolio::new(0.0),
        );
        a.run();
        let mut b = Engine::new(
            InMemoryData::new(pts()),
            strat(),
            Broker::new(no_fee()),
            Portfolio::new(0.0),
        )
        .with_execution(Box::new(CloseExecution));
        b.run();
        assert_eq!(format!("{:?}", a.daily()), format!("{:?}", b.daily()));
        assert_eq!(format!("{:?}", a.trades()), format!("{:?}", b.trades()));
        assert!(a.rejected().is_empty() && b.rejected().is_empty());
        assert_eq!(a.execution_name(), "close");
    }
```

- [ ] **Step 4: 运行确认失败**

Run: `cargo test --lib engine::tests::explicit_close_execution_is_identical_to_default`
Expected: 编译失败,`no method named with_execution`

- [ ] **Step 5: DataHandler 增加 exec_bar**

`src/data/mod.rs`:在 `use crate::event::MarketEvent;` 下加 `use crate::execution::ExecBar;`,并在 `trait DataHandler` 的 `history` 声明之后加入:

```rust

    /// 当日原始行情,**只给成交模型用,不进入策略上下文**。
    /// 基金数据没有开盘价与涨跌停概念,默认 None。
    fn exec_bar(&self) -> Option<ExecBar> {
        None
    }
```

- [ ] **Step 6: 引擎接入**

`src/engine.rs` 修改:

1. 顶部 use 增加:
```rust
use crate::execution::{CloseExecution, ExecutionModel, RejectedOrder};
```

2. 结构体增加字段(放在 `trades` 之后):
```rust
    exec: Box<dyn ExecutionModel>,
    rejected: Vec<RejectedOrder>,
```

3. `new` 中初始化(放在 `trades: Vec::new(),` 之后):
```rust
            exec: Box::new(CloseExecution),
            rejected: Vec::new(),
```

4. `new` 之后新增:
```rust
    /// 替换成交模型;默认 `CloseExecution`(历史行为)。
    pub fn with_execution(mut self, exec: Box<dyn ExecutionModel>) -> Self {
        self.exec = exec;
        self
    }
```

5. 将 `Event::Order(o) => { ... }` 分支替换为:
```rust
                    Event::Order(o) => {
                        let bar = self.data.exec_bar();
                        match self.exec.prepare(&o, &today, bar.as_ref(), &self.broker) {
                            Ok(p) => {
                                let fill = self.broker.execute(&p.order, p.price);
                                queue.push_back(Event::Fill(fill));
                            }
                            Err(reason) => self.rejected.push(RejectedOrder {
                                date: o.date,
                                direction: o.direction,
                                reason,
                            }),
                        }
                    }
```

6. `trades()` 之后新增:
```rust
    /// 因成交规则被拒绝的订单(涨跌停、不足一手、T+1 等)。
    pub fn rejected(&self) -> &[RejectedOrder] {
        &self.rejected
    }

    pub fn execution_name(&self) -> &'static str {
        self.exec.name()
    }
```

- [ ] **Step 7: 运行确认通过 + 全量回归**

Run: `cargo test --lib engine::tests`
Expected: 全部 PASS

Run: `cargo test --lib`
Expected: `508 passed; 0 failed; 8 ignored`(502 + Task1 的 3 + 本任务 3)

- [ ] **Step 8: Commit**

```bash
git add src/execution.rs src/lib.rs src/data/mod.rs src/engine.rs
git commit -m "feat(engine): 可插拔成交模型 ExecutionModel,默认 CloseExecution 保持基金行为"
```

---

### Task 3: A 股交易规则纯函数

**Files:**
- Create: `src/stock/ashare.rs`
- Modify: `src/stock/mod.rs`(按字母序在 `pub mod attribution;` 前加 `pub mod ashare;`)
- Test: `src/stock/ashare.rs` 内 `mod tests`

**Interfaces:**
- Consumes: 无
- Produces(计划 2 的 `trade/rules.rs` 直接调用):

```rust
pub fn limit_ratio(code: &str, name: Option<&str>) -> f64;
pub fn price_decimals(code: &str) -> i32;
pub fn limit_prices(prev_close: f64, ratio: f64, decimals: i32) -> (f64, f64); // (涨停, 跌停)
pub struct BuyLot { pub min: u64, pub step: u64 } // Copy, Eq
pub fn buy_lot(code: &str) -> BuyLot;
pub fn round_buy_shares(raw_shares: f64, lot: BuyLot) -> u64;
pub fn step_down(n: u64, lot: BuyLot) -> u64;
```

- [ ] **Step 1: 写失败测试**

创建 `src/stock/ashare.rs`,内容仅为测试模块:

```rust
//! A 股交易规则(纯函数)与 A 股成交模型。
//! 规则只在此处定义一次:回测成交模型与交易闸门共用。

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn limit_ratio_by_board_and_st() {
        assert!(close(limit_ratio("600000", None), 0.10));
        assert!(close(limit_ratio("000001", Some("ST 某某")), 0.05));
        assert!(close(limit_ratio("600001", Some("*st某某")), 0.05));
        assert!(close(limit_ratio("300750", Some("ST 某某")), 0.20), "创业板 ST 仍 20%");
        assert!(close(limit_ratio("301001", None), 0.20));
        assert!(close(limit_ratio("688981", None), 0.20));
        assert!(close(limit_ratio("830799", None), 0.30));
        assert!(close(limit_ratio("430047", None), 0.30));
    }

    #[test]
    fn limit_prices_round_to_tick() {
        let (up, down) = limit_prices(10.0, 0.10, 2);
        assert!(close(up, 11.0) && close(down, 9.0));
        let (up, down) = limit_prices(3.33, 0.10, 2);
        assert!(close(up, 3.66) && close(down, 3.00), "up={up} down={down}");
        assert_eq!(price_decimals("510300"), 3);
        assert_eq!(price_decimals("600000"), 2);
        let (up, down) = limit_prices(3.456, 0.10, 3);
        assert!(close(up, 3.802) && close(down, 3.110), "up={up} down={down}");
    }

    #[test]
    fn buy_lot_by_board() {
        assert_eq!(buy_lot("600000"), BuyLot { min: 100, step: 100 });
        assert_eq!(buy_lot("510300"), BuyLot { min: 100, step: 100 });
        assert_eq!(buy_lot("688001"), BuyLot { min: 200, step: 1 });
        assert_eq!(buy_lot("830799"), BuyLot { min: 100, step: 1 });
    }

    #[test]
    fn round_buy_shares_floors_to_lot() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(round_buy_shares(999.0, main), 900);
        assert_eq!(round_buy_shares(99.9, main), 0);
        assert_eq!(round_buy_shares(100.0, main), 100);
        assert_eq!(round_buy_shares(299.5, star), 299);
        assert_eq!(round_buy_shares(150.0, star), 0);
        assert_eq!(round_buy_shares(f64::NAN, main), 0);
    }

    #[test]
    fn step_down_stops_at_min() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(step_down(900, main), 800);
        assert_eq!(step_down(100, main), 0);
        assert_eq!(step_down(201, star), 200);
        assert_eq!(step_down(200, star), 0);
    }
}
```

在 `src/stock/mod.rs` 加入 `pub mod ashare;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::ashare::tests`
Expected: 编译失败,`cannot find function limit_ratio`

- [ ] **Step 3: 实现**

在 `src/stock/ashare.rs` 的模块文档注释之后、`#[cfg(test)]` 之前插入:

```rust
fn is_star(code: &str) -> bool {
    code.starts_with("688") || code.starts_with("689")
}

fn is_chinext(code: &str) -> bool {
    code.starts_with("300") || code.starts_with("301")
}

fn is_bse(code: &str) -> bool {
    code.starts_with('8') || code.starts_with("43") || code.starts_with("92")
}

fn is_etf(code: &str) -> bool {
    ["51", "52", "56", "58", "15", "16", "18"]
        .iter()
        .any(|p| code.starts_with(p))
}

/// 涨跌幅比例。`name` 未知时传 None(视为非 ST)。
/// 创业板/科创板无论是否 ST 均为 20%。
pub fn limit_ratio(code: &str, name: Option<&str>) -> f64 {
    if is_star(code) || is_chinext(code) {
        return 0.20;
    }
    if is_bse(code) {
        return 0.30;
    }
    if name.is_some_and(|n| n.to_uppercase().contains("ST")) {
        0.05
    } else {
        0.10
    }
}

/// 最小价位小数位:场内基金 0.001,股票 0.01。
pub fn price_decimals(code: &str) -> i32 {
    if is_etf(code) {
        3
    } else {
        2
    }
}

fn round_to(x: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (x * m).round() / m
}

/// (涨停价, 跌停价),按最小价位四舍五入。
pub fn limit_prices(prev_close: f64, ratio: f64, decimals: i32) -> (f64, f64) {
    (
        round_to(prev_close * (1.0 + ratio), decimals),
        round_to(prev_close * (1.0 - ratio), decimals),
    )
}

/// 买入数量约束:至少 `min` 股,超出部分按 `step` 递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyLot {
    pub min: u64,
    pub step: u64,
}

pub fn buy_lot(code: &str) -> BuyLot {
    if is_star(code) {
        BuyLot { min: 200, step: 1 }
    } else if is_bse(code) {
        BuyLot { min: 100, step: 1 }
    } else {
        BuyLot {
            min: 100,
            step: 100,
        }
    }
}

/// 把「理论可买股数」向下取整到合法数量;不足最小数量返回 0。
pub fn round_buy_shares(raw_shares: f64, lot: BuyLot) -> u64 {
    if !raw_shares.is_finite() || raw_shares < lot.min as f64 {
        return 0;
    }
    let n = (raw_shares + 1e-6).floor() as u64;
    lot.min + (n - lot.min) / lot.step * lot.step
}

/// 减少一个步长;低于最小数量返回 0。
pub fn step_down(n: u64, lot: BuyLot) -> u64 {
    if n >= lot.min + lot.step {
        n - lot.step
    } else {
        0
    }
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib stock::ashare::tests`
Expected: 5 PASS

- [ ] **Step 5: Commit**

```bash
git add src/stock/ashare.rs src/stock/mod.rs
git commit -m "feat(stock): A 股交易规则纯函数(涨跌幅、最小价位、整手)"
```

---

### Task 4: AShareExecution 与 StockData 原始行情

**Files:**
- Modify: `src/stock/ashare.rs`(追加实现与测试)
- Modify: `src/stock/data/mod.rs:34-71`
- Test: `src/stock/ashare.rs`、`src/stock/data/mod.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 1 `Broker::{buy_fee, sellable_shares}`;Task 2 `ExecBar, Prepared, RejectReason, ExecutionModel, CloseExecution, Engine::with_execution, Engine::rejected`;Task 3 全部规则函数
- Produces:

```rust
pub struct AShareExecution { .. }
impl AShareExecution {
    pub const DEFAULT_SLIPPAGE: f64 = 0.001;
    pub fn new(code: &str, name: Option<&str>, slippage: f64) -> Self;
}
impl ExecutionModel for AShareExecution {} // name() == "a_share"
/// 港股(116)/美股(105..=107) → CloseExecution;其余按 A 股
pub fn execution_for_market(market: u16, code: &str) -> Box<dyn ExecutionModel>;
// StockData 实现 DataHandler::exec_bar()
```

- [ ] **Step 1: 写 StockData 失败测试**

在 `src/stock/data/mod.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn exec_bar_exposes_raw_ohlc_and_prev_close() {
        let mut b1 = bar(d(2024, 1, 1), 10.0, 20.0);
        b1.open = 9.5;
        let mut b2 = bar(d(2024, 1, 2), 11.0, 22.0);
        b2.open = 10.5;
        let mut h = StockData::new(vec![b1, b2]);
        assert!(h.exec_bar().is_none(), "未推进时无当日");
        h.next_bar();
        let e1 = h.exec_bar().unwrap();
        assert!((e1.open - 9.5).abs() < 1e-9);
        assert!(e1.prev_close.is_none(), "首根无前收");
        h.next_bar();
        let e2 = h.exec_bar().unwrap();
        assert!((e2.open - 10.5).abs() < 1e-9);
        assert!((e2.close - 11.0).abs() < 1e-9);
        assert!((e2.adj_close - 22.0).abs() < 1e-9);
        assert_eq!(e2.prev_close, Some(10.0), "前收为不复权 close");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::data::tests::exec_bar_exposes_raw_ohlc_and_prev_close`
Expected: FAIL —— `exec_bar()` 走默认实现返回 None,`unwrap()` panic

- [ ] **Step 3: 实现 StockData::exec_bar**

`src/stock/data/mod.rs`:

1. 顶部 use 增加 `use crate::execution::ExecBar;`
2. `StockData` 结构体改为:
```rust
pub struct StockData {
    bars: Vec<MarketEvent>,
    /// 原始 bar,仅供成交模型(开盘价、前收)使用,不进入策略上下文。
    raw: Vec<StockBar>,
    cursor: usize,
}
```
3. `StockData::new` 改为:
```rust
    pub fn new(raw: Vec<StockBar>) -> Self {
        let bars = raw
            .iter()
            .map(|b| MarketEvent {
                date: b.date,
                nav: b.close,
                adj_nav: b.adj_close,
            })
            .collect();
        Self {
            bars,
            raw,
            cursor: 0,
        }
    }
```
4. `impl DataHandler for StockData` 中 `history` 之后加入:
```rust
    fn exec_bar(&self) -> Option<ExecBar> {
        let i = self.cursor.checked_sub(1)?;
        let b = self.raw.get(i)?;
        Some(ExecBar {
            open: b.open,
            close: b.close,
            adj_close: b.adj_close,
            prev_close: i.checked_sub(1).map(|j| self.raw[j].close),
        })
    }
```

Run: `cargo test --lib stock::data::tests`
Expected: 全部 PASS

- [ ] **Step 4: 写 AShareExecution 失败测试**

在 `src/stock/ashare.rs` 的 `mod tests` 内(`close` 辅助函数之后)追加:

```rust
    use crate::broker::Broker;
    use crate::event::{Direction, MarketEvent, OrderEvent, OrderQty};
    use crate::execution::{ExecBar, ExecutionModel, RejectReason};
    use crate::stock::fee::StockFee;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn today() -> MarketEvent {
        MarketEvent {
            date: d(2024, 1, 3),
            nav: 0.0,
            adj_nav: 0.0,
        }
    }
    fn xbar(open: f64, close: f64, adj_close: f64, prev: Option<f64>) -> ExecBar {
        ExecBar {
            open,
            close,
            adj_close,
            prev_close: prev,
        }
    }
    fn buy(cash: f64) -> OrderEvent {
        OrderEvent {
            date: d(2024, 1, 3),
            direction: Direction::Buy,
            qty: OrderQty::Cash(cash),
        }
    }
    fn sell(qty: OrderQty, date: NaiveDate) -> OrderEvent {
        OrderEvent {
            date,
            direction: Direction::Sell,
            qty,
        }
    }
    /// 1/2 买入 `shares` 股 @10 的持仓。
    fn holding(shares: f64) -> Broker {
        let mut b = Broker::new(StockFee::a_share());
        b.execute(
            &OrderEvent {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                qty: OrderQty::Shares(shares),
            },
            10.0,
        );
        b
    }
    fn shares_of(o: &OrderEvent) -> f64 {
        match o.qty {
            OrderQty::Shares(s) => s,
            other => panic!("应为 Shares,实际 {other:?}"),
        }
    }

    #[test]
    fn buy_fills_at_open_plus_slippage_in_lots() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(&buy(10000.0), &today(), Some(&xbar(10.0, 10.5, 10.5, Some(10.0))), &b)
            .unwrap();
        // 10.01 → 999 股 → 900;9009 + 5.09 ≤ 10000
        assert!(close(p.price, 10.01), "price={}", p.price);
        assert!(close(shares_of(&p.order), 900.0));
        assert_eq!(ex.name(), "a_share");
    }

    #[test]
    fn buy_steps_down_when_fee_exceeds_budget() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let r = ex.prepare(&buy(1005.0), &today(), Some(&xbar(10.0, 10.0, 10.0, Some(10.0))), &b);
        // 100 股 = 1000 + 5.01 费 > 1005
        assert_eq!(r.unwrap_err(), RejectReason::BelowOneLot);
    }

    #[test]
    fn buy_rejected_at_limit_up_by_board() {
        let b = Broker::new(StockFee::a_share());
        let bar = xbar(11.0, 11.0, 11.0, Some(10.0));
        let main = AShareExecution::new("600000", None, 0.001);
        assert_eq!(
            main.prepare(&buy(10000.0), &today(), Some(&bar), &b).unwrap_err(),
            RejectReason::LimitUp
        );
        let star = AShareExecution::new("688001", None, 0.001);
        assert!(star.prepare(&buy(10000.0), &today(), Some(&bar), &b).is_ok(), "科创板 20%");
        let st = AShareExecution::new("600000", Some("*ST某某"), 0.001);
        assert_eq!(
            st.prepare(&buy(10000.0), &today(), Some(&xbar(10.5, 10.5, 10.5, Some(10.0))), &b)
                .unwrap_err(),
            RejectReason::LimitUp
        );
    }

    #[test]
    fn slippage_is_capped_at_limit_up() {
        let ex = AShareExecution::new("600000", None, 0.05);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(&buy(100000.0), &today(), Some(&xbar(10.9, 10.9, 10.9, Some(10.0))), &b)
            .unwrap();
        assert!(close(p.price, 11.0), "price={}", p.price);
    }

    #[test]
    fn buy_converts_lots_into_adjusted_units() {
        // 复权因子 2:不复权 10 元 ↔ 复权 20 元
        let ex = AShareExecution::new("600000", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(&buy(10000.0), &today(), Some(&xbar(10.0, 10.0, 20.0, Some(10.0))), &b)
            .unwrap();
        // 1000 股需 10005.1 > 预算 → 900 股;复权尺度 450 份 @20,市值同为 9000
        assert!(close(p.price, 20.0));
        assert!(close(shares_of(&p.order), 450.0));
    }

    #[test]
    fn star_board_buys_in_single_shares_above_200() {
        let ex = AShareExecution::new("688001", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(&buy(3000.0), &today(), Some(&xbar(10.0, 10.0, 10.0, Some(10.0))), &b)
            .unwrap();
        // 300 股 3000+5.03 超预算 → 299 股 2990+5.0299
        assert!(close(shares_of(&p.order), 299.0));
    }

    #[test]
    fn sell_respects_t_plus_one() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let mut b = Broker::new(StockFee::a_share());
        b.execute(&buy_shares_on(d(2024, 1, 3), 1000.0), 10.0);
        let bar = xbar(10.0, 10.0, 10.0, Some(10.0));
        assert_eq!(
            ex.prepare(&sell(OrderQty::AllShares, d(2024, 1, 3)), &today(), Some(&bar), &b)
                .unwrap_err(),
            RejectReason::NothingSellable
        );
        let p = ex
            .prepare(&sell(OrderQty::AllShares, d(2024, 1, 4)), &today(), Some(&bar), &b)
            .unwrap();
        assert!(close(shares_of(&p.order), 1000.0));
        assert!(close(p.price, 9.99));
    }

    fn buy_shares_on(date: NaiveDate, shares: f64) -> OrderEvent {
        OrderEvent {
            date,
            direction: Direction::Buy,
            qty: OrderQty::Shares(shares),
        }
    }

    #[test]
    fn sell_rejected_at_limit_down() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = holding(1000.0);
        let r = ex.prepare(
            &sell(OrderQty::AllShares, d(2024, 1, 3)),
            &today(),
            Some(&xbar(9.0, 9.0, 9.0, Some(10.0))),
            &b,
        );
        assert_eq!(r.unwrap_err(), RejectReason::LimitDown);
    }

    #[test]
    fn partial_sell_floors_to_step_but_full_exit_allows_odd_lot() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let bar = xbar(10.0, 10.0, 10.0, Some(10.0));
        let b = holding(1050.0);
        let p = ex
            .prepare(&sell(OrderQty::Shares(250.0), d(2024, 1, 3)), &today(), Some(&bar), &b)
            .unwrap();
        assert!(close(shares_of(&p.order), 200.0));
        assert_eq!(
            ex.prepare(&sell(OrderQty::Shares(50.0), d(2024, 1, 3)), &today(), Some(&bar), &b)
                .unwrap_err(),
            RejectReason::BelowOneLot
        );
        let all = ex
            .prepare(&sell(OrderQty::AllShares, d(2024, 1, 3)), &today(), Some(&bar), &b)
            .unwrap();
        assert!(close(shares_of(&all.order), 1050.0), "清仓允许零股");
    }

    #[test]
    fn missing_prev_close_or_bar_is_rejected() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = Broker::new(StockFee::a_share());
        assert_eq!(
            ex.prepare(&buy(10000.0), &today(), Some(&xbar(10.0, 10.0, 10.0, None)), &b)
                .unwrap_err(),
            RejectReason::NoPrevClose
        );
        assert_eq!(
            ex.prepare(&buy(10000.0), &today(), None, &b).unwrap_err(),
            RejectReason::NoPrice
        );
    }

    #[test]
    fn execution_for_market_picks_model() {
        assert_eq!(execution_for_market(1, "600000").name(), "a_share");
        assert_eq!(execution_for_market(0, "000001").name(), "a_share");
        assert_eq!(execution_for_market(116, "00700").name(), "close");
        assert_eq!(execution_for_market(105, "AAPL").name(), "close");
    }

    /// 端到端:经引擎运行,记录拒单原因并按开盘价成交。
    #[test]
    fn engine_records_rejections_and_fills_at_open() {
        use crate::engine::Engine;
        use crate::portfolio::Portfolio;
        use crate::stock::data::{StockBar, StockData};
        use crate::strategy::dca::Dca;
        use crate::strategy::Period;
        let sb = |date: NaiveDate, open: f64, close: f64| StockBar {
            date,
            open,
            high: open.max(close),
            low: open.min(close),
            close,
            volume: 1.0,
            adj_close: close,
        };
        let bars = vec![
            sb(d(2024, 1, 1), 10.0, 10.0), // 定投日,首根无前收 → 拒
            sb(d(2024, 2, 1), 11.0, 11.0), // 定投日,开盘涨停 → 拒
            sb(d(2024, 3, 1), 11.0, 11.0), // 定投日,正常成交
        ];
        let mut e = Engine::new(
            StockData::new(bars),
            Dca::new(Period::Monthly, 1, 10000.0),
            Broker::new(StockFee::a_share()),
            Portfolio::new(0.0),
        )
        .with_execution(Box::new(AShareExecution::new("600000", None, 0.001)));
        e.run();
        let reasons: Vec<_> = e.rejected().iter().map(|r| r.reason).collect();
        assert_eq!(reasons, vec![RejectReason::NoPrevClose, RejectReason::LimitUp]);
        assert_eq!(e.trades().len(), 1);
        let t = &e.trades()[0];
        assert_eq!(t.date, d(2024, 3, 1));
        assert!(close(t.shares, 900.0), "shares={}", t.shares);
        assert!(close(t.price, 11.011), "price={}", t.price);
    }
```

注意:`holding` 与 `partial_sell…` 测试中持仓在 1/2 买入,卖出日 1/3,满足 T+1。

- [ ] **Step 5: 运行确认失败**

Run: `cargo test --lib stock::ashare::tests`
Expected: 编译失败,`cannot find type AShareExecution`

- [ ] **Step 6: 实现 AShareExecution**

先在 `src/stock/ashare.rs` 顶部模块文档注释(`//!` 两行)之后加入:

```rust
use crate::broker::Broker;
use crate::event::{Direction, MarketEvent, OrderEvent, OrderQty};
use crate::execution::{CloseExecution, ExecBar, ExecutionModel, Prepared, RejectReason};
```

再在 `step_down` 函数之后、`#[cfg(test)]` 之前插入:

```rust
/// A 股成交口径:T 日开盘价 ± 滑点(不越过涨跌停价)、整手、开盘涨停不买/跌停不卖、T+1。
///
/// 涨跌停与整手按**不复权**价计算;交给 Broker 的价格与份额换算回复权尺度,
/// 保证「份额 × 价格」等于真实成交金额。
pub struct AShareExecution {
    ratio: f64,
    decimals: i32,
    lot: BuyLot,
    slippage: f64,
}

impl AShareExecution {
    pub const DEFAULT_SLIPPAGE: f64 = 0.001;

    pub fn new(code: &str, name: Option<&str>, slippage: f64) -> Self {
        Self {
            ratio: limit_ratio(code, name),
            decimals: price_decimals(code),
            lot: buy_lot(code),
            slippage,
        }
    }
}

impl ExecutionModel for AShareExecution {
    fn name(&self) -> &'static str {
        "a_share"
    }

    fn prepare(
        &self,
        order: &OrderEvent,
        _today: &MarketEvent,
        bar: Option<&ExecBar>,
        broker: &Broker,
    ) -> Result<Prepared, RejectReason> {
        let bar = bar.ok_or(RejectReason::NoPrice)?;
        if bar.open <= 0.0 || bar.close <= 0.0 || bar.adj_close <= 0.0 {
            return Err(RejectReason::NoPrice);
        }
        let prev = bar.prev_close.ok_or(RejectReason::NoPrevClose)?;
        let (up, down) = limit_prices(prev, self.ratio, self.decimals);
        let eps = 0.5 * 10f64.powi(-self.decimals);
        // 复权因子:复权尺度 = 不复权 × factor
        let factor = bar.adj_close / bar.close;

        match order.direction {
            Direction::Buy => {
                if bar.open >= up - eps {
                    return Err(RejectReason::LimitUp);
                }
                let OrderQty::Cash(budget) = order.qty else {
                    return Err(RejectReason::UnsupportedQty);
                };
                let raw_price = (bar.open * (1.0 + self.slippage)).min(up);
                let mut n = round_buy_shares(budget / raw_price, self.lot);
                while n > 0 {
                    let value = n as f64 * raw_price;
                    if value + broker.buy_fee(value) <= budget + 1e-9 {
                        break;
                    }
                    n = step_down(n, self.lot);
                }
                if n == 0 {
                    return Err(RejectReason::BelowOneLot);
                }
                Ok(Prepared {
                    order: OrderEvent {
                        date: order.date,
                        direction: Direction::Buy,
                        qty: OrderQty::Shares(n as f64 / factor),
                    },
                    price: raw_price * factor,
                })
            }
            Direction::Sell => {
                if bar.open <= down + eps {
                    return Err(RejectReason::LimitDown);
                }
                let sellable = broker.sellable_shares(order.date);
                if sellable <= 1e-9 {
                    return Err(RejectReason::NothingSellable);
                }
                let want = match order.qty {
                    OrderQty::AllShares => sellable,
                    OrderQty::Shares(s) => s.min(sellable),
                    OrderQty::Cash(_) => return Err(RejectReason::UnsupportedQty),
                };
                let shares = if want >= sellable - 1e-9 {
                    // 清掉全部可卖份额:允许零股
                    sellable
                } else {
                    let raw_n = ((want * factor + 1e-6).floor() as u64) / self.lot.step
                        * self.lot.step;
                    if raw_n == 0 {
                        return Err(RejectReason::BelowOneLot);
                    }
                    raw_n as f64 / factor
                };
                let raw_price = (bar.open * (1.0 - self.slippage)).max(down);
                Ok(Prepared {
                    order: OrderEvent {
                        date: order.date,
                        direction: Direction::Sell,
                        qty: OrderQty::Shares(shares),
                    },
                    price: raw_price * factor,
                })
            }
        }
    }
}

/// 按市场选择成交口径:港股(116)、美股(105..=107)沿用收盘成交,其余按 A 股规则。
pub fn execution_for_market(market: u16, code: &str) -> Box<dyn ExecutionModel> {
    match market {
        116 | 105..=107 => Box::new(CloseExecution),
        _ => Box::new(AShareExecution::new(
            code,
            None,
            AShareExecution::DEFAULT_SLIPPAGE,
        )),
    }
}
```

- [ ] **Step 7: 运行确认通过**

Run: `cargo test --lib stock::ashare::tests`
Expected: 17 PASS(Task 3 的 5 个 + 本任务 12 个)

- [ ] **Step 8: 全量回归 + clippy**

Run: `cargo test --lib`
Expected: `521 passed; 0 failed; 8 ignored`(508 + StockData 1 + ashare 12)

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无警告

- [ ] **Step 9: Commit**

```bash
git add src/stock/ashare.rs src/stock/data/mod.rs
git commit -m "feat(stock): AShareExecution 成交模型(开盘价+滑点、整手、涨跌停、T+1)"
```

---

### Task 5: 股票单股回测切换 A 股口径并展示未成交订单

**Files:**
- Modify: `src/stock/backtest.rs:1-155`
- Modify: `src/stock/recommend.rs:165-176`(`run_metrics`)
- Modify: `src/web/stock.rs:167-203`(`run_blocking`)
- Modify: `src/web/page.rs:1212-1222`(`renderStockRun`)
- Test: `src/stock/backtest.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `ExecutionModel, RejectedOrder, CloseExecution, Engine::{with_execution, rejected, execution_name}`;Task 4 `execution_for_market`
- Produces:

```rust
pub struct StockRunOutcome { /* 原字段 */, pub execution: String, pub rejected: Vec<RejectedOrder> }
pub fn run_one(name: String, code: String, bars: Vec<StockBar>, strategy: Box<dyn Strategy>, fee: StockFee, initial_cash: f64, exec: Box<dyn ExecutionModel>) -> StockRunOutcome;
```

JSON 新增字段:`"execution": "a_share" | "close"`,`"rejected": [{"date":"2024-02-01","direction":"buy","reason":"limit_up"}]`

- [ ] **Step 1: 修改测试(先失败)**

`src/stock/backtest.rs` 的 `mod tests`:

1. 在 `use` 区加入 `use crate::execution::CloseExecution;`
2. 三个现有测试中的每个 `run_one(...)` 调用在最后一个参数(`0.0,`)后追加一行 `Box::new(CloseExecution),`
3. `outcome_serializes_to_json` 的 key 列表追加 `"\"execution\"",` 与 `"\"rejected\"",`
4. 追加新测试:

```rust
    #[test]
    fn a_share_execution_is_applied_and_reported() {
        use crate::stock::ashare::AShareExecution;
        // bar() 的 open == close
        let bars = vec![
            bar(d(2024, 1, 1), 10.0),
            bar(d(2024, 2, 1), 11.0), // 相对前收 10 开盘涨停
            bar(d(2024, 3, 1), 11.0),
        ];
        let out = run_one(
            "t".into(),
            "600519".into(),
            bars,
            Box::new(Dca::new(Period::Monthly, 1, 10000.0)),
            StockFee::a_share(),
            0.0,
            Box::new(AShareExecution::new("600519", None, 0.001)),
        );
        assert_eq!(out.execution, "a_share");
        assert_eq!(out.rejected.len(), 2);
        assert_eq!(out.trades.len(), 1);
        let j = serde_json::to_string(&out).unwrap();
        assert!(j.contains("\"limit_up\""), "拒单原因应序列化: {j}");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::backtest::tests`
Expected: 编译失败,`this function takes 6 arguments but 7 arguments were supplied`

- [ ] **Step 3: 实现 backtest.rs**

1. use 区加入 `use crate::execution::{ExecutionModel, RejectedOrder};`
2. `StockRunOutcome` 在 `trades` 字段后加入:
```rust
    /// 成交口径:"a_share" | "close"
    pub execution: String,
    /// 因成交规则未能成交的订单
    pub rejected: Vec<RejectedOrder>,
```
3. `run_one` 签名在 `initial_cash: f64,` 后加入 `exec: Box<dyn ExecutionModel>,`,函数体改为:
```rust
    let data = StockData::new(bars);
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio).with_execution(exec);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let stats = trade_stats::trade_stats(engine.trades());
    StockRunOutcome {
        name,
        code,
        summary,
        trade_stats: stats,
        daily: engine.daily().to_vec(),
        trades: engine.trades().to_vec(),
        execution: engine.execution_name().to_string(),
        rejected: engine.rejected().to_vec(),
    }
```
4. 更新 `run_one` 文档注释为:`/// 装配引擎跑单股回测:StockData + 复用策略 + StockFee + Portfolio + 成交模型。`

- [ ] **Step 4: 更新调用方**

`src/stock/recommend.rs` 的 `run_metrics`:

```rust
fn run_metrics(kind: &str, bars: &[StockBar], fee: StockFee) -> Summary {
    let strat = candidate(kind);
    // 候选策略固定每次 1000 元,A 股整手规则下高价股一手都买不起,
    // 暂保持收盘成交口径;迁移到 A 股口径见量化交易设计 §10(策略准入)。
    backtest::run_one(
        kind.to_string(),
        String::new(),
        bars.to_vec(),
        strat,
        fee,
        0.0,
        Box::new(crate::execution::CloseExecution),
    )
    .summary
}
```

`src/web/stock.rs` 的 `run_blocking`,将末尾 `Ok(backtest::run_one(...))` 替换为:

```rust
    let exec = crate::stock::ashare::execution_for_market(secid.market, &q.code);
    Ok(backtest::run_one(
        q.strategy.clone(),
        q.code.clone(),
        bars,
        strategy,
        fee,
        q.initial_cash,
        exec,
    ))
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::backtest::tests`
Expected: 4 PASS

Run: `cargo build`
Expected: 编译成功(若有其他 `run_one` 调用方报错,按 Step 4 同样方式补参数:A 股场景用 `execution_for_market`,无市场信息的内部评估用 `CloseExecution` 并加同样注释)

- [ ] **Step 6: 页面展示**

`src/web/page.rs` 的 `renderStockRun` 中,把 `box.innerHTML = ...` 整段替换为:

```js
  var rj = o.rejected || [];
  var rjNames = {limit_up:'涨停买不进', limit_down:'跌停卖不出', no_prev_close:'首日无前收', below_one_lot:'资金不足一手', nothing_sellable:'T+1 不可卖', no_price:'无报价', unsupported_qty:'数量类型不支持'};
  var rjCnt = {};
  rj.forEach(function(r){ rjCnt[r.reason] = (rjCnt[r.reason]||0) + 1; });
  var rjHtml = rj.length
    ? '<div style="margin-top:6px;color:#b8860b">未成交订单 '+rj.length+' 笔：'+Object.keys(rjCnt).map(function(k){ return (rjNames[k]||k)+' '+rjCnt[k]; }).join(' · ')+'</div>'
    : '';
  var execHtml = o.execution === 'a_share'
    ? '<div style="margin-top:6px;color:#7f8c8d;font-size:.9em">成交口径：A 股 · 当日开盘价 +0.1% 滑点 · 整手 · 开盘涨跌停不成交 · T+1</div>'
    : '';
  box.innerHTML = '<div class="card" style="margin-top:0">'
    + '<div style="font-size:1.1rem;font-weight:600">'+esc(o.code)+' · '+esc(o.name)+'</div>'
    + '<div style="margin-top:8px;color:#34495e">总收益 '+pct(s.total_return)+' · 年化 '+pct(s.annualized)+' · 夏普 '+s.sharpe.toFixed(2)+' · 最大回撤 '+pct(s.max_drawdown)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">投入 '+s.total_contributed.toFixed(0)+' · 期末 '+s.final_equity.toFixed(0)+' · 成交 '+s.trade_count+' 笔</div>'
    + '<div style="margin-top:6px;color:#5a6a7a">交易统计：卖出 '+(ts.round_trips||0)+' 次 · 胜率 '+pct(ts.win_rate||0)+' · 盈亏比 '+pf(ts.profit_factor)+' · 实现盈亏 '+(ts.realized_pnl||0).toFixed(0)+'</div>'
    + rjHtml + execHtml
    + '</div>';
```

`page.rs` 的页面字符串以 `r##"` … `"##` 定界,JS 中的双引号可直接使用。

- [ ] **Step 7: CI 三件套**

Run: `cargo fmt --check`
Expected: 无输出(若有差异先执行 `cargo fmt` 再检查)

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无警告

Run: `cargo test --all-targets`
Expected: 全部 PASS;lib 部分 `522 passed; 0 failed; 8 ignored`

- [ ] **Step 8: 手工验证页面**

Run: `cargo run -- serve`(默认地址见 `.env.example`)
在浏览器登录后打开股票回测页,输入 `600519`、策略 `dca`、每次金额 `200000`、区间近 3 年,点击回测。
Expected: 结果卡片出现「成交口径:A 股…」一行;成交价不再等于当日收盘价;若区间内有涨跌停定投日,出现「未成交订单 N 笔」。再输入 `600519` 每次金额 `1000`,Expected: 显示「资金不足一手」且成交 0 笔。

- [ ] **Step 9: Commit**

```bash
git add src/stock/backtest.rs src/stock/recommend.rs src/web/stock.rs src/web/page.rs
git commit -m "feat(stock): 单股回测采用 A 股成交口径并展示未成交订单"
```

---

## 完成标准

- [ ] 基金回测相关测试全部原样通过,未修改任何基金测试的期望值
- [ ] `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 通过
- [ ] 股票回测页展示成交口径与未成交订单
- [ ] 向用户说明「相对 spec 的实现细化」5 条

## 后续计划(不在本计划范围)

- 计划 2:交易核心(`src/trade/`:模型与存储、工单状态机、闸门、Broker、trade-monitor、四类信号源)—— 复用本计划 `stock::ashare` 规则函数与 `AShareExecution`(PaperBroker)
- 计划 3:策略准入(walk-forward、准入状态机、watchdog、成绩单;股票推荐迁移到 A 股口径)
- 计划 4:Web `/trade` 页面、签名链接、推送与日报

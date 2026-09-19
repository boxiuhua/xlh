# 量化交易 · 计划 2a/4:交易核心库(工单、闸门、成交、模拟盘)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `src/trade/` 建立不依赖线程与网页的交易核心库:信号入库去重 → 闸门(准入/交易规则/风控/仓位)→ 实盘人工工单与模拟盘工单 → 状态机 → 成交回填更新持仓与资金 → 模拟盘按报价自动成交;并先修复计划 1 遗留的「卖出最低佣金按 lot 重复收取」与价格最小价位问题。

**Architecture:** 纯函数闸门 `gate::evaluate` 只做判断;`ticket`/`store` 负责 SQLite 读写与状态转换(条件 UPDATE 保证幂等);`router` 负责模拟盘自动成交;`service::submit_signal` 串起全流程。所有表按 `user_id` 隔离,时间统一 `NaiveDateTime` 本地时间字符串 `%Y-%m-%d %H:%M:%S`。

**Tech Stack:** Rust 2021、rusqlite 0.31(bundled)、chrono 0.4、serde/serde_json、anyhow。无新依赖。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md`(本计划实现 §4 数据模型、§6 闸门、§7 状态机、§12 中与库相关的幂等/越界项;§14 任务 2、3、4)。前置:计划 1 `docs/superpowers/plans/2026-09-15-quant-trading-p1-execution-model.md` 已完成于分支 `feat/quant-p1-execution`,其「执行后遗留项」中标注计划 2 的条目由本计划 Task 1–2 吸收。

## Global Constraints

- 基金回测结果**逐位不变**;不得修改任何基金相关测试期望值
- 所有交易表与查询必须按 `user_id` 隔离;跨用户操作一律视为「不存在 / 已处理」
- 时间:`NaiveDateTime`,存储格式 `%Y-%m-%d %H:%M:%S`;日期 `%Y-%m-%d`
- 状态转换一律条件 UPDATE(`WHERE ... AND status = ?`),影响 0 行返回 `Transition::AlreadyHandled`
- 默认风控值(照抄 spec §6 与设计讨论):单笔上限 50000 元、单票仓位上限 20%、每日实盘工单上限 20、当日亏损熔断 3%、冷却 60 分钟、价格偏离阈值 1.5%、默认止损 8%、默认止盈 20%、滑点 0.1%、总开关开
- 工单有效期(spec §5):exit 30 分钟;mover 10 分钟;strategy 当日 10:30;manual 当日 15:00(若已过则 30 分钟)
- 止盈止损与手动信号**不受准入约束**;strategy/mover:已准入 → 实盘+模拟盘,观察期 → 仅模拟盘,其他 → 拒绝
- 不引入新依赖;推送进程不引入 tokio
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外无失败
- 不使用 `git stash`

### 相对 spec 的实现细化(执行者照此实现)

1. **Broker trait**:spec §3.1 的 `trait Broker { submit, cancel, positions }` 在本期落为两个函数:人工券商 = 工单保持 `pending/confirmed` 等待 `ticket::record_fill`;模拟盘 = `router::fill_paper_ticket`。统一 trait 推迟到三期接 QMT 时再抽象(届时才有第二种自动成交实现)。
2. **AccountView**:计划 1 最终审查建议的 `AccountView` trait 不做;模拟盘改为复用抽出的纯函数 `ashare::slipped_price` / `ashare::sell_qty`,不再需要把持仓伪装成 `Broker`。`ExecutionModel` 增加 `Send` 约束。
3. **当日亏损熔断**只统计当日**已实现**盈亏(卖出成交的 `realized_pnl`),不含浮动盈亏(报价缓存在计划 2b)。
4. **账户资金**:模拟盘账户在首次提交信号时按实盘总资金自动创建。成交回填允许可用资金变为负数(反映真实已发生成交),仓位计算已按可用资金限额。
5. **持仓成本**含买入费用;卖出 `realized_pnl = (成交价 − 成本价) × 数量 − 卖出费用`。
6. **T+1**:持仓记录 `last_buy_date` 与 `today_bought_qty`,可卖 = `qty − (last_buy_date == 今日 ? today_bought_qty : 0)`。
7. **持仓校准表 `trade_position_adjusts`**、报价缓存 `trade_quotes`、策略相关表不在本计划(分别在计划 4、2b、3)。
8. **账户范围**:`NewSignal.scope`(Both / RealOnly / PaperOnly)与准入推导出的账户取交集;止盈止损须按账户分别发信号,dedup_key 需含账户。
9. **被拒信号可重试**:同 dedup_key 的信号若此前被闸门拒绝,再次提交会重新激活同一行并重新判定;已生成工单的信号仍返回 Duplicate。
10. **事务**:`submit_signal` 与 `record_fill` 使用 IMMEDIATE 事务,避免 WAL 下与推送守护进程连接并发时的 BUSY_SNAPSHOT。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/broker.rs` | 修改 | `Fee::sell_fee_order` 默认逐 lot;卖出按整单计费 |
| `src/stock/fee.rs` | 修改 | `StockFee` 覆盖 `sell_fee_order`:最低佣金每单一次 |
| `src/stock/ashare.rs` | 修改 | `round_price_to_tick`、`slipped_price`、`sell_qty`;`is_etf` 加 `50`;`AShareExecution` 使用 `slipped_price` |
| `src/execution.rs` | 修改 | `trait ExecutionModel: Send` |
| `src/trade/mod.rs` | 新建 | 模块声明 |
| `src/trade/model.rs` | 新建 | 枚举、结构体、时间格式 |
| `src/trade/store.rs` | 新建 | 建表;账户、风控、持仓读写 |
| `src/trade/gate.rs` | 新建 | 纯函数闸门与仓位计算 |
| `src/trade/ticket.rs` | 新建 | 信号入库、工单状态机、成交回填 |
| `src/trade/router.rs` | 新建 | 模拟盘自动成交 |
| `src/trade/service.rs` | 新建 | `submit_signal` 全流程 |
| `src/lib.rs` | 修改 | `pub mod trade;` |
| `src/main.rs`、`src/web/mod.rs` | 修改 | 启动时 `trade::store::migrate` |
| `tests/trade_core.rs` | 新建 | 端到端集成测试 |

---

### Task 1: 卖出最低佣金按整单收取

**Files:**
- Modify: `src/broker.rs:36-49`(`trait Fee`、`impl Fee for FeeModel`)、`src/broker.rs:142-171`(`execute` 卖出分支)
- Modify: `src/stock/fee.rs:45-55`(`impl Fee for StockFee`)
- Test: `src/broker.rs`、`src/stock/fee.rs` 内 `mod tests`

**Interfaces:**
- Consumes: 无
- Produces: `trait Fee { fn sell_fee_order(&self, legs: &[(f64, i64)], price: f64) -> f64 }`(默认逐 lot 累加);`StockFee` 覆盖为整单一次最低佣金

- [ ] **Step 1: 写失败测试**

`src/stock/fee.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn sell_fee_order_charges_min_commission_once() {
        // 三个 lot 各 100 股 @10,合计 3000:佣金 0.75<5 取 5;印花 1.5;过户 0.03 → 6.53
        let legs = [(100.0, 10), (100.0, 5), (100.0, 1)];
        let fee = StockFee::a_share().sell_fee_order(&legs, 10.0);
        assert!((fee - 6.53).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn sell_fee_order_with_no_legs_is_zero() {
        assert!(StockFee::a_share().sell_fee_order(&[], 10.0).abs() < 1e-12);
    }
```

`src/broker.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn fund_fee_model_order_fee_still_sums_lots_by_tier() {
        // 默认实现逐 lot:3 天档 1.5% + 100 天档 0.5%
        let fee = fee_model().sell_fee_order(&[(100.0, 3), (100.0, 100)], 1.0);
        assert!((fee - 2.0).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn stock_sell_across_lots_pays_min_commission_once() {
        let mut b = Broker::new(crate::stock::fee::StockFee::a_share());
        for day in [2, 3, 4] {
            b.execute(
                &OrderEvent {
                    date: d(2024, 1, day),
                    direction: Direction::Buy,
                    qty: OrderQty::Shares(100.0),
                },
                10.0,
            );
        }
        let fill = b.execute(
            &OrderEvent {
                date: d(2024, 1, 10),
                direction: Direction::Sell,
                qty: OrderQty::AllShares,
            },
            10.0,
        );
        assert!((fill.shares - 300.0).abs() < 1e-9);
        assert!((fill.fee - 6.53).abs() < 1e-9, "整单一次最低佣金,实际 {}", fill.fee);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib broker::tests stock::fee::tests`
Expected: 编译失败,`no method named sell_fee_order`

- [ ] **Step 3: 实现**

`src/broker.rs` 的 `trait Fee` 改为:

```rust
/// 资产无关的费用抽象：基金用 FeeModel，股票用 StockFee。
pub trait Fee {
    fn buy_fee(&self, cash: f64) -> f64;
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64;

    /// 一笔卖单的总费用。`legs` 为 FIFO 拆出的 (份额, 持有天数)。
    /// 默认逐 lot 累加(基金赎回费按持有期分档,本就逐 lot 计);
    /// 有「每笔最低佣金」的费率须覆盖,整单只收一次最低佣金。
    fn sell_fee_order(&self, legs: &[(f64, i64)], price: f64) -> f64 {
        legs.iter()
            .fold(0.0, |acc, &(shares, days)| acc + self.sell_fee(shares, price, days))
    }
}
```

`execute` 的 `Direction::Sell` 分支中,把循环改为先收集 legs 再整单计费(其余不变):

```rust
                let mut remaining = want.min(self.total_shares());
                let mut sold = 0.0;
                let mut legs: Vec<(f64, i64)> = Vec::new();
                let mut i = 0;
                while remaining > 1e-9 && i < self.lots.len() {
                    let take = remaining.min(self.lots[i].shares);
                    let days = (order.date - self.lots[i].date).num_days();
                    legs.push((take, days));
                    self.lots[i].shares -= take;
                    sold += take;
                    remaining -= take;
                    i += 1;
                }
                let fee = self.fee.sell_fee_order(&legs, price);
                self.lots.retain(|l| l.shares > 1e-9);
```

`src/stock/fee.rs` 的 `impl Fee for StockFee` 中 `sell_fee` 之后加入:

```rust
    /// 股票佣金按「每笔委托」计最低收费:多个 lot 合并为一单,只收一次最低佣金。
    fn sell_fee_order(&self, legs: &[(f64, i64)], price: f64) -> f64 {
        let shares: f64 = legs.iter().map(|(s, _)| s).sum();
        if shares <= 0.0 {
            return 0.0;
        }
        self.sell_fee(shares, price, 0)
    }
```

- [ ] **Step 4: 运行确认通过 + 全量回归**

Run: `cargo test --lib broker::tests stock::fee::tests`
Expected: 全部 PASS

Run: `cargo test --all-targets`
Expected: 除既有 `realtime_pipeline::full_day_flow_from_detection_to_summary` 外全部 PASS。若有**股票路径**测试因费用下降而改变期望值,在报告中逐个列出(不得修改基金测试)。

- [ ] **Step 5: Commit**

```bash
git add src/broker.rs src/stock/fee.rs
git commit -m "fix(broker): 股票卖出最低佣金按整单收取一次"
```

---

### Task 2: A 股价格与数量纯函数(最小价位、滑点、卖出数量)

**Files:**
- Modify: `src/stock/ashare.rs`(`is_etf`、新增 3 个 pub fn、`AShareExecution::prepare` 两处价格计算、一个测试期望)
- Modify: `src/execution.rs:59`(`pub trait ExecutionModel`)
- Test: `src/stock/ashare.rs`、`src/execution.rs` 内 `mod tests`

**Interfaces:**
- Consumes: 现有 `limit_ratio`、`price_decimals`、`buy_lot`、`is_star`
- Produces:

```rust
pub fn round_price_to_tick(price: f64, decimals: i32, side: Direction) -> f64; // 买向上、卖向下
pub fn slipped_price(side: Direction, price: f64, slippage: f64, decimals: i32, limit_up: f64, limit_down: f64) -> f64;
pub fn sell_qty(code: &str, want: u64, sellable: u64) -> u64;
pub trait ExecutionModel: Send { .. }
```

- [ ] **Step 1: 写失败测试**

`src/stock/ashare.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn round_price_to_tick_is_adverse_to_trader() {
        assert!(close(round_price_to_tick(11.011, 2, Direction::Buy), 11.02));
        assert!(close(round_price_to_tick(11.011, 2, Direction::Sell), 11.01));
        assert!(close(round_price_to_tick(10.0 * 1.001, 2, Direction::Buy), 10.01));
        assert!(close(round_price_to_tick(10.0 * 0.999, 2, Direction::Sell), 9.99));
        assert!(close(round_price_to_tick(3.4561, 3, Direction::Buy), 3.457));
    }

    #[test]
    fn slipped_price_respects_limits() {
        assert!(close(slipped_price(Direction::Buy, 10.9, 0.05, 2, 11.0, 9.0), 11.0));
        assert!(close(slipped_price(Direction::Sell, 9.1, 0.05, 2, 11.0, 9.0), 9.0));
        assert!(close(
            slipped_price(Direction::Buy, 10.0, 0.001, 2, f64::INFINITY, 0.0),
            10.01
        ));
    }

    #[test]
    fn sell_qty_by_board() {
        assert_eq!(sell_qty("600000", 250, 1050), 200);
        assert_eq!(sell_qty("600000", 50, 1050), 0);
        assert_eq!(sell_qty("600000", 2000, 1050), 1050, "超出可卖 → 全部可卖(允许零股)");
        assert_eq!(sell_qty("600000", 1050, 1050), 1050);
        assert_eq!(sell_qty("688001", 150, 300), 0, "科创板部分卖出不足 200 股");
        assert_eq!(sell_qty("688001", 250, 300), 250);
        assert_eq!(sell_qty("688001", 150, 150), 150, "清仓允许不足 200 股");
        assert_eq!(sell_qty("600000", 100, 0), 0);
    }

    #[test]
    fn lof_prefix_50_uses_three_decimals() {
        assert_eq!(price_decimals("501018"), 3);
    }
```

并把 `engine_records_rejections_and_fills_at_open` 中的

```rust
        assert!(close(t.price, 11.011), "price={}", t.price);
```

改为(买入价向上取整到分:11 × 1.001 = 11.011 → 11.02;900 股 9918 + 5.09 仍在预算内):

```rust
        assert!(close(t.price, 11.02), "price={}", t.price);
```

`src/execution.rs` 的 `mod tests` 末尾追加:

```rust
    #[test]
    fn execution_models_are_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<Box<dyn ExecutionModel>>();
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::ashare::tests execution::tests`
Expected: 编译失败(`round_price_to_tick` 等未定义;`dyn ExecutionModel` 未实现 `Send`)

- [ ] **Step 3: 实现**

1. `src/execution.rs`:`pub trait ExecutionModel {` 改为 `pub trait ExecutionModel: Send {`,并在其文档注释补一行 `/// 需 Send:后台评估线程会持有成交模型。`

2. `src/stock/ashare.rs`:`is_etf` 前缀数组改为 `["50", "51", "52", "56", "58", "15", "16", "18"]`。

3. 在 `step_down` 之后插入:

```rust
/// 按最小价位取整,方向对交易者不利(保守):买入向上、卖出向下。
pub fn round_price_to_tick(price: f64, decimals: i32, side: Direction) -> f64 {
    let m = 10f64.powi(decimals);
    match side {
        Direction::Buy => ((price * m) - 1e-6).ceil() / m,
        Direction::Sell => ((price * m) + 1e-6).floor() / m,
    }
}

/// 含滑点的成交价:买 ×(1+s)、卖 ×(1−s),按最小价位取整,且不越过涨跌停价。
/// 无涨跌停限制时传 `f64::INFINITY` / `0.0`。
pub fn slipped_price(
    side: Direction,
    price: f64,
    slippage: f64,
    decimals: i32,
    limit_up: f64,
    limit_down: f64,
) -> f64 {
    match side {
        Direction::Buy => {
            round_price_to_tick(price * (1.0 + slippage), decimals, Direction::Buy).min(limit_up)
        }
        Direction::Sell => {
            round_price_to_tick(price * (1.0 - slippage), decimals, Direction::Sell)
                .max(limit_down)
        }
    }
}

/// 卖出股数:想卖 ≥ 可卖 → 全部可卖(允许零股);否则按步长向下取整;
/// 科创板部分卖出不得少于 200 股。
pub fn sell_qty(code: &str, want: u64, sellable: u64) -> u64 {
    if sellable == 0 || want == 0 {
        return 0;
    }
    if want >= sellable {
        return sellable;
    }
    let lot = buy_lot(code);
    let n = want / lot.step * lot.step;
    if is_star(code) && n < lot.min {
        return 0;
    }
    n
}
```

4. `AShareExecution::prepare` 中:
   - 买入 `let raw_price = (bar.open * (1.0 + self.slippage)).min(up);` 改为
     `let raw_price = slipped_price(Direction::Buy, bar.open, self.slippage, self.decimals, up, down);`
   - 卖出 `let raw_price = (bar.open * (1.0 - self.slippage)).max(down);` 改为
     `let raw_price = slipped_price(Direction::Sell, bar.open, self.slippage, self.decimals, up, down);`

- [ ] **Step 4: 运行确认通过 + 全量回归**

Run: `cargo test --lib stock::ashare::tests execution::tests`
Expected: 全部 PASS

Run: `cargo test --all-targets`
Expected: 除既有失败外全部 PASS;若其他测试断言了非整分价格而失败,按「买向上/卖向下取整到分」更新期望并在报告中逐个列出。

- [ ] **Step 5: Commit**

```bash
git add src/stock/ashare.rs src/execution.rs
git commit -m "feat(stock): 成交价按最小价位取整、卖出数量规则、ExecutionModel: Send"
```

---

### Task 3: trade 模块骨架、数据模型与建表

**Files:**
- Create: `src/trade/mod.rs`、`src/trade/model.rs`、`src/trade/store.rs`
- Modify: `src/lib.rs`(按字母序在 `pub mod strategy;` 之后加 `pub mod trade;`)
- Modify: `src/main.rs:126`、`src/web/mod.rs:537`、`src/web/mod.rs:1379`(各加一行 migrate)
- Test: `src/trade/model.rs`、`src/trade/store.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `crate::event::Direction`
- Produces(后续任务依赖,名称与类型必须一致):

```rust
// model.rs
pub const TS_FMT: &str = "%Y-%m-%d %H:%M:%S";
pub const DATE_FMT: &str = "%Y-%m-%d";
pub fn fmt_ts(t: NaiveDateTime) -> String;
pub fn parse_ts(s: &str) -> anyhow::Result<NaiveDateTime>;
pub fn side_str(side: Direction) -> &'static str;          // "buy" | "sell"
pub fn parse_side(s: &str) -> anyhow::Result<Direction>;
pub enum Account { Real, Paper }                            // as_str "real"/"paper", parse
pub enum SignalSource { Exit, Strategy, Mover, Manual }     // as_str "exit"/"strategy"/"mover"/"manual", parse
pub enum TicketStatus { Pending, Confirmed, Partial, Filled, Expired, Rejected, Cancelled } // as_str 小写, parse, is_open
pub struct NewSignal { user_id: i64, source: SignalSource, strategy_id: Option<i64>, code: String, name: Option<String>, side: Direction, ref_price: f64, reason: String, ai_note: Option<String>, dedup_key: String, suggest_cash: Option<f64>, suggest_qty: Option<u64> }
pub struct Quote { code: String, price: f64, limit_up: Option<f64>, limit_down: Option<f64>, ts: NaiveDateTime }
pub struct RiskRules { max_order_amount: f64, max_position_pct: f64, max_daily_tickets: u32, daily_loss_halt_pct: f64, cooldown_min: i64, deviation_th: f64, default_stop_loss_pct: f64, default_take_profit_pct: f64, slippage: f64, enabled: bool } // Default
pub struct AccountState { user_id: i64, account: Account, total_capital: f64, available_cash: f64 }
pub struct Position { user_id: i64, account: Account, code: String, qty: u64, avg_cost: f64, today_bought_qty: u64, last_buy_date: Option<NaiveDate>, stop_loss: Option<f64>, take_profit: Option<f64>, trailing_pct: Option<f64>, trailing_high: Option<f64> }
impl Position { pub fn empty(user_id: i64, account: Account, code: &str) -> Self; pub fn sellable(&self, today: NaiveDate) -> u64; }
pub struct Ticket { id: i64, user_id: i64, signal_id: i64, account: Account, code: String, side: Direction, suggest_price: f64, qty: u64, filled_qty: u64, expires_at: NaiveDateTime, deviation_th: f64, status: TicketStatus, urgency: i64, created_at: NaiveDateTime, confirmed_at: Option<NaiveDateTime>, ignore_reason: Option<String> }
// store.rs
pub fn migrate(conn: &Connection) -> anyhow::Result<()>;
```

(以上字段全部 `pub`。)

- [ ] **Step 1: 写 model.rs(含测试)**

创建 `src/trade/model.rs`:

```rust
//! 交易核心数据模型。所有时间为本地 `NaiveDateTime`,存储为 `TS_FMT` 字符串。

use crate::event::Direction;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

pub const TS_FMT: &str = "%Y-%m-%d %H:%M:%S";
pub const DATE_FMT: &str = "%Y-%m-%d";

pub fn fmt_ts(t: NaiveDateTime) -> String {
    t.format(TS_FMT).to_string()
}

pub fn parse_ts(s: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, TS_FMT).map_err(|e| anyhow!("时间格式错误 {s}: {e}"))
}

pub fn side_str(side: Direction) -> &'static str {
    match side {
        Direction::Buy => "buy",
        Direction::Sell => "sell",
    }
}

pub fn parse_side(s: &str) -> Result<Direction> {
    match s {
        "buy" => Ok(Direction::Buy),
        "sell" => Ok(Direction::Sell),
        _ => Err(anyhow!("未知买卖方向: {s}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Account {
    /// 实盘:用户确认后线下成交、回填
    Real,
    /// 模拟盘:自动成交
    Paper,
}

impl Account {
    pub fn as_str(self) -> &'static str {
        match self {
            Account::Real => "real",
            Account::Paper => "paper",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "real" => Ok(Account::Real),
            "paper" => Ok(Account::Paper),
            _ => Err(anyhow!("未知账户类型: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalSource {
    /// 持仓止盈止损
    Exit,
    /// 日线策略
    Strategy,
    /// 实时异动
    Mover,
    /// 用户手动 / AI 建议
    Manual,
}

impl SignalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalSource::Exit => "exit",
            SignalSource::Strategy => "strategy",
            SignalSource::Mover => "mover",
            SignalSource::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "exit" => Ok(SignalSource::Exit),
            "strategy" => Ok(SignalSource::Strategy),
            "mover" => Ok(SignalSource::Mover),
            "manual" => Ok(SignalSource::Manual),
            _ => Err(anyhow!("未知信号来源: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketStatus {
    Pending,
    Confirmed,
    Partial,
    Filled,
    Expired,
    Rejected,
    Cancelled,
}

impl TicketStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TicketStatus::Pending => "pending",
            TicketStatus::Confirmed => "confirmed",
            TicketStatus::Partial => "partial",
            TicketStatus::Filled => "filled",
            TicketStatus::Expired => "expired",
            TicketStatus::Rejected => "rejected",
            TicketStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => TicketStatus::Pending,
            "confirmed" => TicketStatus::Confirmed,
            "partial" => TicketStatus::Partial,
            "filled" => TicketStatus::Filled,
            "expired" => TicketStatus::Expired,
            "rejected" => TicketStatus::Rejected,
            "cancelled" => TicketStatus::Cancelled,
            _ => return Err(anyhow!("未知工单状态: {s}")),
        })
    }

    /// 未完结:待确认 / 待成交 / 部分成交
    pub fn is_open(self) -> bool {
        matches!(
            self,
            TicketStatus::Pending | TicketStatus::Confirmed | TicketStatus::Partial
        )
    }
}

/// 待提交的交易信号(四类来源统一结构)。
#[derive(Debug, Clone, PartialEq)]
pub struct NewSignal {
    pub user_id: i64,
    pub source: SignalSource,
    pub strategy_id: Option<i64>,
    pub code: String,
    pub name: Option<String>,
    pub side: Direction,
    pub ref_price: f64,
    pub reason: String,
    pub ai_note: Option<String>,
    /// 同一用户内唯一,用于幂等去重
    pub dedup_key: String,
    /// 买入建议金额;None 则按单笔上限
    pub suggest_cash: Option<f64>,
    /// 卖出建议股数;None 则全部可卖
    pub suggest_qty: Option<u64>,
}

/// 实时报价(不复权)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Quote {
    pub code: String,
    pub price: f64,
    pub limit_up: Option<f64>,
    pub limit_down: Option<f64>,
    pub ts: NaiveDateTime,
}

/// 每个用户的风控规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskRules {
    pub max_order_amount: f64,
    pub max_position_pct: f64,
    pub max_daily_tickets: u32,
    pub daily_loss_halt_pct: f64,
    pub cooldown_min: i64,
    pub deviation_th: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
    pub slippage: f64,
    pub enabled: bool,
}

impl Default for RiskRules {
    fn default() -> Self {
        Self {
            max_order_amount: 50_000.0,
            max_position_pct: 0.20,
            max_daily_tickets: 20,
            daily_loss_halt_pct: 0.03,
            cooldown_min: 60,
            deviation_th: 0.015,
            default_stop_loss_pct: 0.08,
            default_take_profit_pct: 0.20,
            slippage: 0.001,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountState {
    pub user_id: i64,
    pub account: Account,
    pub total_capital: f64,
    pub available_cash: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Position {
    pub user_id: i64,
    pub account: Account,
    pub code: String,
    pub qty: u64,
    /// 含买入费用的成本价
    pub avg_cost: f64,
    pub today_bought_qty: u64,
    pub last_buy_date: Option<NaiveDate>,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub trailing_pct: Option<f64>,
    pub trailing_high: Option<f64>,
}

impl Position {
    pub fn empty(user_id: i64, account: Account, code: &str) -> Self {
        Self {
            user_id,
            account,
            code: code.to_string(),
            qty: 0,
            avg_cost: 0.0,
            today_bought_qty: 0,
            last_buy_date: None,
            stop_loss: None,
            take_profit: None,
            trailing_pct: None,
            trailing_high: None,
        }
    }

    /// T+1:当日买入份额当日不可卖。
    pub fn sellable(&self, today: NaiveDate) -> u64 {
        if self.last_buy_date == Some(today) {
            self.qty.saturating_sub(self.today_bought_qty)
        } else {
            self.qty
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ticket {
    pub id: i64,
    pub user_id: i64,
    pub signal_id: i64,
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub suggest_price: f64,
    pub qty: u64,
    pub filled_qty: u64,
    pub expires_at: NaiveDateTime,
    pub deviation_th: f64,
    pub status: TicketStatus,
    pub urgency: i64,
    pub created_at: NaiveDateTime,
    pub confirmed_at: Option<NaiveDateTime>,
    pub ignore_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn enums_round_trip_through_strings() {
        for a in [Account::Real, Account::Paper] {
            assert_eq!(Account::parse(a.as_str()).unwrap(), a);
        }
        for s in [
            SignalSource::Exit,
            SignalSource::Strategy,
            SignalSource::Mover,
            SignalSource::Manual,
        ] {
            assert_eq!(SignalSource::parse(s.as_str()).unwrap(), s);
        }
        for t in [
            TicketStatus::Pending,
            TicketStatus::Confirmed,
            TicketStatus::Partial,
            TicketStatus::Filled,
            TicketStatus::Expired,
            TicketStatus::Rejected,
            TicketStatus::Cancelled,
        ] {
            assert_eq!(TicketStatus::parse(t.as_str()).unwrap(), t);
        }
        assert_eq!(parse_side(side_str(Direction::Sell)).unwrap(), Direction::Sell);
        assert!(Account::parse("x").is_err());
        let t = day(15).and_hms_opt(9, 30, 5).unwrap();
        assert_eq!(parse_ts(&fmt_ts(t)).unwrap(), t);
    }

    #[test]
    fn open_statuses() {
        assert!(TicketStatus::Pending.is_open());
        assert!(TicketStatus::Partial.is_open());
        assert!(!TicketStatus::Filled.is_open());
        assert!(!TicketStatus::Expired.is_open());
    }

    #[test]
    fn sellable_applies_t_plus_one() {
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.today_bought_qty = 300;
        p.last_buy_date = Some(day(15));
        assert_eq!(p.sellable(day(15)), 700);
        assert_eq!(p.sellable(day(16)), 1000);
    }

    #[test]
    fn risk_rules_defaults_and_partial_json() {
        let r: RiskRules = serde_json::from_str(r#"{"cooldown_min": 30}"#).unwrap();
        assert_eq!(r.cooldown_min, 30);
        assert!((r.max_order_amount - 50_000.0).abs() < 1e-9);
        assert!(r.enabled);
    }
}
```

- [ ] **Step 2: 写 store.rs 建表(含测试)与 mod.rs**

创建 `src/trade/mod.rs`:

```rust
//! 量化交易核心:信号 → 闸门 → 工单 → 成交 → 持仓。
//! 设计见 docs/superpowers/specs/2026-09-15-quant-trading-design.md

pub mod model;
pub mod store;
```

创建 `src/trade/store.rs`:

```rust
//! 交易表结构与账户 / 风控 / 持仓读写。所有查询按 user_id 隔离。

use anyhow::{Context, Result};
use rusqlite::Connection;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS trade_accounts (
  user_id        INTEGER NOT NULL,
  account        TEXT NOT NULL,
  total_capital  REAL NOT NULL,
  available_cash REAL NOT NULL,
  updated_at     TEXT NOT NULL,
  PRIMARY KEY (user_id, account)
);
CREATE TABLE IF NOT EXISTS trade_risk_rules (
  user_id    INTEGER PRIMARY KEY,
  rules_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trade_signals (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id       INTEGER NOT NULL,
  source        TEXT NOT NULL,
  strategy_id   INTEGER,
  code          TEXT NOT NULL,
  name          TEXT,
  side          TEXT NOT NULL,
  ref_price     REAL NOT NULL,
  reason        TEXT NOT NULL,
  ai_note       TEXT,
  dedup_key     TEXT NOT NULL,
  suggest_cash  REAL,
  suggest_qty   INTEGER,
  status        TEXT NOT NULL DEFAULT 'new',
  reject_reason TEXT,
  created_at    TEXT NOT NULL,
  UNIQUE (user_id, dedup_key)
);
CREATE TABLE IF NOT EXISTS trade_tickets (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id       INTEGER NOT NULL,
  signal_id     INTEGER NOT NULL,
  account       TEXT NOT NULL,
  code          TEXT NOT NULL,
  side          TEXT NOT NULL,
  suggest_price REAL NOT NULL,
  qty           INTEGER NOT NULL,
  filled_qty    INTEGER NOT NULL DEFAULT 0,
  expires_at    TEXT NOT NULL,
  deviation_th  REAL NOT NULL,
  status        TEXT NOT NULL,
  urgency       INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  confirmed_at  TEXT,
  ignore_reason TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_tickets_user_status ON trade_tickets(user_id, status);
CREATE TABLE IF NOT EXISTS trade_fills (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id    INTEGER NOT NULL,
  user_id      INTEGER NOT NULL,
  account      TEXT NOT NULL,
  code         TEXT NOT NULL,
  side         TEXT NOT NULL,
  price        REAL NOT NULL,
  qty          INTEGER NOT NULL,
  fee          REAL NOT NULL,
  realized_pnl REAL,
  source       TEXT NOT NULL,
  filled_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_fills_user_time ON trade_fills(user_id, filled_at);
CREATE TABLE IF NOT EXISTS trade_positions (
  user_id          INTEGER NOT NULL,
  account          TEXT NOT NULL,
  code             TEXT NOT NULL,
  qty              INTEGER NOT NULL,
  avg_cost         REAL NOT NULL,
  today_bought_qty INTEGER NOT NULL DEFAULT 0,
  last_buy_date    TEXT,
  stop_loss        REAL,
  take_profit      REAL,
  trailing_pct     REAL,
  trailing_high    REAL,
  updated_at       TEXT NOT NULL,
  PRIMARY KEY (user_id, account, code)
);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建交易表失败")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    #[test]
    fn migrate_is_idempotent_and_creates_tables() {
        let c = db();
        migrate(&c).unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'trade_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 6);
    }

    #[test]
    fn signal_dedup_key_is_unique_per_user() {
        let c = db();
        let ins = |uid: i64| {
            c.execute(
                "INSERT OR IGNORE INTO trade_signals (user_id, source, code, side, ref_price, reason, dedup_key, created_at)
                 VALUES (?1, 'manual', '600000', 'buy', 10.0, 'r', 'k1', '2026-09-15 10:00:00')",
                [uid],
            )
            .unwrap()
        };
        assert_eq!(ins(1), 1);
        assert_eq!(ins(1), 0, "同用户同 key 被忽略");
        assert_eq!(ins(2), 1, "不同用户可用相同 key");
    }
}
```

`src/lib.rs` 在 `pub mod strategy;` 后加 `pub mod trade;`。

- [ ] **Step 3: 接入启动迁移**

- `src/main.rs`:在 `xlh::push::store::migrate(&conn)?;` 下一行加 `xlh::trade::store::migrate(&conn)?;`
- `src/web/mod.rs` 的 serve 中:在 `crate::push::store::migrate(&conn).context("建推送配置表失败")?;` 下一行加 `crate::trade::store::migrate(&conn).context("建交易表失败")?;`
- `src/web/mod.rs` 测试辅助(约 1379 行,`crate::push::store::migrate(&conn).unwrap();` 之后)加 `crate::trade::store::migrate(&conn).unwrap();`

- [ ] **Step 4: 运行测试**

Run: `cargo test --lib trade::`
Expected: model 4 个、store 2 个全部 PASS

Run: `cargo build`
Expected: 成功

- [ ] **Step 5: Commit**

```bash
git add src/trade src/lib.rs src/main.rs src/web/mod.rs
git commit -m "feat(trade): 交易模块骨架、数据模型与建表迁移"
```

---

### Task 4: 账户、风控规则、持仓读写

**Files:**
- Modify: `src/trade/store.rs`
- Test: `src/trade/store.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 3 model 全部类型
- Produces:

```rust
pub fn set_capital(conn: &Connection, user_id: i64, account: Account, total: f64, now: NaiveDateTime) -> Result<()>;
pub fn get_account(conn: &Connection, user_id: i64, account: Account) -> Result<Option<AccountState>>;
pub fn add_cash(conn: &Connection, user_id: i64, account: Account, delta: f64, now: NaiveDateTime) -> Result<()>; // 无账户 → Err("未设置账户资金")
pub fn get_risk_rules(conn: &Connection, user_id: i64) -> Result<RiskRules>;           // 无记录 → Default
pub fn save_risk_rules(conn: &Connection, user_id: i64, rules: &RiskRules, now: NaiveDateTime) -> Result<()>;
pub fn get_position(conn: &Connection, user_id: i64, account: Account, code: &str) -> Result<Option<Position>>;
pub fn list_positions(conn: &Connection, user_id: i64, account: Account) -> Result<Vec<Position>>;
pub fn upsert_position(conn: &Connection, p: &Position, now: NaiveDateTime) -> Result<()>; // qty == 0 → 删除
pub fn set_exit_levels(conn: &Connection, user_id: i64, account: Account, code: &str, stop_loss: Option<f64>, take_profit: Option<f64>, trailing_pct: Option<f64>, now: NaiveDateTime) -> Result<bool>;
```

- [ ] **Step 1: 写失败测试**

`src/trade/store.rs` 的 `mod tests` 中,`use super::*;` 之后加入 `use crate::trade::model::*;` 与 `use chrono::{NaiveDate, NaiveDateTime};`,并追加:

```rust
    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn capital_set_then_adjust_keeps_cash_delta() {
        let c = db();
        assert!(get_account(&c, 1, Account::Real).unwrap().is_none());
        set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        add_cash(&c, 1, Account::Real, -30_000.0, at(15, 10, 0)).unwrap();
        set_capital(&c, 1, Account::Real, 120_000.0, at(15, 11, 0)).unwrap();
        let a = get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!((a.total_capital - 120_000.0).abs() < 1e-9);
        assert!((a.available_cash - 90_000.0).abs() < 1e-9, "70000 + 20000");
        assert!(get_account(&c, 2, Account::Real).unwrap().is_none(), "用户隔离");
        assert!(set_capital(&c, 1, Account::Real, 0.0, at(15, 11, 0)).is_err());
        assert!(add_cash(&c, 2, Account::Real, 1.0, at(15, 11, 0)).is_err());
    }

    #[test]
    fn risk_rules_default_then_saved() {
        let c = db();
        assert_eq!(get_risk_rules(&c, 1).unwrap(), RiskRules::default());
        let r = RiskRules {
            cooldown_min: 15,
            ..RiskRules::default()
        };
        save_risk_rules(&c, 1, &r, at(15, 9, 0)).unwrap();
        assert_eq!(get_risk_rules(&c, 1).unwrap().cooldown_min, 15);
        assert_eq!(get_risk_rules(&c, 2).unwrap(), RiskRules::default());
    }

    #[test]
    fn position_upsert_list_and_delete_on_zero() {
        let c = db();
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0051;
        p.today_bought_qty = 1000;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        p.stop_loss = Some(9.2);
        upsert_position(&c, &p, at(15, 10, 0)).unwrap();
        assert_eq!(get_position(&c, 1, Account::Real, "600000").unwrap(), Some(p.clone()));
        assert!(get_position(&c, 1, Account::Paper, "600000").unwrap().is_none());
        assert_eq!(list_positions(&c, 1, Account::Real).unwrap().len(), 1);

        assert!(set_exit_levels(&c, 1, Account::Real, "600000", Some(9.5), Some(12.0), Some(0.05), at(15, 11, 0)).unwrap());
        let got = get_position(&c, 1, Account::Real, "600000").unwrap().unwrap();
        assert_eq!((got.stop_loss, got.take_profit, got.trailing_pct), (Some(9.5), Some(12.0), Some(0.05)));
        assert!(!set_exit_levels(&c, 2, Account::Real, "600000", None, None, None, at(15, 11, 0)).unwrap());

        p.qty = 0;
        upsert_position(&c, &p, at(15, 12, 0)).unwrap();
        assert!(get_position(&c, 1, Account::Real, "600000").unwrap().is_none());
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::store::tests`
Expected: 编译失败,`cannot find function set_capital`

- [ ] **Step 3: 实现**

`src/trade/store.rs` 顶部 use 改为:

```rust
use crate::trade::model::{
    fmt_ts, Account, AccountState, Position, RiskRules, DATE_FMT,
};
use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension, Row};
```

在 `migrate` 之后追加:

```rust
pub fn set_capital(
    conn: &Connection,
    user_id: i64,
    account: Account,
    total: f64,
    now: NaiveDateTime,
) -> Result<()> {
    if !(total.is_finite() && total > 0.0) {
        return Err(anyhow!("账户总资金必须为正数"));
    }
    // 已存在时按总资金变化量调整可用资金(已占用资金不变)
    conn.execute(
        "INSERT INTO trade_accounts (user_id, account, total_capital, available_cash, updated_at)
         VALUES (?1, ?2, ?3, ?3, ?4)
         ON CONFLICT(user_id, account) DO UPDATE SET
           available_cash = available_cash + (excluded.total_capital - total_capital),
           total_capital  = excluded.total_capital,
           updated_at     = excluded.updated_at",
        params![user_id, account.as_str(), total, fmt_ts(now)],
    )
    .context("设置账户资金失败")?;
    Ok(())
}

pub fn get_account(
    conn: &Connection,
    user_id: i64,
    account: Account,
) -> Result<Option<AccountState>> {
    conn.query_row(
        "SELECT total_capital, available_cash FROM trade_accounts
         WHERE user_id = ?1 AND account = ?2",
        params![user_id, account.as_str()],
        |r| {
            Ok(AccountState {
                user_id,
                account,
                total_capital: r.get(0)?,
                available_cash: r.get(1)?,
            })
        },
    )
    .optional()
    .context("读取账户失败")
}

pub fn add_cash(
    conn: &Connection,
    user_id: i64,
    account: Account,
    delta: f64,
    now: NaiveDateTime,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE trade_accounts SET available_cash = available_cash + ?1, updated_at = ?2
         WHERE user_id = ?3 AND account = ?4",
        params![delta, fmt_ts(now), user_id, account.as_str()],
    )?;
    if n == 0 {
        return Err(anyhow!("未设置账户资金"));
    }
    Ok(())
}

pub fn get_risk_rules(conn: &Connection, user_id: i64) -> Result<RiskRules> {
    let json: Option<String> = conn
        .query_row(
            "SELECT rules_json FROM trade_risk_rules WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?;
    match json {
        Some(j) => serde_json::from_str(&j).context("风控规则格式错误"),
        None => Ok(RiskRules::default()),
    }
}

pub fn save_risk_rules(
    conn: &Connection,
    user_id: i64,
    rules: &RiskRules,
    now: NaiveDateTime,
) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_risk_rules (user_id, rules_json, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET rules_json = excluded.rules_json, updated_at = excluded.updated_at",
        params![user_id, serde_json::to_string(rules)?, fmt_ts(now)],
    )?;
    Ok(())
}

const POSITION_COLS: &str = "code, qty, avg_cost, today_bought_qty, last_buy_date, \
                             stop_loss, take_profit, trailing_pct, trailing_high";

struct RawPosition {
    code: String,
    qty: i64,
    avg_cost: f64,
    today_bought_qty: i64,
    last_buy_date: Option<String>,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
    trailing_high: Option<f64>,
}

fn read_raw_position(r: &Row) -> rusqlite::Result<RawPosition> {
    Ok(RawPosition {
        code: r.get(0)?,
        qty: r.get(1)?,
        avg_cost: r.get(2)?,
        today_bought_qty: r.get(3)?,
        last_buy_date: r.get(4)?,
        stop_loss: r.get(5)?,
        take_profit: r.get(6)?,
        trailing_pct: r.get(7)?,
        trailing_high: r.get(8)?,
    })
}

impl RawPosition {
    fn into_position(self, user_id: i64, account: Account) -> Result<Position> {
        let last_buy_date = self
            .last_buy_date
            .map(|s| NaiveDate::parse_from_str(&s, DATE_FMT))
            .transpose()
            .context("持仓买入日期格式错误")?;
        Ok(Position {
            user_id,
            account,
            code: self.code,
            qty: self.qty.max(0) as u64,
            avg_cost: self.avg_cost,
            today_bought_qty: self.today_bought_qty.max(0) as u64,
            last_buy_date,
            stop_loss: self.stop_loss,
            take_profit: self.take_profit,
            trailing_pct: self.trailing_pct,
            trailing_high: self.trailing_high,
        })
    }
}

pub fn get_position(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
) -> Result<Option<Position>> {
    let raw = conn
        .query_row(
            &format!(
                "SELECT {POSITION_COLS} FROM trade_positions
                 WHERE user_id = ?1 AND account = ?2 AND code = ?3"
            ),
            params![user_id, account.as_str(), code],
            read_raw_position,
        )
        .optional()?;
    raw.map(|r| r.into_position(user_id, account)).transpose()
}

pub fn list_positions(conn: &Connection, user_id: i64, account: Account) -> Result<Vec<Position>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POSITION_COLS} FROM trade_positions
         WHERE user_id = ?1 AND account = ?2 ORDER BY code"
    ))?;
    let raws = stmt
        .query_map(params![user_id, account.as_str()], read_raw_position)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter()
        .map(|r| r.into_position(user_id, account))
        .collect()
}

pub fn upsert_position(conn: &Connection, p: &Position, now: NaiveDateTime) -> Result<()> {
    if p.qty == 0 {
        conn.execute(
            "DELETE FROM trade_positions WHERE user_id = ?1 AND account = ?2 AND code = ?3",
            params![p.user_id, p.account.as_str(), p.code],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO trade_positions (user_id, account, code, qty, avg_cost, today_bought_qty,
           last_buy_date, stop_loss, take_profit, trailing_pct, trailing_high, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(user_id, account, code) DO UPDATE SET
           qty = excluded.qty, avg_cost = excluded.avg_cost,
           today_bought_qty = excluded.today_bought_qty, last_buy_date = excluded.last_buy_date,
           stop_loss = excluded.stop_loss, take_profit = excluded.take_profit,
           trailing_pct = excluded.trailing_pct, trailing_high = excluded.trailing_high,
           updated_at = excluded.updated_at",
        params![
            p.user_id,
            p.account.as_str(),
            p.code,
            p.qty as i64,
            p.avg_cost,
            p.today_bought_qty as i64,
            p.last_buy_date.map(|d| d.format(DATE_FMT).to_string()),
            p.stop_loss,
            p.take_profit,
            p.trailing_pct,
            p.trailing_high,
            fmt_ts(now),
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn set_exit_levels(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
    now: NaiveDateTime,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE trade_positions SET stop_loss = ?1, take_profit = ?2, trailing_pct = ?3, updated_at = ?4
         WHERE user_id = ?5 AND account = ?6 AND code = ?7",
        params![stop_loss, take_profit, trailing_pct, fmt_ts(now), user_id, account.as_str(), code],
    )?;
    Ok(n > 0)
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib trade::store::tests`
Expected: 5 PASS

- [ ] **Step 5: Commit**

```bash
git add src/trade/store.rs
git commit -m "feat(trade): 账户资金、风控规则、持仓读写"
```

---

### Task 5: 闸门(纯函数)

**Files:**
- Create: `src/trade/gate.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod gate;`)
- Test: `src/trade/gate.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `ashare::{price_decimals, buy_lot, round_buy_shares, step_down, sell_qty}`;Task 3 model;`crate::broker::Fee`、`crate::stock::fee::StockFee`
- Produces:

```rust
pub enum Admission { NotRequired, Admitted, Probation, Blocked }            // Copy
pub struct GateInput<'a> { signal: &'a NewSignal, quote: Option<&'a Quote>, admission: Admission, rules: &'a RiskRules, real_account: Option<&'a AccountState>, paper_account: Option<&'a AccountState>, real_position: Option<&'a Position>, paper_position: Option<&'a Position>, has_open_ticket: bool, last_signal_at: Option<NaiveDateTime>, tickets_today: u32, realized_pnl_today: f64, now: NaiveDateTime }
pub struct Plan { account: Account, qty: u64, price: f64 }                   // PartialEq
pub enum GateDecision { Pass(Vec<Plan>), Reject(GateReject) }
pub enum GateReject { TradingDisabled, DuplicateOpenTicket, Cooldown, NotAdmitted, NoQuote, LimitUp, LimitDown, DailyTicketCap, DailyLossHalt, NoCapital, BelowOneLot, NothingSellable } // Copy, Serialize snake_case, as_str
pub fn evaluate(input: &GateInput) -> GateDecision;
pub fn size_buy(code: &str, price: f64, slippage: f64, budget: f64) -> u64;
```

规则顺序(先失败者为拒绝原因):总开关 → 未完结同向工单 → 冷却(exit 不受限)→ 准入 → 报价 → 涨跌停 → 每日工单上限(涉及实盘时)→ 当日亏损熔断(买入且涉及实盘)→ 逐账户仓位计算。账户列表第一个为主账户:主账户计算失败则整单拒绝,次账户(模拟盘)失败则仅跳过。

- [ ] **Step 1: 写失败测试**

创建 `src/trade/gate.rs`,先写入测试模块(实现 Step 3 补上):

```rust
//! 闸门:信号能否成为工单、给哪些账户、各多少股。纯函数,无 IO。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::*;
    use chrono::{NaiveDate, NaiveDateTime};

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }
    fn yesterday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
    }

    struct Fx {
        sig: NewSignal,
        quote: Option<Quote>,
        admission: Admission,
        rules: RiskRules,
        real: Option<AccountState>,
        paper: Option<AccountState>,
        real_pos: Option<Position>,
        paper_pos: Option<Position>,
        open: bool,
        last: Option<NaiveDateTime>,
        tickets_today: u32,
        pnl: f64,
    }

    fn account(a: Account, total: f64) -> AccountState {
        AccountState {
            user_id: 1,
            account: a,
            total_capital: total,
            available_cash: total,
        }
    }

    fn position(a: Account, qty: u64, bought: NaiveDate) -> Position {
        let mut p = Position::empty(1, a, "600000");
        p.qty = qty;
        p.avg_cost = 10.0;
        p.today_bought_qty = qty;
        p.last_buy_date = Some(bought);
        p
    }

    impl Fx {
        fn buy(source: SignalSource) -> Self {
            Fx {
                sig: NewSignal {
                    user_id: 1,
                    source,
                    strategy_id: None,
                    code: "600000".into(),
                    name: None,
                    side: Direction::Buy,
                    ref_price: 10.0,
                    reason: "r".into(),
                    ai_note: None,
                    dedup_key: "k".into(),
                    suggest_cash: None,
                    suggest_qty: None,
                },
                quote: Some(Quote {
                    code: "600000".into(),
                    price: 10.0,
                    limit_up: Some(11.0),
                    limit_down: Some(9.0),
                    ts: now(),
                }),
                admission: Admission::NotRequired,
                rules: RiskRules::default(),
                real: Some(account(Account::Real, 100_000.0)),
                paper: Some(account(Account::Paper, 100_000.0)),
                real_pos: None,
                paper_pos: None,
                open: false,
                last: None,
                tickets_today: 0,
                pnl: 0.0,
            }
        }

        fn sell(qty: u64, bought: NaiveDate) -> Self {
            let mut f = Fx::buy(SignalSource::Exit);
            f.sig.side = Direction::Sell;
            f.real_pos = Some(position(Account::Real, qty, bought));
            f.paper_pos = Some(position(Account::Paper, qty, bought));
            f
        }

        fn run(&self) -> GateDecision {
            evaluate(&GateInput {
                signal: &self.sig,
                quote: self.quote.as_ref(),
                admission: self.admission,
                rules: &self.rules,
                real_account: self.real.as_ref(),
                paper_account: self.paper.as_ref(),
                real_position: self.real_pos.as_ref(),
                paper_position: self.paper_pos.as_ref(),
                has_open_ticket: self.open,
                last_signal_at: self.last,
                tickets_today: self.tickets_today,
                realized_pnl_today: self.pnl,
                now: now(),
            })
        }
    }

    fn rejected(d: GateDecision) -> GateReject {
        match d {
            GateDecision::Reject(r) => r,
            GateDecision::Pass(p) => panic!("应被拒绝,实际通过 {p:?}"),
        }
    }

    fn plans(d: GateDecision) -> Vec<Plan> {
        match d {
            GateDecision::Pass(p) => p,
            GateDecision::Reject(r) => panic!("应通过,实际拒绝 {r:?}"),
        }
    }

    fn plan(account: Account, qty: u64) -> Plan {
        Plan {
            account,
            qty,
            price: 10.0,
        }
    }

    #[test]
    fn buy_passes_for_real_and_paper_sized_by_position_cap() {
        // 预算 = min(50000, 50000, 100000, 100000×20%) = 20000;执行价 10.01 → 1998 → 1900 股
        assert_eq!(
            plans(Fx::buy(SignalSource::Manual).run()),
            vec![plan(Account::Real, 1900), plan(Account::Paper, 1900)]
        );
    }

    #[test]
    fn rejections_in_rule_order() {
        let mut f = Fx::buy(SignalSource::Manual);
        f.rules.enabled = false;
        assert_eq!(rejected(f.run()), GateReject::TradingDisabled);

        let mut f = Fx::buy(SignalSource::Manual);
        f.open = true;
        assert_eq!(rejected(f.run()), GateReject::DuplicateOpenTicket);

        let mut f = Fx::buy(SignalSource::Strategy);
        f.last = Some(now() - chrono::Duration::minutes(30));
        assert_eq!(rejected(f.run()), GateReject::Cooldown);

        let mut f = Fx::buy(SignalSource::Strategy);
        f.admission = Admission::Blocked;
        assert_eq!(rejected(f.run()), GateReject::NotAdmitted);

        let mut f = Fx::buy(SignalSource::Manual);
        f.quote = None;
        assert_eq!(rejected(f.run()), GateReject::NoQuote);

        let mut f = Fx::buy(SignalSource::Manual);
        f.quote.as_mut().unwrap().limit_up = Some(10.0);
        assert_eq!(rejected(f.run()), GateReject::LimitUp);

        let mut f = Fx::buy(SignalSource::Manual);
        f.tickets_today = 20;
        assert_eq!(rejected(f.run()), GateReject::DailyTicketCap);

        let mut f = Fx::buy(SignalSource::Manual);
        f.pnl = -3_000.0;
        assert_eq!(rejected(f.run()), GateReject::DailyLossHalt);

        let mut f = Fx::buy(SignalSource::Manual);
        f.real = None;
        assert_eq!(rejected(f.run()), GateReject::NoCapital);

        let mut f = Fx::buy(SignalSource::Manual);
        let q = f.quote.as_mut().unwrap();
        q.price = 300.0;
        q.limit_up = Some(330.0);
        assert_eq!(rejected(f.run()), GateReject::BelowOneLot, "20000/300.3 不足一手");
    }

    #[test]
    fn exit_signals_ignore_cooldown_and_admission() {
        let mut f = Fx::sell(1000, yesterday());
        f.last = Some(now() - chrono::Duration::minutes(1));
        f.admission = Admission::Blocked;
        assert_eq!(
            plans(f.run()),
            vec![plan(Account::Real, 1000), plan(Account::Paper, 1000)]
        );
    }

    #[test]
    fn probation_strategy_trades_paper_only() {
        let mut f = Fx::buy(SignalSource::Strategy);
        f.admission = Admission::Probation;
        assert_eq!(plans(f.run()), vec![plan(Account::Paper, 1900)]);
    }

    #[test]
    fn loss_halt_blocks_buys_but_not_sells() {
        let mut f = Fx::sell(1000, yesterday());
        f.pnl = -5_000.0;
        assert_eq!(plans(f.run()).len(), 2);
    }

    #[test]
    fn sell_rules_t_plus_one_limit_down_and_partial() {
        let f = Fx::sell(1000, now().date());
        assert_eq!(rejected(f.run()), GateReject::NothingSellable, "今日买入不可卖");

        let mut f = Fx::sell(1000, yesterday());
        f.quote.as_mut().unwrap().limit_down = Some(10.0);
        assert_eq!(rejected(f.run()), GateReject::LimitDown);

        let mut f = Fx::sell(1000, yesterday());
        f.sig.suggest_qty = Some(250);
        assert_eq!(
            plans(f.run()),
            vec![plan(Account::Real, 200), plan(Account::Paper, 200)]
        );
    }

    #[test]
    fn existing_position_reduces_buy_budget() {
        // 已持 1500 股 × 10 = 15000;上限 20000 → 预算 5000 → 499 → 400 股
        let mut f = Fx::buy(SignalSource::Manual);
        f.real_pos = Some(position(Account::Real, 1500, yesterday()));
        assert_eq!(plans(f.run())[0], plan(Account::Real, 400));
    }

    #[test]
    fn missing_paper_account_is_skipped_not_rejected() {
        let mut f = Fx::buy(SignalSource::Manual);
        f.paper = None;
        assert_eq!(plans(f.run()), vec![plan(Account::Real, 1900)]);
    }

    #[test]
    fn size_buy_steps_down_for_fees() {
        assert_eq!(size_buy("600000", 10.0, 0.0, 1005.0), 0, "1000 + 5.01 > 1005");
        assert_eq!(size_buy("600000", 10.0, 0.0, 1005.01), 100);
        assert_eq!(size_buy("688001", 10.0, 0.0, 3000.0), 299);
        assert_eq!(size_buy("600000", 10.0, 0.0, -1.0), 0);
    }

    #[test]
    fn reject_reason_strings() {
        assert_eq!(GateReject::BelowOneLot.as_str(), "below_one_lot");
        assert_eq!(
            serde_json::to_string(&GateReject::DailyLossHalt).unwrap(),
            "\"daily_loss_halt\""
        );
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod gate;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::gate::tests`
Expected: 编译失败,`cannot find type Admission`

- [ ] **Step 3: 实现**

在 `src/trade/gate.rs` 模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::ashare::{buy_lot, price_decimals, round_buy_shares, sell_qty, step_down};
use crate::stock::fee::StockFee;
use crate::trade::model::{
    Account, AccountState, NewSignal, Position, Quote, RiskRules, SignalSource,
};
use chrono::NaiveDateTime;
use serde::Serialize;

/// 策略准入状态(由调用方提供;止盈止损与手动信号忽略此项)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// 非策略来源,或尚无准入机制
    NotRequired,
    /// 已准入:实盘 + 模拟盘
    Admitted,
    /// 观察期:仅模拟盘
    Probation,
    /// 未通过 / 已暂停 / 草稿
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateReject {
    TradingDisabled,
    DuplicateOpenTicket,
    Cooldown,
    NotAdmitted,
    NoQuote,
    LimitUp,
    LimitDown,
    DailyTicketCap,
    DailyLossHalt,
    NoCapital,
    BelowOneLot,
    NothingSellable,
}

impl GateReject {
    pub fn as_str(self) -> &'static str {
        match self {
            GateReject::TradingDisabled => "trading_disabled",
            GateReject::DuplicateOpenTicket => "duplicate_open_ticket",
            GateReject::Cooldown => "cooldown",
            GateReject::NotAdmitted => "not_admitted",
            GateReject::NoQuote => "no_quote",
            GateReject::LimitUp => "limit_up",
            GateReject::LimitDown => "limit_down",
            GateReject::DailyTicketCap => "daily_ticket_cap",
            GateReject::DailyLossHalt => "daily_loss_halt",
            GateReject::NoCapital => "no_capital",
            GateReject::BelowOneLot => "below_one_lot",
            GateReject::NothingSellable => "nothing_sellable",
        }
    }
}

pub struct GateInput<'a> {
    pub signal: &'a NewSignal,
    pub quote: Option<&'a Quote>,
    pub admission: Admission,
    pub rules: &'a RiskRules,
    pub real_account: Option<&'a AccountState>,
    pub paper_account: Option<&'a AccountState>,
    pub real_position: Option<&'a Position>,
    pub paper_position: Option<&'a Position>,
    /// 同用户同代码同方向是否已有未完结实盘工单
    pub has_open_ticket: bool,
    /// 同代码同方向最近一次成功生成工单的信号时间(冷却用)
    pub last_signal_at: Option<NaiveDateTime>,
    /// 今日已生成实盘工单数
    pub tickets_today: u32,
    /// 实盘今日已实现盈亏
    pub realized_pnl_today: f64,
    pub now: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub account: Account,
    pub qty: u64,
    /// 建议价(报价现价)
    pub price: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Pass(Vec<Plan>),
    Reject(GateReject),
}

pub fn evaluate(inp: &GateInput) -> GateDecision {
    use GateDecision::{Pass, Reject};
    let s = inp.signal;
    let rules = inp.rules;

    if !rules.enabled {
        return Reject(GateReject::TradingDisabled);
    }
    if inp.has_open_ticket {
        return Reject(GateReject::DuplicateOpenTicket);
    }
    if s.source != SignalSource::Exit {
        if let Some(last) = inp.last_signal_at {
            if inp.now - last < chrono::Duration::minutes(rules.cooldown_min) {
                return Reject(GateReject::Cooldown);
            }
        }
    }
    let accounts = match (s.source, inp.admission) {
        (SignalSource::Exit | SignalSource::Manual, _) => vec![Account::Real, Account::Paper],
        (_, Admission::NotRequired | Admission::Admitted) => vec![Account::Real, Account::Paper],
        (_, Admission::Probation) => vec![Account::Paper],
        (_, Admission::Blocked) => return Reject(GateReject::NotAdmitted),
    };
    let Some(q) = inp.quote.filter(|q| q.price.is_finite() && q.price > 0.0) else {
        return Reject(GateReject::NoQuote);
    };
    let eps = 0.5 * 10f64.powi(-price_decimals(&s.code));
    match s.side {
        Direction::Buy if q.limit_up.is_some_and(|u| q.price >= u - eps) => {
            return Reject(GateReject::LimitUp)
        }
        Direction::Sell if q.limit_down.is_some_and(|d| q.price <= d + eps) => {
            return Reject(GateReject::LimitDown)
        }
        _ => {}
    }
    let real_involved = accounts.contains(&Account::Real);
    if real_involved && inp.tickets_today >= rules.max_daily_tickets {
        return Reject(GateReject::DailyTicketCap);
    }
    if s.side == Direction::Buy && real_involved {
        if let Some(acc) = inp.real_account {
            if inp.realized_pnl_today < 0.0
                && -inp.realized_pnl_today >= acc.total_capital * rules.daily_loss_halt_pct
            {
                return Reject(GateReject::DailyLossHalt);
            }
        }
    }

    let today = inp.now.date();
    let mut plans = Vec::with_capacity(accounts.len());
    for (i, account) in accounts.iter().copied().enumerate() {
        let (acc, pos) = match account {
            Account::Real => (inp.real_account, inp.real_position),
            Account::Paper => (inp.paper_account, inp.paper_position),
        };
        let sized = match s.side {
            Direction::Buy => size_buy_for(s, q, rules, acc, pos),
            Direction::Sell => size_sell_for(s, pos, today),
        };
        match sized {
            Ok(qty) => plans.push(Plan {
                account,
                qty,
                price: q.price,
            }),
            Err(reason) if i == 0 => return Reject(reason),
            Err(_) => {}
        }
    }
    Pass(plans)
}

fn size_buy_for(
    s: &NewSignal,
    q: &Quote,
    rules: &RiskRules,
    acc: Option<&AccountState>,
    pos: Option<&Position>,
) -> Result<u64, GateReject> {
    let acc = acc.ok_or(GateReject::NoCapital)?;
    let held_value = pos.map_or(0.0, |p| p.qty as f64 * q.price);
    let budget = [
        s.suggest_cash.unwrap_or(rules.max_order_amount),
        rules.max_order_amount,
        acc.available_cash,
        acc.total_capital * rules.max_position_pct - held_value,
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    match size_buy(&s.code, q.price, rules.slippage, budget) {
        0 => Err(GateReject::BelowOneLot),
        n => Ok(n),
    }
}

fn size_sell_for(
    s: &NewSignal,
    pos: Option<&Position>,
    today: chrono::NaiveDate,
) -> Result<u64, GateReject> {
    let sellable = pos.map_or(0, |p| p.sellable(today));
    if sellable == 0 {
        return Err(GateReject::NothingSellable);
    }
    match sell_qty(&s.code, s.suggest_qty.unwrap_or(sellable), sellable) {
        0 => Err(GateReject::BelowOneLot),
        n => Ok(n),
    }
}

/// 预算内最多可买股数:按含滑点执行价与 A 股费用,整手向下取整。
pub fn size_buy(code: &str, price: f64, slippage: f64, budget: f64) -> u64 {
    if !(budget > 0.0 && price > 0.0) {
        return 0;
    }
    let fee = StockFee::a_share();
    let lot = buy_lot(code);
    let exec_price = price * (1.0 + slippage);
    let mut n = round_buy_shares(budget / exec_price, lot);
    while n > 0 {
        let value = n as f64 * exec_price;
        if value + fee.buy_fee(value) <= budget + 1e-9 {
            break;
        }
        n = step_down(n, lot);
    }
    n
}
```

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib trade::gate::tests`
Expected: 10 PASS

- [ ] **Step 5: Commit**

```bash
git add src/trade/gate.rs src/trade/mod.rs
git commit -m "feat(trade): 闸门纯函数(去重、冷却、准入、涨跌停、风控、仓位计算)"
```

---

### Task 6: 信号入库、工单状态机与成交回填

**Files:**
- Create: `src/trade/ticket.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod ticket;`)
- Test: `src/trade/ticket.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 3 model、Task 4 store(`get_risk_rules`、`get_position`、`upsert_position`、`add_cash`、`set_capital`)
- Produces:

```rust
pub enum Transition { Applied, AlreadyHandled }
pub struct NewTicket { user_id: i64, signal_id: i64, account: Account, code: String, side: Direction, suggest_price: f64, qty: u64, expires_at: NaiveDateTime, deviation_th: f64, status: TicketStatus, created_at: NaiveDateTime }
pub struct FillOutcome { fill_id: i64, status: TicketStatus, fee: f64, realized_pnl: Option<f64> }
pub fn default_expiry(source: SignalSource, now: NaiveDateTime) -> NaiveDateTime;
pub fn insert_signal(conn: &Connection, s: &NewSignal, now: NaiveDateTime) -> Result<Option<i64>>; // None = 重复
pub fn mark_signal(conn: &Connection, signal_id: i64, status: &str, reason: Option<&str>) -> Result<()>;
pub fn last_signal_at(conn: &Connection, user_id: i64, code: &str, side: Direction, exclude_id: i64) -> Result<Option<NaiveDateTime>>;
pub fn has_open_ticket(conn: &Connection, user_id: i64, code: &str, side: Direction) -> Result<bool>;
pub fn count_real_tickets_on(conn: &Connection, user_id: i64, day: NaiveDate) -> Result<u32>;
pub fn realized_pnl_on(conn: &Connection, user_id: i64, account: Account, day: NaiveDate) -> Result<f64>;
pub fn create_ticket(conn: &Connection, t: &NewTicket) -> Result<i64>;
pub fn get_ticket(conn: &Connection, id: i64) -> Result<Option<Ticket>>;
pub fn list_tickets(conn: &Connection, user_id: i64, statuses: &[TicketStatus]) -> Result<Vec<Ticket>>;
pub fn list_open_paper(conn: &Connection) -> Result<Vec<Ticket>>;          // 全体用户 confirmed/partial 模拟盘工单
pub fn confirm(conn: &Connection, user_id: i64, id: i64, now: NaiveDateTime) -> Result<Transition>;
pub fn ignore(conn: &Connection, user_id: i64, id: i64, reason: &str) -> Result<Transition>;
pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize>;
pub fn cancel_unfilled(conn: &Connection, created_before: NaiveDateTime) -> Result<usize>;
pub fn record_fill(conn: &mut Connection, user_id: i64, ticket_id: i64, price: f64, qty: u64, source: &str, now: NaiveDateTime) -> Result<FillOutcome>;
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/ticket.rs`,先写测试模块:

```rust
//! 信号入库去重、工单状态机、成交回填(更新持仓与资金)。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::store;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        c
    }

    fn signal(key: &str, side: Direction) -> NewSignal {
        NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: Some("浦发银行".into()),
            side,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        }
    }

    fn ticket(c: &Connection, side: Direction, qty: u64, status: TicketStatus, now: NaiveDateTime) -> i64 {
        let sid = insert_signal(c, &signal(&format!("k-{}", now), side), now)
            .unwrap()
            .unwrap();
        create_ticket(
            c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Real,
                code: "600000".into(),
                side,
                suggest_price: 10.0,
                qty,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status,
                created_at: now,
            },
        )
        .unwrap()
    }

    #[test]
    fn expiry_by_source() {
        assert_eq!(default_expiry(SignalSource::Exit, at(16, 9, 35)), at(16, 10, 5));
        assert_eq!(default_expiry(SignalSource::Mover, at(16, 10, 0)), at(16, 10, 10));
        assert_eq!(default_expiry(SignalSource::Strategy, at(16, 9, 25)), at(16, 10, 30));
        assert_eq!(default_expiry(SignalSource::Strategy, at(16, 11, 0)), at(16, 12, 0));
        assert_eq!(default_expiry(SignalSource::Manual, at(16, 10, 0)), at(16, 15, 0));
        assert_eq!(default_expiry(SignalSource::Manual, at(16, 15, 20)), at(16, 15, 50));
    }

    #[test]
    fn insert_signal_dedups_and_cooldown_query() {
        let c = db();
        let s = signal("dup", Direction::Buy);
        let id = insert_signal(&c, &s, at(15, 10, 0)).unwrap().unwrap();
        assert!(insert_signal(&c, &s, at(15, 10, 1)).unwrap().is_none());
        assert!(last_signal_at(&c, 1, "600000", Direction::Buy, -1).unwrap().is_none(), "未生成工单不计冷却");
        mark_signal(&c, id, "ticketed", None).unwrap();
        assert_eq!(last_signal_at(&c, 1, "600000", Direction::Buy, -1).unwrap(), Some(at(15, 10, 0)));
        assert!(last_signal_at(&c, 1, "600000", Direction::Buy, id).unwrap().is_none(), "排除自身");
    }

    #[test]
    fn confirm_ignore_and_idempotency() {
        let c = db();
        let t = ticket(&c, Direction::Buy, 1000, TicketStatus::Pending, at(15, 10, 0));
        assert_eq!(confirm(&c, 2, t, at(15, 10, 1)).unwrap(), Transition::AlreadyHandled, "他人工单");
        assert_eq!(confirm(&c, 1, t, at(15, 10, 1)).unwrap(), Transition::Applied);
        assert_eq!(confirm(&c, 1, t, at(15, 10, 2)).unwrap(), Transition::AlreadyHandled, "重复确认");
        assert_eq!(get_ticket(&c, t).unwrap().unwrap().status, TicketStatus::Confirmed);
        assert!(has_open_ticket(&c, 1, "600000", Direction::Buy).unwrap());

        let t2 = ticket(&c, Direction::Buy, 100, TicketStatus::Pending, at(15, 11, 0));
        assert_eq!(confirm(&c, 1, t2, at(15, 11, 31)).unwrap(), Transition::AlreadyHandled, "已过有效期");
        assert_eq!(ignore(&c, 1, t2, "不看好").unwrap(), Transition::Applied);
        let got = get_ticket(&c, t2).unwrap().unwrap();
        assert_eq!((got.status, got.ignore_reason.as_deref()), (TicketStatus::Rejected, Some("不看好")));
        assert_eq!(count_real_tickets_on(&c, 1, at(15, 0, 0).date()).unwrap(), 2);
    }

    #[test]
    fn expire_and_cancel_batches() {
        let c = db();
        let p = ticket(&c, Direction::Buy, 100, TicketStatus::Pending, at(15, 10, 0));
        let k = ticket(&c, Direction::Buy, 100, TicketStatus::Confirmed, at(15, 10, 5));
        assert_eq!(expire_due(&c, at(15, 10, 20)).unwrap(), 0);
        assert_eq!(expire_due(&c, at(15, 10, 30)).unwrap(), 1);
        assert_eq!(get_ticket(&c, p).unwrap().unwrap().status, TicketStatus::Expired);
        assert_eq!(cancel_unfilled(&c, at(16, 0, 0)).unwrap(), 1);
        assert_eq!(get_ticket(&c, k).unwrap().unwrap().status, TicketStatus::Cancelled);
        assert_eq!(list_tickets(&c, 1, &[TicketStatus::Cancelled]).unwrap().len(), 1);
    }

    #[test]
    fn buy_fill_creates_position_with_fee_in_cost_and_default_exits() {
        let mut c = db();
        let t = ticket(&c, Direction::Buy, 1000, TicketStatus::Confirmed, at(15, 10, 0));
        let out = record_fill(&mut c, 1, t, 10.0, 600, "manual", at(15, 10, 10)).unwrap();
        assert_eq!(out.status, TicketStatus::Partial);
        let out = record_fill(&mut c, 1, t, 10.0, 400, "manual", at(15, 10, 20)).unwrap();
        assert_eq!(out.status, TicketStatus::Filled);
        assert!(record_fill(&mut c, 1, t, 10.0, 1, "manual", at(15, 10, 30)).is_err(), "已成交不可再回填");

        let p = store::get_position(&c, 1, Account::Real, "600000").unwrap().unwrap();
        assert_eq!((p.qty, p.today_bought_qty), (1000, 1000));
        // 两笔:6000 费 5.06;4000 费 5.04 → 成本 (10000 + 10.1) / 1000
        assert!((p.avg_cost - 10.0101).abs() < 1e-9, "avg={}", p.avg_cost);
        assert_eq!((p.stop_loss, p.take_profit), (Some(9.2), Some(12.0)));
        let a = store::get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!((a.available_cash - (100_000.0 - 10_010.1)).abs() < 1e-6);
    }

    #[test]
    fn fill_validation_errors() {
        let mut c = db();
        let pending = ticket(&c, Direction::Buy, 1000, TicketStatus::Pending, at(15, 10, 0));
        assert!(record_fill(&mut c, 1, pending, 10.0, 100, "manual", at(15, 10, 1)).is_err(), "未确认");
        let t = ticket(&c, Direction::Buy, 1000, TicketStatus::Confirmed, at(15, 10, 2));
        assert!(record_fill(&mut c, 1, t, 10.0, 1001, "manual", at(15, 10, 3)).is_err(), "超量");
        assert!(record_fill(&mut c, 1, t, 0.0, 100, "manual", at(15, 10, 3)).is_err(), "价格非正");
        assert!(record_fill(&mut c, 2, t, 10.0, 100, "manual", at(15, 10, 3)).is_err(), "他人工单");
    }

    #[test]
    fn sell_fill_respects_t_plus_one_and_realizes_pnl() {
        let mut c = db();
        let b = ticket(&c, Direction::Buy, 1000, TicketStatus::Confirmed, at(15, 10, 0));
        record_fill(&mut c, 1, b, 10.0, 1000, "manual", at(15, 10, 10)).unwrap();

        let s1 = ticket(&c, Direction::Sell, 1000, TicketStatus::Confirmed, at(15, 14, 0));
        assert!(record_fill(&mut c, 1, s1, 11.0, 1000, "manual", at(15, 14, 1)).is_err(), "T+1");

        let s2 = ticket(&c, Direction::Sell, 1000, TicketStatus::Confirmed, at(16, 10, 0));
        let out = record_fill(&mut c, 1, s2, 11.0, 1000, "manual", at(16, 10, 1)).unwrap();
        // 成本 10.0051;卖出费 5 + 5.5 + 0.11 = 10.61 → (11 − 10.0051) × 1000 − 10.61 = 984.29
        assert!((out.realized_pnl.unwrap() - 984.29).abs() < 1e-6, "{:?}", out.realized_pnl);
        assert!(store::get_position(&c, 1, Account::Real, "600000").unwrap().is_none());
        assert!((realized_pnl_on(&c, 1, Account::Real, at(16, 0, 0).date()).unwrap() - 984.29).abs() < 1e-6);
        let a = store::get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!((a.available_cash - 100_984.29).abs() < 1e-6, "cash={}", a.available_cash);
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod ticket;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::ticket::tests`
Expected: 编译失败,`cannot find function insert_signal`

- [ ] **Step 3: 实现**

在 `src/trade/ticket.rs` 模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::fee::StockFee;
use crate::trade::model::{
    fmt_ts, parse_side, parse_ts, side_str, Account, NewSignal, Position, SignalSource, Ticket,
    TicketStatus, DATE_FMT,
};
use crate::trade::store;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Applied,
    AlreadyHandled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewTicket {
    pub user_id: i64,
    pub signal_id: i64,
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub suggest_price: f64,
    pub qty: u64,
    pub expires_at: NaiveDateTime,
    pub deviation_th: f64,
    pub status: TicketStatus,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillOutcome {
    pub fill_id: i64,
    pub status: TicketStatus,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
}

fn at_time(now: NaiveDateTime, h: u32, m: u32) -> NaiveDateTime {
    now.date().and_time(NaiveTime::from_hms_opt(h, m, 0).expect("合法时刻"))
}

/// 工单默认有效期(spec §5)。
pub fn default_expiry(source: SignalSource, now: NaiveDateTime) -> NaiveDateTime {
    use chrono::Duration;
    match source {
        SignalSource::Exit => now + Duration::minutes(30),
        SignalSource::Mover => now + Duration::minutes(10),
        SignalSource::Strategy => {
            let end = at_time(now, 10, 30);
            if now < end {
                end
            } else {
                now + Duration::minutes(60)
            }
        }
        SignalSource::Manual => {
            let end = at_time(now, 15, 0);
            if now < end {
                end
            } else {
                now + Duration::minutes(30)
            }
        }
    }
}

pub fn insert_signal(conn: &Connection, s: &NewSignal, now: NaiveDateTime) -> Result<Option<i64>> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO trade_signals (user_id, source, strategy_id, code, name, side,
           ref_price, reason, ai_note, dedup_key, suggest_cash, suggest_qty, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'new', ?13)",
        params![
            s.user_id,
            s.source.as_str(),
            s.strategy_id,
            s.code,
            s.name,
            side_str(s.side),
            s.ref_price,
            s.reason,
            s.ai_note,
            s.dedup_key,
            s.suggest_cash,
            s.suggest_qty.map(|q| q as i64),
            fmt_ts(now),
        ],
    )?;
    Ok((n > 0).then(|| conn.last_insert_rowid()))
}

/// status:"ticketed" | "rejected"
pub fn mark_signal(conn: &Connection, signal_id: i64, status: &str, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE trade_signals SET status = ?1, reject_reason = ?2 WHERE id = ?3",
        params![status, reason, signal_id],
    )?;
    Ok(())
}

pub fn last_signal_at(
    conn: &Connection,
    user_id: i64,
    code: &str,
    side: Direction,
    exclude_id: i64,
) -> Result<Option<NaiveDateTime>> {
    let s: Option<String> = conn.query_row(
        "SELECT MAX(created_at) FROM trade_signals
         WHERE user_id = ?1 AND code = ?2 AND side = ?3 AND status = 'ticketed' AND id <> ?4",
        params![user_id, code, side_str(side), exclude_id],
        |r| r.get(0),
    )?;
    s.as_deref().map(parse_ts).transpose()
}

pub fn has_open_ticket(conn: &Connection, user_id: i64, code: &str, side: Direction) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM trade_tickets
           WHERE user_id = ?1 AND code = ?2 AND side = ?3 AND account = 'real'
             AND status IN ('pending', 'confirmed', 'partial'))",
        params![user_id, code, side_str(side)],
        |r| r.get(0),
    )?)
}

pub fn count_real_tickets_on(conn: &Connection, user_id: i64, day: NaiveDate) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM trade_tickets
         WHERE user_id = ?1 AND account = 'real' AND substr(created_at, 1, 10) = ?2",
        params![user_id, day.format(DATE_FMT).to_string()],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u32)
}

pub fn realized_pnl_on(conn: &Connection, user_id: i64, account: Account, day: NaiveDate) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(realized_pnl), 0.0) FROM trade_fills
         WHERE user_id = ?1 AND account = ?2 AND substr(filled_at, 1, 10) = ?3",
        params![user_id, account.as_str(), day.format(DATE_FMT).to_string()],
        |r| r.get(0),
    )?)
}

pub fn create_ticket(conn: &Connection, t: &NewTicket) -> Result<i64> {
    conn.execute(
        "INSERT INTO trade_tickets (user_id, signal_id, account, code, side, suggest_price, qty,
           filled_qty, expires_at, deviation_th, status, urgency, created_at, confirmed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, 0, ?11, ?12)",
        params![
            t.user_id,
            t.signal_id,
            t.account.as_str(),
            t.code,
            side_str(t.side),
            t.suggest_price,
            t.qty as i64,
            fmt_ts(t.expires_at),
            t.deviation_th,
            t.status.as_str(),
            fmt_ts(t.created_at),
            (t.status == TicketStatus::Confirmed).then(|| fmt_ts(t.created_at)),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

const TICKET_COLS: &str = "id, user_id, signal_id, account, code, side, suggest_price, qty, \
    filled_qty, expires_at, deviation_th, status, urgency, created_at, confirmed_at, ignore_reason";

struct RawTicket {
    id: i64,
    user_id: i64,
    signal_id: i64,
    account: String,
    code: String,
    side: String,
    suggest_price: f64,
    qty: i64,
    filled_qty: i64,
    expires_at: String,
    deviation_th: f64,
    status: String,
    urgency: i64,
    created_at: String,
    confirmed_at: Option<String>,
    ignore_reason: Option<String>,
}

fn read_raw_ticket(r: &Row) -> rusqlite::Result<RawTicket> {
    Ok(RawTicket {
        id: r.get(0)?,
        user_id: r.get(1)?,
        signal_id: r.get(2)?,
        account: r.get(3)?,
        code: r.get(4)?,
        side: r.get(5)?,
        suggest_price: r.get(6)?,
        qty: r.get(7)?,
        filled_qty: r.get(8)?,
        expires_at: r.get(9)?,
        deviation_th: r.get(10)?,
        status: r.get(11)?,
        urgency: r.get(12)?,
        created_at: r.get(13)?,
        confirmed_at: r.get(14)?,
        ignore_reason: r.get(15)?,
    })
}

impl RawTicket {
    fn into_ticket(self) -> Result<Ticket> {
        Ok(Ticket {
            id: self.id,
            user_id: self.user_id,
            signal_id: self.signal_id,
            account: Account::parse(&self.account)?,
            code: self.code,
            side: parse_side(&self.side)?,
            suggest_price: self.suggest_price,
            qty: self.qty.max(0) as u64,
            filled_qty: self.filled_qty.max(0) as u64,
            expires_at: parse_ts(&self.expires_at)?,
            deviation_th: self.deviation_th,
            status: TicketStatus::parse(&self.status)?,
            urgency: self.urgency,
            created_at: parse_ts(&self.created_at)?,
            confirmed_at: self.confirmed_at.as_deref().map(parse_ts).transpose()?,
            ignore_reason: self.ignore_reason,
        })
    }
}

pub fn get_ticket(conn: &Connection, id: i64) -> Result<Option<Ticket>> {
    conn.query_row(
        &format!("SELECT {TICKET_COLS} FROM trade_tickets WHERE id = ?1"),
        [id],
        read_raw_ticket,
    )
    .optional()?
    .map(RawTicket::into_ticket)
    .transpose()
}

fn query_tickets(conn: &Connection, sql_where: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Ticket>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TICKET_COLS} FROM trade_tickets WHERE {sql_where} ORDER BY id"
    ))?;
    let raws = stmt
        .query_map(args, read_raw_ticket)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(RawTicket::into_ticket).collect()
}

pub fn list_tickets(conn: &Connection, user_id: i64, statuses: &[TicketStatus]) -> Result<Vec<Ticket>> {
    let all = query_tickets(conn, "user_id = ?1", params![user_id])?;
    Ok(all
        .into_iter()
        .filter(|t| statuses.is_empty() || statuses.contains(&t.status))
        .collect())
}

pub fn list_open_paper(conn: &Connection) -> Result<Vec<Ticket>> {
    query_tickets(
        conn,
        "account = 'paper' AND status IN ('confirmed', 'partial')",
        params![],
    )
}

pub fn confirm(conn: &Connection, user_id: i64, id: i64, now: NaiveDateTime) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_tickets SET status = 'confirmed', confirmed_at = ?1
         WHERE id = ?2 AND user_id = ?3 AND status = 'pending' AND expires_at > ?1",
        params![fmt_ts(now), id, user_id],
    )?;
    Ok(if n > 0 { Transition::Applied } else { Transition::AlreadyHandled })
}

pub fn ignore(conn: &Connection, user_id: i64, id: i64, reason: &str) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_tickets SET status = 'rejected', ignore_reason = ?1
         WHERE id = ?2 AND user_id = ?3 AND status = 'pending'",
        params![reason, id, user_id],
    )?;
    Ok(if n > 0 { Transition::Applied } else { Transition::AlreadyHandled })
}

pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'expired' WHERE status = 'pending' AND expires_at <= ?1",
        [fmt_ts(now)],
    )?)
}

pub fn cancel_unfilled(conn: &Connection, created_before: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'cancelled'
         WHERE account = 'real' AND status IN ('confirmed', 'partial') AND created_at < ?1",
        [fmt_ts(created_before)],
    )?)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// 回填一笔成交:校验 → 更新持仓 / 资金 → 写成交 → 推进工单状态。单事务。
pub fn record_fill(
    conn: &mut Connection,
    user_id: i64,
    ticket_id: i64,
    price: f64,
    qty: u64,
    source: &str,
    now: NaiveDateTime,
) -> Result<FillOutcome> {
    let tx = conn.transaction()?;
    let t = get_ticket(&tx, ticket_id)?
        .filter(|t| t.user_id == user_id)
        .ok_or_else(|| anyhow!("工单不存在"))?;
    if !matches!(t.status, TicketStatus::Confirmed | TicketStatus::Partial) {
        return Err(anyhow!("工单状态为 {},不可回填成交", t.status.as_str()));
    }
    if !(price.is_finite() && price > 0.0) || qty == 0 {
        return Err(anyhow!("成交价与数量必须为正数"));
    }
    if t.filled_qty + qty > t.qty {
        return Err(anyhow!("成交数量超过工单剩余数量({})", t.qty - t.filled_qty));
    }
    let today = now.date();
    let rules = store::get_risk_rules(&tx, user_id)?;
    let fee_model = StockFee::a_share();
    let value = price * qty as f64;
    let pos = store::get_position(&tx, user_id, t.account, &t.code)?;

    let (fee, realized, new_pos, cash_delta) = match t.side {
        Direction::Buy => {
            let fee = fee_model.buy_fee(value);
            let mut p = pos.unwrap_or_else(|| Position::empty(user_id, t.account, &t.code));
            let was_empty = p.qty == 0;
            let new_qty = p.qty + qty;
            p.avg_cost = (p.avg_cost * p.qty as f64 + value + fee) / new_qty as f64;
            p.today_bought_qty = if p.last_buy_date == Some(today) {
                p.today_bought_qty + qty
            } else {
                qty
            };
            p.last_buy_date = Some(today);
            p.qty = new_qty;
            if was_empty {
                p.stop_loss = Some(round2(price * (1.0 - rules.default_stop_loss_pct)));
                p.take_profit = Some(round2(price * (1.0 + rules.default_take_profit_pct)));
                p.trailing_high = None;
            }
            (fee, None, p, -(value + fee))
        }
        Direction::Sell => {
            let mut p = pos.ok_or_else(|| anyhow!("无持仓,不可卖出"))?;
            let sellable = p.sellable(today);
            if qty > sellable {
                return Err(anyhow!("卖出数量超过可卖数量 {sellable}(T+1)"));
            }
            let fee = fee_model.sell_fee(qty as f64, price, 0);
            let realized = (price - p.avg_cost) * qty as f64 - fee;
            p.qty -= qty;
            p.today_bought_qty = p.today_bought_qty.min(p.qty);
            (fee, Some(realized), p, value - fee)
        }
    };

    store::add_cash(&tx, user_id, t.account, cash_delta, now)?;
    store::upsert_position(&tx, &new_pos, now)?;
    tx.execute(
        "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee,
           realized_pnl, source, filled_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            t.id,
            user_id,
            t.account.as_str(),
            t.code,
            side_str(t.side),
            price,
            qty as i64,
            fee,
            realized,
            source,
            fmt_ts(now),
        ],
    )?;
    let fill_id = tx.last_insert_rowid();
    let filled = t.filled_qty + qty;
    let status = if filled == t.qty {
        TicketStatus::Filled
    } else {
        TicketStatus::Partial
    };
    tx.execute(
        "UPDATE trade_tickets SET filled_qty = ?1, status = ?2 WHERE id = ?3",
        params![filled as i64, status.as_str(), t.id],
    )?;
    tx.commit()?;
    Ok(FillOutcome {
        fill_id,
        status,
        fee,
        realized_pnl: realized,
    })
}
```

> 测试模块通过 `use super::*;` 获得上面导入的 `Account`、`Direction`、`NaiveDate` 等;若编译器提示某个名称未导入测试模块,在测试模块内显式 `use`,不要改动实现的导入。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib trade::ticket::tests`
Expected: 7 PASS

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 仅 3 个既有问题

- [ ] **Step 5: Commit**

```bash
git add src/trade/ticket.rs src/trade/mod.rs
git commit -m "feat(trade): 信号入库去重、工单状态机与成交回填"
```

---

### Task 7: 模拟盘自动成交与信号提交全流程

**Files:**
- Create: `src/trade/router.rs`、`src/trade/service.rs`、`tests/trade_core.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod router;`、`pub mod service;`)
- Test: `src/trade/router.rs` 内 `mod tests`、`tests/trade_core.rs`

**Interfaces:**
- Consumes: Task 2 `ashare::{price_decimals, slipped_price}`;Task 4 store;Task 5 `gate::{evaluate, GateInput, GateDecision, Admission, GateReject}`;Task 6 ticket 全部
- Produces:

```rust
// router.rs
pub enum RouteOutcome { Filled { fill_id: i64 }, Waiting, NotFillable }
pub fn fill_paper_ticket(conn: &mut Connection, ticket: &Ticket, quote: &Quote, slippage: f64, now: NaiveDateTime) -> Result<RouteOutcome>;
pub fn fill_pending_paper(conn: &mut Connection, quotes: &HashMap<String, Quote>, now: NaiveDateTime) -> Result<usize>;
// service.rs
pub struct SubmitContext<'a> { quote: Option<&'a Quote>, admission: Admission, now: NaiveDateTime }
pub enum SubmitOutcome { Duplicate, Rejected { signal_id: i64, reason: GateReject }, Ticketed { signal_id: i64, real_ticket: Option<i64>, paper_ticket: Option<i64> } }
pub fn submit_signal(conn: &mut Connection, sig: &NewSignal, ctx: &SubmitContext) -> Result<SubmitOutcome>;
```

- [ ] **Step 1: 写 router 失败测试与集成失败测试**

创建 `src/trade/router.rs`:

```rust
//! 模拟盘自动成交:按报价 ± 滑点(取整到最小价位、不越涨跌停)成交;涨跌停或 T+1 时等待下一次报价。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::NewSignal;
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use crate::trade::{store, ticket};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn paper_ticket(c: &Connection, side: Direction, qty: u64, now: NaiveDateTime) -> Ticket {
        let sid = insert_signal(
            c,
            &NewSignal {
                user_id: 1,
                source: crate::trade::model::SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side,
                ref_price: 10.0,
                reason: "r".into(),
                ai_note: None,
                dedup_key: format!("{now}-{side:?}"),
                suggest_cash: None,
                suggest_qty: None,
            },
            now,
        )
        .unwrap()
        .unwrap();
        let id = create_ticket(
            c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Paper,
                code: "600000".into(),
                side,
                suggest_price: 10.0,
                qty,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                created_at: now,
            },
        )
        .unwrap();
        ticket::get_ticket(c, id).unwrap().unwrap()
    }

    fn quote(price: f64, up: Option<f64>, down: Option<f64>, now: NaiveDateTime) -> Quote {
        Quote {
            code: "600000".into(),
            price,
            limit_up: up,
            limit_down: down,
            ts: now,
        }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Paper, 100_000.0, at(15, 9, 0)).unwrap();
        c
    }

    #[test]
    fn buy_fills_at_slipped_tick_price_and_waits_on_limit_up() {
        let mut c = db();
        let t = paper_ticket(&c, Direction::Buy, 1000, at(15, 10, 0));
        let wait = fill_paper_ticket(&mut c, &t, &quote(11.0, Some(11.0), None, at(15, 10, 0)), 0.001, at(15, 10, 0)).unwrap();
        assert_eq!(wait, RouteOutcome::Waiting, "涨停买不进,等待");
        let out = fill_paper_ticket(&mut c, &t, &quote(10.0, Some(11.0), None, at(15, 10, 1)), 0.001, at(15, 10, 1)).unwrap();
        assert!(matches!(out, RouteOutcome::Filled { .. }));
        let p = store::get_position(&c, 1, Account::Paper, "600000").unwrap().unwrap();
        assert_eq!(p.qty, 1000);
        // 10 × 1.001 → 10.01;费 max(2.5025, 5) + 0.1001
        assert!((p.avg_cost - (10_010.0 + 5.1001) / 1000.0).abs() < 1e-9, "avg={}", p.avg_cost);
        let again = ticket::get_ticket(&c, t.id).unwrap().unwrap();
        assert_eq!(fill_paper_ticket(&mut c, &again, &quote(10.0, None, None, at(15, 10, 2)), 0.001, at(15, 10, 2)).unwrap(), RouteOutcome::NotFillable);
    }

    #[test]
    fn sell_waits_for_t_plus_one_then_fills_and_batch_counts() {
        let mut c = db();
        let b = paper_ticket(&c, Direction::Buy, 1000, at(15, 10, 0));
        fill_paper_ticket(&mut c, &b, &quote(10.0, None, None, at(15, 10, 0)), 0.0, at(15, 10, 0)).unwrap();
        let s = paper_ticket(&c, Direction::Sell, 1000, at(15, 11, 0));
        let mut quotes = HashMap::new();
        quotes.insert("600000".to_string(), quote(10.5, None, Some(9.0), at(15, 11, 0)));
        assert_eq!(fill_pending_paper(&mut c, &quotes, at(15, 11, 0)).unwrap(), 0, "T+1 等待");
        assert_eq!(fill_pending_paper(&mut c, &quotes, at(16, 9, 31)).unwrap(), 1);
        assert_eq!(ticket::get_ticket(&c, s.id).unwrap().unwrap().status, TicketStatus::Filled);
        assert!(store::get_position(&c, 1, Account::Paper, "600000").unwrap().is_none());
    }
}
```

创建 `tests/trade_core.rs`:

```rust
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use xlh::event::Direction;
use xlh::trade::gate::{Admission, GateReject};
use xlh::trade::model::{Account, NewSignal, Quote, SignalSource, TicketStatus};
use xlh::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use xlh::trade::{store, ticket};

fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
}

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    store::migrate(&c).unwrap();
    store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
    c
}

fn signal(source: SignalSource, side: Direction, key: &str) -> NewSignal {
    NewSignal {
        user_id: 1,
        source,
        strategy_id: None,
        code: "600000".into(),
        name: Some("浦发银行".into()),
        side,
        ref_price: 10.0,
        reason: "集成测试".into(),
        ai_note: None,
        dedup_key: key.into(),
        suggest_cash: None,
        suggest_qty: None,
    }
}

fn quote(price: f64, up: Option<f64>, down: Option<f64>, ts: NaiveDateTime) -> Quote {
    Quote { code: "600000".into(), price, limit_up: up, limit_down: down, ts }
}

#[test]
fn manual_buy_confirm_fill_then_exit_sell_next_day() {
    let mut c = db();
    let q = quote(10.0, Some(11.0), Some(9.0), at(15, 10, 0));
    let buy = signal(SignalSource::Manual, Direction::Buy, "manual-1");
    let ctx = SubmitContext { quote: Some(&q), admission: Admission::NotRequired, now: at(15, 10, 0) };

    let SubmitOutcome::Ticketed { real_ticket: Some(rt), paper_ticket: Some(pt), .. } =
        submit_signal(&mut c, &buy, &ctx).unwrap()
    else {
        panic!("应生成实盘与模拟盘工单");
    };
    assert_eq!(submit_signal(&mut c, &buy, &ctx).unwrap(), SubmitOutcome::Duplicate);

    let real = ticket::get_ticket(&c, rt).unwrap().unwrap();
    assert_eq!((real.status, real.qty), (TicketStatus::Pending, 1900));
    assert_eq!(real.expires_at, at(15, 15, 0));
    let paper = ticket::get_ticket(&c, pt).unwrap().unwrap();
    assert_eq!(paper.status, TicketStatus::Filled, "模拟盘按报价立即成交");
    let paper_pos = store::get_position(&c, 1, Account::Paper, "600000").unwrap().unwrap();
    assert_eq!(paper_pos.qty, 1900);

    assert_eq!(ticket::confirm(&c, 1, rt, at(15, 10, 5)).unwrap(), ticket::Transition::Applied);
    let out = ticket::record_fill(&mut c, 1, rt, 10.0, 1900, "manual", at(15, 10, 20)).unwrap();
    assert_eq!(out.status, TicketStatus::Filled);
    let pos = store::get_position(&c, 1, Account::Real, "600000").unwrap().unwrap();
    assert_eq!((pos.qty, pos.stop_loss, pos.take_profit), (1900, Some(9.2), Some(12.0)));
    let cash = store::get_account(&c, 1, Account::Real).unwrap().unwrap().available_cash;
    assert!((cash - (100_000.0 - 19_000.0 - 5.19)).abs() < 1e-6, "cash={cash}");

    // 当日触发止损:T+1 不可卖
    let q2 = quote(9.1, Some(11.0), Some(9.0), at(15, 14, 0));
    let exit_today = signal(SignalSource::Exit, Direction::Sell, "exit-600000-stop-2026-09-15");
    let ctx2 = SubmitContext { quote: Some(&q2), admission: Admission::NotRequired, now: at(15, 14, 0) };
    assert!(matches!(
        submit_signal(&mut c, &exit_today, &ctx2).unwrap(),
        SubmitOutcome::Rejected { reason: GateReject::NothingSellable, .. }
    ));

    // 次日:生成实盘卖出工单,模拟盘直接卖出
    let q3 = quote(9.1, Some(10.01), Some(9.0), at(16, 9, 35));
    let exit_next = signal(SignalSource::Exit, Direction::Sell, "exit-600000-stop-2026-09-16");
    let ctx3 = SubmitContext { quote: Some(&q3), admission: Admission::NotRequired, now: at(16, 9, 35) };
    let SubmitOutcome::Ticketed { real_ticket: Some(sell_rt), paper_ticket: Some(_), .. } =
        submit_signal(&mut c, &exit_next, &ctx3).unwrap()
    else {
        panic!("次日应生成卖出工单");
    };
    assert_eq!(ticket::get_ticket(&c, sell_rt).unwrap().unwrap().qty, 1900);
    assert!(store::get_position(&c, 1, Account::Paper, "600000").unwrap().is_none(), "模拟盘已卖出");

    assert_eq!(ticket::expire_due(&c, at(16, 10, 30)).unwrap(), 1);
    assert_eq!(ticket::get_ticket(&c, sell_rt).unwrap().unwrap().status, TicketStatus::Expired);
}

#[test]
fn strategy_admission_controls_accounts() {
    let mut c = db();
    let q = quote(10.0, Some(11.0), Some(9.0), at(15, 10, 0));

    let probation = signal(SignalSource::Strategy, Direction::Buy, "strategy-probation");
    let ctx = SubmitContext { quote: Some(&q), admission: Admission::Probation, now: at(15, 10, 0) };
    assert!(matches!(
        submit_signal(&mut c, &probation, &ctx).unwrap(),
        SubmitOutcome::Ticketed { real_ticket: None, paper_ticket: Some(_), .. }
    ));

    let mut other = signal(SignalSource::Strategy, Direction::Buy, "strategy-blocked");
    other.code = "600036".into();
    let q_other = Quote { code: "600036".into(), ..q.clone() };
    let ctx_blocked = SubmitContext { quote: Some(&q_other), admission: Admission::Blocked, now: at(15, 10, 0) };
    let SubmitOutcome::Rejected { signal_id, reason } = submit_signal(&mut c, &other, &ctx_blocked).unwrap() else {
        panic!("未准入应拒绝");
    };
    assert_eq!(reason, GateReject::NotAdmitted);
    let (status, why): (String, Option<String>) = c
        .query_row("SELECT status, reject_reason FROM trade_signals WHERE id = ?1", [signal_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((status.as_str(), why.as_deref()), ("rejected", Some("not_admitted")));
}
```

在 `src/trade/mod.rs` 加 `pub mod router;` 与 `pub mod service;`,并创建只含模块注释的 `src/trade/service.rs`:

```rust
//! 信号提交全流程:入库去重 → 收集闸门输入 → 判定 → 生成工单 → 模拟盘即时成交。
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::router::tests`
Expected: 编译失败,`cannot find function fill_paper_ticket`

Run: `cargo test --test trade_core`
Expected: 编译失败,`cannot find ... submit_signal`

- [ ] **Step 3: 实现 router**

在 `src/trade/router.rs` 模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::event::Direction;
use crate::stock::ashare::{price_decimals, slipped_price};
use crate::trade::model::{Account, Quote, Ticket, TicketStatus};
use crate::trade::{store, ticket};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOutcome {
    Filled { fill_id: i64 },
    /// 涨跌停 / 无可卖 / 报价无效:保持待成交,等下一次报价
    Waiting,
    /// 工单已非待成交状态
    NotFillable,
}

pub fn fill_paper_ticket(
    conn: &mut Connection,
    t: &Ticket,
    quote: &Quote,
    slippage: f64,
    now: NaiveDateTime,
) -> Result<RouteOutcome> {
    if t.account != Account::Paper {
        return Err(anyhow!("仅模拟盘工单可自动成交"));
    }
    let Some(current) = ticket::get_ticket(conn, t.id)? else {
        return Ok(RouteOutcome::NotFillable);
    };
    if !matches!(current.status, TicketStatus::Confirmed | TicketStatus::Partial) {
        return Ok(RouteOutcome::NotFillable);
    }
    if !(quote.price.is_finite() && quote.price > 0.0) {
        return Ok(RouteOutcome::Waiting);
    }
    let decimals = price_decimals(&current.code);
    let eps = 0.5 * 10f64.powi(-decimals);
    let hit_limit = match current.side {
        Direction::Buy => quote.limit_up.is_some_and(|u| quote.price >= u - eps),
        Direction::Sell => quote.limit_down.is_some_and(|d| quote.price <= d + eps),
    };
    if hit_limit {
        return Ok(RouteOutcome::Waiting);
    }
    let price = slipped_price(
        current.side,
        quote.price,
        slippage,
        decimals,
        quote.limit_up.unwrap_or(f64::INFINITY),
        quote.limit_down.unwrap_or(0.0),
    );
    let mut qty = current.qty - current.filled_qty;
    if current.side == Direction::Sell {
        let sellable = store::get_position(conn, current.user_id, Account::Paper, &current.code)?
            .map_or(0, |p| p.sellable(now.date()));
        qty = qty.min(sellable);
    }
    if qty == 0 {
        return Ok(RouteOutcome::Waiting);
    }
    let out = ticket::record_fill(conn, current.user_id, current.id, price, qty, "paper", now)?;
    Ok(RouteOutcome::Filled {
        fill_id: out.fill_id,
    })
}

/// 用最新一批报价撮合所有待成交模拟盘工单,返回成交笔数。
pub fn fill_pending_paper(
    conn: &mut Connection,
    quotes: &HashMap<String, Quote>,
    now: NaiveDateTime,
) -> Result<usize> {
    let mut filled = 0;
    for t in ticket::list_open_paper(conn)? {
        let Some(q) = quotes.get(&t.code) else {
            continue;
        };
        let slippage = store::get_risk_rules(conn, t.user_id)?.slippage;
        if let RouteOutcome::Filled { .. } = fill_paper_ticket(conn, &t, q, slippage, now)? {
            filled += 1;
        }
    }
    Ok(filled)
}
```

- [ ] **Step 4: 实现 service**

在 `src/trade/service.rs` 模块注释之后追加:

```rust
use crate::trade::gate::{self, Admission, GateDecision, GateInput, GateReject};
use crate::trade::model::{Account, NewSignal, Quote, TicketStatus};
use crate::trade::router;
use crate::trade::store;
use crate::trade::ticket::{self, NewTicket};
use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::Connection;

pub struct SubmitContext<'a> {
    pub quote: Option<&'a Quote>,
    pub admission: Admission,
    pub now: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    /// dedup_key 已存在
    Duplicate,
    Rejected {
        signal_id: i64,
        reason: GateReject,
    },
    Ticketed {
        signal_id: i64,
        real_ticket: Option<i64>,
        paper_ticket: Option<i64>,
    },
}

pub fn submit_signal(
    conn: &mut Connection,
    sig: &NewSignal,
    ctx: &SubmitContext,
) -> Result<SubmitOutcome> {
    let now = ctx.now;
    let Some(signal_id) = ticket::insert_signal(conn, sig, now)? else {
        return Ok(SubmitOutcome::Duplicate);
    };

    let rules = store::get_risk_rules(conn, sig.user_id)?;
    let real_account = store::get_account(conn, sig.user_id, Account::Real)?;
    let mut paper_account = store::get_account(conn, sig.user_id, Account::Paper)?;
    if paper_account.is_none() {
        if let Some(real) = &real_account {
            store::set_capital(conn, sig.user_id, Account::Paper, real.total_capital, now)?;
            paper_account = store::get_account(conn, sig.user_id, Account::Paper)?;
        }
    }
    let real_position = store::get_position(conn, sig.user_id, Account::Real, &sig.code)?;
    let paper_position = store::get_position(conn, sig.user_id, Account::Paper, &sig.code)?;
    let has_open_ticket = ticket::has_open_ticket(conn, sig.user_id, &sig.code, sig.side)?;
    let last_signal_at =
        ticket::last_signal_at(conn, sig.user_id, &sig.code, sig.side, signal_id)?;
    let tickets_today = ticket::count_real_tickets_on(conn, sig.user_id, now.date())?;
    let realized_pnl_today =
        ticket::realized_pnl_on(conn, sig.user_id, Account::Real, now.date())?;

    let decision = gate::evaluate(&GateInput {
        signal: sig,
        quote: ctx.quote,
        admission: ctx.admission,
        rules: &rules,
        real_account: real_account.as_ref(),
        paper_account: paper_account.as_ref(),
        real_position: real_position.as_ref(),
        paper_position: paper_position.as_ref(),
        has_open_ticket,
        last_signal_at,
        tickets_today,
        realized_pnl_today,
        now,
    });

    let plans = match decision {
        GateDecision::Reject(reason) => {
            ticket::mark_signal(conn, signal_id, "rejected", Some(reason.as_str()))?;
            return Ok(SubmitOutcome::Rejected { signal_id, reason });
        }
        GateDecision::Pass(plans) => plans,
    };

    let expires_at = ticket::default_expiry(sig.source, now);
    let mut real_ticket = None;
    let mut paper_ticket = None;
    for plan in &plans {
        let status = match plan.account {
            Account::Real => TicketStatus::Pending,
            Account::Paper => TicketStatus::Confirmed,
        };
        let id = ticket::create_ticket(
            conn,
            &NewTicket {
                user_id: sig.user_id,
                signal_id,
                account: plan.account,
                code: sig.code.clone(),
                side: sig.side,
                suggest_price: plan.price,
                qty: plan.qty,
                expires_at,
                deviation_th: rules.deviation_th,
                status,
                created_at: now,
            },
        )?;
        match plan.account {
            Account::Real => real_ticket = Some(id),
            Account::Paper => paper_ticket = Some(id),
        }
    }
    ticket::mark_signal(conn, signal_id, "ticketed", None)?;

    if let (Some(id), Some(q)) = (paper_ticket, ctx.quote) {
        if let Some(t) = ticket::get_ticket(conn, id)? {
            router::fill_paper_ticket(conn, &t, q, rules.slippage, now)?;
        }
    }

    Ok(SubmitOutcome::Ticketed {
        signal_id,
        real_ticket,
        paper_ticket,
    })
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::router::tests`
Expected: 2 PASS

Run: `cargo test --test trade_core`
Expected: 2 PASS

若 `manual_buy_confirm_fill_then_exit_sell_next_day` 中数值断言失败,先按以下预期手算核对,确属测试笔误则修正测试并在报告写明算式;否则修实现:
- 闸门:预算 = min(50000, 50000, 100000, 20000) = 20000;执行价 10.01 → 1998 → 1900 股
- 实盘回填 1900 @ 10.00:金额 19000,费用 max(4.75, 5) + 0.19 = 5.19

- [ ] **Step 6: 全量门禁**

Run: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`
Expected: fmt 干净;clippy 仅 3 个既有问题;测试除既有 `realtime_pipeline::full_day_flow_from_detection_to_summary` 外全部通过

- [ ] **Step 7: Commit**

```bash
git add src/trade tests/trade_core.rs
git commit -m "feat(trade): 模拟盘自动成交与信号提交全流程"
```

---

## 完成标准

- [ ] 基金测试期望值未改动;股票费用 / 价格取整导致的期望变更已在报告中逐项列出
- [ ] `trade` 全部单元测试与 `tests/trade_core.rs` 通过
- [ ] CI 三件套符合 Global Constraints

## 后续计划(不在本计划范围)

- **计划 2b 运行时与信号**:`trade_quotes` 报价缓存与 trade-monitor 线程(15 秒快照、`fill_pending_paper`、`expire_due`、止盈止损与移动止盈信号、心跳、失败退避与告警)、策略 / 异动 / 手动信号接入、现有持仓导入、推送文案、9:25 / 15:05 / 次日 9:00 调度、挂单占用资金(未成交买入工单预留现金)、`fill_pending_paper` 单工单错误隔离与滑点缓存、`expire_due` 覆盖模拟盘、`NewTicket.urgency`、停牌/过期报价视为无报价、模拟盘成交价基准决策
- 计划 3 策略准入(为 `Admission` 提供真实状态)
- 计划 4 Web `/trade` 页面、签名链接、持仓校准表、风控设置页;另需补:按用户隔离的工单读取(`get_ticket_for_user`)、SQL 端状态过滤、部分成交重复最低佣金、输入校验(NaN / 0)
- 计划 1 遗留且仍未吸收:T+0 ETF、`execution_for_market` 显式市场匹配、主板 ST 涨跌幅核实、`kline.rs` hfq 回退、`UnsupportedQty` 与跌停滑点封顶测试、`Broker` 买入 `Shares` 非正数保护测试 → 计划 2b 首个任务评估

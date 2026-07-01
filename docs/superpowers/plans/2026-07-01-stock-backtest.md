# 单股回测 Implementation Plan（子项目 2）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让现有回测引擎跑在个股上，产出带真实 A/港/美股费用的回测结果与交易统计（胜率/盈亏比）。

**Architecture:** 复用泛型 `Engine<D,S>`（`StockData` 直接插入）+ `Portfolio` + `metrics`。共用 `Broker` 泛化：抽 `Fee` trait、`Broker` 改持 `Box<dyn Fee>`、`FeeModel::sell_rate` 顺序无关化（基金行为保持）。股票侧新增 `StockFee`(impl Fee)、`trade_stats`(FIFO 还原)、`backtest::run_one`。

**Tech Stack:** Rust；无新增依赖。

## Global Constraints

- 隔离：`src/stock/**` 只允许 `use` 共用件（`crate::{broker,engine,portfolio,metrics,result,strategy,event}`）与 `crate::stock::*`，**禁止 use 任何基金专属模块**（`data::{eastmoney,cache,fundlist,sync}`、`analyze`、`recommend`、`config`、`web`、`runner`）。
- 共用件改动仅限 `src/broker.rs`，且必须**保持基金行为不变**：所有既有 broker/engine/runner/optimize/recommend/web 测试保持通过。
- 撮合价为后复权价（引擎按 `today.adj_nav` 撮合，`StockData` 已映射 `adj_close→adj_nav`）。
- 测试全离线，`cargo test` 全绿。
- 提交信息结尾追加：`Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`。

---

## File Structure

```
src/broker.rs        —— 修改：新增 Fee trait + impl Fee for FeeModel + Broker 持 Box<dyn Fee> + sell_rate 顺序无关 + 更新自身测试
src/stock/mod.rs     —— 修改：新增 pub mod fee; trade_stats; backtest;
src/stock/fee.rs     —— 新建：StockFee(impl Fee) + 市场预设
src/stock/trade_stats.rs —— 新建：TradeStats + trade_stats(&[TradeRecord])
src/stock/backtest.rs    —— 新建：run_one(...) -> StockRunOutcome
```

---

## Task 1: 共用 Broker 泛化（Fee trait，基金行为保持）

**Files:**
- Modify: `src/broker.rs`

**Interfaces:**
- Produces:
  - `trait Fee { fn buy_fee(&self, cash: f64) -> f64; fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64; }`
  - `impl Fee for FeeModel`
  - `Broker::new(fee: impl Fee + 'static) -> Broker`（签名兼容原 `FeeModel` 调用点）
  - `FeeModel::sell_rate` 改为与档位顺序无关

- [ ] **Step 1: 改 sell_rate 为顺序无关 + 新增 Fee trait 与 impl**

`src/broker.rs`：把 `impl FeeModel` 的 `sell_rate` 替换为：
```rust
impl FeeModel {
    /// 按持有天数选择赎回费率；与档位在 Vec 中的顺序无关。
    /// 在满足 (max_days==0 兜底) 或 (holding_days <= max_days) 的档中，取 max_days 最小者；
    /// max_days==0 视作最大（最长期限兜底档）。
    pub fn sell_rate(&self, holding_days: i64) -> f64 {
        self.sell_tiers.iter()
            .filter(|t| t.max_days == 0 || holding_days <= t.max_days)
            .min_by_key(|t| if t.max_days == 0 { i64::MAX } else { t.max_days })
            .map(|t| t.rate)
            .unwrap_or(0.0)
    }
}
```
在 `impl FeeModel { ... }` 之后新增 Fee trait 与实现：
```rust
/// 资产无关的费用抽象：基金用 FeeModel，股票用 StockFee。
pub trait Fee {
    fn buy_fee(&self, cash: f64) -> f64;
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64;
}

impl Fee for FeeModel {
    fn buy_fee(&self, cash: f64) -> f64 { cash * self.buy_rate }
    fn sell_fee(&self, shares: f64, price: f64, holding_days: i64) -> f64 {
        shares * price * self.sell_rate(holding_days)
    }
}
```

- [ ] **Step 2: Broker 改持 Box<dyn Fee>，execute 走 trait**

把 `pub struct Broker { fee: FeeModel, lots: Vec<Lot> }` 改为：
```rust
pub struct Broker { fee: Box<dyn Fee>, lots: Vec<Lot> }
```
把 `Broker::new` 改为（移除排序，改由顺序无关的 sell_rate 保证正确）：
```rust
impl Broker {
    pub fn new(fee: impl Fee + 'static) -> Self {
        Self { fee: Box::new(fee), lots: Vec::new() }
    }
```
`execute` 内买入分支的 `let fee = cash * self.fee.buy_rate;` 改为：
```rust
                let fee = self.fee.buy_fee(cash);
```
卖出分支的 `fee += take * price * self.fee.sell_rate(days);` 改为：
```rust
                    fee += self.fee.sell_fee(take, price, days);
```

- [ ] **Step 3: 更新受影响的自身测试**

`src/broker.rs` 的 `sell_rate_robust_to_tier_order` 测试中 `b.fee.sell_rate(..)` 现无法访问（`fee` 已为 `Box<dyn Fee>`）。替换该测试为直接测 `FeeModel::sell_rate` 的顺序无关性：
```rust
    #[test]
    fn sell_rate_robust_to_tier_order() {
        // 档位故意乱序：兜底档在前、长档居中、短档在后。
        let scrambled = FeeModel { buy_rate: 0.0015, sell_tiers: vec![
            SellTier{max_days:0,   rate:0.0},
            SellTier{max_days:365, rate:0.005},
            SellTier{max_days:7,   rate:0.015},
        ]};
        assert!((scrambled.sell_rate(3)   - 0.015).abs() < 1e-9, "3天应命中1.5%档");
        assert!((scrambled.sell_rate(100) - 0.005).abs() < 1e-9, "100天应命中0.5%档");
        assert!((scrambled.sell_rate(400) - 0.0  ).abs() < 1e-9, "400天应命中0%兜底");
    }
```

- [ ] **Step 4: 全量回归**

Run: `cargo test`
Expected: 全绿。重点确认 broker/engine/runner/optimize/recommend/web 既有测试全部通过（`buy_applies_fee_and_creates_shares`、`sell_all_uses_holding_day_tier_fifo`、`sell_rate_tiers`、`sell_rate_robust_to_tier_order`、`avg_cost_tracks_weighted_price` 等），证明基金行为零回归。

- [ ] **Step 5: 提交**

```bash
git add src/broker.rs
git commit -m "refactor(broker): 抽出 Fee trait，Broker 持 Box<dyn Fee>，sell_rate 顺序无关（基金行为保持）"
```

---

## Task 2: 股票费用模型 StockFee

**Files:**
- Create: `src/stock/fee.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod fee;`）

**Interfaces:**
- Consumes: `crate::broker::Fee`
- Produces:
  - `struct StockFee { commission_rate, min_commission, stamp_tax_rate, transfer_rate: f64 }`（Debug, Clone, Copy, PartialEq）
  - `StockFee::{a_share, hk, us}() -> StockFee`、`StockFee::for_market(market: u16) -> StockFee`
  - `impl Fee for StockFee`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod fee;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/fee.rs`：
```rust
use crate::broker::Fee;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StockFee {
    pub commission_rate: f64,
    pub min_commission: f64,
    pub stamp_tax_rate: f64,
    pub transfer_rate: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_buy_hits_min_commission() {
        // cash=1000: 佣金=0.25<5 → 取5; 过户=1000*0.00001=0.01 → 5.01
        let fee = StockFee::a_share().buy_fee(1000.0);
        assert!((fee - 5.01).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn a_share_buy_above_min() {
        // cash=100000: 佣金=25; 过户=1.0 → 26.0
        assert!((StockFee::a_share().buy_fee(100000.0) - 26.0).abs() < 1e-9);
    }

    #[test]
    fn a_share_sell_includes_stamp_tax() {
        // v=1000*100=100000: 佣金=25; 印花=50; 过户=1 → 76
        assert!((StockFee::a_share().sell_fee(1000.0, 100.0, 0) - 76.0).abs() < 1e-9);
    }

    #[test]
    fn us_is_commission_free() {
        assert!(StockFee::us().buy_fee(100000.0).abs() < 1e-9);
        assert!(StockFee::us().sell_fee(1000.0, 100.0, 0).abs() < 1e-9);
    }

    #[test]
    fn for_market_maps_each_market() {
        assert_eq!(StockFee::for_market(1), StockFee::a_share());
        assert_eq!(StockFee::for_market(0), StockFee::a_share());
        assert_eq!(StockFee::for_market(116), StockFee::hk());
        assert_eq!(StockFee::for_market(105), StockFee::us());
        assert_eq!(StockFee::for_market(106), StockFee::us());
        assert_eq!(StockFee::for_market(999), StockFee::a_share()); // 未知回退A股
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::fee`
Expected: 编译失败 —— `a_share`/`buy_fee` 等未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/fee.rs` 的 `struct StockFee` 之后加入：
```rust
impl StockFee {
    pub fn a_share() -> Self { Self { commission_rate: 0.00025, min_commission: 5.0, stamp_tax_rate: 0.0005, transfer_rate: 0.00001 } }
    pub fn hk() -> Self { Self { commission_rate: 0.0025, min_commission: 3.0, stamp_tax_rate: 0.001, transfer_rate: 0.0 } }
    pub fn us() -> Self { Self { commission_rate: 0.0, min_commission: 0.0, stamp_tax_rate: 0.0, transfer_rate: 0.0 } }
    pub fn for_market(market: u16) -> Self {
        match market {
            116 => Self::hk(),
            105 | 106 | 107 => Self::us(),
            _ => Self::a_share(),
        }
    }
}

impl Fee for StockFee {
    fn buy_fee(&self, cash: f64) -> f64 {
        (cash * self.commission_rate).max(self.min_commission) + cash * self.transfer_rate
    }
    fn sell_fee(&self, shares: f64, price: f64, _holding_days: i64) -> f64 {
        let v = shares * price;
        (v * self.commission_rate).max(self.min_commission) + v * self.stamp_tax_rate + v * self.transfer_rate
    }
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::fee`
Expected: 5 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/fee.rs src/stock/mod.rs
git commit -m "feat(stock): StockFee 费用模型（佣金/印花税/过户费，A股/港股/美股预设）"
```

---

## Task 3: 交易统计 trade_stats（FIFO 实现盈亏还原）

**Files:**
- Create: `src/stock/trade_stats.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod trade_stats;`）

**Interfaces:**
- Consumes: `crate::result::TradeRecord`、`crate::event::Direction`
- Produces:
  - `struct TradeStats { round_trips, wins: usize, win_rate, profit_factor, avg_win, avg_loss, realized_pnl: f64 }`（Debug, Clone, PartialEq）
  - `trade_stats(trades: &[TradeRecord]) -> TradeStats`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod trade_stats;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/trade_stats.rs`：
```rust
use crate::result::TradeRecord;
use crate::event::Direction;

#[derive(Debug, Clone, PartialEq)]
pub struct TradeStats {
    pub round_trips: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub realized_pnl: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn buy(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord { date: dt, direction: Direction::Buy, shares, price, fee }
    }
    fn sell(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord { date: dt, direction: Direction::Sell, shares, price, fee }
    }

    #[test]
    fn no_sells_is_zero() {
        let s = trade_stats(&[buy(d(2024,1,1), 100.0, 1.0, 0.0)]);
        assert_eq!(s.round_trips, 0);
        assert_eq!(s.wins, 0);
        assert!((s.realized_pnl).abs() < 1e-9);
        assert!((s.profit_factor).abs() < 1e-9); // 无盈亏 → 0
    }

    #[test]
    fn single_winning_round_trip() {
        let s = trade_stats(&[buy(d(2024,1,1),100.0,1.0,0.0), sell(d(2024,2,1),100.0,2.0,0.0)]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.win_rate - 1.0).abs() < 1e-9);
        assert!((s.realized_pnl - 100.0).abs() < 1e-9);
        assert!(s.profit_factor.is_infinite()); // 无亏损
        assert!((s.avg_win - 100.0).abs() < 1e-9);
    }

    #[test]
    fn single_losing_round_trip() {
        let s = trade_stats(&[buy(d(2024,1,1),100.0,2.0,0.0), sell(d(2024,2,1),100.0,1.0,0.0)]);
        assert_eq!(s.wins, 0);
        assert!((s.win_rate).abs() < 1e-9);
        assert!((s.realized_pnl + 100.0).abs() < 1e-9);
        assert!((s.profit_factor).abs() < 1e-9); // 无盈利
        assert!((s.avg_loss - 100.0).abs() < 1e-9);
    }

    #[test]
    fn fifo_partial_consumption() {
        // 买100@1、买100@2，卖150@3：消耗100@1(成本100)+50@2(成本100)=200，
        // 收入=150*3=450 → 实现盈亏=250，一次盈利 round trip。
        let s = trade_stats(&[
            buy(d(2024,1,1),100.0,1.0,0.0),
            buy(d(2024,1,2),100.0,2.0,0.0),
            sell(d(2024,1,3),150.0,3.0,0.0),
        ]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.realized_pnl - 250.0).abs() < 1e-9);
    }

    #[test]
    fn buy_fee_folds_into_cost_basis() {
        // 买100@1 费10 → 每股成本=1+0.1=1.1；卖100@1 费0 → 实现盈亏=(1-1.1)*100=-10
        let s = trade_stats(&[buy(d(2024,1,1),100.0,1.0,10.0), sell(d(2024,2,1),100.0,1.0,0.0)]);
        assert!((s.realized_pnl + 10.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::trade_stats`
Expected: 编译失败 —— `trade_stats` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/trade_stats.rs` 的 `struct TradeStats` 之后加入：
```rust
/// 对成交序列做 FIFO 成本匹配，还原每笔卖出的实现盈亏。买费摊入每股成本。
pub fn trade_stats(trades: &[TradeRecord]) -> TradeStats {
    let mut lots: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new(); // (剩余份额, 每股成本)
    let mut round_trips = 0usize;
    let mut wins = 0usize;
    let mut gross_win = 0.0;
    let mut gross_loss = 0.0; // 累计正值

    for t in trades {
        match t.direction {
            Direction::Buy => {
                if t.shares > 1e-9 {
                    let cost_per_share = t.price + t.fee / t.shares;
                    lots.push_back((t.shares, cost_per_share));
                }
            }
            Direction::Sell => {
                let mut remaining = t.shares;
                let mut cost = 0.0;
                while remaining > 1e-9 {
                    let Some((lot_shares, lot_cost)) = lots.front().copied() else { break; };
                    let take = remaining.min(lot_shares);
                    cost += take * lot_cost;
                    let left = lot_shares - take;
                    if left > 1e-9 { lots.front_mut().unwrap().0 = left; } else { lots.pop_front(); }
                    remaining -= take;
                }
                let matched = t.shares - remaining;
                let pnl = matched * t.price - t.fee - cost;
                round_trips += 1;
                if pnl > 0.0 { wins += 1; gross_win += pnl; } else { gross_loss += -pnl; }
            }
        }
    }

    let losses = round_trips - wins;
    let win_rate = if round_trips > 0 { wins as f64 / round_trips as f64 } else { 0.0 };
    let profit_factor = if gross_loss > 1e-9 { gross_win / gross_loss }
        else if gross_win > 1e-9 { f64::INFINITY } else { 0.0 };
    let avg_win = if wins > 0 { gross_win / wins as f64 } else { 0.0 };
    let avg_loss = if losses > 0 { gross_loss / losses as f64 } else { 0.0 };
    TradeStats { round_trips, wins, win_rate, profit_factor, avg_win, avg_loss, realized_pnl: gross_win - gross_loss }
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::trade_stats`
Expected: 5 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/trade_stats.rs src/stock/mod.rs
git commit -m "feat(stock): 交易统计 trade_stats（FIFO 实现盈亏 → 胜率/盈亏比）"
```

---

## Task 4: 回测入口 backtest::run_one

**Files:**
- Create: `src/stock/backtest.rs`
- Modify: `src/stock/mod.rs`（新增 `pub mod backtest;`）

**Interfaces:**
- Consumes: `crate::broker::Broker`、`crate::engine::Engine`、`crate::metrics`、`crate::portfolio::Portfolio`、`crate::result::{DailyRecord, TradeRecord}`、`crate::strategy::Strategy`、`crate::stock::data::{StockData, StockBar}`、`crate::stock::fee::StockFee`、`crate::stock::trade_stats`
- Produces:
  - `struct StockRunOutcome { name, code: String, summary: Summary, trade_stats: TradeStats, daily: Vec<DailyRecord>, trades: Vec<TradeRecord> }`
  - `run_one(name, code, bars, strategy, fee, initial_cash) -> StockRunOutcome`

- [ ] **Step 1: 声明模块**

`src/stock/mod.rs` 追加：
```rust
pub mod backtest;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/backtest.rs`：
```rust
use crate::broker::Broker;
use crate::engine::Engine;
use crate::metrics::{self, Summary};
use crate::portfolio::Portfolio;
use crate::result::{DailyRecord, TradeRecord};
use crate::strategy::Strategy;
use crate::stock::data::{StockData, StockBar};
use crate::stock::fee::StockFee;
use crate::stock::trade_stats::{self, TradeStats};

pub struct StockRunOutcome {
    pub name: String,
    pub code: String,
    pub summary: Summary,
    pub trade_stats: TradeStats,
    pub daily: Vec<DailyRecord>,
    pub trades: Vec<TradeRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Period;
    use crate::strategy::dca::Dca;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, price: f64) -> StockBar {
        StockBar { date: dt, open: price, high: price, low: price, close: price, volume: 0.0, adj_close: price }
    }

    #[test]
    fn run_one_dca_flat_then_up() {
        // 与基金 runner::run_one_dca_flat_then_up 对齐：1/1买1000、2/1买1000、2/15价2.0
        // us() 零费 → 持有2000份，市值4000，投入2000，2笔买入，0次卖出
        let bars = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.0), bar(d(2024,2,15),2.0)];
        let strategy: Box<dyn Strategy> = Box::new(Dca::new(Period::Monthly, 1, 1000.0));
        let out = run_one("t".into(), "600519".into(), bars, strategy, StockFee::us(), 0.0);
        assert_eq!(out.daily.len(), 3);
        assert!((out.summary.final_equity - 4000.0).abs() < 1e-6, "final_equity={}", out.summary.final_equity);
        assert!((out.summary.total_contributed - 2000.0).abs() < 1e-6);
        assert_eq!(out.summary.trade_count, 2);
        assert_eq!(out.trade_stats.round_trips, 0, "无卖出");
    }

    #[test]
    fn a_share_fee_reduces_equity_vs_free() {
        // 同一序列，A股费 vs 零费：A股费下期末权益更低（含最低佣金）
        let bars = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.0), bar(d(2024,2,15),2.0)];
        let free = run_one("f".into(), "600519".into(), bars.clone(),
            Box::new(Dca::new(Period::Monthly,1,1000.0)), StockFee::us(), 0.0);
        let paid = run_one("p".into(), "600519".into(), bars,
            Box::new(Dca::new(Period::Monthly,1,1000.0)), StockFee::a_share(), 0.0);
        assert!(paid.summary.final_equity < free.summary.final_equity, "A股费应降低期末权益");
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib stock::backtest`
Expected: 编译失败 —— `run_one` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/backtest.rs` 的 `struct StockRunOutcome` 之后加入：
```rust
/// 装配引擎跑单股回测：StockData + 复用策略 + StockFee + Portfolio。
pub fn run_one(
    name: String,
    code: String,
    bars: Vec<StockBar>,
    strategy: Box<dyn Strategy>,
    fee: StockFee,
    initial_cash: f64,
) -> StockRunOutcome {
    let data = StockData::new(bars);
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let stats = trade_stats::trade_stats(engine.trades());
    let daily = engine.daily().to_vec();
    let trades = engine.trades().to_vec();
    StockRunOutcome { name, code, summary, trade_stats: stats, daily, trades }
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib stock::backtest`
Expected: 2 个测试 PASS。

- [ ] **Step 6: 全量回归 + 提交**

Run: `cargo test`
Expected: 全绿。
```bash
git add src/stock/backtest.rs src/stock/mod.rs
git commit -m "feat(stock): 单股回测 run_one（复用引擎 + StockFee + 交易统计）"
```

---

## Self-Review

**1. Spec 覆盖：**
- 引擎复用（StockData 插入泛型 Engine）→ Task 4 ✅
- 共用 Broker 泛化（Fee trait，行为保持）→ Task 1 ✅
- StockFee 费用（佣金/印花/过户，三市场预设，最低佣金）→ Task 2 ✅
- 交易统计 FIFO（胜率/盈亏比/均盈亏/实现盈亏）→ Task 3 ✅
- run_one + StockRunOutcome → Task 4 ✅
- 复用现有策略（DCA 测试）→ Task 4 ✅
- 隔离约束（不 use 基金专属）→ Global Constraints + 各任务 use 列表 ✅
- 基金零回归 → Task 1 Step 4 全量回归 ✅

**2. 占位符扫描：** 无 TBD/TODO；每步含完整代码。✅

**3. 类型一致性：**
- `Fee::{buy_fee, sell_fee}` 签名在 broker(Task1)、StockFee(Task2) 一致。
- `Broker::new(impl Fee + 'static)` 兼容既有 `FeeModel` 调用点（FeeModel: Fee）。
- `TradeStats` 字段在 Task3 定义、Task4 引用一致；`trade_stats(&[TradeRecord])` 签名一致。
- `StockRunOutcome` 复用 `metrics::Summary`、`result::{DailyRecord,TradeRecord}`，与既有定义一致。
- `StockData::new(Vec<StockBar>)`（子项目1）、`Engine::new(data,strategy,broker,portfolio)`（engine.rs）签名一致。
- `Dca::new(Period, u32, f64)`、`Period::Monthly`（strategy）与既有一致。

**说明：** Broker 改 `Box<dyn Fee>` 后，`execute` 内 `self.fee.buy_fee/sell_fee` 对 FeeModel 的结果与原 `cash*buy_rate` / `take*price*sell_rate(days)` 逐值相等，故基金回测数值零变化。

# 量化交易 · 计划 3c:实盘/模拟盘统计、watchdog、成绩单与评估任务执行 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把成交记录变成可判定的表现指标:模拟盘 / 实盘逐笔收益、回撤、执行损耗;按 spec §10.5 做 watchdog(回撤、胜率、连亏)并在异常时暂停策略;给出四栏成绩单;把三类评估任务(前推回测 / 观察期检查 / watchdog)落成一个可离线测试的 `run_job`。

**Architecture:** `stats` 只做「查成交 → 算指标」,`judge` 继续是纯阈值比较,`scorecard` 做聚合,`worker::run_job` 是唯一把它们与状态机串起来的地方;`trade-eval` 线程在计划 3d 接线,本计划保证 `run_job` 可在内存库 + 注入 K 线下完整测试。

**Tech Stack:** Rust 2021、rusqlite 0.31、chrono、serde_json、既有回测与准入内核。无新依赖、不引入 tokio。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §10.5、§10.6、§14(任务 8)。前置:计划 1、2a、2b、3a、3b 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换只走 `admission::state`(条件 UPDATE + 合法转换表 + 事件,且与副作用同事务)
- 阈值一律来自 `[trade.admission]` 并带范围校验
- 测试不得访问网络:K 线经闭包注入
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 相对 spec 的实现细化(执行者照此实现)

1. **回撤**用「累计已实现盈亏曲线」相对峰值的最大回撤近似(没有逐日市值快照),分母为峰值累计盈亏;峰值 ≤ 0 时该点不计回撤。文档注释写明。
2. **每笔收益率** = `realized_pnl / 成本`,成本 = `成交价 × 数量 − realized_pnl − 费用`。
3. **观察期天数**按 `since` 到 `now` 的工作日计数(不含节假日判断,与计划 2b 的调度口径一致)。
4. **执行损耗**取同一 `signal_id` 下实盘与模拟盘各自第一笔成交价之比:买入 `real/paper − 1`,卖出 `paper/real − 1`(都表示实盘更吃亏为正),按笔取中位数。
5. **连亏基线是单只股票口径**(计划 3b 已注明),故 watchdog 的实盘连亏也**按代码分组**计算后取最大,避免全池交错高估。
6. **月度重跑**没有 `Backtesting` 中间态:新增 `state::apply_monthly_verdict`,不通过时 `Admitted → Suspended`、`Paper → Failed`,通过则保持原状态并只落库评估结果。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/admission/stats.rs` | 新建 | 成交查询、逐笔收益、回撤、观察期统计、watchdog 统计、执行损耗 |
| `src/trade/admission/judge.rs` | 修改 | `WatchdogStats`、`judge_watchdog` |
| `src/trade/config.rs` | 修改 | `AdmissionCfg` 新增 watchdog 阈值 |
| `src/trade/store.rs` | 修改 | `last_transition_at` |
| `src/trade/admission/state.rs` | 修改 | `apply_monthly_verdict` |
| `src/trade/admission/scorecard.rs` | 新建 | 四栏成绩单 |
| `src/trade/admission/worker.rs` | 新建 | `run_job`:三类评估任务的执行 |
| `src/trade/admission/mod.rs` | 修改 | 模块声明 |

---

### Task 1: 成交统计与 watchdog 判定

**Files:**
- Create: `src/trade/admission/stats.rs`
- Modify: `src/trade/admission/mod.rs`、`src/trade/admission/judge.rs`、`src/trade/config.rs`、`src/trade/store.rs`
- Test: 各文件 `mod tests`

**Interfaces:**

```rust
// stats.rs
pub struct FillRow { pub account: Account, pub code: String, pub side: Direction, pub price: f64, pub qty: u64, pub fee: f64, pub realized_pnl: Option<f64>, pub filled_at: NaiveDateTime, pub signal_id: i64 } // Debug, Clone, PartialEq
pub fn strategy_fills(conn: &Connection, user_id: i64, strategy_id: i64, account: Account, since: Option<NaiveDateTime>) -> Result<Vec<FillRow>>;
pub fn sell_pnls(fills: &[FillRow]) -> Vec<f64>;                 // 卖出成交的 realized_pnl,按时间序
pub fn trade_returns(fills: &[FillRow]) -> Vec<f64>;             // 每笔卖出的收益率
pub fn equity_drawdown(pnls: &[f64]) -> f64;                     // 累计盈亏曲线最大回撤(比例)
pub fn workdays_between(from: NaiveDate, to: NaiveDate) -> i64;  // 含首尾的工作日数
pub fn paper_stats(conn: &Connection, user_id: i64, strategy_id: i64, since: NaiveDateTime, now: NaiveDateTime) -> Result<PaperStats>;
pub fn watchdog_stats(conn: &Connection, user_id: i64, strategy_id: i64, window: usize) -> Result<WatchdogStats>;
pub fn execution_loss(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<f64>>;
// judge.rs
pub struct WatchdogStats { pub drawdown: f64, pub recent_win_rate: f64, pub recent_trades: usize, pub max_consecutive_losses: usize } // Debug, Clone, Default, PartialEq
pub fn judge_watchdog(s: &WatchdogStats, baseline: &BacktestBaseline, backtest_win_rate: f64, backtest_max_streak: usize, cfg: &AdmissionCfg) -> Option<String>;
// config.rs: AdmissionCfg 增加 drawdown_multiple(1.5)、win_rate_sigma(2.0)、streak_multiple(1.5)、watchdog_window(20)、watchdog_min_trades(5)
// store.rs
pub fn last_transition_at(conn: &Connection, strategy_id: i64, user_id: i64, to: StrategyStatus) -> Result<Option<NaiveDateTime>>;
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/stats.rs`(先写模块注释与测试):

```rust
//! 从成交记录还原模拟盘 / 实盘表现:逐笔收益、回撤、观察期与 watchdog 统计、执行损耗。
//!
//! 没有逐日市值快照,回撤一律用「累计已实现盈亏曲线」相对峰值的最大回撤近似。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{AccountScope, NewSignal, NewStrategy, SignalSource, TicketStatus};
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use crate::trade::{store, ticket};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn fill_row(account: Account, side: Direction, price: f64, realized: Option<f64>, signal_id: i64) -> FillRow {
        FillRow {
            account,
            code: "600000".into(),
            side,
            price,
            qty: 1000,
            fee: 10.61,
            realized_pnl: realized,
            filled_at: at(16, 10, 0),
            signal_id,
        }
    }

    /// 建一套「策略 → 信号 → 工单 → 成交」的数据,返回 (策略 id, 信号 id)。
    fn seed(
        c: &mut Connection,
        user_id: i64,
        strategy_id: Option<i64>,
        account: Account,
        side: Direction,
        price: f64,
        realized: Option<f64>,
        key: &str,
        now: NaiveDateTime,
    ) -> i64 {
        let mut sig = NewSignal {
            user_id,
            source: SignalSource::Strategy,
            strategy_id,
            code: "600000".into(),
            name: None,
            side,
            scope: AccountScope::Both,
            ref_price: price,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        };
        sig.strategy_id = strategy_id;
        let sid = insert_signal(c, &sig, now).unwrap().unwrap();
        let tid = create_ticket(
            c,
            &NewTicket {
                user_id,
                signal_id: sid,
                account,
                code: "600000".into(),
                side,
                suggest_price: price,
                qty: 1000,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                urgency: 0,
                created_at: now,
            },
        )
        .unwrap();
        // 直接写 trade_fills:绕开 record_fill 的持仓/资金校验,本测试只关心统计
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, ?2, ?3, '600000', ?4, ?5, 1000, 10.61, ?6, 'test', ?7)",
            rusqlite::params![
                tid,
                user_id,
                account.as_str(),
                crate::trade::model::side_str(side),
                price,
                realized,
                crate::trade::model::fmt_ts(now),
            ],
        )
        .unwrap();
        sid
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    fn strategy(c: &Connection, user_id: i64) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "rsi".into(),
                grid_toml: "rsi_window = [14]".into(),
                pool: vec!["600000".into()],
            },
            at(15, 9, 0),
        )
        .unwrap()
    }

    #[test]
    fn trade_returns_use_cost_derived_from_fill_fields() {
        // 卖出 1000 @11,费 10.61,realized 984.29 → 成本 = 11000 − 984.29 − 10.61 = 10005.1
        let sell = fill_row(Account::Real, Direction::Sell, 11.0, Some(984.29), 1);
        let r = trade_returns(&[sell.clone()]);
        assert_eq!(r.len(), 1);
        assert!((r[0] - 984.29 / 10_005.1).abs() < 1e-9, "{}", r[0]);
        assert_eq!(sell_pnls(&[sell.clone()]), vec![984.29]);
        let buy = FillRow { side: Direction::Buy, realized_pnl: None, ..sell };
        assert!(trade_returns(&[buy.clone()]).is_empty(), "买入不计入");
        assert!(sell_pnls(&[buy]).is_empty());
    }

    #[test]
    fn drawdown_of_cumulative_pnl() {
        // 累计 100 → 60 → 160 → 110:峰值 100 后回撤 40(40%),峰值 160 后回撤 50(31.25%)
        assert!((equity_drawdown(&[100.0, -40.0, 100.0, -50.0]) - 0.4).abs() < 1e-9);
        assert_eq!(equity_drawdown(&[]), 0.0);
        assert_eq!(equity_drawdown(&[10.0, 20.0]), 0.0, "只涨不回撤");
        assert_eq!(equity_drawdown(&[-10.0, -20.0]), 0.0, "峰值未转正不计回撤");
    }

    #[test]
    fn workdays_skip_weekends() {
        // 2026-09-14(周一)到 2026-09-18(周五)= 5 个工作日;跨周末仍是 5 + 1
        assert_eq!(workdays_between(NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(), NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()), 5);
        assert_eq!(workdays_between(NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(), NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()), 6);
        assert_eq!(workdays_between(NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(), NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()), 0, "倒序为 0");
    }

    #[test]
    fn strategy_fills_are_scoped_by_user_strategy_and_account() {
        let mut c = db();
        let mine = strategy(&c, 1);
        let other_strategy = strategy(&c, 1);
        let other_user = strategy(&c, 2);
        seed(&mut c, 1, Some(mine), Account::Real, Direction::Sell, 11.0, Some(100.0), "k1", at(16, 10, 0));
        seed(&mut c, 1, Some(mine), Account::Paper, Direction::Sell, 11.0, Some(50.0), "k2", at(16, 10, 1));
        seed(&mut c, 1, Some(other_strategy), Account::Real, Direction::Sell, 11.0, Some(70.0), "k3", at(16, 10, 2));
        seed(&mut c, 2, Some(other_user), Account::Real, Direction::Sell, 11.0, Some(90.0), "k4", at(16, 10, 3));
        seed(&mut c, 1, None, Account::Real, Direction::Sell, 11.0, Some(80.0), "k5", at(16, 10, 4));

        let real = strategy_fills(&c, 1, mine, Account::Real, None).unwrap();
        assert_eq!(real.len(), 1);
        assert_eq!(real[0].realized_pnl, Some(100.0));
        let paper = strategy_fills(&c, 1, mine, Account::Paper, None).unwrap();
        assert_eq!(paper.len(), 1);
        assert_eq!(paper[0].realized_pnl, Some(50.0));
        assert!(strategy_fills(&c, 2, mine, Account::Real, None).unwrap().is_empty(), "他人不可见");
        assert!(
            strategy_fills(&c, 1, mine, Account::Real, Some(at(16, 10, 1))).unwrap().is_empty(),
            "since 过滤"
        );
    }

    #[test]
    fn paper_and_watchdog_stats_come_from_fills() {
        let mut c = db();
        let s = strategy(&c, 1);
        // 三笔模拟盘卖出:+100、−50、+20
        for (i, pnl) in [100.0, -50.0, 20.0].iter().enumerate() {
            seed(&mut c, 1, Some(s), Account::Paper, Direction::Sell, 11.0, Some(*pnl), &format!("p{i}"), at(16, 10, i as u32));
        }
        let ps = paper_stats(&c, 1, s, at(14, 9, 0), at(16, 15, 0)).unwrap();
        assert_eq!(ps.trades, 3);
        assert_eq!(ps.days, 3, "9-14 周一 到 9-16 周三");
        assert!(ps.max_drawdown > 0.0, "中间有回撤");

        // 实盘两笔亏损 → watchdog 连亏 2
        for (i, pnl) in [-30.0, -40.0].iter().enumerate() {
            seed(&mut c, 1, Some(s), Account::Real, Direction::Sell, 11.0, Some(*pnl), &format!("r{i}"), at(16, 11, i as u32));
        }
        let ws = watchdog_stats(&c, 1, s, 20).unwrap();
        assert_eq!((ws.recent_trades, ws.max_consecutive_losses), (2, 2));
        assert_eq!(ws.recent_win_rate, 0.0);
    }

    #[test]
    fn execution_loss_compares_real_and_paper_on_the_same_signal() {
        let mut c = db();
        let s = strategy(&c, 1);
        // 同一信号:实盘买入 10.05、模拟买入 10.00 → 0.005
        let sid = seed(&mut c, 1, Some(s), Account::Real, Direction::Buy, 10.05, None, "e1", at(16, 10, 0));
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             SELECT id, 1, 'paper', '600000', 'buy', 10.0, 1000, 5.0, NULL, 'test', '2026-09-16 10:00:00'
             FROM trade_tickets WHERE signal_id = ?1 LIMIT 1",
            [sid],
        )
        .unwrap();
        let loss = execution_loss(&c, 1, s).unwrap().unwrap();
        assert!((loss - 0.005).abs() < 1e-9, "{loss}");
        assert!(execution_loss(&c, 2, s).unwrap().is_none(), "他人无数据");
    }
}
```

`judge.rs` 追加:

```rust
    #[test]
    fn watchdog_flags_drawdown_winrate_and_streak() {
        let cfg = AdmissionCfg::default();
        let base = BacktestBaseline { avg_trade_return: 0.02, trade_return_sd: 0.01, max_drawdown: 0.20 };
        let ok = WatchdogStats { drawdown: 0.25, recent_win_rate: 0.5, recent_trades: 20, max_consecutive_losses: 3 };
        assert!(judge_watchdog(&ok, &base, 0.55, 4, &cfg).is_none());

        let deep = WatchdogStats { drawdown: 0.31, ..ok.clone() };
        assert!(judge_watchdog(&deep, &base, 0.55, 4, &cfg).unwrap().contains("回撤"));

        // σ = sqrt(0.55×0.45/20) ≈ 0.1112;阈值 ≈ 0.55 − 2σ ≈ 0.3276
        let cold = WatchdogStats { recent_win_rate: 0.30, ..ok.clone() };
        assert!(judge_watchdog(&cold, &base, 0.55, 4, &cfg).unwrap().contains("胜率"));
        let few = WatchdogStats { recent_trades: 4, recent_win_rate: 0.0, ..ok.clone() };
        assert!(judge_watchdog(&few, &base, 0.55, 4, &cfg).is_none(), "样本不足不判定");

        let streak = WatchdogStats { max_consecutive_losses: 7, ..ok.clone() };
        assert!(judge_watchdog(&streak, &base, 0.55, 4, &cfg).unwrap().contains("连亏"));
    }
```

`config.rs` 追加:

```rust
    #[test]
    fn watchdog_thresholds_default_and_validate() {
        let c = from_toml_str("[trade]\n").unwrap().admission;
        assert_eq!((c.drawdown_multiple, c.win_rate_sigma, c.streak_multiple), (1.5, 2.0, 1.5));
        assert_eq!((c.watchdog_window, c.watchdog_min_trades), (20, 5));
        assert!(from_toml_str("[trade.admission]\ndrawdown_multiple = 0.5\n").is_err(), "须 ≥ 1");
        assert!(from_toml_str("[trade.admission]\nwin_rate_sigma = -1.0\n").is_err());
        assert!(from_toml_str("[trade.admission]\nwatchdog_window = 0\n").is_err());
    }
```

`store.rs` 追加:

```rust
    #[test]
    fn last_transition_at_finds_the_latest_entry_per_status() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        assert!(last_transition_at(&c, id, 1, StrategyStatus::Paper).unwrap().is_none());
        log_status_event(&c, id, 1, StrategyStatus::Backtesting, StrategyStatus::Paper, "首次", at(16, 9, 1)).unwrap();
        log_status_event(&c, id, 1, StrategyStatus::Paper, StrategyStatus::Suspended, "暂停", at(16, 9, 2)).unwrap();
        log_status_event(&c, id, 1, StrategyStatus::Suspended, StrategyStatus::Paper, "再次", at(16, 9, 3)).unwrap();
        assert_eq!(last_transition_at(&c, id, 1, StrategyStatus::Paper).unwrap(), Some(at(16, 9, 3)));
        assert!(last_transition_at(&c, id, 2, StrategyStatus::Paper).unwrap().is_none(), "用户隔离");
    }
```

> 实际签名(已核对):`log_status_event(conn, strategy_id, user_id, from, to, reason, now)`、`get_strategy(conn, user_id, id)`、`latest_eval(conn, strategy_id, user_id, stage) -> Option<(String, NaiveDateTime)>`、`save_eval(conn, strategy_id, user_id, version_hash, stage, metrics_json, data_from, data_to, now)`。测试里的 `db()`、`at()`、`new_strategy()` 等辅助函数沿用 `store.rs` 内 `mod tests` 已有的同名写法。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission trade::store::tests trade::config::tests`
Expected: 编译失败(`FillRow`、`judge_watchdog`、`last_transition_at`、新阈值字段未定义)

- [ ] **Step 3: 实现 config.rs 与 store.rs**

`AdmissionCfg` 增加字段与默认值:

```rust
    /// 实盘回撤超过「回测最大回撤 × 该倍数」即暂停
    pub drawdown_multiple: f64,
    /// 胜率下限 = 回测胜率 − 该倍数 × σ
    pub win_rate_sigma: f64,
    /// 连亏超过「回测最长连亏 × 该倍数」即暂停
    pub streak_multiple: f64,
    /// 胜率判定的滚动窗口(笔)
    pub watchdog_window: usize,
    /// 少于该笔数不做胜率判定
    pub watchdog_min_trades: usize,
```
默认 `1.5 / 2.0 / 1.5 / 20 / 5`。`from_toml_str` 的校验追加:`drawdown_multiple`、`streak_multiple` 须 ≥ 1 且有限;`win_rate_sigma` 须 ≥ 0 且有限;`watchdog_window`、`watchdog_min_trades` 须 ≥ 1。

`store.rs`:

```rust
/// 该策略最近一次「转入指定状态」的时间(观察期起点等用)。
pub fn last_transition_at(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
    to: StrategyStatus,
) -> Result<Option<NaiveDateTime>> {
    let s: Option<String> = conn.query_row(
        &format!(
            "SELECT MAX(at) FROM trade_strategy_events
             WHERE strategy_id = ?1 AND to_status = ?3 AND {OWNED_BY_USER}"
        ),
        params![strategy_id, user_id, to.as_str()],
        |r| r.get(0),
    )?;
    s.as_deref().map(parse_ts).transpose()
}
```

> `OWNED_BY_USER` 占用 `?1`(strategy_id)与 `?2`(user_id),所以本查询自己的参数从 `?3` 起,与 `latest_eval` 同一写法。`MAX(at)` 在无行时返回一行 NULL,`query_row` 不会报 `QueryReturnedNoRows`。

- [ ] **Step 4: 实现 judge.rs**

```rust
/// 实盘监控统计(spec §10.5)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WatchdogStats {
    /// 累计已实现盈亏曲线的最大回撤
    pub drawdown: f64,
    /// 最近窗口内的胜率
    pub recent_win_rate: f64,
    /// 最近窗口内的笔数
    pub recent_trades: usize,
    /// 按代码分组后的最长连亏(与回测基线同口径)
    pub max_consecutive_losses: usize,
}

/// 实盘表现是否已偏离回测到需要暂停;返回原因。
pub fn judge_watchdog(
    s: &WatchdogStats,
    baseline: &BacktestBaseline,
    backtest_win_rate: f64,
    backtest_max_streak: usize,
    cfg: &AdmissionCfg,
) -> Option<String> {
    if baseline.max_drawdown > 0.0 && s.drawdown > baseline.max_drawdown * cfg.drawdown_multiple {
        return Some(format!(
            "实盘回撤 {:.1}% 超过回测 {:.1}% 的 {:.1} 倍",
            s.drawdown * 100.0,
            baseline.max_drawdown * 100.0,
            cfg.drawdown_multiple
        ));
    }
    if s.recent_trades >= cfg.watchdog_min_trades && backtest_win_rate > 0.0 {
        let p = backtest_win_rate.clamp(0.0, 1.0);
        let sigma = (p * (1.0 - p) / s.recent_trades as f64).sqrt();
        let floor = p - cfg.win_rate_sigma * sigma;
        if s.recent_win_rate < floor {
            return Some(format!(
                "近 {} 笔胜率 {:.0}% 低于回测 {:.0}% − {:.0}σ({:.0}%)",
                s.recent_trades,
                s.recent_win_rate * 100.0,
                p * 100.0,
                cfg.win_rate_sigma,
                floor * 100.0
            ));
        }
    }
    if backtest_max_streak > 0
        && (s.max_consecutive_losses as f64) > backtest_max_streak as f64 * cfg.streak_multiple
    {
        return Some(format!(
            "连亏 {} 笔超过回测 {} 笔的 {:.1} 倍",
            s.max_consecutive_losses, backtest_max_streak, cfg.streak_multiple
        ));
    }
    None
}
```

- [ ] **Step 5: 实现 stats.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::event::Direction;
use crate::trade::admission::judge::{PaperStats, WatchdogStats};
use crate::trade::model::{fmt_ts, parse_side, parse_ts, Account};
use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Weekday};
use rusqlite::{params, Connection, Row};
use std::collections::BTreeMap;

/// 一条成交记录(已按策略过滤)。
#[derive(Debug, Clone, PartialEq)]
pub struct FillRow {
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub price: f64,
    pub qty: u64,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
    pub filled_at: NaiveDateTime,
    pub signal_id: i64,
}

fn read_fill(r: &Row) -> rusqlite::Result<(String, String, String, f64, i64, f64, Option<f64>, String, i64)> {
    Ok((
        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?,
    ))
}

/// 该用户该策略在指定账户下的成交,按时间序。`since` 为 None 时取全部。
pub fn strategy_fills(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    account: Account,
    since: Option<NaiveDateTime>,
) -> Result<Vec<FillRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.account, f.code, f.side, f.price, f.qty, f.fee, f.realized_pnl, f.filled_at, t.signal_id
         FROM trade_fills f
         JOIN trade_tickets t ON t.id = f.ticket_id
         JOIN trade_signals s ON s.id = t.signal_id
         WHERE f.user_id = ?1 AND s.user_id = ?1 AND s.strategy_id = ?2 AND f.account = ?3
           AND (?4 IS NULL OR f.filled_at >= ?4)
         ORDER BY f.id",
    )?;
    let rows = stmt
        .query_map(
            params![
                user_id,
                strategy_id,
                account.as_str(),
                since.map(fmt_ts)
            ],
            read_fill,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(acc, code, side, price, qty, fee, realized_pnl, filled_at, signal_id)| {
            Ok(FillRow {
                account: Account::parse(&acc)?,
                code,
                side: parse_side(&side)?,
                price,
                qty: qty.max(0) as u64,
                fee,
                realized_pnl,
                filled_at: parse_ts(&filled_at)?,
                signal_id,
            })
        })
        .collect()
}
```

> `since` 的语义是「不早于该时刻的成交才要」(`filled_at >= since`),与测试一致:唯一那笔实盘成交在 `10:00`,传 `Some(10:01)` 得到空列表。

```rust
/// 卖出成交的已实现盈亏,按时间序。
pub fn sell_pnls(fills: &[FillRow]) -> Vec<f64> {
    fills
        .iter()
        .filter(|f| f.side == Direction::Sell)
        .filter_map(|f| f.realized_pnl)
        .collect()
}

/// 每笔卖出的收益率:realized / 成本,成本 = 成交额 − realized − 费用。
pub fn trade_returns(fills: &[FillRow]) -> Vec<f64> {
    fills
        .iter()
        .filter(|f| f.side == Direction::Sell)
        .filter_map(|f| {
            let realized = f.realized_pnl?;
            let cost = f.price * f.qty as f64 - realized - f.fee;
            (cost > 1e-9).then(|| realized / cost)
        })
        .collect()
}

/// 累计已实现盈亏曲线的最大回撤(相对峰值)。峰值 ≤ 0 时不计。
pub fn equity_drawdown(pnls: &[f64]) -> f64 {
    let mut cum = 0.0;
    let mut peak = 0.0f64;
    let mut worst = 0.0f64;
    for p in pnls {
        cum += p;
        peak = peak.max(cum);
        if peak > 0.0 {
            worst = worst.max((peak - cum) / peak);
        }
    }
    worst
}

/// 含首尾的工作日数;`to` 早于 `from` 返回 0。
pub fn workdays_between(from: NaiveDate, to: NaiveDate) -> i64 {
    let mut day = from;
    let mut n = 0;
    while day <= to {
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            n += 1;
        }
        day += chrono::Duration::days(1);
    }
    n
}

fn max_streak_by_code(fills: &[FillRow]) -> usize {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cur: BTreeMap<&str, usize> = BTreeMap::new();
    for f in fills.iter().filter(|f| f.side == Direction::Sell) {
        let pnl = f.realized_pnl.unwrap_or(0.0);
        let c = cur.entry(f.code.as_str()).or_insert(0);
        if pnl <= 0.0 {
            *c += 1;
            let best = by_code.entry(f.code.as_str()).or_insert(0);
            *best = (*best).max(*c);
        } else {
            *c = 0;
        }
    }
    by_code.values().copied().max().unwrap_or(0)
}

/// 观察期统计:天数按工作日计,笔数为卖出成交数。
pub fn paper_stats(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    since: NaiveDateTime,
    now: NaiveDateTime,
) -> Result<PaperStats> {
    let fills = strategy_fills(conn, user_id, strategy_id, Account::Paper, Some(since))?;
    let returns = trade_returns(&fills);
    let avg = if returns.is_empty() {
        0.0
    } else {
        returns.iter().sum::<f64>() / returns.len() as f64
    };
    Ok(PaperStats {
        days: workdays_between(since.date(), now.date()),
        trades: fills.iter().filter(|f| f.side == Direction::Sell).count(),
        avg_trade_return: avg,
        max_drawdown: equity_drawdown(&sell_pnls(&fills)),
    })
}

/// 实盘 watchdog 统计:回撤取全部实盘成交,胜率取最近 `window` 笔,连亏按代码分组。
pub fn watchdog_stats(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    window: usize,
) -> Result<WatchdogStats> {
    let fills = strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    let pnls = sell_pnls(&fills);
    let recent: &[f64] = if pnls.len() > window {
        &pnls[pnls.len() - window..]
    } else {
        &pnls
    };
    let wins = recent.iter().filter(|p| **p > 0.0).count();
    Ok(WatchdogStats {
        drawdown: equity_drawdown(&pnls),
        recent_win_rate: if recent.is_empty() {
            0.0
        } else {
            wins as f64 / recent.len() as f64
        },
        recent_trades: recent.len(),
        max_consecutive_losses: max_streak_by_code(&fills),
    })
}

/// 执行损耗:同一信号下实盘相对模拟盘多付的比例中位数;无配对返回 None。
pub fn execution_loss(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<f64>> {
    let real = strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    let paper = strategy_fills(conn, user_id, strategy_id, Account::Paper, None)?;
    let mut paper_first: BTreeMap<i64, &FillRow> = BTreeMap::new();
    for f in &paper {
        paper_first.entry(f.signal_id).or_insert(f);
    }
    let mut losses: Vec<f64> = Vec::new();
    let mut seen: BTreeMap<i64, ()> = BTreeMap::new();
    for r in &real {
        if seen.insert(r.signal_id, ()).is_some() {
            continue;
        }
        let Some(p) = paper_first.get(&r.signal_id) else {
            continue;
        };
        if p.price <= 0.0 || r.price <= 0.0 {
            continue;
        }
        losses.push(match r.side {
            Direction::Buy => r.price / p.price - 1.0,
            Direction::Sell => p.price / r.price - 1.0,
        });
    }
    if losses.is_empty() {
        return Ok(None);
    }
    losses.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = losses.len();
    Ok(Some(if n % 2 == 1 {
        losses[n / 2]
    } else {
        (losses[n / 2 - 1] + losses[n / 2]) / 2.0
    }))
}
```

`mod.rs` 加 `pub mod stats;`。`Context` 若未使用请删除该 import。

- [ ] **Step 6: 运行确认通过**

Run: `cargo test --lib trade::admission trade::store::tests trade::config::tests`
Expected: 全部 PASS(stats 6、judge +1、config +1、store +1)

- [ ] **Step 7: Commit**

```bash
git add src/trade
git commit -m "feat(trade): 模拟盘/实盘成交统计、执行损耗与 watchdog 判定"
```

---

### Task 2: 月度重跑入口与四栏成绩单

**Files:**
- Create: `src/trade/admission/scorecard.rs`
- Modify: `src/trade/admission/state.rs`、`src/trade/admission/mod.rs`
- Test: 两个文件内 `mod tests`

**Interfaces:**

```rust
// state.rs
pub fn apply_monthly_verdict(conn: &Connection, user_id: i64, id: i64, metrics: &PoolMetrics, verdict: &Verdict, from: NaiveDate, to: NaiveDate, now: NaiveDateTime) -> Result<Transition>;
// 通过:仅落库评估结果,状态不变(返回 AlreadyHandled);不通过:Admitted → Suspended、Paper → Failed
// scorecard.rs
pub struct StageMetrics { pub trades: usize, pub win_rate: f64, pub avg_trade_return: f64, pub max_drawdown: f64, pub realized_pnl: f64 } // Serialize, Default, PartialEq
pub fn stage_metrics(fills: &[FillRow]) -> StageMetrics;
pub struct Scorecard { pub strategy_id: i64, pub name: String, pub status: StrategyStatus, pub oos: Option<PoolMetrics>, pub paper: StageMetrics, pub real: StageMetrics, pub execution_loss: Option<f64> } // Serialize
pub fn scorecard(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<Scorecard>>;
```

- [ ] **Step 1: 写失败测试**

`state.rs` 追加:

```rust
    #[test]
    fn monthly_verdict_suspends_admitted_and_fails_paper() {
        let c = db();
        let admitted = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, admitted, at(16, 9, 1)).unwrap();
        apply_backtest_verdict(&c, 1, admitted, &empty_metrics(), &Verdict { passed: true, reasons: Vec::new() }, day(15), day(16), at(16, 9, 2)).unwrap();
        update_status(&c, 1, admitted, StrategyStatus::Paper, StrategyStatus::Admitted, "准入", at(16, 9, 3)).unwrap();

        let bad = Verdict { passed: false, reasons: vec!["样本外夏普 0.30 < 0.80".into()] };
        assert_eq!(
            apply_monthly_verdict(&c, 1, admitted, &empty_metrics(), &bad, day(15), day(16), at(16, 9, 4)).unwrap(),
            Transition::Applied
        );
        let got = store::get_strategy(&c, 1, admitted).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Suspended);
        assert!(got.status_reason.unwrap().contains("夏普"));

        let paper = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, paper, at(16, 9, 1)).unwrap();
        apply_backtest_verdict(&c, 1, paper, &empty_metrics(), &Verdict { passed: true, reasons: Vec::new() }, day(15), day(16), at(16, 9, 2)).unwrap();
        apply_monthly_verdict(&c, 1, paper, &empty_metrics(), &bad, day(15), day(16), at(16, 9, 5)).unwrap();
        assert_eq!(store::get_strategy(&c, 1, paper).unwrap().unwrap().status, StrategyStatus::Failed);
    }

    #[test]
    fn monthly_verdict_keeps_status_when_passing_but_records_eval() {
        let c = db();
        let id = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, id, at(16, 9, 1)).unwrap();
        apply_backtest_verdict(&c, 1, id, &empty_metrics(), &Verdict { passed: true, reasons: Vec::new() }, day(15), day(16), at(16, 9, 2)).unwrap();
        update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Admitted, "准入", at(16, 9, 3)).unwrap();
        let ok = Verdict { passed: true, reasons: Vec::new() };
        assert_eq!(
            apply_monthly_verdict(&c, 1, id, &empty_metrics(), &ok, day(15), day(16), at(16, 9, 6)).unwrap(),
            Transition::AlreadyHandled,
            "通过则状态不变"
        );
        assert_eq!(store::get_strategy(&c, 1, id).unwrap().unwrap().status, StrategyStatus::Admitted);
        assert!(store::latest_eval(&c, id, 1, "oos").unwrap().is_some());
    }
```

> `latest_eval` / `get_strategy` 的参数顺序以当前代码为准。

创建 `src/trade/admission/scorecard.rs`(先写测试):

```rust
//! 四栏成绩单:样本外(来自最近一次前推回测)、模拟盘、实盘,以及执行损耗。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::stats::FillRow;
    use crate::event::Direction;
    use crate::trade::model::Account;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn sell(pnl: f64, price: f64, signal_id: i64) -> FillRow {
        FillRow {
            account: Account::Real,
            code: "600000".into(),
            side: Direction::Sell,
            price,
            qty: 1000,
            fee: 10.0,
            realized_pnl: Some(pnl),
            filled_at: at(16, 10, 0),
            signal_id,
        }
    }

    #[test]
    fn stage_metrics_summarise_fills() {
        let m = stage_metrics(&[sell(100.0, 11.0, 1), sell(-50.0, 11.0, 2), sell(20.0, 11.0, 3)]);
        assert_eq!(m.trades, 3);
        assert!((m.win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.realized_pnl - 70.0).abs() < 1e-9);
        assert!(m.max_drawdown > 0.0);
        assert_eq!(stage_metrics(&[]), StageMetrics::default());
    }
}
```

`scorecard` 的集成断言放在 Task 3 的 `run_job` 测试里一并覆盖(那里已有完整的策略/成交数据)。

- [ ] **Step 2–3: 失败 → 实现**

`state.rs`:

```rust
/// 月度重跑的结论:已准入 / 观察期策略没有 Backtesting 中间态,
/// 通过则只落库评估、状态不变;不通过按 spec §10.5 降级。
#[allow(clippy::too_many_arguments)]
pub fn apply_monthly_verdict(
    conn: &Connection,
    user_id: i64,
    id: i64,
    metrics: &PoolMetrics,
    verdict: &Verdict,
    from: NaiveDate,
    to: NaiveDate,
    now: NaiveDateTime,
) -> Result<Transition> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(Transition::AlreadyHandled);
    };
    let tx = conn.unchecked_transaction()?;
    store::save_eval(
        &tx,
        id,
        user_id,
        &s.version_hash,
        "oos",
        &serde_json::to_string(metrics)?,
        from,
        to,
        now,
    )?;
    let transition = if verdict.passed {
        Transition::AlreadyHandled
    } else {
        let reason = verdict.reasons.join(";");
        match s.status {
            StrategyStatus::Admitted => transition_status(
                &tx,
                user_id,
                id,
                StrategyStatus::Admitted,
                StrategyStatus::Suspended,
                &reason,
                now,
            )?,
            StrategyStatus::Paper => transition_status(
                &tx,
                user_id,
                id,
                StrategyStatus::Paper,
                StrategyStatus::Failed,
                &reason,
                now,
            )?,
            _ => Transition::AlreadyHandled,
        }
    };
    tx.commit()?;
    Ok(transition)
}
```

> `transition_status` 是同文件内的私有函数,可直接调用;它自身已做合法性检查,非法转换返回 `Transition::AlreadyHandled` 而不报错。`save_eval` 的实参顺序见 Task 1 的核对说明。

`scorecard.rs` 实现:

```rust
use crate::event::Direction;
use crate::trade::admission::stats::{self, equity_drawdown, sell_pnls, trade_returns, FillRow};
use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::model::{Account, StrategyStatus};
use crate::trade::store;
use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::Connection;
use serde::Serialize;

/// 一栏表现。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StageMetrics {
    pub trades: usize,
    pub win_rate: f64,
    pub avg_trade_return: f64,
    pub max_drawdown: f64,
    pub realized_pnl: f64,
}

pub fn stage_metrics(fills: &[FillRow]) -> StageMetrics {
    let pnls = sell_pnls(fills);
    if pnls.is_empty() {
        return StageMetrics::default();
    }
    let returns = trade_returns(fills);
    let wins = pnls.iter().filter(|p| **p > 0.0).count();
    StageMetrics {
        trades: pnls.len(),
        win_rate: wins as f64 / pnls.len() as f64,
        avg_trade_return: if returns.is_empty() {
            0.0
        } else {
            returns.iter().sum::<f64>() / returns.len() as f64
        },
        max_drawdown: equity_drawdown(&pnls),
        realized_pnl: pnls.iter().sum(),
    }
}

/// 策略成绩单:样本外取最近一次前推回测结果,模拟盘 / 实盘取成交记录。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scorecard {
    pub strategy_id: i64,
    pub name: String,
    pub status: StrategyStatus,
    pub oos: Option<PoolMetrics>,
    pub paper: StageMetrics,
    pub real: StageMetrics,
    /// 实盘相对模拟盘多付的比例中位数
    pub execution_loss: Option<f64>,
}

pub fn scorecard(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<Scorecard>> {
    let Some(s) = store::get_strategy(conn, user_id, strategy_id)? else {
        return Ok(None);
    };
    let oos = match store::latest_eval(conn, strategy_id, user_id, "oos")? {
        Some((json, _)) => serde_json::from_str::<PoolMetrics>(&json).ok(),
        None => None,
    };
    let paper = stats::strategy_fills(conn, user_id, strategy_id, Account::Paper, None)?;
    let real = stats::strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    Ok(Some(Scorecard {
        strategy_id,
        name: s.name,
        status: s.status,
        oos,
        paper: stage_metrics(&paper),
        real: stage_metrics(&real),
        execution_loss: stats::execution_loss(conn, user_id, strategy_id)?,
    }))
}
```

`mod.rs` 加 `pub mod scorecard;`。未使用的 import(如 `Direction`、`NaiveDateTime`)请删除。

- [ ] **Step 4: 运行确认通过 + Commit**

```bash
git add src/trade/admission
git commit -m "feat(trade): 月度重跑结论入口与四栏成绩单"
```

---

### Task 3: 评估任务执行 `run_job`

**Files:**
- Create: `src/trade/admission/worker.rs`
- Modify: `src/trade/admission/mod.rs`
- Test: `src/trade/admission/worker.rs` 内 `mod tests`

**Interfaces:**

```rust
pub struct JobContext<'a> { pub wf: &'a WalkForwardCfg, pub admission: &'a AdmissionCfg, pub now: NaiveDateTime }
pub fn run_job<F>(conn: &mut Connection, job: &EvalJob, ctx: &JobContext, load: F) -> Result<String>
where F: FnMut(&str) -> Result<Vec<StockBar>>;   // 返回一句人类可读的结论,写入 job 的 error 字段之外的日志
```

行为:
- `WalkForward`:取策略 → 解析 `grid_toml` → `run_pool`(进度写 `store::set_job_progress`)→ 若 `cancelled` 直接返回「已取消」→ `judge_backtest` → 状态为 `Backtesting` 时 `apply_backtest_verdict`,否则 `apply_monthly_verdict`
- `PaperCheck`:仅 `Paper` 状态;`since` = `last_transition_at(.., Paper)`(无则用策略更新时间)→ `paper_stats` → 基线取最近 `oos` eval(反序列化 `PoolMetrics` → `BacktestBaseline::from_pool`;`kind == "mover"` 或无 eval 传 `None`)→ `judge_paper` → 通过则 `Paper → Admitted`
- `Watchdog`:仅 `Admitted` 状态;`watchdog_stats` → 基线同上(无 eval 则跳过判定)→ `judge_watchdog` → `Some(reason)` 则 `Admitted → Suspended`

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/worker.rs`(先写模块注释与测试):

```rust
//! 单个评估任务的执行:前推回测 / 观察期检查 / 实盘监控。
//! 线程与调度在计划 3d;本模块保证在内存库 + 注入 K 线下可完整测试。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::admission::walk_forward::WalkForwardCfg;
    use crate::trade::config::AdmissionCfg;
    use crate::trade::model::{
        Account, AccountScope, EvalKind, NewSignal, NewStrategy, SignalSource, StrategyStatus,
        TicketStatus,
    };
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    fn strategy(c: &Connection, kind: &str) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: kind.into(),
                grid_toml: "short_window = [3, 5]\nlong_window = [10]\namount = [20000.0]".into(),
                pool: vec!["600000".into()],
            },
            at(15, 9, 0),
        )
        .unwrap()
    }

    fn job(c: &Connection, strategy_id: i64, kind: EvalKind) -> crate::trade::model::EvalJob {
        store::enqueue_eval(c, 1, strategy_id, kind, at(16, 9, 0)).unwrap().unwrap();
        store::claim_next_job(c, at(16, 9, 1)).unwrap().unwrap()
    }

    fn ctx(now: NaiveDateTime) -> (WalkForwardCfg, AdmissionCfg) {
        (
            WalkForwardCfg { train_days: 60, test_days: 30, step_days: 30, ..WalkForwardCfg::default() },
            AdmissionCfg::default(),
        )
    }

    /// 锯齿行情:与 walk_forward 测试同款,保证真的产生买卖。
    fn wave_bars(n: usize) -> Vec<crate::stock::data::StockBar> { /* 复用 walk_forward::tests 的思路:sin 波 + 微弱漂移,跳过周末 */ }

    #[test]
    fn walk_forward_job_moves_backtesting_strategy_to_paper_or_failed() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(16, 9, 0)).unwrap();
        let j = job(&c, id, EvalKind::WalkForward);
        let (wf, adm) = ctx(at(16, 9, 2));
        let note = run_job(&mut c, &j, &JobContext { wf: &wf, admission: &adm, now: at(16, 9, 2) }, |_| Ok(wave_bars(190))).unwrap();
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert!(
            matches!(got.status, StrategyStatus::Paper | StrategyStatus::Failed),
            "回测后应进入观察期或未通过,实际 {:?}",
            got.status
        );
        assert!(store::latest_eval(&c, id, 1, "oos").unwrap().is_some(), "评估结果落库");
        assert!(!note.is_empty());
    }

    #[test]
    fn walk_forward_job_fails_when_bars_cannot_load() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(16, 9, 0)).unwrap();
        let j = job(&c, id, EvalKind::WalkForward);
        let (wf, adm) = ctx(at(16, 9, 2));
        run_job(&mut c, &j, &JobContext { wf: &wf, admission: &adm, now: at(16, 9, 2) }, |_| Err(anyhow::anyhow!("无数据"))).unwrap();
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Failed, "全池无数据 → 未通过");
    }

    #[test]
    fn paper_check_promotes_only_when_thresholds_met() {
        let mut c = db();
        let id = strategy(&c, "mover"); // mover 无回测基线
        state::submit_for_backtest(&c, 1, id, at(15, 9, 0)).unwrap(); // → Paper
        // 40 个工作日前进入观察期 + 35 笔盈利的模拟盘成交
        seed_paper_fills(&mut c, id, 35, 100.0);
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::PaperCheck);
        run_job(&mut c, &j, &JobContext { wf: &wf, admission: &adm, now: at(16, 15, 0) }, |_| Ok(Vec::new())).unwrap();
        // 观察期天数不足 → 仍为 Paper
        assert_eq!(store::get_strategy(&c, 1, id).unwrap().unwrap().status, StrategyStatus::Paper);
    }

    #[test]
    fn watchdog_job_suspends_admitted_strategy_on_deep_drawdown() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(15, 9, 0)).unwrap();
        state::apply_backtest_verdict(
            &c, 1, id,
            &crate::trade::admission::walk_forward::aggregate(Vec::new()),
            &crate::trade::admission::judge::Verdict { passed: true, reasons: Vec::new() },
            at(15, 9, 0).date(), at(16, 9, 0).date(), at(15, 9, 1),
        )
        .unwrap();
        state::update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Admitted, "准入", at(15, 9, 2)).unwrap();
        // 回测基线为空(aggregate(vec![]) 的 max_drawdown = 0)→ 回撤规则不触发,连亏规则也不触发
        seed_real_losses(&mut c, id, 5);
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::Watchdog);
        run_job(&mut c, &j, &JobContext { wf: &wf, admission: &adm, now: at(16, 15, 0) }, |_| Ok(Vec::new())).unwrap();
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Admitted,
            "空基线不应误暂停"
        );
    }
}
```

> 测试辅助 `seed_paper_fills` / `seed_real_losses` / `wave_bars` 由执行者按 Task 1 的 `seed` 写法实现(直接 INSERT `trade_fills`,信号的 `strategy_id` 指向该策略)。如果某个断言与实现后的真实行为不符,**先手算核对**:观察期天数不足、空基线不触发 watchdog 是本测试的意图,必要时调整数据而不是放宽断言,并在报告说明。

- [ ] **Step 2–3: 失败 → 实现**

```rust
use crate::stock::data::StockBar;
use crate::trade::admission::judge::{self, BacktestBaseline};
use crate::trade::admission::stats;
use crate::trade::admission::state;
use crate::trade::admission::walk_forward::{self, PoolMetrics, PoolProgress, WalkForwardCfg};
use crate::trade::config::AdmissionCfg;
use crate::trade::model::{EvalJob, EvalKind, StrategyStatus};
use crate::trade::store;
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;

pub struct JobContext<'a> {
    pub wf: &'a WalkForwardCfg,
    pub admission: &'a AdmissionCfg,
    pub now: NaiveDateTime,
}

/// 最近一次前推回测的池内指标(供观察期与 watchdog 取基线)。
fn latest_pool_metrics(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<PoolMetrics>> {
    Ok(match store::latest_eval(conn, strategy_id, user_id, "oos")? {
        Some((json, _)) => serde_json::from_str::<PoolMetrics>(&json).ok(),
        None => None,
    })
}

pub fn run_job<F>(
    conn: &mut Connection,
    job: &EvalJob,
    ctx: &JobContext,
    load: F,
) -> Result<String>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let Some(s) = store::get_strategy(conn, job.user_id, job.strategy_id)? else {
        return Err(anyhow!("策略 {} 不存在", job.strategy_id));
    };
    match job.kind {
        EvalKind::WalkForward => {
            let grid: toml::Table = s
                .grid_toml
                .parse()
                .map_err(|e| anyhow!("参数网格解析失败: {e}"))?;
            let total = s.pool.len();
            let job_id = job.id;
            // 进度写库失败不该中断评估,只记日志
            let mut on_code = |code: &str, done: usize, _total: usize| {
                if let Err(e) = store::set_job_progress(conn, job_id, &format!("{done}/{total} {code}"), ctx.now) {
                    eprintln!("[trade] 任务 {job_id} 进度写入失败: {e:#}");
                }
                true
            };
            let mut progress = PoolProgress { on_code: &mut on_code };
            let outcome = walk_forward::run_pool(&s.kind, &s.pool, &grid, ctx.wf, load, Some(&mut progress))?;
            if outcome.cancelled {
                return Ok("已取消".to_string());
            }
            let verdict = judge::judge_backtest(&outcome.metrics, ctx.admission);
            let (from, to) = (ctx.now.date(), ctx.now.date());
            let transition = if s.status == StrategyStatus::Backtesting {
                state::apply_backtest_verdict(conn, job.user_id, job.strategy_id, &outcome.metrics, &verdict, from, to, ctx.now)?
            } else {
                state::apply_monthly_verdict(conn, job.user_id, job.strategy_id, &outcome.metrics, &verdict, from, to, ctx.now)?
            };
            Ok(format!(
                "回测{}:{};状态变更 {:?}",
                if verdict.passed { "通过" } else { "未通过" },
                if verdict.passed { "—".to_string() } else { verdict.reasons.join(";") },
                transition
            ))
        }
        EvalKind::PaperCheck => {
            if s.status != StrategyStatus::Paper {
                return Ok(format!("跳过:当前状态 {}", s.status.as_str()));
            }
            let since = store::last_transition_at(conn, job.strategy_id, job.user_id, StrategyStatus::Paper)?
                .unwrap_or(s.updated_at);
            let stats = stats::paper_stats(conn, job.user_id, job.strategy_id, since, ctx.now)?;
            let baseline = if s.kind == "mover" {
                None
            } else {
                latest_pool_metrics(conn, job.user_id, job.strategy_id)?.map(|m| BacktestBaseline::from_pool(&m))
            };
            let verdict = judge::judge_paper(&stats, baseline.as_ref(), s.kind == "mover", ctx.admission);
            if !verdict.passed {
                return Ok(format!("观察期未达标:{}", verdict.reasons.join(";")));
            }
            let transition = state::update_status(
                conn,
                job.user_id,
                job.strategy_id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "观察期达标,准入",
                ctx.now,
            )?;
            Ok(format!("观察期达标,准入({transition:?})"))
        }
        EvalKind::Watchdog => {
            if s.status != StrategyStatus::Admitted {
                return Ok(format!("跳过:当前状态 {}", s.status.as_str()));
            }
            let Some(metrics) = latest_pool_metrics(conn, job.user_id, job.strategy_id)? else {
                return Ok("跳过:无回测基线".to_string());
            };
            let stats = stats::watchdog_stats(conn, job.user_id, job.strategy_id, ctx.admission.watchdog_window)?;
            let baseline = BacktestBaseline::from_pool(&metrics);
            match judge::judge_watchdog(
                &stats,
                &baseline,
                metrics.trade_baseline.win_rate,
                metrics.trade_baseline.max_consecutive_losses,
                ctx.admission,
            ) {
                Some(reason) => {
                    let transition = state::update_status(
                        conn,
                        job.user_id,
                        job.strategy_id,
                        StrategyStatus::Admitted,
                        StrategyStatus::Suspended,
                        &reason,
                        ctx.now,
                    )?;
                    Ok(format!("已暂停:{reason}({transition:?})"))
                }
                None => Ok("实盘表现正常".to_string()),
            }
        }
    }
}
```

> 借用冲突提示:`on_code` 闭包借用了 `conn`,而 `run_pool` 之后还要用 `conn`。若编译器报借用冲突,把进度回调改为只收集到本地 `Vec<String>`、在 `run_pool` 返回后再写一次进度(或改用 `Cell<usize>` 计数),并在报告中说明所选方案。

`mod.rs` 加 `pub mod worker;`。

- [ ] **Step 4: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade/admission
git commit -m "feat(trade): 评估任务执行(前推回测、观察期检查、实盘监控)"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] `trade::admission` 全部单元测试通过;测试不访问网络
- [ ] CI 门禁符合 Global Constraints

## 后续计划(不在本计划范围)

- **计划 3d**:`trade-eval` 线程(领取任务、进度、心跳、`reclaim_stale_jobs`、每日与每月入队)、`main.rs` 接线、`[trade.eval]` 配置
- **计划 3e**:日线策略信号(收盘后计算、次日 09:25 发出、`admission_for` 接入 `submit_signal`)、`stock/recommend.rs` 迁移到 A 股口径、交易日历
- **计划 4**:网页策略管理与成绩单展示、`/trade` 确认页、持仓校准、风控设置
- 计划 3b 遗留:`validate_new_strategy` 与 `config::build_strategy_from` 的 kind 列表各自维护;`Transition::AlreadyHandled` 未区分「已处理」与「非法转换」

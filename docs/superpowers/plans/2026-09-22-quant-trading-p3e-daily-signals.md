# 量化交易 · 计划 3e:日线策略信号、准入接线与交易日历 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让已进入观察期 / 已准入的日线策略真正产生交易信号(收盘后计算、次日 09:25 发出),把策略状态 → 闸门准入的映射收进 `submit_signal`,接入交易日历,并把股票推荐迁到 A 股成交口径。

**Architecture:** 收盘后在 `trade-eval` 线程里用「前推回测最后一个训练窗选出的参数」回放近 2 年行情、问策略「明天做什么」,结论写进新表 `trade_strategy_plans`(每策略每代码每基准日一行,幂等);次日开盘由 `trade-monitor` 线程读计划、拉报价、走 `submit_signal` 发出工单并推送。交易日历用新表 `trade_calendar` 记录「已证实开市 / 休市」的日子,由监听线程每天用一只 ETF 的快照自证,不硬编码节假日表。

**Tech Stack:** Rust 2021、rusqlite 0.31、chrono、toml、serde_json。无新依赖、不引入 tokio。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §5(strategy 行)、§6.3、§10.3、§11、§12。前置:计划 1、2a、2b、3a、3b、3c、3d 已合并入 main。另吸收计划 3c「执行后遗留项」中的 4 项(见文末对照表)。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换只走 `admission::state`
- 阈值与开关一律来自配置并带范围校验
- 测试不得访问网络;K 线经闭包注入,报价经 `QuoteSource` 桩注入
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 设计裁决(执行者照此实现)

1. **实盘参数 = 以最近一天为终点的训练窗上选出的参数**。前推回测每个训练窗都重新选参;实盘相当于「最新的那个训练窗」:以 K 线最后一天为终点、往前 `train_days` 天,用同一 `metric` 在同一网格上选一次,结果存进 `CodeMetrics.live_params`。每月重跑自动刷新,不另起调度。
2. **次日决策 = 回放 + 追问一次**。用实盘参数把近 `train_days` 天的 K 线完整回放一遍(A 股成交口径,与回测同一引擎),得到策略的内部状态与模拟持仓;再以「下一个交易日」为决策日、全部已收盘 K 线为历史,调一次 `on_market`。只保留回测里**会变成订单**的信号(`Portfolio::on_signal` 返回 `Some`),这样实盘与回测口径一致(spec §5:T-1 数据决策,T 开盘成交)。
3. **数量口径**:买入 `SignalAmount::Cash(c)` → `NewSignal.suggest_cash = Some(c)`(闸门再按风控上限与整手截断,与回测里现金单被整手截断同理);卖出 `SignalAmount::AllOut` → `suggest_qty = None`(全部可卖)。现有 5 个策略只产生这两种;其余口径返回错误并记日志,不猜。
4. **计划表是幂等的边界**。`(strategy_id, code, basis_date)` 唯一;无动作也写一行 `idle`。这样「今天算完没有」= 每个活跃(策略,代码)都已有今天的行,重启后不会重复算,也不需要额外的持久标记。
5. **K 线本身就是开市证明**。收盘后计算只接受「最后一根 K 线日期 = 今天」的数据;没更新就留到下一轮重试(默认每 10 分钟,截止 21:00)。
6. **交易日历只记证据,不猜**。`trade_calendar(day, is_open)` 只写被证实的日子:监听线程每个工作日 09:31 起用 `510300`(沪深 300 ETF,几乎不停牌)快照自证——时间戳是今天 → 开市;开盘后仍非今天 → 休市。没有记录的工作日按开市处理(与旧的「工作日」口径一致,不会更差)。
7. **发出窗口 09:25–10:30**。09:25 集合竞价已出结果,腾讯快照的时间戳已是当天;模拟盘工单即时按该价成交,正对应回测的「T 日开盘价」。已证实开市的日子过了 10:30 仍未发出的计划(停牌、无报价)标记 `dropped`;未证实或休市的日子保留到下一个交易日。
8. **准入在事务内判定**。`submit_signal` 自己调 `admission_for`,不再由调用方传入:状态读取与工单生成在同一个 `IMMEDIATE` 事务里,消除「刚读完 Admitted、下一瞬间被暂停」的竞态;调用方也不可能再传错。
9. **异动信号绑定异动策略**。用户若有状态为观察期 / 已准入、且股票池含该代码的 `mover` 策略,信号带上其 `strategy_id`(否则观察期检查永远数不到成交,异动策略永远无法准入);没有则保持旧行为(`strategy_id = None`,仅模拟盘)。
10. **股票推荐迁到 A 股口径**:A 股代码用 `AShareExecution`,每次买入金额提高到「至少能买一手」;非 A 股保持收盘成交。推荐的是排序,收益率本身与投入规模无关,提高金额不改变含义。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/calendar.rs` | 新建 | 交易日历:记录 / 查询 / 数交易日 / 下一个交易日 / 快照自证 |
| `src/trade/store.rs` | 修改 | `SCHEMA` 增加 `trade_calendar`、`trade_strategy_plans` 两张表;`active_mover_strategy` |
| `src/trade/admission/stats.rs` | 修改 | 观察期天数改按交易日历;删 `workdays_between`;连亏跳过无盈亏成交 |
| `src/trade/service.rs` | 修改 | `SubmitContext` 去掉 `admission`,事务内 `resolve_admission` |
| `src/trade/movers.rs` | 修改 | 绑定异动策略;候选代码 = 自选 ∪ 异动策略股票池 |
| `src/trade/monitor.rs` | 修改 | 适配新 `SubmitContext` |
| `src/trade/admission/walk_forward.rs` | 修改 | `select_params` 抽取;`live_params`、`data_from/data_to` |
| `src/trade/admission/worker.rs` | 修改 | 评估落库用真实数据跨度 |
| `src/trade/admission/state.rs` | 修改 | 月度裁决只对观察期 / 已准入落库 |
| `src/trade/admission/schedule.rs` | 修改 | 月度重跑跳过 `mover` |
| `src/engine.rs` | 修改 | `Engine::decide_next` |
| `src/stock/backtest.rs` | 修改 | `replay_and_decide` |
| `src/trade/strategy_signal.rs` | 新建 | 纯函数:实盘参数 + K 线 → 次日决策 |
| `src/trade/plans.rs` | 新建 | 信号计划表读写 |
| `src/trade/daily_signals.rs` | 新建 | 收盘后计算(含重试节奏)与开盘发出 |
| `src/trade/config.rs` | 修改 | `SignalCfg` + `TradeCfg.signals` + 校验 |
| `src/trade/admission/thread.rs` | 修改 | 评估线程接入收盘后计算 |
| `src/trade/daemon.rs` | 修改 | 监听线程接入日历自证与开盘发出 |
| `src/trade/mod.rs` | 修改 | 模块声明 |
| `src/stock/recommend.rs` | 修改 | A 股口径 |
| `config.toml` | 修改 | `[trade.signals]` 样例 |
| `tests/trade_core.rs`、`tests/trade_runtime.rs` | 修改 | 适配新 `SubmitContext`;新增端到端用例 |

---

### Task 1: 交易日历

**Files:**
- Create: `src/trade/calendar.rs`
- Modify: `src/trade/store.rs`(`SCHEMA`)、`src/trade/mod.rs`、`src/trade/admission/stats.rs:292-303, 326-345, 747-775`
- Test: `src/trade/calendar.rs` 内 `mod tests`;`src/trade/admission/stats.rs` 内既有测试

**Interfaces:**
- Consumes: `trade::quotes::QuoteSource`、`stock::realtime::calendar::{is_weekend, stale_means_holiday}`
- Produces:
  ```rust
  pub const PROBE_CODE: &str = "510300";
  pub fn mark_day(conn: &Connection, day: NaiveDate, open: bool, now: NaiveDateTime) -> Result<()>;
  pub fn day_status(conn: &Connection, day: NaiveDate) -> Result<Option<bool>>;
  pub fn is_trading_day(conn: &Connection, day: NaiveDate) -> Result<bool>;
  pub fn trading_days_between(conn: &Connection, from: NaiveDate, to: NaiveDate) -> Result<i64>;
  pub fn next_trading_day(conn: &Connection, day: NaiveDate) -> Result<NaiveDate>;
  pub fn probe(conn: &Connection, source: &dyn QuoteSource, now: NaiveDateTime) -> Result<Option<bool>>;
  ```

- [ ] **Step 1: 建表**

`src/trade/store.rs` 的 `SCHEMA` 末尾(`idx_trade_eval_jobs_pending` 之后)追加:

```sql
CREATE TABLE IF NOT EXISTS trade_calendar (
  day        TEXT PRIMARY KEY,
  is_open    INTEGER NOT NULL,
  checked_at TEXT NOT NULL
);
```

- [ ] **Step 2: 写失败测试**

新建 `src/trade/calendar.rs`,先只放测试与空壳(`todo!()` 函数体),`src/trade/mod.rs` 加 `pub mod calendar;`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::Quote;
    use crate::trade::store;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    fn at(m: u32, day: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(m, day).and_hms_opt(h, mi, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    struct Stub(Vec<Quote>);
    impl QuoteSource for Stub {
        fn fetch(&self, _codes: &[String]) -> Result<Vec<Quote>> {
            Ok(self.0.clone())
        }
    }
    fn probe_quote(ts: NaiveDateTime) -> Stub {
        Stub(vec![Quote {
            code: PROBE_CODE.into(),
            price: 4.0,
            limit_up: None,
            limit_down: None,
            ts,
        }])
    }

    #[test]
    fn unmarked_weekdays_count_as_open_weekends_never() {
        let c = db();
        // 2026-09-14 周一 … 2026-09-20 周日
        assert!(is_trading_day(&c, d(9, 14)).unwrap());
        assert!(!is_trading_day(&c, d(9, 19)).unwrap(), "周六");
        assert_eq!(trading_days_between(&c, d(9, 14), d(9, 18)).unwrap(), 5);
        assert_eq!(trading_days_between(&c, d(9, 14), d(9, 21)).unwrap(), 6);
        assert_eq!(trading_days_between(&c, d(9, 21), d(9, 14)).unwrap(), 0, "倒序为 0");
    }

    #[test]
    fn marked_holidays_are_skipped_and_can_be_corrected() {
        let c = db();
        // 国庆:10-01(周四)~10-08(周四)休市
        for day in 1..=8 {
            mark_day(&c, d(10, day), false, at(10, day, 9, 31)).unwrap();
        }
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), Some(false));
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), None);
        // 09-28(周一)~10-09(周五):工作日 10 个,扣掉 10-01、02、05、06、07、08 六个 → 4
        assert_eq!(trading_days_between(&c, d(9, 28), d(10, 9)).unwrap(), 4);
        assert_eq!(next_trading_day(&c, d(9, 30)).unwrap(), d(10, 9));
        assert_eq!(next_trading_day(&c, d(9, 18)).unwrap(), d(9, 21), "跨周末");
        // 误判可被后来的证据覆盖
        mark_day(&c, d(10, 8), true, at(10, 8, 10, 0)).unwrap();
        assert_eq!(day_status(&c, d(10, 8)).unwrap(), Some(true));
        assert_eq!(next_trading_day(&c, d(9, 30)).unwrap(), d(10, 8));
    }

    #[test]
    fn probe_marks_open_on_fresh_quote_and_closed_only_after_the_open() {
        let c = db();
        // 盘前陈旧:正常,不下结论
        assert_eq!(
            probe(&c, &probe_quote(at(9, 30, 15, 0)), at(10, 1, 9, 20)).unwrap(),
            None
        );
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), None);
        // 开盘后仍陈旧 → 休市
        assert_eq!(
            probe(&c, &probe_quote(at(9, 30, 15, 0)), at(10, 1, 9, 31)).unwrap(),
            Some(false)
        );
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), Some(false));
        // 当天时间戳 → 开市
        assert_eq!(
            probe(&c, &probe_quote(at(10, 9, 9, 31)), at(10, 9, 9, 31)).unwrap(),
            Some(true)
        );
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), Some(true));
        // 周末不探
        assert_eq!(
            probe(&c, &probe_quote(at(9, 18, 15, 0)), at(9, 19, 10, 0)).unwrap(),
            None
        );
    }

    #[test]
    fn probe_with_no_quote_is_an_error_not_a_holiday() {
        let c = db();
        assert!(probe(&c, &Stub(Vec::new()), at(10, 9, 9, 31)).is_err());
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), None);
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib trade::calendar`
Expected: FAIL(`todo!()` panic)

- [ ] **Step 4: 实现**

```rust
//! 交易日历。只记录被证实的日子,不硬编码节假日表(理由同
//! `stock::realtime::calendar` 顶部说明:年底忘了更新就会静默出错)。
//!
//! 证据来源:监听线程每个工作日开盘后用一只几乎不停牌的 ETF 做快照自证。
//! 没有记录的工作日按开市处理——与旧的「数工作日」口径相同,不会更差。

use crate::stock::realtime::calendar::{is_weekend, stale_means_holiday};
use crate::trade::model::{fmt_ts, DATE_FMT};
use crate::trade::quotes::QuoteSource;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};

/// 沪深 300 ETF:成交极活跃、几乎不停牌,拿它的快照时间戳判断今天开不开市。
pub const PROBE_CODE: &str = "510300";

pub fn mark_day(conn: &Connection, day: NaiveDate, open: bool, now: NaiveDateTime) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_calendar (day, is_open, checked_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(day) DO UPDATE SET is_open = excluded.is_open, checked_at = excluded.checked_at",
        params![day.format(DATE_FMT).to_string(), open as i64, fmt_ts(now)],
    )?;
    Ok(())
}

/// 已证实的开市(true)/ 休市(false);无记录为 None。
pub fn day_status(conn: &Connection, day: NaiveDate) -> Result<Option<bool>> {
    Ok(conn
        .query_row(
            "SELECT is_open FROM trade_calendar WHERE day = ?1",
            [day.format(DATE_FMT).to_string()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v != 0))
}

/// 周末一律休市;工作日除非被证实休市,否则按开市处理。
pub fn is_trading_day(conn: &Connection, day: NaiveDate) -> Result<bool> {
    Ok(!is_weekend(day) && day_status(conn, day)? != Some(false))
}

/// 含首尾的交易日数;`to` 早于 `from` 返回 0。
pub fn trading_days_between(conn: &Connection, from: NaiveDate, to: NaiveDate) -> Result<i64> {
    let mut day = from;
    let mut n = 0;
    while day <= to {
        if is_trading_day(conn, day)? {
            n += 1;
        }
        day += chrono::Duration::days(1);
    }
    Ok(n)
}

/// 严格晚于 `day` 的第一个交易日。
pub fn next_trading_day(conn: &Connection, day: NaiveDate) -> Result<NaiveDate> {
    let mut d = day + chrono::Duration::days(1);
    // A 股最长休市(春节 + 周末)不超过 2 周;给足余量仍找不到说明日历表被写坏了。
    for _ in 0..60 {
        if is_trading_day(conn, d)? {
            return Ok(d);
        }
        d += chrono::Duration::days(1);
    }
    Err(anyhow!("{day} 之后 60 天内没有交易日,交易日历数据异常"))
}

/// 快照自证今天是否开市并落库。周末、盘前(陈旧是正常的)返回 None 不下结论;
/// 探针代码没有报价是错误(网络或停牌),不是休市。
pub fn probe(conn: &Connection, source: &dyn QuoteSource, now: NaiveDateTime) -> Result<Option<bool>> {
    let today = now.date();
    if is_weekend(today) {
        return Ok(None);
    }
    let quotes = source.fetch(&[PROBE_CODE.to_string()])?;
    let Some(ts) = quotes.iter().map(|q| q.ts).max() else {
        return Err(anyhow!("日历探针 {PROBE_CODE} 无报价"));
    };
    if ts.date() == today {
        mark_day(conn, today, true, now)?;
        return Ok(Some(true));
    }
    if stale_means_holiday(now) {
        mark_day(conn, today, false, now)?;
        return Ok(Some(false));
    }
    Ok(None)
}
```

- [ ] **Step 5: 观察期天数改按交易日历**

`src/trade/admission/stats.rs`:删除 `workdays_between`(:292-303)及其测试 `workdays_skip_weekends`(约 :747-775,测试已移植到 `calendar.rs`);`paper_stats` 中

```rust
        days: workdays_between(since.date(), now.date()),
```

改为

```rust
        days: crate::trade::calendar::trading_days_between(conn, since.date(), now.date())?,
```

并把其文档注释「天数按工作日计」改为「天数按交易日历计(未证实的工作日按开市)」。清理因此不再使用的 `Weekday` 等 import。

- [ ] **Step 6: 运行确认通过 + 全量门禁**

Run: `cargo test --lib trade::` 然后按 Global Constraints 跑 fmt / clippy / test
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/trade
git commit -m "feat(trade): 交易日历(快照自证、不硬编码节假日),观察期按交易日计"
```

---

### Task 2: 准入判定收进 `submit_signal`,异动信号绑定异动策略

**Files:**
- Modify: `src/trade/service.rs`、`src/trade/monitor.rs:119-127`、`src/trade/movers.rs`、`src/trade/store.rs`
- Modify(适配): `tests/trade_core.rs`、`tests/trade_runtime.rs`、以及 `grep -rn "SubmitContext {" src tests` 找到的所有构造点
- Test: `src/trade/service.rs` 新增 `mod tests`;`src/trade/movers.rs` 内既有 `mod tests`

**Interfaces:**
- Consumes: `admission::state::admission_for(conn, user_id, strategy_id) -> Result<Admission>`
- Produces:
  ```rust
  pub struct SubmitContext<'a> { pub quote: Option<&'a Quote>, pub now: NaiveDateTime } // 去掉 admission
  pub fn resolve_admission(conn: &Connection, sig: &NewSignal) -> Result<Admission>;   // service.rs
  pub fn active_mover_strategy(conn: &Connection, user_id: i64, code: &str) -> Result<Option<StrategyDef>>; // store.rs
  pub fn active_mover_pools(conn: &Connection, user_id: i64) -> Result<Vec<String>>;   // store.rs
  ```

规则(`resolve_admission`):

| `strategy_id` | 来源 | 结果 |
|---|---|---|
| `Some(id)` | 任意 | `admission_for(conn, user_id, Some(id))`(不存在 / 不属于该用户 → `Blocked`) |
| `None` | Exit / Manual | `NotRequired` |
| `None` | Mover | `Probation`(未绑定策略的异动:仅模拟盘,保持旧行为) |
| `None` | Strategy | `Blocked`(日线策略信号必须有策略) |

- [ ] **Step 1: 写失败测试**

`src/trade/service.rs` 末尾新增:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::admission::state;
    use crate::trade::model::{AccountScope, NewStrategy, SignalSource, StrategyStatus};
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, now()).unwrap();
        c
    }

    fn sig(source: SignalSource, strategy_id: Option<i64>, key: &str) -> NewSignal {
        NewSignal {
            user_id: 1,
            source,
            strategy_id,
            code: "600000".into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::Both,
            ref_price: 10.0,
            reason: "t".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        }
    }

    fn quote() -> Quote {
        Quote {
            code: "600000".into(),
            price: 10.0,
            limit_up: Some(11.0),
            limit_down: Some(9.0),
            ts: now(),
        }
    }

    /// kind=mover 的策略提交后直接进入观察期,是造 Paper 状态最短的路径。
    fn paper_strategy(c: &Connection, user_id: i64) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "mover".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            now(),
        )
        .unwrap();
        state::submit_for_backtest(c, user_id, id, now()).unwrap();
        id
    }

    #[test]
    fn admission_follows_strategy_status_read_inside_submit() {
        let c = db();
        let id = paper_strategy(&c);
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Strategy, Some(id), "a")).unwrap(),
            Admission::Probation
        );
        state::update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Admitted, "x", now())
            .unwrap();
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Strategy, Some(id), "a")).unwrap(),
            Admission::Admitted
        );
        let mut other_user = sig(SignalSource::Strategy, Some(id), "a");
        other_user.user_id = 2;
        assert_eq!(resolve_admission(&c, &other_user).unwrap(), Admission::Blocked, "跨用户视为不存在");
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Strategy, None, "a")).unwrap(),
            Admission::Blocked
        );
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Mover, None, "a")).unwrap(),
            Admission::Probation
        );
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Manual, None, "a")).unwrap(),
            Admission::NotRequired
        );
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Exit, None, "a")).unwrap(),
            Admission::NotRequired
        );
    }

    #[test]
    fn submit_uses_current_status_paper_only_then_real_after_admission() {
        let mut c = db();
        let id = paper_strategy(&c);
        let q = quote();
        let ctx = SubmitContext { quote: Some(&q), now: now() };
        assert!(matches!(
            submit_signal(&mut c, &sig(SignalSource::Strategy, Some(id), "k1"), &ctx).unwrap(),
            SubmitOutcome::Ticketed { real_ticket: None, paper_ticket: Some(_), .. }
        ));
        state::update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Suspended, "x", now())
            .unwrap();
        let mut s2 = sig(SignalSource::Strategy, Some(id), "k2");
        s2.code = "600036".into();
        let q2 = Quote { code: "600036".into(), ..q.clone() };
        let ctx2 = SubmitContext { quote: Some(&q2), now: now() };
        assert!(matches!(
            submit_signal(&mut c, &s2, &ctx2).unwrap(),
            SubmitOutcome::Rejected { reason: GateReject::NotAdmitted, .. }
        ));
    }
}
```

> `state::update_status` 的签名以 `src/trade/admission/state.rs` 为准(测试里 `transitions_are_conditional_and_logged` 有调用示例);若 `Paper → Suspended` 不在合法转换表内,改用 `Paper → Failed`,断言不变。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::service`
Expected: 编译失败(`SubmitContext` 无 `admission` 以外字段的构造、`resolve_admission` 未定义)

- [ ] **Step 3: 实现 `service.rs`**

```rust
pub struct SubmitContext<'a> {
    pub quote: Option<&'a Quote>,
    pub now: NaiveDateTime,
}

/// 信号 → 闸门准入。在 `submit_signal` 的写事务内调用:状态读取与工单生成原子,
/// 不会出现「刚读到已准入、下一瞬间被 watchdog 暂停,却仍发出实盘工单」。
pub fn resolve_admission(conn: &Connection, sig: &NewSignal) -> Result<Admission> {
    use crate::trade::model::SignalSource;
    if sig.strategy_id.is_some() {
        return crate::trade::admission::state::admission_for(conn, sig.user_id, sig.strategy_id);
    }
    Ok(match sig.source {
        SignalSource::Exit | SignalSource::Manual => Admission::NotRequired,
        // 没绑定异动策略的异动信号:仅模拟盘(计划 2b 起的既有行为)
        SignalSource::Mover => Admission::Probation,
        // 日线策略信号必须来自某个策略;没有就是调用方的错,宁可拒绝
        SignalSource::Strategy => Admission::Blocked,
    })
}
```

`submit_signal` 中,`let rules = ...` 之前加 `let admission = resolve_admission(&tx, sig)?;`,`GateInput` 里 `admission: ctx.admission` 改为 `admission`。

- [ ] **Step 4: 适配所有调用点**

- `monitor.rs`:删掉 `admission: Admission::NotRequired,` 及不再使用的 `Admission` import。
- `tests/trade_core.rs`、`tests/trade_runtime.rs`:删掉每个 `SubmitContext` 里的 `admission:` 行。`tests/trade_core.rs::strategy_admission_controls_accounts` 原来靠传入 `Admission::Probation` / `Blocked` 构造场景,改为真实策略:`probation` 信号用 `store::create_strategy`(kind `mover`)+ `state::submit_for_backtest` 得到的 Paper 策略 id 填 `strategy_id`;`other`(期望 `NotAdmitted`)保持 `strategy_id: None`、来源 `Strategy`。断言不变。
- 其余 `Admission` 未使用的 import 一并删掉(clippy 会报)。

- [ ] **Step 5: 异动绑定策略——写失败测试**

`src/trade/store.rs`:

```rust
/// 该用户观察期 / 已准入、且股票池含 `code` 的异动策略;已准入优先,同状态取 id 最小。
pub fn active_mover_strategy(
    conn: &Connection,
    user_id: i64,
    code: &str,
) -> Result<Option<StrategyDef>> {
    let mut best: Option<StrategyDef> = None;
    for s in list_strategies(conn, user_id)? {
        if s.kind != "mover" || !s.pool.iter().any(|c| c == code) {
            continue;
        }
        let rank = match s.status {
            StrategyStatus::Admitted => 0,
            StrategyStatus::Paper => 1,
            _ => continue,
        };
        let better = match &best {
            None => true,
            Some(b) => {
                let b_rank = if b.status == StrategyStatus::Admitted { 0 } else { 1 };
                (rank, s.id) < (b_rank, b.id)
            }
        };
        if better {
            best = Some(s);
        }
    }
    Ok(best)
}

/// 该用户观察期 / 已准入的异动策略股票池并集(去重、排序)。
pub fn active_mover_pools(conn: &Connection, user_id: i64) -> Result<Vec<String>> {
    let mut codes: Vec<String> = list_strategies(conn, user_id)?
        .into_iter()
        .filter(|s| {
            s.kind == "mover"
                && matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted)
        })
        .flat_map(|s| s.pool)
        .collect();
    codes.sort();
    codes.dedup();
    Ok(codes)
}
```

`src/trade/movers.rs` 的 `mod tests` 新增(沿用该文件已有的 `db`/`cache_quote`/`mover` 等辅助函数;若辅助函数名不同,按文件现状调整):

```rust
    #[test]
    fn mover_signal_binds_to_active_mover_strategy_and_reaches_real_once_admitted() {
        use crate::trade::admission::state;
        use crate::trade::model::{NewStrategy, StrategyStatus};
        let mut c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0)).unwrap();
        // 不在自选里,只在异动策略股票池里:也应被处理
        let sid = store::create_strategy(
            &c,
            &NewStrategy {
                user_id: 1,
                name: "异动".into(),
                kind: "mover".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 0),
        )
        .unwrap();
        state::submit_for_backtest(&c, 1, sid, at(9, 1)).unwrap();
        state::update_status(&c, 1, sid, StrategyStatus::Paper, StrategyStatus::Admitted, "x", at(9, 2))
            .unwrap();
        // 不写推送配置:自选为空,候选代码只来自异动策略股票池(Step 6 的新行为)
        cache_quote(&c, "600000", 10.0, Some(11.0), at(10, 30));
        let r = submit_mover_signals(&mut c, &[mover("600000", Divergence::Absorb)], at(10, 30)).unwrap();
        assert_eq!(r.ticketed, 1);
        let (strategy_id, real): (Option<i64>, i64) = c
            .query_row(
                "SELECT s.strategy_id,
                        (SELECT COUNT(*) FROM trade_tickets t WHERE t.signal_id = s.id AND t.account = 'real')
                 FROM trade_signals s",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(strategy_id, Some(sid), "信号绑定到异动策略");
        assert_eq!(real, 1, "已准入的异动策略可以生成实盘工单");
    }
```

> `Divergence` 的「买入」变体名以 `src/stock/realtime/movers.rs` 的 `trade_action` 为准:取一个 `trade_action` 返回 `TradeAction::Buy` 的变体。

- [ ] **Step 6: 实现 `movers.rs`**

- `mover_signal` 增加参数 `strategy_id: Option<i64>`,写入 `NewSignal.strategy_id`;`reason` 里的「(未经前瞻检验,观察期仅模拟盘)」只在 `strategy_id` 为 `None` 时追加,绑定策略时改为「(异动策略 #{id})」。
- `process_user`:
  - 候选代码 = `realtime_watch_stocks` ∪ `store::active_mover_pools(conn, uid)?`;推送配置不存在时自选视为空(不再直接 `return Ok(())`),两者皆空才返回。
  - 每个命中的异动:`let bound = store::active_mover_strategy(conn, uid, &m.code)?;`
  - 卖出前的持仓判定:绑定策略时实盘或模拟盘任一有持仓即可;未绑定时保持只看模拟盘。
  - 构造 `SubmitContext { quote: Some(&quote), now }`(准入由 `submit_signal` 自行判定)。
- 模块文档首行改为:「实时异动 → 交易信号。绑定了观察期 / 已准入异动策略的按其准入状态出单,否则只进模拟盘。」

- [ ] **Step 7: 运行确认通过 + 全量门禁 + Commit**

Run: `cargo test --all-targets --no-fail-fast`(及 fmt / clippy)
Expected: 除既有失败外全部通过

```bash
git add src/trade tests
git commit -m "feat(trade): 准入在 submit_signal 事务内判定,异动信号绑定异动策略"
```

---

### Task 3: 前推回测产出实盘参数与真实数据跨度

**Files:**
- Modify: `src/trade/admission/walk_forward.rs`、`src/trade/admission/worker.rs:115`、`src/trade/admission/state.rs:180-225`、`src/trade/admission/judge.rs`(测试里的 `CodeMetrics` 字面量)
- Test: `walk_forward.rs`、`state.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // CodeResult 新增
  pub live_params: Option<toml::Value>,
  // CodeMetrics 新增(均 #[serde(default)],兼容旧记录)
  pub data_from: Option<NaiveDate>,
  pub data_to: Option<NaiveDate>,
  pub live_params: Option<toml::Value>,
  // PoolMetrics 新增(#[serde(default)])
  pub data_from: Option<NaiveDate>,
  pub data_to: Option<NaiveDate>,
  impl PoolMetrics { pub fn live_params_for(&self, code: &str) -> Option<&toml::Value>; }
  ```

- [ ] **Step 1: 写失败测试**

`walk_forward.rs` 的 `mod tests` 新增(复用该文件已有的 `bars(start, prices)` 辅助函数;网格沿用该文件其它 `run_code` 用例所用的策略类型与网格):

```rust
    #[test]
    fn run_code_selects_live_params_on_the_trailing_train_window() {
        // 4 年锯齿行情:足够产出若干检验窗,且最后 train_days 天内也有足够 K 线
        let prices: Vec<f64> = (0..1040)
            .map(|i| 10.0 + ((i % 40) as f64 - 20.0).abs() * 0.1)
            .collect();
        let b = bars(d(2021, 1, 4), &prices);
        let grid: toml::Table = "short_window = [5, 10]\nlong_window = [20]\namount = [100000.0]"
            .parse()
            .unwrap();
        let cfg = WalkForwardCfg::default();
        let r = run_code("trend", "600000", &b, &grid, &cfg).unwrap();
        let live = r.live_params.clone().expect("最近训练窗应选出参数");
        assert!(live.get("short_window").is_some());
        let m = r.metrics();
        assert_eq!(m.live_params, Some(live));
        assert_eq!(m.data_from, Some(b.first().unwrap().date));
        assert_eq!(m.data_to, Some(b.last().unwrap().date));
        let pool = aggregate(vec![m]);
        assert_eq!(pool.data_from, Some(b.first().unwrap().date));
        assert_eq!(pool.data_to, Some(b.last().unwrap().date));
        assert!(pool.live_params_for("600000").is_some());
        assert!(pool.live_params_for("000001").is_none());
    }

    #[test]
    fn old_metrics_json_without_new_fields_still_deserializes() {
        let m = aggregate(vec![sample_code_metrics("600000", 0.1)]);
        let mut v = serde_json::to_value(&m).unwrap();
        v.as_object_mut().unwrap().remove("data_from");
        v.as_object_mut().unwrap().remove("data_to");
        for c in v["codes"].as_array_mut().unwrap() {
            let o = c.as_object_mut().unwrap();
            o.remove("live_params");
            o.remove("data_from");
            o.remove("data_to");
        }
        let back: PoolMetrics = serde_json::from_value(v).unwrap();
        assert_eq!(back.codes[0].live_params, None);
        assert_eq!(back.data_from, None);
    }
```

> `trend` 的参数名以 `src/config.rs` 的 `TrendParams` 为准;若该文件其它用例用的是别的策略与网格,照抄那一组并把断言里的 `"short_window"` 换成其中一个键。

`state.rs` 的 `mod tests` 新增:

```rust
    #[test]
    fn monthly_verdict_on_a_non_running_strategy_writes_nothing() {
        let c = db();
        let id = strategy(&c, "rsi"); // Draft
        let verdict = crate::trade::admission::judge::Verdict { passed: true, reasons: vec![] };
        let t = apply_monthly_verdict(
            &c, 1, id, &empty_metrics(), &verdict,
            at(16, 9, 0).date(), at(16, 9, 0).date(), at(16, 9, 0),
        )
        .unwrap();
        assert_eq!(t, Transition::AlreadyHandled);
        assert!(store::latest_eval(&c, id, 1, "oos").unwrap().is_none(), "草稿不应写 oos 基线");
    }
```

> `Verdict` 的字段以 `judge.rs` 为准。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission`
Expected: 编译失败(新字段不存在)

- [ ] **Step 3: 实现 `walk_forward.rs`**

1. 把 `run_code` 里「对每个参数组合跑训练窗、按 metric 取最优」的循环抽成:

```rust
/// 在训练数据上按 `cfg.metric` 选参;返回 (参数, 训练窗夏普)。
fn select_params(
    kind: &str,
    code: &str,
    train: &[StockBar],
    combos: &[toml::Value],
    cfg: &WalkForwardCfg,
) -> Result<Option<(toml::Value, f64)>> {
    let mut best: Option<(toml::Value, f64, f64)> = None;
    for params in combos {
        let run = run_once(kind, code, StockData::new(train.to_vec()), params, cfg)?;
        let score = metric_of(&run.summary, &cfg.metric);
        if best.as_ref().is_none_or(|(_, s, _)| score > *s) {
            best = Some((params.clone(), score, run.summary.sharpe));
        }
    }
    Ok(best.map(|(p, _, is_sharpe)| (p, is_sharpe)))
}
```

   窗口循环改为 `let Some((params, is_sharpe)) = select_params(kind, code, &train, &combos, cfg)? else { continue; };`,其余不变(基金与既有测试期望值不变)。

2. 循环之后、`Ok(CodeResult { .. })` 之前:

```rust
    // 实盘参数:「最新的训练窗」= 以最后一根 K 线为终点往前 train_days 天。
    // 与各检验窗前的选参完全同口径,只是终点推到了今天(见计划 3e 设计裁决 1)。
    let live_from = last.date - Duration::days(cfg.train_days);
    let live_train: Vec<StockBar> = bars.iter().copied().filter(|b| b.date > live_from).collect();
    let live_params = if live_train.len() >= MIN_TRAIN_BARS {
        select_params(kind, code, &live_train, &combos, cfg)?.map(|(p, _)| p)
    } else {
        None
    };
```

   `CodeResult` 加 `pub live_params: Option<toml::Value>`。

3. `CodeMetrics` 加三个字段(放在 `window_details` 之后,各带 `#[serde(default)]` 与一行中文注释),`metrics()` 里填 `data_from: Some(self.data_from)`、`data_to: Some(self.data_to)`、`live_params: self.live_params.clone()`。
4. `PoolMetrics` 加 `data_from`/`data_to`(`#[serde(default)]`),`aggregate` 里取 `codes` 的 `data_from` 最小值、`data_to` 最大值(`filter_map` 后 `min()`/`max()`)。
5. 

```rust
impl PoolMetrics {
    /// 某只股票的实盘参数;未评估或最近训练窗数据不足时为 None。
    pub fn live_params_for(&self, code: &str) -> Option<&toml::Value> {
        self.codes.iter().find(|c| c.code == code)?.live_params.as_ref()
    }
}
```

6. 修好所有 `CodeMetrics { .. }` 字面量(`judge.rs` 两处、`walk_forward.rs` 测试三处):补 `data_from: None, data_to: None, live_params: None`。

- [ ] **Step 4: 实现 worker / state**

`worker.rs` 的 WalkForward 分支:

```rust
            let (from, to) = (ctx.now.date(), ctx.now.date());
```

改为

```rust
            // 评估记录的数据跨度用真实 K 线区间;全池都没评估出来时退回评估当天。
            let from = outcome.metrics.data_from.unwrap_or(ctx.now.date());
            let to = outcome.metrics.data_to.unwrap_or(ctx.now.date());
```

`state.rs::apply_monthly_verdict`:取到 `s` 后立即

```rust
    // 月度重跑只对在用的策略有意义。草稿 / 未通过 / 已暂停的重跑若也写 oos,
    // 就会改写其余各关读取的基线(计划 3c 遗留项)。
    if !matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted) {
        return Ok(Transition::AlreadyHandled);
    }
```

- [ ] **Step 5: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade
git commit -m "feat(trade): 前推回测产出实盘参数与真实数据跨度,月度裁决只作用于在用策略"
```

---

### Task 4: 引擎次日决策与策略信号纯函数

**Files:**
- Modify: `src/engine.rs`、`src/stock/backtest.rs`、`src/trade/mod.rs`
- Create: `src/trade/strategy_signal.rs`
- Test: `src/engine.rs`、`src/trade/strategy_signal.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `config::build_strategy_from`、`stock::ashare::AShareExecution`、`WalkForwardCfg { train_days, initial_cash, slippage, .. }`
- Produces:
  ```rust
  impl<D: DataHandler, S: Strategy> Engine<D, S> {
      pub fn decide_next(&mut self, next: NaiveDate, history: &[MarketEvent]) -> Vec<SignalEvent>;
  }
  // stock/backtest.rs
  pub fn replay_and_decide(data: StockData, strategy: Box<dyn Strategy>, fee: StockFee,
      initial_cash: f64, exec: Box<dyn ExecutionModel>, next: NaiveDate) -> Vec<SignalEvent>;
  // trade/strategy_signal.rs
  #[derive(Debug, Clone, PartialEq)]
  pub struct Decision { pub side: Direction, pub cash: Option<f64>, pub reason: String }
  pub fn decide(kind: &str, code: &str, params: &toml::Value, bars: &[StockBar],
      next: NaiveDate, cfg: &WalkForwardCfg) -> Result<Option<Decision>>;
  ```

- [ ] **Step 1: 写失败测试**

`src/engine.rs` 的 `mod tests` 新增:

```rust
    #[test]
    fn decide_next_sees_all_closed_bars_and_only_keeps_orderable_signals() {
        // 月定投:1/1、2/1 各买一次;追问 3/2 —— 已跨月,应再给出买入
        let points = vec![
            NavPoint { date: d(2024, 1, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 15), nav: 2.0, acc_nav: 2.0 },
        ];
        let history: Vec<MarketEvent> = points
            .iter()
            .map(|p| MarketEvent { date: p.date, nav: p.nav, adj_nav: p.acc_nav })
            .collect();
        let data = InMemoryData::new(points);
        let strat = Dca::new(Period::Monthly, 1, 1000.0);
        let mut engine = Engine::new(data, strat, Broker::new(no_fee()), Portfolio::new(0.0));
        engine.run();
        let sigs = engine.decide_next(d(2024, 3, 2), &history);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].direction, Direction::Buy);
        // 同月再问一次:定投已触发,不应再买
        assert!(engine.decide_next(d(2024, 3, 5), &history).is_empty());
        // 空历史不决策
        assert!(engine.decide_next(d(2024, 3, 6), &[]).is_empty());
    }
```

> `InMemoryData::new` 对 `NavPoint` 到 `MarketEvent` 的映射(`adj_nav` 取 `acc_nav` 还是别的)以 `src/data/mod.rs` 为准,`history` 按同样的映射构造。

`src/trade/strategy_signal.rs` 的测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// 连续交易日(跳过周末)。
    fn bars(start: NaiveDate, prices: &[f64]) -> Vec<StockBar> {
        let mut out = Vec::new();
        let mut date = start;
        for p in prices {
            while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                date += chrono::Duration::days(1);
            }
            out.push(StockBar { date, open: *p, high: *p, low: *p, close: *p, volume: 1.0, adj_close: *p });
            date += chrono::Duration::days(1);
        }
        out
    }

    fn trend_params() -> toml::Value {
        toml::Value::Table(
            "short_window = 5\nlong_window = 20\namount = 100000.0".parse().unwrap(),
        )
    }

    #[test]
    fn golden_cross_on_the_last_bar_means_buy_next_day() {
        // 长期下跌后最后几天急拉:短均线在最后一根 K 线上穿长均线
        let mut p: Vec<f64> = (0..80).map(|i| 20.0 - i as f64 * 0.1).collect();
        p.extend([13.0, 15.0, 17.0, 19.0, 21.0]);
        let b = bars(d(2026, 5, 4), &p);
        let next = b.last().unwrap().date + chrono::Duration::days(1);
        let dec = decide("trend", "600000", &trend_params(), &b, next, &WalkForwardCfg::default())
            .unwrap()
            .expect("应给出买入");
        assert_eq!(dec.side, Direction::Buy);
        assert_eq!(dec.cash, Some(100000.0));
        assert!(dec.reason.contains(&b.last().unwrap().date.to_string()));
    }

    #[test]
    fn flat_market_means_no_action() {
        let b = bars(d(2026, 5, 4), &[10.0; 80]);
        let next = b.last().unwrap().date + chrono::Duration::days(1);
        assert_eq!(
            decide("trend", "600000", &trend_params(), &b, next, &WalkForwardCfg::default()).unwrap(),
            None
        );
    }

    #[test]
    fn replay_only_uses_the_trailing_train_window() {
        // 很久以前的一段行情若被回放,会让策略在窗口起点就持仓;只回放最近 train_days 天
        // 时结论必须与「只给最近这段数据」完全一致。
        let mut p: Vec<f64> = (0..600).map(|i| 10.0 + (i % 7) as f64).collect();
        p.extend((0..80).map(|i| 20.0 - i as f64 * 0.1));
        p.extend([13.0, 15.0, 17.0, 19.0, 21.0]);
        let all = bars(d(2023, 1, 2), &p);
        let cfg = WalkForwardCfg { train_days: 120, ..WalkForwardCfg::default() };
        let next = all.last().unwrap().date + chrono::Duration::days(1);
        let cut = all.last().unwrap().date - chrono::Duration::days(cfg.train_days);
        let recent: Vec<StockBar> = all.iter().copied().filter(|b| b.date > cut).collect();
        assert_eq!(
            decide("trend", "600000", &trend_params(), &all, next, &cfg).unwrap(),
            decide("trend", "600000", &trend_params(), &recent, next, &cfg).unwrap()
        );
    }

    #[test]
    fn empty_bars_or_bad_params_are_errors() {
        let next = d(2026, 9, 21);
        assert!(decide("trend", "600000", &trend_params(), &[], next, &WalkForwardCfg::default()).is_err());
        let bad = toml::Value::Table("short_window = \"x\"".parse().unwrap());
        let b = bars(d(2026, 5, 4), &[10.0; 30]);
        assert!(decide("trend", "600000", &bad, &b, next, &WalkForwardCfg::default()).is_err());
    }
}
```

> `trend` 的参数键以 `src/config.rs::TrendParams` 为准;`golden_cross_on_the_last_bar_means_buy_next_day` 的价格序列若在该策略的判定规则下金叉不落在最后一根 K 线上(以 `src/strategy/trend.rs` 为准),调整最后几个价格使其恰好在最后一根上穿,断言不变。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib engine::tests::decide_next && cargo test --lib trade::strategy_signal`
Expected: 编译失败

- [ ] **Step 3: 实现 `Engine::decide_next`**

`src/engine.rs`,`impl` 块内 `portfolio()` 之后:

```rust
    /// 回放结束后追问策略:以 `next` 为决策日、`history`(全部已收盘 bar,截止 `next` 的前一交易日)
    /// 为上下文,会发出什么信号。只保留回测里会变成订单的信号(如空仓时的卖出会被丢弃),
    /// 与 `run()` 的口径一致。不推进数据、不成交、不记账;策略内部状态会被推进(如定投的
    /// 当期已触发),因此同一引擎对同一 `next` 只应追问一次。
    pub fn decide_next(&mut self, next: NaiveDate, history: &[MarketEvent]) -> Vec<SignalEvent> {
        let Some(last) = history.last() else {
            return Vec::new();
        };
        let pos = self.broker.position();
        let ctx = StrategyContext {
            today: next,
            history,
            shares: pos.shares,
            avg_cost: pos.avg_cost,
            cash: self.portfolio.cash,
        };
        self.strategy
            .on_market(&ctx)
            .into_iter()
            .filter(|s| self.portfolio.on_signal(s, &pos, last).is_some())
            .collect()
    }
```

补 import:`use crate::event::{Event, MarketEvent, SignalEvent};`、`use chrono::NaiveDate;`。

`src/stock/backtest.rs`:

```rust
/// 回放 `data` 后追问次日决策(见 `Engine::decide_next`)。历史取 `data` 的全部 bar。
pub fn replay_and_decide(
    data: StockData,
    strategy: Box<dyn Strategy>,
    fee: StockFee,
    initial_cash: f64,
    exec: Box<dyn ExecutionModel>,
    next: NaiveDate,
) -> Vec<SignalEvent> {
    let history = data.events().to_vec();
    let mut engine = Engine::new(data, strategy, Broker::new(fee), Portfolio::new(initial_cash))
        .with_execution(exec);
    engine.run();
    engine.decide_next(next, &history)
}
```

并在 `src/stock/data/mod.rs` 的 `impl StockData` 里加:

```rust
    /// 全部 bar 的策略视图(不受游标影响)。
    pub fn events(&self) -> &[MarketEvent] {
        &self.bars
    }
```

- [ ] **Step 4: 实现 `strategy_signal.rs`**

```rust
//! 日线策略的次日决策:实盘参数 + 已收盘 K 线 → 明天开盘做什么。纯函数,无 IO。
//!
//! 与前推回测同一引擎、同一成交口径:回放最近 `train_days` 天建立策略状态与模拟持仓,
//! 再以下一个交易日为决策日追问一次(计划 3e 设计裁决 2)。

use crate::config::build_strategy_from;
use crate::event::{Direction, SignalAmount};
use crate::stock::ashare::AShareExecution;
use crate::stock::backtest;
use crate::stock::data::{StockBar, StockData};
use crate::stock::fee::StockFee;
use crate::trade::admission::walk_forward::WalkForwardCfg;
use anyhow::{anyhow, Result};
use chrono::{Duration, NaiveDate};

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub side: Direction,
    /// 买入金额(策略参数里的每笔金额);卖出为 None(全部可卖)
    pub cash: Option<f64>,
    pub reason: String,
}

pub fn decide(
    kind: &str,
    code: &str,
    params: &toml::Value,
    bars: &[StockBar],
    next: NaiveDate,
    cfg: &WalkForwardCfg,
) -> Result<Option<Decision>> {
    let Some(last) = bars.last() else {
        return Err(anyhow!("{code} 无 K 线,无法决策"));
    };
    let from = last.date - Duration::days(cfg.train_days);
    let replay: Vec<StockBar> = bars.iter().copied().filter(|b| b.date > from).collect();
    let prev = bars.iter().copied().rfind(|b| b.date <= from);
    let strategy = build_strategy_from(kind, &Some(params.clone()), &[])?;
    let signals = backtest::replay_and_decide(
        StockData::with_prev_bar(replay, prev),
        strategy,
        StockFee::a_share(),
        cfg.initial_cash,
        Box::new(AShareExecution::new(code, None, cfg.slippage)),
        next,
    );
    let Some(s) = signals.into_iter().next() else {
        return Ok(None);
    };
    let (cash, action) = match (s.direction, s.amount) {
        (Direction::Buy, SignalAmount::Cash(c)) => (Some(c), "买入"),
        (Direction::Sell, SignalAmount::AllOut) => (None, "清仓卖出"),
        (dir, amount) => {
            return Err(anyhow!("{code} 暂不支持的信号数量口径 {dir:?} {amount:?}"));
        }
    };
    Ok(Some(Decision {
        side: s.direction,
        cash,
        reason: format!(
            "日线策略 {kind}:基于 {} 收盘数据,{next} 开盘{action}(参数 {params})",
            last.date
        ),
    }))
}
```

`src/trade/mod.rs` 加 `pub mod strategy_signal;`。

- [ ] **Step 5: 运行确认通过 + 全量门禁(基金回测逐位不变) + Commit**

```bash
git add src/engine.rs src/stock src/trade
git commit -m "feat(trade): 引擎次日决策与日线策略信号纯函数"
```

---

### Task 5: 信号计划表、`[trade.signals]` 配置与收盘后计算

**Files:**
- Create: `src/trade/plans.rs`、`src/trade/daily_signals.rs`
- Modify: `src/trade/store.rs`(`SCHEMA`)、`src/trade/config.rs`、`config.toml`、`src/trade/mod.rs`、`src/trade/admission/thread.rs`
- Test: `plans.rs`、`daily_signals.rs`、`config.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 1 `calendar::{is_trading_day, next_trading_day}`;Task 3 `PoolMetrics::live_params_for`;Task 4 `strategy_signal::decide`
- Produces:
  ```rust
  // config.rs
  pub struct SignalCfg { pub enabled: bool, pub compute_hour: u32, pub compute_minute: u32,
      pub cutoff_hour: u32, pub retry_minutes: i64, pub emit_hour: u32, pub emit_minute: u32,
      pub emit_end_hour: u32, pub emit_end_minute: u32 }
  // TradeCfg 新增 pub signals: SignalCfg
  // plans.rs
  pub enum PlanStatus { Planned, Idle, Submitted, Dropped }
  pub struct NewPlan { pub user_id: i64, pub strategy_id: i64, pub version_hash: String, pub code: String,
      pub side: Option<Direction>, pub cash: Option<f64>, pub basis_date: NaiveDate, pub reason: String,
      pub status: PlanStatus }
  pub struct SignalPlan { pub id: i64, pub user_id: i64, pub strategy_id: i64, pub version_hash: String,
      pub code: String, pub side: Option<Direction>, pub cash: Option<f64>, pub basis_date: NaiveDate,
      pub reason: String, pub status: PlanStatus, pub note: Option<String> }
  pub fn insert_plan(conn: &Connection, p: &NewPlan, now: NaiveDateTime) -> Result<bool>;
  pub fn has_plan(conn: &Connection, strategy_id: i64, code: &str, basis: NaiveDate) -> Result<bool>;
  pub fn due_plans(conn: &Connection, today: NaiveDate) -> Result<Vec<SignalPlan>>;
  pub fn settle_plan(conn: &Connection, id: i64, status: PlanStatus, note: &str, now: NaiveDateTime) -> Result<bool>;
  // daily_signals.rs
  pub struct ComputeReport { pub skipped_closed: bool, pub planned: usize, pub idle: usize, pub pending: usize, pub errors: Vec<String> }
  pub fn compute<F>(conn: &Connection, now: NaiveDateTime, wf: &WalkForwardCfg, load: F) -> Result<ComputeReport>
      where F: FnMut(&str) -> Result<Vec<StockBar>>;
  pub struct ComputeState { pub done_for: Option<NaiveDate>, pub last_attempt: Option<NaiveDateTime> }
  pub fn compute_due(now: NaiveDateTime, cfg: &SignalCfg, st: &ComputeState) -> bool;
  pub fn run_compute<F>(conn: &Connection, cfg: &SignalCfg, wf: &WalkForwardCfg, st: &mut ComputeState,
      now: NaiveDateTime, load: F) -> Option<ComputeReport> where F: FnMut(&str) -> Result<Vec<StockBar>>;
  ```

- [ ] **Step 1: 配置——写失败测试、实现**

`config.rs` 测试:

```rust
    #[test]
    fn signals_section_defaults_and_validation() {
        let cfg = from_toml_str("").unwrap();
        assert_eq!(cfg.signals, SignalCfg::default());
        assert!(cfg.signals.enabled);
        assert_eq!((cfg.signals.compute_hour, cfg.signals.compute_minute), (15, 30));
        assert_eq!((cfg.signals.emit_hour, cfg.signals.emit_minute), (9, 25));
        let cfg = from_toml_str("[trade.signals]\nenabled = false\nretry_minutes = 5").unwrap();
        assert!(!cfg.signals.enabled);
        assert_eq!(cfg.signals.retry_minutes, 5);
        for bad in [
            "[trade.signals]\ncompute_hour = 24",
            "[trade.signals]\ncompute_minute = 60",
            "[trade.signals]\ncutoff_hour = 15",          // 截止必须晚于开始
            "[trade.signals]\nretry_minutes = 0",
            "[trade.signals]\nretry_minutes = 121",
            "[trade.signals]\nemit_end_hour = 9\nemit_end_minute = 25", // 窗口为空
            "[trade.signals]\nemit_hour = 8",             // 早于集合竞价结束
            "[trade.signals]\nunknown = 1",
        ] {
            assert!(from_toml_str(bad).is_err(), "{bad}");
        }
    }
```

实现(照 `EvalCfg` 的写法,放在它之后):

```rust
/// 日线策略信号配置(计划 3e):收盘后计算、次日开盘发出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalCfg {
    /// 是否计算并发出日线策略信号
    pub enabled: bool,
    /// 收盘后开始计算的时刻
    pub compute_hour: u32,
    pub compute_minute: u32,
    /// 计算截止(整点,不含):到点后当日不再重试
    pub cutoff_hour: u32,
    /// K 线未更新时的重试间隔(分钟)
    pub retry_minutes: i64,
    /// 次日发出窗口 [emit, emit_end)
    pub emit_hour: u32,
    pub emit_minute: u32,
    pub emit_end_hour: u32,
    pub emit_end_minute: u32,
}

impl Default for SignalCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            compute_hour: 15,
            compute_minute: 30,
            cutoff_hour: 21,
            retry_minutes: 10,
            emit_hour: 9,
            emit_minute: 25,
            emit_end_hour: 10,
            emit_end_minute: 30,
        }
    }
}
```

`TradeCfg` 加 `pub signals: SignalCfg,`(Default 里 `signals: SignalCfg::default()`)。`from_toml_str` 末尾按既有风格追加校验,报错前缀 `[trade.signals]`:

- 所有 `*_hour` < 24,所有 `*_minute` < 60
- `cutoff_hour * 60 > compute_hour * 60 + compute_minute`
- `retry_minutes` ∈ [1, 120]
- `emit_hour * 60 + emit_minute >= 9 * 60 + 25`(集合竞价结果出来之前没有当天报价)
- `emit_end_hour * 60 + emit_end_minute > emit_hour * 60 + emit_minute`

`config.toml` 在 `[trade.eval]` 样例之后追加:

```toml
# [trade.signals]
# enabled = true        # 观察期 / 已准入的日线策略:收盘后计算、次日开盘发出工单
# compute_hour = 15     # 收盘后开始计算(K 线未更新会每 retry_minutes 分钟重试)
# compute_minute = 30
# cutoff_hour = 21      # 当日计算截止
# retry_minutes = 10
# emit_hour = 9         # 次日发出窗口开始(集合竞价结束后)
# emit_minute = 25
# emit_end_hour = 10    # 窗口结束:已证实开市的日子仍未发出的计划作废
# emit_end_minute = 30
```

- [ ] **Step 2: 计划表——建表、写失败测试、实现**

`SCHEMA` 追加:

```sql
CREATE TABLE IF NOT EXISTS trade_strategy_plans (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id      INTEGER NOT NULL,
  strategy_id  INTEGER NOT NULL,
  version_hash TEXT NOT NULL,
  code         TEXT NOT NULL,
  side         TEXT,
  cash         REAL,
  basis_date   TEXT NOT NULL,
  reason       TEXT NOT NULL,
  status       TEXT NOT NULL,
  note         TEXT,
  created_at   TEXT NOT NULL,
  settled_at   TEXT
);
-- 每个策略每只股票每个基准日只算一次:重启、重试都靠它幂等
CREATE UNIQUE INDEX IF NOT EXISTS idx_trade_strategy_plans_key
  ON trade_strategy_plans(strategy_id, code, basis_date);
CREATE INDEX IF NOT EXISTS idx_trade_strategy_plans_status ON trade_strategy_plans(status, basis_date);
```

`plans.rs` 测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::store;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, day).unwrap()
    }
    fn at(day: u32, h: u32) -> NaiveDateTime {
        d(day).and_hms_opt(h, 0, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }
    fn plan(code: &str, basis: NaiveDate, status: PlanStatus) -> NewPlan {
        NewPlan {
            user_id: 1,
            strategy_id: 7,
            version_hash: "h".into(),
            code: code.into(),
            side: (status == PlanStatus::Planned).then_some(Direction::Buy),
            cash: Some(5000.0),
            basis_date: basis,
            reason: "r".into(),
            status,
        }
    }

    #[test]
    fn insert_is_idempotent_per_strategy_code_basis() {
        let c = db();
        assert!(insert_plan(&c, &plan("600000", d(16), PlanStatus::Planned), at(16, 15)).unwrap());
        assert!(!insert_plan(&c, &plan("600000", d(16), PlanStatus::Idle), at(16, 16)).unwrap());
        assert!(has_plan(&c, 7, "600000", d(16)).unwrap());
        assert!(!has_plan(&c, 7, "600000", d(17)).unwrap());
    }

    #[test]
    fn due_plans_are_planned_rows_with_an_earlier_basis_and_settle_once() {
        let c = db();
        insert_plan(&c, &plan("600000", d(16), PlanStatus::Planned), at(16, 15)).unwrap();
        insert_plan(&c, &plan("600036", d(16), PlanStatus::Idle), at(16, 15)).unwrap();
        insert_plan(&c, &plan("000001", d(17), PlanStatus::Planned), at(17, 15)).unwrap();
        let due = due_plans(&c, d(17)).unwrap();
        assert_eq!(due.len(), 1, "idle 不发、今天算的明天才发");
        assert_eq!(due[0].code, "600000");
        assert_eq!(due[0].side, Some(Direction::Buy));
        assert!(settle_plan(&c, due[0].id, PlanStatus::Submitted, "ok", at(17, 9)).unwrap());
        assert!(!settle_plan(&c, due[0].id, PlanStatus::Dropped, "x", at(17, 10)).unwrap(), "只结一次");
        assert!(due_plans(&c, d(17)).unwrap().is_empty());
    }
}
```

实现要点(按 `store.rs` 的读写风格):

- `PlanStatus` 带 `as_str()`/`parse()`,字符串 `planned`/`idle`/`submitted`/`dropped`;`derive(Debug, Clone, Copy, PartialEq, Eq)`
- `insert_plan`:`INSERT OR IGNORE`,返回 `changes() == 1`;`side` 用 `model::side_str`,`None` 存 NULL;日期用 `DATE_FMT`,时间用 `fmt_ts`
- `due_plans`:`WHERE status = 'planned' AND basis_date < ?1 ORDER BY id`
- `settle_plan`:`UPDATE … SET status = ?, note = ?, settled_at = ? WHERE id = ? AND status = 'planned'`,返回是否影响 1 行

- [ ] **Step 3: 收盘后计算——写失败测试**

`daily_signals.rs` 测试(本步只含 compute 相关;发出相关在 Task 6 追加):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::{state, walk_forward};
    use crate::trade::model::{NewStrategy, StrategyStatus};
    use crate::trade::plans::{self, PlanStatus};
    use crate::trade::store;
    use chrono::Datelike;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    fn at(m: u32, day: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(m, day).and_hms_opt(h, mi, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    /// 以 `last` 为最后一天、往前连续交易日(跳过周末)的 K 线。
    fn bars_until(last: NaiveDate, prices: &[f64]) -> Vec<StockBar> {
        let mut dates = Vec::new();
        let mut day = last;
        while dates.len() < prices.len() {
            if !matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                dates.push(day);
            }
            day -= chrono::Duration::days(1);
        }
        dates.reverse();
        dates
            .into_iter()
            .zip(prices)
            .map(|(date, p)| StockBar { date, open: *p, high: *p, low: *p, close: *p, volume: 1.0, adj_close: *p })
            .collect()
    }

    /// 最后一根 K 线上金叉(同 strategy_signal 的用例)。
    fn golden_cross(last: NaiveDate) -> Vec<StockBar> {
        let mut p: Vec<f64> = (0..80).map(|i| 20.0 - i as f64 * 0.1).collect();
        p.extend([13.0, 15.0, 17.0, 19.0, 21.0]);
        bars_until(last, &p)
    }

    /// 一只股票的样本外指标,只有 `code` 与 `live_params` 对本模块有意义。
    fn code_metrics(code: &str, live: bool) -> walk_forward::CodeMetrics {
        walk_forward::CodeMetrics {
            code: code.into(),
            windows: 1,
            oos_return: 0.1,
            oos_annualized: 0.1,
            oos_sharpe: 1.0,
            oos_max_drawdown: 0.1,
            oos_trades: 10,
            is_sharpe: 1.0,
            years: 1.0,
            data_years: 4.0,
            buy_hold_return: 0.0,
            trade_baseline: Default::default(),
            window_details: Vec::new(),
            data_from: None,
            data_to: None,
            live_params: live.then(|| {
                toml::Value::Table(
                    "short_window = 5\nlong_window = 20\namount = 100000.0".parse().unwrap(),
                )
            }),
        }
    }

    /// 观察期的 trend 策略;oos 评估里 `with_params` 中的代码带实盘参数,池内其余代码没有。
    fn running_trend_strategy(c: &Connection, pool: &[&str], with_params: &[&str]) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "趋势".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]\nlong_window = [20]\namount = [100000.0]".into(),
                pool: pool.iter().map(|s| s.to_string()).collect(),
            },
            at(9, 1, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(9, 1, 9, 1)).unwrap();
        let metrics = walk_forward::aggregate(
            pool.iter()
                .map(|code| code_metrics(code, with_params.contains(code)))
                .collect(),
        );
        let verdict = crate::trade::admission::judge::Verdict {
            passed: true,
            reasons: Vec::new(),
        };
        state::apply_backtest_verdict(c, 1, id, &metrics, &verdict, d(9, 1), d(9, 1), at(9, 1, 9, 2))
            .unwrap();
        assert_eq!(
            store::get_strategy(c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper
        );
        id
    }
```

> `CodeMetrics` 字面量的字段以 Task 3 完成后的定义为准(若 `trade_baseline` 的类型没有实现 `Default`,照 `walk_forward.rs` 测试里的 `sample_code_metrics` 写法填)。

接着在同一 `mod tests` 追加用例:

```rust
    #[test]
    fn compute_plans_buy_for_next_trading_day_and_is_idempotent() {
        let c = db();
        let sid = running_trend_strategy(&c, &["600000", "600036"], &["600000", "600036"]);
        let today = at(9, 18, 15, 30); // 周五
        let mut loads = 0;
        let r = compute(&c, today, &WalkForwardCfg::default(), |code| {
            loads += 1;
            Ok(if code == "600000" { golden_cross(d(9, 18)) } else { bars_until(d(9, 18), &[10.0; 85]) })
        })
        .unwrap();
        assert_eq!((r.planned, r.idle, r.pending), (1, 1, 0));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // 下周一发出
        let due = plans::due_plans(&c, d(9, 21)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].strategy_id, sid);
        assert_eq!(due[0].side, Some(crate::event::Direction::Buy));
        assert_eq!(due[0].cash, Some(100000.0));
        assert_eq!(due[0].basis_date, d(9, 18));
        // 再跑一次:已有今天的行,不再加载 K 线
        let before = loads;
        let r2 = compute(&c, at(9, 18, 15, 40), &WalkForwardCfg::default(), |_| {
            loads += 1;
            Ok(Vec::new())
        })
        .unwrap();
        assert_eq!((r2.planned, r2.idle, r2.pending), (0, 0, 0));
        assert_eq!(loads, before);
    }

    #[test]
    fn stale_kline_is_pending_and_missing_params_is_idle() {
        let c = db();
        running_trend_strategy(&c, &["600000", "600036"], &["600000", "600036"]);
        let r = compute(&c, at(9, 18, 15, 30), &WalkForwardCfg::default(), |code| {
            Ok(if code == "600000" {
                golden_cross(d(9, 17)) // 还没有今天的 K 线
            } else {
                golden_cross(d(9, 18))
            })
        })
        .unwrap();
        assert_eq!(r.pending, 1, "600000 的 K 线未更新,留待重试");
        assert_eq!(r.planned, 1, "600036 有今天的 K 线与实盘参数,当天金叉");
        let c2 = db();
        running_trend_strategy(&c2, &["600000"], &[]);
        let r = compute(&c2, at(9, 18, 15, 30), &WalkForwardCfg::default(), |_| Ok(golden_cross(d(9, 18)))).unwrap();
        assert_eq!(r.idle, 1, "无实盘参数:记 idle(当天算完),不发信号");
    }

    #[test]
    fn compute_skips_closed_days_draft_strategies_and_movers() {
        let c = db();
        running_trend_strategy(&c, &["600000"], &["600000"]);
        crate::trade::calendar::mark_day(&c, d(10, 1), false, at(10, 1, 9, 31)).unwrap();
        let r = compute(&c, at(10, 1, 15, 30), &WalkForwardCfg::default(), |_| panic!("休市日不应加载")).unwrap();
        assert!(r.skipped_closed);
        // 草稿策略不参与
        let c2 = db();
        store::create_strategy(
            &c2,
            &NewStrategy {
                user_id: 1,
                name: "草稿".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 1, 9, 0),
        )
        .unwrap();
        let r = compute(&c2, at(9, 18, 15, 30), &WalkForwardCfg::default(), |_| panic!("草稿不应加载")).unwrap();
        assert_eq!((r.planned, r.idle, r.pending), (0, 0, 0));
    }

    #[test]
    fn compute_due_paces_retries_and_stops_at_cutoff() {
        let cfg = SignalCfg::default();
        let mut st = ComputeState::default();
        assert!(!compute_due(at(9, 18, 15, 29), &cfg, &st), "未到点");
        assert!(compute_due(at(9, 18, 15, 30), &cfg, &st));
        assert!(!compute_due(at(9, 19, 15, 30), &cfg, &st), "周六");
        st.last_attempt = Some(at(9, 18, 15, 30));
        assert!(!compute_due(at(9, 18, 15, 35), &cfg, &st), "重试间隔内");
        assert!(compute_due(at(9, 18, 15, 40), &cfg, &st));
        assert!(!compute_due(at(9, 18, 21, 0), &cfg, &st), "截止");
        st.done_for = Some(d(9, 18));
        assert!(!compute_due(at(9, 18, 16, 0), &cfg, &st), "当日已完成");
    }

    #[test]
    fn run_compute_marks_done_only_when_nothing_is_pending() {
        let c = db();
        running_trend_strategy(&c, &["600000"], &["600000"]);
        let cfg = SignalCfg::default();
        let wf = WalkForwardCfg::default();
        let mut st = ComputeState::default();
        let r = run_compute(&c, &cfg, &wf, &mut st, at(9, 18, 15, 30), |_| Ok(golden_cross(d(9, 17)))).unwrap();
        assert_eq!(r.pending, 1);
        assert_eq!(st.done_for, None);
        assert_eq!(st.last_attempt, Some(at(9, 18, 15, 30)));
        assert!(run_compute(&c, &cfg, &wf, &mut st, at(9, 18, 15, 31), |_| unreachable!()).is_none(), "未到重试点");
        let r = run_compute(&c, &cfg, &wf, &mut st, at(9, 18, 15, 40), |_| Ok(golden_cross(d(9, 18)))).unwrap();
        assert_eq!(r.planned, 1);
        assert_eq!(st.done_for, Some(d(9, 18)));
    }
```

- [ ] **Step 4: 运行确认失败**

Run: `cargo test --lib trade::daily_signals trade::plans trade::config`
Expected: 编译失败

- [ ] **Step 5: 实现 `compute` / `compute_due` / `run_compute`**

```rust
//! 日线策略信号:收盘后计算(评估线程)→ 计划表 → 次日开盘发出(监听线程)。
//! 设计见计划 3e 设计裁决 1–7。

use crate::stock::data::StockBar;
use crate::trade::admission::walk_forward::{PoolMetrics, WalkForwardCfg};
use crate::trade::calendar;
use crate::trade::config::SignalCfg;
use crate::trade::model::StrategyStatus;
use crate::trade::plans::{self, NewPlan, PlanStatus};
use crate::trade::store;
use crate::trade::strategy_signal;
use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::Connection;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComputeReport {
    /// 今天已被证实休市,整轮跳过
    pub skipped_closed: bool,
    pub planned: usize,
    pub idle: usize,
    /// K 线未更新或加载失败,留待重试的(策略, 代码)数
    pub pending: usize,
    pub errors: Vec<String>,
}

pub fn compute<F>(
    conn: &Connection,
    now: NaiveDateTime,
    wf: &WalkForwardCfg,
    mut load: F,
) -> Result<ComputeReport>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let today = now.date();
    let mut r = ComputeReport::default();
    if !calendar::is_trading_day(conn, today)? {
        r.skipped_closed = true;
        return Ok(r);
    }
    let next = calendar::next_trading_day(conn, today)?;
    for uid in store::users_with_strategies(conn)? {
        for s in store::list_strategies(conn, uid)? {
            if s.kind == "mover"
                || !matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted)
            {
                continue;
            }
            // 基线读不出来是数据问题,记错误、跳过该策略,不影响其它策略
            let metrics: Option<PoolMetrics> = match store::latest_eval(conn, s.id, uid, "oos")? {
                Some((json, _)) => match serde_json::from_str(&json)
                    .with_context(|| format!("策略 {} 的 oos 评估无法反序列化", s.id))
                {
                    Ok(m) => Some(m),
                    Err(e) => {
                        r.errors.push(format!("{e:#}"));
                        continue;
                    }
                },
                None => None,
            };
            for code in &s.pool {
                if plans::has_plan(conn, s.id, code, today)? {
                    continue;
                }
                let base = |status: PlanStatus, reason: String| NewPlan {
                    user_id: uid,
                    strategy_id: s.id,
                    version_hash: s.version_hash.clone(),
                    code: code.clone(),
                    side: None,
                    cash: None,
                    basis_date: today,
                    reason,
                    status,
                };
                let Some(params) = metrics.as_ref().and_then(|m| m.live_params_for(code)) else {
                    plans::insert_plan(conn, &base(PlanStatus::Idle, "无实盘参数(前推回测未产出)".into()), now)?;
                    r.idle += 1;
                    continue;
                };
                let bars = match load(code) {
                    Ok(b) => b,
                    Err(e) => {
                        r.pending += 1;
                        r.errors.push(format!("策略 {} {code} K 线加载失败: {e:#}", s.id));
                        continue;
                    }
                };
                // K 线本身就是开市证明:最后一根不是今天就还没更新,留待重试(设计裁决 5)
                if bars.last().map(|b| b.date) != Some(today) {
                    r.pending += 1;
                    continue;
                }
                match strategy_signal::decide(&s.kind, code, params, &bars, next, wf) {
                    Ok(Some(dec)) => {
                        let p = NewPlan {
                            side: Some(dec.side),
                            cash: dec.cash,
                            ..base(PlanStatus::Planned, dec.reason)
                        };
                        plans::insert_plan(conn, &p, now)?;
                        r.planned += 1;
                    }
                    Ok(None) => {
                        plans::insert_plan(conn, &base(PlanStatus::Idle, "无操作".into()), now)?;
                        r.idle += 1;
                    }
                    Err(e) => {
                        // 决策报错(参数与策略不匹配等)重试也不会好:记 idle 并留下原因
                        plans::insert_plan(conn, &base(PlanStatus::Idle, format!("决策失败: {e:#}")), now)?;
                        r.idle += 1;
                        r.errors.push(format!("策略 {} {code} 决策失败: {e:#}", s.id));
                    }
                }
            }
        }
    }
    Ok(r)
}

/// 跨轮次的计算状态。不落库:重启后 `done_for` 为 None 会再跑一轮,
/// 但计划表的唯一键让已算过的(策略, 代码)直接跳过,不会重复加载或重复出信号。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComputeState {
    pub done_for: Option<NaiveDate>,
    pub last_attempt: Option<NaiveDateTime>,
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("配置已校验时刻合法")
}

pub fn compute_due(now: NaiveDateTime, cfg: &SignalCfg, st: &ComputeState) -> bool {
    let t = now.time();
    !crate::stock::realtime::calendar::is_weekend(now.date())
        && t >= hm(cfg.compute_hour, cfg.compute_minute)
        && t < hm(cfg.cutoff_hour, 0)
        && st.done_for != Some(now.date())
        && st.last_attempt.is_none_or(|a| {
            a.date() != now.date() || (now - a).num_minutes() >= cfg.retry_minutes
        })
}

/// 到点才算;算完若无待重试项(且无错误)则标记当日完成。未到点返回 None。
pub fn run_compute<F>(
    conn: &Connection,
    cfg: &SignalCfg,
    wf: &WalkForwardCfg,
    st: &mut ComputeState,
    now: NaiveDateTime,
    load: F,
) -> Option<ComputeReport>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    if !cfg.enabled || !compute_due(now, cfg, st) {
        return None;
    }
    st.last_attempt = Some(now);
    let r = match compute(conn, now, wf, load) {
        Ok(r) => r,
        Err(e) => ComputeReport {
            errors: vec![format!("日线信号计算失败: {e:#}")],
            ..ComputeReport::default()
        },
    };
    if r.skipped_closed || (r.pending == 0 && r.errors.is_empty()) {
        st.done_for = Some(now.date());
    }
    Some(r)
}
```

> 注意:`compute` 顶层 `Err`(如数据库错误)经 `run_compute` 转成带错误的报告,不会标记完成,下个重试点再来。`errors` 非空但 `pending == 0` 的情况(决策失败已记 idle)也不标记完成——下一轮所有行都已存在,会零成本地确认完成。

`src/trade/mod.rs` 加 `pub mod daily_signals;`、`pub mod plans;`。

- [ ] **Step 6: 评估线程接线**

`src/trade/admission/thread.rs::run_loop`:循环外 `let mut signal_state = crate::trade::daily_signals::ComputeState::default();`;在 `catch_unwind` 闭包内、`let out = tick(...)` 之前:

```rust
            // 日线信号需要**今天**的 K 线,与前推回测的 `end`(昨天,见上)不同;
            // `load_or_fetch` 在缓存不含今天时会联网重抓,K 线还没更新则由
            // `compute` 判为待重试,按 `retry_minutes` 节奏再来。
            let today = now.date();
            let sig_start = today - chrono::Duration::days(cfg.walk_forward.train_days + 30);
            if let Some(r) = crate::trade::daily_signals::run_compute(
                &conn,
                &cfg.signals,
                deps.wf,
                &mut signal_state,
                now,
                |code| {
                    crate::stock::data::cache::load_or_fetch(
                        code,
                        std::path::Path::new(".cache/stock"),
                        sig_start,
                        today,
                    )
                },
            ) {
                for e in &r.errors {
                    eprintln!("[trade] 日线信号: {e}");
                }
                if r.planned + r.idle > 0 {
                    println!(
                        "[trade] 日线信号:计划 {} 条、无操作 {} 条、待重试 {} 条",
                        r.planned, r.idle, r.pending
                    );
                }
            }
```

> `cfg.walk_forward.train_days` 的实际字段名以 `WalkForwardTuning` 为准;`deps` 需要在这段之前构造(把 `let deps = …` 挪到它前面)。

- [ ] **Step 7: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade config.toml
git commit -m "feat(trade): 信号计划表与收盘后日线策略计算"
```

---

### Task 6: 开盘发出、日历自证接线与端到端用例

**Files:**
- Modify: `src/trade/daily_signals.rs`、`src/trade/daemon.rs`
- Test: `src/trade/daily_signals.rs` 内 `mod tests`;`tests/trade_runtime.rs`

**Interfaces:**
- Consumes: Task 1 `calendar::{day_status, probe}`;Task 2 `SubmitContext { quote, now }`;Task 5 `plans::*`、`SignalCfg`
- Produces:
  ```rust
  #[derive(Debug, Clone, Default, PartialEq)]
  pub struct EmitReport { pub submitted: usize, pub dropped: usize, pub waiting: usize,
      pub new_real_tickets: Vec<i64>, pub errors: Vec<String> }
  pub fn emit_due(conn: &mut Connection, source: &dyn QuoteSource, cfg: &SignalCfg, now: NaiveDateTime) -> Result<EmitReport>;
  pub fn drop_unsent(conn: &Connection, cfg: &SignalCfg, now: NaiveDateTime) -> Result<usize>;
  ```

发出规则:

1. 不在 `[emit, emit_end)` 窗口、周末、`!cfg.enabled`、或今天已证实休市 → 返回空报告
2. `due_plans(today)` 为空 → 返回空报告(无网络请求)
3. 策略不存在 / `version_hash` 已变 / 状态不在观察期、已准入 → `dropped`(note 写明原因)
4. 其余计划的代码去重后一次拉报价,只要今天时间戳的,写入 `trade_quotes`
5. 有新鲜报价的计划:构造 `NewSignal`(来源 `Strategy`、`strategy_id`、`scope Both`、`ref_price` = 现价、`reason` = 计划理由、`dedup_key = "strategy-{sid}-{code}-{basis}"`、`suggest_cash` = 计划金额、`suggest_qty None`),`submit_signal`;任何结果(工单 / 拒绝 / 重复)都把计划结为 `submitted`,note 为结果(拒绝时写 `GateReject::as_str()`)
6. 无新鲜报价 → `waiting`,留到下一轮
7. `drop_unsent`:今天**已证实开市**且已过 `emit_end` → 把 `due_plans(today)` 全部结为 `dropped`(「开盘窗口内无有效报价,可能停牌」);未证实或休市的日子一律不动

- [ ] **Step 1: 写失败测试**

`daily_signals.rs` 的 `mod tests` 追加:

```rust
    use crate::trade::model::{Account, Quote};
    use crate::trade::quotes::QuoteSource;

    struct Stub(Vec<Quote>);
    impl QuoteSource for Stub {
        fn fetch(&self, codes: &[String]) -> Result<Vec<Quote>> {
            Ok(self.0.iter().filter(|q| codes.contains(&q.code)).cloned().collect())
        }
    }
    fn q(code: &str, price: f64, ts: NaiveDateTime) -> Quote {
        Quote { code: code.into(), price, limit_up: Some(price * 1.1), limit_down: Some(price * 0.9), ts }
    }

    /// 周五收盘算出 600000 的买入计划,返回策略 id。
    fn planned_buy(c: &Connection) -> i64 {
        store::set_capital(c, 1, Account::Real, 1_000_000.0, at(9, 1, 9, 0)).unwrap();
        let sid = running_trend_strategy(c, &["600000"], &["600000"]);
        let r = compute(c, at(9, 18, 15, 30), &WalkForwardCfg::default(), |_| Ok(golden_cross(d(9, 18)))).unwrap();
        assert_eq!(r.planned, 1);
        sid
    }

    #[test]
    fn emit_waits_for_the_window_and_fresh_quotes_then_submits_once() {
        let mut c = db();
        planned_buy(&c);
        let cfg = SignalCfg::default();
        let fresh = Stub(vec![q("600000", 21.0, at(9, 21, 9, 25))]);
        assert_eq!(emit_due(&mut c, &fresh, &cfg, at(9, 21, 9, 20)).unwrap(), EmitReport::default(), "窗口前");
        let stale = Stub(vec![q("600000", 21.0, at(9, 18, 15, 0))]);
        let r = emit_due(&mut c, &stale, &cfg, at(9, 21, 9, 25)).unwrap();
        assert_eq!((r.submitted, r.waiting), (0, 1), "报价仍是上周五的");
        let r = emit_due(&mut c, &fresh, &cfg, at(9, 21, 9, 26)).unwrap();
        assert_eq!(r.submitted, 1);
        assert!(r.new_real_tickets.is_empty(), "观察期只进模拟盘");
        let paper: i64 = c
            .query_row("SELECT COUNT(*) FROM trade_tickets WHERE account = 'paper'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(paper, 1);
        let r = emit_due(&mut c, &fresh, &cfg, at(9, 21, 9, 27)).unwrap();
        assert_eq!(r, EmitReport::default(), "计划已结,不重复发");
    }

    #[test]
    fn admitted_strategy_emits_a_real_ticket() {
        let mut c = db();
        let sid = planned_buy(&c);
        state::update_status(&c, 1, sid, StrategyStatus::Paper, StrategyStatus::Admitted, "x", at(9, 18, 16, 0)).unwrap();
        let fresh = Stub(vec![q("600000", 21.0, at(9, 21, 9, 25))]);
        let r = emit_due(&mut c, &fresh, &SignalCfg::default(), at(9, 21, 9, 26)).unwrap();
        assert_eq!(r.new_real_tickets.len(), 1);
    }

    #[test]
    fn changed_or_stopped_strategy_drops_the_plan_without_fetching() {
        let mut c = db();
        let sid = planned_buy(&c);
        state::update_status(&c, 1, sid, StrategyStatus::Paper, StrategyStatus::Failed, "x", at(9, 18, 16, 0)).unwrap();
        struct Boom;
        impl QuoteSource for Boom {
            fn fetch(&self, _: &[String]) -> Result<Vec<Quote>> {
                panic!("无可发计划时不应拉报价")
            }
        }
        let r = emit_due(&mut c, &Boom, &SignalCfg::default(), at(9, 21, 9, 26)).unwrap();
        assert_eq!(r.dropped, 1);
        assert!(plans::due_plans(&c, d(9, 21)).unwrap().is_empty());
    }

    #[test]
    fn holidays_keep_plans_and_confirmed_open_days_drop_leftovers_after_the_window() {
        let mut c = db();
        planned_buy(&c);
        let cfg = SignalCfg::default();
        // 周一被证实休市:不发、不丢
        crate::trade::calendar::mark_day(&c, d(9, 21), false, at(9, 21, 9, 31)).unwrap();
        let fresh_mon = Stub(vec![q("600000", 21.0, at(9, 21, 9, 25))]);
        assert_eq!(emit_due(&mut c, &fresh_mon, &cfg, at(9, 21, 9, 40)).unwrap(), EmitReport::default());
        assert_eq!(drop_unsent(&c, &cfg, at(9, 21, 11, 0)).unwrap(), 0);
        // 周二开市但停牌(无报价),窗口过后作废
        crate::trade::calendar::mark_day(&c, d(9, 22), true, at(9, 22, 9, 31)).unwrap();
        let r = emit_due(&mut c, &Stub(Vec::new()), &cfg, at(9, 22, 9, 40)).unwrap();
        assert_eq!(r.waiting, 1);
        assert_eq!(drop_unsent(&c, &cfg, at(9, 22, 10, 0)).unwrap(), 0, "窗口未结束");
        assert_eq!(drop_unsent(&c, &cfg, at(9, 22, 10, 30)).unwrap(), 1);
        assert!(plans::due_plans(&c, d(9, 22)).unwrap().is_empty());
    }
```

`tests/trade_runtime.rs` 追加端到端用例(串起:策略 → oos 基线 → 收盘后计算 → 开盘发出 → 模拟盘成交 → 观察期统计能数到这笔成交):

```rust
#[test]
fn daily_strategy_signal_flows_from_close_to_paper_fill() {
    // 1. 内存库 migrate,set_capital(real)
    // 2. 建 trend 策略(pool ["600000"]),submit_for_backtest,apply_backtest_verdict(passed)
    //    写入带 live_params 的 PoolMetrics(做法同 daily_signals 单测的 running_trend_strategy)
    // 3. daily_signals::compute(周五 15:30,K 线闭包返回最后一根为周五的金叉序列)→ planned == 1
    // 4. daily_signals::emit_due(下周一 09:26,桩报价 ts 为周一 09:25)→ submitted == 1
    // 5. 断言:trade_fills 有一条 account='paper' 的买入成交;
    //    admission::stats::strategy_fills(conn, 1, sid, Account::Paper, None) 返回 1 条
}
```

> 这是必须落实的用例:按注释里的 5 步写出完整代码(集成测试只能用 `xlh::` 公开接口;`running_trend_strategy` 在集成测试里重写一份,不能跨 crate 引用单测私有函数)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::daily_signals` 与 `cargo test --test trade_runtime`
Expected: 编译失败(`emit_due` / `drop_unsent` 未定义)

- [ ] **Step 3: 实现 `emit_due` / `drop_unsent`**

```rust
use crate::trade::model::{AccountScope, NewSignal, Quote, SignalSource};
use crate::trade::quotes::QuoteSource;
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmitReport {
    pub submitted: usize,
    pub dropped: usize,
    /// 无今日报价,留待下一轮
    pub waiting: usize,
    /// 本轮新建的实盘工单,供推送
    pub new_real_tickets: Vec<i64>,
    pub errors: Vec<String>,
}

fn in_emit_window(now: NaiveDateTime, cfg: &SignalCfg) -> bool {
    let t = now.time();
    t >= hm(cfg.emit_hour, cfg.emit_minute) && t < hm(cfg.emit_end_hour, cfg.emit_end_minute)
}

pub fn emit_due(
    conn: &mut Connection,
    source: &dyn QuoteSource,
    cfg: &SignalCfg,
    now: NaiveDateTime,
) -> Result<EmitReport> {
    let today = now.date();
    let mut r = EmitReport::default();
    if !cfg.enabled
        || crate::stock::realtime::calendar::is_weekend(today)
        || !in_emit_window(now, cfg)
        || calendar::day_status(conn, today)? == Some(false)
    {
        return Ok(r);
    }
    let mut live = Vec::new();
    for p in plans::due_plans(conn, today)? {
        let why = match store::get_strategy(conn, p.user_id, p.strategy_id)? {
            None => Some("策略已删除"),
            Some(s) if s.version_hash != p.version_hash => Some("策略定义已变更"),
            Some(s) if !matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted) => {
                Some("策略已不在观察期 / 已准入状态")
            }
            Some(_) => None,
        };
        match why {
            Some(note) => {
                if plans::settle_plan(conn, p.id, PlanStatus::Dropped, note, now)? {
                    r.dropped += 1;
                }
            }
            None => live.push(p),
        }
    }
    if live.is_empty() {
        return Ok(r);
    }
    let mut codes: Vec<String> = live.iter().map(|p| p.code.clone()).collect();
    codes.sort();
    codes.dedup();
    let fresh: Vec<Quote> = source
        .fetch(&codes)?
        .into_iter()
        .filter(|q| q.ts.date() == today)
        .collect();
    store::upsert_quotes(conn, &fresh, now)?;
    let quotes: HashMap<&str, &Quote> = fresh.iter().map(|q| (q.code.as_str(), q)).collect();
    for p in live {
        let Some(q) = quotes.get(p.code.as_str()) else {
            r.waiting += 1;
            continue;
        };
        let Some(side) = p.side else { continue };
        let sig = NewSignal {
            user_id: p.user_id,
            source: SignalSource::Strategy,
            strategy_id: Some(p.strategy_id),
            code: p.code.clone(),
            name: None,
            side,
            scope: AccountScope::Both,
            ref_price: q.price,
            reason: p.reason.clone(),
            ai_note: None,
            dedup_key: format!("strategy-{}-{}-{}", p.strategy_id, p.code, p.basis_date),
            suggest_cash: p.cash,
            suggest_qty: None,
        };
        // 单个计划失败(如写锁超时)不影响其余计划;计划保持 planned,下一轮重试
        let note = match submit_signal(conn, &sig, &SubmitContext { quote: Some(q), now }) {
            Ok(SubmitOutcome::Ticketed { real_ticket, .. }) => {
                r.new_real_tickets.extend(real_ticket);
                "已生成工单".to_string()
            }
            Ok(SubmitOutcome::Rejected { reason, .. }) => format!("闸门拒绝: {}", reason.as_str()),
            Ok(SubmitOutcome::Duplicate) => "信号已存在".to_string(),
            Err(e) => {
                r.errors.push(format!("计划 {} 提交失败: {e:#}", p.id));
                continue;
            }
        };
        if plans::settle_plan(conn, p.id, PlanStatus::Submitted, &note, now)? {
            r.submitted += 1;
        }
    }
    Ok(r)
}

/// 已证实开市的日子过了发出窗口仍未发出的计划作废;未证实 / 休市的日子不动(设计裁决 7)。
pub fn drop_unsent(conn: &Connection, cfg: &SignalCfg, now: NaiveDateTime) -> Result<usize> {
    let today = now.date();
    if now.time() < hm(cfg.emit_end_hour, cfg.emit_end_minute)
        || calendar::day_status(conn, today)? != Some(true)
    {
        return Ok(0);
    }
    let mut n = 0;
    for p in plans::due_plans(conn, today)? {
        if plans::settle_plan(conn, p.id, PlanStatus::Dropped, "开盘窗口内无有效报价,可能停牌", now)? {
            n += 1;
        }
    }
    Ok(n)
}
```

- [ ] **Step 4: 监听线程接线**

`src/trade/daemon.rs::run_loop`:循环外加 `let mut last_probe: Option<NaiveDate> = None;`;在 `catch_unwind` 闭包里、09:00 撤销任务之后插入:

```rust
            // 交易日历自证:每个工作日开盘后探一次,直到得出结论(见 trade::calendar)。
            if due_daily(now, 9, 31, last_probe) {
                match crate::trade::calendar::probe(&conn, &source, now) {
                    Ok(Some(open)) => {
                        last_probe = Some(now.date());
                        if !open {
                            println!("[trade] {} 休市(开盘后行情仍非今日)", now.date());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("[trade] 交易日历自证失败: {e:#}"),
                }
            }

            // 日线策略信号:窗口内每轮都尝试(无可发计划时只是一次本地查询)。
            match crate::trade::daily_signals::emit_due(&mut conn, &source, &cfg.signals, now) {
                Ok(r) => {
                    notify_new_tickets(&conn, notifier.as_ref(), &r.new_real_tickets);
                    for e in &r.errors {
                        eprintln!("[trade] 日线信号发出: {e}");
                    }
                    if r.submitted + r.dropped > 0 {
                        println!("[trade] 日线信号:发出 {} 条、作废 {} 条", r.submitted, r.dropped);
                    }
                }
                Err(e) => eprintln!("[trade] 日线信号发出失败: {e:#}"),
            }
            if let Err(e) = crate::trade::daily_signals::drop_unsent(&conn, &cfg.signals, now) {
                eprintln!("[trade] 作废过期信号计划失败: {e:#}");
            }
```

> `due_daily` 的时刻判定是「≥ 09:31 且当日未成功」,`Ok(None)`(周末 / 盘前)不写 `last_probe`,之后每轮会再探——但 `due_daily` 已排除周末,盘前又不会满足 ≥ 09:31,所以实际只有「探针报错」时才会逐轮重试,属期望行为。

- [ ] **Step 5: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade tests
git commit -m "feat(trade): 日线策略信号次日开盘发出,监听线程接入交易日历自证"
```

---

### Task 7: 股票推荐迁到 A 股成交口径

**Files:**
- Modify: `src/stock/recommend.rs:155-230`
- Test: `src/stock/recommend.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `stock::ashare::{AShareExecution, buy_lot}`、`StockData::with_prev_bar`
- Produces: 无新公开接口(`evaluate_stock` 签名不变)

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn a_share_candidates_use_ashare_execution_and_can_afford_a_lot() {
        // 股价 300 元:一手 3 万,旧的固定 1000 元在 A 股口径下一股都买不到
        let vals: Vec<f64> = (0..300).map(|i| 300.0 + i as f64 * 0.5).collect();
        let bars = series(&vals);
        let amount = lot_amount("600519", &bars);
        assert!(amount >= 100.0 * 450.0, "至少够买最高价时的一手,当前 {amount}");
        assert_eq!(amount % 100.0, 0.0, "取整到百元");
        assert_eq!(lot_amount("688981", &series(&[10.0; 10])), 2_200.0, "科创板一手 200 股 × 10 元 × 1.1");
        assert_eq!(lot_amount("600000", &series(&[1.0; 10])), 1_000.0, "低价股不低于原来的 1000 元");
        let r = evaluate_stock("600519", "茅台", &bars, &RecommendParams::default()).unwrap();
        let dca = r.all_strategies.iter().find(|e| e.kind == "dca").unwrap();
        assert!(dca.is_return > 0.0, "上涨行情里定投必须真的买到了股票");
    }

    #[test]
    fn non_a_share_keeps_close_execution() {
        assert!(!is_a_share_code("AAPL"));
        assert!(!is_a_share_code("00700"));
        assert!(is_a_share_code("600519"));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib stock::recommend`
Expected: 编译失败(`lot_amount` / `is_a_share_code` 未定义)

- [ ] **Step 3: 实现**

```rust
fn is_a_share_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

/// 候选策略每次买入的金额:原先固定 1000 元,A 股整手规则下高价股一手都买不起,
/// 回测会变成「从不交易」。改为至少够买区间最高价时的一手(留 10% 余量给滑点
/// 与涨停价),取整到百元,且不低于 1000 元。推荐比的是收益率与回撤,与投入规模无关。
fn lot_amount(code: &str, bars: &[StockBar]) -> f64 {
    let max_close = bars.iter().map(|b| b.close).fold(0.0, f64::max);
    let lot = crate::stock::ashare::buy_lot(code).min as f64;
    let need = (max_close * lot * 1.1 / 100.0).ceil() * 100.0;
    need.max(1000.0)
}
```

- `candidate(kind)` 改为 `candidate(kind, amount: f64)`,5 处 `1000.0` 换成 `amount`
- `run_metrics(kind, bars, fee)` 改为 `run_metrics(kind: &str, code: &str, bars: &[StockBar], prev: Option<StockBar>, fee: StockFee) -> Summary`:

```rust
fn run_metrics(kind: &str, code: &str, bars: &[StockBar], prev: Option<StockBar>, fee: StockFee) -> Summary {
    let (amount, exec): (f64, Box<dyn crate::execution::ExecutionModel>) = if is_a_share_code(code) {
        (
            lot_amount(code, bars),
            Box::new(crate::stock::ashare::AShareExecution::new(
                code,
                None,
                crate::stock::ashare::AShareExecution::DEFAULT_SLIPPAGE,
            )),
        )
    } else {
        (1000.0, Box::new(crate::execution::CloseExecution))
    };
    backtest::run_one(
        kind.to_string(),
        code.to_string(),
        crate::stock::data::StockData::with_prev_bar(bars.to_vec(), prev),
        candidate(kind, amount),
        fee,
        0.0,
        exec,
    )
    .summary
}
```

- `evaluate_stock` 中:`run_metrics(kind, code, train, None, p.fee)` 与 `run_metrics(kind, code, test, train.last().copied(), p.fee)`(检验段首日需要前收来算涨跌停)
- 删除 `run_metrics` 上方那段「暂保持收盘成交口径」注释

- [ ] **Step 4: 运行确认通过 + 全量门禁**

既有推荐测试只断言结构与排序关系;若 `profile_does_not_affect_ranking_score` 等因分数变化而失败,检查是否只是具体数值变化——该用例比较的是两次运行彼此相等,不应受影响;若有断言写死了旧口径下的数值,改为新口径实测值并在提交说明中列出。

- [ ] **Step 5: Commit**

```bash
git add src/stock/recommend.rs
git commit -m "feat(stock): 股票推荐迁到 A 股成交口径,候选买入额至少够一手"
```

---

### Task 8: 计划 3c 遗留小修

**Files:**
- Modify: `src/trade/admission/stats.rs:305-320`(`max_streak_by_code`)、`src/trade/admission/schedule.rs:55-61`
- Test: 同文件 `mod tests`

- [ ] **Step 1: 写失败测试**

`stats.rs`(用该文件既有的 `TradeUnit` 构造方式;下为语义):

```rust
    #[test]
    fn fills_without_realized_pnl_do_not_extend_a_losing_streak() {
        // 卖出序列:亏、无盈亏(qmt 导入)、亏 —— 连亏应是 2,且中间的 NULL 不打断也不计入
        // 构造 3 个同代码的卖出 TradeUnit,realized_pnl 依次为 Some(-1.0)、None、Some(-2.0)
        // 断言 max_streak_by_code(&units) == 2
    }
```

> 按注释写出完整用例(`TradeUnit` 字段以该文件定义为准)。

`schedule.rs`:

```rust
    #[test]
    fn monthly_rerun_skips_mover_strategies() {
        // 建一个 kind=mover 的观察期策略与一个 kind=trend 的观察期策略(trend 的造法同本文件
        // 既有用例:submit_for_backtest 后 apply_backtest_verdict passed),
        // enqueue_monthly 后断言 walk_forward == 1(只有 trend)
    }
```

> 按注释写出完整用例,沿用本文件既有的 `strategy_at` 等辅助函数。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission::stats trade::admission::schedule`
Expected: 两个新用例 FAIL

- [ ] **Step 3: 实现**

`max_streak_by_code`:

```rust
    for u in units.iter().filter(|u| u.side == Direction::Sell) {
        // 无盈亏的成交(如未来 qmt 导入)与 sell_pnls / trade_returns 一样跳过,
        // 不能按亏损计——否则会静默拉长连亏、误触 watchdog。
        let Some(pnl) = u.realized_pnl else { continue };
```

(删去原来的 `let pnl = u.realized_pnl.unwrap_or(0.0);`)

`enqueue_monthly` 的状态映射改为同时看类型。`enqueue_by_status` 的 `pick` 参数改为接收 `&StrategyDef`:

```rust
fn enqueue_by_status(
    conn: &Connection,
    now: NaiveDateTime,
    pick: impl Fn(&StrategyDef) -> Option<EvalKind>,
) -> Result<Enqueued> {
```

循环里 `let Some(kind) = pick(&s) else { continue };`;`enqueue_daily` 改为 `|s| match s.status { … }`;`enqueue_monthly`:

```rust
/// 每月:对还在用的策略重跑前推回测(计划 3c 的 `apply_monthly_verdict` 负责裁决)。
/// 异动类没有历史分时、不可回测(spec §10.2),跳过——否则每月都会失败一次并留下噪音。
pub fn enqueue_monthly(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |s| {
        (s.kind != "mover" && matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted))
            .then_some(EvalKind::WalkForward)
    })
}
```

(`use crate::trade::model::StrategyDef;`)

> `thread.rs` 的 `tick_persists_daily_and_monthly_markers_for_seed_tick_state_to_use_after_restart` 用 mover 策略断言「monthly 命中,WalkForward 入队」——该断言依赖的正是本任务修掉的行为。把该用例的策略换成非 mover 的观察期策略(造法同上),断言保持不变。

- [ ] **Step 4: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade
git commit -m "fix(trade): 连亏统计跳过无盈亏成交,月度重跑跳过异动策略"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动(推荐模块若有数值断言变化,已在提交说明中列出)
- [ ] 新增单元测试与 `tests/trade_runtime.rs::daily_strategy_signal_flows_from_close_to_paper_fill` 通过;测试不访问网络、不起真线程
- [ ] CI 门禁符合 Global Constraints

## 计划 3c 遗留项对照

| 遗留项 | 处理 |
|---|---|
| `data_from`/`data_to` 写成评估当天 | Task 3:用 K 线真实区间 |
| `apply_monthly_verdict` 无条件写 oos | Task 3:只对观察期 / 已准入落库 |
| `max_streak_by_code` 把 NULL 盈亏算作亏损 | Task 8 |
| `workdays_between` 数日历工作日 | Task 1:交易日历 |
| `on_code` 恒返 true(取消是死代码) | 计划 4:网页「取消评估」接入后一并处理 |
| 成绩单四栏与缺行、执行损耗均值 / 中位数定案 | 计划 4 |
| `judge_watchdog` 只记首条原因 | 计划 4(成绩单展示时一并改为列出全部触发项) |
| watchdog 未按 `version_hash` 过滤成交 | 计划 4 |

## 后续计划(不在本计划范围)

- **计划 4**:网页策略管理与成绩单展示、`/trade` 确认页、持仓校准、风控设置,以及上表列为「计划 4」的遗留项
- 日线信号的推送文案目前复用 `render_new_ticket`;日报与专门文案留给 spec 任务 10

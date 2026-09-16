# 量化交易 · 计划 3d:评估线程与调度 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让计划 3c 的 `run_job` 真正跑起来:一个 `trade-eval` 后台线程领取并执行评估任务,加上每日 / 每月的自动入队,以及 `[trade.eval]` 配置与 `main.rs` 接线。

**Architecture:** 沿用 `trade-monitor` 的线程骨架(开库重试、心跳、`catch_unwind`、退避),但评估是慢任务(一次前推回测可能跑几分钟),所以它独占一个线程、串行执行、每轮只领一个任务;调度只负责**入队**,执行一律经 `store::claim_next_job` → `worker::run_job` → `store::finish_job`,重启后由 `reclaim_stale_jobs` 兜底。

**Tech Stack:** Rust 2021、rusqlite 0.31、chrono。无新依赖、不引入 tokio。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §10.5、§11、§14(任务 8)。前置:计划 1、2a、2b、3a、3b、3c 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换只走 `admission::state`
- 阈值与开关一律来自配置并带范围校验
- 测试不得访问网络;评估线程的 K 线加载在测试中经闭包注入
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 设计裁决(执行者照此实现)

1. **串行执行,每轮一个任务**。评估重 CPU 且会长时间占住 SQLite 写锁,并行只会互相拖慢并放大锁冲突。
2. **入队与执行分离**。调度只写队列(`enqueue_eval` 自带「同策略同类型已排队/运行中则不重复入队」的去重),执行只读队列。这样重启、崩溃、手动触发三条路径共用同一套执行逻辑。
3. **评估线程独立于 `trade-monitor`**。止盈止损是 15 秒一轮的实时路径,绝不能被一次几分钟的回测挤掉。
4. **每日任务在收盘后**:`PaperCheck`(观察期是否达标)与 `Watchdog`(实盘是否失控)默认 16:30 入队,此时当日成交已回填。
5. **每月重跑**默认每月 1 日 17:00 为 `Paper` 与 `Admitted` 的策略入队 `WalkForward`,对应计划 3c 的 `apply_monthly_verdict`。
6. **K 线加载走既有的 Eastmoney 抓取**,由线程注入;测试用假闭包。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/config.rs` | 修改 | `EvalCfg` + `TradeCfg.eval` + 校验 |
| `src/trade/admission/schedule.rs` | 新建 | 每日 / 每月入队的纯逻辑 |
| `src/trade/admission/thread.rs` | 新建 | `trade-eval` 线程:领取、执行、收尾、心跳、退避 |
| `src/trade/admission/mod.rs` | 修改 | 模块声明 |
| `src/main.rs` | 修改 | Push 分支启动评估线程 |
| `config.toml` | 修改 | `[trade.eval]` 样例 |

---

### Task 1: `[trade.eval]` 配置

**Files:**
- Modify: `src/trade/config.rs`、`config.toml`
- Test: `src/trade/config.rs` 内 `mod tests`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalCfg {
    /// 是否启动评估线程
    pub enabled: bool,
    /// 空闲时轮询队列的间隔(秒)
    pub poll_secs: u64,
    /// 每日入队 PaperCheck / Watchdog 的时刻
    pub daily_hour: u32,
    pub daily_minute: u32,
    /// 每月重跑 WalkForward 的日、时
    pub monthly_day: u32,
    pub monthly_hour: u32,
}
// 默认:enabled = true, poll_secs = 30, daily 16:30, monthly 1 日 17 时
// TradeCfg 增加 `pub eval: EvalCfg`,Default 用 EvalCfg::default()
```

- [ ] **Step 1: 写失败测试**

`src/trade/config.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn eval_cfg_defaults_and_validation() {
        let c = from_toml_str("[trade]\n").unwrap().eval;
        assert!(c.enabled);
        assert_eq!(c.poll_secs, 30);
        assert_eq!((c.daily_hour, c.daily_minute), (16, 30));
        assert_eq!((c.monthly_day, c.monthly_hour), (1, 17));

        let c = from_toml_str("[trade.eval]\npoll_secs = 60\ndaily_hour = 15\n")
            .unwrap()
            .eval;
        assert_eq!((c.poll_secs, c.daily_hour), (60, 15));

        assert!(from_toml_str("[trade.eval]\npoll_secs = 0\n").is_err(), "轮询须 ≥ 5 秒");
        assert!(from_toml_str("[trade.eval]\ndaily_hour = 24\n").is_err());
        assert!(from_toml_str("[trade.eval]\ndaily_minute = 60\n").is_err());
        assert!(from_toml_str("[trade.eval]\nmonthly_day = 0\n").is_err());
        assert!(from_toml_str("[trade.eval]\nmonthly_day = 29\n").is_err(), "避开 2 月无此日");
        assert!(from_toml_str("[trade.eval]\nmonthly_hour = 24\n").is_err());
        assert!(from_toml_str("[trade.eval]\nunknown = 1\n").is_err(), "拒绝未知键");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::config`
Expected: 编译失败(`TradeCfg` 无 `eval` 字段)

- [ ] **Step 3: 实现**

在 `AdmissionCfg` 之后加 `EvalCfg` 定义与 `impl Default`,`TradeCfg` 加 `pub eval: EvalCfg`(`Default` 同步),`from_toml_str` 追加校验:

```rust
    let e = &cfg.eval;
    if e.poll_secs < 5 {
        return Err(anyhow!("[trade.eval] poll_secs 须 ≥ 5,当前 {}", e.poll_secs));
    }
    if e.daily_hour > 23 || e.monthly_hour > 23 {
        return Err(anyhow!("[trade.eval] 小时须在 0..=23"));
    }
    if e.daily_minute > 59 {
        return Err(anyhow!("[trade.eval] daily_minute 须在 0..=59"));
    }
    // 28 之后的日子并非每月都有,会导致 2 月整月不重跑
    if e.monthly_day < 1 || e.monthly_day > 28 {
        return Err(anyhow!("[trade.eval] monthly_day 须在 1..=28,当前 {}", e.monthly_day));
    }
```

`config.toml` 在 `[trade]` 样例块后追加(全部注释掉,与既有样例风格一致):

```toml
# [trade.eval]
# enabled = true       # 是否启动策略评估线程
# poll_secs = 30       # 空闲时轮询任务队列的间隔(秒)
# daily_hour = 16      # 每日入队「观察期检查 / 实盘监控」的时刻
# daily_minute = 30
# monthly_day = 1      # 每月重跑前推回测的日与时
# monthly_hour = 17
```

- [ ] **Step 4: 运行确认通过 + Commit**

```bash
git add src/trade/config.rs config.toml
git commit -m "feat(trade): [trade.eval] 评估线程配置"
```

---

### Task 2: 每日 / 每月入队

**Files:**
- Create: `src/trade/admission/schedule.rs`
- Modify: `src/trade/admission/mod.rs`
- Test: `src/trade/admission/schedule.rs` 内 `mod tests`

**Interfaces:**

```rust
/// 一轮入队的结果,仅用于日志与测试。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Enqueued { pub paper: usize, pub watchdog: usize, pub walk_forward: usize, pub errors: Vec<String> }

/// 每日:为所有用户的 Paper 策略入队 PaperCheck,为 Admitted 策略入队 Watchdog。
pub fn enqueue_daily(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued>;
/// 每月:为 Paper 与 Admitted 策略入队 WalkForward 重跑。
pub fn enqueue_monthly(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued>;
/// 今天是否是每月重跑日且已到点(与 `due_daily` 同样靠 last_run 去重)。
pub fn due_monthly(now: NaiveDateTime, day: u32, hour: u32, last_run: Option<NaiveDate>) -> bool;
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/schedule.rs`,先写模块注释与测试:

```rust
//! 评估任务的自动入队。只负责写队列,执行在 `thread.rs`:
//! 入队与执行分离,重启、崩溃与手动触发三条路径才能共用同一套执行逻辑。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{EvalKind, NewStrategy, StrategyStatus};
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

    /// 建一个指定状态的策略。`mover` 提交后直接进观察期,便于造 Paper;
    /// Admitted 再从 Paper 转一次。
    fn strategy_at(c: &Connection, user_id: i64, status: StrategyStatus) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "mover".into(),
                grid_toml: String::new(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        if status == StrategyStatus::Draft {
            return id;
        }
        state::submit_for_backtest(c, user_id, id, at(16, 9, 1)).unwrap(); // → Paper
        if status == StrategyStatus::Admitted {
            state::update_status(c, user_id, id, StrategyStatus::Paper, StrategyStatus::Admitted, "准入", at(16, 9, 2)).unwrap();
        }
        id
    }

    fn queued_kinds(c: &Connection) -> Vec<(i64, EvalKind)> {
        let mut out = Vec::new();
        while let Some(j) = store::claim_next_job(c, at(16, 17, 0)).unwrap() {
            out.push((j.strategy_id, j.kind));
        }
        out
    }

    #[test]
    fn daily_enqueues_paper_check_and_watchdog_per_status() {
        let c = db();
        let paper = strategy_at(&c, 1, StrategyStatus::Paper);
        let admitted = strategy_at(&c, 2, StrategyStatus::Admitted);
        let draft = strategy_at(&c, 1, StrategyStatus::Draft);

        let r = enqueue_daily(&c, at(16, 16, 30)).unwrap();
        assert_eq!((r.paper, r.watchdog), (1, 1));
        assert!(r.errors.is_empty());
        let mut got = queued_kinds(&c);
        got.sort();
        assert_eq!(got, vec![(paper, EvalKind::PaperCheck), (admitted, EvalKind::Watchdog)]);
        assert!(!got.iter().any(|(id, _)| *id == draft), "草稿不入队");
    }

    #[test]
    fn daily_does_not_double_enqueue() {
        let c = db();
        strategy_at(&c, 1, StrategyStatus::Paper);
        assert_eq!(enqueue_daily(&c, at(16, 16, 30)).unwrap().paper, 1);
        assert_eq!(
            enqueue_daily(&c, at(16, 16, 31)).unwrap().paper,
            0,
            "已排队的不重复入队"
        );
    }

    #[test]
    fn monthly_enqueues_walk_forward_for_paper_and_admitted() {
        let c = db();
        strategy_at(&c, 1, StrategyStatus::Paper);
        strategy_at(&c, 1, StrategyStatus::Admitted);
        strategy_at(&c, 1, StrategyStatus::Draft);
        let r = enqueue_monthly(&c, at(16, 17, 0)).unwrap();
        assert_eq!(r.walk_forward, 2);
        assert!(queued_kinds(&c).iter().all(|(_, k)| *k == EvalKind::WalkForward));
    }

    #[test]
    fn due_monthly_fires_once_on_the_configured_day() {
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 9, d).unwrap();
        assert!(due_monthly(at(1, 17, 0), 1, 17, None));
        assert!(!due_monthly(at(1, 16, 59), 1, 17, None), "未到点");
        assert!(!due_monthly(at(2, 17, 0), 1, 17, None), "非重跑日");
        assert!(!due_monthly(at(1, 18, 0), 1, 17, Some(day(1))), "当日已跑");
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission::schedule`
Expected: 编译失败(模块与函数未定义)

- [ ] **Step 3: 实现**

在模块注释后、`#[cfg(test)]` 前插入:

```rust
use crate::trade::model::{EvalKind, StrategyStatus};
use crate::trade::store;
use anyhow::Result;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use rusqlite::Connection;

/// 一轮入队的结果,仅用于日志与测试。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Enqueued {
    pub paper: usize,
    pub watchdog: usize,
    pub walk_forward: usize,
    /// 单个策略入队失败不该中断整轮
    pub errors: Vec<String>,
}

/// 逐用户遍历策略,按状态映射出要入队的任务类型。
fn enqueue_by_status(
    conn: &Connection,
    now: NaiveDateTime,
    pick: impl Fn(StrategyStatus) -> Option<EvalKind>,
) -> Result<Enqueued> {
    let mut out = Enqueued::default();
    for user_id in store::users_with_strategies(conn)? {
        for s in store::list_strategies(conn, user_id)? {
            let Some(kind) = pick(s.status) else { continue };
            match store::enqueue_eval(conn, user_id, s.id, kind, now) {
                Ok(Some(_)) => match kind {
                    EvalKind::PaperCheck => out.paper += 1,
                    EvalKind::Watchdog => out.watchdog += 1,
                    EvalKind::WalkForward => out.walk_forward += 1,
                },
                // None = 同类型任务已排队或运行中,不是错误
                Ok(None) => {}
                Err(e) => out.errors.push(format!("策略 {} 入队失败: {e:#}", s.id)),
            }
        }
    }
    Ok(out)
}

/// 每日:观察期策略查是否达标,已准入策略查实盘是否失控。
pub fn enqueue_daily(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |st| match st {
        StrategyStatus::Paper => Some(EvalKind::PaperCheck),
        StrategyStatus::Admitted => Some(EvalKind::Watchdog),
        _ => None,
    })
}

/// 每月:对还在用的策略重跑前推回测(计划 3c 的 `apply_monthly_verdict` 负责裁决)。
pub fn enqueue_monthly(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |st| {
        matches!(st, StrategyStatus::Paper | StrategyStatus::Admitted).then_some(EvalKind::WalkForward)
    })
}

/// 今天是否是每月重跑日且已到点;`last_run` 为当日则已跑过。
pub fn due_monthly(now: NaiveDateTime, day: u32, hour: u32, last_run: Option<NaiveDate>) -> bool {
    now.day() == day && now.hour() >= hour && last_run != Some(now.date())
}
```

`store.rs` 追加(与 `users_with_real_positions` 同风格):

```rust
/// 有策略定义的用户 id,升序。
pub fn users_with_strategies(conn: &Connection) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT DISTINCT user_id FROM trade_strategies ORDER BY user_id")?;
    let rows = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(rows)
}
```

`mod.rs` 加 `pub mod schedule;`。注意 `due_monthly` 用到 `chrono::Timelike`,按需 import。

- [ ] **Step 4: 运行确认通过 + Commit**

```bash
git add src/trade
git commit -m "feat(trade): 评估任务的每日与每月自动入队"
```

---

### Task 3: `trade-eval` 线程与接线

**Files:**
- Create: `src/trade/admission/thread.rs`
- Modify: `src/trade/admission/mod.rs`、`src/main.rs`
- Test: `src/trade/admission/thread.rs` 内 `mod tests`

**Interfaces:**

```rust
/// 线程每轮做的事,抽出来是为了能在测试里注入时钟与 K 线、不起真线程。
pub struct EvalDeps<'a> { pub wf: &'a WalkForwardCfg, pub admission: &'a AdmissionCfg, pub eval: &'a EvalCfg }
/// 跑一轮:回收僵死任务 → 到点则入队 → 领一个任务执行。返回本轮是否做了实事。
pub fn tick<F>(conn: &mut Connection, deps: &EvalDeps, state: &mut TickState, now: NaiveDateTime, load: F) -> TickOutcome
where F: FnMut(&str) -> Result<Vec<StockBar>>;
#[derive(Debug, Default)] pub struct TickState { pub last_daily: Option<NaiveDate>, pub last_monthly: Option<NaiveDate>, pub reclaimed: bool }
#[derive(Debug, Default, PartialEq)] pub struct TickOutcome { pub reclaimed: usize, pub enqueued: Enqueued, pub ran: Option<(i64, String)>, pub errors: Vec<String> }
/// 起线程。与 `trade-monitor` 分开:评估一次可能几分钟,绝不能挤占 15 秒一轮的止盈止损。
pub fn spawn(db_path: PathBuf, cfg: TradeCfg) -> std::io::Result<std::thread::JoinHandle<()>>;
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/thread.rs`,先写模块注释与测试:

```rust
//! `trade-eval` 线程:领取并执行评估任务。
//!
//! 与 `trade-monitor` 分开跑。评估一次前推回测可能几分钟并长时间占住写锁,
//! 而止盈止损是 15 秒一轮的实时路径,两者共线程必然互相拖累。
//! 每轮只领一个任务、串行执行:并行只会放大 SQLite 的写锁冲突。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{EvalKind, NewStrategy, StrategyStatus};
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

    fn paper_strategy(c: &Connection) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: "mover".into(),
                grid_toml: String::new(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(16, 9, 1)).unwrap();
        id
    }

    fn deps() -> (WalkForwardCfg, AdmissionCfg, EvalCfg) {
        (WalkForwardCfg::default(), AdmissionCfg::default(), EvalCfg::default())
    }

    #[test]
    fn tick_enqueues_at_the_daily_hour_then_runs_one_job() {
        let mut c = db();
        let id = paper_strategy(&c);
        let (wf, adm, ev) = deps();
        let d = EvalDeps { wf: &wf, admission: &adm, eval: &ev };
        let mut st = TickState::default();

        // 未到点:不入队也无任务可跑
        let r = tick(&mut c, &d, &mut st, at(16, 10, 0), |_| Ok(Vec::new()));
        assert_eq!(r.enqueued, Enqueued::default());
        assert!(r.ran.is_none());

        // 到点:入队一个 PaperCheck,并在同一轮领走执行
        let r = tick(&mut c, &d, &mut st, at(16, 16, 30), |_| Ok(Vec::new()));
        assert_eq!(r.enqueued.paper, 1);
        assert_eq!(r.ran.map(|(sid, _)| sid), Some(id));
        assert!(r.errors.is_empty());
        assert_eq!(st.last_daily, Some(at(16, 16, 30).date()));

        // 队列空了:下一轮无事可做,且当日不重复入队
        let r = tick(&mut c, &d, &mut st, at(16, 16, 40), |_| Ok(Vec::new()));
        assert_eq!(r.enqueued.paper, 0);
        assert!(r.ran.is_none());
    }

    #[test]
    fn first_tick_reclaims_jobs_left_running_by_a_crash() {
        let mut c = db();
        let id = paper_strategy(&c);
        store::enqueue_eval(&c, 1, id, EvalKind::PaperCheck, at(16, 9, 0)).unwrap();
        store::claim_next_job(&c, at(16, 9, 1)).unwrap().unwrap(); // 变成 running 后「崩溃」
        let (wf, adm, ev) = deps();
        let d = EvalDeps { wf: &wf, admission: &adm, eval: &ev };
        let mut st = TickState::default();
        let r = tick(&mut c, &d, &mut st, at(16, 10, 0), |_| Ok(Vec::new()));
        assert_eq!(r.reclaimed, 1, "重启后僵死任务被标记失败");
        let r = tick(&mut c, &d, &mut st, at(16, 10, 1), |_| Ok(Vec::new()));
        assert_eq!(r.reclaimed, 0, "只回收一次");
    }

    #[test]
    fn a_failing_job_is_recorded_not_left_running() {
        let mut c = db();
        let id = store::create_strategy(
            &c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: "trend".into(),
                // 非法网格:run_job 会报错
                grid_toml: "short_window = \"不是数组\"".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(&c, 1, id, at(16, 9, 1)).unwrap();
        store::enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 2)).unwrap();
        let (wf, adm, ev) = deps();
        let d = EvalDeps { wf: &wf, admission: &adm, eval: &ev };
        let mut st = TickState { reclaimed: true, ..Default::default() };
        let r = tick(&mut c, &d, &mut st, at(16, 10, 0), |_| Ok(Vec::new()));
        assert!(!r.errors.is_empty(), "失败被记录");
        let j = store::get_job(&c, 1, /* job_id */ 1).unwrap().unwrap();
        assert_eq!(j.status, crate::trade::model::JobStatus::Failed);
        assert!(j.error.unwrap().contains("网格") || j.finished_at.is_some());
    }
}
```

> `store::get_job` 若尚不存在,按 `JOB_COLS` 与 `claim_next_job` 的读法在 `store.rs` 补一个 `pub fn get_job(conn, user_id, job_id) -> Result<Option<EvalJob>>`(按 user_id 隔离),并补一个它自己的单元测试。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission::thread`
Expected: 编译失败

- [ ] **Step 3: 实现 `tick`**

```rust
use crate::stock::data::StockBar;
use crate::trade::admission::schedule::{self, Enqueued};
use crate::trade::admission::walk_forward::WalkForwardCfg;
use crate::trade::admission::worker::{self, JobContext};
use crate::trade::config::{AdmissionCfg, EvalCfg, TradeCfg};
use crate::trade::store;
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use std::path::PathBuf;

pub struct EvalDeps<'a> {
    pub wf: &'a WalkForwardCfg,
    pub admission: &'a AdmissionCfg,
    pub eval: &'a EvalCfg,
}

/// 跨轮次保留的状态:当日 / 当月是否已入队,以及本进程是否已回收过僵死任务。
#[derive(Debug, Default)]
pub struct TickState {
    pub last_daily: Option<NaiveDate>,
    pub last_monthly: Option<NaiveDate>,
    pub reclaimed: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct TickOutcome {
    pub reclaimed: usize,
    pub enqueued: Enqueued,
    /// 本轮执行的任务:(策略 id, 结论)
    pub ran: Option<(i64, String)>,
    pub errors: Vec<String>,
}

pub fn tick<F>(
    conn: &mut Connection,
    deps: &EvalDeps,
    state: &mut TickState,
    now: NaiveDateTime,
    load: F,
) -> TickOutcome
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let mut out = TickOutcome::default();
    // 进程重启时,上次崩溃留下的 running 任务永远不会有人收尾,先一次性标记失败。
    if !state.reclaimed {
        match store::reclaim_stale_jobs(conn, now) {
            Ok(n) => {
                out.reclaimed = n;
                state.reclaimed = true;
            }
            Err(e) => out.errors.push(format!("回收僵死任务失败: {e:#}")),
        }
    }
    let e = deps.eval;
    if crate::trade::daemon::due_daily(now, e.daily_hour, e.daily_minute, state.last_daily) {
        match schedule::enqueue_daily(conn, now) {
            Ok(r) => {
                out.errors.extend(r.errors.iter().cloned());
                out.enqueued = r;
                state.last_daily = Some(now.date());
            }
            Err(err) => out.errors.push(format!("每日入队失败: {err:#}")),
        }
    }
    if schedule::due_monthly(now, e.monthly_day, e.monthly_hour, state.last_monthly) {
        match schedule::enqueue_monthly(conn, now) {
            Ok(r) => {
                out.errors.extend(r.errors.iter().cloned());
                out.enqueued.walk_forward += r.walk_forward;
                state.last_monthly = Some(now.date());
            }
            Err(err) => out.errors.push(format!("每月入队失败: {err:#}")),
        }
    }
    // 每轮只领一个:评估重 CPU 且占写锁,排队比并发更可预期。
    let job = match store::claim_next_job(conn, now) {
        Ok(j) => j,
        Err(err) => {
            out.errors.push(format!("领取任务失败: {err:#}"));
            return out;
        }
    };
    let Some(job) = job else { return out };
    let ctx = JobContext {
        wf: deps.wf,
        admission: deps.admission,
        now,
    };
    let (note, error) = match worker::run_job(conn, &job, &ctx, load) {
        Ok(note) => (note, None),
        Err(err) => {
            let msg = format!("{err:#}");
            out.errors.push(format!("任务 {} 执行失败: {msg}", job.id));
            (msg.clone(), Some(msg))
        }
    };
    if let Err(err) = store::finish_job(conn, job.id, error.as_deref(), now) {
        out.errors.push(format!("任务 {} 收尾失败: {err:#}", job.id));
    }
    out.ran = Some((job.strategy_id, note));
    out
}
```

- [ ] **Step 4: 实现 `spawn` 与接线**

```rust
pub fn spawn(db_path: PathBuf, cfg: TradeCfg) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("trade-eval".into())
        .spawn(move || run_loop(db_path, cfg))
}

fn run_loop(db_path: PathBuf, cfg: TradeCfg) {
    // 与 trade-monitor 同样的策略:开库失败不能让线程退出,否则评估此后彻底停摆且无人知晓。
    let mut conn = loop {
        match crate::web::auth::store::open(&db_path).and_then(|c| store::migrate(&c).map(|_| c)) {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("[trade] 评估线程打开数据库失败,60 秒后重试: {e:#}");
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
    };
    println!("策略评估线程已启动(轮询 {} 秒)", cfg.eval.poll_secs);
    let mut state = TickState::default();
    loop {
        let now = chrono::Local::now().naive_local();
        // 单轮 panic 只记日志、照常进入下一轮;线程退出等于评估静默停摆。
        let busy = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = store::beat(&conn, "trade-eval", now) {
                eprintln!("[trade] 评估心跳失败: {e:#}");
            }
            let deps = EvalDeps {
                wf: &cfg.walk_forward.to_cfg(cfg.slippage),
                admission: &cfg.admission,
                eval: &cfg.eval,
            };
            // 前推回测要尽可能长的历史(准入的数据年限关默认 3 年,训练窗还要再往前推),
            // 统一取 12 年;`load_or_fetch` 命中缓存就不会真的联网。
            let end = now.date();
            let start = end - chrono::Duration::days(365 * 12);
            let out = tick(&mut conn, &deps, &mut state, now, |code| {
                crate::stock::data::cache::load_or_fetch(
                    code,
                    std::path::Path::new(".cache/stock"),
                    start,
                    end,
                )
            });
            for e in &out.errors {
                eprintln!("[trade] {e}");
            }
            if let Some((sid, note)) = &out.ran {
                println!("[trade] 策略 {sid} 评估:{note}");
            }
            out.ran.is_some()
        }))
        .unwrap_or(false);
        // 刚跑完一个任务就立刻再领下一个,队列积压时不必干等一个轮询周期。
        if !busy {
            std::thread::sleep(std::time::Duration::from_secs(cfg.eval.poll_secs));
        }
    }
}
```

> 两处已核对(直接照用,无需再找):`WalkForwardTuning::to_cfg(&self, slippage: f64) -> WalkForwardCfg` 在 `src/trade/config.rs:88`;K 线加载用 `crate::stock::data::cache::load_or_fetch(input, cache_dir, start, end) -> Result<Vec<StockBar>>`(`src/stock/data/cache.rs:50`),缓存目录与选股页一致,取 `.cache/stock`(见 `src/web/stock.rs:19` 的 `stock_cache()` 与 `:303` 的调用)。
>
> `WalkForwardCfg` 在循环外构造一次即可(`to_cfg` 每轮重建也无妨,但 `EvalDeps` 借用它,放循环外更省事);`start`/`end` 每轮重算,跨日时自动跟进。

`src/main.rs` 的 Push 分支,在 `daemon::spawn(...)` 之后追加:

```rust
        if cfg.eval.enabled {
            if let Err(e) = crate::trade::admission::thread::spawn(db_path.clone(), cfg.clone()) {
                eprintln!("[trade] 评估线程启动失败: {e:#}");
            }
        }
```

`mod.rs` 加 `pub mod thread;`。

- [ ] **Step 5: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade src/main.rs
git commit -m "feat(trade): trade-eval 线程与主程序接线"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] `trade::admission::{schedule,thread}` 单元测试通过;测试不访问网络、不起真线程
- [ ] CI 门禁符合 Global Constraints

## 后续计划(不在本计划范围)

- **计划 3e**:日线策略信号(收盘后计算、次日 09:25 发出、`admission_for` 接入 `submit_signal`)、`stock/recommend.rs` 迁移到 A 股口径、交易日历
- **计划 4**:网页策略管理与成绩单展示、`/trade` 确认页、持仓校准、风控设置
- 计划 3c 的「执行后遗留项」(见该计划文末)一并在 3e / 4 中消化

//! `trade-eval` 线程:领取并执行评估任务。
//!
//! 与 `trade-monitor` 分开跑。评估一次前推回测可能几分钟并长时间占住写锁,
//! 而止盈止损是 15 秒一轮的实时路径,两者共线程必然互相拖累。
//! 每轮只领一个任务、串行执行:并行只会放大 SQLite 的写锁冲突。

use crate::stock::data::StockBar;
use crate::trade::admission::schedule::{self, Enqueued};
use crate::trade::admission::state;
use crate::trade::admission::walk_forward::WalkForwardCfg;
use crate::trade::admission::worker::{self, JobContext};
use crate::trade::config::{AdmissionCfg, EvalCfg, TradeCfg};
use crate::trade::model::{EvalKind, StrategyStatus};
use crate::trade::store;
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use std::path::PathBuf;

/// 心跳表复用为「每日 / 每月上次成功入队」的持久标记(见 `seed_tick_state`)。
const DAILY_MARK: &str = "trade-eval-daily";
const MONTHLY_MARK: &str = "trade-eval-monthly";

pub struct EvalDeps<'a> {
    pub wf: &'a WalkForwardCfg,
    pub admission: &'a AdmissionCfg,
    pub eval: &'a EvalCfg,
}

/// 跨轮次保留的状态:当日 / 当月是否已入队,以及本进程是否已回收过僵死任务、
/// 是否已为缺实盘参数的策略补排过前推回测。
#[derive(Debug, Default)]
pub struct TickState {
    pub last_daily: Option<NaiveDate>,
    pub last_monthly: Option<NaiveDate>,
    pub reclaimed: bool,
    pub live_params_backfilled: bool,
}

#[derive(Debug, Default, PartialEq)]
pub struct TickOutcome {
    pub reclaimed: usize,
    pub enqueued: Enqueued,
    /// 本轮为缺实盘参数的策略补排的前推回测数(见 `schedule::enqueue_missing_live_params`)
    pub backfilled: usize,
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
                out.enqueued.paper += r.paper;
                out.enqueued.watchdog += r.watchdog;
                out.enqueued.walk_forward += r.walk_forward;
                state.last_daily = Some(now.date());
                // 持久化「今天已入队」,重启后靠它复原,而不是永远从 None 起步
                // (见 `seed_tick_state`)——否则重启越过 16:30 就会当天重复入队。
                if let Err(err) = store::beat(conn, DAILY_MARK, now) {
                    out.errors.push(format!("每日入队标记写入失败: {err:#}"));
                }
            }
            Err(err) => out.errors.push(format!("每日入队失败: {err:#}")),
        }
    }
    if schedule::due_monthly(now, e.monthly_day, e.monthly_hour, state.last_monthly) {
        match schedule::enqueue_monthly(conn, now) {
            Ok(r) => {
                out.errors.extend(r.errors.iter().cloned());
                out.enqueued.paper += r.paper;
                out.enqueued.watchdog += r.watchdog;
                out.enqueued.walk_forward += r.walk_forward;
                state.last_monthly = Some(now.date());
                if let Err(err) = store::beat(conn, MONTHLY_MARK, now) {
                    out.errors.push(format!("每月入队标记写入失败: {err:#}"));
                }
            }
            Err(err) => out.errors.push(format!("每月入队失败: {err:#}")),
        }
    }
    // 缺实盘参数的日线策略每个进程补排一次前推回测(计划 3e 上线前进入观察期的策略,
    // 否则要等下次月度重跑才有日线信号)。与月度重跑一样等到每日入队时刻再动手,
    // 不在盘中跑整池回测;放在月度之后,月度当天由月度那一次覆盖、这里去重为 0。
    if !state.live_params_backfilled
        && crate::trade::daemon::due_daily(now, e.daily_hour, e.daily_minute, None)
    {
        match schedule::enqueue_missing_live_params(conn, now) {
            Ok(r) => {
                out.errors.extend(r.errors.iter().cloned());
                out.backfilled = r.walk_forward;
                state.live_params_backfilled = true;
            }
            Err(err) => out.errors.push(format!("补排前推回测失败: {err:#}")),
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
    // `run_job` 内部 panic(如回测代码遇到极端输入)只用外层 `catch_unwind` 兜底会
    // 恢复线程,但领到的任务行仍停在 running:`state.reclaimed` 本轮已经是
    // true,不会再有人回收,`enqueue_eval` 的 queued/running 去重又会让同策略
    // 同类型永远排不进队——必须在这里把 panic 当成 Err,才能照常 `finish_job`。
    // `state::*` 用 `unchecked_transaction()`,其 RAII 守卫在 unwind 时 Drop 会
    // 回滚,数据库不会残留半截写入;这里只是把「任务」这一行标记失败。
    let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        worker::run_job(&mut *conn, &job, &ctx, load)
    }));
    let (note, error) = match run_result {
        Ok(Ok(note)) => (note, None),
        Ok(Err(err)) => {
            let msg = format!("{err:#}");
            out.errors.push(format!("任务 {} 执行失败: {msg}", job.id));
            (msg.clone(), Some(msg))
        }
        Err(payload) => {
            let msg = format!("任务执行 panic: {}", panic_message(&payload));
            out.errors.push(format!("任务 {} {msg}", job.id));
            (msg.clone(), Some(msg))
        }
    };
    if let Err(err) = store::finish_job(conn, job.id, error.as_deref(), now) {
        out.errors
            .push(format!("任务 {} 收尾失败: {err:#}", job.id));
    }
    // spec §12:「eval 任务失败/数据不足 → 策略 FAILED 并写原因,不滞留
    // BACKTESTING」。首次回测的 WalkForward 任务出错(网格非法、指标未知、
    // panic……)时,`run_job` 在产出裁决前就已经 `?` 冒泡失败,策略行本身
    // 从未被 `apply_backtest_verdict` 碰过,会永远停在 Backtesting——
    // `enqueue_daily`/`enqueue_monthly` 都不会再管这个状态,没有重试也没有
    // 超时。这里补上兜底转换;`update_status` 是条件 UPDATE,策略已不在
    // Backtesting(比如月度重跑失败,或另一次调用已经处理过)时是空操作。
    if let Some(msg) = &error {
        if job.kind == EvalKind::WalkForward {
            // 兜底转换本身失败(如写锁超时)不能静默:那正是「策略永远卡在
            // Backtesting」重新出现的方式,必须留下痕迹。
            if let Err(e) = state::update_status(
                conn,
                job.user_id,
                job.strategy_id,
                StrategyStatus::Backtesting,
                StrategyStatus::Failed,
                msg,
                now,
            ) {
                out.errors
                    .push(format!("策略 {} 标记未通过失败: {e:#}", job.strategy_id));
            }
        }
    }
    out.ran = Some((job.strategy_id, note));
    out
}

/// 线程重启时用心跳表复原 `last_daily` / `last_monthly`,而不是永远从 `TickState::default()`
/// 起步:内存态清零本身没问题,但配合 `due_daily`/`due_monthly` 的「今天 / 本月还没
/// 跑过」判定,`None` 会被读成「从没跑过」——重启越过触发时刻,就会在当天/当月
/// 再入队一遍;而这时早上的任务已经是 `done`,`enqueue_eval` 的 queued/running
/// 去重完全不拦这种重复,于是同一策略当月被裁决两次、`oos` 多写一条重复记录。
fn seed_tick_state(conn: &Connection) -> TickState {
    // 读失败只会多入队一轮(退回本分支之前的行为),不值得挡住线程启动;
    // 但要留一行日志,否则「每次重启都重跑一遍」将无从诊断。
    let read = |name: &str| match store::last_beat(conn, name) {
        Ok(t) => t.map(|t| t.date()),
        Err(e) => {
            eprintln!("[trade] 读取调度标记 {name} 失败,本轮按未跑过处理: {e:#}");
            None
        }
    };
    let last_daily = read(DAILY_MARK);
    let last_monthly = read(MONTHLY_MARK);
    TickState {
        last_daily,
        last_monthly,
        reclaimed: false,
        live_params_backfilled: false,
    }
}

/// 从 panic payload 里尽量取出可读文本;取不到就给个占位,不能让收尾逻辑崩掉。
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "未知 panic".to_string()
    }
}

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
    let mut state = seed_tick_state(&conn);
    let mut signal_state = crate::trade::daily_signals::ComputeState::default();
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
            // 日线信号需要**今天收盘后**的 K 线,与前推回测的 `end`(昨天,见下)不同;
            // 缓存不含今天、或写于收盘落定之前(盘中别的路径写的,最后一根还是
            // 半截 K 线)都会联网重抓,K 线还没更新则由 `compute` 判为待重试,
            // 按 `retry_minutes` 节奏再来。
            let today = now.date();
            let sig_start = today - chrono::Duration::days(cfg.walk_forward.train_days + 30);
            let fresh_after = crate::trade::daily_signals::kline_fresh_after(today);
            if let Some(r) = crate::trade::daily_signals::run_compute(
                &conn,
                &cfg.signals,
                deps.wf,
                &mut signal_state,
                now,
                |code| {
                    crate::stock::data::cache::load_or_fetch_fresh(
                        code,
                        std::path::Path::new(".cache/stock"),
                        sig_start,
                        today,
                        fresh_after,
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
            // 前推回测要尽可能长的历史(准入的数据年限关默认 3 年,训练窗还要再往前推),
            // 统一取 12 年。`end` 必须退一天:收盘前当天没有 K 线,非交易日(周末/
            // 节假日)更是永远没有,`end = now.date()` 会让 `cache::covers` 永远
            // 判定「没覆盖到今天」,从而在每一次跑回测时都对整池重新联网抓取。
            // 退一天只是把「永远不命中」改成「多数日子能命中」:周日(end 为周六)、
            // 周一(end 为周日)与长假后的头几天仍会落空,代价是一次重抓,可接受。
            let end = now.date() - chrono::Duration::days(1);
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
            if out.backfilled > 0 {
                println!("[trade] {} 个策略缺实盘参数,已补排前推回测", out.backfilled);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{EvalKind, NewStrategy};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
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
                // create_strategy 拒绝空网格(与 kind 无关);mover 评估不读网格,
                // 这里同 schedule.rs 的 strategy_at 一样填个占位值。
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(16, 9, 1)).unwrap();
        id
    }

    /// 非 mover(如 trend)的观察期策略:提交前推回测后再走一次通过的裁决。
    /// 计划 3c 遗留项修复后,月度重跑只认非 mover 策略,需要这种策略来触发它。
    fn trend_paper_strategy(c: &Connection) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "T".into(),
                kind: "trend".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(16, 9, 1)).unwrap();
        state::apply_backtest_verdict(
            c,
            1,
            id,
            &crate::trade::admission::walk_forward::aggregate(Vec::new()),
            &crate::trade::admission::judge::Verdict {
                passed: true,
                reasons: vec![],
            },
            NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            at(16, 9, 2),
        )
        .unwrap();
        id
    }

    fn deps() -> (WalkForwardCfg, AdmissionCfg, EvalCfg) {
        (
            WalkForwardCfg::default(),
            AdmissionCfg::default(),
            EvalCfg::default(),
        )
    }

    #[test]
    fn tick_enqueues_at_the_daily_hour_then_runs_one_job() {
        let mut c = db();
        let id = paper_strategy(&c);
        let (wf, adm, ev) = deps();
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
        // 本测试只关心「每日」入队/执行这条线;月度默认在每月 1 日触发,
        // F13 的「过了触发日就该补跑」语义会让 9 月 16 日的首轮 tick 也顺带
        // 判定月度到期(`last_monthly` 还是 None),与这里要看的东西无关,
        // 所以显式标记「本月已跑」把它关掉——月度自身的行为在别处单独测。
        let mut st = TickState {
            last_monthly: Some(at(16, 10, 0).date()),
            ..Default::default()
        };

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

    /// F13(修复轮 2 finding 2):`last_daily`/`last_monthly` 必须落库,重启后靠
    /// `seed_tick_state` 复原,而不是每次进程重启都从 `None` 起步——否则任何一次
    /// 重启只要越过当天/当月的触发时刻,就会把已经跑过的日/月任务重新入队一遍。
    #[test]
    fn tick_persists_daily_and_monthly_markers_for_seed_tick_state_to_use_after_restart() {
        let mut c = db();
        trend_paper_strategy(&c); // 非 mover → Paper:daily(PaperCheck)与 monthly(WalkForward)都会命中
        let (wf, adm, ev) = deps();
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
        let mut st = TickState::default();
        // 月度默认 day=1 hour=17,daily 默认 16:30——9 月 1 日 17:00 两者同时到点
        let now = at(1, 17, 0);
        let r = tick(&mut c, &d, &mut st, now, |_| Ok(Vec::new()));
        assert_eq!(r.enqueued.paper, 1, "daily 命中,PaperCheck 入队");
        assert_eq!(r.enqueued.walk_forward, 1, "monthly 命中,WalkForward 入队");

        // 模拟重启:凭数据库心跳复原,而不是永远从 None 起步
        let mut restarted = seed_tick_state(&c);
        assert_eq!(restarted.last_daily, Some(now.date()), "daily 标记已落库");
        assert_eq!(
            restarted.last_monthly,
            Some(now.date()),
            "monthly 标记已落库"
        );

        // 用复原后的状态在同一天/同一月内再跑一轮,不应重复入队
        let r2 = tick(
            &mut c,
            &d,
            &mut restarted,
            at(1, 17, 30),
            |_| Ok(Vec::new()),
        );
        assert_eq!(r2.enqueued.paper, 0, "重启后同日不应重复入队");
        assert_eq!(r2.enqueued.walk_forward, 0, "重启后同月不应重复入队");
    }

    /// F5:上线前就在观察期的日线策略没有实盘参数,本进程第一次到每日入队时刻时
    /// 补排一次前推回测(盘中不动手,理由同 `due_monthly`),之后不再重复。
    #[test]
    fn tick_backfills_missing_live_params_once_per_process_after_the_daily_hour() {
        let mut c = db();
        let id = trend_paper_strategy(&c); // oos 评估为空:没有实盘参数
        let (wf, adm, ev) = deps();
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
        // 当日 / 当月入队都已跑过,只看补跑
        let mut st = TickState {
            last_daily: Some(at(16, 0, 0).date()),
            last_monthly: Some(at(16, 0, 0).date()),
            reclaimed: true,
            ..Default::default()
        };
        let r = tick(&mut c, &d, &mut st, at(16, 10, 0), |_| Ok(Vec::new()));
        assert_eq!(r.backfilled, 0, "盘中不补跑");
        assert!(r.ran.is_none());

        let r = tick(&mut c, &d, &mut st, at(16, 16, 30), |_| Ok(Vec::new()));
        assert_eq!(r.backfilled, 1);
        assert_eq!(r.ran.map(|(sid, _)| sid), Some(id), "同一轮领走执行");

        let r = tick(&mut c, &d, &mut st, at(16, 16, 40), |_| Ok(Vec::new()));
        assert_eq!(r.backfilled, 0, "本进程只补一次");
        assert!(r.ran.is_none());
    }

    #[test]
    fn first_tick_reclaims_jobs_left_running_by_a_crash() {
        let mut c = db();
        let id = paper_strategy(&c);
        store::enqueue_eval(&c, 1, id, EvalKind::PaperCheck, at(16, 9, 0)).unwrap();
        store::claim_next_job(&c, at(16, 9, 1)).unwrap().unwrap(); // 变成 running 后「崩溃」
        let (wf, adm, ev) = deps();
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
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
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
        let mut st = TickState {
            reclaimed: true,
            ..Default::default()
        };
        let r = tick(&mut c, &d, &mut st, at(16, 10, 0), |_| Ok(Vec::new()));
        assert!(!r.errors.is_empty(), "失败被记录");
        let j = store::get_job(&c, 1, /* job_id */ 1).unwrap().unwrap();
        assert_eq!(j.status, crate::trade::model::JobStatus::Failed);
        // 实际报错来自 `expand_grid`("...必须是数组，例如 k = [..]"),不含「网格」
        // 二字;原断言用 `|| j.finished_at.is_some()` 兜底是永真式,`finish_job`
        // 总会写 finished_at,右侧条件形同虚设,这里直接钉死真实文案。
        assert!(
            j.error.as_deref().unwrap().contains("必须是数组"),
            "{:?}",
            j.error
        );
        // spec §12:任务失败必须把策略变成 FAILED、不能滞留 BACKTESTING——
        // 否则这个策略此后永远不会再被 enqueue_daily/enqueue_monthly 碰到,
        // 谁也无法让它重新进入回测或观察期。
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(
            s.status,
            crate::trade::model::StrategyStatus::Failed,
            "任务失败不得让策略滞留 Backtesting"
        );
        assert!(
            s.status_reason
                .as_deref()
                .unwrap_or("")
                .contains("必须是数组"),
            "{:?}",
            s.status_reason
        );
    }

    /// panic 与 `Err` 走同一条收尾路径:`run_job` 内部 panic(经由 `load` 闭包注入)
    /// 只被外层线程的 `catch_unwind` 兜住会恢复线程但留下 running 的任务行——
    /// `state.reclaimed` 本轮已是 true,不会再有人回收,而 `enqueue_eval` 的
    /// queued/running 去重会让同策略同类型永远排不进队。`tick` 内必须自己把
    /// panic 当成 `Err` 处理并照常 `finish_job`。
    #[test]
    fn a_panicking_job_is_recorded_failed_not_left_running() {
        let mut c = db();
        let id = store::create_strategy(
            &c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [3, 5]\nlong_window = [10]\namount = [20000.0]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(&c, 1, id, at(16, 9, 1)).unwrap();
        store::enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 2)).unwrap();
        let (wf, adm, ev) = deps();
        let d = EvalDeps {
            wf: &wf,
            admission: &adm,
            eval: &ev,
        };
        let mut st = TickState {
            reclaimed: true,
            ..Default::default()
        };
        // load 闭包 panic,模拟回测代码遇到极端输入
        let r = tick(
            &mut c,
            &d,
            &mut st,
            at(16, 10, 0),
            |_| -> Result<Vec<StockBar>> { panic!("模拟加载 K 线时崩溃") },
        );
        assert!(
            r.errors.iter().any(|e| e.contains("panic")),
            "panic 应被记录为错误: {:?}",
            r.errors
        );
        let j = store::get_job(&c, 1, 1).unwrap().unwrap();
        assert_eq!(
            j.status,
            crate::trade::model::JobStatus::Failed,
            "panic 后任务不能停在 running"
        );
        assert!(
            j.error.as_deref().unwrap_or("").contains("panic"),
            "{:?}",
            j.error
        );
        assert!(j.finished_at.is_some());

        // 同策略同类型此后仍能重新入队,证明没有永久卡死在 running
        assert!(
            store::enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 11, 0))
                .unwrap()
                .is_some(),
            "panic 后应可重新入队"
        );
    }
}

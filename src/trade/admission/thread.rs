//! `trade-eval` 线程:领取并执行评估任务。
//!
//! 与 `trade-monitor` 分开跑。评估一次前推回测可能几分钟并长时间占住写锁,
//! 而止盈止损是 15 秒一轮的实时路径,两者共线程必然互相拖累。
//! 每轮只领一个任务、串行执行:并行只会放大 SQLite 的写锁冲突。

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
        out.errors
            .push(format!("任务 {} 收尾失败: {err:#}", job.id));
    }
    out.ran = Some((job.strategy_id, note));
    out
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
        assert!(j.error.unwrap().contains("网格") || j.finished_at.is_some());
    }
}

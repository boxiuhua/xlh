//! 评估任务的自动入队。只负责写队列,执行在 `thread.rs`:
//! 入队与执行分离,重启、崩溃与手动触发三条路径才能共用同一套执行逻辑。

use crate::trade::model::{EvalKind, StrategyStatus};
use crate::trade::store;
use anyhow::Result;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
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
        matches!(st, StrategyStatus::Paper | StrategyStatus::Admitted)
            .then_some(EvalKind::WalkForward)
    })
}

/// 今天是否是每月重跑日且已到点;`last_run` 为当日则已跑过。
pub fn due_monthly(now: NaiveDateTime, day: u32, hour: u32, last_run: Option<NaiveDate>) -> bool {
    now.day() == day && now.hour() >= hour && last_run != Some(now.date())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{EvalKind, NewStrategy, StrategyStatus};
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

    /// 建一个指定状态的策略。`mover` 提交后直接进观察期,便于造 Paper;
    /// Admitted 再从 Paper 转一次。
    fn strategy_at(c: &Connection, user_id: i64, status: StrategyStatus) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "mover".into(),
                grid_toml: "x = [1]".into(),
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
            state::update_status(
                c,
                user_id,
                id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "准入",
                at(16, 9, 2),
            )
            .unwrap();
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
        assert_eq!(
            got,
            vec![
                (paper, EvalKind::PaperCheck),
                (admitted, EvalKind::Watchdog)
            ]
        );
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
        assert!(queued_kinds(&c)
            .iter()
            .all(|(_, k)| *k == EvalKind::WalkForward));
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

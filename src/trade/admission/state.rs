//! 策略状态机(spec §10.2)。所有转换都是条件 UPDATE,并写事件表。

use crate::trade::admission::judge::Verdict;
use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::gate::Admission;
use crate::trade::model::{fmt_ts, StrategyStatus};
use crate::trade::store;
use crate::trade::ticket::Transition;
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection};

/// `update_status` 的核心逻辑,接受任意 `&Connection`(含事务句柄的 `Deref`),
/// 供调用方决定事务边界——`update_status` 自己开关事务,`apply_backtest_verdict`
/// 则把它并入自己的事务,与落库评估一起提交(见 F8)。
fn transition_status(
    conn: &Connection,
    user_id: i64,
    id: i64,
    expect: StrategyStatus,
    to: StrategyStatus,
    reason: &str,
    now: NaiveDateTime,
) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_strategies SET status = ?1, status_reason = ?2, updated_at = ?3
         WHERE id = ?4 AND user_id = ?5 AND status = ?6",
        params![
            to.as_str(),
            reason,
            fmt_ts(now),
            id,
            user_id,
            expect.as_str()
        ],
    )?;
    if n == 0 {
        return Ok(Transition::AlreadyHandled);
    }
    store::log_status_event(conn, id, user_id, expect, to, reason, now)?;
    Ok(Transition::Applied)
}

/// 条件转换:状态必须等于 `expect`,且策略属于该用户。成功则写事件。
/// UPDATE 与事件写入包在一个事务里提交(F8):要么都生效,要么都不生效,
/// 不会出现「状态已改但事件未记」的中间态。
pub fn update_status(
    conn: &Connection,
    user_id: i64,
    id: i64,
    expect: StrategyStatus,
    to: StrategyStatus,
    reason: &str,
    now: NaiveDateTime,
) -> Result<Transition> {
    let tx = conn.unchecked_transaction()?;
    let transition = transition_status(&tx, user_id, id, expect, to, reason, now)?;
    tx.commit()?;
    Ok(transition)
}

/// 提交评估:草稿 / 未通过 / 已暂停 → 回测中;异动类没有历史分时,直接进观察期。
pub fn submit_for_backtest(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: NaiveDateTime,
) -> Result<Transition> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(Transition::AlreadyHandled);
    };
    let to = if s.kind == "mover" {
        StrategyStatus::Paper
    } else {
        StrategyStatus::Backtesting
    };
    let reason = if s.kind == "mover" {
        "异动类无历史分时,直接进入观察期"
    } else {
        "提交前推回测"
    };
    for expect in [
        StrategyStatus::Draft,
        StrategyStatus::Failed,
        StrategyStatus::Suspended,
    ] {
        if s.status == expect {
            return update_status(conn, user_id, id, expect, to, reason, now);
        }
    }
    Ok(Transition::AlreadyHandled)
}

/// 落库回测结论并推进状态:通过 → 观察期,不通过 → 未通过。
/// 先转状态,只有真正生效(而非 `AlreadyHandled`)才落库评估,避免为无效转换写入脏数据。
/// 状态转换与评估落库包在一个事务里提交(F8):要么都生效,要么都不生效,
/// 不会出现「策略已进入观察期但没有对应评估记录」的中间态。
#[allow(clippy::too_many_arguments)]
pub fn apply_backtest_verdict(
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
    let (next, reason) = if verdict.passed {
        (StrategyStatus::Paper, "回测达标,进入观察期".to_string())
    } else {
        (StrategyStatus::Failed, verdict.reasons.join(";"))
    };
    let tx = conn.unchecked_transaction()?;
    let transition = transition_status(
        &tx,
        user_id,
        id,
        StrategyStatus::Backtesting,
        next,
        &reason,
        now,
    )?;
    if transition == Transition::Applied {
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
    }
    tx.commit()?;
    Ok(transition)
}

/// 策略状态 → 闸门准入。无策略(止盈止损 / 手动)为 NotRequired。
pub fn admission_for(
    conn: &Connection,
    user_id: i64,
    strategy_id: Option<i64>,
) -> Result<Admission> {
    let Some(id) = strategy_id else {
        return Ok(Admission::NotRequired);
    };
    Ok(match store::get_strategy(conn, user_id, id)? {
        Some(s) => match s.status {
            StrategyStatus::Admitted => Admission::Admitted,
            StrategyStatus::Paper => Admission::Probation,
            _ => Admission::Blocked,
        },
        None => Admission::Blocked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::walk_forward::PoolMetrics;
    use crate::trade::model::NewStrategy;
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
        c
    }

    fn strategy(c: &Connection, kind: &str) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: kind.into(),
                grid_toml: "rsi_window = [14]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap()
    }

    fn empty_metrics() -> PoolMetrics {
        crate::trade::admission::walk_forward::aggregate(Vec::new())
    }

    #[test]
    fn transitions_are_conditional_and_logged() {
        let c = db();
        let id = strategy(&c, "rsi");
        assert_eq!(
            update_status(
                &c,
                1,
                id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "x",
                at(16, 9, 1)
            )
            .unwrap(),
            Transition::AlreadyHandled,
            "当前不是观察期"
        );
        assert_eq!(
            update_status(
                &c,
                2,
                id,
                StrategyStatus::Draft,
                StrategyStatus::Backtesting,
                "x",
                at(16, 9, 1)
            )
            .unwrap(),
            Transition::AlreadyHandled,
            "他人策略"
        );
        assert_eq!(
            update_status(
                &c,
                1,
                id,
                StrategyStatus::Draft,
                StrategyStatus::Backtesting,
                "提交评估",
                at(16, 9, 2)
            )
            .unwrap(),
            Transition::Applied
        );
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(
            (got.status, got.status_reason.as_deref()),
            (StrategyStatus::Backtesting, Some("提交评估"))
        );
        assert_eq!(store::list_status_events(&c, id, 1).unwrap().len(), 1);
    }

    #[test]
    fn mover_strategies_skip_backtest_stage() {
        let c = db();
        let rsi = strategy(&c, "rsi");
        let mover = strategy(&c, "mover");
        assert_eq!(
            submit_for_backtest(&c, 1, rsi, at(16, 9, 1)).unwrap(),
            Transition::Applied
        );
        assert_eq!(
            store::get_strategy(&c, 1, rsi).unwrap().unwrap().status,
            StrategyStatus::Backtesting
        );
        assert_eq!(
            submit_for_backtest(&c, 1, mover, at(16, 9, 1)).unwrap(),
            Transition::Applied
        );
        assert_eq!(
            store::get_strategy(&c, 1, mover).unwrap().unwrap().status,
            StrategyStatus::Paper,
            "异动类无历史分时,直接进观察期"
        );
    }

    #[test]
    fn verdict_moves_to_paper_or_failed_and_saves_eval() {
        let c = db();
        let id = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, id, at(16, 9, 1)).unwrap();
        let ok = Verdict {
            passed: true,
            reasons: Vec::new(),
        };
        assert_eq!(
            apply_backtest_verdict(
                &c,
                1,
                id,
                &empty_metrics(),
                &ok,
                day(15),
                day(16),
                at(16, 9, 2)
            )
            .unwrap(),
            Transition::Applied
        );
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper
        );
        assert!(store::latest_eval(&c, id, 1, "oos").unwrap().is_some());

        let id2 = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, id2, at(16, 9, 1)).unwrap();
        let bad = Verdict {
            passed: false,
            reasons: vec!["样本外夏普 0.50 < 0.80".into()],
        };
        apply_backtest_verdict(
            &c,
            1,
            id2,
            &empty_metrics(),
            &bad,
            day(15),
            day(16),
            at(16, 9, 3),
        )
        .unwrap();
        let got = store::get_strategy(&c, 1, id2).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Failed);
        assert!(got.status_reason.unwrap().contains("夏普"));
    }

    #[test]
    fn verdict_on_wrong_status_writes_nothing() {
        let c = db();
        let id = strategy(&c, "rsi"); // 仍是 Draft,未提交评估
        let ok = Verdict {
            passed: true,
            reasons: Vec::new(),
        };
        assert_eq!(
            apply_backtest_verdict(
                &c,
                1,
                id,
                &empty_metrics(),
                &ok,
                day(15),
                day(16),
                at(16, 9, 2)
            )
            .unwrap(),
            Transition::AlreadyHandled,
            "策略不在回测中,转换不生效"
        );
        assert!(
            store::latest_eval(&c, id, 1, "oos").unwrap().is_none(),
            "转换未生效不应落库评估"
        );
    }

    #[test]
    fn admission_maps_status_to_gate_admission() {
        let c = db();
        let id = strategy(&c, "rsi");
        assert_eq!(admission_for(&c, 1, None).unwrap(), Admission::NotRequired);
        assert_eq!(
            admission_for(&c, 1, Some(id)).unwrap(),
            Admission::Blocked,
            "草稿不得交易"
        );
        update_status(
            &c,
            1,
            id,
            StrategyStatus::Draft,
            StrategyStatus::Paper,
            "观察",
            at(16, 9, 2),
        )
        .unwrap();
        assert_eq!(
            admission_for(&c, 1, Some(id)).unwrap(),
            Admission::Probation
        );
        update_status(
            &c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "准入",
            at(16, 9, 3),
        )
        .unwrap();
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Admitted);
        update_status(
            &c,
            1,
            id,
            StrategyStatus::Admitted,
            StrategyStatus::Suspended,
            "异常",
            at(16, 9, 4),
        )
        .unwrap();
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Blocked);
        assert_eq!(
            admission_for(&c, 2, Some(id)).unwrap(),
            Admission::Blocked,
            "他人策略"
        );
        assert_eq!(admission_for(&c, 1, Some(999)).unwrap(), Admission::Blocked);
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }
}

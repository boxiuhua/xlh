//! 用户动作:受保护的确认、实盘持仓校准(计划 4a)。
//! Web 层不含业务规则(design decision 1):偏离保护、行情延迟、总开关、
//! 持仓校准留痕都在这里实现并单测;handler 只调用。

use crate::trade::admission::state;
use crate::trade::model::{fmt_ts, Account, EvalKind, JobStatus, Position, TicketStatus};
use crate::trade::settings;
use crate::trade::store;
use crate::trade::ticket::{self, Transition};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::{params, Connection};

/// 行情缓存超过此秒数视为陈旧(design decision 2)。
pub const QUOTE_MAX_AGE_SECS: i64 = 60;

/// 现价相对建议价的偏离(绝对值,比例)。确认保护与工单列表展示共用同一口径。
pub fn deviation(price: f64, suggest_price: f64) -> f64 {
    (price / suggest_price - 1.0).abs()
}

/// 行情是否陈旧:时间戳距 `now` 超过 `QUOTE_MAX_AGE_SECS`(与 `store::fresh_quote` 同一口径)。
pub fn quote_is_stale(quote_ts: NaiveDateTime, now: NaiveDateTime) -> bool {
    (now - quote_ts).num_seconds() > QUOTE_MAX_AGE_SECS
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmError {
    /// 工单不存在,或不属于该用户(跨用户视为不存在)。
    NotFound,
    /// 已非待确认(过期 / 已处理 / 并发重复点击),或模拟盘无需人工确认。
    AlreadyHandled,
    /// 管理员总开关打开。
    KillSwitch,
    /// `trade_quotes` 无该代码,或行情距今超过 `QUOTE_MAX_AGE_SECS`。
    StaleQuote,
    /// 现价相对建议价的偏离超过工单的 `deviation_th`,且未 `ack_deviation`。
    Deviation { price: f64, deviation: f64 },
}

/// 受保护的工单确认(spec §8,design decision 1 + 2):规则按序——
/// 不存在/跨用户 → 总开关 → 状态/过期 → 模拟盘无需确认 → 行情陈旧 → 价格偏离 → 落库确认。
pub fn confirm_ticket(
    conn: &Connection,
    user_id: i64,
    ticket_id: i64,
    ack_deviation: bool,
    now: NaiveDateTime,
) -> Result<std::result::Result<(), ConfirmError>> {
    let Some(t) = ticket::get_ticket(conn, ticket_id)?.filter(|t| t.user_id == user_id) else {
        return Ok(Err(ConfirmError::NotFound));
    };
    if settings::kill_switch(conn)? {
        return Ok(Err(ConfirmError::KillSwitch));
    }
    if t.status != TicketStatus::Pending || t.expires_at <= now {
        return Ok(Err(ConfirmError::AlreadyHandled));
    }
    if t.account == Account::Paper {
        return Ok(Err(ConfirmError::AlreadyHandled));
    }
    let Some(q) = store::fresh_quote(conn, &t.code, now, QUOTE_MAX_AGE_SECS)? else {
        return Ok(Err(ConfirmError::StaleQuote));
    };
    let dev = deviation(q.price, t.suggest_price);
    if dev > t.deviation_th + 1e-12 && !ack_deviation {
        return Ok(Err(ConfirmError::Deviation {
            price: q.price,
            deviation: dev,
        }));
    }
    Ok(match ticket::confirm(conn, user_id, ticket_id, now)? {
        Transition::Applied => Ok(()),
        Transition::AlreadyHandled => Err(ConfirmError::AlreadyHandled),
    })
}

/// 止盈止损校验(计划 4a):价格须有限且 > 0;两者都有时止损须低于止盈;
/// `trailing_pct` 须在 (0, 0.5] 之间。
pub fn validate_exit_levels(
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
) -> Result<()> {
    for (name, v) in [("止损", stop_loss), ("止盈", take_profit)] {
        if let Some(v) = v {
            if !(v.is_finite() && v > 0.0) {
                return Err(anyhow!("{name}价格必须为正数: {v}"));
            }
        }
    }
    if let (Some(sl), Some(tp)) = (stop_loss, take_profit) {
        if sl >= tp {
            return Err(anyhow!("止损须低于止盈: stop_loss={sl}, take_profit={tp}"));
        }
    }
    if let Some(t) = trailing_pct {
        if !(t.is_finite() && t > 0.0 && t <= 0.5) {
            return Err(anyhow!("移动止损比例须在 (0, 0.5] 之间: {t}"));
        }
    }
    Ok(())
}

/// 实盘持仓校准的目标状态(design decision 5:只作用于实盘账户)。
#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    pub code: String,
    pub qty: u64,
    pub avg_cost: f64,
    pub reason: String,
}

/// 持仓校准:代码须为 6 位数字;`qty` 为 0 时删除实盘持仓,否则 `avg_cost` 须有限且 > 0;
/// `reason` 去掉首尾空白后不得为空。改前 / 改后各写一条 `trade_position_adjusts`,整体一个
/// `IMMEDIATE` 事务,失败不留痕。
pub fn calibrate_position(
    conn: &mut Connection,
    user_id: i64,
    c: &Calibration,
    now: NaiveDateTime,
) -> Result<()> {
    if c.code.len() != 6 || !c.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(anyhow!("股票代码须为 6 位数字: {}", c.code));
    }
    let reason = c.reason.trim();
    if reason.is_empty() {
        return Err(anyhow!("校准原因不能为空"));
    }
    if c.qty > 0 && !(c.avg_cost.is_finite() && c.avg_cost > 0.0) {
        return Err(anyhow!("持仓成本必须为正数: {}", c.avg_cost));
    }

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = store::get_position(&tx, user_id, Account::Real, &c.code)?;
    let before_json = before.as_ref().map(serde_json::to_value).transpose()?;

    let after: Option<Position> = if c.qty == 0 {
        None
    } else {
        let mut p = before
            .clone()
            .unwrap_or_else(|| Position::empty(user_id, Account::Real, &c.code));
        p.today_bought_qty = p.today_bought_qty.min(c.qty);
        p.qty = c.qty;
        p.avg_cost = c.avg_cost;
        Some(p)
    };
    let after_json = after.as_ref().map(serde_json::to_value).transpose()?;

    match &after {
        Some(p) => store::upsert_position(&tx, p, now)?,
        None => {
            store::delete_position(&tx, user_id, Account::Real, &c.code)?;
        }
    }
    store::record_position_adjust(
        &tx,
        user_id,
        Account::Real,
        &c.code,
        before_json,
        after_json,
        reason,
        now,
    )?;
    tx.commit()?;
    Ok(())
}

/// `submit_strategy` 的结果(计划 4a §5:策略提交与评估取消)。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitStrategyOutcome {
    /// 已入队前推回测评估任务。
    Queued { job_id: i64 },
    /// 异动类没有历史分时,直接进入观察期。
    Paper,
    /// 状态不允许提交(过期点击 / 并发重复提交)。
    AlreadyHandled,
    /// 策略不存在,或不属于该用户。
    NotFound,
}

/// 提交策略评估:草稿 / 未通过 / 已暂停 → 排队前推回测;异动类直接进观察期。
/// 跨用户或不存在一律 `NotFound`,不泄露存在性。
pub fn submit_strategy(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: NaiveDateTime,
) -> Result<SubmitStrategyOutcome> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(SubmitStrategyOutcome::NotFound);
    };
    match state::submit_for_backtest(conn, user_id, id, now)? {
        Transition::AlreadyHandled => Ok(SubmitStrategyOutcome::AlreadyHandled),
        Transition::Applied => {
            if s.kind == "mover" {
                return Ok(SubmitStrategyOutcome::Paper);
            }
            match store::enqueue_eval(conn, user_id, id, EvalKind::WalkForward, now)? {
                Some(job_id) => Ok(SubmitStrategyOutcome::Queued { job_id }),
                None => {
                    // F5 去重命中:已有同策略同类型的排队/运行中任务,取其任务号。
                    let existing = store::list_jobs(conn, user_id, i64::MAX as usize)?
                        .into_iter()
                        .find(|j| {
                            j.strategy_id == id
                                && j.kind == EvalKind::WalkForward
                                && matches!(j.status, JobStatus::Queued | JobStatus::Running)
                        })
                        .ok_or_else(|| anyhow!("入队去重命中但查不到既有任务(策略 {id})"))?;
                    Ok(SubmitStrategyOutcome::Queued {
                        job_id: existing.id,
                    })
                }
            }
        }
    }
}

/// `cancel_job` 的结果(计划 4a §5,design decision 7)。
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    /// 排队中的任务已直接结束为失败。
    Cancelled,
    /// 运行中的任务已打上取消标记,由 worker 在处理下一只股票前中止。
    Requested,
    /// 任务已结束(done/failed),不可取消。
    NotCancellable,
    /// 任务不存在,或不属于该用户。
    NotFound,
}

/// 取消评估任务(design decision 7):排队中的直接判失败,不让前推回测滞留;
/// 运行中的只打标记,真正的中止发生在 `worker.rs` 的 `on_code` 闭包里。
pub fn cancel_job(
    conn: &Connection,
    user_id: i64,
    job_id: i64,
    now: NaiveDateTime,
) -> Result<CancelOutcome> {
    let Some(job) = store::get_job(conn, user_id, job_id)? else {
        return Ok(CancelOutcome::NotFound);
    };
    match job.status {
        JobStatus::Queued => {
            let n = conn.execute(
                "UPDATE trade_eval_jobs SET status = 'failed', error = '用户取消', finished_at = ?1
                 WHERE id = ?2 AND status = 'queued'",
                params![fmt_ts(now), job_id],
            )?;
            if n == 0 {
                // 并发:在我们判断状态之后、更新之前被领走了。
                return Ok(CancelOutcome::NotCancellable);
            }
            state::fail_cancelled_backtest(conn, user_id, job.strategy_id, now)?;
            Ok(CancelOutcome::Cancelled)
        }
        JobStatus::Running => {
            let n = conn.execute(
                "UPDATE trade_eval_jobs SET cancel_requested = 1 WHERE id = ?1 AND status = 'running'",
                params![job_id],
            )?;
            if n == 0 {
                // 并发:在我们判断状态之后、结束之前任务已经跑完了。
                return Ok(CancelOutcome::NotCancellable);
            }
            Ok(CancelOutcome::Requested)
        }
        JobStatus::Done | JobStatus::Failed => Ok(CancelOutcome::NotCancellable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::{
        Account, AccountScope, NewSignal, Quote, SignalSource, TicketStatus,
    };
    use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0, 0)).unwrap();
        c
    }
    fn quote(price: f64, ts: NaiveDateTime) -> Quote {
        Quote {
            code: "600000".into(),
            price,
            limit_up: Some(11.0),
            limit_down: Some(9.0),
            ts,
        }
    }
    /// 手动买入信号 → 一张待确认实盘工单,返回工单号。
    fn pending_ticket(c: &mut Connection) -> i64 {
        let q = quote(10.0, at(10, 0, 0));
        store::upsert_quotes(c, std::slice::from_ref(&q), at(10, 0, 0)).unwrap();
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "t".into(),
            ai_note: None,
            dedup_key: "m1".into(),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        match submit_signal(
            c,
            &sig,
            &SubmitContext {
                quote: Some(&q),
                now: at(10, 0, 0),
            },
        )
        .unwrap()
        {
            SubmitOutcome::Ticketed {
                real_ticket: Some(id),
                ..
            } => id,
            other => panic!("{other:?}"),
        }
    }
    fn status(c: &Connection, id: i64) -> TicketStatus {
        crate::trade::ticket::get_ticket(c, id)
            .unwrap()
            .unwrap()
            .status
    }

    #[test]
    fn confirm_requires_fresh_quote_and_ack_for_deviation() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        // 行情 61 秒前 → 延迟
        assert_eq!(
            confirm_ticket(&c, 1, id, false, at(10, 1, 1)).unwrap(),
            Err(ConfirmError::StaleQuote)
        );
        // 新鲜但偏离 3%(阈值 1.5%)→ 需确认
        store::upsert_quotes(&c, &[quote(10.3, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        assert!(matches!(
            confirm_ticket(&c, 1, id, false, at(10, 1, 5)).unwrap(),
            Err(ConfirmError::Deviation { .. })
        ));
        assert_eq!(status(&c, id), TicketStatus::Pending);
        // 带 ack 通过
        assert_eq!(
            confirm_ticket(&c, 1, id, true, at(10, 1, 5)).unwrap(),
            Ok(())
        );
        assert_eq!(status(&c, id), TicketStatus::Confirmed);
        // 重复点击
        assert_eq!(
            confirm_ticket(&c, 1, id, true, at(10, 1, 6)).unwrap(),
            Err(ConfirmError::AlreadyHandled)
        );
    }

    #[test]
    fn confirm_is_scoped_to_user_and_blocked_by_kill_switch() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        assert_eq!(
            confirm_ticket(&c, 2, id, true, at(10, 0, 5)).unwrap(),
            Err(ConfirmError::NotFound)
        );
        crate::trade::settings::set_kill_switch(&c, true, at(10, 0, 0)).unwrap();
        assert_eq!(
            confirm_ticket(&c, 1, id, true, at(10, 0, 5)).unwrap(),
            Err(ConfirmError::KillSwitch)
        );
    }

    #[test]
    fn validate_exit_levels_rejects_bad_input() {
        assert!(validate_exit_levels(Some(9.0), Some(12.0), None).is_ok());
        assert!(validate_exit_levels(None, None, None).is_ok());
        assert!(
            validate_exit_levels(Some(0.0), None, None).is_err(),
            "止损须为正数"
        );
        assert!(
            validate_exit_levels(None, Some(f64::NAN), None).is_err(),
            "止盈须有限"
        );
        assert!(
            validate_exit_levels(Some(12.0), Some(9.0), None).is_err(),
            "止损须低于止盈"
        );
        assert!(
            validate_exit_levels(Some(10.0), Some(10.0), None).is_err(),
            "相等也不行"
        );
        assert!(validate_exit_levels(None, None, Some(0.5)).is_ok());
        assert!(
            validate_exit_levels(None, None, Some(0.0)).is_err(),
            "trailing_pct 须 > 0"
        );
        assert!(
            validate_exit_levels(None, None, Some(0.51)).is_err(),
            "trailing_pct 须 <= 0.5"
        );
    }

    #[test]
    fn calibration_logs_before_and_after_and_zero_deletes() {
        let mut c = db();
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 1000,
                avg_cost: 10.5,
                reason: "券商对账".into(),
            },
            at(15, 0, 0),
        )
        .unwrap();
        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!((p.qty, p.avg_cost), (1000, 10.5));
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 0,
                avg_cost: 0.0,
                reason: "已清仓".into(),
            },
            at(15, 1, 0),
        )
        .unwrap();
        assert!(store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .is_none());
        let log = store::list_adjusts(&c, 1, 10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().any(|a| a.before.is_none() && a.after.is_some()));
        assert!(log.iter().any(|a| a.before.is_some() && a.after.is_none()));
        assert!(
            store::list_adjusts(&c, 2, 10).unwrap().is_empty(),
            "按用户隔离"
        );
    }

    #[test]
    fn calibration_rejects_bad_input() {
        let mut c = db();
        for bad in [
            Calibration {
                code: "60000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "x".into(),
            },
            Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 0.0,
                reason: "x".into(),
            },
            Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "  ".into(),
            },
        ] {
            assert!(calibrate_position(&mut c, 1, &bad, at(15, 0, 0)).is_err());
        }
        assert!(
            store::list_adjusts(&c, 1, 10).unwrap().is_empty(),
            "失败不留痕"
        );
    }

    fn trend_strategy(c: &Connection) -> i64 {
        store::create_strategy(
            c,
            &crate::trade::model::NewStrategy {
                user_id: 1,
                name: "趋势".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]\nlong_window = [20]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 0, 0),
        )
        .unwrap()
    }

    #[test]
    fn submit_queues_walk_forward_once_and_cancel_fails_the_strategy() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } =
            submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap()
        else {
            panic!("应入队前推回测");
        };
        assert_eq!(
            submit_strategy(&c, 1, id, at(9, 2, 0)).unwrap(),
            SubmitStrategyOutcome::AlreadyHandled
        );
        assert_eq!(
            submit_strategy(&c, 2, id, at(9, 2, 0)).unwrap(),
            SubmitStrategyOutcome::NotFound
        );
        assert_eq!(
            cancel_job(&c, 2, job_id, at(9, 3, 0)).unwrap(),
            CancelOutcome::NotFound
        );
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 3, 0)).unwrap(),
            CancelOutcome::Cancelled
        );
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(
            s.status,
            crate::trade::model::StrategyStatus::Failed,
            "不滞留回测中"
        );
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 4, 0)).unwrap(),
            CancelOutcome::NotCancellable
        );
    }

    #[test]
    fn cancelling_a_running_job_only_sets_the_flag() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } =
            submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap()
        else {
            panic!()
        };
        store::claim_next_job(&c, at(9, 1, 30)).unwrap().unwrap();
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 2, 0)).unwrap(),
            CancelOutcome::Requested
        );
        assert!(store::job_cancel_requested(&c, job_id).unwrap());
    }
}

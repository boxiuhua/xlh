//! 用户动作:受保护的确认、实盘持仓校准(计划 4a)。
//! Web 层不含业务规则(design decision 1):偏离保护、行情延迟、总开关、
//! 持仓校准留痕都在这里实现并单测;handler 只调用。

use crate::trade::model::{Account, Position, TicketStatus};
use crate::trade::settings;
use crate::trade::store;
use crate::trade::ticket::{self, Transition};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;

/// 行情缓存超过此秒数视为陈旧(design decision 2)。
pub const QUOTE_MAX_AGE_SECS: i64 = 60;

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
    let dev = (q.price / t.suggest_price - 1.0).abs();
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
}

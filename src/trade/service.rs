//! 信号提交全流程:入库去重 → 收集闸门输入 → 判定 → 生成工单 → 模拟盘即时成交。

use crate::trade::gate::{self, Admission, GateDecision, GateInput, GateReject};
use crate::trade::model::{Account, NewSignal, Quote, TicketStatus};
use crate::trade::router;
use crate::trade::store;
use crate::trade::ticket::{self, NewTicket};
use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::Connection;

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

#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    /// dedup_key 已存在
    Duplicate,
    Rejected {
        signal_id: i64,
        reason: GateReject,
    },
    Ticketed {
        signal_id: i64,
        real_ticket: Option<i64>,
        paper_ticket: Option<i64>,
    },
}

pub fn submit_signal(
    conn: &mut Connection,
    sig: &NewSignal,
    ctx: &SubmitContext,
) -> Result<SubmitOutcome> {
    let now = ctx.now;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(signal_id) = ticket::insert_signal(&tx, sig, now)? else {
        return Ok(SubmitOutcome::Duplicate);
    };

    let admission = resolve_admission(&tx, sig)?;

    let rules = store::get_risk_rules(&tx, sig.user_id)?;
    let real_account = store::get_account(&tx, sig.user_id, Account::Real)?;
    let mut paper_account = store::get_account(&tx, sig.user_id, Account::Paper)?;
    if paper_account.is_none() {
        if let Some(real) = &real_account {
            store::set_capital(&tx, sig.user_id, Account::Paper, real.total_capital, now)?;
            paper_account = store::get_account(&tx, sig.user_id, Account::Paper)?;
        }
    }
    let real_position = store::get_position(&tx, sig.user_id, Account::Real, &sig.code)?;
    let paper_position = store::get_position(&tx, sig.user_id, Account::Paper, &sig.code)?;
    let has_open_ticket = ticket::has_open_ticket(&tx, sig.user_id, &sig.code, sig.side)?;
    let last_signal_at = ticket::last_signal_at(&tx, sig.user_id, &sig.code, sig.side, signal_id)?;
    let tickets_today = ticket::count_real_tickets_on(&tx, sig.user_id, now.date())?;
    let realized_pnl_today = ticket::realized_pnl_on(&tx, sig.user_id, Account::Real, now.date())?;
    let real_reserved_cash = ticket::reserved_cash(&tx, sig.user_id, Account::Real)?;
    let paper_reserved_cash = ticket::reserved_cash(&tx, sig.user_id, Account::Paper)?;

    let decision = gate::evaluate(&GateInput {
        signal: sig,
        quote: ctx.quote,
        admission,
        rules: &rules,
        real_account: real_account.as_ref(),
        paper_account: paper_account.as_ref(),
        real_position: real_position.as_ref(),
        paper_position: paper_position.as_ref(),
        real_reserved_cash,
        paper_reserved_cash,
        has_open_ticket,
        last_signal_at,
        tickets_today,
        realized_pnl_today,
        now,
    });

    let plans = match decision {
        GateDecision::Reject(reason) => {
            ticket::mark_signal(&tx, signal_id, "rejected", Some(reason.as_str()))?;
            tx.commit()?;
            return Ok(SubmitOutcome::Rejected { signal_id, reason });
        }
        GateDecision::Pass(plans) => plans,
    };

    let expires_at = ticket::default_expiry(sig.source, now);
    let mut real_ticket = None;
    let mut paper_ticket = None;
    for plan in &plans {
        let status = match plan.account {
            Account::Real => TicketStatus::Pending,
            Account::Paper => TicketStatus::Confirmed,
        };
        let id = ticket::create_ticket(
            &tx,
            &NewTicket {
                user_id: sig.user_id,
                signal_id,
                account: plan.account,
                code: sig.code.clone(),
                side: sig.side,
                suggest_price: plan.price,
                qty: plan.qty,
                expires_at,
                deviation_th: rules.deviation_th,
                status,
                urgency: 0,
                created_at: now,
            },
        )?;
        match plan.account {
            Account::Real => real_ticket = Some(id),
            Account::Paper => paper_ticket = Some(id),
        }
    }
    ticket::mark_signal(&tx, signal_id, "ticketed", None)?;
    tx.commit()?;

    if let (Some(id), Some(q)) = (paper_ticket, ctx.quote) {
        if let Some(t) = ticket::get_ticket(conn, id)? {
            if let Err(e) = router::fill_paper_ticket(conn, &t, q, rules.slippage, now) {
                eprintln!("模拟盘工单 {id} 即时成交失败,留待批量撮合重试: {e:#}");
            }
        }
    }

    Ok(SubmitOutcome::Ticketed {
        signal_id,
        real_ticket,
        paper_ticket,
    })
}

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
    fn paper_strategy(c: &Connection) -> i64 {
        let user_id = 1;
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
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "x",
            now(),
        )
        .unwrap();
        assert_eq!(
            resolve_admission(&c, &sig(SignalSource::Strategy, Some(id), "a")).unwrap(),
            Admission::Admitted
        );
        let mut other_user = sig(SignalSource::Strategy, Some(id), "a");
        other_user.user_id = 2;
        assert_eq!(
            resolve_admission(&c, &other_user).unwrap(),
            Admission::Blocked,
            "跨用户视为不存在"
        );
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
        let ctx = SubmitContext {
            quote: Some(&q),
            now: now(),
        };
        assert!(matches!(
            submit_signal(&mut c, &sig(SignalSource::Strategy, Some(id), "k1"), &ctx).unwrap(),
            SubmitOutcome::Ticketed {
                real_ticket: None,
                paper_ticket: Some(_),
                ..
            }
        ));
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Suspended,
            "x",
            now(),
        )
        .unwrap();
        let mut s2 = sig(SignalSource::Strategy, Some(id), "k2");
        s2.code = "600036".into();
        let q2 = Quote {
            code: "600036".into(),
            ..q.clone()
        };
        let ctx2 = SubmitContext {
            quote: Some(&q2),
            now: now(),
        };
        assert!(matches!(
            submit_signal(&mut c, &s2, &ctx2).unwrap(),
            SubmitOutcome::Rejected {
                reason: GateReject::NotAdmitted,
                ..
            }
        ));
    }
}

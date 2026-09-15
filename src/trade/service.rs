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
    pub admission: Admission,
    pub now: NaiveDateTime,
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
        admission: ctx.admission,
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

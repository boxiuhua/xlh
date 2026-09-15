//! 模拟盘自动成交:按报价 ± 滑点(取整到最小价位、不越涨跌停)成交;涨跌停或 T+1 时等待下一次报价。

use crate::event::Direction;
use crate::stock::ashare::{price_decimals, slipped_price};
use crate::trade::model::{Account, Quote, Ticket, TicketStatus};
use crate::trade::{store, ticket};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteOutcome {
    Filled {
        fill_id: i64,
    },
    /// 涨跌停 / 无可卖 / 报价无效:保持待成交,等下一次报价
    Waiting,
    /// 工单已非待成交状态
    NotFillable,
}

/// 模拟盘工单同样受有效期约束:过期未成交即标记 expired。
pub fn fill_paper_ticket(
    conn: &mut Connection,
    t: &Ticket,
    quote: &Quote,
    slippage: f64,
    now: NaiveDateTime,
) -> Result<RouteOutcome> {
    if t.account != Account::Paper {
        return Err(anyhow!("仅模拟盘工单可自动成交"));
    }
    let Some(current) = ticket::get_ticket(conn, t.id)? else {
        return Ok(RouteOutcome::NotFillable);
    };
    if !matches!(
        current.status,
        TicketStatus::Confirmed | TicketStatus::Partial
    ) {
        return Ok(RouteOutcome::NotFillable);
    }
    if now >= current.expires_at {
        conn.execute(
            "UPDATE trade_tickets SET status = 'expired' WHERE id = ?1 AND status IN ('confirmed', 'partial')",
            [current.id],
        )?;
        return Ok(RouteOutcome::NotFillable);
    }
    if !(quote.price.is_finite() && quote.price > 0.0) {
        return Ok(RouteOutcome::Waiting);
    }
    let decimals = price_decimals(&current.code);
    let eps = 0.5 * 10f64.powi(-decimals);
    let hit_limit = match current.side {
        Direction::Buy => quote.limit_up.is_some_and(|u| quote.price >= u - eps),
        Direction::Sell => quote.limit_down.is_some_and(|d| quote.price <= d + eps),
    };
    if hit_limit {
        return Ok(RouteOutcome::Waiting);
    }
    let price = slipped_price(
        current.side,
        quote.price,
        slippage,
        decimals,
        quote.limit_up.unwrap_or(f64::INFINITY),
        quote.limit_down.unwrap_or(0.0),
    );
    let mut qty = current.qty - current.filled_qty;
    if current.side == Direction::Sell {
        let sellable = store::get_position(conn, current.user_id, Account::Paper, &current.code)?
            .map_or(0, |p| p.sellable(now.date()));
        qty = qty.min(sellable);
    }
    if qty == 0 {
        return Ok(RouteOutcome::Waiting);
    }
    let out = ticket::record_fill(conn, current.user_id, current.id, price, qty, "paper", now)?;
    Ok(RouteOutcome::Filled {
        fill_id: out.fill_id,
    })
}

/// 用最新一批报价撮合所有待成交模拟盘工单,返回成交笔数。
pub fn fill_pending_paper(
    conn: &mut Connection,
    quotes: &HashMap<String, Quote>,
    now: NaiveDateTime,
) -> Result<usize> {
    let mut filled = 0;
    for t in ticket::list_open_paper(conn)? {
        let Some(q) = quotes.get(&t.code) else {
            continue;
        };
        let slippage = store::get_risk_rules(conn, t.user_id)?.slippage;
        if let RouteOutcome::Filled { .. } = fill_paper_ticket(conn, &t, q, slippage, now)? {
            filled += 1;
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::NewSignal;
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use crate::trade::{store, ticket};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn paper_ticket(c: &Connection, side: Direction, qty: u64, now: NaiveDateTime) -> Ticket {
        let sid = insert_signal(
            c,
            &NewSignal {
                user_id: 1,
                source: crate::trade::model::SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side,
                ref_price: 10.0,
                reason: "r".into(),
                ai_note: None,
                dedup_key: format!("{now}-{side:?}"),
                suggest_cash: None,
                suggest_qty: None,
            },
            now,
        )
        .unwrap()
        .unwrap();
        let id = create_ticket(
            c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Paper,
                code: "600000".into(),
                side,
                suggest_price: 10.0,
                qty,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                created_at: now,
            },
        )
        .unwrap();
        ticket::get_ticket(c, id).unwrap().unwrap()
    }

    fn quote(price: f64, up: Option<f64>, down: Option<f64>, now: NaiveDateTime) -> Quote {
        Quote {
            code: "600000".into(),
            price,
            limit_up: up,
            limit_down: down,
            ts: now,
        }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Paper, 100_000.0, at(15, 9, 0)).unwrap();
        c
    }

    #[test]
    fn buy_fills_at_slipped_tick_price_and_waits_on_limit_up() {
        let mut c = db();
        let t = paper_ticket(&c, Direction::Buy, 1000, at(15, 10, 0));
        let wait = fill_paper_ticket(
            &mut c,
            &t,
            &quote(11.0, Some(11.0), None, at(15, 10, 0)),
            0.001,
            at(15, 10, 0),
        )
        .unwrap();
        assert_eq!(wait, RouteOutcome::Waiting, "涨停买不进,等待");
        let out = fill_paper_ticket(
            &mut c,
            &t,
            &quote(10.0, Some(11.0), None, at(15, 10, 1)),
            0.001,
            at(15, 10, 1),
        )
        .unwrap();
        assert!(matches!(out, RouteOutcome::Filled { .. }));
        let p = store::get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .unwrap();
        assert_eq!(p.qty, 1000);
        // 10 × 1.001 → 10.01;费 max(2.5025, 5) + 0.1001
        assert!(
            (p.avg_cost - (10_010.0 + 5.1001) / 1000.0).abs() < 1e-9,
            "avg={}",
            p.avg_cost
        );
        let again = ticket::get_ticket(&c, t.id).unwrap().unwrap();
        assert_eq!(
            fill_paper_ticket(
                &mut c,
                &again,
                &quote(10.0, None, None, at(15, 10, 2)),
                0.001,
                at(15, 10, 2)
            )
            .unwrap(),
            RouteOutcome::NotFillable
        );
    }

    #[test]
    fn sell_waits_t_plus_one_expires_stale_and_fills_fresh_ticket() {
        let mut c = db();
        let b = paper_ticket(&c, Direction::Buy, 1000, at(15, 10, 0));
        fill_paper_ticket(
            &mut c,
            &b,
            &quote(10.0, None, None, at(15, 10, 0)),
            0.0,
            at(15, 10, 0),
        )
        .unwrap();
        let s1 = paper_ticket(&c, Direction::Sell, 1000, at(15, 11, 0));
        let mut quotes = HashMap::new();
        quotes.insert(
            "600000".to_string(),
            quote(10.5, None, Some(9.0), at(15, 11, 0)),
        );
        assert_eq!(
            fill_pending_paper(&mut c, &quotes, at(15, 11, 10)).unwrap(),
            0,
            "T+1 等待"
        );
        assert_eq!(
            fill_pending_paper(&mut c, &quotes, at(16, 9, 31)).unwrap(),
            0,
            "过期工单不成交"
        );
        assert_eq!(
            ticket::get_ticket(&c, s1.id).unwrap().unwrap().status,
            TicketStatus::Expired
        );

        let s2 = paper_ticket(&c, Direction::Sell, 1000, at(16, 9, 20));
        assert_eq!(
            fill_pending_paper(&mut c, &quotes, at(16, 9, 31)).unwrap(),
            1
        );
        assert_eq!(
            ticket::get_ticket(&c, s2.id).unwrap().unwrap().status,
            TicketStatus::Filled
        );
        assert!(store::get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .is_none());
    }

    #[test]
    fn paper_buy_expires_instead_of_filling_late() {
        let mut c = db();
        let t = paper_ticket(&c, Direction::Buy, 1000, at(15, 10, 0));
        let out = fill_paper_ticket(
            &mut c,
            &t,
            &quote(10.0, Some(11.0), None, at(15, 10, 30)),
            0.001,
            at(15, 10, 30),
        )
        .unwrap();
        assert_eq!(out, RouteOutcome::NotFillable);
        assert_eq!(
            ticket::get_ticket(&c, t.id).unwrap().unwrap().status,
            TicketStatus::Expired
        );
        assert!(store::get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .is_none());
    }
}

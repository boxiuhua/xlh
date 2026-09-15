//! 信号入库去重、工单状态机、成交回填(更新持仓与资金)。

use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::fee::StockFee;
use crate::trade::model::{
    fmt_ts, parse_side, parse_ts, side_str, Account, NewSignal, Position, SignalSource, Ticket,
    TicketStatus, DATE_FMT,
};
use crate::trade::store;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Applied,
    AlreadyHandled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewTicket {
    pub user_id: i64,
    pub signal_id: i64,
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub suggest_price: f64,
    pub qty: u64,
    pub expires_at: NaiveDateTime,
    pub deviation_th: f64,
    pub status: TicketStatus,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillOutcome {
    pub fill_id: i64,
    pub status: TicketStatus,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
}

fn at_time(now: NaiveDateTime, h: u32, m: u32) -> NaiveDateTime {
    now.date()
        .and_time(NaiveTime::from_hms_opt(h, m, 0).expect("合法时刻"))
}

/// 工单默认有效期(spec §5)。
pub fn default_expiry(source: SignalSource, now: NaiveDateTime) -> NaiveDateTime {
    use chrono::Duration;
    match source {
        SignalSource::Exit => now + Duration::minutes(30),
        SignalSource::Mover => now + Duration::minutes(10),
        SignalSource::Strategy => {
            let end = at_time(now, 10, 30);
            if now < end {
                end
            } else {
                now + Duration::minutes(60)
            }
        }
        SignalSource::Manual => {
            let end = at_time(now, 15, 0);
            if now < end {
                end
            } else {
                now + Duration::minutes(30)
            }
        }
    }
}

pub fn insert_signal(conn: &Connection, s: &NewSignal, now: NaiveDateTime) -> Result<Option<i64>> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO trade_signals (user_id, source, strategy_id, code, name, side,
           ref_price, reason, ai_note, dedup_key, suggest_cash, suggest_qty, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'new', ?13)",
        params![
            s.user_id,
            s.source.as_str(),
            s.strategy_id,
            s.code,
            s.name,
            side_str(s.side),
            s.ref_price,
            s.reason,
            s.ai_note,
            s.dedup_key,
            s.suggest_cash,
            s.suggest_qty.map(|q| q as i64),
            fmt_ts(now),
        ],
    )?;
    Ok((n > 0).then(|| conn.last_insert_rowid()))
}

/// status:"ticketed" | "rejected"
pub fn mark_signal(
    conn: &Connection,
    signal_id: i64,
    status: &str,
    reason: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE trade_signals SET status = ?1, reject_reason = ?2 WHERE id = ?3",
        params![status, reason, signal_id],
    )?;
    Ok(())
}

pub fn last_signal_at(
    conn: &Connection,
    user_id: i64,
    code: &str,
    side: Direction,
    exclude_id: i64,
) -> Result<Option<NaiveDateTime>> {
    let s: Option<String> = conn.query_row(
        "SELECT MAX(created_at) FROM trade_signals
         WHERE user_id = ?1 AND code = ?2 AND side = ?3 AND status = 'ticketed' AND id <> ?4",
        params![user_id, code, side_str(side), exclude_id],
        |r| r.get(0),
    )?;
    s.as_deref().map(parse_ts).transpose()
}

pub fn has_open_ticket(
    conn: &Connection,
    user_id: i64,
    code: &str,
    side: Direction,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM trade_tickets
           WHERE user_id = ?1 AND code = ?2 AND side = ?3 AND account = 'real'
             AND status IN ('pending', 'confirmed', 'partial'))",
        params![user_id, code, side_str(side)],
        |r| r.get(0),
    )?)
}

/// 今日已生成的实盘买入工单数(每日工单上限只约束买入)。
pub fn count_real_tickets_on(conn: &Connection, user_id: i64, day: NaiveDate) -> Result<u32> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM trade_tickets
         WHERE user_id = ?1 AND account = 'real' AND side = 'buy' AND substr(created_at, 1, 10) = ?2",
        params![user_id, day.format(DATE_FMT).to_string()],
        |r| r.get(0),
    )?;
    Ok(n.max(0) as u32)
}

pub fn realized_pnl_on(
    conn: &Connection,
    user_id: i64,
    account: Account,
    day: NaiveDate,
) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(realized_pnl), 0.0) FROM trade_fills
         WHERE user_id = ?1 AND account = ?2 AND substr(filled_at, 1, 10) = ?3",
        params![user_id, account.as_str(), day.format(DATE_FMT).to_string()],
        |r| r.get(0),
    )?)
}

pub fn create_ticket(conn: &Connection, t: &NewTicket) -> Result<i64> {
    conn.execute(
        "INSERT INTO trade_tickets (user_id, signal_id, account, code, side, suggest_price, qty,
           filled_qty, expires_at, deviation_th, status, urgency, created_at, confirmed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, 0, ?11, ?12)",
        params![
            t.user_id,
            t.signal_id,
            t.account.as_str(),
            t.code,
            side_str(t.side),
            t.suggest_price,
            t.qty as i64,
            fmt_ts(t.expires_at),
            t.deviation_th,
            t.status.as_str(),
            fmt_ts(t.created_at),
            (t.status == TicketStatus::Confirmed).then(|| fmt_ts(t.created_at)),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

const TICKET_COLS: &str = "id, user_id, signal_id, account, code, side, suggest_price, qty, \
    filled_qty, expires_at, deviation_th, status, urgency, created_at, confirmed_at, ignore_reason";

struct RawTicket {
    id: i64,
    user_id: i64,
    signal_id: i64,
    account: String,
    code: String,
    side: String,
    suggest_price: f64,
    qty: i64,
    filled_qty: i64,
    expires_at: String,
    deviation_th: f64,
    status: String,
    urgency: i64,
    created_at: String,
    confirmed_at: Option<String>,
    ignore_reason: Option<String>,
}

fn read_raw_ticket(r: &Row) -> rusqlite::Result<RawTicket> {
    Ok(RawTicket {
        id: r.get(0)?,
        user_id: r.get(1)?,
        signal_id: r.get(2)?,
        account: r.get(3)?,
        code: r.get(4)?,
        side: r.get(5)?,
        suggest_price: r.get(6)?,
        qty: r.get(7)?,
        filled_qty: r.get(8)?,
        expires_at: r.get(9)?,
        deviation_th: r.get(10)?,
        status: r.get(11)?,
        urgency: r.get(12)?,
        created_at: r.get(13)?,
        confirmed_at: r.get(14)?,
        ignore_reason: r.get(15)?,
    })
}

impl RawTicket {
    fn into_ticket(self) -> Result<Ticket> {
        Ok(Ticket {
            id: self.id,
            user_id: self.user_id,
            signal_id: self.signal_id,
            account: Account::parse(&self.account)?,
            code: self.code,
            side: parse_side(&self.side)?,
            suggest_price: self.suggest_price,
            qty: self.qty.max(0) as u64,
            filled_qty: self.filled_qty.max(0) as u64,
            expires_at: parse_ts(&self.expires_at)?,
            deviation_th: self.deviation_th,
            status: TicketStatus::parse(&self.status)?,
            urgency: self.urgency,
            created_at: parse_ts(&self.created_at)?,
            confirmed_at: self.confirmed_at.as_deref().map(parse_ts).transpose()?,
            ignore_reason: self.ignore_reason,
        })
    }
}

pub fn get_ticket(conn: &Connection, id: i64) -> Result<Option<Ticket>> {
    conn.query_row(
        &format!("SELECT {TICKET_COLS} FROM trade_tickets WHERE id = ?1"),
        [id],
        read_raw_ticket,
    )
    .optional()?
    .map(RawTicket::into_ticket)
    .transpose()
}

fn query_tickets(
    conn: &Connection,
    sql_where: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Ticket>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TICKET_COLS} FROM trade_tickets WHERE {sql_where} ORDER BY id"
    ))?;
    let raws = stmt
        .query_map(args, read_raw_ticket)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(RawTicket::into_ticket).collect()
}

pub fn list_tickets(
    conn: &Connection,
    user_id: i64,
    statuses: &[TicketStatus],
) -> Result<Vec<Ticket>> {
    let all = query_tickets(conn, "user_id = ?1", params![user_id])?;
    Ok(all
        .into_iter()
        .filter(|t| statuses.is_empty() || statuses.contains(&t.status))
        .collect())
}

pub fn list_open_paper(conn: &Connection) -> Result<Vec<Ticket>> {
    query_tickets(
        conn,
        "account = 'paper' AND status IN ('confirmed', 'partial')",
        params![],
    )
}

pub fn confirm(conn: &Connection, user_id: i64, id: i64, now: NaiveDateTime) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_tickets SET status = 'confirmed', confirmed_at = ?1
         WHERE id = ?2 AND user_id = ?3 AND status = 'pending' AND expires_at > ?1",
        params![fmt_ts(now), id, user_id],
    )?;
    Ok(if n > 0 {
        Transition::Applied
    } else {
        Transition::AlreadyHandled
    })
}

pub fn ignore(conn: &Connection, user_id: i64, id: i64, reason: &str) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_tickets SET status = 'rejected', ignore_reason = ?1
         WHERE id = ?2 AND user_id = ?3 AND status = 'pending'",
        params![reason, id, user_id],
    )?;
    Ok(if n > 0 {
        Transition::Applied
    } else {
        Transition::AlreadyHandled
    })
}

pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'expired' WHERE status = 'pending' AND expires_at <= ?1",
        [fmt_ts(now)],
    )?)
}

pub fn cancel_unfilled(conn: &Connection, created_before: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'cancelled'
         WHERE account = 'real' AND status IN ('confirmed', 'partial') AND created_at < ?1",
        [fmt_ts(created_before)],
    )?)
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// 回填一笔成交:校验 → 更新持仓 / 资金 → 写成交 → 推进工单状态。单事务。
pub fn record_fill(
    conn: &mut Connection,
    user_id: i64,
    ticket_id: i64,
    price: f64,
    qty: u64,
    source: &str,
    now: NaiveDateTime,
) -> Result<FillOutcome> {
    let tx = conn.transaction()?;
    let t = get_ticket(&tx, ticket_id)?
        .filter(|t| t.user_id == user_id)
        .ok_or_else(|| anyhow!("工单不存在"))?;
    if !matches!(t.status, TicketStatus::Confirmed | TicketStatus::Partial) {
        return Err(anyhow!("工单状态为 {},不可回填成交", t.status.as_str()));
    }
    if !(price.is_finite() && price > 0.0) || qty == 0 {
        return Err(anyhow!("成交价与数量必须为正数"));
    }
    if t.filled_qty + qty > t.qty {
        return Err(anyhow!(
            "成交数量超过工单剩余数量({})",
            t.qty - t.filled_qty
        ));
    }
    let today = now.date();
    let rules = store::get_risk_rules(&tx, user_id)?;
    let fee_model = StockFee::a_share();
    let value = price * qty as f64;
    let pos = store::get_position(&tx, user_id, t.account, &t.code)?;

    let (fee, realized, new_pos, cash_delta) = match t.side {
        Direction::Buy => {
            let fee = fee_model.buy_fee(value);
            let mut p = pos.unwrap_or_else(|| Position::empty(user_id, t.account, &t.code));
            let was_empty = p.qty == 0;
            let new_qty = p.qty + qty;
            p.avg_cost = (p.avg_cost * p.qty as f64 + value + fee) / new_qty as f64;
            p.today_bought_qty = if p.last_buy_date == Some(today) {
                p.today_bought_qty + qty
            } else {
                qty
            };
            p.last_buy_date = Some(today);
            p.qty = new_qty;
            if was_empty {
                p.stop_loss = Some(round2(price * (1.0 - rules.default_stop_loss_pct)));
                p.take_profit = Some(round2(price * (1.0 + rules.default_take_profit_pct)));
                p.trailing_high = None;
            }
            (fee, None, p, -(value + fee))
        }
        Direction::Sell => {
            let mut p = pos.ok_or_else(|| anyhow!("无持仓,不可卖出"))?;
            let sellable = p.sellable(today);
            if qty > sellable {
                return Err(anyhow!("卖出数量超过可卖数量 {sellable}(T+1)"));
            }
            let fee = fee_model.sell_fee(qty as f64, price, 0);
            let realized = (price - p.avg_cost) * qty as f64 - fee;
            p.qty -= qty;
            p.today_bought_qty = p.today_bought_qty.min(p.qty);
            (fee, Some(realized), p, value - fee)
        }
    };

    store::add_cash(&tx, user_id, t.account, cash_delta, now)?;
    store::upsert_position(&tx, &new_pos, now)?;
    tx.execute(
        "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee,
           realized_pnl, source, filled_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            t.id,
            user_id,
            t.account.as_str(),
            t.code,
            side_str(t.side),
            price,
            qty as i64,
            fee,
            realized,
            source,
            fmt_ts(now),
        ],
    )?;
    let fill_id = tx.last_insert_rowid();
    let filled = t.filled_qty + qty;
    let status = if filled == t.qty {
        TicketStatus::Filled
    } else {
        TicketStatus::Partial
    };
    tx.execute(
        "UPDATE trade_tickets SET filled_qty = ?1, status = ?2 WHERE id = ?3",
        params![filled as i64, status.as_str(), t.id],
    )?;
    tx.commit()?;
    Ok(FillOutcome {
        fill_id,
        status,
        fee,
        realized_pnl: realized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        c
    }

    fn signal(key: &str, side: Direction) -> NewSignal {
        NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: Some("浦发银行".into()),
            side,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        }
    }

    fn ticket(
        c: &Connection,
        side: Direction,
        qty: u64,
        status: TicketStatus,
        now: NaiveDateTime,
    ) -> i64 {
        let sid = insert_signal(c, &signal(&format!("k-{}", now), side), now)
            .unwrap()
            .unwrap();
        create_ticket(
            c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Real,
                code: "600000".into(),
                side,
                suggest_price: 10.0,
                qty,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status,
                created_at: now,
            },
        )
        .unwrap()
    }

    #[test]
    fn expiry_by_source() {
        assert_eq!(
            default_expiry(SignalSource::Exit, at(16, 9, 35)),
            at(16, 10, 5)
        );
        assert_eq!(
            default_expiry(SignalSource::Mover, at(16, 10, 0)),
            at(16, 10, 10)
        );
        assert_eq!(
            default_expiry(SignalSource::Strategy, at(16, 9, 25)),
            at(16, 10, 30)
        );
        assert_eq!(
            default_expiry(SignalSource::Strategy, at(16, 11, 0)),
            at(16, 12, 0)
        );
        assert_eq!(
            default_expiry(SignalSource::Manual, at(16, 10, 0)),
            at(16, 15, 0)
        );
        assert_eq!(
            default_expiry(SignalSource::Manual, at(16, 15, 20)),
            at(16, 15, 50)
        );
    }

    #[test]
    fn insert_signal_dedups_and_cooldown_query() {
        let c = db();
        let s = signal("dup", Direction::Buy);
        let id = insert_signal(&c, &s, at(15, 10, 0)).unwrap().unwrap();
        assert!(insert_signal(&c, &s, at(15, 10, 1)).unwrap().is_none());
        assert!(
            last_signal_at(&c, 1, "600000", Direction::Buy, -1)
                .unwrap()
                .is_none(),
            "未生成工单不计冷却"
        );
        mark_signal(&c, id, "ticketed", None).unwrap();
        assert_eq!(
            last_signal_at(&c, 1, "600000", Direction::Buy, -1).unwrap(),
            Some(at(15, 10, 0))
        );
        assert!(
            last_signal_at(&c, 1, "600000", Direction::Buy, id)
                .unwrap()
                .is_none(),
            "排除自身"
        );
    }

    #[test]
    fn confirm_ignore_and_idempotency() {
        let c = db();
        let t = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Pending,
            at(15, 10, 0),
        );
        assert_eq!(
            confirm(&c, 2, t, at(15, 10, 1)).unwrap(),
            Transition::AlreadyHandled,
            "他人工单"
        );
        assert_eq!(
            confirm(&c, 1, t, at(15, 10, 1)).unwrap(),
            Transition::Applied
        );
        assert_eq!(
            confirm(&c, 1, t, at(15, 10, 2)).unwrap(),
            Transition::AlreadyHandled,
            "重复确认"
        );
        assert_eq!(
            get_ticket(&c, t).unwrap().unwrap().status,
            TicketStatus::Confirmed
        );
        assert!(has_open_ticket(&c, 1, "600000", Direction::Buy).unwrap());

        let t2 = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Pending,
            at(15, 11, 0),
        );
        assert_eq!(
            confirm(&c, 1, t2, at(15, 11, 31)).unwrap(),
            Transition::AlreadyHandled,
            "已过有效期"
        );
        assert_eq!(ignore(&c, 1, t2, "不看好").unwrap(), Transition::Applied);
        let got = get_ticket(&c, t2).unwrap().unwrap();
        assert_eq!(
            (got.status, got.ignore_reason.as_deref()),
            (TicketStatus::Rejected, Some("不看好"))
        );
        assert_eq!(
            count_real_tickets_on(&c, 1, at(15, 0, 0).date()).unwrap(),
            2
        );
    }

    #[test]
    fn daily_count_excludes_sell_tickets() {
        let c = db();
        ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Pending,
            at(15, 10, 0),
        );
        ticket(
            &c,
            Direction::Sell,
            100,
            TicketStatus::Pending,
            at(15, 10, 1),
        );
        assert_eq!(
            count_real_tickets_on(&c, 1, at(15, 0, 0).date()).unwrap(),
            1
        );
    }

    #[test]
    fn expire_and_cancel_batches() {
        let c = db();
        let p = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Pending,
            at(15, 10, 0),
        );
        let k = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Confirmed,
            at(15, 10, 5),
        );
        assert_eq!(expire_due(&c, at(15, 10, 20)).unwrap(), 0);
        assert_eq!(expire_due(&c, at(15, 10, 30)).unwrap(), 1);
        assert_eq!(
            get_ticket(&c, p).unwrap().unwrap().status,
            TicketStatus::Expired
        );
        assert_eq!(cancel_unfilled(&c, at(16, 0, 0)).unwrap(), 1);
        assert_eq!(
            get_ticket(&c, k).unwrap().unwrap().status,
            TicketStatus::Cancelled
        );
        assert_eq!(
            list_tickets(&c, 1, &[TicketStatus::Cancelled])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn buy_fill_creates_position_with_fee_in_cost_and_default_exits() {
        let mut c = db();
        let t = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Confirmed,
            at(15, 10, 0),
        );
        let out = record_fill(&mut c, 1, t, 10.0, 600, "manual", at(15, 10, 10)).unwrap();
        assert_eq!(out.status, TicketStatus::Partial);
        let out = record_fill(&mut c, 1, t, 10.0, 400, "manual", at(15, 10, 20)).unwrap();
        assert_eq!(out.status, TicketStatus::Filled);
        assert!(
            record_fill(&mut c, 1, t, 10.0, 1, "manual", at(15, 10, 30)).is_err(),
            "已成交不可再回填"
        );

        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!((p.qty, p.today_bought_qty), (1000, 1000));
        // 两笔:6000 费 5.06;4000 费 5.04 → 成本 (10000 + 10.1) / 1000
        assert!((p.avg_cost - 10.0101).abs() < 1e-9, "avg={}", p.avg_cost);
        assert_eq!((p.stop_loss, p.take_profit), (Some(9.2), Some(12.0)));
        let a = store::get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!((a.available_cash - (100_000.0 - 10_010.1)).abs() < 1e-6);
    }

    #[test]
    fn fill_validation_errors() {
        let mut c = db();
        let pending = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Pending,
            at(15, 10, 0),
        );
        assert!(
            record_fill(&mut c, 1, pending, 10.0, 100, "manual", at(15, 10, 1)).is_err(),
            "未确认"
        );
        let t = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Confirmed,
            at(15, 10, 2),
        );
        assert!(
            record_fill(&mut c, 1, t, 10.0, 1001, "manual", at(15, 10, 3)).is_err(),
            "超量"
        );
        assert!(
            record_fill(&mut c, 1, t, 0.0, 100, "manual", at(15, 10, 3)).is_err(),
            "价格非正"
        );
        assert!(
            record_fill(&mut c, 2, t, 10.0, 100, "manual", at(15, 10, 3)).is_err(),
            "他人工单"
        );
    }

    #[test]
    fn sell_fill_respects_t_plus_one_and_realizes_pnl() {
        let mut c = db();
        let b = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Confirmed,
            at(15, 10, 0),
        );
        record_fill(&mut c, 1, b, 10.0, 1000, "manual", at(15, 10, 10)).unwrap();

        let s1 = ticket(
            &c,
            Direction::Sell,
            1000,
            TicketStatus::Confirmed,
            at(15, 14, 0),
        );
        assert!(
            record_fill(&mut c, 1, s1, 11.0, 1000, "manual", at(15, 14, 1)).is_err(),
            "T+1"
        );

        let s2 = ticket(
            &c,
            Direction::Sell,
            1000,
            TicketStatus::Confirmed,
            at(16, 10, 0),
        );
        let out = record_fill(&mut c, 1, s2, 11.0, 1000, "manual", at(16, 10, 1)).unwrap();
        // 成本 10.0051;卖出费 5 + 5.5 + 0.11 = 10.61 → (11 − 10.0051) × 1000 − 10.61 = 984.29
        assert!(
            (out.realized_pnl.unwrap() - 984.29).abs() < 1e-6,
            "{:?}",
            out.realized_pnl
        );
        assert!(store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .is_none());
        assert!(
            (realized_pnl_on(&c, 1, Account::Real, at(16, 0, 0).date()).unwrap() - 984.29).abs()
                < 1e-6
        );
        let a = store::get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!(
            (a.available_cash - 100_984.29).abs() < 1e-6,
            "cash={}",
            a.available_cash
        );
    }
}

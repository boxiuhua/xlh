//! 信号入库去重、工单状态机、成交回填(更新持仓与资金)。

use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::ashare::price_decimals;
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
    pub urgency: i64,
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

/// 入库信号；同 dedup_key 已被拒绝的信号会被重新激活(可重试)，其余重复返回 None。
pub fn insert_signal(conn: &Connection, s: &NewSignal, now: NaiveDateTime) -> Result<Option<i64>> {
    conn.query_row(
        "INSERT INTO trade_signals (user_id, source, strategy_id, code, name, side, scope, ref_price, reason,
           ai_note, dedup_key, suggest_cash, suggest_qty, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'new', ?14)
         ON CONFLICT(user_id, dedup_key) DO UPDATE SET
           source = excluded.source, strategy_id = excluded.strategy_id, code = excluded.code,
           name = excluded.name, side = excluded.side, scope = excluded.scope,
           ref_price = excluded.ref_price, reason = excluded.reason, ai_note = excluded.ai_note,
           suggest_cash = excluded.suggest_cash, suggest_qty = excluded.suggest_qty,
           status = 'new', reject_reason = NULL, created_at = excluded.created_at
         WHERE trade_signals.status = 'rejected'
         RETURNING id",
        params![
            s.user_id,
            s.source.as_str(),
            s.strategy_id,
            s.code,
            s.name,
            side_str(s.side),
            s.scope.as_str(),
            s.ref_price,
            s.reason,
            s.ai_note,
            s.dedup_key,
            s.suggest_cash,
            s.suggest_qty.map(|q| q as i64),
            fmt_ts(now),
        ],
        |r| r.get(0),
    )
    .optional()
    .map_err(Into::into)
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

/// 未完结买入工单占用的资金:Σ 建议价 × 未成交数量。
pub fn reserved_cash(conn: &Connection, user_id: i64, account: Account) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(suggest_price * (qty - filled_qty)), 0.0) FROM trade_tickets
         WHERE user_id = ?1 AND account = ?2 AND side = 'buy'
           AND status IN ('pending', 'confirmed', 'partial')",
        params![user_id, account.as_str()],
        |r| r.get(0),
    )?)
}

pub fn create_ticket(conn: &Connection, t: &NewTicket) -> Result<i64> {
    conn.execute(
        "INSERT INTO trade_tickets (user_id, signal_id, account, code, side, suggest_price, qty,
           filled_qty, expires_at, deviation_th, status, urgency, created_at, confirmed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13)",
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
            t.urgency,
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

/// 「已完成」视图:终态工单(`filled/expired/rejected/cancelled`),实盘与模拟盘都含,
/// 按 id 倒序(新到旧)且 SQL 内直接限量,不再全表读进内存(4a 遗留)。
pub fn list_done_tickets(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<Ticket>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TICKET_COLS} FROM trade_tickets
         WHERE user_id = ?1 AND status IN ('filled', 'expired', 'rejected', 'cancelled')
         ORDER BY id DESC LIMIT ?2"
    ))?;
    let raws = stmt
        .query_map(params![user_id, limit as i64], read_raw_ticket)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(RawTicket::into_ticket).collect()
}

pub fn list_open_paper(conn: &Connection) -> Result<Vec<Ticket>> {
    query_tickets(
        conn,
        "account = 'paper' AND status IN ('confirmed', 'partial')",
        params![],
    )
}

/// 全体用户已确认但未完全回填的实盘工单(15:05 提醒用)。
pub fn list_unfilled_real(conn: &Connection) -> Result<Vec<Ticket>> {
    query_tickets(
        conn,
        "account = 'real' AND status IN ('confirmed', 'partial')",
        params![],
    )
}

/// 止盈止损工单过期而条件仍成立:基于同一信号新建实盘工单,urgency + 1。
/// 仅当该信号最近一张实盘工单为 expired 时重发(用户忽略、已成交、仍待确认均不重发)。
pub fn reissue_expired_exit(
    conn: &Connection,
    user_id: i64,
    dedup_key: &str,
    qty: u64,
    price: f64,
    now: NaiveDateTime,
) -> Result<Option<i64>> {
    if qty == 0 {
        return Ok(None);
    }
    let signal_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM trade_signals
             WHERE user_id = ?1 AND dedup_key = ?2 AND source = 'exit' AND status = 'ticketed'",
            params![user_id, dedup_key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(signal_id) = signal_id else {
        return Ok(None);
    };
    let last_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM trade_tickets WHERE signal_id = ?1 AND account = 'real'
             ORDER BY id DESC LIMIT 1",
            [signal_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(last) = last_id
        .map(|id| get_ticket(conn, id))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    if last.status != TicketStatus::Expired {
        return Ok(None);
    }
    let id = create_ticket(
        conn,
        &NewTicket {
            user_id,
            signal_id,
            account: Account::Real,
            code: last.code.clone(),
            side: last.side,
            suggest_price: price,
            qty,
            expires_at: default_expiry(SignalSource::Exit, now),
            deviation_th: last.deviation_th,
            status: TicketStatus::Pending,
            urgency: last.urgency + 1,
            created_at: now,
        },
    )?;
    Ok(Some(id))
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

/// 过期:实盘待确认、模拟盘待成交(模拟盘同样受有效期约束)。
pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'expired'
         WHERE expires_at <= ?1
           AND ((account = 'real' AND status = 'pending')
             OR (account = 'paper' AND status IN ('confirmed', 'partial')))",
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

pub(crate) fn round_dec(x: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (x * m).round() / m
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
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let out = record_fill_in(&tx, user_id, ticket_id, price, qty, source, now)?;
    tx.commit()?;
    Ok(out)
}

/// `record_fill` 的事务内部分:调用方持有事务并负责提交(出错时丢弃事务即回滚)。
/// 供 `actions::manual_fill` 在同一把写锁内先做业务预校验再落库。
/// 这里的校验是兜底——人工回填的业务拒绝应在调用方预校验时已给出。
pub(crate) fn record_fill_in(
    tx: &Connection,
    user_id: i64,
    ticket_id: i64,
    price: f64,
    qty: u64,
    source: &str,
    now: NaiveDateTime,
) -> Result<FillOutcome> {
    let t = get_ticket(tx, ticket_id)?
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
    let rules = store::get_risk_rules(tx, user_id)?;
    let fee_model = StockFee::a_share();
    let value = price * qty as f64;
    let pos = store::get_position(tx, user_id, t.account, &t.code)?;

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
                let decimals = price_decimals(&t.code);
                p.stop_loss = Some(round_dec(
                    price * (1.0 - rules.default_stop_loss_pct),
                    decimals,
                ));
                p.take_profit = Some(round_dec(
                    price * (1.0 + rules.default_take_profit_pct),
                    decimals,
                ));
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

    store::add_cash(tx, user_id, t.account, cash_delta, now)?;
    store::upsert_position(tx, &new_pos, now)?;
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
    use crate::trade::model::AccountScope;
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
            scope: AccountScope::Both,
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
                urgency: 0,
                created_at: now,
            },
        )
        .unwrap()
    }

    fn new_ticket(
        signal_id: i64,
        account: Account,
        code: &str,
        side: Direction,
        qty: u64,
        status: TicketStatus,
        now: NaiveDateTime,
    ) -> NewTicket {
        NewTicket {
            user_id: 1,
            signal_id,
            account,
            code: code.into(),
            side,
            suggest_price: 10.0,
            qty,
            expires_at: now + chrono::Duration::minutes(30),
            deviation_th: 0.015,
            status,
            urgency: 0,
            created_at: now,
        }
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
    fn rejected_signal_can_be_resubmitted_ticketed_cannot() {
        let c = db();
        let s = signal("re-key", Direction::Sell);
        let id = insert_signal(&c, &s, at(15, 10, 0)).unwrap().unwrap();
        mark_signal(&c, id, "rejected", Some("limit_down")).unwrap();

        let again = insert_signal(&c, &s, at(15, 10, 5)).unwrap();
        assert_eq!(again, Some(id), "被拒的信号可用同 key 重新激活同一行");
        let (status, reject_reason): (String, Option<String>) = c
            .query_row(
                "SELECT status, reject_reason FROM trade_signals WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status.as_str(), reject_reason.as_deref()), ("new", None));

        mark_signal(&c, id, "ticketed", None).unwrap();
        assert!(
            insert_signal(&c, &s, at(15, 10, 10)).unwrap().is_none(),
            "已生成工单的信号仍返回 None"
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

    /// 「已完成」只含终态工单(filled/expired/rejected/cancelled),按 id 倒序,受 limit 约束,
    /// 且按用户隔离(4a 遗留:不再全表读进内存 + 内存截断)。
    #[test]
    fn done_list_is_terminal_only_newest_first_and_limited() {
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
            Direction::Buy,
            100,
            TicketStatus::Confirmed,
            at(15, 10, 1),
        );
        let filled = ticket(&c, Direction::Buy, 100, TicketStatus::Filled, at(15, 10, 2));
        let expired = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Expired,
            at(15, 10, 3),
        );
        let cancelled = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Cancelled,
            at(15, 10, 4),
        );

        let done = list_done_tickets(&c, 1, 10).unwrap();
        assert_eq!(
            done.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![cancelled, expired, filled],
            "只含终态,按 id 倒序(新到旧)"
        );

        let limited = list_done_tickets(&c, 1, 2).unwrap();
        assert_eq!(
            limited.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![cancelled, expired],
            "limit 生效"
        );

        assert!(
            list_done_tickets(&c, 2, 10).unwrap().is_empty(),
            "按用户隔离"
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

    #[test]
    fn expire_due_covers_open_paper_tickets_but_not_confirmed_real() {
        let c = db();
        let sid = insert_signal(&c, &signal("exp-paper", Direction::Buy), at(15, 10, 0))
            .unwrap()
            .unwrap();
        let paper = create_ticket(
            &c,
            &new_ticket(
                sid,
                Account::Paper,
                "600000",
                Direction::Buy,
                100,
                TicketStatus::Confirmed,
                at(15, 10, 0),
            ),
        )
        .unwrap();
        let real = ticket(
            &c,
            Direction::Buy,
            100,
            TicketStatus::Confirmed,
            at(15, 10, 1),
        );
        assert_eq!(expire_due(&c, at(15, 10, 40)).unwrap(), 1);
        assert_eq!(
            get_ticket(&c, paper).unwrap().unwrap().status,
            TicketStatus::Expired
        );
        assert_eq!(
            get_ticket(&c, real).unwrap().unwrap().status,
            TicketStatus::Confirmed
        );
        let unfilled = list_unfilled_real(&c).unwrap();
        assert_eq!(
            unfilled.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![real]
        );
    }

    #[test]
    fn reserved_cash_counts_unfilled_part_of_open_buy_tickets() {
        let mut c = db();
        let b = ticket(
            &c,
            Direction::Buy,
            1000,
            TicketStatus::Pending,
            at(15, 10, 0),
        );
        ticket(
            &c,
            Direction::Sell,
            500,
            TicketStatus::Pending,
            at(15, 10, 1),
        );
        assert!((reserved_cash(&c, 1, Account::Real).unwrap() - 10_000.0).abs() < 1e-9);
        assert_eq!(
            confirm(&c, 1, b, at(15, 10, 2)).unwrap(),
            Transition::Applied
        );
        record_fill(&mut c, 1, b, 10.0, 400, "manual", at(15, 10, 3)).unwrap();
        assert!((reserved_cash(&c, 1, Account::Real).unwrap() - 6_000.0).abs() < 1e-9);
        assert!(reserved_cash(&c, 1, Account::Paper).unwrap().abs() < 1e-12);
        assert!(reserved_cash(&c, 2, Account::Real).unwrap().abs() < 1e-12);
    }

    #[test]
    fn reissue_expired_exit_bumps_urgency_once_per_expiry() {
        let c = db();
        let key = "exit-real-600000-stop-2026-09-15";
        let mut s = signal(key, Direction::Sell);
        s.source = SignalSource::Exit;
        let sid = insert_signal(&c, &s, at(15, 10, 0)).unwrap().unwrap();
        mark_signal(&c, sid, "ticketed", None).unwrap();
        let first = create_ticket(
            &c,
            &new_ticket(
                sid,
                Account::Real,
                "600000",
                Direction::Sell,
                1000,
                TicketStatus::Pending,
                at(15, 10, 0),
            ),
        )
        .unwrap();
        assert!(
            reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 20))
                .unwrap()
                .is_none(),
            "未过期不重发"
        );
        assert_eq!(expire_due(&c, at(15, 10, 30)).unwrap(), 1);
        let second = reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 31))
            .unwrap()
            .unwrap();
        assert_ne!(second, first);
        let t = get_ticket(&c, second).unwrap().unwrap();
        assert_eq!(
            (t.signal_id, t.urgency, t.qty, t.status),
            (sid, 1, 1000, TicketStatus::Pending)
        );
        assert_eq!(t.expires_at, at(15, 11, 1));
        assert!((t.suggest_price - 9.1).abs() < 1e-9);
        assert!(
            reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 32))
                .unwrap()
                .is_none(),
            "已有待确认工单"
        );
        assert!(
            reissue_expired_exit(&c, 2, key, 1000, 9.1, at(15, 11, 40))
                .unwrap()
                .is_none(),
            "他人信号"
        );

        assert_eq!(ignore(&c, 1, second, "不卖").unwrap(), Transition::Applied);
        assert!(
            reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 11, 40))
                .unwrap()
                .is_none(),
            "用户忽略不重发"
        );
    }

    #[test]
    fn default_exit_levels_round_to_price_tick() {
        let mut c = db();
        let sid = insert_signal(&c, &signal("etf-buy", Direction::Buy), at(15, 10, 0))
            .unwrap()
            .unwrap();
        let mut nt = new_ticket(
            sid,
            Account::Real,
            "510300",
            Direction::Buy,
            1000,
            TicketStatus::Confirmed,
            at(15, 10, 0),
        );
        nt.suggest_price = 3.456;
        let id = create_ticket(&c, &nt).unwrap();
        record_fill(&mut c, 1, id, 3.456, 1000, "manual", at(15, 10, 1)).unwrap();
        let p = store::get_position(&c, 1, Account::Real, "510300")
            .unwrap()
            .unwrap();
        // 3.456 × 0.92 = 3.17952 → 3.180;3.456 × 1.2 = 4.1472 → 4.147
        assert_eq!((p.stop_loss, p.take_profit), (Some(3.18), Some(4.147)));
    }
}

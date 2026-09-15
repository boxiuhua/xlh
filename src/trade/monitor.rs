//! 交易监听单轮:过期 → 报价 → 缓存 → 止盈止损 → 过期重发 → 模拟盘撮合。
//! 报价源经参数注入,整轮可离线测试;线程、休眠与退避在 `daemon`。

use crate::stock::realtime::calendar::is_weekend;
use crate::trade::exits::{exit_signal, next_trailing_high};
use crate::trade::gate::Admission;
use crate::trade::model::{Account, Position, Quote};
use crate::trade::quotes::QuoteSource;
use crate::trade::router::{self, PaperBatch};
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use crate::trade::{store, ticket};
use anyhow::Result;
use chrono::{NaiveDateTime, Timelike};
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    OutOfSession,
    NothingWatched,
    StaleQuotes,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickReport {
    pub skipped: Option<Skip>,
    pub expired: usize,
    pub quotes: usize,
    pub exit_signals: usize,
    /// 本轮新建的实盘工单(含过期重发),供推送
    pub new_real_tickets: Vec<i64>,
    pub paper: PaperBatch,
    pub errors: Vec<String>,
}

/// 工作日 09:30–11:30、13:00–15:00(含端点)。
pub fn is_session(now: NaiveDateTime) -> bool {
    if is_weekend(now.date()) {
        return false;
    }
    let m = now.hour() * 60 + now.minute();
    (570..=690).contains(&m) || (780..=900).contains(&m)
}

/// 需要报价的代码:全体持仓 ∪ 未完结工单。
pub fn watched_codes(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT code FROM trade_positions
         UNION
         SELECT code FROM trade_tickets WHERE status IN ('pending', 'confirmed', 'partial')
         ORDER BY code",
    )?;
    let codes = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(codes)
}

pub fn run_tick(
    conn: &mut Connection,
    source: &dyn QuoteSource,
    now: NaiveDateTime,
) -> Result<TickReport> {
    let mut report = TickReport {
        expired: ticket::expire_due(conn, now)?,
        ..TickReport::default()
    };
    if !is_session(now) {
        report.skipped = Some(Skip::OutOfSession);
        return Ok(report);
    }
    let codes = watched_codes(conn)?;
    if codes.is_empty() {
        report.skipped = Some(Skip::NothingWatched);
        return Ok(report);
    }
    let fresh: Vec<Quote> = source
        .fetch(&codes)?
        .into_iter()
        .filter(|q| q.ts.date() == now.date())
        .collect();
    if fresh.is_empty() {
        report.skipped = Some(Skip::StaleQuotes);
        return Ok(report);
    }
    store::upsert_quotes(conn, &fresh, now)?;
    report.quotes = fresh.len();
    let quotes: HashMap<String, Quote> = fresh.into_iter().map(|q| (q.code.clone(), q)).collect();

    for p in store::list_all_positions(conn)? {
        let Some(q) = quotes.get(&p.code) else {
            continue;
        };
        let label = format!("用户 {} {} {}", p.user_id, p.account.as_str(), p.code);
        if let Err(e) = process_position(conn, p, q, now, &mut report) {
            report.errors.push(format!("{label}: {e:#}"));
        }
    }
    report.paper = router::fill_pending_paper(conn, &quotes, now)?;
    Ok(report)
}

fn process_position(
    conn: &mut Connection,
    mut p: Position,
    q: &Quote,
    now: NaiveDateTime,
    report: &mut TickReport,
) -> Result<()> {
    if let Some(high) = next_trailing_high(&p, q.price) {
        store::set_trailing_high(conn, p.user_id, p.account, &p.code, high, now)?;
        p.trailing_high = Some(high);
    }
    let Some(sig) = exit_signal(&p, q, now) else {
        return Ok(());
    };
    report.exit_signals += 1;
    let ctx = SubmitContext {
        quote: Some(q),
        admission: Admission::NotRequired,
        now,
    };
    match submit_signal(conn, &sig, &ctx)? {
        SubmitOutcome::Ticketed {
            real_ticket: Some(id),
            ..
        } => report.new_real_tickets.push(id),
        SubmitOutcome::Duplicate if p.account == Account::Real => {
            if let Some(id) = ticket::reissue_expired_exit(
                conn,
                p.user_id,
                &sig.dedup_key,
                p.sellable(now.date()),
                q.price,
                now,
            )? {
                report.new_real_tickets.push(id);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Account, Position, TicketStatus};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    struct Stub(std::result::Result<Vec<Quote>, String>);

    impl QuoteSource for Stub {
        fn fetch(&self, _codes: &[String]) -> Result<Vec<Quote>> {
            self.0.clone().map_err(|e| anyhow::anyhow!(e))
        }
    }

    fn quote(price: f64, ts: NaiveDateTime) -> Stub {
        Stub(Ok(vec![Quote {
            code: "600000".into(),
            price,
            limit_up: None,
            limit_down: Some(8.19),
            ts,
        }]))
    }

    fn db_with_position(configure: impl FnOnce(&mut Position)) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        configure(&mut p);
        store::upsert_position(&c, &p, at(15, 15, 0)).unwrap();
        c
    }

    #[test]
    fn session_windows() {
        assert!(is_session(at(16, 9, 30)));
        assert!(is_session(at(16, 11, 30)));
        assert!(!is_session(at(16, 12, 0)));
        assert!(is_session(at(16, 13, 0)));
        assert!(is_session(at(16, 15, 0)));
        assert!(!is_session(at(16, 15, 1)));
        assert!(!is_session(at(19, 10, 0)), "2026-09-19 是周六");
    }

    #[test]
    fn stop_loss_creates_ticket_then_reissues_after_expiry() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r1 = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!((r1.skipped, r1.quotes, r1.exit_signals), (None, 1, 1));
        assert_eq!(r1.new_real_tickets.len(), 1);
        let t1 = ticket::get_ticket(&c, r1.new_real_tickets[0])
            .unwrap()
            .unwrap();
        assert_eq!(
            (t1.qty, t1.urgency, t1.status),
            (1000, 0, TicketStatus::Pending)
        );
        assert!(
            store::get_quote(&c, "600000").unwrap().is_some(),
            "报价已缓存"
        );

        let r2 = run_tick(&mut c, &quote(9.0, at(16, 10, 15)), at(16, 10, 15)).unwrap();
        assert!(r2.new_real_tickets.is_empty(), "已有待确认工单,不重复");

        let r3 = run_tick(&mut c, &quote(9.0, at(16, 10, 31)), at(16, 10, 31)).unwrap();
        assert_eq!(r3.expired, 1);
        assert_eq!(r3.new_real_tickets.len(), 1);
        let t2 = ticket::get_ticket(&c, r3.new_real_tickets[0])
            .unwrap()
            .unwrap();
        assert_eq!((t2.signal_id, t2.urgency), (t1.signal_id, 1));
        assert!(r3.errors.is_empty(), "{:?}", r3.errors);
    }

    #[test]
    fn trailing_high_is_recorded_then_triggers() {
        let mut c = db_with_position(|p| p.trailing_pct = Some(0.05));
        let r1 = run_tick(&mut c, &quote(12.0, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r1.exit_signals, 0);
        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!(p.trailing_high, Some(12.0));
        let r2 = run_tick(&mut c, &quote(11.3, at(16, 10, 1)), at(16, 10, 1)).unwrap();
        assert_eq!((r2.exit_signals, r2.new_real_tickets.len()), (1, 1));
    }

    #[test]
    fn skips_and_errors() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r = run_tick(&mut c, &quote(9.1, at(16, 12, 0)), at(16, 12, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::OutOfSession));
        let r = run_tick(&mut c, &quote(9.1, at(15, 15, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::StaleQuotes), "昨日报价");
        assert!(run_tick(&mut c, &Stub(Err("网络错误".into())), at(16, 10, 0)).is_err());

        let mut empty = Connection::open_in_memory().unwrap();
        store::migrate(&empty).unwrap();
        let r = run_tick(&mut empty, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::NothingWatched));
        assert_eq!(watched_codes(&c).unwrap(), vec!["600000".to_string()]);
    }
}

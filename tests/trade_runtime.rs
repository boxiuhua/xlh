use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use std::cell::RefCell;
use xlh::trade::daemon::{alert_holders, notify_new_tickets, send_fill_reminders};
use xlh::trade::gate::Admission;
use xlh::trade::model::{Account, AccountScope, NewSignal, Position, Quote, SignalSource};
use xlh::trade::monitor::run_tick;
use xlh::trade::notify::Notifier;
use xlh::trade::quotes::QuoteSource;
use xlh::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use xlh::trade::{store, ticket};

fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, d)
        .unwrap()
        .and_hms_opt(h, m, 0)
        .unwrap()
}

#[derive(Default)]
struct Recorder(RefCell<Vec<(i64, String, String)>>);

impl Notifier for Recorder {
    fn notify(
        &self,
        _conn: &Connection,
        user_id: i64,
        title: &str,
        md: &str,
    ) -> anyhow::Result<()> {
        self.0
            .borrow_mut()
            .push((user_id, title.to_string(), md.to_string()));
        Ok(())
    }
}

struct Fixed(Vec<Quote>);

impl QuoteSource for Fixed {
    fn fetch(&self, _codes: &[String]) -> anyhow::Result<Vec<Quote>> {
        Ok(self.0.clone())
    }
}

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    store::migrate(&c).unwrap();
    for uid in [1, 2] {
        store::set_capital(&c, uid, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
    }
    c
}

fn hold(c: &Connection, uid: i64) {
    let mut p = Position::empty(uid, Account::Real, "600000");
    p.qty = 1000;
    p.avg_cost = 10.0;
    p.stop_loss = Some(9.2);
    p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
    store::upsert_position(c, &p, at(15, 15, 0)).unwrap();
}

#[test]
fn stop_loss_tick_notifies_owner_with_reason() {
    let mut c = db();
    hold(&c, 1);
    let quotes = Fixed(vec![Quote {
        code: "600000".into(),
        price: 9.1,
        limit_up: None,
        limit_down: Some(8.19),
        ts: at(16, 10, 0),
    }]);
    let report = run_tick(&mut c, &quotes, at(16, 10, 0)).unwrap();
    let rec = Recorder::default();
    assert_eq!(notify_new_tickets(&c, &rec, &report.new_real_tickets), 1);
    let sent = rec.0.borrow();
    assert_eq!(sent[0].0, 1);
    assert_eq!(sent[0].1, "交易工单:卖出 600000");
    assert!(sent[0].2.contains("触发止损"), "{}", sent[0].2);
}

#[test]
fn fill_reminders_are_grouped_per_user() {
    let mut c = db();
    let q = Quote {
        code: "600000".into(),
        price: 10.0,
        limit_up: Some(11.0),
        limit_down: Some(9.0),
        ts: at(16, 10, 0),
    };
    for uid in [1, 2] {
        let sig = NewSignal {
            user_id: uid,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side: xlh::event::Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "手动".into(),
            ai_note: None,
            dedup_key: format!("manual-{uid}"),
            suggest_cash: None,
            suggest_qty: None,
        };
        let ctx = SubmitContext {
            quote: Some(&q),
            admission: Admission::NotRequired,
            now: at(16, 10, 0),
        };
        let SubmitOutcome::Ticketed {
            real_ticket: Some(id),
            ..
        } = submit_signal(&mut c, &sig, &ctx).unwrap()
        else {
            panic!("应生成实盘工单");
        };
        ticket::confirm(&c, uid, id, at(16, 10, 1)).unwrap();
    }
    let rec = Recorder::default();
    assert_eq!(send_fill_reminders(&c, &rec).unwrap(), 2);
    let sent = rec.0.borrow();
    assert_eq!(sent.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2]);
    assert!(sent.iter().all(|s| s.1 == "待回填成交提醒"));
}

#[test]
fn monitor_down_alert_goes_to_position_holders_only() {
    let c = db();
    hold(&c, 2);
    let rec = Recorder::default();
    assert_eq!(alert_holders(&c, &rec, 3).unwrap(), 1);
    assert_eq!(rec.0.borrow()[0].0, 2);
}

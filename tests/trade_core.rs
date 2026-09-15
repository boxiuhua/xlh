use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use xlh::event::Direction;
use xlh::trade::gate::{Admission, GateReject};
use xlh::trade::model::{
    Account, AccountScope, NewSignal, Position, Quote, SignalSource, TicketStatus,
};
use xlh::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use xlh::trade::{store, ticket};

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

fn signal(source: SignalSource, side: Direction, key: &str) -> NewSignal {
    NewSignal {
        user_id: 1,
        source,
        strategy_id: None,
        code: "600000".into(),
        name: Some("浦发银行".into()),
        side,
        scope: AccountScope::Both,
        ref_price: 10.0,
        reason: "集成测试".into(),
        ai_note: None,
        dedup_key: key.into(),
        suggest_cash: None,
        suggest_qty: None,
    }
}

fn quote(price: f64, up: Option<f64>, down: Option<f64>, ts: NaiveDateTime) -> Quote {
    Quote {
        code: "600000".into(),
        price,
        limit_up: up,
        limit_down: down,
        ts,
    }
}

#[test]
fn manual_buy_confirm_fill_then_exit_sell_next_day() {
    let mut c = db();
    let q = quote(10.0, Some(11.0), Some(9.0), at(15, 10, 0));
    let buy = signal(SignalSource::Manual, Direction::Buy, "manual-1");
    let ctx = SubmitContext {
        quote: Some(&q),
        admission: Admission::NotRequired,
        now: at(15, 10, 0),
    };

    let SubmitOutcome::Ticketed {
        real_ticket: Some(rt),
        paper_ticket: Some(pt),
        ..
    } = submit_signal(&mut c, &buy, &ctx).unwrap()
    else {
        panic!("应生成实盘与模拟盘工单");
    };
    assert_eq!(
        submit_signal(&mut c, &buy, &ctx).unwrap(),
        SubmitOutcome::Duplicate
    );

    let real = ticket::get_ticket(&c, rt).unwrap().unwrap();
    assert_eq!((real.status, real.qty), (TicketStatus::Pending, 1900));
    assert_eq!(real.expires_at, at(15, 15, 0));
    let paper = ticket::get_ticket(&c, pt).unwrap().unwrap();
    assert_eq!(paper.status, TicketStatus::Filled, "模拟盘按报价立即成交");
    let paper_pos = store::get_position(&c, 1, Account::Paper, "600000")
        .unwrap()
        .unwrap();
    assert_eq!(paper_pos.qty, 1900);

    assert_eq!(
        ticket::confirm(&c, 1, rt, at(15, 10, 5)).unwrap(),
        ticket::Transition::Applied
    );
    let out = ticket::record_fill(&mut c, 1, rt, 10.0, 1900, "manual", at(15, 10, 20)).unwrap();
    assert_eq!(out.status, TicketStatus::Filled);
    let pos = store::get_position(&c, 1, Account::Real, "600000")
        .unwrap()
        .unwrap();
    assert_eq!(
        (pos.qty, pos.stop_loss, pos.take_profit),
        (1900, Some(9.2), Some(12.0))
    );
    let cash = store::get_account(&c, 1, Account::Real)
        .unwrap()
        .unwrap()
        .available_cash;
    assert!(
        (cash - (100_000.0 - 19_000.0 - 5.19)).abs() < 1e-6,
        "cash={cash}"
    );

    // 当日触发止损:T+1 不可卖
    let q2 = quote(9.1, Some(11.0), Some(9.0), at(15, 14, 0));
    let exit_today = signal(
        SignalSource::Exit,
        Direction::Sell,
        "exit-600000-stop-2026-09-15",
    );
    let ctx2 = SubmitContext {
        quote: Some(&q2),
        admission: Admission::NotRequired,
        now: at(15, 14, 0),
    };
    assert!(matches!(
        submit_signal(&mut c, &exit_today, &ctx2).unwrap(),
        SubmitOutcome::Rejected {
            reason: GateReject::NothingSellable,
            ..
        }
    ));

    // 次日:生成实盘卖出工单,模拟盘直接卖出
    let q3 = quote(9.1, Some(10.01), Some(9.0), at(16, 9, 35));
    let exit_next = signal(
        SignalSource::Exit,
        Direction::Sell,
        "exit-600000-stop-2026-09-16",
    );
    let ctx3 = SubmitContext {
        quote: Some(&q3),
        admission: Admission::NotRequired,
        now: at(16, 9, 35),
    };
    let SubmitOutcome::Ticketed {
        real_ticket: Some(sell_rt),
        paper_ticket: Some(_),
        ..
    } = submit_signal(&mut c, &exit_next, &ctx3).unwrap()
    else {
        panic!("次日应生成卖出工单");
    };
    assert_eq!(ticket::get_ticket(&c, sell_rt).unwrap().unwrap().qty, 1900);
    assert!(
        store::get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .is_none(),
        "模拟盘已卖出"
    );

    assert_eq!(ticket::expire_due(&c, at(16, 10, 30)).unwrap(), 1);
    assert_eq!(
        ticket::get_ticket(&c, sell_rt).unwrap().unwrap().status,
        TicketStatus::Expired
    );
}

#[test]
fn stop_loss_rejected_at_limit_down_refires_same_day() {
    let mut c = db();
    let mut pos = Position::empty(1, Account::Real, "600000");
    pos.qty = 1000;
    pos.avg_cost = 10.0;
    pos.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 14).unwrap());
    store::upsert_position(&c, &pos, at(14, 15, 0)).unwrap();

    let mut sig = signal(
        SignalSource::Exit,
        Direction::Sell,
        "exit-real-600000-stop-2026-09-16",
    );
    sig.scope = AccountScope::RealOnly;

    let q1 = quote(9.0, Some(11.0), Some(9.0), at(16, 9, 35));
    let ctx1 = SubmitContext {
        quote: Some(&q1),
        admission: Admission::NotRequired,
        now: at(16, 9, 35),
    };
    let SubmitOutcome::Rejected {
        signal_id: first,
        reason: GateReject::LimitDown,
    } = submit_signal(&mut c, &sig, &ctx1).unwrap()
    else {
        panic!("跌停应被拒绝");
    };

    let q2 = quote(9.2, Some(11.0), Some(9.0), at(16, 10, 5));
    let ctx2 = SubmitContext {
        quote: Some(&q2),
        admission: Admission::NotRequired,
        now: at(16, 10, 5),
    };
    let SubmitOutcome::Ticketed {
        signal_id,
        real_ticket: Some(_),
        paper_ticket: None,
    } = submit_signal(&mut c, &sig, &ctx2).unwrap()
    else {
        panic!("解除跌停后应重新激活同一信号并生成实盘工单");
    };
    assert_eq!(signal_id, first, "重新激活的应是同一行");

    assert_eq!(
        submit_signal(&mut c, &sig, &ctx2).unwrap(),
        SubmitOutcome::Duplicate,
        "已生成工单后重复提交应视为重复"
    );
}

#[test]
fn strategy_admission_controls_accounts() {
    let mut c = db();
    let q = quote(10.0, Some(11.0), Some(9.0), at(15, 10, 0));

    let probation = signal(SignalSource::Strategy, Direction::Buy, "strategy-probation");
    let ctx = SubmitContext {
        quote: Some(&q),
        admission: Admission::Probation,
        now: at(15, 10, 0),
    };
    assert!(matches!(
        submit_signal(&mut c, &probation, &ctx).unwrap(),
        SubmitOutcome::Ticketed {
            real_ticket: None,
            paper_ticket: Some(_),
            ..
        }
    ));

    let mut other = signal(SignalSource::Strategy, Direction::Buy, "strategy-blocked");
    other.code = "600036".into();
    let q_other = Quote {
        code: "600036".into(),
        ..q.clone()
    };
    let ctx_blocked = SubmitContext {
        quote: Some(&q_other),
        admission: Admission::Blocked,
        now: at(15, 10, 0),
    };
    let SubmitOutcome::Rejected { signal_id, reason } =
        submit_signal(&mut c, &other, &ctx_blocked).unwrap()
    else {
        panic!("未准入应拒绝");
    };
    assert_eq!(reason, GateReject::NotAdmitted);
    let (status, why): (String, Option<String>) = c
        .query_row(
            "SELECT status, reject_reason FROM trade_signals WHERE id = ?1",
            [signal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (status.as_str(), why.as_deref()),
        ("rejected", Some("not_admitted"))
    );
}

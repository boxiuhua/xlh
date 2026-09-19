//! 交易监听单轮:过期 → 报价 → 缓存 → 止盈止损 → 过期重发 → 模拟盘撮合。
//! 报价源经参数注入,整轮可离线测试;线程、休眠与退避在 `daemon`。

use crate::event::Direction;
use crate::stock::realtime::calendar::is_weekend;
use crate::trade::exits::{exit_signal, next_trailing_high};
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
    match router::fill_pending_paper(conn, &quotes, now) {
        Ok(batch) => report.paper = batch,
        Err(e) => report.errors.push(format!("模拟盘批量撮合失败: {e:#}")),
    }
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
        now,
    };
    match submit_signal(conn, &sig, &ctx)? {
        SubmitOutcome::Ticketed {
            real_ticket: Some(id),
            ..
        } => report.new_real_tickets.push(id),
        SubmitOutcome::Duplicate if p.account == Account::Real => {
            // 过期重发同样受风控总开关、管理员总开关与跌停约束:关闭交易时不该再挂新单;
            // 跌停价挂卖单大概率无法成交,只会消耗当日工单额度并误导用户「已在处理」。
            // (submit_signal 先查重再查总开关,Duplicate 分支必须自己再查一次。)
            let rules = store::get_risk_rules(conn, p.user_id)?;
            if !rules.enabled || crate::trade::settings::kill_switch(conn)? {
                return Ok(());
            }
            let eps = 0.5 * 10f64.powi(-crate::stock::ashare::price_decimals(&p.code));
            if q.limit_down.is_some_and(|d| q.price <= d + eps) {
                return Ok(());
            }
            // 同用户同代码同方向若已有另一张挂起的实盘卖出工单(即便所属信号不同),
            // 也不应重发——避免同一持仓同时存在多张待确认的卖出工单。
            if !ticket::has_open_ticket(conn, p.user_id, &p.code, Direction::Sell)? {
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
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Account, Position, RiskRules, TicketStatus};
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
    fn reissue_skipped_when_trading_disabled() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r1 = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r1.new_real_tickets.len(), 1, "首次止损应出单");

        store::save_risk_rules(
            &c,
            1,
            &RiskRules {
                enabled: false,
                ..RiskRules::default()
            },
            at(16, 10, 5),
        )
        .unwrap();

        let r2 = run_tick(&mut c, &quote(9.0, at(16, 10, 31)), at(16, 10, 31)).unwrap();
        assert_eq!(r2.expired, 1, "旧工单应正常到期");
        assert!(
            r2.new_real_tickets.is_empty(),
            "总开关关闭时不应重发,{:?}",
            r2.new_real_tickets
        );
    }

    #[test]
    fn reissue_skipped_when_kill_switch_on() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r1 = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r1.new_real_tickets.len(), 1, "首次止损应出单");

        crate::trade::settings::set_kill_switch(&c, true, at(16, 10, 5)).unwrap();

        let r2 = run_tick(&mut c, &quote(9.0, at(16, 10, 31)), at(16, 10, 31)).unwrap();
        assert_eq!(r2.expired, 1, "旧工单应正常到期");
        assert!(
            r2.new_real_tickets.is_empty(),
            "管理员总开关打开时不应重发,{:?}",
            r2.new_real_tickets
        );
    }

    #[test]
    fn reissue_skipped_at_limit_down() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r1 = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r1.new_real_tickets.len(), 1, "首次止损应出单");

        // 10:31:旧工单到期,但现价 8.19 已触及跌停(8.19)→ 不应重发
        let r2 = run_tick(&mut c, &quote(8.19, at(16, 10, 31)), at(16, 10, 31)).unwrap();
        assert_eq!(r2.expired, 1);
        assert!(
            r2.new_real_tickets.is_empty(),
            "跌停时不应重发,{:?}",
            r2.new_real_tickets
        );

        // 10:32:价格回到跌停价之上 → 应正常重发,urgency 递增
        let r3 = run_tick(&mut c, &quote(8.5, at(16, 10, 32)), at(16, 10, 32)).unwrap();
        assert_eq!(r3.new_real_tickets.len(), 1, "脱离跌停后应重发");
        let t = ticket::get_ticket(&c, r3.new_real_tickets[0])
            .unwrap()
            .unwrap();
        assert_eq!(t.urgency, 1);
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
    fn no_reissue_while_another_sell_ticket_is_open() {
        let mut c = db_with_position(|p| {
            p.stop_loss = Some(9.2);
            p.trailing_pct = Some(0.05);
        });

        // t0: 12.0 → 记录移动止盈最高价,不触发
        let r0 = run_tick(&mut c, &quote(12.0, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r0.exit_signals, 0);

        // t1: 11.3 ≤ 12×0.95=11.4 → 触发移动止盈,创建实盘工单 A(挂起,10:31 到期)
        let r1 = run_tick(&mut c, &quote(11.3, at(16, 10, 1)), at(16, 10, 1)).unwrap();
        assert_eq!(r1.new_real_tickets.len(), 1, "移动止盈应创建实盘工单");
        let ticket_a = r1.new_real_tickets[0];
        assert_eq!(
            ticket::get_ticket(&c, ticket_a)
                .unwrap()
                .unwrap()
                .expires_at,
            at(16, 10, 31)
        );

        // t2: 9.1 ≤ 9.2 → 止损优先于移动止盈触发,dedup_key 是全新的;
        // 但 A 仍挂起(同用户同代码同方向)→ 闸门按 DuplicateOpenTicket 拒绝,不产生新工单
        let r2 = run_tick(&mut c, &quote(9.1, at(16, 10, 2)), at(16, 10, 2)).unwrap();
        assert!(
            r2.new_real_tickets.is_empty(),
            "A 挂起中,止损信号应被闸门拒绝而非出单"
        );
        assert!(r2.errors.is_empty(), "{:?}", r2.errors);

        // 直接到期 A(不经 run_tick)
        assert_eq!(ticket::expire_due(&c, at(16, 10, 31)).unwrap(), 1);
        assert_eq!(
            ticket::get_ticket(&c, ticket_a).unwrap().unwrap().status,
            TicketStatus::Expired
        );

        // t3: 9.1 → 止损信号的 dedup_key 此前被拒绝,现重新激活;A 已过期且无其它挂起 → 正常出单 B
        let r3 = run_tick(&mut c, &quote(9.1, at(16, 10, 32)), at(16, 10, 32)).unwrap();
        assert_eq!(
            r3.new_real_tickets.len(),
            1,
            "止损 key 重新激活后应正常出单"
        );
        let ticket_b = r3.new_real_tickets[0];
        assert_ne!(ticket_b, ticket_a);

        // t4: 11.3 → 移动止盈的 dedup_key(A 所属信号,已 ticketed)→ Duplicate;
        // A 本身已过期,但止损工单 B 仍挂起(同用户同代码同方向)→ 不应重发
        //
        // 注:若沿用协调者原始描述在此仍用价格 9.1,由于 `trigger()` 对止损的优先级
        // 高于移动止盈(见 exits.rs trigger()),9.1 会重新算出"止损"key(而非"移动止盈"
        // key),两者都会因 B 挂起而不重发,但那样测的是同一 key 的自重复,而非"另一张
        // 挂起工单挡住重发"。这里改用 11.3 使 `trigger()` 真正切回移动止盈规则,
        // 从而实际验证"不同信号的挂起工单也能挡住重发"这一行为(详见任务报告 Concerns)。
        let r4 = run_tick(&mut c, &quote(11.3, at(16, 10, 33)), at(16, 10, 33)).unwrap();
        assert!(
            r4.new_real_tickets.is_empty(),
            "止损工单 B 仍挂起,移动止盈不应重发"
        );
        assert!(r4.errors.is_empty(), "{:?}", r4.errors);
    }

    #[test]
    fn same_code_real_and_paper_exit_independently() {
        let mut c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        for account in [Account::Real, Account::Paper] {
            let mut p = Position::empty(1, account, "600000");
            p.qty = 1000;
            p.avg_cost = 10.0;
            p.stop_loss = Some(9.2);
            p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
            store::upsert_position(&c, &p, at(15, 15, 0)).unwrap();
        }

        let r = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.new_real_tickets.len(), 1, "实盘应挂起待确认工单");
        assert!(
            store::get_position(&c, 1, Account::Paper, "600000")
                .unwrap()
                .is_none(),
            "模拟盘应已即时撮合清仓"
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn paper_match_failure_is_recorded_not_propagated() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        // 插入一张 side 字段损坏的模拟盘工单,使 fill_pending_paper 顶层解析失败
        c.execute(
            "INSERT INTO trade_tickets (user_id, signal_id, account, code, side, suggest_price,
               qty, filled_qty, expires_at, deviation_th, status, urgency, created_at)
             VALUES (1, 0, 'paper', '600000', 'sideways', 10.0, 100, 0,
               '2026-09-16 23:59:59', 0.015, 'confirmed', 0, '2026-09-16 10:00:00')",
            [],
        )
        .unwrap();

        let r = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.paper, PaperBatch::default(), "撮合失败时不产出成交批次");
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].contains("模拟盘批量撮合失败"), "{:?}", r.errors);
        // 止损信号本身应正常出单,不受模拟盘撮合失败影响
        assert_eq!(r.new_real_tickets.len(), 1);
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

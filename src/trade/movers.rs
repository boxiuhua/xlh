//! 实时异动 → 交易信号。策略准入(计划 3)上线前一律按观察期处理:只进模拟盘。

use crate::event::Direction;
use crate::stock::realtime::movers::{trade_action, Mover, TradeAction};
use crate::trade::gate::Admission;
use crate::trade::model::{side_str, Account, AccountScope, NewSignal, Quote, SignalSource};
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use crate::trade::store;
use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::Connection;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MoverReport {
    pub signals: usize,
    pub ticketed: usize,
    pub errors: Vec<String>,
}

pub fn mover_signal(user_id: i64, m: &Mover, side: Direction) -> NewSignal {
    let hint = match side {
        Direction::Buy => "主力疑似吸筹",
        Direction::Sell => "疑似散户抬轿、主力流出",
    };
    NewSignal {
        user_id,
        source: SignalSource::Mover,
        strategy_id: None,
        code: m.code.clone(),
        name: Some(m.name.clone()),
        side,
        scope: AccountScope::Both,
        ref_price: m.price,
        reason: format!(
            "盘中异动:10 分钟 {:+.1}%,量能 {:.1} 倍,{hint}(未经前瞻检验,观察期仅模拟盘)",
            m.jump_pct * 100.0,
            m.vol_surge_x
        ),
        ai_note: None,
        dedup_key: format!(
            "mover-{}-{}-{}",
            side_str(side),
            m.code,
            m.ts.format("%Y%m%d%H%M")
        ),
        suggest_cash: None,
        suggest_qty: None,
    }
}

pub fn submit_mover_signals(
    conn: &mut Connection,
    movers: &[Mover],
    now: NaiveDateTime,
) -> Result<MoverReport> {
    let mut report = MoverReport::default();
    if movers.is_empty() {
        return Ok(report);
    }
    for uid in store::users_with_real_account(conn)? {
        let watch = match crate::push::store::get(conn, uid)? {
            Some(cfg) => cfg.realtime_watch_stocks,
            None => continue,
        };
        if watch.is_empty() {
            continue;
        }
        for m in movers.iter().filter(|m| watch.iter().any(|c| c == &m.code)) {
            let side = match trade_action(m.divergence) {
                TradeAction::Buy => Direction::Buy,
                TradeAction::Sell => {
                    let held = store::get_position(conn, uid, Account::Real, &m.code)?.is_some()
                        || store::get_position(conn, uid, Account::Paper, &m.code)?.is_some();
                    if !held {
                        continue;
                    }
                    Direction::Sell
                }
                TradeAction::Hold => continue,
            };
            let quote = store::fresh_quote(conn, &m.code, now, 60)?.unwrap_or(Quote {
                code: m.code.clone(),
                price: m.price,
                limit_up: None,
                limit_down: None,
                ts: m.ts,
            });
            let sig = mover_signal(uid, m, side);
            report.signals += 1;
            let ctx = SubmitContext {
                quote: Some(&quote),
                admission: Admission::Probation,
                now,
            };
            match submit_signal(conn, &sig, &ctx) {
                Ok(SubmitOutcome::Ticketed { .. }) => report.ticketed += 1,
                Ok(_) => {}
                Err(e) => report.errors.push(format!("用户 {uid} {}: {e:#}", m.code)),
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::realtime::movers::{Baseline, Divergence, Horizon};
    use crate::trade::model::{Account, Position};
    use crate::trade::ticket;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn mover(code: &str, divergence: Divergence) -> Mover {
        Mover {
            code: code.into(),
            name: "测试股".into(),
            ts: at(10, 30),
            price: 10.0,
            jump_pct: 0.03,
            vol_surge_x: 4.0,
            main_net: Some(1.0e7),
            main_net_pct: Some(0.08),
            divergence,
            horizon: Horizon::Short,
            baseline: Baseline::History,
        }
    }

    fn db(watch: &[&str]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        // push_configs.user_id 声明 REFERENCES users(id),但本测试不建 users 表;
        // 这里没有经 web::auth::store::open_in_memory() 打开连接,而该函数正是
        // 因为本地捆绑的 SQLite 默认开启外键约束才显式 OFF 掉(见 src/web/auth/store.rs)。
        c.pragma_update(None, "foreign_keys", "OFF").ok();
        crate::trade::store::migrate(&c).unwrap();
        crate::push::store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0)).unwrap();
        let mut cfg = crate::push::config::default_config();
        cfg.realtime_watch_stocks = watch.iter().map(|s| s.to_string()).collect();
        crate::push::store::upsert(&c, 1, &cfg).unwrap();
        c
    }

    #[test]
    fn buy_mover_in_watchlist_becomes_paper_only_ticket() {
        let mut c = db(&["600000"]);
        let r = submit_mover_signals(
            &mut c,
            &[
                mover("600000", Divergence::MainAccumulating),
                mover("600036", Divergence::MainAccumulating),
            ],
            at(10, 31),
        )
        .unwrap();
        assert_eq!((r.signals, r.ticketed), (1, 1), "{:?}", r.errors);
        let tickets = ticket::list_tickets(&c, 1, &[]).unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].account, Account::Paper, "观察期只进模拟盘");
        let sig = mover_signal(
            1,
            &mover("600000", Divergence::MainAccumulating),
            Direction::Buy,
        );
        assert_eq!(sig.dedup_key, "mover-buy-600000-202609161030");
        assert_eq!(sig.source, SignalSource::Mover);
    }

    #[test]
    fn hold_and_unheld_sell_movers_are_ignored() {
        let mut c = db(&["600000"]);
        let r = submit_mover_signals(
            &mut c,
            &[
                mover("600000", Divergence::None),
                mover("600000", Divergence::RetailChasing),
            ],
            at(10, 31),
        )
        .unwrap();
        assert_eq!(r.signals, 0);

        let mut p = Position::empty(1, Account::Paper, "600000");
        p.qty = 1000;
        p.avg_cost = 9.0;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        store::upsert_position(&c, &p, at(9, 0)).unwrap();
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::RetailChasing)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!((r.signals, r.ticketed), (1, 1), "{:?}", r.errors);
    }

    #[test]
    fn users_without_watchlist_or_real_account_are_skipped() {
        let mut c = db(&[]);
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::MainAccumulating)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!(r.signals, 0, "空名单不生成交易信号");
    }
}

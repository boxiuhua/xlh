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
    /// 命中订阅但无今日新鲜报价而被跳过的异动数(不计入 signals)。
    pub no_quote: usize,
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

/// 为异动代码拉取实时报价并写入缓存(只保留今日时间戳),返回写入条数。
///
/// 异动信号需要涨跌停价才能在闸门正确判定「已涨停不追买/已跌停不追卖」;
/// 异动榜自身的 `Mover.price` 只是触发时刻的快照价,不带涨跌停信息。
pub fn refresh_mover_quotes(
    conn: &Connection,
    source: &dyn crate::trade::quotes::QuoteSource,
    movers: &[Mover],
    now: NaiveDateTime,
) -> Result<usize> {
    let mut codes: Vec<String> = movers.iter().map(|m| m.code.clone()).collect();
    codes.sort();
    codes.dedup();
    if codes.is_empty() {
        return Ok(0);
    }
    let fresh: Vec<Quote> = source
        .fetch(&codes)?
        .into_iter()
        .filter(|q| q.ts.date() == now.date())
        .collect();
    store::upsert_quotes(conn, &fresh, now)
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
        if let Err(e) = process_user(conn, uid, movers, now, &mut report) {
            report.errors.push(format!("用户 {uid}: {e:#}"));
        }
    }
    Ok(report)
}

/// 单个用户的订阅匹配与提交;任一环节出错都整体中止该用户,由调用方记入
/// `report.errors` 并继续处理下一个用户,不影响其余用户的信号。
fn process_user(
    conn: &mut Connection,
    uid: i64,
    movers: &[Mover],
    now: NaiveDateTime,
    report: &mut MoverReport,
) -> Result<()> {
    let watch = match crate::push::store::get(conn, uid)? {
        Some(cfg) => cfg.realtime_watch_stocks,
        None => return Ok(()),
    };
    if watch.is_empty() {
        return Ok(());
    }
    for m in movers.iter().filter(|m| watch.iter().any(|c| c == &m.code)) {
        let side = match trade_action(m.divergence) {
            TradeAction::Buy => Direction::Buy,
            TradeAction::Sell => {
                // 观察期只进模拟盘,故只以模拟盘持仓判定是否可卖。
                let held = store::get_position(conn, uid, Account::Paper, &m.code)?.is_some();
                if !held {
                    continue;
                }
                Direction::Sell
            }
            TradeAction::Hold => continue,
        };
        // 无今日新鲜报价(未进异动扫描的报价缓存,或已陈旧)就没有可靠的涨跌停价,
        // 宁可跳过也不能像旧实现那样拿 Mover 快照价拼一个 limit_up/down 皆为 None
        // 的假报价——那会让涨跌停判定形同虚设。
        let Some(quote) = store::fresh_quote(conn, &m.code, now, 60)? else {
            report.no_quote += 1;
            continue;
        };
        let sig = mover_signal(uid, m, side);
        report.signals += 1;
        let ctx = SubmitContext {
            quote: Some(&quote),
            admission: Admission::Probation,
            now,
        };
        if let SubmitOutcome::Ticketed { .. } = submit_signal(conn, &sig, &ctx)? {
            report.ticketed += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::realtime::movers::{Baseline, Divergence, Horizon};
    use crate::trade::model::{Account, Position};
    use crate::trade::quotes::QuoteSource;
    use crate::trade::ticket;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    /// 直接写入报价缓存,绕开真实报价源:测试只关心 submit_mover_signals /
    /// refresh_mover_quotes 各自的行为,不关心报价从哪来。
    fn cache_quote(
        c: &Connection,
        code: &str,
        price: f64,
        limit_up: Option<f64>,
        now: NaiveDateTime,
    ) {
        store::upsert_quotes(
            c,
            &[Quote {
                code: code.into(),
                price,
                limit_up,
                limit_down: None,
                ts: now,
            }],
            now,
        )
        .unwrap();
    }

    struct StubSource(Vec<Quote>);

    impl QuoteSource for StubSource {
        fn fetch(&self, _codes: &[String]) -> Result<Vec<Quote>> {
            Ok(self.0.clone())
        }
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
        cache_quote(&c, "600000", 10.0, None, at(10, 31));
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
        cache_quote(&c, "600000", 10.0, None, at(10, 31));
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::RetailChasing)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!((r.signals, r.ticketed), (1, 1), "{:?}", r.errors);
    }

    #[test]
    fn per_user_failure_is_isolated_and_recorded() {
        let mut c = db(&["600000"]);
        // 用户 2 也订阅同一代码,但写入损坏的风控规则 JSON,使其 submit_signal 内部
        // 读取风控规则时报错;应仅记入该用户的错误,不影响用户 1 也不使整体调用失败。
        store::set_capital(&c, 2, Account::Real, 50_000.0, at(9, 0)).unwrap();
        let mut cfg2 = crate::push::config::default_config();
        cfg2.realtime_watch_stocks = vec!["600000".into()];
        crate::push::store::upsert(&c, 2, &cfg2).unwrap();
        c.execute(
            "INSERT INTO trade_risk_rules (user_id, rules_json, updated_at) VALUES (2, 'not-json', 'x')",
            [],
        )
        .unwrap();
        cache_quote(&c, "600000", 10.0, None, at(10, 31));

        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::MainAccumulating)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!(r.ticketed, 1, "用户 1 应正常成交,不受用户 2 影响");
        assert_eq!(r.errors.len(), 1);
        assert!(r.errors[0].starts_with("用户 2:"), "{:?}", r.errors);
    }

    #[test]
    fn sell_mover_requires_paper_holding_even_if_real_position_exists() {
        let mut c = db(&["600000"]);
        let mut p = Position::empty(1, Account::Real, "600000");
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
        assert_eq!(r.signals, 0, "仅有实盘持仓、无模拟盘持仓时不应视为可卖");
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

    #[test]
    fn mover_without_fresh_quote_is_skipped() {
        let mut c = db(&["600000"]);
        // 未调用 refresh_mover_quotes / cache_quote,缓存中无该代码的今日报价。
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::MainAccumulating)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!((r.signals, r.no_quote), (0, 1), "{:?}", r.errors);
    }

    #[test]
    fn mover_buy_at_limit_up_is_rejected() {
        let mut c = db(&["600000"]);
        cache_quote(&c, "600000", 11.0, Some(11.0), at(10, 31));
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::MainAccumulating)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!(
            (r.signals, r.ticketed, r.no_quote),
            (1, 0, 0),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn refresh_mover_quotes_caches_today_only() {
        let c = db(&["600000"]);
        let today = at(10, 31);
        let yesterday_ts = NaiveDate::from_ymd_opt(2026, 9, 15)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap();
        let source = StubSource(vec![
            Quote {
                code: "600000".into(),
                price: 10.0,
                limit_up: None,
                limit_down: None,
                ts: today,
            },
            Quote {
                code: "600036".into(),
                price: 20.0,
                limit_up: None,
                limit_down: None,
                ts: yesterday_ts,
            },
        ]);
        let movers = [
            mover("600000", Divergence::None),
            mover("600036", Divergence::None),
        ];
        let n = refresh_mover_quotes(&c, &source, &movers, today).unwrap();
        assert_eq!(n, 1, "只有今日行情应写入缓存");
        assert!(
            store::get_quote(&c, "600000").unwrap().is_some(),
            "今日报价应缓存"
        );
        assert!(
            store::get_quote(&c, "600036").unwrap().is_none(),
            "昨日报价不应缓存"
        );
    }
}

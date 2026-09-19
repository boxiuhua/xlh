use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use std::cell::RefCell;
use xlh::trade::daemon::{alert_holders, notify_new_tickets, send_fill_reminders};
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
    assert_eq!(
        notify_new_tickets(&c, &rec, &report.new_real_tickets, ""),
        1
    );
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
    let mut paper_only = Position::empty(1, Account::Paper, "600000");
    paper_only.qty = 100;
    store::upsert_position(&c, &paper_only, at(15, 15, 0)).unwrap();
    let rec = Recorder::default();
    assert_eq!(
        alert_holders(&c, &rec, 3).unwrap(),
        1,
        "用户 1 只有模拟盘持仓,不应计入告警"
    );
    assert_eq!(rec.0.borrow()[0].0, 2);
}

/// 以 `last` 为最后一天、往前连续工作日的 K 线(开高低收同价)。
fn bars_until(last: NaiveDate, prices: &[f64]) -> Vec<xlh::stock::data::StockBar> {
    use chrono::Datelike;
    let mut dates = Vec::new();
    let mut day = last;
    while dates.len() < prices.len() {
        if !matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
            dates.push(day);
        }
        day -= chrono::Duration::days(1);
    }
    dates.reverse();
    dates
        .into_iter()
        .zip(prices)
        .map(|(date, p)| xlh::stock::data::StockBar {
            date,
            open: *p,
            high: *p,
            low: *p,
            close: *p,
            volume: 1.0,
            adj_close: *p,
        })
        .collect()
}

/// 观察期的 trend 策略,oos 基线里每只股票都带实盘参数(短 5 / 长 20 / 每次 10 万)。
fn running_trend_strategy(c: &Connection, pool: &[&str]) -> i64 {
    use xlh::trade::admission::{judge::Verdict, state, walk_forward};
    let id = store::create_strategy(
        c,
        &xlh::trade::model::NewStrategy {
            user_id: 1,
            name: "趋势".into(),
            kind: "trend".into(),
            grid_toml: "short_window = [5]\nlong_window = [20]\namount = [100000.0]".into(),
            pool: pool.iter().map(|s| s.to_string()).collect(),
        },
        at(1, 9, 0),
    )
    .unwrap();
    state::submit_for_backtest(c, 1, id, at(1, 9, 1)).unwrap();
    let metrics = walk_forward::aggregate(
        pool.iter()
            .map(|code| walk_forward::CodeMetrics {
                code: code.to_string(),
                windows: 1,
                oos_return: 0.1,
                oos_annualized: 0.1,
                oos_sharpe: 1.0,
                oos_max_drawdown: 0.1,
                oos_trades: 10,
                is_sharpe: 1.0,
                years: 1.0,
                data_years: 4.0,
                buy_hold_return: 0.0,
                trade_baseline: Default::default(),
                window_details: Vec::new(),
                data_from: None,
                data_to: None,
                live_params: Some(toml::Value::Table(
                    "short_window = 5\nlong_window = 20\namount = 100000.0"
                        .parse()
                        .unwrap(),
                )),
            })
            .collect(),
    );
    let verdict = Verdict {
        passed: true,
        reasons: Vec::new(),
    };
    let day = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    state::apply_backtest_verdict(c, 1, id, &metrics, &verdict, day, day, at(1, 9, 2)).unwrap();
    assert_eq!(
        store::get_strategy(c, 1, id).unwrap().unwrap().status,
        xlh::trade::model::StrategyStatus::Paper
    );
    id
}

#[test]
fn daily_strategy_signal_flows_from_close_to_paper_fill() {
    use xlh::trade::admission::walk_forward::WalkForwardCfg;
    use xlh::trade::config::SignalCfg;
    use xlh::trade::daily_signals;

    // 1. 内存库 + 实盘资金
    let mut c = Connection::open_in_memory().unwrap();
    store::migrate(&c).unwrap();
    store::set_capital(&c, 1, Account::Real, 1_000_000.0, at(1, 9, 0)).unwrap();
    // 2. 观察期 trend 策略,oos 基线带实盘参数
    let sid = running_trend_strategy(&c, &["600000"]);
    // 3. 周五收盘后计算:最后一根 K 线(周五)金叉
    let friday = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
    let mut prices: Vec<f64> = (0..80).map(|i| 20.0 - i as f64 * 0.1).collect();
    prices.extend([12.2, 12.3, 12.4, 12.5, 25.0]);
    let r = daily_signals::compute(
        &c,
        &xlh::trade::config::SignalCfg::default(),
        at(18, 15, 30),
        &WalkForwardCfg::default(),
        |_| Ok(bars_until(friday, &prices)),
    )
    .unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.planned, 1);
    // 4. 下周一开盘窗口内发出,报价为周一 09:25 的集合竞价价
    let quotes = Fixed(vec![Quote {
        code: "600000".into(),
        price: 25.0,
        limit_up: Some(27.5),
        limit_down: Some(22.5),
        ts: at(21, 9, 25),
    }]);
    let r = daily_signals::emit_due(&mut c, &quotes, &SignalCfg::default(), at(21, 9, 26)).unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.submitted, 1);
    assert!(r.new_real_tickets.is_empty(), "观察期只进模拟盘");
    // 5. 模拟盘即时成交,且观察期统计能数到这笔成交
    let paper_buys: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM trade_fills WHERE account = 'paper' AND side = 'buy' AND code = '600000'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(paper_buys, 1);
    let fills =
        xlh::trade::admission::stats::strategy_fills(&c, 1, sid, Account::Paper, None).unwrap();
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].side, xlh::event::Direction::Buy);
    // 模拟盘按开盘报价加滑点成交,落在周一
    assert!((25.0..25.5).contains(&fills[0].price), "{}", fills[0].price);
    assert_eq!(fills[0].filled_at.date(), at(21, 9, 26).date());
}

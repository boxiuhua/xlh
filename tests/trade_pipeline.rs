//! 全链路集成测试(spec §13):信号 → 闸门 → 工单 → 确认 → 回填 → 持仓 → 止损触发 → 卖出工单。
//! 只用 `xlh::` 公开 API、内存库、`QuoteSource` 桩,时间全部固定。

use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use xlh::event::Direction;
use xlh::trade::actions::{self, ManualOrder, ManualOutcome};
use xlh::trade::model::{Account, Quote, RiskRules, TicketStatus};
use xlh::trade::monitor::run_tick;
use xlh::trade::quotes::QuoteSource;
use xlh::trade::{store, ticket};

fn at(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, d)
        .unwrap()
        .and_hms_opt(h, m, s)
        .unwrap()
}

fn quote(price: f64, ts: NaiveDateTime) -> Quote {
    Quote {
        code: "600000".into(),
        price,
        limit_up: Some(11.0),
        limit_down: Some(9.0),
        ts,
    }
}

/// 单只股票的固定报价桩(`monitor::run_tick` 用):每次 `fetch` 返回同一份报价。
struct Fixed(Vec<Quote>);

impl QuoteSource for Fixed {
    fn fetch(&self, _codes: &[String]) -> anyhow::Result<Vec<Quote>> {
        Ok(self.0.clone())
    }
}

/// 2026-09-23(周三)10:00 手动买入 600000,次日(周四)止损行情触发监听,
/// 产出卖出工单;同日再触发一次不重复出单。
#[test]
fn signal_to_stop_loss_sell_ticket() {
    // 1. 内存库 + 资金 + 默认风控规则
    let mut c = Connection::open_in_memory().unwrap();
    store::migrate(&c).unwrap();
    store::set_capital(&c, 1, Account::Real, 100_000.0, at(23, 9, 0, 0)).unwrap();
    store::save_risk_rules(&c, 1, &RiskRules::default(), at(23, 9, 0, 0)).unwrap();

    // 2. 手动买入信号,报价 10.00、今日时间戳 → 实盘工单待确认,模拟盘同时成交
    let buy_time = at(23, 10, 0, 0);
    let q = quote(10.0, buy_time);
    store::upsert_quotes(&c, std::slice::from_ref(&q), buy_time).unwrap();
    let order = ManualOrder {
        request_id: "req-1".into(),
        code: "600000".into(),
        name: None,
        side: Direction::Buy,
        amount: None,
        qty: None,
        reason: "集成测试买入".into(),
        ai_note: None,
    };
    let outcome = actions::submit_manual(&mut c, 1, &order, Some(&q), buy_time).unwrap();
    let ManualOutcome::Ticketed {
        real_ticket: Some(real_ticket_id),
        paper_ticket,
    } = outcome
    else {
        panic!("手动买入应生成待确认实盘工单: {outcome:?}");
    };
    assert!(paper_ticket.is_some(), "模拟盘工单应同时生成");
    let buy_ticket = ticket::get_ticket(&c, real_ticket_id).unwrap().unwrap();
    assert_eq!(buy_ticket.status, TicketStatus::Pending);
    let buy_qty = buy_ticket.qty;
    assert!(buy_qty > 0);
    let paper_pos = store::get_position(&c, 1, Account::Paper, "600000")
        .unwrap()
        .unwrap();
    assert_eq!(paper_pos.qty, buy_qty, "模拟盘工单应同时成交");

    // 3. 确认实盘工单:行情新鲜、无偏离 → Ok(())
    let confirm_time = at(23, 10, 0, 30);
    assert_eq!(
        actions::confirm_ticket(&c, 1, real_ticket_id, false, None, confirm_time).unwrap(),
        Ok(())
    );
    assert_eq!(
        ticket::get_ticket(&c, real_ticket_id)
            .unwrap()
            .unwrap()
            .status,
        TicketStatus::Confirmed
    );

    // 4. 回填成交:实盘持仓数量 = 工单数量、均价 ≈ 10.00 + 费用摊薄、默认止损价按价位取整 = 9.20
    let fill_time = at(23, 10, 5, 0);
    let fill = actions::manual_fill(&mut c, 1, real_ticket_id, 10.0, buy_qty, fill_time)
        .unwrap()
        .unwrap();
    assert_eq!(fill.status, TicketStatus::Filled);
    let pos = store::get_position(&c, 1, Account::Real, "600000")
        .unwrap()
        .unwrap();
    assert_eq!(pos.qty, buy_qty);
    assert!(
        pos.avg_cost >= 10.0 && pos.avg_cost < 10.01,
        "avg_cost={}",
        pos.avg_cost
    );
    assert_eq!(pos.stop_loss, Some(9.2));

    // 5. 次日 2026-09-24 10:00 起跑 monitor::run_tick:先 9.50(不触发),再 9.10(触发止损)
    let tick1_time = at(24, 10, 0, 0);
    let r1 = run_tick(&mut c, &Fixed(vec![quote(9.5, tick1_time)]), tick1_time).unwrap();
    assert!(r1.skipped.is_none(), "{:?}", r1.skipped);
    assert_eq!(r1.exit_signals, 0, "9.50 高于止损价,不应触发");
    assert!(r1.new_real_tickets.is_empty());

    let tick2_time = at(24, 10, 1, 0);
    let r2 = run_tick(&mut c, &Fixed(vec![quote(9.1, tick2_time)]), tick2_time).unwrap();

    // 6. 第二次 run_tick 触发止损:实盘、模拟盘持仓各触发一次退出信号(monitor 按持仓逐行
    // 判定,两个账户独立计数,故 exit_signals == 2),但只有实盘产出待确认卖出工单——
    // 模拟盘在 submit_signal 内已同步按现价成交,不计入 new_real_tickets。
    // 数量 = 可卖数量(T+1 已过,次日全部可卖),来源为 exit
    assert_eq!(r2.exit_signals, 2, "{:?}", r2);
    assert_eq!(r2.new_real_tickets.len(), 1, "{:?}", r2.new_real_tickets);
    assert!(
        store::get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .is_none(),
        "模拟盘止损应已同步平仓"
    );
    let sell_ticket_id = r2.new_real_tickets[0];
    let sell_ticket = ticket::get_ticket(&c, sell_ticket_id).unwrap().unwrap();
    assert_eq!(sell_ticket.side, Direction::Sell);
    let sellable = pos.sellable(NaiveDate::from_ymd_opt(2026, 9, 24).unwrap());
    assert_eq!(sellable, buy_qty, "T+1 已过,应全部可卖");
    assert_eq!(sell_ticket.qty, sellable);
    let source: String = c
        .query_row(
            "SELECT source FROM trade_signals WHERE id = ?1",
            [sell_ticket.signal_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "exit");

    // 7. 同一日再跑一次(9.05):当日同一规则只触发一次,不产生新的实盘工单
    let tick3_time = at(24, 10, 2, 0);
    let r3 = run_tick(&mut c, &Fixed(vec![quote(9.05, tick3_time)]), tick3_time).unwrap();
    assert!(
        r3.new_real_tickets.is_empty(),
        "同日重复触发不应再出新单: {:?}",
        r3.new_real_tickets
    );
    let real_tickets_for_code: i64 = c
        .query_row(
            "SELECT COUNT(*) FROM trade_tickets WHERE account = 'real' AND side = 'sell' AND code = '600000'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(real_tickets_for_code, 1, "全程只应有一张实盘卖出工单");
}

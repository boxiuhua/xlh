//! 从成交记录还原模拟盘 / 实盘表现:逐笔收益、回撤、观察期与 watchdog 统计、执行损耗。
//!
//! 没有逐日市值快照,回撤一律用「累计已实现盈亏曲线」相对峰值的最大回撤近似。

use crate::event::Direction;
use crate::trade::admission::judge::{PaperStats, WatchdogStats};
use crate::trade::model::{fmt_ts, parse_side, parse_ts, Account};
use anyhow::Result;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Weekday};
use rusqlite::{params, Connection, Row};
use std::collections::BTreeMap;

/// 一条成交记录(已按策略过滤)。
#[derive(Debug, Clone, PartialEq)]
pub struct FillRow {
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub price: f64,
    pub qty: u64,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
    pub filled_at: NaiveDateTime,
    pub signal_id: i64,
}

#[allow(clippy::type_complexity)]
fn read_fill(
    r: &Row,
) -> rusqlite::Result<(
    String,
    String,
    String,
    f64,
    i64,
    f64,
    Option<f64>,
    String,
    i64,
)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
    ))
}

/// 该用户该策略在指定账户下的成交,按时间序。`since` 为 None 时取全部。
pub fn strategy_fills(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    account: Account,
    since: Option<NaiveDateTime>,
) -> Result<Vec<FillRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.account, f.code, f.side, f.price, f.qty, f.fee, f.realized_pnl, f.filled_at, t.signal_id
         FROM trade_fills f
         JOIN trade_tickets t ON t.id = f.ticket_id
         JOIN trade_signals s ON s.id = t.signal_id
         WHERE f.user_id = ?1 AND s.user_id = ?1 AND s.strategy_id = ?2 AND f.account = ?3
           AND (?4 IS NULL OR f.filled_at >= ?4)
         ORDER BY f.id",
    )?;
    let rows = stmt
        .query_map(
            params![user_id, strategy_id, account.as_str(), since.map(fmt_ts)],
            read_fill,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(acc, code, side, price, qty, fee, realized_pnl, filled_at, signal_id)| {
                Ok(FillRow {
                    account: Account::parse(&acc)?,
                    code,
                    side: parse_side(&side)?,
                    price,
                    qty: qty.max(0) as u64,
                    fee,
                    realized_pnl,
                    filled_at: parse_ts(&filled_at)?,
                    signal_id,
                })
            },
        )
        .collect()
}

/// 卖出成交的已实现盈亏,按时间序。
pub fn sell_pnls(fills: &[FillRow]) -> Vec<f64> {
    fills
        .iter()
        .filter(|f| f.side == Direction::Sell)
        .filter_map(|f| f.realized_pnl)
        .collect()
}

/// 每笔卖出的收益率:realized / 成本,成本 = 成交额 − realized − 费用。
pub fn trade_returns(fills: &[FillRow]) -> Vec<f64> {
    fills
        .iter()
        .filter(|f| f.side == Direction::Sell)
        .filter_map(|f| {
            let realized = f.realized_pnl?;
            let cost = f.price * f.qty as f64 - realized - f.fee;
            (cost > 1e-9).then(|| realized / cost)
        })
        .collect()
}

/// 累计已实现盈亏曲线的最大回撤(相对峰值)。峰值 ≤ 0 时不计。
pub fn equity_drawdown(pnls: &[f64]) -> f64 {
    let mut cum = 0.0;
    let mut peak = 0.0f64;
    let mut worst = 0.0f64;
    for p in pnls {
        cum += p;
        peak = peak.max(cum);
        if peak > 0.0 {
            worst = worst.max((peak - cum) / peak);
        }
    }
    worst
}

/// 含首尾的工作日数;`to` 早于 `from` 返回 0。
pub fn workdays_between(from: NaiveDate, to: NaiveDate) -> i64 {
    let mut day = from;
    let mut n = 0;
    while day <= to {
        if !matches!(day.weekday(), Weekday::Sat | Weekday::Sun) {
            n += 1;
        }
        day += chrono::Duration::days(1);
    }
    n
}

fn max_streak_by_code(fills: &[FillRow]) -> usize {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cur: BTreeMap<&str, usize> = BTreeMap::new();
    for f in fills.iter().filter(|f| f.side == Direction::Sell) {
        let pnl = f.realized_pnl.unwrap_or(0.0);
        let c = cur.entry(f.code.as_str()).or_insert(0);
        if pnl <= 0.0 {
            *c += 1;
            let best = by_code.entry(f.code.as_str()).or_insert(0);
            *best = (*best).max(*c);
        } else {
            *c = 0;
        }
    }
    by_code.values().copied().max().unwrap_or(0)
}

/// 观察期统计:天数按工作日计,笔数为卖出成交数。
pub fn paper_stats(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    since: NaiveDateTime,
    now: NaiveDateTime,
) -> Result<PaperStats> {
    let fills = strategy_fills(conn, user_id, strategy_id, Account::Paper, Some(since))?;
    let returns = trade_returns(&fills);
    let avg = if returns.is_empty() {
        0.0
    } else {
        returns.iter().sum::<f64>() / returns.len() as f64
    };
    Ok(PaperStats {
        days: workdays_between(since.date(), now.date()),
        trades: fills.iter().filter(|f| f.side == Direction::Sell).count(),
        avg_trade_return: avg,
        max_drawdown: equity_drawdown(&sell_pnls(&fills)),
    })
}

/// 实盘 watchdog 统计:回撤取全部实盘成交,胜率取最近 `window` 笔,连亏按代码分组。
pub fn watchdog_stats(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    window: usize,
) -> Result<WatchdogStats> {
    let fills = strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    let pnls = sell_pnls(&fills);
    let recent: &[f64] = if pnls.len() > window {
        &pnls[pnls.len() - window..]
    } else {
        &pnls
    };
    let wins = recent.iter().filter(|p| **p > 0.0).count();
    Ok(WatchdogStats {
        drawdown: equity_drawdown(&pnls),
        recent_win_rate: if recent.is_empty() {
            0.0
        } else {
            wins as f64 / recent.len() as f64
        },
        recent_trades: recent.len(),
        max_consecutive_losses: max_streak_by_code(&fills),
    })
}

/// 执行损耗:同一信号下实盘相对模拟盘多付的比例中位数;无配对返回 None。
pub fn execution_loss(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<f64>> {
    let real = strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    let paper = strategy_fills(conn, user_id, strategy_id, Account::Paper, None)?;
    let mut paper_first: BTreeMap<i64, &FillRow> = BTreeMap::new();
    for f in &paper {
        paper_first.entry(f.signal_id).or_insert(f);
    }
    let mut losses: Vec<f64> = Vec::new();
    let mut seen: BTreeMap<i64, ()> = BTreeMap::new();
    for r in &real {
        if seen.insert(r.signal_id, ()).is_some() {
            continue;
        }
        let Some(p) = paper_first.get(&r.signal_id) else {
            continue;
        };
        if p.price <= 0.0 || r.price <= 0.0 {
            continue;
        }
        losses.push(match r.side {
            Direction::Buy => r.price / p.price - 1.0,
            Direction::Sell => p.price / r.price - 1.0,
        });
    }
    if losses.is_empty() {
        return Ok(None);
    }
    losses.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = losses.len();
    Ok(Some(if n % 2 == 1 {
        losses[n / 2]
    } else {
        (losses[n / 2 - 1] + losses[n / 2]) / 2.0
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{AccountScope, NewSignal, NewStrategy, SignalSource, TicketStatus};
    use crate::trade::store;
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn fill_row(
        account: Account,
        side: Direction,
        price: f64,
        realized: Option<f64>,
        signal_id: i64,
    ) -> FillRow {
        FillRow {
            account,
            code: "600000".into(),
            side,
            price,
            qty: 1000,
            fee: 10.61,
            realized_pnl: realized,
            filled_at: at(16, 10, 0),
            signal_id,
        }
    }

    /// 建一套「策略 → 信号 → 工单 → 成交」的数据,返回 (策略 id, 信号 id)。
    #[allow(clippy::too_many_arguments)]
    fn seed(
        c: &mut Connection,
        user_id: i64,
        strategy_id: Option<i64>,
        account: Account,
        side: Direction,
        price: f64,
        realized: Option<f64>,
        key: &str,
        now: NaiveDateTime,
    ) -> i64 {
        let mut sig = NewSignal {
            user_id,
            source: SignalSource::Strategy,
            strategy_id,
            code: "600000".into(),
            name: None,
            side,
            scope: AccountScope::Both,
            ref_price: price,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        };
        sig.strategy_id = strategy_id;
        let sid = insert_signal(c, &sig, now).unwrap().unwrap();
        let tid = create_ticket(
            c,
            &NewTicket {
                user_id,
                signal_id: sid,
                account,
                code: "600000".into(),
                side,
                suggest_price: price,
                qty: 1000,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                urgency: 0,
                created_at: now,
            },
        )
        .unwrap();
        // 直接写 trade_fills:绕开 record_fill 的持仓/资金校验,本测试只关心统计
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, ?2, ?3, '600000', ?4, ?5, 1000, 10.61, ?6, 'test', ?7)",
            rusqlite::params![
                tid,
                user_id,
                account.as_str(),
                crate::trade::model::side_str(side),
                price,
                realized,
                crate::trade::model::fmt_ts(now),
            ],
        )
        .unwrap();
        sid
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    fn strategy(c: &Connection, user_id: i64) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "rsi".into(),
                grid_toml: "rsi_window = [14]".into(),
                pool: vec!["600000".into()],
            },
            at(15, 9, 0),
        )
        .unwrap()
    }

    #[test]
    fn trade_returns_use_cost_derived_from_fill_fields() {
        // 卖出 1000 @11,费 10.61,realized 984.29 → 成本 = 11000 − 984.29 − 10.61 = 10005.1
        let sell = fill_row(Account::Real, Direction::Sell, 11.0, Some(984.29), 1);
        let r = trade_returns(std::slice::from_ref(&sell));
        assert_eq!(r.len(), 1);
        assert!((r[0] - 984.29 / 10_005.1).abs() < 1e-9, "{}", r[0]);
        assert_eq!(sell_pnls(std::slice::from_ref(&sell)), vec![984.29]);
        let buy = FillRow {
            side: Direction::Buy,
            realized_pnl: None,
            ..sell
        };
        assert!(
            trade_returns(std::slice::from_ref(&buy)).is_empty(),
            "买入不计入"
        );
        assert!(sell_pnls(&[buy]).is_empty());
    }

    #[test]
    fn drawdown_of_cumulative_pnl() {
        // 累计 100 → 60 → 160 → 110:峰值 100 后回撤 40(40%),峰值 160 后回撤 50(31.25%)
        assert!((equity_drawdown(&[100.0, -40.0, 100.0, -50.0]) - 0.4).abs() < 1e-9);
        assert_eq!(equity_drawdown(&[]), 0.0);
        assert_eq!(equity_drawdown(&[10.0, 20.0]), 0.0, "只涨不回撤");
        assert_eq!(equity_drawdown(&[-10.0, -20.0]), 0.0, "峰值未转正不计回撤");
    }

    #[test]
    fn workdays_skip_weekends() {
        // 2026-09-14(周一)到 2026-09-18(周五)= 5 个工作日;跨周末仍是 5 + 1
        assert_eq!(
            workdays_between(
                NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()
            ),
            5
        );
        assert_eq!(
            workdays_between(
                NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()
            ),
            6
        );
        assert_eq!(
            workdays_between(
                NaiveDate::from_ymd_opt(2026, 9, 21).unwrap(),
                NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
            ),
            0,
            "倒序为 0"
        );
    }

    #[test]
    fn strategy_fills_are_scoped_by_user_strategy_and_account() {
        let mut c = db();
        let mine = strategy(&c, 1);
        let other_strategy = strategy(&c, 1);
        let other_user = strategy(&c, 2);
        seed(
            &mut c,
            1,
            Some(mine),
            Account::Real,
            Direction::Sell,
            11.0,
            Some(100.0),
            "k1",
            at(16, 10, 0),
        );
        seed(
            &mut c,
            1,
            Some(mine),
            Account::Paper,
            Direction::Sell,
            11.0,
            Some(50.0),
            "k2",
            at(16, 10, 1),
        );
        seed(
            &mut c,
            1,
            Some(other_strategy),
            Account::Real,
            Direction::Sell,
            11.0,
            Some(70.0),
            "k3",
            at(16, 10, 2),
        );
        seed(
            &mut c,
            2,
            Some(other_user),
            Account::Real,
            Direction::Sell,
            11.0,
            Some(90.0),
            "k4",
            at(16, 10, 3),
        );
        seed(
            &mut c,
            1,
            None,
            Account::Real,
            Direction::Sell,
            11.0,
            Some(80.0),
            "k5",
            at(16, 10, 4),
        );

        let real = strategy_fills(&c, 1, mine, Account::Real, None).unwrap();
        assert_eq!(real.len(), 1);
        assert_eq!(real[0].realized_pnl, Some(100.0));
        let paper = strategy_fills(&c, 1, mine, Account::Paper, None).unwrap();
        assert_eq!(paper.len(), 1);
        assert_eq!(paper[0].realized_pnl, Some(50.0));
        assert!(
            strategy_fills(&c, 2, mine, Account::Real, None)
                .unwrap()
                .is_empty(),
            "他人不可见"
        );
        assert!(
            strategy_fills(&c, 1, mine, Account::Real, Some(at(16, 10, 1)))
                .unwrap()
                .is_empty(),
            "since 过滤"
        );
    }

    #[test]
    fn paper_and_watchdog_stats_come_from_fills() {
        let mut c = db();
        let s = strategy(&c, 1);
        // 三笔模拟盘卖出:+100、−50、+20
        for (i, pnl) in [100.0, -50.0, 20.0].iter().enumerate() {
            seed(
                &mut c,
                1,
                Some(s),
                Account::Paper,
                Direction::Sell,
                11.0,
                Some(*pnl),
                &format!("p{i}"),
                at(16, 10, i as u32),
            );
        }
        let ps = paper_stats(&c, 1, s, at(14, 9, 0), at(16, 15, 0)).unwrap();
        assert_eq!(ps.trades, 3);
        assert_eq!(ps.days, 3, "9-14 周一 到 9-16 周三");
        assert!(ps.max_drawdown > 0.0, "中间有回撤");

        // 实盘两笔亏损 → watchdog 连亏 2
        for (i, pnl) in [-30.0, -40.0].iter().enumerate() {
            seed(
                &mut c,
                1,
                Some(s),
                Account::Real,
                Direction::Sell,
                11.0,
                Some(*pnl),
                &format!("r{i}"),
                at(16, 11, i as u32),
            );
        }
        let ws = watchdog_stats(&c, 1, s, 20).unwrap();
        assert_eq!((ws.recent_trades, ws.max_consecutive_losses), (2, 2));
        assert_eq!(ws.recent_win_rate, 0.0);
    }

    #[test]
    fn execution_loss_compares_real_and_paper_on_the_same_signal() {
        let mut c = db();
        let s = strategy(&c, 1);
        // 同一信号:实盘买入 10.05、模拟买入 10.00 → 0.005
        let sid = seed(
            &mut c,
            1,
            Some(s),
            Account::Real,
            Direction::Buy,
            10.05,
            None,
            "e1",
            at(16, 10, 0),
        );
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             SELECT id, 1, 'paper', '600000', 'buy', 10.0, 1000, 5.0, NULL, 'test', '2026-09-16 10:00:00'
             FROM trade_tickets WHERE signal_id = ?1 LIMIT 1",
            [sid],
        )
        .unwrap();
        let loss = execution_loss(&c, 1, s).unwrap().unwrap();
        assert!((loss - 0.005).abs() < 1e-9, "{loss}");
        assert!(execution_loss(&c, 2, s).unwrap().is_none(), "他人无数据");
    }
}

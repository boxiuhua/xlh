//! 从成交记录还原模拟盘 / 实盘表现:逐笔收益、回撤、观察期与 watchdog 统计、执行损耗。
//!
//! 没有逐日市值快照,回撤一律用「投入资金 + 累计已实现盈亏」构成的权益曲线近似;
//! 分母是**投入资金**而不是峰值利润,这样才与回测端 `metrics.rs::max_drawdown`
//! (`1 − equity/peak_equity`,而前推回测的 `initial_cash = 0`、组合按需注资,
//! 其 equity 就是实际投入的资金)同口径。

use crate::event::Direction;
use crate::trade::admission::judge::{PaperStats, WatchdogStats};
use crate::trade::model::{fmt_ts, parse_side, parse_ts, Account};
use anyhow::Result;
use chrono::{Datelike, NaiveDate, NaiveDateTime, Weekday};
use rusqlite::{params, Connection, Row};
use std::collections::BTreeMap;

/// 一条成交记录(已按策略过滤)。一张工单可能分多批成交,对应多条 `FillRow`——
/// 逻辑交易的粒度见 `TradeUnit` / `trade_units`。
#[derive(Debug, Clone, PartialEq)]
pub struct FillRow {
    pub account: Account,
    /// 所属工单;同一工单的多批成交合并成一笔逻辑交易(见 `trade_units`)。
    pub ticket_id: i64,
    pub code: String,
    pub side: Direction,
    pub price: f64,
    pub qty: u64,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
    pub filled_at: NaiveDateTime,
    pub signal_id: i64,
}

/// `strategy_fills` 的一行原始列(账户 / 代码 / 方向 / 时间仍是字符串,稍后解析)。
struct RawFill {
    account: String,
    code: String,
    side: String,
    price: f64,
    qty: i64,
    fee: f64,
    realized_pnl: Option<f64>,
    filled_at: String,
    signal_id: i64,
    ticket_id: i64,
}

fn read_fill(r: &Row) -> rusqlite::Result<RawFill> {
    Ok(RawFill {
        account: r.get(0)?,
        code: r.get(1)?,
        side: r.get(2)?,
        price: r.get(3)?,
        qty: r.get(4)?,
        fee: r.get(5)?,
        realized_pnl: r.get(6)?,
        filled_at: r.get(7)?,
        signal_id: r.get(8)?,
        ticket_id: r.get(9)?,
    })
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
        "SELECT f.account, f.code, f.side, f.price, f.qty, f.fee, f.realized_pnl, f.filled_at,
                t.signal_id, f.ticket_id
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
        .map(|r| {
            Ok(FillRow {
                account: Account::parse(&r.account)?,
                ticket_id: r.ticket_id,
                code: r.code,
                side: parse_side(&r.side)?,
                price: r.price,
                qty: r.qty.max(0) as u64,
                fee: r.fee,
                realized_pnl: r.realized_pnl,
                filled_at: parse_ts(&r.filled_at)?,
                signal_id: r.signal_id,
            })
        })
        .collect()
}

/// 一次逻辑交易:同一工单的多笔分批成交合并。实盘一张工单可能分三次成交,
/// 回测一次卖出只算一笔;不合并会把 1 次亏损数成 3 连亏、把笔数灌水三倍。
#[derive(Debug, Clone, PartialEq)]
pub struct TradeUnit {
    pub code: String,
    pub side: Direction,
    /// 建仓成本:`Σ(price × qty) − Σ realized_pnl − Σ fee`,与 `ticket.rs` 的
    /// `avg_cost`(买入费用已在成本内)同口径。
    pub cost: f64,
    pub realized_pnl: Option<f64>,
    /// **最后**一笔分批成交的时间;而向量本身按**首**笔出现排序,所以逻辑交易的先后
    /// 与实际了结先后可能略有出入(分批成交交错时),对笔数 / 连亏 / 胜率均无影响。
    pub at: NaiveDateTime,
}

/// 按 `ticket_id` 合并成交,保持首次出现的时间序(`fills` 须已按时间/主键排序)。
pub fn trade_units(fills: &[FillRow]) -> Vec<TradeUnit> {
    let mut seen: BTreeMap<i64, usize> = BTreeMap::new();
    let mut out: Vec<TradeUnit> = Vec::new();
    for f in fills {
        // 卖出的成本靠 realized 反推;`realized_pnl` 为 NULL 的分批(未来 qmt 导入可能出现)
        // 没有反推依据,把它的全额毛收入计进成本会把 `trade_returns` 压掉一半,直接跳过。
        let cost = if f.side == Direction::Sell && f.realized_pnl.is_none() {
            0.0
        } else {
            f.price * f.qty as f64 - f.realized_pnl.unwrap_or(0.0) - f.fee
        };
        match seen.get(&f.ticket_id) {
            Some(&i) => {
                let u = &mut out[i];
                u.cost += cost;
                if let Some(r) = f.realized_pnl {
                    u.realized_pnl = Some(u.realized_pnl.unwrap_or(0.0) + r);
                }
                u.at = f.filled_at;
            }
            None => {
                seen.insert(f.ticket_id, out.len());
                out.push(TradeUnit {
                    code: f.code.clone(),
                    side: f.side,
                    cost,
                    realized_pnl: f.realized_pnl,
                    at: f.filled_at,
                });
            }
        }
    }
    out
}

/// 卖出交易的已实现盈亏,按时间序(一张工单一笔,见 `trade_units`)。
pub fn sell_pnls(units: &[TradeUnit]) -> Vec<f64> {
    units
        .iter()
        .filter(|u| u.side == Direction::Sell)
        .filter_map(|u| u.realized_pnl)
        .collect()
}

/// 每笔卖出的收益率:realized / 成本,成本 = 成交额 − realized − 费用。
pub fn trade_returns(units: &[TradeUnit]) -> Vec<f64> {
    units
        .iter()
        .filter(|u| u.side == Direction::Sell)
        .filter_map(|u| {
            let realized = u.realized_pnl?;
            (u.cost > 1e-9).then(|| realized / u.cost)
        })
        .collect()
}

/// 逐笔重建持仓成本,回调 `(截至当前的投入资金**运行峰值**, 累计已实现盈亏)`。
///
/// 每条成交回调**两次**:先是「资金已到位、这笔盈亏尚未实现」的水位,再是实现之后的水位。
/// 少了前一个点,窗口里第一笔就亏损的策略会没有高水位可比(权益曲线只有谷底一个点),
/// 回撤退化成 0.0 —— 而按本次准入截断成交后,这恰恰是常见形态。
///
/// 买入按 `price × qty + fee` 计入成本(与 `ticket.rs` 的 `avg_cost` 同口径),卖出按
/// 比例减少成本。峰值在**减仓之前**取样:卖出当刻那笔仓位的资金仍在场上,是这笔盈亏的分母。
fn walk_deployed(fills: &[FillRow], mut on_step: impl FnMut(f64, f64)) {
    // code → (未平仓股数, 未平仓成本合计)
    let mut open: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    let mut deployed = 0.0f64;
    let mut cum = 0.0f64;
    for f in fills {
        let qty = f.qty as f64;
        {
            let slot = open.entry(f.code.as_str()).or_insert((0.0, 0.0));
            match f.side {
                Direction::Buy => {
                    slot.1 += f.price * qty + f.fee;
                    slot.0 += qty;
                }
                Direction::Sell if slot.1 <= 0.0 => {
                    // 开仓买入落在统计窗口之外(watchdog 按本次准入截断成交)时,窗口里
                    // 只看得到卖出。用流水反推成本 `price × qty − realized − fee` 给持仓
                    // 补票,否则分母为 0、再大的亏损回撤也是 0.0 ——「带仓被暂停、复活后
                    // 割肉」恰恰是 watchdog 最该开火的场景。`realized` 缺失时无从反推。
                    // 只在完全没有在册成本时补:部分在册(持 500 卖 1000)仍按持仓封顶。
                    if let Some(r) = f.realized_pnl {
                        let basis = f.price * qty - r - f.fee;
                        if basis > 0.0 {
                            *slot = (qty, basis);
                        }
                    }
                }
                Direction::Sell => {}
            }
        }
        deployed = deployed.max(open.values().map(|(_, cost)| *cost).sum::<f64>());
        on_step(deployed, cum);
        if f.side == Direction::Sell {
            let slot = open.entry(f.code.as_str()).or_insert((0.0, 0.0));
            // 卖出数量超过持仓时按持仓封顶(实盘可能有本系统之外建的仓)
            let sold = qty.min(slot.0);
            if slot.0 > 0.0 {
                slot.1 -= slot.1 / slot.0 * sold;
                slot.0 -= sold;
            }
            if slot.0 <= 0.0 {
                *slot = (0.0, 0.0);
            }
        }
        cum += f.realized_pnl.unwrap_or(0.0);
        on_step(deployed, cum);
    }
}

/// 该策略投入的资金峰值:各时刻未平仓成本合计的最大值。
///
/// 回测端 `initial_cash = 0`、组合按需注资,其 equity 就是投入资金;实盘要与之
/// 可比,分母必须是投入资金而不是峰值利润(否则回撤被高估约一个数量级)。
/// 注意这是**整个窗口**的全局峰值,只适合展示;回撤请用 `realized_drawdown`,
/// 它用的是逐笔的运行峰值(见 `walk_deployed`)。
pub fn peak_deployed(fills: &[FillRow]) -> f64 {
    let mut peak = 0.0f64;
    walk_deployed(fills, |deployed, _| peak = deployed);
    peak
}

/// 纯函数内核:权益曲线相对高水位的最大回撤,与回测端 `metrics.rs::max_drawdown`
/// 同式(`1 − equity / peak_equity`,`peak_equity ≤ 0` 的点不计)。
pub fn drawdown_of_equity(equity: &[f64]) -> f64 {
    let mut peak = f64::MIN;
    let mut worst = 0.0f64;
    for e in equity {
        peak = peak.max(*e);
        if peak > 0.0 {
            worst = worst.max((peak - e) / peak);
        }
    }
    worst
}

/// 纯函数内核的**资金基数恒定**特例:一次性投入 `capital_base`、之后不再加减仓时的
/// 权益回撤。`capital_base <= 0` 返回 0.0。
///
/// 实际成交流水的基数会随加仓变化,所以 `realized_drawdown` 不走这里,而是逐笔用
/// 当时的投入峰值做基数(见 `walk_deployed`);本函数留作该口径的可读参照。
pub fn equity_drawdown(pnls: &[f64], capital_base: f64) -> f64 {
    if !capital_base.is_finite() || capital_base <= 0.0 {
        return 0.0;
    }
    let mut equity = capital_base;
    let curve: Vec<f64> = std::iter::once(capital_base)
        .chain(pnls.iter().map(|p| {
            equity += p;
            equity
        }))
        .collect();
    drawdown_of_equity(&curve)
}

/// 已实现盈亏曲线相对**权益高水位**的最大回撤,权益 = 截至当笔的投入资金峰值 + 累计盈亏。
/// 与回测的 `max_drawdown` 同口径:两边都是「相对高水位损失掉的资金比例」,且两边的
/// 分母都只用**到当前为止**的信息——事后加仓不得追溯性地稀释已经发生的回撤。
/// 无成交或投入为 0 时返回 0.0。
pub fn realized_drawdown(fills: &[FillRow]) -> f64 {
    let mut curve = Vec::with_capacity(fills.len() * 2);
    walk_deployed(fills, |deployed, cum| curve.push(deployed + cum));
    drawdown_of_equity(&curve)
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

fn max_streak_by_code(units: &[TradeUnit]) -> usize {
    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    let mut cur: BTreeMap<&str, usize> = BTreeMap::new();
    for u in units.iter().filter(|u| u.side == Direction::Sell) {
        let pnl = u.realized_pnl.unwrap_or(0.0);
        let c = cur.entry(u.code.as_str()).or_insert(0);
        if pnl <= 0.0 {
            *c += 1;
            let best = by_code.entry(u.code.as_str()).or_insert(0);
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
    let units = trade_units(&fills);
    let returns = trade_returns(&units);
    let avg = if returns.is_empty() {
        0.0
    } else {
        returns.iter().sum::<f64>() / returns.len() as f64
    };
    Ok(PaperStats {
        days: workdays_between(since.date(), now.date()),
        trades: units.iter().filter(|u| u.side == Direction::Sell).count(),
        avg_trade_return: avg,
        max_drawdown: realized_drawdown(&fills),
    })
}

/// 实盘 watchdog 统计:回撤取窗口内全部实盘成交,胜率取最近 `window` 笔,连亏按代码分组。
///
/// `since` 必须传本次准入(`Admitted`)的时刻:回撤与连亏都是**永不衰减的运行最大值**,
/// 若取全历史,`Suspended → … → Admitted` 复活后的第一次 watchdog 会拿同一份旧证据
/// 再次暂停,被暂停的策略永远无法康复。`None` 退回全历史(老数据没有事件行)。
pub fn watchdog_stats(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    window: usize,
    since: Option<NaiveDateTime>,
) -> Result<WatchdogStats> {
    let fills = strategy_fills(conn, user_id, strategy_id, Account::Real, since)?;
    let units = trade_units(&fills);
    let pnls = sell_pnls(&units);
    let recent: &[f64] = if pnls.len() > window {
        &pnls[pnls.len() - window..]
    } else {
        &pnls
    };
    let wins = recent.iter().filter(|p| **p > 0.0).count();
    Ok(WatchdogStats {
        drawdown: realized_drawdown(&fills),
        recent_win_rate: if recent.is_empty() {
            0.0
        } else {
            wins as f64 / recent.len() as f64
        },
        recent_trades: recent.len(),
        max_consecutive_losses: max_streak_by_code(&units),
    })
}

/// 执行损耗:同一信号下实盘相对模拟盘多付的比例中位数;无配对返回 None。
///
/// 这里**不**按工单合并:比的是同一信号下两侧的**首笔成交价**,分批成交的后续批次
/// 本就不参与比较(见裁决 5)。
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
            ticket_id: signal_id,
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

    /// 一条自定义的买入/卖出流水,用于回撤与投入资金的算术断言。
    fn raw(
        ticket_id: i64,
        side: Direction,
        price: f64,
        qty: u64,
        fee: f64,
        realized: Option<f64>,
    ) -> FillRow {
        FillRow {
            account: Account::Real,
            ticket_id,
            code: "600000".into(),
            side,
            price,
            qty,
            fee,
            realized_pnl: realized,
            filled_at: at(16, 10, 0),
            signal_id: ticket_id,
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
        let units = trade_units(std::slice::from_ref(&sell));
        let r = trade_returns(&units);
        assert_eq!(r.len(), 1);
        assert!((r[0] - 984.29 / 10_005.1).abs() < 1e-9, "{}", r[0]);
        assert_eq!(sell_pnls(&units), vec![984.29]);
        let buy = FillRow {
            side: Direction::Buy,
            realized_pnl: None,
            ..sell
        };
        let buy_units = trade_units(std::slice::from_ref(&buy));
        assert!(trade_returns(&buy_units).is_empty(), "买入不计入");
        assert!(sell_pnls(&buy_units).is_empty());
    }

    /// 裁决 5:一张工单分三次成交,只能算**一笔**逻辑交易。
    #[test]
    fn partial_fills_of_one_ticket_merge_into_one_trade() {
        // 同一工单三次卖出,各 realized −10:合并后 1 笔亏损(不是 3 笔,更不是 3 连亏)
        let batched: Vec<FillRow> = (0..3)
            .map(|i| FillRow {
                filled_at: at(16, 10, i),
                ..raw(7, Direction::Sell, 11.0, 1000, 10.61, Some(-10.0))
            })
            .collect();
        let units = trade_units(&batched);
        assert_eq!(units.len(), 1, "同一工单合并成一笔");
        assert_eq!(units[0].realized_pnl, Some(-30.0));
        assert_eq!(units[0].at, at(16, 10, 2), "时间取最后一笔");
        // 成本 = Σ(11000) − Σ(−10) − Σ(10.61) = 33000 + 30 − 31.83 = 32998.17
        assert!(
            (units[0].cost - 32_998.17).abs() < 1e-9,
            "{}",
            units[0].cost
        );
        assert_eq!(sell_pnls(&units), vec![-30.0]);
        assert_eq!(trade_returns(&units).len(), 1);
        assert!((trade_returns(&units)[0] - (-30.0 / 32_998.17)).abs() < 1e-12);
        assert_eq!(max_streak_by_code(&units), 1, "一次亏损卖出不是 3 连亏");

        // 对照:三张不同工单才是 3 笔、3 连亏
        let separate: Vec<FillRow> = (0..3)
            .map(|i| raw(10 + i, Direction::Sell, 11.0, 1000, 10.61, Some(-10.0)))
            .collect();
        let sep_units = trade_units(&separate);
        assert_eq!(sep_units.len(), 3);
        assert_eq!(max_streak_by_code(&sep_units), 3);
    }

    /// 裁决 1:回撤的分母是投入资金(与回测 `metrics.rs::max_drawdown` 同口径),
    /// 不是峰值利润。原 `drawdown_of_cumulative_pnl` 的两条意图(峰值守卫、空输入)保留。
    #[test]
    fn drawdown_is_measured_against_deployed_capital() {
        // 权益 10000 → 10100 → 10060 → 10160 → 10110:最深回撤 = 50 / 10160
        let dd = equity_drawdown(&[100.0, -40.0, 100.0, -50.0], 10_000.0);
        assert!((dd - 50.0 / 10_160.0).abs() < 1e-12, "{dd}");
        assert!((dd - 0.004_921_259).abs() < 1e-8, "{dd}");
        // 只亏不赚:旧口径(分母为峰值利润)恒返 0.0,恰在最该开火时失效
        let loss = equity_drawdown(&[-100.0, -100.0, -100.0], 10_000.0);
        assert!((loss - 0.03).abs() < 1e-12, "{loss}");
        assert_eq!(equity_drawdown(&[], 10_000.0), 0.0, "无成交无回撤");
        assert_eq!(equity_drawdown(&[10.0, 20.0], 10_000.0), 0.0, "只涨不回撤");
        assert_eq!(equity_drawdown(&[-1.0], 0.0), 0.0, "资金基数非正不计回撤");
        assert_eq!(equity_drawdown(&[-1.0], -5.0), 0.0, "资金基数为负不计回撤");
    }

    /// 裁决 1:投入资金峰值 = 各时刻未平仓成本合计的最大值(买入费用计入成本)。
    #[test]
    fn peak_deployed_tracks_open_position_cost() {
        let buy1 = raw(1, Direction::Buy, 10.0, 1000, 5.0, None);
        let buy2 = raw(2, Direction::Buy, 12.0, 1000, 6.0, None);
        let sell = raw(3, Direction::Sell, 11.0, 1000, 7.0, Some(500.0));
        // 买 1000@10(费 5)→ 10 005
        assert!((peak_deployed(std::slice::from_ref(&buy1)) - 10_005.0).abs() < 1e-9);
        // 再买 1000@12(费 6)→ 22 011
        let two = vec![buy1.clone(), buy2.clone()];
        assert!((peak_deployed(&two) - 22_011.0).abs() < 1e-9);
        // 卖 1000 后剩余成本按比例减半 = 11 005.5,峰值仍是 22 011
        let three = vec![buy1.clone(), buy2.clone(), sell.clone()];
        assert!((peak_deployed(&three) - 22_011.0).abs() < 1e-9);
        // 再买 1000@20(免费)→ 11 005.5 + 20 000 = 31 005.5,这钉住了上面的余额
        let four = vec![
            buy1.clone(),
            buy2,
            sell,
            raw(4, Direction::Buy, 20.0, 1000, 0.0, None),
        ];
        assert!(
            (peak_deployed(&four) - 31_005.5).abs() < 1e-9,
            "{}",
            peak_deployed(&four)
        );
        // 卖出数量超过持仓时按持仓封顶,成本归零
        let over = vec![buy1, raw(5, Direction::Sell, 11.0, 5000, 7.0, Some(1.0))];
        assert!((peak_deployed(&over) - 10_005.0).abs() < 1e-9);
        assert_eq!(peak_deployed(&[]), 0.0);
    }

    /// 裁决 1:`realized_drawdown` 把两者串起来——连亏不再是 0。
    #[test]
    fn realized_drawdown_divides_losses_by_deployed_capital() {
        let fills = vec![
            raw(1, Direction::Buy, 10.0, 1000, 10.0, None),
            raw(2, Direction::Sell, 9.7, 1000, 0.0, Some(-300.0)),
        ];
        // 投入 10 010,亏 300 → 300 / 10 010 ≈ 0.029 97
        let dd = realized_drawdown(&fills);
        assert!((dd - 300.0 / 10_010.0).abs() < 1e-12, "{dd}");
        assert_eq!(realized_drawdown(&[]), 0.0);
    }

    /// 修复轮 2 · 发现 1:分母必须是**当时**的投入资金峰值,不能用整个窗口的全局峰值——
    /// 否则事后加仓会追溯性地缩小早期的回撤(准入初期正在加仓,watchdog 恰好被解除武装)。
    #[test]
    fn drawdown_does_not_shrink_when_capital_ramps_up_later() {
        // 投入 10 000 后割肉 4 000 → 权益 10 000 → 6 000,回撤 0.40
        let base = vec![
            raw(1, Direction::Buy, 10.0, 1000, 0.0, None),
            raw(2, Direction::Sell, 6.0, 1000, 0.0, Some(-4000.0)),
        ];
        assert!((realized_drawdown(&base) - 0.4).abs() < 1e-12);
        // 之后再投入 100 000:权益 6 000 → 106 000(注资抬高水位),但那一刻的回撤已成事实
        let mut ramped = base.clone();
        ramped.push(raw(3, Direction::Buy, 10.0, 10_000, 0.0, None));
        assert!(
            (realized_drawdown(&ramped) - 0.4).abs() < 1e-12,
            "事后加仓不得把已发生的回撤稀释成 {}",
            realized_drawdown(&ramped)
        );
        // 全局峰值口径下会退化成 4000 / 100 000 = 0.04,差 10 倍
        assert!((peak_deployed(&ramped) - 100_000.0).abs() < 1e-9);
    }

    /// 修复轮 2 · 发现 2:开仓买入落在统计窗口之外时(watchdog 按本次准入截断成交),
    /// 必须用卖出流水反推持仓成本,否则分母为 0、再大的亏损回撤也是 0.0。
    #[test]
    fn sell_without_an_in_window_buy_uses_the_implied_basis() {
        // 只看得到一笔卖出:反推成本 = 6 × 1000 − (−4 000) − 0 = 10 000 → 回撤 0.40
        let orphan = vec![raw(1, Direction::Sell, 6.0, 1000, 0.0, Some(-4000.0))];
        assert!((peak_deployed(&orphan) - 10_000.0).abs() < 1e-9);
        let dd = realized_drawdown(&orphan);
        assert!((dd - 0.4).abs() < 1e-12, "{dd}");
        // realized 缺失时无从反推,仍然是 0(没有可用的分母)
        let unknown = vec![raw(1, Direction::Sell, 6.0, 1000, 0.0, None)];
        assert_eq!(peak_deployed(&unknown), 0.0);
        assert_eq!(realized_drawdown(&unknown), 0.0);
        // 已有在册成本时不补票:持 1000 却卖 5000 仍按持仓封顶(见 peak_deployed 测试)
        let partial = vec![
            raw(1, Direction::Buy, 10.0, 1000, 5.0, None),
            raw(2, Direction::Sell, 11.0, 5000, 7.0, Some(1.0)),
        ];
        assert!((peak_deployed(&partial) - 10_005.0).abs() < 1e-9);
    }

    /// 修复轮 2 · 发现 3:同一工单里 `realized_pnl` 为 NULL 的分批没有反推依据,
    /// 把它的全额毛收入计进成本会把 `trade_returns` 压掉一半。
    #[test]
    fn trade_unit_cost_skips_sell_fills_without_realized_pnl() {
        let fills = vec![
            raw(7, Direction::Sell, 11.0, 1000, 10.61, Some(-10.0)),
            FillRow {
                filled_at: at(16, 10, 1),
                ..raw(7, Direction::Sell, 11.0, 1000, 10.61, None)
            },
        ];
        let units = trade_units(&fills);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].realized_pnl, Some(-10.0));
        // 成本只算有 realized 的那一批:11 000 + 10 − 10.61 = 10 999.39(不是 21 990 上下)
        assert!(
            (units[0].cost - 10_999.39).abs() < 1e-9,
            "{}",
            units[0].cost
        );
        assert!((trade_returns(&units)[0] - (-10.0 / 10_999.39)).abs() < 1e-12);
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
        // 三轮「买 1000 @10(费 10.61)→ 卖 1000 @11」,每轮投入 10 010.61(回撤的分母)
        for (i, pnl) in [100.0, -50.0, 20.0].iter().enumerate() {
            seed(
                &mut c,
                1,
                Some(s),
                Account::Paper,
                Direction::Buy,
                10.0,
                None,
                &format!("pb{i}"),
                at(16, 9, 30 + i as u32),
            );
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
        assert_eq!(ps.trades, 3, "买入不算交易笔数");
        assert_eq!(ps.days, 3, "9-14 周一 到 9-16 周三");
        // 投入峰值恒为 10 010.61(每轮先平后建),权益 → 10 110.61(峰值)→ 10 060.61
        // → 10 080.61,最深回撤 50 / 10 110.61
        assert!(
            (ps.max_drawdown - 50.0 / 10_110.61).abs() < 1e-12,
            "{}",
            ps.max_drawdown
        );

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
        let ws = watchdog_stats(&c, 1, s, 20, None).unwrap();
        assert_eq!((ws.recent_trades, ws.max_consecutive_losses), (2, 2));
        assert_eq!(ws.recent_win_rate, 0.0);
    }

    /// 裁决 2:watchdog 的窗口必须能从本次准入起算,否则被暂停的策略永远无法康复
    /// (回撤与连亏都是永不衰减的运行最大值)。
    #[test]
    fn watchdog_stats_only_count_fills_since_the_given_moment() {
        let mut c = db();
        let s = strategy(&c, 1);
        // 旧时代:建仓 + 两笔亏损
        seed(
            &mut c,
            1,
            Some(s),
            Account::Real,
            Direction::Buy,
            10.0,
            None,
            "ob",
            at(14, 9, 30),
        );
        for (i, pnl) in [-2000.0, -1500.0].iter().enumerate() {
            seed(
                &mut c,
                1,
                Some(s),
                Account::Real,
                Direction::Sell,
                8.0,
                Some(*pnl),
                &format!("o{i}"),
                at(14, 10, i as u32),
            );
        }
        let all = watchdog_stats(&c, 1, s, 20, None).unwrap();
        assert_eq!(all.recent_trades, 2);
        assert!(all.drawdown > 0.3, "全历史回撤很深:{}", all.drawdown);

        // 复活之后:重新建仓 + 一笔小赚
        seed(
            &mut c,
            1,
            Some(s),
            Account::Real,
            Direction::Buy,
            10.0,
            None,
            "nb",
            at(16, 9, 30),
        );
        seed(
            &mut c,
            1,
            Some(s),
            Account::Real,
            Direction::Sell,
            11.0,
            Some(100.0),
            "n0",
            at(16, 10, 0),
        );
        let fresh = watchdog_stats(&c, 1, s, 20, Some(at(16, 0, 0))).unwrap();
        assert_eq!(fresh.recent_trades, 1, "只数转换之后的成交");
        assert_eq!(fresh.recent_win_rate, 1.0);
        assert_eq!(fresh.max_consecutive_losses, 0, "旧连亏不再计入");
        assert_eq!(fresh.drawdown, 0.0, "旧回撤不再计入");
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

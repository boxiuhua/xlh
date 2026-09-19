//! 交易日报:汇总当日工单、成交、被拦截的信号、当前持仓与策略状态变化。
//! 只陈述系统记录的统计,不给出任何买卖建议(设计裁决 2,spec §15 合规)。

use crate::event::Direction;
use crate::trade::gate::GateReject;
use crate::trade::model::{parse_side, Account, StrategyStatus, TicketStatus, DATE_FMT};
use crate::trade::store;
use crate::trade::ticket;
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection};

/// 当日创建的实盘工单,按「当前状态」计数(不是按当日经历过的状态变化次数)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TicketCounts {
    pub created: usize,
    pub confirmed: usize,
    pub filled: usize,
    pub expired: usize,
    pub ignored: usize,
    pub cancelled: usize,
    pub open: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillLine {
    pub account: Account,
    pub code: String,
    pub name: Option<String>,
    pub side: Direction,
    pub qty: u64,
    pub price: f64,
    pub fee: f64,
    pub realized_pnl: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct HoldingSummary {
    pub positions: usize,
    pub market_value: f64,
    pub cost: f64,
    pub unpriced: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StrategyChange {
    pub strategy_id: i64,
    pub name: String,
    pub from: StrategyStatus,
    pub to: StrategyStatus,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DailyReport {
    pub user_id: i64,
    pub date: NaiveDate,
    /// 当日创建的实盘工单按「当前状态」计数
    pub real_tickets: TicketCounts,
    /// 当日成交(实盘在前,各自按时间)
    pub fills: Vec<FillLine>,
    pub realized_real: f64,
    pub realized_paper: f64,
    /// 当日被拦截信号:原因中文(`GateReject::label_zh`,未知原样)× 次数,按次数降序
    pub rejected: Vec<(String, usize)>,
    pub real_holdings: HoldingSummary,
    pub paper_holdings: HoldingSummary,
    pub strategy_changes: Vec<StrategyChange>,
}

fn ticket_counts(conn: &Connection, user_id: i64, day: &str) -> Result<TicketCounts> {
    let mut counts = TicketCounts::default();
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) FROM trade_tickets
         WHERE user_id = ?1 AND account = 'real' AND substr(created_at, 1, 10) = ?2
         GROUP BY status",
    )?;
    let rows = stmt
        .query_map(params![user_id, day], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (status, n) in rows {
        let n = n as usize;
        counts.created += n;
        match TicketStatus::parse(&status)? {
            TicketStatus::Pending => counts.open += n,
            TicketStatus::Confirmed | TicketStatus::Partial => counts.confirmed += n,
            TicketStatus::Filled => counts.filled += n,
            TicketStatus::Expired => counts.expired += n,
            TicketStatus::Rejected => counts.ignored += n,
            TicketStatus::Cancelled => counts.cancelled += n,
        }
    }
    Ok(counts)
}

#[allow(clippy::type_complexity)]
fn fill_lines(conn: &Connection, user_id: i64, day: &str) -> Result<Vec<FillLine>> {
    let mut stmt = conn.prepare(
        "SELECT f.account, f.code, s.name, f.side, f.qty, f.price, f.fee, f.realized_pnl
         FROM trade_fills f
         JOIN trade_tickets t ON t.id = f.ticket_id
         JOIN trade_signals s ON s.id = t.signal_id
         WHERE f.user_id = ?1 AND substr(f.filled_at, 1, 10) = ?2
         ORDER BY (f.account <> 'real'), f.filled_at, f.id",
    )?;
    let raws = stmt
        .query_map(params![user_id, day], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, f64>(6)?,
                r.get::<_, Option<f64>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter()
        .map(
            |(account, code, name, side, qty, price, fee, realized_pnl)| {
                Ok(FillLine {
                    account: Account::parse(&account)?,
                    code,
                    name,
                    side: parse_side(&side)?,
                    qty: qty.max(0) as u64,
                    price,
                    fee,
                    realized_pnl,
                })
            },
        )
        .collect()
}

/// 拦截原因文案:已知原因用 `GateReject::label_zh`,未识别的原样保留。
fn reject_label(raw: &str) -> String {
    GateReject::ALL
        .into_iter()
        .find(|r| r.as_str() == raw)
        .map(|r| r.label_zh().to_string())
        .unwrap_or_else(|| raw.to_string())
}

/// 当日被拦截信号,按文案分组计数,次数降序(同次数按文案排序,保证稳定输出)。
fn rejected_counts(conn: &Connection, user_id: i64, day: &str) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare(
        "SELECT reject_reason FROM trade_signals
         WHERE user_id = ?1 AND status = 'rejected' AND substr(created_at, 1, 10) = ?2",
    )?;
    let raws = stmt
        .query_map(params![user_id, day], |r| r.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for raw in raws.into_iter().flatten() {
        *counts.entry(reject_label(&raw)).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(v)
}

/// 当前持仓汇总(非当日快照):无报价的持仓按成本估值市值,并计入 `unpriced`。
fn holding_summary(conn: &Connection, user_id: i64, account: Account) -> Result<HoldingSummary> {
    let mut s = HoldingSummary::default();
    for p in store::list_positions(conn, user_id, account)? {
        s.positions += 1;
        s.cost += p.qty as f64 * p.avg_cost;
        match store::get_quote(conn, &p.code)? {
            Some(q) => s.market_value += p.qty as f64 * q.price,
            None => {
                s.market_value += p.qty as f64 * p.avg_cost;
                s.unpriced += 1;
            }
        }
    }
    Ok(s)
}

fn strategy_changes(conn: &Connection, user_id: i64, day: &str) -> Result<Vec<StrategyChange>> {
    let mut stmt = conn.prepare(
        "SELECT e.strategy_id, s.name, e.from_status, e.to_status, e.reason
         FROM trade_strategy_events e
         JOIN trade_strategies s ON s.id = e.strategy_id
         WHERE s.user_id = ?1 AND substr(e.at, 1, 10) = ?2
         ORDER BY e.id",
    )?;
    let raws = stmt
        .query_map(params![user_id, day], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter()
        .map(|(strategy_id, name, from, to, reason)| {
            Ok(StrategyChange {
                strategy_id,
                name,
                from: StrategyStatus::parse(&from)?,
                to: StrategyStatus::parse(&to)?,
                reason,
            })
        })
        .collect()
}

/// 汇总某用户某日的交易活动。当日无工单、无成交、无被拦截信号、无策略状态变化,
/// 且实盘与模拟盘都无持仓时返回 `None`(设计裁决 1:没有活动就不刷屏)。
pub fn build_daily_report(
    conn: &Connection,
    user_id: i64,
    date: NaiveDate,
) -> Result<Option<DailyReport>> {
    let day = date.format(DATE_FMT).to_string();
    let real_tickets = ticket_counts(conn, user_id, &day)?;
    let fills = fill_lines(conn, user_id, &day)?;
    let realized_real = ticket::realized_pnl_on(conn, user_id, Account::Real, date)?;
    let realized_paper = ticket::realized_pnl_on(conn, user_id, Account::Paper, date)?;
    let rejected = rejected_counts(conn, user_id, &day)?;
    let real_holdings = holding_summary(conn, user_id, Account::Real)?;
    let paper_holdings = holding_summary(conn, user_id, Account::Paper)?;
    let strategy_changes = strategy_changes(conn, user_id, &day)?;

    if real_tickets.created == 0
        && fills.is_empty()
        && rejected.is_empty()
        && strategy_changes.is_empty()
        && real_holdings.positions == 0
        && paper_holdings.positions == 0
    {
        return Ok(None);
    }

    Ok(Some(DailyReport {
        user_id,
        date,
        real_tickets,
        fills,
        realized_real,
        realized_paper,
        rejected,
        real_holdings,
        paper_holdings,
        strategy_changes,
    }))
}

fn account_label(a: Account) -> &'static str {
    match a {
        Account::Real => "实盘",
        Account::Paper => "模拟盘",
    }
}

fn side_label(s: Direction) -> &'static str {
    match s {
        Direction::Buy => "买入",
        Direction::Sell => "卖出",
    }
}

fn push_holding_line(md: &mut String, label: &str, h: &HoldingSummary) {
    if h.positions == 0 {
        return;
    }
    md.push_str(&format!(
        "- {label}:{} 只,市值 {:.2},成本 {:.2},浮动盈亏 {:+.2}",
        h.positions,
        h.market_value,
        h.cost,
        h.market_value - h.cost
    ));
    if h.unpriced > 0 {
        md.push_str(&format!("(其中 {} 只无报价,按成本估值)", h.unpriced));
    }
    md.push('\n');
}

/// 模拟盘成交最多列出的笔数(实盘成交不设上限)。
const MAX_PAPER_FILL_LINES: usize = 10;
/// 被拦截信号最多列出的原因种类数。
const MAX_REJECT_LINES: usize = 5;
/// 策略状态变化最多列出的条数。
const MAX_STRATEGY_CHANGE_LINES: usize = 10;

fn fill_line(f: &FillLine) -> String {
    let mut line = format!(
        "- {} {} {}",
        account_label(f.account),
        side_label(f.side),
        f.code
    );
    if let Some(name) = &f.name {
        line.push(' ');
        line.push_str(name);
    }
    line.push_str(&format!(" {} 股 @ {:.2}(费 {:.2})", f.qty, f.price, f.fee));
    if let Some(pnl) = f.realized_pnl {
        line.push_str(&format!(",已实现盈亏 {pnl:+.2}"));
    }
    line
}

/// 渲染为 (标题, Markdown 正文)。只陈述系统记录的统计事实,不出现任何
/// 「建议买入 / 建议卖出」措辞(设计裁决 2,spec §15 合规);没有内容的节省略。
pub fn render_daily_report(r: &DailyReport) -> (String, String) {
    let title = format!("交易日报 {}", r.date.format(DATE_FMT));
    let mut md = format!("### {title}\n\n");

    let c = &r.real_tickets;
    if c.created > 0 {
        md.push_str(&format!(
            "#### 今日工单\n\n- 实盘工单共 {} 张:待确认 {}、待成交(含部分成交) {}、已成交 {}、已过期 {}、已拒绝 {}、已取消 {}\n\n",
            c.created, c.open, c.confirmed, c.filled, c.expired, c.ignored, c.cancelled
        ));
    }

    if !r.fills.is_empty() {
        md.push_str("#### 今日成交\n\n");
        // 实盘成交全部列出(需要线下核对);模拟盘成交可能很多,只列前若干笔,
        // 余下给出笔数——正文过长会被推送渠道(如企业微信 markdown 4096 字节)拒收。
        let mut paper_listed = 0;
        let mut paper_omitted = 0;
        for f in &r.fills {
            if f.account == Account::Paper {
                if paper_listed >= MAX_PAPER_FILL_LINES {
                    paper_omitted += 1;
                    continue;
                }
                paper_listed += 1;
            }
            md.push_str(&fill_line(f));
            md.push('\n');
        }
        if paper_omitted > 0 {
            md.push_str(&format!("- 另 {paper_omitted} 笔模拟盘成交(详见交易页)\n"));
        }
        md.push('\n');
    }

    if r.realized_real != 0.0 || r.realized_paper != 0.0 {
        md.push_str("#### 已实现盈亏\n\n");
        if r.realized_real != 0.0 {
            md.push_str(&format!("- 实盘:{:+.2}\n", r.realized_real));
        }
        if r.realized_paper != 0.0 {
            md.push_str(&format!("- 模拟盘:{:+.2}\n", r.realized_paper));
        }
        md.push('\n');
    }

    if !r.rejected.is_empty() {
        md.push_str("#### 被拦截的信号\n\n");
        for (label, n) in r.rejected.iter().take(MAX_REJECT_LINES) {
            md.push_str(&format!("- {label} × {n}\n"));
        }
        let rest = &r.rejected[r.rejected.len().min(MAX_REJECT_LINES)..];
        if !rest.is_empty() {
            let times: usize = rest.iter().map(|(_, n)| n).sum();
            md.push_str(&format!("- 其它 {} 类 × {times}\n", rest.len()));
        }
        md.push('\n');
    }

    if r.real_holdings.positions > 0 || r.paper_holdings.positions > 0 {
        md.push_str("#### 持仓\n\n");
        push_holding_line(&mut md, "实盘", &r.real_holdings);
        push_holding_line(&mut md, "模拟盘", &r.paper_holdings);
        md.push('\n');
    }

    if !r.strategy_changes.is_empty() {
        md.push_str("#### 策略状态变化\n\n");
        for ch in r.strategy_changes.iter().take(MAX_STRATEGY_CHANGE_LINES) {
            md.push_str(&format!(
                "- {}(#{}):{} → {},原因:{}\n",
                ch.name,
                ch.strategy_id,
                ch.from.label_zh(),
                ch.to.label_zh(),
                ch.reason
            ));
        }
        let rest = r
            .strategy_changes
            .len()
            .saturating_sub(MAX_STRATEGY_CHANGE_LINES);
        if rest > 0 {
            md.push_str(&format!("- 另 {rest} 条策略状态变化(详见交易页)\n"));
        }
        md.push('\n');
    }

    md.push_str("> 以上为系统记录的统计,不构成投资建议。\n");
    (title, md)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{
        fmt_ts, AccountScope, NewSignal, NewStrategy, Position, Quote, SignalSource,
    };
    use crate::trade::service::{self, SubmitContext, SubmitOutcome};
    use crate::trade::ticket::{NewTicket, Transition};
    use chrono::NaiveDateTime;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        day(d).and_hms_opt(h, m, 0).unwrap()
    }

    fn signal(user_id: i64, code: &str, side: Direction, key: &str) -> NewSignal {
        NewSignal {
            user_id,
            source: SignalSource::Manual,
            strategy_id: None,
            code: code.into(),
            name: None,
            side,
            scope: AccountScope::Both,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        }
    }

    fn named_signal(user_id: i64, code: &str, side: Direction, key: &str, name: &str) -> NewSignal {
        NewSignal {
            name: Some(name.into()),
            ..signal(user_id, code, side, key)
        }
    }

    fn new_real_ticket(
        user_id: i64,
        signal_id: i64,
        code: &str,
        side: Direction,
        qty: u64,
        status: TicketStatus,
        now: NaiveDateTime,
    ) -> NewTicket {
        NewTicket {
            user_id,
            signal_id,
            account: Account::Real,
            code: code.into(),
            side,
            suggest_price: 10.0,
            qty,
            expires_at: now + chrono::Duration::minutes(30),
            deviation_th: 0.015,
            status,
            urgency: 0,
            created_at: now,
        }
    }

    #[test]
    fn quiet_user_without_positions_gets_no_report() {
        let c = db();
        assert_eq!(build_daily_report(&c, 1, day(23)).unwrap(), None);
    }

    #[test]
    fn report_counts_tickets_fills_pnl_rejections_holdings_and_strategy_changes() {
        let mut c = db();
        let today = day(23);

        store::set_capital(&c, 1, Account::Real, 200_000.0, at(20, 9, 0)).unwrap();

        // 持仓:1 只有报价、1 只无报价
        let mut p1 = Position::empty(1, Account::Real, "600010");
        p1.qty = 100;
        p1.avg_cost = 10.0;
        store::upsert_position(&c, &p1, at(20, 9, 5)).unwrap();
        let mut p2 = Position::empty(1, Account::Real, "600011");
        p2.qty = 50;
        p2.avg_cost = 20.0;
        store::upsert_position(&c, &p2, at(20, 9, 5)).unwrap();
        store::upsert_quotes(
            &c,
            &[Quote {
                code: "600010".into(),
                price: 12.0,
                limit_up: None,
                limit_down: None,
                ts: at(23, 9, 0),
            }],
            at(23, 9, 0),
        )
        .unwrap();

        // 策略:草稿 → 观察期(昨日之前,不计入)→ 已准入(今日)
        let strategy_id = store::create_strategy(
            &c,
            &NewStrategy {
                user_id: 1,
                name: "策略A".into(),
                kind: "dca".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600010".into()],
            },
            at(20, 9, 0),
        )
        .unwrap();
        state::update_status(
            &c,
            1,
            strategy_id,
            StrategyStatus::Draft,
            StrategyStatus::Paper,
            "进入观察期",
            at(20, 9, 1),
        )
        .unwrap();
        state::update_status(
            &c,
            1,
            strategy_id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "准入",
            at(23, 11, 0),
        )
        .unwrap();

        // 前一日:1 张工单与 1 笔成交(不应计入)
        let sid_prev = ticket::insert_signal(
            &c,
            &signal(1, "600099", Direction::Buy, "prev"),
            at(22, 10, 0),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid_prev, "ticketed", None).unwrap();
        let tid_prev = ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid_prev,
                "600099",
                Direction::Buy,
                100,
                TicketStatus::Filled,
                at(22, 10, 0),
            ),
        )
        .unwrap();
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, 1, 'real', '600099', 'buy', 10.0, 100, 5.0, NULL, 'manual', ?2)",
            params![tid_prev, fmt_ts(at(22, 10, 5))],
        )
        .unwrap();

        // 今日:3 张实盘工单 —— 1 张确认并回填买入、1 张忽略、1 张仍 pending
        let sid_a = ticket::insert_signal(
            &c,
            &named_signal(1, "600001", Direction::Buy, "a", "浦发银行"),
            at(23, 9, 31),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid_a, "ticketed", None).unwrap();
        let tid_a = ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid_a,
                "600001",
                Direction::Buy,
                1000,
                TicketStatus::Pending,
                at(23, 9, 31),
            ),
        )
        .unwrap();
        assert_eq!(
            ticket::confirm(&c, 1, tid_a, at(23, 9, 32)).unwrap(),
            Transition::Applied
        );
        let fill_a =
            ticket::record_fill(&mut c, 1, tid_a, 10.0, 1000, "manual", at(23, 9, 33)).unwrap();
        assert_eq!(fill_a.status, TicketStatus::Filled);
        store::delete_position(&c, 1, Account::Real, "600001").unwrap();

        let sid_b =
            ticket::insert_signal(&c, &signal(1, "600002", Direction::Buy, "b"), at(23, 9, 34))
                .unwrap()
                .unwrap();
        ticket::mark_signal(&c, sid_b, "ticketed", None).unwrap();
        let tid_b = ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid_b,
                "600002",
                Direction::Buy,
                500,
                TicketStatus::Pending,
                at(23, 9, 34),
            ),
        )
        .unwrap();
        assert_eq!(
            ticket::ignore(&c, 1, tid_b, "不看好").unwrap(),
            Transition::Applied
        );

        let sid_c =
            ticket::insert_signal(&c, &signal(1, "600003", Direction::Buy, "c"), at(23, 9, 36))
                .unwrap()
                .unwrap();
        ticket::mark_signal(&c, sid_c, "ticketed", None).unwrap();
        ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid_c,
                "600003",
                Direction::Buy,
                300,
                TicketStatus::Pending,
                at(23, 9, 36),
            ),
        )
        .unwrap();

        // 今日:1 笔实盘卖出成交,已实现盈亏 +200(工单本身创建于更早,今日只是回填)
        let sid_d = ticket::insert_signal(
            &c,
            &named_signal(1, "600004", Direction::Sell, "d", "兆易创新"),
            at(20, 9, 50),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid_d, "ticketed", None).unwrap();
        let tid_d = ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid_d,
                "600004",
                Direction::Sell,
                1000,
                TicketStatus::Filled,
                at(20, 9, 55),
            ),
        )
        .unwrap();
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, 1, 'real', '600004', 'sell', 12.0, 1000, 5.0, 200.0, 'manual', ?2)",
            params![tid_d, fmt_ts(at(23, 10, 0))],
        )
        .unwrap();

        // 今日:1 个被拦截信号(cooldown)——通过 service::submit_signal 触发真实闸门拒绝
        let dummy = signal(1, "600050", Direction::Buy, "cool-setup");
        let dummy_id = ticket::insert_signal(&c, &dummy, at(23, 9, 0))
            .unwrap()
            .unwrap();
        ticket::mark_signal(&c, dummy_id, "ticketed", None).unwrap();
        let cooled = signal(1, "600050", Direction::Buy, "cool-actual");
        let ctx = SubmitContext {
            quote: None,
            now: at(23, 9, 20),
        };
        match service::submit_signal(&mut c, &cooled, &ctx).unwrap() {
            SubmitOutcome::Rejected {
                reason: GateReject::Cooldown,
                ..
            } => {}
            other => panic!("期望冷却拒绝,实际 {other:?}"),
        }

        let report = build_daily_report(&c, 1, today).unwrap().unwrap();

        assert_eq!(
            report.real_tickets,
            TicketCounts {
                created: 3,
                confirmed: 0,
                filled: 1,
                expired: 0,
                ignored: 1,
                cancelled: 0,
                open: 1,
            }
        );

        assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
        assert_eq!(report.fills[0].account, Account::Real);
        assert_eq!(report.fills[0].code, "600001");
        assert_eq!(report.fills[0].name.as_deref(), Some("浦发银行"));
        assert_eq!(report.fills[0].side, Direction::Buy);
        assert_eq!(report.fills[0].qty, 1000);
        assert!((report.fills[0].price - 10.0).abs() < 1e-9);
        assert!(
            (report.fills[0].fee - 5.1).abs() < 1e-9,
            "fee={}",
            report.fills[0].fee
        );
        assert_eq!(report.fills[0].realized_pnl, None);

        assert_eq!(report.fills[1].account, Account::Real);
        assert_eq!(report.fills[1].code, "600004");
        assert_eq!(report.fills[1].name.as_deref(), Some("兆易创新"));
        assert_eq!(report.fills[1].side, Direction::Sell);
        assert_eq!(report.fills[1].qty, 1000);
        assert!((report.fills[1].price - 12.0).abs() < 1e-9);
        assert!((report.fills[1].fee - 5.0).abs() < 1e-9);
        assert_eq!(report.fills[1].realized_pnl, Some(200.0));

        assert!((report.realized_real - 200.0).abs() < 1e-9);
        assert!(report.realized_paper.abs() < 1e-9);

        assert_eq!(
            report.rejected,
            vec![(GateReject::Cooldown.label_zh().to_string(), 1)]
        );

        assert_eq!(report.real_holdings.positions, 2);
        assert!((report.real_holdings.market_value - 2200.0).abs() < 1e-9);
        assert!((report.real_holdings.cost - 2000.0).abs() < 1e-9);
        assert_eq!(report.real_holdings.unpriced, 1);

        assert_eq!(report.paper_holdings, HoldingSummary::default());

        assert_eq!(
            report.strategy_changes,
            vec![StrategyChange {
                strategy_id,
                name: "策略A".into(),
                from: StrategyStatus::Paper,
                to: StrategyStatus::Admitted,
                reason: "准入".into(),
            }]
        );
    }

    #[test]
    fn rendering_is_factual_and_omits_empty_sections() {
        let r = DailyReport {
            user_id: 1,
            date: day(23),
            real_tickets: TicketCounts::default(),
            fills: vec![],
            realized_real: 0.0,
            realized_paper: 0.0,
            rejected: vec![],
            real_holdings: HoldingSummary {
                positions: 1,
                market_value: 1200.0,
                cost: 1000.0,
                unpriced: 0,
            },
            paper_holdings: HoldingSummary::default(),
            strategy_changes: vec![],
        };
        let (title, md) = render_daily_report(&r);
        assert_eq!(title, "交易日报 2026-09-23");
        assert!(md.contains("#### 持仓"), "{md}");
        assert!(md.contains("+200.00"), "浮动盈亏应带正号: {md}");
        for absent in [
            "今日工单",
            "今日成交",
            "已实现盈亏",
            "被拦截的信号",
            "策略状态变化",
        ] {
            assert!(!md.contains(absent), "不应含 {absent}: {md}");
        }
        for banned in ["建议买入", "建议卖出"] {
            assert!(!md.contains(banned), "不得出现 {banned}: {md}");
        }
        assert!(md.contains("以上为系统记录的统计,不构成投资建议。"), "{md}");
    }

    fn fill(account: Account, code: String, side: Direction) -> FillLine {
        FillLine {
            account,
            code,
            name: Some("某某股份".into()),
            side,
            qty: 1000,
            price: 12.34,
            fee: 5.0,
            realized_pnl: Some(-123.45),
        }
    }

    #[test]
    fn long_report_caps_paper_fills_rejections_and_strategy_changes() {
        let mut fills: Vec<FillLine> = (0..5)
            .map(|i| fill(Account::Real, format!("60{i:04}"), Direction::Buy))
            .collect();
        fills.extend((0..100).map(|i| fill(Account::Paper, format!("30{i:04}"), Direction::Sell)));
        let rejected: Vec<(String, usize)> = (0..20)
            .map(|i| (format!("某种相当长的拦截原因文案第{i}类"), 20 - i))
            .collect();
        let strategy_changes: Vec<StrategyChange> = (0..15)
            .map(|i| StrategyChange {
                strategy_id: i,
                name: format!("策略{i}"),
                from: StrategyStatus::Paper,
                to: StrategyStatus::Admitted,
                reason: "观察期达标".into(),
            })
            .collect();
        let r = DailyReport {
            user_id: 1,
            date: day(23),
            real_tickets: TicketCounts {
                created: 5,
                filled: 5,
                ..TicketCounts::default()
            },
            fills,
            realized_real: 0.0,
            realized_paper: -12345.0,
            rejected,
            real_holdings: HoldingSummary::default(),
            paper_holdings: HoldingSummary {
                positions: 30,
                market_value: 1_000_000.0,
                cost: 990_000.0,
                unpriced: 2,
            },
            strategy_changes,
        };
        let (_, md) = render_daily_report(&r);
        assert!(md.len() < 4000, "正文 {} 字节,超出推送上限: {md}", md.len());
        for i in 0..5 {
            assert!(
                md.contains(&format!("实盘 买入 60{i:04}")),
                "实盘成交须全部列出: {md}"
            );
        }
        assert_eq!(md.matches("- 模拟盘 卖出").count(), 10, "{md}");
        assert!(md.contains("- 另 90 笔模拟盘成交(详见交易页)"), "{md}");
        assert!(md.contains("第4类 × 16"), "{md}");
        assert!(!md.contains("第5类"), "{md}");
        assert!(md.contains("- 其它 15 类"), "{md}");
        assert!(md.contains("策略9(#9)"), "{md}");
        assert!(!md.contains("策略10(#10)"), "{md}");
        assert!(md.contains("- 另 5 条策略状态变化(详见交易页)"), "{md}");
        assert!(md.contains("以上为系统记录的统计,不构成投资建议。"), "{md}");
    }

    #[test]
    fn short_lists_render_without_remainder_lines() {
        let r = DailyReport {
            user_id: 1,
            date: day(23),
            real_tickets: TicketCounts::default(),
            fills: (0..10)
                .map(|i| fill(Account::Paper, format!("30{i:04}"), Direction::Buy))
                .collect(),
            realized_real: 0.0,
            realized_paper: 0.0,
            rejected: (0..5).map(|i| (format!("原因{i}"), 1)).collect(),
            real_holdings: HoldingSummary::default(),
            paper_holdings: HoldingSummary::default(),
            strategy_changes: vec![],
        };
        let (_, md) = render_daily_report(&r);
        assert_eq!(md.matches("- 模拟盘 买入").count(), 10, "{md}");
        assert!(md.contains("原因4 × 1"), "{md}");
        assert!(!md.contains("- 另 "), "{md}");
        assert!(!md.contains("其它"), "{md}");
    }

    #[test]
    fn strategy_status_labels_match_trade_page() {
        assert_eq!(StrategyStatus::Draft.label_zh(), "草稿");
        assert_eq!(StrategyStatus::Backtesting.label_zh(), "回测中");
        assert_eq!(StrategyStatus::Failed.label_zh(), "未通过");
        assert_eq!(StrategyStatus::Paper.label_zh(), "观察期");
        assert_eq!(StrategyStatus::Admitted.label_zh(), "已准入");
        assert_eq!(StrategyStatus::Suspended.label_zh(), "已暂停");
    }

    #[test]
    fn rendering_uses_page_wording_for_statuses() {
        let r = DailyReport {
            user_id: 1,
            date: day(23),
            real_tickets: TicketCounts {
                created: 6,
                open: 1,
                confirmed: 1,
                filled: 1,
                expired: 1,
                ignored: 1,
                cancelled: 1,
            },
            fills: vec![],
            realized_real: 0.0,
            realized_paper: 0.0,
            rejected: vec![],
            real_holdings: HoldingSummary::default(),
            paper_holdings: HoldingSummary::default(),
            strategy_changes: vec![StrategyChange {
                strategy_id: 7,
                name: "策略A".into(),
                from: StrategyStatus::Paper,
                to: StrategyStatus::Admitted,
                reason: "准入".into(),
            }],
        };
        let (_, md) = render_daily_report(&r);
        assert!(md.contains("已拒绝 1"), "{md}");
        assert!(md.contains("已取消 1"), "{md}");
        assert!(!md.contains("已忽略") && !md.contains("已撤销"), "{md}");
        assert!(md.contains("策略A(#7):观察期 → 已准入,原因:准入"), "{md}");
        assert!(!md.contains("paper") && !md.contains("admitted"), "{md}");
    }

    #[test]
    fn report_is_scoped_to_the_user() {
        let c = db();
        let today = day(23);
        store::set_capital(&c, 1, Account::Real, 50_000.0, at(20, 9, 0)).unwrap();
        store::set_capital(&c, 2, Account::Real, 50_000.0, at(20, 9, 0)).unwrap();

        // 用户 1:1 张待确认实盘工单 + 1 只有报价的实盘持仓
        let sid1 = ticket::insert_signal(
            &c,
            &signal(1, "600020", Direction::Buy, "u1-sig"),
            at(23, 9, 30),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid1, "ticketed", None).unwrap();
        ticket::create_ticket(
            &c,
            &new_real_ticket(
                1,
                sid1,
                "600020",
                Direction::Buy,
                100,
                TicketStatus::Pending,
                at(23, 9, 30),
            ),
        )
        .unwrap();

        let mut pos1 = Position::empty(1, Account::Real, "600021");
        pos1.qty = 200;
        pos1.avg_cost = 8.0;
        store::upsert_position(&c, &pos1, at(20, 9, 5)).unwrap();
        store::upsert_quotes(
            &c,
            &[Quote {
                code: "600021".into(),
                price: 9.0,
                limit_up: None,
                limit_down: None,
                ts: at(23, 9, 0),
            }],
            at(23, 9, 0),
        )
        .unwrap();

        // 用户 2:当日有大量活动(工单被拒、成交、持仓、策略状态变化),不应出现在用户 1 的日报里
        let sid2_rej = ticket::insert_signal(
            &c,
            &signal(2, "700001", Direction::Buy, "u2-sig"),
            at(23, 9, 30),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid2_rej, "rejected", Some("cooldown")).unwrap();

        let sid2 = ticket::insert_signal(
            &c,
            &signal(2, "700002", Direction::Sell, "u2-sig-b"),
            at(23, 9, 31),
        )
        .unwrap()
        .unwrap();
        ticket::mark_signal(&c, sid2, "ticketed", None).unwrap();
        let tid2 = ticket::create_ticket(
            &c,
            &new_real_ticket(
                2,
                sid2,
                "700002",
                Direction::Sell,
                100,
                TicketStatus::Filled,
                at(23, 9, 31),
            ),
        )
        .unwrap();
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, 2, 'real', '700002', 'sell', 9.0, 100, 5.0, 50.0, 'manual', ?2)",
            params![tid2, fmt_ts(at(23, 9, 32))],
        )
        .unwrap();

        let mut pos2 = Position::empty(2, Account::Real, "700003");
        pos2.qty = 300;
        pos2.avg_cost = 5.0;
        store::upsert_position(&c, &pos2, at(20, 9, 5)).unwrap();

        let strat2 = store::create_strategy(
            &c,
            &NewStrategy {
                user_id: 2,
                name: "U2策略".into(),
                kind: "dca".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["700001".into()],
            },
            at(20, 9, 0),
        )
        .unwrap();
        state::update_status(
            &c,
            2,
            strat2,
            StrategyStatus::Draft,
            StrategyStatus::Paper,
            "观察期",
            at(23, 9, 40),
        )
        .unwrap();

        let report = build_daily_report(&c, 1, today).unwrap().unwrap();
        assert_eq!(
            report.real_tickets,
            TicketCounts {
                created: 1,
                open: 1,
                ..TicketCounts::default()
            }
        );
        assert!(report.fills.is_empty(), "{:?}", report.fills);
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        assert!(
            report.strategy_changes.is_empty(),
            "{:?}",
            report.strategy_changes
        );
        assert_eq!(report.real_holdings.positions, 1);
        assert!((report.real_holdings.market_value - 1800.0).abs() < 1e-9);
        assert_eq!(report.real_holdings.unpriced, 0);
        assert_eq!(report.paper_holdings, HoldingSummary::default());
    }
}

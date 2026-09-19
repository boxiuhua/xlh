//! 用户动作:受保护的确认、实盘持仓校准(计划 4a)。
//! Web 层不含业务规则(design decision 1):偏离保护、行情延迟、总开关、
//! 持仓校准留痕都在这里实现并单测;handler 只调用。

use crate::event::Direction;
use crate::trade::admission::state;
use crate::trade::gate::GateReject;
use crate::trade::model::{
    fmt_ts, Account, AccountScope, EvalKind, JobStatus, NewSignal, Position, Quote, SignalSource,
    TicketStatus,
};
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use crate::trade::settings;
use crate::trade::store;
use crate::trade::ticket::{self, Transition};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::{params, Connection};

/// 行情缓存超过此秒数视为陈旧(design decision 2)。
pub const QUOTE_MAX_AGE_SECS: i64 = 60;

/// 现价相对建议价的偏离(绝对值,比例)。确认保护与工单列表展示共用同一口径。
pub fn deviation(price: f64, suggest_price: f64) -> f64 {
    (price / suggest_price - 1.0).abs()
}

/// 行情是否陈旧:时间戳距 `now` 超过 `QUOTE_MAX_AGE_SECS`(与 `store::fresh_quote` 同一口径)。
pub fn quote_is_stale(quote_ts: NaiveDateTime, now: NaiveDateTime) -> bool {
    (now - quote_ts).num_seconds() > QUOTE_MAX_AGE_SECS
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfirmError {
    /// 工单不存在,或不属于该用户(跨用户视为不存在)。
    NotFound,
    /// 已非待确认(过期 / 已处理 / 并发重复点击),或模拟盘无需人工确认。
    AlreadyHandled,
    /// 管理员总开关打开。
    KillSwitch,
    /// `trade_quotes` 无该代码,或行情距今超过 `QUOTE_MAX_AGE_SECS`。
    StaleQuote,
    /// 现价相对建议价的偏离超过工单的 `deviation_th`,且未 `ack_deviation`,
    /// 或确认时的偏离已超过用户看到的 `ack_max_deviation` + `ACK_DEVIATION_TOLERANCE`。
    Deviation { price: f64, deviation: f64 },
}

/// 偏离确认的容差(比例):用户看到并确认的偏离之后,现价允许再走 0.5 个百分点。
pub const ACK_DEVIATION_TOLERANCE: f64 = 0.005;

/// 受保护的工单确认(spec §8,design decision 1 + 2):规则按序——
/// 不存在/跨用户 → 总开关 → 状态/过期 → 模拟盘无需确认 → 行情陈旧 → 价格偏离 → 落库确认。
///
/// 偏离确认绑定到用户看到的数值(4b 终审 I1):`ack_max_deviation` 是页面按钮上显示的偏离;
/// 当前偏离超过它 + `ACK_DEVIATION_TOLERANCE` 时仍返回 `Deviation`(带当前偏离),
/// 让页面按新值再确认一次。`None` 为旧客户端,`ack_deviation` 即放行。
/// 调用方负责保证 `ack_max_deviation` 有限且 ≥ 0(web 层校验,非法 → 400)。
pub fn confirm_ticket(
    conn: &Connection,
    user_id: i64,
    ticket_id: i64,
    ack_deviation: bool,
    ack_max_deviation: Option<f64>,
    now: NaiveDateTime,
) -> Result<std::result::Result<(), ConfirmError>> {
    let Some(t) = ticket::get_ticket(conn, ticket_id)?.filter(|t| t.user_id == user_id) else {
        return Ok(Err(ConfirmError::NotFound));
    };
    if settings::kill_switch(conn)? {
        return Ok(Err(ConfirmError::KillSwitch));
    }
    if t.status != TicketStatus::Pending || t.expires_at <= now {
        return Ok(Err(ConfirmError::AlreadyHandled));
    }
    if t.account == Account::Paper {
        return Ok(Err(ConfirmError::AlreadyHandled));
    }
    let Some(q) = store::fresh_quote(conn, &t.code, now, QUOTE_MAX_AGE_SECS)? else {
        return Ok(Err(ConfirmError::StaleQuote));
    };
    let dev = deviation(q.price, t.suggest_price);
    let acked = ack_deviation
        && ack_max_deviation.is_none_or(|shown| dev <= shown + ACK_DEVIATION_TOLERANCE + 1e-12);
    if dev > t.deviation_th + 1e-12 && !acked {
        return Ok(Err(ConfirmError::Deviation {
            price: q.price,
            deviation: dev,
        }));
    }
    Ok(match ticket::confirm(conn, user_id, ticket_id, now)? {
        Transition::Applied => Ok(()),
        Transition::AlreadyHandled => Err(ConfirmError::AlreadyHandled),
    })
}

/// `manual_fill` 的业务拒绝(数据库等内部错误走外层 `Err`)。
#[derive(Debug, Clone, PartialEq)]
pub enum FillError {
    /// 工单不存在,或不属于该用户(跨用户视为不存在)。
    NotFound,
    /// 模拟盘工单只由系统撮合,不允许人工回填(design decision 5)。
    PaperNotAllowed,
    /// 输入或状态不合法:价格 / 数量非正、工单非已确认 / 部分成交、超过剩余数量、
    /// 卖出无持仓或超过可卖数量。
    Validation(String),
}

/// 人工回填实盘成交。总开关不限制回填(design decision 4:已发生的事实必须能记)。
/// 在同一个 `IMMEDIATE` 事务里先读工单与持仓做业务预校验(返回 `FillError`),
/// 再调用 `ticket::record_fill_in` 落库;此后任何 `Err` 都是内部错误(web 层 500)。
pub fn manual_fill(
    conn: &mut Connection,
    user_id: i64,
    ticket_id: i64,
    price: f64,
    qty: u64,
    now: NaiveDateTime,
) -> Result<std::result::Result<ticket::FillOutcome, FillError>> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let Some(t) = ticket::get_ticket(&tx, ticket_id)?.filter(|t| t.user_id == user_id) else {
        return Ok(Err(FillError::NotFound));
    };
    if t.account == Account::Paper {
        return Ok(Err(FillError::PaperNotAllowed));
    }
    let invalid = |msg: String| Ok(Err(FillError::Validation(msg)));
    // 4a 终审遗留:账户资金行不存在时,此前会落到 `record_fill_in` 内的 `add_cash` 报错(500)。
    if store::get_account(&tx, user_id, t.account)?.is_none() {
        return invalid("未设置账户资金,请先在「风控设置」里设置总资金".into());
    }
    if !(price.is_finite() && price > 0.0) || qty == 0 {
        return invalid("成交价与数量必须为正数".into());
    }
    if !matches!(t.status, TicketStatus::Confirmed | TicketStatus::Partial) {
        return invalid(format!("工单状态为 {},不可回填成交", t.status.as_str()));
    }
    let remaining = t.qty - t.filled_qty;
    if qty > remaining {
        return invalid(format!("成交数量超过工单剩余数量({remaining})"));
    }
    if t.side == crate::event::Direction::Sell {
        let Some(p) = store::get_position(&tx, user_id, t.account, &t.code)? else {
            return invalid("无持仓,不可卖出".into());
        };
        let sellable = p.sellable(now.date());
        if qty > sellable {
            return invalid(format!("卖出数量超过可卖数量 {sellable}(T+1)"));
        }
    }
    let out = ticket::record_fill_in(&tx, user_id, ticket_id, price, qty, "manual", now)?;
    tx.commit()?;
    Ok(Ok(out))
}

/// 止盈止损校验(计划 4a):价格须有限且 > 0;两者都有时止损须低于止盈;
/// `trailing_pct` 须在 (0, 0.5] 之间。
pub fn validate_exit_levels(
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
) -> Result<()> {
    for (name, v) in [("止损", stop_loss), ("止盈", take_profit)] {
        if let Some(v) = v {
            if !(v.is_finite() && v > 0.0) {
                return Err(anyhow!("{name}价格必须为正数: {v}"));
            }
        }
    }
    if let (Some(sl), Some(tp)) = (stop_loss, take_profit) {
        if sl >= tp {
            return Err(anyhow!("止损须低于止盈: stop_loss={sl}, take_profit={tp}"));
        }
    }
    if let Some(t) = trailing_pct {
        if !(t.is_finite() && t > 0.0 && t <= 0.5) {
            return Err(anyhow!("移动止损比例须在 (0, 0.5] 之间: {t}"));
        }
    }
    Ok(())
}

/// 实盘持仓校准的目标状态(design decision 5:只作用于实盘账户)。
#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    pub code: String,
    pub qty: u64,
    pub avg_cost: f64,
    pub reason: String,
}

/// 持仓校准输入校验:代码须为 6 位数字;`qty` 为 0 时删除实盘持仓,否则 `avg_cost` 须有限
/// 且 > 0;`reason` 去掉首尾空白后不得为空。独立导出,供 web 层在落库前先行校验(400),
/// `calibrate_position` 自身也调用它,直接调用方同样受保护。
pub fn validate_calibration(c: &Calibration) -> Result<()> {
    if c.code.len() != 6 || !c.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(anyhow!("股票代码须为 6 位数字: {}", c.code));
    }
    if c.reason.trim().is_empty() {
        return Err(anyhow!("校准原因不能为空"));
    }
    if c.qty > 0 && !(c.avg_cost.is_finite() && c.avg_cost > 0.0) {
        return Err(anyhow!("持仓成本必须为正数: {}", c.avg_cost));
    }
    Ok(())
}

/// 持仓校准:输入校验见 `validate_calibration`。改前 / 改后各写一条 `trade_position_adjusts`,
/// 整体一个 `IMMEDIATE` 事务,失败不留痕。
pub fn calibrate_position(
    conn: &mut Connection,
    user_id: i64,
    c: &Calibration,
    now: NaiveDateTime,
) -> Result<()> {
    validate_calibration(c)?;
    let reason = c.reason.trim();

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let before = store::get_position(&tx, user_id, Account::Real, &c.code)?;
    let before_json = before.as_ref().map(serde_json::to_value).transpose()?;

    let after: Option<Position> = if c.qty == 0 {
        None
    } else {
        let mut p = match &before {
            Some(p) => p.clone(),
            None => {
                // 新建持仓:与 `record_fill` 首次建仓同口径,套用用户的默认止损 / 止盈
                // (以成本价为基准,按价格精度取整);已有持仓保持原止盈止损。
                let rules = store::get_risk_rules(&tx, user_id)?;
                let decimals = crate::stock::ashare::price_decimals(&c.code);
                let mut p = Position::empty(user_id, Account::Real, &c.code);
                p.stop_loss = Some(ticket::round_dec(
                    c.avg_cost * (1.0 - rules.default_stop_loss_pct),
                    decimals,
                ));
                p.take_profit = Some(ticket::round_dec(
                    c.avg_cost * (1.0 + rules.default_take_profit_pct),
                    decimals,
                ));
                p
            }
        };
        p.today_bought_qty = p.today_bought_qty.min(c.qty);
        p.qty = c.qty;
        p.avg_cost = c.avg_cost;
        Some(p)
    };
    let after_json = after.as_ref().map(serde_json::to_value).transpose()?;

    match &after {
        Some(p) => store::upsert_position(&tx, p, now)?,
        None => {
            store::delete_position(&tx, user_id, Account::Real, &c.code)?;
        }
    }
    store::record_position_adjust(
        &tx,
        user_id,
        Account::Real,
        &c.code,
        before_json,
        after_json,
        reason,
        now,
    )?;
    tx.commit()?;
    Ok(())
}

/// `submit_strategy` 的结果(计划 4a §5:策略提交与评估取消)。
#[derive(Debug, Clone, PartialEq)]
pub enum SubmitStrategyOutcome {
    /// 已入队前推回测评估任务。
    Queued { job_id: i64 },
    /// 异动类没有历史分时,直接进入观察期。
    Paper,
    /// 状态不允许提交(过期点击 / 并发重复提交)。
    AlreadyHandled,
    /// 策略不存在,或不属于该用户。
    NotFound,
}

/// 提交策略评估:草稿 / 未通过 / 已暂停 → 排队前推回测;异动类直接进观察期。
/// 跨用户或不存在一律 `NotFound`,不泄露存在性。
/// 状态转换与入队在同一个事务里:入队失败时状态一并回滚,策略不会停在「回测中」却没有任务。
pub fn submit_strategy(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: NaiveDateTime,
) -> Result<SubmitStrategyOutcome> {
    let tx = conn.unchecked_transaction()?;
    let out = submit_strategy_in(&tx, user_id, id, now)?;
    tx.commit()?;
    Ok(out)
}

fn submit_strategy_in(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: NaiveDateTime,
) -> Result<SubmitStrategyOutcome> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(SubmitStrategyOutcome::NotFound);
    };
    match state::submit_for_backtest_in(conn, user_id, id, now)? {
        Transition::AlreadyHandled => Ok(SubmitStrategyOutcome::AlreadyHandled),
        Transition::Applied => {
            if s.kind == "mover" {
                return Ok(SubmitStrategyOutcome::Paper);
            }
            match store::enqueue_eval(conn, user_id, id, EvalKind::WalkForward, now)? {
                Some(job_id) => Ok(SubmitStrategyOutcome::Queued { job_id }),
                None => {
                    // F5 去重命中:已有同策略同类型的排队/运行中任务,取其任务号。
                    let existing = store::list_jobs(conn, user_id, i64::MAX as usize)?
                        .into_iter()
                        .find(|j| {
                            j.strategy_id == id
                                && j.kind == EvalKind::WalkForward
                                && matches!(j.status, JobStatus::Queued | JobStatus::Running)
                        })
                        .ok_or_else(|| anyhow!("入队去重命中但查不到既有任务(策略 {id})"))?;
                    Ok(SubmitStrategyOutcome::Queued {
                        job_id: existing.id,
                    })
                }
            }
        }
    }
}

/// `cancel_job` 的结果(计划 4a §5,design decision 7)。
#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    /// 排队中的任务已直接结束为失败。
    Cancelled,
    /// 运行中的任务已打上取消标记,由 worker 在处理下一只股票前中止。
    Requested,
    /// 任务已结束(done/failed),或运行中但不是前推回测,不可取消。
    NotCancellable,
    /// 任务不存在,或不属于该用户。
    NotFound,
}

/// 取消评估任务(design decision 7):排队中的直接判失败,不让前推回测滞留;
/// 运行中的只打标记,真正的中止发生在 `worker.rs` 的 `on_code` 闭包里。
pub fn cancel_job(
    conn: &Connection,
    user_id: i64,
    job_id: i64,
    now: NaiveDateTime,
) -> Result<CancelOutcome> {
    let Some(job) = store::get_job(conn, user_id, job_id)? else {
        return Ok(CancelOutcome::NotFound);
    };
    match job.status {
        JobStatus::Queued => {
            let n = conn.execute(
                "UPDATE trade_eval_jobs SET status = 'failed', error = '用户取消', finished_at = ?1
                 WHERE id = ?2 AND status = 'queued'",
                params![fmt_ts(now), job_id],
            )?;
            if n == 0 {
                // 并发:在我们判断状态之后、更新之前被领走了。
                return Ok(CancelOutcome::NotCancellable);
            }
            state::fail_cancelled_backtest(conn, user_id, job.strategy_id, now)?;
            Ok(CancelOutcome::Cancelled)
        }
        // 只有前推回测会在 `on_code` 里检查取消标记;其它类型的运行中任务打了标记也
        // 不会被中止,如实返回不可取消,避免前端误以为「已请求取消」。
        JobStatus::Running if job.kind != EvalKind::WalkForward => {
            Ok(CancelOutcome::NotCancellable)
        }
        JobStatus::Running => {
            let n = conn.execute(
                "UPDATE trade_eval_jobs SET cancel_requested = 1 WHERE id = ?1 AND status = 'running'",
                params![job_id],
            )?;
            if n == 0 {
                // 并发:在我们判断状态之后、结束之前任务已经跑完了。
                return Ok(CancelOutcome::NotCancellable);
            }
            Ok(CancelOutcome::Requested)
        }
        JobStatus::Done | JobStatus::Failed => Ok(CancelOutcome::NotCancellable),
    }
}

/// 手动 / AI 建议下单(计划 4c)。字段校验见 `validate_manual`。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct ManualOrder {
    pub request_id: String,
    pub code: String,
    pub name: Option<String>,
    pub side: Direction,
    /// 买入金额;`None` 按风控单笔上限
    pub amount: Option<f64>,
    /// 卖出股数;`None` 全部可卖
    pub qty: Option<u64>,
    pub reason: String,
    pub ai_note: Option<String>,
}

/// 手动工单字段校验(design decision 1/2:与自动信号同路径,只是入口不同)。
pub fn validate_manual(o: &ManualOrder) -> Result<()> {
    if o.request_id.is_empty()
        || o.request_id.len() > 64
        || !o
            .request_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(anyhow!(
            "request_id 须为 1-64 个字符,只能包含字母、数字、'-': {}",
            o.request_id
        ));
    }
    if o.code.len() != 6 || !o.code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(anyhow!("股票代码须为 6 位数字: {}", o.code));
    }
    if let Some(n) = &o.name {
        if n.trim().chars().count() > 20 {
            return Err(anyhow!("股票名称过长(最多 20 字符)"));
        }
    }
    match o.side {
        Direction::Buy => {
            if let Some(a) = o.amount {
                if !(a.is_finite() && a > 0.0) {
                    return Err(anyhow!("买入金额必须为正数: {a}"));
                }
            }
            if o.qty.is_some() {
                return Err(anyhow!("买入不支持指定股数"));
            }
        }
        Direction::Sell => {
            if o.amount.is_some() {
                return Err(anyhow!("卖出不支持指定金额"));
            }
            if let Some(q) = o.qty {
                if q == 0 {
                    return Err(anyhow!("卖出股数必须大于 0"));
                }
            }
        }
    }
    if o.reason.trim().is_empty() {
        return Err(anyhow!("下单理由不能为空"));
    }
    if o.reason.trim().chars().count() > 500 {
        return Err(anyhow!("下单理由过长(最多 500 字符)"));
    }
    if let Some(note) = &o.ai_note {
        if note.chars().count() > 8000 {
            return Err(anyhow!("AI 备注过长(最多 8000 字符)"));
        }
    }
    Ok(())
}

/// `submit_manual` 的结果。
#[derive(Debug, Clone, PartialEq)]
pub enum ManualOutcome {
    Ticketed {
        real_ticket: Option<i64>,
        paper_ticket: Option<i64>,
    },
    Rejected(GateReject),
    Duplicate,
    /// 无今日行情(盘前 / 休市 / 停牌),或行情代码与工单不符。
    NoQuote,
}

/// 手动 / AI 工单入口(design decision 1-3):与自动信号同一闸门(`submit_signal`),
/// 只在有今日行情时放行,`request_id` 映射为 `dedup_key` 做幂等。
pub fn submit_manual(
    conn: &mut Connection,
    user_id: i64,
    o: &ManualOrder,
    quote: Option<&Quote>,
    now: NaiveDateTime,
) -> Result<ManualOutcome> {
    validate_manual(o)?;
    let Some(q) = quote.filter(|q| q.ts.date() == now.date() && q.code == o.code) else {
        return Ok(ManualOutcome::NoQuote);
    };
    // name / ai_note:去空白后空串视为 None(brief 校验表)。
    let trim_to_none = |s: &Option<String>| -> Option<String> {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let sig = NewSignal {
        user_id,
        source: SignalSource::Manual,
        strategy_id: None,
        code: o.code.clone(),
        name: trim_to_none(&o.name),
        side: o.side,
        scope: AccountScope::Both,
        ref_price: q.price,
        reason: o.reason.trim().to_string(),
        ai_note: trim_to_none(&o.ai_note),
        dedup_key: format!("manual-{}", o.request_id),
        suggest_cash: o.amount,
        suggest_qty: o.qty,
    };
    let outcome = submit_signal(
        conn,
        &sig,
        &SubmitContext {
            quote: Some(q),
            now,
        },
    )?;
    Ok(match outcome {
        SubmitOutcome::Duplicate => ManualOutcome::Duplicate,
        SubmitOutcome::Rejected { reason, .. } => ManualOutcome::Rejected(reason),
        SubmitOutcome::Ticketed {
            real_ticket,
            paper_ticket,
            ..
        } => ManualOutcome::Ticketed {
            real_ticket,
            paper_ticket,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::{
        Account, AccountScope, NewSignal, Quote, SignalSource, TicketStatus,
    };
    use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0, 0)).unwrap();
        c
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
    /// 手动买入信号 → 一张待确认实盘工单,返回工单号。
    fn pending_ticket(c: &mut Connection) -> i64 {
        let q = quote(10.0, at(10, 0, 0));
        store::upsert_quotes(c, std::slice::from_ref(&q), at(10, 0, 0)).unwrap();
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "t".into(),
            ai_note: None,
            dedup_key: "m1".into(),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        match submit_signal(
            c,
            &sig,
            &SubmitContext {
                quote: Some(&q),
                now: at(10, 0, 0),
            },
        )
        .unwrap()
        {
            SubmitOutcome::Ticketed {
                real_ticket: Some(id),
                ..
            } => id,
            other => panic!("{other:?}"),
        }
    }
    fn status(c: &Connection, id: i64) -> TicketStatus {
        crate::trade::ticket::get_ticket(c, id)
            .unwrap()
            .unwrap()
            .status
    }

    #[test]
    fn confirm_requires_fresh_quote_and_ack_for_deviation() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        // 行情 61 秒前 → 延迟
        assert_eq!(
            confirm_ticket(&c, 1, id, false, None, at(10, 1, 1)).unwrap(),
            Err(ConfirmError::StaleQuote)
        );
        // 新鲜但偏离 3%(阈值 1.5%)→ 需确认
        store::upsert_quotes(&c, &[quote(10.3, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        assert!(matches!(
            confirm_ticket(&c, 1, id, false, None, at(10, 1, 5)).unwrap(),
            Err(ConfirmError::Deviation { .. })
        ));
        assert_eq!(status(&c, id), TicketStatus::Pending);
        // 带 ack 通过
        assert_eq!(
            confirm_ticket(&c, 1, id, true, None, at(10, 1, 5)).unwrap(),
            Ok(())
        );
        assert_eq!(status(&c, id), TicketStatus::Confirmed);
        // 重复点击
        assert_eq!(
            confirm_ticket(&c, 1, id, true, None, at(10, 1, 6)).unwrap(),
            Err(ConfirmError::AlreadyHandled)
        );
    }

    #[test]
    fn confirm_ack_is_bound_to_the_deviation_shown() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        // 现价偏离 3%;用户只看到并确认过 2% → 超出容差 0.5%,要求按新偏离重新确认
        store::upsert_quotes(&c, &[quote(10.3, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        match confirm_ticket(&c, 1, id, true, Some(0.02), at(10, 1, 5)).unwrap() {
            Err(ConfirmError::Deviation { price, deviation }) => {
                assert_eq!(price, 10.3);
                assert!((deviation - 0.03).abs() < 1e-9, "{deviation}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(status(&c, id), TicketStatus::Pending);
        // 确认过 2.6%:3% ≤ 2.6% + 0.5% 容差 → 通过
        assert_eq!(
            confirm_ticket(&c, 1, id, true, Some(0.026), at(10, 1, 5)).unwrap(),
            Ok(())
        );
        assert_eq!(status(&c, id), TicketStatus::Confirmed);
    }

    #[test]
    fn confirm_ack_without_shown_value_is_legacy_accept() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        store::upsert_quotes(&c, &[quote(10.5, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        assert_eq!(
            confirm_ticket(&c, 1, id, true, None, at(10, 1, 5)).unwrap(),
            Ok(())
        );
    }

    #[test]
    fn confirm_ack_max_is_ignored_when_within_threshold() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        // 偏离 1%(阈值 1.5%)→ 无需 ack,ack_max 过小也不拦
        store::upsert_quotes(&c, &[quote(10.1, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        assert_eq!(
            confirm_ticket(&c, 1, id, false, Some(0.0), at(10, 1, 5)).unwrap(),
            Ok(())
        );
    }

    #[test]
    fn confirm_is_scoped_to_user_and_blocked_by_kill_switch() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        assert_eq!(
            confirm_ticket(&c, 2, id, true, None, at(10, 0, 5)).unwrap(),
            Err(ConfirmError::NotFound)
        );
        crate::trade::settings::set_kill_switch(&c, true, None, at(10, 0, 0)).unwrap();
        assert_eq!(
            confirm_ticket(&c, 1, id, true, None, at(10, 0, 5)).unwrap(),
            Err(ConfirmError::KillSwitch)
        );
    }

    #[test]
    fn validate_exit_levels_rejects_bad_input() {
        assert!(validate_exit_levels(Some(9.0), Some(12.0), None).is_ok());
        assert!(validate_exit_levels(None, None, None).is_ok());
        assert!(
            validate_exit_levels(Some(0.0), None, None).is_err(),
            "止损须为正数"
        );
        assert!(
            validate_exit_levels(None, Some(f64::NAN), None).is_err(),
            "止盈须有限"
        );
        assert!(
            validate_exit_levels(Some(12.0), Some(9.0), None).is_err(),
            "止损须低于止盈"
        );
        assert!(
            validate_exit_levels(Some(10.0), Some(10.0), None).is_err(),
            "相等也不行"
        );
        assert!(validate_exit_levels(None, None, Some(0.5)).is_ok());
        assert!(
            validate_exit_levels(None, None, Some(0.0)).is_err(),
            "trailing_pct 须 > 0"
        );
        assert!(
            validate_exit_levels(None, None, Some(0.51)).is_err(),
            "trailing_pct 须 <= 0.5"
        );
    }

    #[test]
    fn validate_calibration_rejects_bad_input() {
        assert!(validate_calibration(&Calibration {
            code: "600000".into(),
            qty: 100,
            avg_cost: 10.0,
            reason: "对账".into(),
        })
        .is_ok());
        assert!(
            validate_calibration(&Calibration {
                code: "60000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "x".into(),
            })
            .is_err(),
            "代码须为 6 位数字"
        );
        assert!(
            validate_calibration(&Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 0.0,
                reason: "x".into(),
            })
            .is_err(),
            "持仓成本须为正数"
        );
        assert!(
            validate_calibration(&Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "  ".into(),
            })
            .is_err(),
            "原因不能为空"
        );
        assert!(
            validate_calibration(&Calibration {
                code: "600000".into(),
                qty: 0,
                avg_cost: 0.0,
                reason: "清仓".into(),
            })
            .is_ok(),
            "qty 为 0 时不校验 avg_cost"
        );
    }

    #[test]
    fn calibration_logs_before_and_after_and_zero_deletes() {
        let mut c = db();
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 1000,
                avg_cost: 10.5,
                reason: "券商对账".into(),
            },
            at(15, 0, 0),
        )
        .unwrap();
        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!((p.qty, p.avg_cost), (1000, 10.5));
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 0,
                avg_cost: 0.0,
                reason: "已清仓".into(),
            },
            at(15, 1, 0),
        )
        .unwrap();
        assert!(store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .is_none());
        let log = store::list_adjusts(&c, 1, 10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().any(|a| a.before.is_none() && a.after.is_some()));
        assert!(log.iter().any(|a| a.before.is_some() && a.after.is_none()));
        assert!(
            store::list_adjusts(&c, 2, 10).unwrap().is_empty(),
            "按用户隔离"
        );
    }

    /// 校准新建的实盘持仓套用用户的默认止损 / 止盈(以成本价为基准,按价格精度取整,
    /// 与 `record_fill` 首次建仓同口径);已有持仓的止盈止损保持不变。
    #[test]
    fn calibration_new_position_gets_default_exit_levels_existing_keeps_them() {
        let mut c = db();
        let cal = |qty: u64, avg_cost: f64| Calibration {
            code: "600000".into(),
            qty,
            avg_cost,
            reason: "券商对账".into(),
        };
        calibrate_position(&mut c, 1, &cal(1000, 10.37), at(15, 0, 0)).unwrap();
        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        // 默认 8% / 20%:10.37 × 0.92 = 9.5404 → 9.54;10.37 × 1.2 = 12.444 → 12.44
        assert_eq!((p.stop_loss, p.take_profit), (Some(9.54), Some(12.44)));

        store::set_exit_levels(
            &c,
            1,
            Account::Real,
            "600000",
            Some(9.0),
            Some(13.0),
            None,
            at(15, 0, 30),
        )
        .unwrap();
        calibrate_position(&mut c, 1, &cal(1500, 11.0), at(15, 1, 0)).unwrap();
        let p = store::get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!(p.qty, 1500);
        assert_eq!(
            (p.stop_loss, p.take_profit),
            (Some(9.0), Some(13.0)),
            "已有持仓保持原止盈止损"
        );
    }

    #[test]
    fn calibration_rejects_bad_input() {
        let mut c = db();
        for bad in [
            Calibration {
                code: "60000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "x".into(),
            },
            Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 0.0,
                reason: "x".into(),
            },
            Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "  ".into(),
            },
        ] {
            assert!(calibrate_position(&mut c, 1, &bad, at(15, 0, 0)).is_err());
        }
        assert!(
            store::list_adjusts(&c, 1, 10).unwrap().is_empty(),
            "失败不留痕"
        );
    }

    /// 手动信号出单,返回 (实盘工单, 模拟盘工单)。
    fn ticket_for(
        c: &mut Connection,
        side: Direction,
        scope: AccountScope,
        key: &str,
        now: NaiveDateTime,
    ) -> (Option<i64>, Option<i64>) {
        let q = quote(10.0, now);
        store::upsert_quotes(c, std::slice::from_ref(&q), now).unwrap();
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side,
            scope,
            ref_price: 10.0,
            reason: "t".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        match submit_signal(
            c,
            &sig,
            &SubmitContext {
                quote: Some(&q),
                now,
            },
        )
        .unwrap()
        {
            SubmitOutcome::Ticketed {
                real_ticket,
                paper_ticket,
                ..
            } => (real_ticket, paper_ticket),
            other => panic!("{other:?}"),
        }
    }

    fn validation(r: std::result::Result<ticket::FillOutcome, FillError>) -> bool {
        matches!(r, Err(FillError::Validation(_)))
    }

    #[test]
    fn manual_fill_rejects_paper_foreign_and_bad_input() {
        let mut c = db();
        store::set_capital(&c, 1, Account::Paper, 100_000.0, at(9, 0, 0)).unwrap();
        let (real, paper) = ticket_for(
            &mut c,
            Direction::Buy,
            AccountScope::Both,
            "both",
            at(10, 0, 0),
        );
        let (real, paper) = (real.unwrap(), paper.unwrap());
        let now = at(10, 0, 5);

        // 模拟盘工单只由系统撮合(design decision 5)
        assert_eq!(
            manual_fill(&mut c, 1, paper, 10.0, 100, now).unwrap(),
            Err(FillError::PaperNotAllowed)
        );
        // 跨用户 / 不存在
        assert_eq!(
            manual_fill(&mut c, 2, real, 10.0, 100, now).unwrap(),
            Err(FillError::NotFound)
        );
        assert_eq!(
            manual_fill(&mut c, 1, 9999, 10.0, 100, now).unwrap(),
            Err(FillError::NotFound)
        );
        // 未确认的工单不可回填
        assert!(validation(
            manual_fill(&mut c, 1, real, 10.0, 100, now).unwrap()
        ));
        store::upsert_quotes(&c, &[quote(10.0, now)], now).unwrap();
        assert_eq!(
            confirm_ticket(&c, 1, real, false, None, now).unwrap(),
            Ok(())
        );
        // 价格 / 数量非法
        for (price, qty) in [(0.0, 100), (-1.0, 100), (f64::NAN, 100), (10.0, 0)] {
            assert!(validation(
                manual_fill(&mut c, 1, real, price, qty, now).unwrap()
            ));
        }
        let total = ticket::get_ticket(&c, real).unwrap().unwrap().qty;
        // 超过剩余数量
        assert!(validation(
            manual_fill(&mut c, 1, real, 10.0, total + 100, now).unwrap()
        ));
        let out = manual_fill(&mut c, 1, real, 10.0, total, now)
            .unwrap()
            .unwrap();
        assert_eq!(out.status, TicketStatus::Filled);
        // 已成交不可再回填
        assert!(validation(
            manual_fill(&mut c, 1, real, 10.0, 100, now).unwrap()
        ));
    }

    #[test]
    fn manual_fill_sell_checks_position_and_sellable() {
        let mut c = db();
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 1000,
                avg_cost: 10.0,
                reason: "对账".into(),
            },
            at(9, 30, 0),
        )
        .unwrap();
        let (real, _) = ticket_for(
            &mut c,
            Direction::Sell,
            AccountScope::RealOnly,
            "sell",
            at(10, 0, 0),
        );
        let real = real.unwrap();
        let now = at(10, 0, 5);
        assert_eq!(
            confirm_ticket(&c, 1, real, false, None, now).unwrap(),
            Ok(())
        );
        let qty = ticket::get_ticket(&c, real).unwrap().unwrap().qty;
        assert!(qty > 100, "{qty}");

        // 持仓被校准到 100 → 卖出超过可卖数量
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 100,
                avg_cost: 10.0,
                reason: "对账".into(),
            },
            now,
        )
        .unwrap();
        assert!(validation(
            manual_fill(&mut c, 1, real, 10.0, qty, now).unwrap()
        ));
        // 持仓被清掉 → 无持仓
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 0,
                avg_cost: 0.0,
                reason: "对账".into(),
            },
            now,
        )
        .unwrap();
        assert!(validation(
            manual_fill(&mut c, 1, real, 10.0, 100, now).unwrap()
        ));
        assert!(
            store::get_position(&c, 1, Account::Real, "600000")
                .unwrap()
                .is_none(),
            "被拒的回填不落库"
        );
    }

    /// 4a 终审遗留:账户资金行不存在时,人工回填此前会落到 `add_cash` 报错(500);
    /// 现应在预校验阶段就返回业务拒绝(400)。用户没有 `trade_accounts` 行,
    /// 但有实盘持仓(`calibrate_position` 造)与一张已确认的实盘卖出工单
    /// (直接用 `ticket::create_ticket` 写入 `status = Confirmed`,绕开需要账户资金的信号出单)。
    #[test]
    fn manual_fill_without_account_row_is_validation_error() {
        let mut c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        assert!(
            store::get_account(&c, 1, Account::Real).unwrap().is_none(),
            "未设置账户资金"
        );
        calibrate_position(
            &mut c,
            1,
            &Calibration {
                code: "600000".into(),
                qty: 1000,
                avg_cost: 10.0,
                reason: "对账".into(),
            },
            at(9, 30, 0),
        )
        .unwrap();
        let sid = ticket::insert_signal(
            &c,
            &NewSignal {
                user_id: 1,
                source: SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side: Direction::Sell,
                scope: AccountScope::RealOnly,
                ref_price: 10.0,
                reason: "t".into(),
                ai_note: None,
                dedup_key: "no-account".into(),
                suggest_cash: None,
                suggest_qty: Some(100),
            },
            at(10, 0, 0),
        )
        .unwrap()
        .unwrap();
        let real = ticket::create_ticket(
            &c,
            &ticket::NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Real,
                code: "600000".into(),
                side: Direction::Sell,
                suggest_price: 10.0,
                qty: 100,
                expires_at: at(10, 30, 0),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                urgency: 0,
                created_at: at(10, 0, 0),
            },
        )
        .unwrap();
        assert_eq!(
            manual_fill(&mut c, 1, real, 10.0, 100, at(10, 0, 5)).unwrap(),
            Err(FillError::Validation(
                "未设置账户资金,请先在「风控设置」里设置总资金".into()
            ))
        );
    }

    fn trend_strategy(c: &Connection) -> i64 {
        store::create_strategy(
            c,
            &crate::trade::model::NewStrategy {
                user_id: 1,
                name: "趋势".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]\nlong_window = [20]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 0, 0),
        )
        .unwrap()
    }

    #[test]
    fn submit_queues_walk_forward_once_and_cancel_fails_the_strategy() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } =
            submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap()
        else {
            panic!("应入队前推回测");
        };
        assert_eq!(
            submit_strategy(&c, 1, id, at(9, 2, 0)).unwrap(),
            SubmitStrategyOutcome::AlreadyHandled
        );
        assert_eq!(
            submit_strategy(&c, 2, id, at(9, 2, 0)).unwrap(),
            SubmitStrategyOutcome::NotFound
        );
        assert_eq!(
            cancel_job(&c, 2, job_id, at(9, 3, 0)).unwrap(),
            CancelOutcome::NotFound
        );
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 3, 0)).unwrap(),
            CancelOutcome::Cancelled
        );
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(
            s.status,
            crate::trade::model::StrategyStatus::Failed,
            "不滞留回测中"
        );
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 4, 0)).unwrap(),
            CancelOutcome::NotCancellable
        );
    }

    /// 只有前推回测会检查取消标记;运行中的其它类型任务不可取消,也不打标记。
    #[test]
    fn running_non_walk_forward_job_is_not_cancellable() {
        let c = db();
        let id = trend_strategy(&c);
        let job_id = store::enqueue_eval(&c, 1, id, EvalKind::PaperCheck, at(9, 1, 0))
            .unwrap()
            .unwrap();
        store::claim_next_job(&c, at(9, 1, 30)).unwrap().unwrap();
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 2, 0)).unwrap(),
            CancelOutcome::NotCancellable
        );
        assert!(!store::job_cancel_requested(&c, job_id).unwrap());
    }

    /// 入队失败时状态转换一并回滚,策略不会停在「回测中」却没有任务。
    #[test]
    fn submit_rolls_back_the_status_change_when_enqueue_fails() {
        let c = db();
        let id = trend_strategy(&c);
        c.execute_batch("DROP TABLE trade_eval_jobs").unwrap();
        assert!(submit_strategy(&c, 1, id, at(9, 1, 0)).is_err());
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(s.status, crate::trade::model::StrategyStatus::Draft);
    }

    #[test]
    fn cancelling_a_running_job_only_sets_the_flag() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } =
            submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap()
        else {
            panic!()
        };
        store::claim_next_job(&c, at(9, 1, 30)).unwrap().unwrap();
        assert_eq!(
            cancel_job(&c, 1, job_id, at(9, 2, 0)).unwrap(),
            CancelOutcome::Requested
        );
        assert!(store::job_cancel_requested(&c, job_id).unwrap());
    }

    fn manual(side: Direction, req: &str) -> ManualOrder {
        ManualOrder {
            request_id: req.into(),
            code: "600000".into(),
            name: Some("浦发银行".into()),
            side,
            amount: (side == Direction::Buy).then_some(5_000.0),
            qty: None,
            reason: "看好".into(),
            ai_note: Some("AI:估值偏低".into()),
        }
    }

    #[test]
    fn manual_buy_creates_real_and_paper_tickets_and_is_idempotent() {
        let mut c = db();
        let q = quote(10.0, at(10, 0, 0));
        let o = manual(Direction::Buy, "req-1");
        let r = submit_manual(&mut c, 1, &o, Some(&q), at(10, 0, 0)).unwrap();
        let ManualOutcome::Ticketed {
            real_ticket: Some(real),
            paper_ticket: Some(_),
        } = r
        else {
            panic!("{r:?}");
        };
        let t = crate::trade::ticket::get_ticket(&c, real).unwrap().unwrap();
        assert_eq!(t.expires_at, at(15, 0, 0), "手动工单默认当日 15:00 到期");
        assert_eq!(
            submit_manual(&mut c, 1, &o, Some(&q), at(10, 0, 5)).unwrap(),
            ManualOutcome::Duplicate,
            "同一 request_id 重复提交"
        );
        let meta = store::signal_meta(&c, t.signal_id).unwrap().unwrap();
        assert_eq!(meta.name.as_deref(), Some("浦发银行"));
        assert_eq!(meta.ai_note.as_deref(), Some("AI:估值偏低"));
    }

    #[test]
    fn manual_blank_name_and_empty_ai_note_become_none() {
        let mut c = db();
        let q = quote(10.0, at(10, 0, 0));
        let o = ManualOrder {
            name: Some("  ".into()),
            ai_note: Some("".into()),
            ..manual(Direction::Buy, "req-blank")
        };
        let r = submit_manual(&mut c, 1, &o, Some(&q), at(10, 0, 0)).unwrap();
        let ManualOutcome::Ticketed {
            real_ticket: Some(real),
            ..
        } = r
        else {
            panic!("{r:?}");
        };
        let t = crate::trade::ticket::get_ticket(&c, real).unwrap().unwrap();
        let meta = store::signal_meta(&c, t.signal_id).unwrap().unwrap();
        assert_eq!(meta.name, None, "空白名称视为 None");
        assert_eq!(meta.ai_note, None, "空串 AI 备注视为 None");
    }

    #[test]
    fn manual_needs_todays_quote_for_the_same_code() {
        let mut c = db();
        let o = manual(Direction::Buy, "req-2");
        assert_eq!(
            submit_manual(&mut c, 1, &o, None, at(10, 0, 0)).unwrap(),
            ManualOutcome::NoQuote
        );
        let yesterday = NaiveDate::from_ymd_opt(2026, 9, 22)
            .unwrap()
            .and_hms_opt(15, 0, 0)
            .unwrap();
        assert_eq!(
            submit_manual(&mut c, 1, &o, Some(&quote(10.0, yesterday)), at(10, 0, 0)).unwrap(),
            ManualOutcome::NoQuote
        );
        let other = Quote {
            code: "600036".into(),
            ..quote(10.0, at(10, 0, 0))
        };
        assert_eq!(
            submit_manual(&mut c, 1, &o, Some(&other), at(10, 0, 0)).unwrap(),
            ManualOutcome::NoQuote
        );
    }

    #[test]
    fn manual_goes_through_the_gate() {
        let mut c = db();
        crate::trade::settings::set_kill_switch(&c, true, None, at(9, 0, 0)).unwrap();
        let r = submit_manual(
            &mut c,
            1,
            &manual(Direction::Buy, "req-3"),
            Some(&quote(10.0, at(10, 0, 0))),
            at(10, 0, 0),
        )
        .unwrap();
        assert_eq!(
            r,
            ManualOutcome::Rejected(crate::trade::gate::GateReject::TradingDisabled)
        );
        crate::trade::settings::set_kill_switch(&c, false, None, at(9, 0, 0)).unwrap();
        let r = submit_manual(
            &mut c,
            1,
            &manual(Direction::Sell, "req-4"),
            Some(&quote(10.0, at(10, 0, 0))),
            at(10, 0, 0),
        )
        .unwrap();
        assert_eq!(
            r,
            ManualOutcome::Rejected(crate::trade::gate::GateReject::NothingSellable),
            "无持仓卖出"
        );
    }

    #[test]
    fn manual_validation() {
        let ok = manual(Direction::Buy, "req-5");
        assert!(validate_manual(&ok).is_ok());
        let bad = [
            ManualOrder {
                request_id: "".into(),
                ..ok.clone()
            },
            ManualOrder {
                request_id: "有中文".into(),
                ..ok.clone()
            },
            ManualOrder {
                request_id: "x".repeat(65),
                ..ok.clone()
            },
            ManualOrder {
                code: "60000".into(),
                ..ok.clone()
            },
            ManualOrder {
                amount: Some(0.0),
                ..ok.clone()
            },
            ManualOrder {
                amount: Some(f64::NAN),
                ..ok.clone()
            },
            ManualOrder {
                qty: Some(100),
                ..ok.clone()
            },
            ManualOrder {
                reason: "  ".into(),
                ..ok.clone()
            },
            ManualOrder {
                reason: "长".repeat(501),
                ..ok.clone()
            },
            ManualOrder {
                ai_note: Some("长".repeat(8001)),
                ..ok.clone()
            },
            ManualOrder {
                name: Some("长".repeat(21)),
                ..ok.clone()
            },
            ManualOrder {
                side: Direction::Sell,
                amount: Some(1000.0),
                qty: None,
                ..ok.clone()
            },
            ManualOrder {
                side: Direction::Sell,
                amount: None,
                qty: Some(0),
                ..ok.clone()
            },
        ];
        for b in bad {
            assert!(validate_manual(&b).is_err(), "{b:?}");
        }
    }
}

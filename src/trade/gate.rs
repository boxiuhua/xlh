//! 闸门:信号能否成为工单、给哪些账户、各多少股。纯函数,无 IO。

use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::ashare::{buy_lot, price_decimals, round_buy_shares, sell_qty, step_down};
use crate::stock::fee::StockFee;
use crate::trade::model::{
    Account, AccountState, NewSignal, Position, Quote, RiskRules, SignalSource,
};
use chrono::NaiveDateTime;
use serde::Serialize;

/// 策略准入状态(由调用方提供;止盈止损与手动信号忽略此项)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// 非策略来源,或尚无准入机制
    NotRequired,
    /// 已准入:实盘 + 模拟盘
    Admitted,
    /// 观察期:仅模拟盘
    Probation,
    /// 未通过 / 已暂停 / 草稿
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GateReject {
    TradingDisabled,
    DuplicateOpenTicket,
    Cooldown,
    NotAdmitted,
    NoQuote,
    LimitUp,
    LimitDown,
    DailyTicketCap,
    DailyLossHalt,
    NoCapital,
    BelowOneLot,
    NothingSellable,
}

impl GateReject {
    pub fn as_str(self) -> &'static str {
        match self {
            GateReject::TradingDisabled => "trading_disabled",
            GateReject::DuplicateOpenTicket => "duplicate_open_ticket",
            GateReject::Cooldown => "cooldown",
            GateReject::NotAdmitted => "not_admitted",
            GateReject::NoQuote => "no_quote",
            GateReject::LimitUp => "limit_up",
            GateReject::LimitDown => "limit_down",
            GateReject::DailyTicketCap => "daily_ticket_cap",
            GateReject::DailyLossHalt => "daily_loss_halt",
            GateReject::NoCapital => "no_capital",
            GateReject::BelowOneLot => "below_one_lot",
            GateReject::NothingSellable => "nothing_sellable",
        }
    }
}

pub struct GateInput<'a> {
    pub signal: &'a NewSignal,
    pub quote: Option<&'a Quote>,
    pub admission: Admission,
    pub rules: &'a RiskRules,
    pub real_account: Option<&'a AccountState>,
    pub paper_account: Option<&'a AccountState>,
    pub real_position: Option<&'a Position>,
    pub paper_position: Option<&'a Position>,
    /// 同用户同代码同方向是否已有未完结实盘工单
    pub has_open_ticket: bool,
    /// 同代码同方向最近一次成功生成工单的信号时间(冷却用)
    pub last_signal_at: Option<NaiveDateTime>,
    /// 今日已生成实盘工单数
    pub tickets_today: u32,
    /// 实盘今日已实现盈亏
    pub realized_pnl_today: f64,
    pub now: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub account: Account,
    pub qty: u64,
    /// 建议价(报价现价)
    pub price: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GateDecision {
    Pass(Vec<Plan>),
    Reject(GateReject),
}

pub fn evaluate(inp: &GateInput) -> GateDecision {
    use GateDecision::{Pass, Reject};
    let s = inp.signal;
    let rules = inp.rules;

    if !rules.enabled {
        return Reject(GateReject::TradingDisabled);
    }
    if inp.has_open_ticket {
        return Reject(GateReject::DuplicateOpenTicket);
    }
    if s.source != SignalSource::Exit {
        if let Some(last) = inp.last_signal_at {
            if inp.now - last < chrono::Duration::minutes(rules.cooldown_min) {
                return Reject(GateReject::Cooldown);
            }
        }
    }
    let accounts = match (s.source, inp.admission) {
        (SignalSource::Exit | SignalSource::Manual, _) => vec![Account::Real, Account::Paper],
        (_, Admission::NotRequired | Admission::Admitted) => vec![Account::Real, Account::Paper],
        (_, Admission::Probation) => vec![Account::Paper],
        (_, Admission::Blocked) => return Reject(GateReject::NotAdmitted),
    };
    let Some(q) = inp.quote.filter(|q| q.price.is_finite() && q.price > 0.0) else {
        return Reject(GateReject::NoQuote);
    };
    let eps = 0.5 * 10f64.powi(-price_decimals(&s.code));
    match s.side {
        Direction::Buy if q.limit_up.is_some_and(|u| q.price >= u - eps) => {
            return Reject(GateReject::LimitUp)
        }
        Direction::Sell if q.limit_down.is_some_and(|d| q.price <= d + eps) => {
            return Reject(GateReject::LimitDown)
        }
        _ => {}
    }
    let real_involved = accounts.contains(&Account::Real);
    if real_involved && inp.tickets_today >= rules.max_daily_tickets {
        return Reject(GateReject::DailyTicketCap);
    }
    if s.side == Direction::Buy && real_involved {
        if let Some(acc) = inp.real_account {
            if inp.realized_pnl_today < 0.0
                && -inp.realized_pnl_today >= acc.total_capital * rules.daily_loss_halt_pct
            {
                return Reject(GateReject::DailyLossHalt);
            }
        }
    }

    let today = inp.now.date();
    let mut plans = Vec::with_capacity(accounts.len());
    for (i, account) in accounts.iter().copied().enumerate() {
        let (acc, pos) = match account {
            Account::Real => (inp.real_account, inp.real_position),
            Account::Paper => (inp.paper_account, inp.paper_position),
        };
        let sized = match s.side {
            Direction::Buy => size_buy_for(s, q, rules, acc, pos),
            Direction::Sell => size_sell_for(s, pos, today),
        };
        match sized {
            Ok(qty) => plans.push(Plan {
                account,
                qty,
                price: q.price,
            }),
            Err(reason) if i == 0 => return Reject(reason),
            Err(_) => {}
        }
    }
    Pass(plans)
}

fn size_buy_for(
    s: &NewSignal,
    q: &Quote,
    rules: &RiskRules,
    acc: Option<&AccountState>,
    pos: Option<&Position>,
) -> Result<u64, GateReject> {
    let acc = acc.ok_or(GateReject::NoCapital)?;
    let held_value = pos.map_or(0.0, |p| p.qty as f64 * q.price);
    let budget = [
        s.suggest_cash.unwrap_or(rules.max_order_amount),
        rules.max_order_amount,
        acc.available_cash,
        acc.total_capital * rules.max_position_pct - held_value,
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min);
    match size_buy(&s.code, q.price, rules.slippage, budget) {
        0 => Err(GateReject::BelowOneLot),
        n => Ok(n),
    }
}

fn size_sell_for(
    s: &NewSignal,
    pos: Option<&Position>,
    today: chrono::NaiveDate,
) -> Result<u64, GateReject> {
    let sellable = pos.map_or(0, |p| p.sellable(today));
    if sellable == 0 {
        return Err(GateReject::NothingSellable);
    }
    match sell_qty(&s.code, s.suggest_qty.unwrap_or(sellable), sellable) {
        0 => Err(GateReject::BelowOneLot),
        n => Ok(n),
    }
}

/// 预算内最多可买股数:按含滑点执行价与 A 股费用,整手向下取整。
pub fn size_buy(code: &str, price: f64, slippage: f64, budget: f64) -> u64 {
    if !(budget > 0.0 && price > 0.0) {
        return 0;
    }
    let fee = StockFee::a_share();
    let lot = buy_lot(code);
    let exec_price = price * (1.0 + slippage);
    let mut n = round_buy_shares(budget / exec_price, lot);
    while n > 0 {
        let value = n as f64 * exec_price;
        if value + fee.buy_fee(value) <= budget + 1e-9 {
            break;
        }
        n = step_down(n, lot);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::*;
    use chrono::{NaiveDate, NaiveDateTime};

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }
    fn yesterday() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()
    }

    struct Fx {
        sig: NewSignal,
        quote: Option<Quote>,
        admission: Admission,
        rules: RiskRules,
        real: Option<AccountState>,
        paper: Option<AccountState>,
        real_pos: Option<Position>,
        paper_pos: Option<Position>,
        open: bool,
        last: Option<NaiveDateTime>,
        tickets_today: u32,
        pnl: f64,
    }

    fn account(a: Account, total: f64) -> AccountState {
        AccountState {
            user_id: 1,
            account: a,
            total_capital: total,
            available_cash: total,
        }
    }

    fn position(a: Account, qty: u64, bought: NaiveDate) -> Position {
        let mut p = Position::empty(1, a, "600000");
        p.qty = qty;
        p.avg_cost = 10.0;
        p.today_bought_qty = qty;
        p.last_buy_date = Some(bought);
        p
    }

    impl Fx {
        fn buy(source: SignalSource) -> Self {
            Fx {
                sig: NewSignal {
                    user_id: 1,
                    source,
                    strategy_id: None,
                    code: "600000".into(),
                    name: None,
                    side: Direction::Buy,
                    ref_price: 10.0,
                    reason: "r".into(),
                    ai_note: None,
                    dedup_key: "k".into(),
                    suggest_cash: None,
                    suggest_qty: None,
                },
                quote: Some(Quote {
                    code: "600000".into(),
                    price: 10.0,
                    limit_up: Some(11.0),
                    limit_down: Some(9.0),
                    ts: now(),
                }),
                admission: Admission::NotRequired,
                rules: RiskRules::default(),
                real: Some(account(Account::Real, 100_000.0)),
                paper: Some(account(Account::Paper, 100_000.0)),
                real_pos: None,
                paper_pos: None,
                open: false,
                last: None,
                tickets_today: 0,
                pnl: 0.0,
            }
        }

        fn sell(qty: u64, bought: NaiveDate) -> Self {
            let mut f = Fx::buy(SignalSource::Exit);
            f.sig.side = Direction::Sell;
            f.real_pos = Some(position(Account::Real, qty, bought));
            f.paper_pos = Some(position(Account::Paper, qty, bought));
            f
        }

        fn run(&self) -> GateDecision {
            evaluate(&GateInput {
                signal: &self.sig,
                quote: self.quote.as_ref(),
                admission: self.admission,
                rules: &self.rules,
                real_account: self.real.as_ref(),
                paper_account: self.paper.as_ref(),
                real_position: self.real_pos.as_ref(),
                paper_position: self.paper_pos.as_ref(),
                has_open_ticket: self.open,
                last_signal_at: self.last,
                tickets_today: self.tickets_today,
                realized_pnl_today: self.pnl,
                now: now(),
            })
        }
    }

    fn rejected(d: GateDecision) -> GateReject {
        match d {
            GateDecision::Reject(r) => r,
            GateDecision::Pass(p) => panic!("应被拒绝,实际通过 {p:?}"),
        }
    }

    fn plans(d: GateDecision) -> Vec<Plan> {
        match d {
            GateDecision::Pass(p) => p,
            GateDecision::Reject(r) => panic!("应通过,实际拒绝 {r:?}"),
        }
    }

    fn plan(account: Account, qty: u64) -> Plan {
        Plan {
            account,
            qty,
            price: 10.0,
        }
    }

    #[test]
    fn buy_passes_for_real_and_paper_sized_by_position_cap() {
        // 预算 = min(50000, 50000, 100000, 100000×20%) = 20000;执行价 10.01 → 1998 → 1900 股
        assert_eq!(
            plans(Fx::buy(SignalSource::Manual).run()),
            vec![plan(Account::Real, 1900), plan(Account::Paper, 1900)]
        );
    }

    #[test]
    fn rejections_in_rule_order() {
        let mut f = Fx::buy(SignalSource::Manual);
        f.rules.enabled = false;
        assert_eq!(rejected(f.run()), GateReject::TradingDisabled);

        let mut f = Fx::buy(SignalSource::Manual);
        f.open = true;
        assert_eq!(rejected(f.run()), GateReject::DuplicateOpenTicket);

        let mut f = Fx::buy(SignalSource::Strategy);
        f.last = Some(now() - chrono::Duration::minutes(30));
        assert_eq!(rejected(f.run()), GateReject::Cooldown);

        let mut f = Fx::buy(SignalSource::Strategy);
        f.admission = Admission::Blocked;
        assert_eq!(rejected(f.run()), GateReject::NotAdmitted);

        let mut f = Fx::buy(SignalSource::Manual);
        f.quote = None;
        assert_eq!(rejected(f.run()), GateReject::NoQuote);

        let mut f = Fx::buy(SignalSource::Manual);
        f.quote.as_mut().unwrap().limit_up = Some(10.0);
        assert_eq!(rejected(f.run()), GateReject::LimitUp);

        let mut f = Fx::buy(SignalSource::Manual);
        f.tickets_today = 20;
        assert_eq!(rejected(f.run()), GateReject::DailyTicketCap);

        let mut f = Fx::buy(SignalSource::Manual);
        f.pnl = -3_000.0;
        assert_eq!(rejected(f.run()), GateReject::DailyLossHalt);

        let mut f = Fx::buy(SignalSource::Manual);
        f.real = None;
        assert_eq!(rejected(f.run()), GateReject::NoCapital);

        let mut f = Fx::buy(SignalSource::Manual);
        let q = f.quote.as_mut().unwrap();
        q.price = 300.0;
        q.limit_up = Some(330.0);
        assert_eq!(
            rejected(f.run()),
            GateReject::BelowOneLot,
            "20000/300.3 不足一手"
        );
    }

    #[test]
    fn exit_signals_ignore_cooldown_and_admission() {
        let mut f = Fx::sell(1000, yesterday());
        f.last = Some(now() - chrono::Duration::minutes(1));
        f.admission = Admission::Blocked;
        assert_eq!(
            plans(f.run()),
            vec![plan(Account::Real, 1000), plan(Account::Paper, 1000)]
        );
    }

    #[test]
    fn probation_strategy_trades_paper_only() {
        let mut f = Fx::buy(SignalSource::Strategy);
        f.admission = Admission::Probation;
        assert_eq!(plans(f.run()), vec![plan(Account::Paper, 1900)]);
    }

    #[test]
    fn loss_halt_blocks_buys_but_not_sells() {
        let mut f = Fx::sell(1000, yesterday());
        f.pnl = -5_000.0;
        assert_eq!(plans(f.run()).len(), 2);
    }

    #[test]
    fn sell_rules_t_plus_one_limit_down_and_partial() {
        let f = Fx::sell(1000, now().date());
        assert_eq!(
            rejected(f.run()),
            GateReject::NothingSellable,
            "今日买入不可卖"
        );

        let mut f = Fx::sell(1000, yesterday());
        f.quote.as_mut().unwrap().limit_down = Some(10.0);
        assert_eq!(rejected(f.run()), GateReject::LimitDown);

        let mut f = Fx::sell(1000, yesterday());
        f.sig.suggest_qty = Some(250);
        assert_eq!(
            plans(f.run()),
            vec![plan(Account::Real, 200), plan(Account::Paper, 200)]
        );
    }

    #[test]
    fn existing_position_reduces_buy_budget() {
        // 已持 1500 股 × 10 = 15000;上限 20000 → 预算 5000 → 499 → 400 股
        let mut f = Fx::buy(SignalSource::Manual);
        f.real_pos = Some(position(Account::Real, 1500, yesterday()));
        assert_eq!(plans(f.run())[0], plan(Account::Real, 400));
    }

    #[test]
    fn missing_paper_account_is_skipped_not_rejected() {
        let mut f = Fx::buy(SignalSource::Manual);
        f.paper = None;
        assert_eq!(plans(f.run()), vec![plan(Account::Real, 1900)]);
    }

    #[test]
    fn size_buy_steps_down_for_fees() {
        assert_eq!(
            size_buy("600000", 10.0, 0.0, 1005.0),
            0,
            "1000 + 5.01 > 1005"
        );
        assert_eq!(size_buy("600000", 10.0, 0.0, 1005.01), 100);
        assert_eq!(size_buy("688001", 10.0, 0.0, 3000.0), 299);
        assert_eq!(size_buy("600000", 10.0, 0.0, -1.0), 0);
    }

    #[test]
    fn reject_reason_strings() {
        assert_eq!(GateReject::BelowOneLot.as_str(), "below_one_lot");
        assert_eq!(
            serde_json::to_string(&GateReject::DailyLossHalt).unwrap(),
            "\"daily_loss_halt\""
        );
    }
}

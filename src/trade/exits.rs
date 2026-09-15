//! 止盈 / 止损 / 移动止盈判定。纯函数:输入持仓与报价,输出是否触发及信号。

use crate::event::Direction;
use crate::trade::model::{
    Account, AccountScope, NewSignal, Position, Quote, SignalSource, DATE_FMT,
};
use chrono::{NaiveDate, NaiveDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRule {
    StopLoss,
    Trailing,
    TakeProfit,
}

impl ExitRule {
    pub fn as_str(self) -> &'static str {
        match self {
            ExitRule::StopLoss => "stop",
            ExitRule::Trailing => "trailing",
            ExitRule::TakeProfit => "take",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExitRule::StopLoss => "止损",
            ExitRule::Trailing => "移动止盈",
            ExitRule::TakeProfit => "止盈",
        }
    }
}

pub fn trigger(p: &Position, price: f64) -> Option<ExitRule> {
    if p.qty == 0 || !(price.is_finite() && price > 0.0) {
        return None;
    }
    if p.stop_loss.is_some_and(|s| price <= s) {
        return Some(ExitRule::StopLoss);
    }
    if let (Some(pct), Some(high)) = (p.trailing_pct, p.trailing_high) {
        if pct > 0.0 && price <= high * (1.0 - pct) {
            return Some(ExitRule::Trailing);
        }
    }
    if p.take_profit.is_some_and(|t| price >= t) {
        return Some(ExitRule::TakeProfit);
    }
    None
}

/// 启用移动止盈时,价格创出新高则返回新的最高价。
pub fn next_trailing_high(p: &Position, price: f64) -> Option<f64> {
    p.trailing_pct?;
    match p.trailing_high {
        Some(high) if price <= high => None,
        _ => Some(price),
    }
}

pub fn scope_for(account: Account) -> AccountScope {
    match account {
        Account::Real => AccountScope::RealOnly,
        Account::Paper => AccountScope::PaperOnly,
    }
}

pub fn exit_dedup_key(p: &Position, rule: ExitRule, day: NaiveDate) -> String {
    format!(
        "exit-{}-{}-{}-{}",
        p.account.as_str(),
        p.code,
        rule.as_str(),
        day.format(DATE_FMT)
    )
}

pub fn exit_signal(p: &Position, q: &Quote, now: NaiveDateTime) -> Option<NewSignal> {
    let rule = trigger(p, q.price)?;
    let (cmp, level) = match rule {
        ExitRule::StopLoss => ("≤", p.stop_loss),
        ExitRule::TakeProfit => ("≥", p.take_profit),
        ExitRule::Trailing => (
            "≤",
            p.trailing_high
                .zip(p.trailing_pct)
                .map(|(h, pct)| h * (1.0 - pct)),
        ),
    };
    let reason = format!(
        "触发{}:现价 {:.3} {} {:.3}(成本 {:.3})",
        rule.label(),
        q.price,
        cmp,
        level.unwrap_or(0.0),
        p.avg_cost
    );
    Some(NewSignal {
        user_id: p.user_id,
        source: SignalSource::Exit,
        strategy_id: None,
        code: p.code.clone(),
        name: None,
        side: Direction::Sell,
        scope: scope_for(p.account),
        ref_price: q.price,
        reason,
        ai_note: None,
        dedup_key: exit_dedup_key(p, rule, now.date()),
        suggest_cash: None,
        suggest_qty: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    fn pos(account: Account) -> Position {
        let mut p = Position::empty(1, account, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0;
        p.stop_loss = Some(9.2);
        p.take_profit = Some(12.0);
        p
    }

    fn quote(price: f64) -> Quote {
        Quote {
            code: "600000".into(),
            price,
            limit_up: None,
            limit_down: None,
            ts: now(),
        }
    }

    #[test]
    fn trigger_priority_and_bounds() {
        let p = pos(Account::Real);
        assert_eq!(
            trigger(&p, 9.2),
            Some(ExitRule::StopLoss),
            "等于止损价即触发"
        );
        assert_eq!(trigger(&p, 10.0), None);
        assert_eq!(trigger(&p, 12.0), Some(ExitRule::TakeProfit));
        let mut t = p.clone();
        t.trailing_pct = Some(0.05);
        t.trailing_high = Some(11.9);
        // 11.9 × 0.95 = 11.305
        assert_eq!(trigger(&t, 11.3), Some(ExitRule::Trailing));
        assert_eq!(trigger(&t, 11.31), None);
        t.stop_loss = Some(11.5);
        assert_eq!(
            trigger(&t, 11.3),
            Some(ExitRule::StopLoss),
            "止损优先于移动止盈"
        );
        let mut empty = p.clone();
        empty.qty = 0;
        assert_eq!(trigger(&empty, 1.0), None);
        assert_eq!(trigger(&p, 0.0), None);
    }

    #[test]
    fn trailing_high_tracks_new_highs_only_when_enabled() {
        let mut p = pos(Account::Real);
        assert_eq!(next_trailing_high(&p, 13.0), None, "未启用移动止盈");
        p.trailing_pct = Some(0.05);
        assert_eq!(next_trailing_high(&p, 10.5), Some(10.5), "首次记录");
        p.trailing_high = Some(12.0);
        assert_eq!(next_trailing_high(&p, 12.5), Some(12.5));
        assert_eq!(next_trailing_high(&p, 12.0), None);
        assert_eq!(next_trailing_high(&p, 11.0), None);
    }

    #[test]
    fn exit_signal_is_scoped_and_keyed_per_account_rule_day() {
        let real = exit_signal(&pos(Account::Real), &quote(9.1), now()).unwrap();
        assert_eq!(real.dedup_key, "exit-real-600000-stop-2026-09-16");
        assert_eq!(real.scope, AccountScope::RealOnly);
        assert_eq!(
            (real.source, real.side),
            (SignalSource::Exit, Direction::Sell)
        );
        assert!(real.suggest_qty.is_none(), "全部可卖");
        assert!(real.reason.contains("止损"), "{}", real.reason);
        assert!((real.ref_price - 9.1).abs() < 1e-12);

        let paper = exit_signal(&pos(Account::Paper), &quote(12.3), now()).unwrap();
        assert_eq!(paper.dedup_key, "exit-paper-600000-take-2026-09-16");
        assert_eq!(paper.scope, AccountScope::PaperOnly);

        assert!(exit_signal(&pos(Account::Real), &quote(10.0), now()).is_none());
    }
}

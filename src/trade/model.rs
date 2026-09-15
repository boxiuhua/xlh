//! 交易核心数据模型。所有时间为本地 `NaiveDateTime`，存储为 `TS_FMT` 字符串。

use crate::event::Direction;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

pub const TS_FMT: &str = "%Y-%m-%d %H:%M:%S";
pub const DATE_FMT: &str = "%Y-%m-%d";

pub fn fmt_ts(t: NaiveDateTime) -> String {
    t.format(TS_FMT).to_string()
}

pub fn parse_ts(s: &str) -> Result<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, TS_FMT).map_err(|e| anyhow!("时间格式错误 {s}: {e}"))
}

pub fn side_str(side: Direction) -> &'static str {
    match side {
        Direction::Buy => "buy",
        Direction::Sell => "sell",
    }
}

pub fn parse_side(s: &str) -> Result<Direction> {
    match s {
        "buy" => Ok(Direction::Buy),
        "sell" => Ok(Direction::Sell),
        _ => Err(anyhow!("未知买卖方向: {s}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Account {
    /// 实盘：用户确认后线下成交、回填
    Real,
    /// 模拟盘：自动成交
    Paper,
}

impl Account {
    pub fn as_str(self) -> &'static str {
        match self {
            Account::Real => "real",
            Account::Paper => "paper",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "real" => Ok(Account::Real),
            "paper" => Ok(Account::Paper),
            _ => Err(anyhow!("未知账户类型: {s}")),
        }
    }
}

/// 信号作用的账户范围。止盈止损按账户分别发信号(实盘持仓 → RealOnly,模拟盘持仓 → PaperOnly)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountScope {
    Both,
    RealOnly,
    PaperOnly,
}

impl AccountScope {
    pub fn as_str(self) -> &'static str {
        match self {
            AccountScope::Both => "both",
            AccountScope::RealOnly => "real_only",
            AccountScope::PaperOnly => "paper_only",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "both" => Ok(AccountScope::Both),
            "real_only" => Ok(AccountScope::RealOnly),
            "paper_only" => Ok(AccountScope::PaperOnly),
            _ => Err(anyhow!("未知账户范围: {s}")),
        }
    }

    pub fn includes(self, a: Account) -> bool {
        matches!(
            (self, a),
            (AccountScope::Both, _)
                | (AccountScope::RealOnly, Account::Real)
                | (AccountScope::PaperOnly, Account::Paper)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalSource {
    /// 持仓止盈止损
    Exit,
    /// 日线策略
    Strategy,
    /// 实时异动
    Mover,
    /// 用户手动 / AI 建议
    Manual,
}

impl SignalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalSource::Exit => "exit",
            SignalSource::Strategy => "strategy",
            SignalSource::Mover => "mover",
            SignalSource::Manual => "manual",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "exit" => Ok(SignalSource::Exit),
            "strategy" => Ok(SignalSource::Strategy),
            "mover" => Ok(SignalSource::Mover),
            "manual" => Ok(SignalSource::Manual),
            _ => Err(anyhow!("未知信号来源: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TicketStatus {
    Pending,
    Confirmed,
    Partial,
    Filled,
    Expired,
    Rejected,
    Cancelled,
}

impl TicketStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TicketStatus::Pending => "pending",
            TicketStatus::Confirmed => "confirmed",
            TicketStatus::Partial => "partial",
            TicketStatus::Filled => "filled",
            TicketStatus::Expired => "expired",
            TicketStatus::Rejected => "rejected",
            TicketStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => TicketStatus::Pending,
            "confirmed" => TicketStatus::Confirmed,
            "partial" => TicketStatus::Partial,
            "filled" => TicketStatus::Filled,
            "expired" => TicketStatus::Expired,
            "rejected" => TicketStatus::Rejected,
            "cancelled" => TicketStatus::Cancelled,
            _ => return Err(anyhow!("未知工单状态: {s}")),
        })
    }

    /// 未完结：待确认 / 待成交 / 部分成交
    pub fn is_open(self) -> bool {
        matches!(
            self,
            TicketStatus::Pending | TicketStatus::Confirmed | TicketStatus::Partial
        )
    }
}

/// 待提交的交易信号（四类来源统一结构）。
#[derive(Debug, Clone, PartialEq)]
pub struct NewSignal {
    pub user_id: i64,
    pub source: SignalSource,
    pub strategy_id: Option<i64>,
    pub code: String,
    pub name: Option<String>,
    pub side: Direction,
    pub scope: AccountScope,
    pub ref_price: f64,
    pub reason: String,
    pub ai_note: Option<String>,
    /// 同一用户内唯一，用于幂等去重
    pub dedup_key: String,
    /// 买入建议金额；None 则按单笔上限
    pub suggest_cash: Option<f64>,
    /// 卖出建议股数；None 则全部可卖
    pub suggest_qty: Option<u64>,
}

/// 实时报价（不复权）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Quote {
    pub code: String,
    pub price: f64,
    pub limit_up: Option<f64>,
    pub limit_down: Option<f64>,
    pub ts: NaiveDateTime,
}

/// 每个用户的风控规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiskRules {
    pub max_order_amount: f64,
    pub max_position_pct: f64,
    pub max_daily_tickets: u32,
    pub daily_loss_halt_pct: f64,
    pub cooldown_min: i64,
    pub deviation_th: f64,
    pub default_stop_loss_pct: f64,
    pub default_take_profit_pct: f64,
    pub slippage: f64,
    pub enabled: bool,
}

impl Default for RiskRules {
    fn default() -> Self {
        Self {
            max_order_amount: 50_000.0,
            max_position_pct: 0.20,
            max_daily_tickets: 20,
            daily_loss_halt_pct: 0.03,
            cooldown_min: 60,
            deviation_th: 0.015,
            default_stop_loss_pct: 0.08,
            default_take_profit_pct: 0.20,
            slippage: 0.001,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AccountState {
    pub user_id: i64,
    pub account: Account,
    pub total_capital: f64,
    pub available_cash: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Position {
    pub user_id: i64,
    pub account: Account,
    pub code: String,
    pub qty: u64,
    /// 含买入费用的成本价
    pub avg_cost: f64,
    pub today_bought_qty: u64,
    pub last_buy_date: Option<NaiveDate>,
    pub stop_loss: Option<f64>,
    pub take_profit: Option<f64>,
    pub trailing_pct: Option<f64>,
    pub trailing_high: Option<f64>,
}

impl Position {
    pub fn empty(user_id: i64, account: Account, code: &str) -> Self {
        Self {
            user_id,
            account,
            code: code.to_string(),
            qty: 0,
            avg_cost: 0.0,
            today_bought_qty: 0,
            last_buy_date: None,
            stop_loss: None,
            take_profit: None,
            trailing_pct: None,
            trailing_high: None,
        }
    }

    /// T+1：当日买入份额当日不可卖。
    pub fn sellable(&self, today: NaiveDate) -> u64 {
        if self.last_buy_date == Some(today) {
            self.qty.saturating_sub(self.today_bought_qty)
        } else {
            self.qty
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ticket {
    pub id: i64,
    pub user_id: i64,
    pub signal_id: i64,
    pub account: Account,
    pub code: String,
    pub side: Direction,
    pub suggest_price: f64,
    pub qty: u64,
    pub filled_qty: u64,
    pub expires_at: NaiveDateTime,
    pub deviation_th: f64,
    pub status: TicketStatus,
    pub urgency: i64,
    pub created_at: NaiveDateTime,
    pub confirmed_at: Option<NaiveDateTime>,
    pub ignore_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn enums_round_trip_through_strings() {
        for a in [Account::Real, Account::Paper] {
            assert_eq!(Account::parse(a.as_str()).unwrap(), a);
        }
        for s in [
            SignalSource::Exit,
            SignalSource::Strategy,
            SignalSource::Mover,
            SignalSource::Manual,
        ] {
            assert_eq!(SignalSource::parse(s.as_str()).unwrap(), s);
        }
        for t in [
            TicketStatus::Pending,
            TicketStatus::Confirmed,
            TicketStatus::Partial,
            TicketStatus::Filled,
            TicketStatus::Expired,
            TicketStatus::Rejected,
            TicketStatus::Cancelled,
        ] {
            assert_eq!(TicketStatus::parse(t.as_str()).unwrap(), t);
        }
        for sc in [
            AccountScope::Both,
            AccountScope::RealOnly,
            AccountScope::PaperOnly,
        ] {
            assert_eq!(AccountScope::parse(sc.as_str()).unwrap(), sc);
        }
        assert_eq!(
            parse_side(side_str(Direction::Sell)).unwrap(),
            Direction::Sell
        );
        assert!(Account::parse("x").is_err());
        let t = day(15).and_hms_opt(9, 30, 5).unwrap();
        assert_eq!(parse_ts(&fmt_ts(t)).unwrap(), t);
    }

    #[test]
    fn open_statuses() {
        assert!(TicketStatus::Pending.is_open());
        assert!(TicketStatus::Partial.is_open());
        assert!(!TicketStatus::Filled.is_open());
        assert!(!TicketStatus::Expired.is_open());
    }

    #[test]
    fn sellable_applies_t_plus_one() {
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.today_bought_qty = 300;
        p.last_buy_date = Some(day(15));
        assert_eq!(p.sellable(day(15)), 700);
        assert_eq!(p.sellable(day(16)), 1000);
    }

    #[test]
    fn risk_rules_defaults_and_partial_json() {
        let r: RiskRules = serde_json::from_str(r#"{"cooldown_min": 30}"#).unwrap();
        assert_eq!(r.cooldown_min, 30);
        assert!((r.max_order_amount - 50_000.0).abs() < 1e-9);
        assert!(r.enabled);
    }
}

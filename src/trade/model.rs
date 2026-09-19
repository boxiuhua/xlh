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

impl RiskRules {
    /// 风控规则范围校验(计划 4a):落库前拦住非法值,阈值一律带范围校验。
    pub fn validate(&self) -> Result<()> {
        let in_range = |v: f64, lo: f64, hi: f64, lo_incl: bool, hi_incl: bool| {
            v.is_finite()
                && (if lo_incl { v >= lo } else { v > lo })
                && (if hi_incl { v <= hi } else { v < hi })
        };
        if !(self.max_order_amount.is_finite() && self.max_order_amount > 0.0) {
            return Err(anyhow!("单笔限额必须为正数: {}", self.max_order_amount));
        }
        if !in_range(self.max_position_pct, 0.0, 1.0, false, true) {
            return Err(anyhow!(
                "单只仓位占比须在 (0, 1] 之间: {}",
                self.max_position_pct
            ));
        }
        if self.max_daily_tickets < 1 {
            return Err(anyhow!("每日工单上限须 >= 1: {}", self.max_daily_tickets));
        }
        if !in_range(self.daily_loss_halt_pct, 0.0, 0.5, false, true) {
            return Err(anyhow!(
                "日内亏损熔断比例须在 (0, 0.5] 之间: {}",
                self.daily_loss_halt_pct
            ));
        }
        if self.cooldown_min < 0 {
            return Err(anyhow!("冷却分钟数须 >= 0: {}", self.cooldown_min));
        }
        if !in_range(self.deviation_th, 0.0, 0.1, false, true) {
            return Err(anyhow!("偏离阈值须在 (0, 0.1] 之间: {}", self.deviation_th));
        }
        if !in_range(self.default_stop_loss_pct, 0.0, 0.5, false, true) {
            return Err(anyhow!(
                "默认止损比例须在 (0, 0.5] 之间: {}",
                self.default_stop_loss_pct
            ));
        }
        if !in_range(self.default_take_profit_pct, 0.0, 5.0, false, true) {
            return Err(anyhow!(
                "默认止盈比例须在 (0, 5] 之间: {}",
                self.default_take_profit_pct
            ));
        }
        if !in_range(self.slippage, 0.0, 0.05, true, true) {
            return Err(anyhow!("滑点须在 [0, 0.05] 之间: {}", self.slippage));
        }
        Ok(())
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

/// 策略生命周期(spec §10.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyStatus {
    /// 草稿:定义已保存,未提交评估
    Draft,
    /// 回测中:前推回测排队 / 运行中
    Backtesting,
    /// 未通过:回测或观察期不达标
    Failed,
    /// 观察期:仅模拟盘
    Paper,
    /// 已准入:实盘 + 模拟盘
    Admitted,
    /// 已暂停:实盘表现异常,退回观察期前需重新提交
    Suspended,
}

impl StrategyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyStatus::Draft => "draft",
            StrategyStatus::Backtesting => "backtesting",
            StrategyStatus::Failed => "failed",
            StrategyStatus::Paper => "paper",
            StrategyStatus::Admitted => "admitted",
            StrategyStatus::Suspended => "suspended",
        }
    }

    /// 中文文案,与交易页 `STRATEGY_STATUS` 保持一致(日报等推送用)。
    pub fn label_zh(self) -> &'static str {
        match self {
            StrategyStatus::Draft => "草稿",
            StrategyStatus::Backtesting => "回测中",
            StrategyStatus::Failed => "未通过",
            StrategyStatus::Paper => "观察期",
            StrategyStatus::Admitted => "已准入",
            StrategyStatus::Suspended => "已暂停",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "draft" => StrategyStatus::Draft,
            "backtesting" => StrategyStatus::Backtesting,
            "failed" => StrategyStatus::Failed,
            "paper" => StrategyStatus::Paper,
            "admitted" => StrategyStatus::Admitted,
            "suspended" => StrategyStatus::Suspended,
            _ => return Err(anyhow!("未知策略状态: {s}")),
        })
    }
}

/// 待创建 / 待更新的策略定义。策略 = 类型 + 参数网格 + 股票池。
#[derive(Debug, Clone, PartialEq)]
pub struct NewStrategy {
    pub user_id: i64,
    pub name: String,
    /// 策略类型,见 `crate::config::build_strategy_from`
    pub kind: String,
    /// 参数网格(TOML 表文本),每个训练窗在其中选参
    pub grid_toml: String,
    pub pool: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StrategyDef {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub kind: String,
    pub grid_toml: String,
    pub pool: Vec<String>,
    pub version_hash: String,
    pub status: StrategyStatus,
    pub status_reason: Option<String>,
    pub updated_at: NaiveDateTime,
}

/// 评估任务类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    /// 滚动前推回测
    WalkForward,
    /// 观察期检查
    PaperCheck,
    /// 实盘表现监控
    Watchdog,
}

impl EvalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvalKind::WalkForward => "walk_forward",
            EvalKind::PaperCheck => "paper_check",
            EvalKind::Watchdog => "watchdog",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "walk_forward" => EvalKind::WalkForward,
            "paper_check" => EvalKind::PaperCheck,
            "watchdog" => EvalKind::Watchdog,
            _ => return Err(anyhow!("未知评估任务类型: {s}")),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            _ => return Err(anyhow!("未知任务状态: {s}")),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvalJob {
    pub id: i64,
    pub user_id: i64,
    pub strategy_id: i64,
    pub kind: EvalKind,
    pub status: JobStatus,
    pub progress: Option<String>,
    pub error: Option<String>,
    pub created_at: NaiveDateTime,
    /// 领取时间(F7):供计划 4 展示任务耗时。
    pub started_at: Option<NaiveDateTime>,
    /// 结束时间(F7):供计划 4 展示任务耗时。
    pub finished_at: Option<NaiveDateTime>,
}

/// 定义指纹:类型 + 网格 + 排序后的股票池。定义一变即换版本,状态回到草稿。
pub fn strategy_version_hash(kind: &str, grid_toml: &str, pool: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&str> = pool.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"\n");
    h.update(grid_toml.as_bytes());
    h.update(b"\n");
    h.update(sorted.join(",").as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
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
        for k in [
            EvalKind::WalkForward,
            EvalKind::PaperCheck,
            EvalKind::Watchdog,
        ] {
            assert_eq!(EvalKind::parse(k.as_str()).unwrap(), k);
        }
        for js in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Failed,
        ] {
            assert_eq!(JobStatus::parse(js.as_str()).unwrap(), js);
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

    #[test]
    fn strategy_status_round_trip_and_hash_is_order_insensitive() {
        for s in [
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            StrategyStatus::Failed,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            StrategyStatus::Suspended,
        ] {
            assert_eq!(StrategyStatus::parse(s.as_str()).unwrap(), s);
        }
        assert!(StrategyStatus::parse("x").is_err());

        let a = strategy_version_hash(
            "rsi",
            "rsi_window = [14]",
            &["600000".into(), "000001".into()],
        );
        let b = strategy_version_hash(
            "rsi",
            "rsi_window = [14]",
            &["000001".into(), "600000".into()],
        );
        assert_eq!(a, b, "池内顺序不影响版本");
        assert_eq!(a.len(), 16);
        assert_ne!(
            a,
            strategy_version_hash("rsi", "rsi_window = [20]", &["600000".into()])
        );
        assert_ne!(
            a,
            strategy_version_hash(
                "trend",
                "rsi_window = [14]",
                &["600000".into(), "000001".into()]
            )
        );
    }

    #[test]
    fn risk_rules_validation_rejects_out_of_range_values() {
        assert!(RiskRules::default().validate().is_ok());
        let bad = [
            RiskRules {
                max_order_amount: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                max_position_pct: 1.5,
                ..RiskRules::default()
            },
            RiskRules {
                max_position_pct: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                max_daily_tickets: 0,
                ..RiskRules::default()
            },
            RiskRules {
                daily_loss_halt_pct: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                daily_loss_halt_pct: 0.6,
                ..RiskRules::default()
            },
            RiskRules {
                cooldown_min: -1,
                ..RiskRules::default()
            },
            RiskRules {
                deviation_th: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                deviation_th: 0.2,
                ..RiskRules::default()
            },
            RiskRules {
                default_stop_loss_pct: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                default_stop_loss_pct: 0.6,
                ..RiskRules::default()
            },
            RiskRules {
                default_take_profit_pct: 0.0,
                ..RiskRules::default()
            },
            RiskRules {
                default_take_profit_pct: 5.1,
                ..RiskRules::default()
            },
            RiskRules {
                slippage: -0.01,
                ..RiskRules::default()
            },
            RiskRules {
                slippage: 0.06,
                ..RiskRules::default()
            },
            RiskRules {
                max_order_amount: f64::NAN,
                ..RiskRules::default()
            },
        ];
        for r in bad {
            assert!(r.validate().is_err(), "{r:?}");
        }
    }
}

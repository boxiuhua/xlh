//! `config.toml` 的 `[trade]` 段。段缺失 → 默认;段非法 → 报错(调用方决定禁用监听)。

use crate::trade::admission::walk_forward::WalkForwardCfg;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 策略准入阈值(spec §10.4)。管理员可在 `[trade.admission]` 调整;用户侧只可调严。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdmissionCfg {
    pub min_oos_sharpe: f64,
    pub max_oos_drawdown: f64,
    pub min_oos_trades: usize,
    /// 1 − 样本外夏普 / 样本内夏普 的上限
    pub max_sharpe_decay: f64,
    pub min_positive_ratio: f64,
    /// 池内可评估股票占比下限(见 F5):`requested` 为 0 时不检查。
    pub min_evaluated_ratio: f64,
    pub min_years: f64,
    pub paper_days: i64,
    pub paper_trades: usize,
    pub mover_paper_days: i64,
    pub mover_paper_trades: usize,
    /// 实盘回撤超过「回测最大回撤 × 该倍数」即暂停
    pub drawdown_multiple: f64,
    /// 胜率下限 = 回测胜率 − 该倍数 × σ
    pub win_rate_sigma: f64,
    /// 连亏超过「回测最长连亏 × 该倍数」即暂停
    pub streak_multiple: f64,
    /// 胜率判定的滚动窗口(笔)
    pub watchdog_window: usize,
    /// 少于该笔数不做胜率判定
    pub watchdog_min_trades: usize,
}

impl Default for AdmissionCfg {
    fn default() -> Self {
        Self {
            min_oos_sharpe: 0.8,
            max_oos_drawdown: 0.25,
            min_oos_trades: 30,
            max_sharpe_decay: 0.5,
            min_positive_ratio: 0.55,
            min_evaluated_ratio: 0.6,
            min_years: 3.0,
            paper_days: 20,
            paper_trades: 10,
            mover_paper_days: 40,
            mover_paper_trades: 30,
            drawdown_multiple: 1.5,
            win_rate_sigma: 2.0,
            streak_multiple: 1.5,
            watchdog_window: 20,
            watchdog_min_trades: 5,
        }
    }
}

/// 前推回测的窗口切分与选参依据(见 F10)。转换成 `WalkForwardCfg` 时另外
/// 传入 `[trade].slippage`——`walk_forward.rs` 不依赖 `config` 模块。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WalkForwardTuning {
    pub train_days: i64,
    pub test_days: i64,
    pub step_days: i64,
    /// 训练窗选参依据:sharpe | total_return | annualized | max_drawdown
    pub metric: String,
    /// 初始现金。默认 0(同 `WalkForwardCfg`):组合按需注入资金,收益与回撤
    /// 都相对「实际投入的资金」度量。
    pub initial_cash: f64,
}

impl Default for WalkForwardTuning {
    fn default() -> Self {
        Self {
            train_days: 730,
            test_days: 182,
            step_days: 182,
            metric: "sharpe".into(),
            initial_cash: 0.0,
        }
    }
}

impl WalkForwardTuning {
    pub fn to_cfg(&self, slippage: f64) -> WalkForwardCfg {
        WalkForwardCfg {
            train_days: self.train_days,
            test_days: self.test_days,
            step_days: self.step_days,
            metric: self.metric.clone(),
            initial_cash: self.initial_cash,
            slippage,
        }
    }
}

/// 评估线程配置(计划 3d):自动入队与执行前推回测/观察期检查/看门狗。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvalCfg {
    /// 是否启动评估线程
    pub enabled: bool,
    /// 空闲时轮询队列的间隔(秒)
    pub poll_secs: u64,
    /// 每日入队 PaperCheck / Watchdog 的时刻
    pub daily_hour: u32,
    pub daily_minute: u32,
    /// 每月重跑 WalkForward 的日、时
    pub monthly_day: u32,
    pub monthly_hour: u32,
}

impl Default for EvalCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_secs: 30,
            daily_hour: 16,
            daily_minute: 30,
            monthly_day: 1,
            monthly_hour: 17,
        }
    }
}

/// 日线策略信号配置(计划 3e):收盘后计算、次日开盘发出。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalCfg {
    /// 是否计算并发出日线策略信号
    pub enabled: bool,
    /// 收盘后开始计算的时刻
    pub compute_hour: u32,
    pub compute_minute: u32,
    /// 计算截止(整点,不含):到点后当日不再重试
    pub cutoff_hour: u32,
    /// K 线未更新时的重试间隔(分钟)
    pub retry_minutes: i64,
    /// 已证实开市的日子,到这个整点仍没有当天 K 线就按停牌记「无操作」,
    /// 不再每轮整段重抓历史直到截止;不早于截止时刻则等于关闭此规则
    pub no_bar_idle_hour: u32,
    /// 次日发出窗口 [emit, emit_end)
    pub emit_hour: u32,
    pub emit_minute: u32,
    pub emit_end_hour: u32,
    pub emit_end_minute: u32,
}

impl Default for SignalCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            compute_hour: 15,
            compute_minute: 30,
            cutoff_hour: 21,
            retry_minutes: 10,
            no_bar_idle_hour: 17,
            emit_hour: 9,
            emit_minute: 25,
            emit_end_hour: 10,
            emit_end_minute: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeCfg {
    /// 是否启动交易监听线程
    pub enabled: bool,
    /// 交易时段报价轮询间隔(秒)
    pub monitor_interval_secs: u64,
    /// 连续失败多久后推送「止损监听中断」(秒)
    pub alert_after_secs: i64,
    /// 是否把实时异动转为(观察期)交易信号
    pub mover_signals: bool,
    pub admission: AdmissionCfg,
    /// 前推回测成交滑点(比例)
    pub slippage: f64,
    pub walk_forward: WalkForwardTuning,
    pub eval: EvalCfg,
    pub signals: SignalCfg,
    /// 推送里工单链接的站点根地址(如 https://xlh.example.com);为空则不带链接
    pub link_base_url: String,
}

impl Default for TradeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor_interval_secs: 15,
            alert_after_secs: 180,
            mover_signals: true,
            admission: AdmissionCfg::default(),
            slippage: 0.001,
            walk_forward: WalkForwardTuning::default(),
            eval: EvalCfg::default(),
            signals: SignalCfg::default(),
            link_base_url: String::new(),
        }
    }
}

static CFG: std::sync::OnceLock<TradeCfg> = std::sync::OnceLock::new();

pub fn from_toml_str(text: &str) -> Result<TradeCfg> {
    #[derive(Deserialize)]
    struct Root {
        trade: Option<TradeCfg>,
    }
    let root: Root = toml::from_str(text).map_err(|e| anyhow!("[trade] 段解析失败: {e}"))?;
    let cfg = root.trade.unwrap_or_default();
    if cfg.monitor_interval_secs < 5 {
        return Err(anyhow!(
            "[trade] monitor_interval_secs 须 ≥ 5,当前 {}",
            cfg.monitor_interval_secs
        ));
    }
    if cfg.monitor_interval_secs > 3600 {
        return Err(anyhow!(
            "[trade] monitor_interval_secs 须 ≤ 3600,当前 {}",
            cfg.monitor_interval_secs
        ));
    }
    if cfg.alert_after_secs < 60 {
        return Err(anyhow!(
            "[trade] alert_after_secs 须 ≥ 60,当前 {}",
            cfg.alert_after_secs
        ));
    }
    if !(0.0..=0.05).contains(&cfg.slippage) {
        return Err(anyhow!(
            "[trade] slippage 须在 [0,0.05],当前 {}",
            cfg.slippage
        ));
    }
    if !cfg.link_base_url.is_empty()
        && !cfg.link_base_url.starts_with("http://")
        && !cfg.link_base_url.starts_with("https://")
    {
        return Err(anyhow!(
            "[trade] link_base_url 须为空或以 http:// / https:// 开头,当前 {}",
            cfg.link_base_url
        ));
    }
    let wf = &cfg.walk_forward;
    if wf.train_days <= 0 {
        return Err(anyhow!(
            "[trade.walk_forward] train_days 须 > 0,当前 {}",
            wf.train_days
        ));
    }
    if wf.test_days <= 0 {
        return Err(anyhow!(
            "[trade.walk_forward] test_days 须 > 0,当前 {}",
            wf.test_days
        ));
    }
    if wf.step_days <= 0 {
        return Err(anyhow!(
            "[trade.walk_forward] step_days 须 > 0,当前 {}",
            wf.step_days
        ));
    }
    if wf.step_days < wf.test_days {
        return Err(anyhow!(
            "[trade.walk_forward] step_days 须 >= test_days,当前 {} < {}",
            wf.step_days,
            wf.test_days
        ));
    }
    crate::trade::admission::walk_forward::validate_metric(&wf.metric)
        .map_err(|e| anyhow!("[trade.walk_forward] {e}"))?;
    if wf.initial_cash < 0.0 {
        return Err(anyhow!(
            "[trade.walk_forward] initial_cash 须 >= 0,当前 {}",
            wf.initial_cash
        ));
    }
    let a = &cfg.admission;
    for (name, v) in [
        ("min_oos_sharpe", a.min_oos_sharpe),
        ("max_sharpe_decay", a.max_sharpe_decay),
        ("min_positive_ratio", a.min_positive_ratio),
        ("min_evaluated_ratio", a.min_evaluated_ratio),
        ("min_years", a.min_years),
        ("max_oos_drawdown", a.max_oos_drawdown),
    ] {
        if !v.is_finite() {
            return Err(anyhow!("[trade.admission] {name} 必须是有限数,当前 {v}"));
        }
    }
    if !(a.max_oos_drawdown > 0.0 && a.max_oos_drawdown <= 1.0) {
        return Err(anyhow!(
            "[trade.admission] max_oos_drawdown 须在 (0,1],当前 {}",
            a.max_oos_drawdown
        ));
    }
    if !(0.0..=1.0).contains(&a.min_positive_ratio) {
        return Err(anyhow!(
            "[trade.admission] min_positive_ratio 须在 [0,1],当前 {}",
            a.min_positive_ratio
        ));
    }
    if !(0.0..=1.0).contains(&a.min_evaluated_ratio) {
        return Err(anyhow!(
            "[trade.admission] min_evaluated_ratio 须在 [0,1],当前 {}",
            a.min_evaluated_ratio
        ));
    }
    if !(0.0..=1.0).contains(&a.max_sharpe_decay) {
        return Err(anyhow!(
            "[trade.admission] max_sharpe_decay 须在 [0,1],当前 {}",
            a.max_sharpe_decay
        ));
    }
    if a.min_oos_sharpe < 0.0 {
        return Err(anyhow!(
            "[trade.admission] min_oos_sharpe 须 >= 0,当前 {}",
            a.min_oos_sharpe
        ));
    }
    if a.min_years <= 0.0 {
        return Err(anyhow!(
            "[trade.admission] min_years 须 > 0,当前 {}",
            a.min_years
        ));
    }
    if a.min_oos_trades == 0 {
        return Err(anyhow!(
            "[trade.admission] min_oos_trades 须 >= 1,当前 {}",
            a.min_oos_trades
        ));
    }
    if a.paper_days <= 0 {
        return Err(anyhow!(
            "[trade.admission] paper_days 须 > 0,当前 {}",
            a.paper_days
        ));
    }
    if a.paper_trades == 0 {
        return Err(anyhow!(
            "[trade.admission] paper_trades 须 >= 1,当前 {}",
            a.paper_trades
        ));
    }
    if a.mover_paper_days <= 0 {
        return Err(anyhow!(
            "[trade.admission] mover_paper_days 须 > 0,当前 {}",
            a.mover_paper_days
        ));
    }
    if a.mover_paper_trades == 0 {
        return Err(anyhow!(
            "[trade.admission] mover_paper_trades 须 >= 1,当前 {}",
            a.mover_paper_trades
        ));
    }
    for (name, v) in [
        ("drawdown_multiple", a.drawdown_multiple),
        ("streak_multiple", a.streak_multiple),
    ] {
        if !(v.is_finite() && v >= 1.0) {
            return Err(anyhow!("[trade.admission] {name} 须 >= 1,当前 {v}"));
        }
    }
    if !(a.win_rate_sigma.is_finite() && a.win_rate_sigma >= 0.0) {
        return Err(anyhow!(
            "[trade.admission] win_rate_sigma 须 >= 0,当前 {}",
            a.win_rate_sigma
        ));
    }
    if a.watchdog_window == 0 {
        return Err(anyhow!(
            "[trade.admission] watchdog_window 须 >= 1,当前 {}",
            a.watchdog_window
        ));
    }
    if a.watchdog_min_trades == 0 {
        return Err(anyhow!(
            "[trade.admission] watchdog_min_trades 须 >= 1,当前 {}",
            a.watchdog_min_trades
        ));
    }
    let e = &cfg.eval;
    if e.poll_secs < 5 {
        return Err(anyhow!(
            "[trade.eval] poll_secs 须 ≥ 5,当前 {}",
            e.poll_secs
        ));
    }
    if e.daily_hour > 23 || e.monthly_hour > 23 {
        return Err(anyhow!("[trade.eval] 小时须在 0..=23"));
    }
    if e.daily_minute > 59 {
        return Err(anyhow!("[trade.eval] daily_minute 须在 0..=59"));
    }
    // 28 之后的日子并非每月都有,会导致 2 月整月不重跑
    if e.monthly_day < 1 || e.monthly_day > 28 {
        return Err(anyhow!(
            "[trade.eval] monthly_day 须在 1..=28,当前 {}",
            e.monthly_day
        ));
    }
    let s = &cfg.signals;
    if [s.compute_hour, s.cutoff_hour, s.emit_hour, s.emit_end_hour]
        .iter()
        .any(|h| *h > 23)
    {
        return Err(anyhow!("[trade.signals] 小时须在 0..=23"));
    }
    if [s.compute_minute, s.emit_minute, s.emit_end_minute]
        .iter()
        .any(|m| *m > 59)
    {
        return Err(anyhow!("[trade.signals] 分钟须在 0..=59"));
    }
    if s.cutoff_hour * 60 <= s.compute_hour * 60 + s.compute_minute {
        return Err(anyhow!(
            "[trade.signals] cutoff_hour 须晚于开始计算时刻 {:02}:{:02},当前 {}",
            s.compute_hour,
            s.compute_minute,
            s.cutoff_hour
        ));
    }
    if s.no_bar_idle_hour > 23 || s.no_bar_idle_hour * 60 <= s.compute_hour * 60 + s.compute_minute
    {
        return Err(anyhow!(
            "[trade.signals] no_bar_idle_hour 须在 0..=23 且晚于开始计算时刻 {:02}:{:02},当前 {}",
            s.compute_hour,
            s.compute_minute,
            s.no_bar_idle_hour
        ));
    }
    if !(1..=120).contains(&s.retry_minutes) {
        return Err(anyhow!(
            "[trade.signals] retry_minutes 须在 1..=120,当前 {}",
            s.retry_minutes
        ));
    }
    let emit = s.emit_hour * 60 + s.emit_minute;
    // 09:25 集合竞价出结果之前,快照里没有当天报价
    if emit < 9 * 60 + 25 {
        return Err(anyhow!(
            "[trade.signals] 发出窗口须不早于 09:25,当前 {:02}:{:02}",
            s.emit_hour,
            s.emit_minute
        ));
    }
    if s.emit_end_hour * 60 + s.emit_end_minute <= emit {
        return Err(anyhow!("[trade.signals] 发出窗口结束须晚于开始"));
    }
    // 策略实盘工单固定在 10:30 过期(`ticket::default_expiry`,spec §5 的 09:30–10:30
    // 开盘窗口);窗口再往后开,10:30 之后发出的工单只能落到「发出后 60 分钟」的
    // 兜底期限,与「T 日开盘成交」的口径也不再相符。
    if s.emit_end_hour * 60 + s.emit_end_minute > 10 * 60 + 30 {
        return Err(anyhow!(
            "[trade.signals] 发出窗口结束须不晚于 10:30(策略实盘工单 10:30 过期),当前 {:02}:{:02}",
            s.emit_end_hour,
            s.emit_end_minute
        ));
    }
    Ok(cfg)
}

pub fn init(path: &Path) -> Result<&'static TradeCfg> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取配置 {} 失败: {e}", path.display()))?;
    let cfg = from_toml_str(&text)?;
    Ok(CFG.get_or_init(|| cfg))
}

pub fn get() -> &'static TradeCfg {
    CFG.get_or_init(TradeCfg::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_uses_defaults() {
        assert_eq!(
            from_toml_str("[realtime]\nretain_days = 10\n").unwrap(),
            TradeCfg::default()
        );
    }

    #[test]
    fn partial_section_overrides_fields() {
        let c =
            from_toml_str("[trade]\nmonitor_interval_secs = 30\nmover_signals = false\n").unwrap();
        assert_eq!(c.monitor_interval_secs, 30);
        assert!(!c.mover_signals);
        assert!(c.enabled);
    }

    #[test]
    fn invalid_values_are_errors() {
        assert!(from_toml_str("[trade]\nmonitor_interval_secs = 1\n").is_err());
        assert!(from_toml_str("[trade]\nmonitor_interval_secs = 3601\n").is_err());
        assert!(from_toml_str("[trade]\nalert_after_secs = 10\n").is_err());
        assert!(from_toml_str("[trade]\nenabled = \"yes\"\n").is_err());
    }

    #[test]
    fn admission_section_defaults_and_overrides() {
        let c = from_toml_str("[trade]\n[trade.admission]\nmin_oos_sharpe = 1.2\n").unwrap();
        assert!((c.admission.min_oos_sharpe - 1.2).abs() < 1e-9);
        assert_eq!(c.admission.min_oos_trades, 30, "未覆盖项取默认");
        assert_eq!(
            from_toml_str("[trade]\n").unwrap().admission,
            AdmissionCfg::default()
        );
    }

    #[test]
    fn admission_section_rejects_invalid_thresholds() {
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmax_oos_drawdown = 0.0\n").is_err(),
            "回撤上限须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmax_oos_drawdown = 1.5\n").is_err(),
            "回撤上限须 ≤ 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_positive_ratio = -0.1\n").is_err(),
            "正收益占比须 ≥ 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_positive_ratio = 1.1\n").is_err(),
            "正收益占比须 ≤ 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_years = 0.0\n").is_err(),
            "数据年限须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\npaper_days = 0\n").is_err(),
            "观察期天数须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmover_paper_days = 0\n").is_err(),
            "异动类观察期天数须 > 0"
        );
        // F12:补齐剩余字段的校验
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_oos_sharpe = nan\n").is_err(),
            "夏普阈值须是有限数"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_oos_sharpe = -0.1\n").is_err(),
            "夏普阈值须 >= 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmax_sharpe_decay = -0.1\n").is_err(),
            "夏普衰减上限须 >= 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmax_sharpe_decay = 1.1\n").is_err(),
            "夏普衰减上限须 <= 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_evaluated_ratio = -0.1\n").is_err(),
            "可评估占比须 >= 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_evaluated_ratio = 1.1\n").is_err(),
            "可评估占比须 <= 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_years = inf\n").is_err(),
            "数据年限须是有限数"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmax_oos_drawdown = nan\n").is_err(),
            "回撤上限须是有限数"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmin_oos_trades = 0\n").is_err(),
            "样本外交易笔数下限须 >= 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\npaper_trades = 0\n").is_err(),
            "观察期笔数下限须 >= 1"
        );
        assert!(
            from_toml_str("[trade]\n[trade.admission]\nmover_paper_trades = 0\n").is_err(),
            "异动类观察期笔数下限须 >= 1"
        );
    }

    /// F10:`[trade]` 段的 `slippage` / `[trade.walk_forward]` 默认值、覆盖与非法值。
    #[test]
    fn walk_forward_section_defaults_and_validation() {
        let c = from_toml_str("[trade]\n").unwrap();
        assert!((c.slippage - 0.001).abs() < 1e-9);
        assert_eq!(c.walk_forward, WalkForwardTuning::default());
        assert_eq!(c.walk_forward.train_days, 730);
        assert_eq!(c.walk_forward.test_days, 182);
        assert_eq!(c.walk_forward.step_days, 182);
        assert_eq!(c.walk_forward.metric, "sharpe");
        assert!((c.walk_forward.initial_cash - 0.0).abs() < 1e-9);

        let c2 = from_toml_str("[trade]\n[trade.walk_forward]\ntrain_days = 365\n").unwrap();
        assert_eq!(c2.walk_forward.train_days, 365, "覆盖项生效");
        assert_eq!(c2.walk_forward.test_days, 182, "未覆盖项取默认");

        let wf = c2.walk_forward.to_cfg(c2.slippage);
        assert_eq!(wf.train_days, 365);
        assert_eq!(wf.test_days, 182);
        assert_eq!(wf.step_days, 182);
        assert_eq!(wf.metric, "sharpe");
        assert!((wf.initial_cash - 0.0).abs() < 1e-9);
        assert!((wf.slippage - 0.001).abs() < 1e-9);

        assert!(
            from_toml_str("[trade]\nslippage = -0.001\n").is_err(),
            "滑点须 >= 0"
        );
        assert!(
            from_toml_str("[trade]\nslippage = 0.06\n").is_err(),
            "滑点须 <= 0.05"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\ntrain_days = 0\n").is_err(),
            "train_days 须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\ntest_days = 0\n").is_err(),
            "test_days 须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\nstep_days = 0\n").is_err(),
            "step_days 须 > 0"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\nstep_days = 10\ntest_days = 30\n")
                .is_err(),
            "step_days 须 >= test_days"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\nmetric = \"foo\"\n").is_err(),
            "metric 须是枚举值之一"
        );
        assert!(
            from_toml_str("[trade]\n[trade.walk_forward]\ninitial_cash = -1.0\n").is_err(),
            "initial_cash 须 >= 0"
        );
    }

    #[test]
    fn eval_cfg_defaults_and_validation() {
        let c = from_toml_str("[trade]\n").unwrap().eval;
        assert!(c.enabled);
        assert_eq!(c.poll_secs, 30);
        assert_eq!((c.daily_hour, c.daily_minute), (16, 30));
        assert_eq!((c.monthly_day, c.monthly_hour), (1, 17));

        let c = from_toml_str("[trade.eval]\npoll_secs = 60\ndaily_hour = 15\n")
            .unwrap()
            .eval;
        assert_eq!((c.poll_secs, c.daily_hour), (60, 15));

        assert!(
            from_toml_str("[trade.eval]\npoll_secs = 0\n").is_err(),
            "轮询须 ≥ 5 秒"
        );
        assert!(from_toml_str("[trade.eval]\ndaily_hour = 24\n").is_err());
        assert!(from_toml_str("[trade.eval]\ndaily_minute = 60\n").is_err());
        assert!(from_toml_str("[trade.eval]\nmonthly_day = 0\n").is_err());
        assert!(
            from_toml_str("[trade.eval]\nmonthly_day = 29\n").is_err(),
            "避开 2 月无此日"
        );
        assert!(from_toml_str("[trade.eval]\nmonthly_hour = 24\n").is_err());
        assert!(
            from_toml_str("[trade.eval]\nunknown = 1\n").is_err(),
            "拒绝未知键"
        );
    }

    #[test]
    fn watchdog_thresholds_default_and_validate() {
        let c = from_toml_str("[trade]\n").unwrap().admission;
        assert_eq!(
            (c.drawdown_multiple, c.win_rate_sigma, c.streak_multiple),
            (1.5, 2.0, 1.5)
        );
        assert_eq!((c.watchdog_window, c.watchdog_min_trades), (20, 5));
        assert!(
            from_toml_str("[trade.admission]\ndrawdown_multiple = 0.5\n").is_err(),
            "须 ≥ 1"
        );
        assert!(from_toml_str("[trade.admission]\nwin_rate_sigma = -1.0\n").is_err());
        assert!(from_toml_str("[trade.admission]\nwatchdog_window = 0\n").is_err());
    }

    #[test]
    fn link_base_url_defaults_empty_and_validates_scheme() {
        assert_eq!(from_toml_str("[trade]\n").unwrap().link_base_url, "");
        let c = from_toml_str("[trade]\nlink_base_url = \"https://x\"\n").unwrap();
        assert_eq!(c.link_base_url, "https://x");
        let c = from_toml_str("[trade]\nlink_base_url = \"http://x\"\n").unwrap();
        assert_eq!(c.link_base_url, "http://x");
        assert!(
            from_toml_str("[trade]\nlink_base_url = \"ftp://x\"\n").is_err(),
            "只允许 http(s)://"
        );
        assert!(
            from_toml_str("[trade]\nlink_base_url = \"x\"\n").is_err(),
            "只允许 http(s)://"
        );
    }

    #[test]
    fn signals_section_defaults_and_validation() {
        let cfg = from_toml_str("").unwrap();
        assert_eq!(cfg.signals, SignalCfg::default());
        assert!(cfg.signals.enabled);
        assert_eq!(
            (cfg.signals.compute_hour, cfg.signals.compute_minute),
            (15, 30)
        );
        assert_eq!((cfg.signals.emit_hour, cfg.signals.emit_minute), (9, 25));
        let cfg = from_toml_str("[trade.signals]\nenabled = false\nretry_minutes = 5").unwrap();
        assert!(!cfg.signals.enabled);
        assert_eq!(cfg.signals.retry_minutes, 5);
        for bad in [
            "[trade.signals]\ncompute_hour = 24",
            "[trade.signals]\ncompute_minute = 60",
            "[trade.signals]\ncutoff_hour = 15", // 截止必须晚于开始
            "[trade.signals]\nretry_minutes = 0",
            "[trade.signals]\nretry_minutes = 121",
            "[trade.signals]\nno_bar_idle_hour = 15", // 须晚于开始计算
            "[trade.signals]\nno_bar_idle_hour = 24",
            "[trade.signals]\nemit_end_hour = 9\nemit_end_minute = 25", // 窗口为空
            "[trade.signals]\nemit_hour = 8",                           // 早于集合竞价结束
            "[trade.signals]\nemit_end_minute = 31",                    // 晚于实盘工单 10:30 过期
            "[trade.signals]\nemit_end_hour = 11\nemit_end_minute = 0",
            "[trade.signals]\nunknown = 1",
        ] {
            assert!(from_toml_str(bad).is_err(), "{bad}");
        }
    }
}

//! `config.toml` 的 `[trade]` 段。段缺失 → 默认;段非法 → 报错(调用方决定禁用监听)。

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
    pub min_years: f64,
    pub paper_days: i64,
    pub paper_trades: usize,
    pub mover_paper_days: i64,
    pub mover_paper_trades: usize,
}

impl Default for AdmissionCfg {
    fn default() -> Self {
        Self {
            min_oos_sharpe: 0.8,
            max_oos_drawdown: 0.25,
            min_oos_trades: 30,
            max_sharpe_decay: 0.5,
            min_positive_ratio: 0.55,
            min_years: 3.0,
            paper_days: 20,
            paper_trades: 10,
            mover_paper_days: 40,
            mover_paper_trades: 30,
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
}

impl Default for TradeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor_interval_secs: 15,
            alert_after_secs: 180,
            mover_signals: true,
            admission: AdmissionCfg::default(),
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
}

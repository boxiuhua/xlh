//! `config.toml` 的 `[trade]` 段。段缺失 → 默认;段非法 → 报错(调用方决定禁用监听)。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
}

impl Default for TradeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor_interval_secs: 15,
            alert_after_secs: 180,
            mover_signals: true,
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
}

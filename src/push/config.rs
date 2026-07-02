//! 推送配置 `push.toml` 解析与校验。
use std::path::{Path, PathBuf};
use std::str::FromStr;
use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::holdings::Holding;

const CHANNELS: [&str; 4] = ["dingtalk", "feishu", "wework", "serverchan"];

#[derive(Debug, Clone, Deserialize)]
pub struct PushConfig {
    pub schedule: ScheduleCfg,
    pub channel: ChannelCfg,
    #[serde(default)]
    pub portfolio: PortfolioCfg,
    #[serde(default)]
    pub holdings: Vec<Holding>,
    #[serde(default)]
    pub diagnose: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleCfg {
    pub cron: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelCfg {
    /// dingtalk | feishu | wework | serverchan
    pub kind: String,
    #[serde(default)]
    pub webhook: String,
    /// 钉钉/飞书加签密钥（可选）；serverchan 时 webhook 填 sendkey。
    #[serde(default)]
    pub secret: String,
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
}

fn default_cache_dir() -> PathBuf { PathBuf::from(".cache") }

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PortfolioCfg {
    #[serde(default)]
    pub total_amount: Option<f64>,
    #[serde(default)]
    pub total_profit: Option<f64>,
    #[serde(default)]
    pub cumulative_profit: Option<f64>,
}

pub fn load(path: &Path) -> Result<PushConfig> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取推送配置 {} 失败: {e}", path.display()))?;
    let cfg: PushConfig = toml::from_str(&text).map_err(|e| anyhow!("推送配置解析失败: {e}"))?;
    validate(&cfg)?;
    Ok(cfg)
}

pub fn validate(cfg: &PushConfig) -> Result<()> {
    if !CHANNELS.contains(&cfg.channel.kind.as_str()) {
        return Err(anyhow!("未知推送渠道 kind={}（支持 {:?}）", cfg.channel.kind, CHANNELS));
    }
    if cfg.channel.webhook.trim().is_empty() {
        return Err(anyhow!("channel.webhook 不能为空"));
    }
    cron::Schedule::from_str(&cfg.schedule.cron)
        .map_err(|e| anyhow!("cron 表达式非法 '{}': {e}", cfg.schedule.cron))?;
    if cfg.holdings.is_empty() && cfg.diagnose.is_empty() {
        return Err(anyhow!("holdings 与 diagnose 至少配置一项"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意 TOML：根级键 diagnose 必须在 [[holdings]] 之前，否则会被并入 holdings 表。
    const SAMPLE: &str = r#"
diagnose = ["110022"]

[schedule]
cron = "0 30 8 * * *"

[channel]
kind = "feishu"
webhook = "https://open.feishu.cn/open-apis/bot/v2/hook/xxx"
secret = "abc"

[portfolio]
total_amount = 30000
total_profit = 1800

[[holdings]]
code = "161725"
amount = 12000
profit = 900
"#;

    #[test]
    fn parses_sample() {
        let cfg: PushConfig = toml::from_str(SAMPLE).unwrap();
        validate(&cfg).unwrap();
        assert_eq!(cfg.channel.kind, "feishu");
        assert_eq!(cfg.schedule.cron, "0 30 8 * * *");
        assert_eq!(cfg.holdings.len(), 1);
        assert_eq!(cfg.holdings[0].code, "161725");
        assert_eq!(cfg.diagnose, vec!["110022".to_string()]);
        assert_eq!(cfg.portfolio.total_amount, Some(30000.0));
        assert_eq!(cfg.channel.cache_dir, PathBuf::from(".cache"), "cache_dir 默认 .cache");
    }

    #[test]
    fn rejects_unknown_channel() {
        let cfg: PushConfig = toml::from_str(&SAMPLE.replace("feishu", "qq")).unwrap();
        assert!(validate(&cfg).unwrap_err().to_string().contains("未知推送渠道"));
    }

    #[test]
    fn rejects_empty_webhook() {
        let t = SAMPLE.replace("https://open.feishu.cn/open-apis/bot/v2/hook/xxx", "");
        let cfg: PushConfig = toml::from_str(&t).unwrap();
        assert!(validate(&cfg).unwrap_err().to_string().contains("webhook"));
    }

    #[test]
    fn rejects_bad_cron() {
        let cfg: PushConfig = toml::from_str(&SAMPLE.replace("0 30 8 * * *", "not a cron")).unwrap();
        assert!(validate(&cfg).unwrap_err().to_string().contains("cron"));
    }

    #[test]
    fn rejects_no_targets() {
        let t = r#"
[schedule]
cron = "0 30 8 * * *"
[channel]
kind = "wework"
webhook = "https://x"
"#;
        let cfg: PushConfig = toml::from_str(t).unwrap();
        assert!(validate(&cfg).unwrap_err().to_string().contains("至少配置一项"));
    }
}

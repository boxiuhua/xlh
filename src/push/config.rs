//! 推送配置 `push.toml` 解析与校验。
use std::path::{Path, PathBuf};
use std::str::FromStr;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::holdings::Holding;

const CHANNELS: [&str; 4] = ["dingtalk", "feishu", "wework", "serverchan"];
/// 以 webhook 作为完整 URL 请求的渠道（serverchan 的 webhook 是 sendkey，不在此列）。
const URL_CHANNELS: [&str; 3] = ["dingtalk", "feishu", "wework"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushConfig {
    pub schedule: ScheduleCfg,
    pub channel: ChannelCfg,
    #[serde(default)]
    pub portfolio: PortfolioCfg,
    #[serde(default)]
    pub holdings: Vec<Holding>,
    #[serde(default)]
    pub diagnose: Vec<String>,
    /// 股票持仓（同 holdings 结构）。
    #[serde(default)]
    pub stocks: Vec<Holding>,
    /// 额外只诊断、不持有的股票代码。
    #[serde(default)]
    pub diagnose_stocks: Vec<String>,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleCfg {
    pub cron: String,
    /// 仅在有新净值/行情时才推送（默认 true，天然规避周末/节假日空推）。
    #[serde(default = "default_true")]
    pub only_on_new_data: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortfolioCfg {
    #[serde(default)]
    pub total_amount: Option<f64>,
    #[serde(default)]
    pub total_profit: Option<f64>,
    #[serde(default)]
    pub cumulative_profit: Option<f64>,
}

/// 一份空白默认配置（Web 首次打开、push.toml 不存在时用于起表单）。
pub fn default_config() -> PushConfig {
    PushConfig {
        schedule: ScheduleCfg { cron: "0 30 8 * * *".into(), only_on_new_data: true },
        channel: ChannelCfg { kind: "feishu".into(), webhook: String::new(), secret: String::new(), cache_dir: default_cache_dir() },
        portfolio: PortfolioCfg::default(),
        holdings: Vec::new(),
        diagnose: Vec::new(),
        stocks: Vec::new(),
        diagnose_stocks: Vec::new(),
    }
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
    // URL 类渠道的 webhook 必须是完整 http(s) 地址，否则发送时才炸出含糊的 builder error。
    // 飞书裸 hook token(UUID) 会被 canonical_webhook 补全为完整 URL，故此处按补全后的形态校验。
    if URL_CHANNELS.contains(&cfg.channel.kind.as_str()) {
        let w = super::channels::canonical_webhook(&cfg.channel.kind, &cfg.channel.webhook);
        if !(w.starts_with("http://") || w.starts_with("https://")) {
            return Err(anyhow!(
                "{} 的 webhook 必须是完整 URL（http/https 开头）或飞书 hook token，当前为 '{}'。\
                 飞书需填「自定义机器人」的 Webhook 地址（形如 https://open.feishu.cn/open-apis/bot/v2/hook/…）\
                 或其末段 token(UUID)，而非群会话ID(oc_…)",
                cfg.channel.kind, cfg.channel.webhook.trim()));
        }
    }
    cron::Schedule::from_str(&cfg.schedule.cron)
        .map_err(|e| anyhow!("cron 表达式非法 '{}': {e}", cfg.schedule.cron))?;
    if cfg.holdings.is_empty() && cfg.diagnose.is_empty()
        && cfg.stocks.is_empty() && cfg.diagnose_stocks.is_empty()
    {
        return Err(anyhow!("holdings/stocks/diagnose/diagnose_stocks 至少配置一项"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意 TOML：根级键 diagnose/diagnose_stocks 必须在 [[holdings]]/[[stocks]] 之前。
    const SAMPLE: &str = r#"
diagnose = ["110022"]
diagnose_stocks = ["000001"]

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

[[stocks]]
code = "600519"
amount = 20000
profit = 1500
"#;

    #[test]
    fn parses_sample() {
        let cfg: PushConfig = toml::from_str(SAMPLE).unwrap();
        validate(&cfg).unwrap();
        assert_eq!(cfg.channel.kind, "feishu");
        assert_eq!(cfg.schedule.cron, "0 30 8 * * *");
        assert!(cfg.schedule.only_on_new_data, "默认 true");
        assert_eq!(cfg.holdings.len(), 1);
        assert_eq!(cfg.holdings[0].code, "161725");
        assert_eq!(cfg.stocks.len(), 1);
        assert_eq!(cfg.stocks[0].code, "600519");
        assert_eq!(cfg.diagnose, vec!["110022".to_string()]);
        assert_eq!(cfg.diagnose_stocks, vec!["000001".to_string()]);
        assert_eq!(cfg.portfolio.total_amount, Some(30000.0));
        assert_eq!(cfg.channel.cache_dir, PathBuf::from(".cache"), "cache_dir 默认 .cache");
    }

    #[test]
    fn serialize_roundtrip() {
        let cfg: PushConfig = toml::from_str(SAMPLE).unwrap();
        let text = toml::to_string(&cfg).unwrap();
        let back: PushConfig = toml::from_str(&text).unwrap();
        validate(&back).unwrap();
        assert_eq!(back.channel.kind, "feishu");
        assert_eq!(back.holdings.len(), 1);
        assert_eq!(back.stocks[0].code, "600519");
        assert_eq!(back.diagnose_stocks, vec!["000001".to_string()]);
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
    fn rejects_feishu_webhook_that_is_not_url() {
        // 群会话ID(oc_...) 不是自定义机器人 webhook URL → 应在校验期报错，而非发送时炸 builder error
        let t = SAMPLE.replace("https://open.feishu.cn/open-apis/bot/v2/hook/xxx", "oc_f1103754b002dc17b290d470b9b1d05c");
        let cfg: PushConfig = toml::from_str(&t).unwrap();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("webhook") && err.contains("URL"), "应提示 webhook 需为 URL，实际: {err}");
    }

    #[test]
    fn accepts_feishu_webhook_with_https() {
        // SAMPLE 本就是合法 https webhook
        let cfg: PushConfig = toml::from_str(SAMPLE).unwrap();
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn accepts_feishu_bare_hook_token() {
        // 只贴 hook token(UUID) 也应通过（发送时自动补全为完整 URL）
        let t = SAMPLE.replace("https://open.feishu.cn/open-apis/bot/v2/hook/xxx", "097074dc-0f9c-44c0-a7ab-af8942e24143");
        let cfg: PushConfig = toml::from_str(&t).unwrap();
        assert!(validate(&cfg).is_ok(), "飞书裸 token 应通过校验");
    }

    #[test]
    fn serverchan_sendkey_need_not_be_url() {
        let t = r#"
[schedule]
cron = "0 30 8 * * *"
[channel]
kind = "serverchan"
webhook = "SCTKEY123"
[[holdings]]
code = "161725"
amount = 12000
profit = 900
"#;
        let cfg: PushConfig = toml::from_str(t).unwrap();
        assert!(validate(&cfg).is_ok(), "serverchan 的 webhook 是 sendkey，不应要求 URL");
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

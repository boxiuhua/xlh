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
    /// 质量筛选（可选）。`#[serde(default)]` 保证向后兼容 ——
    /// push_configs 存的是 JSON，老配置读出来这里就是 None，无需 DB 迁移。
    #[serde(default)]
    pub screen: Option<ScreenCfg>,
}

/// 质量筛选推送配置。
///
/// 注意这**不是**「选股推荐」：它只做排除（亏损/ST退市/历史过短），
/// 并在推送里附带历史基础发生率。详见 `stock::screen` 的模块文档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenCfg {
    /// 待筛的股票代码。留空则不推送筛选章节。
    #[serde(default)]
    pub codes: Vec<String>,
    /// 输出条数上限
    #[serde(default = "default_screen_top_n")]
    pub top_n: usize,
}

fn default_screen_top_n() -> usize { 10 }

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
        screen: None,
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

/// 覆盖用户不可控字段：缓存目录固定为服务端默认，杜绝路径滥用。
pub fn harden(cfg: &mut PushConfig) {
    cfg.channel.cache_dir = default_cache_dir();
}

/// 要求 cron 秒位为固定数字（拒绝 * / 范围 / 列表 / 步进），避免每秒级狂刷。
pub fn require_fixed_seconds(cron: &str) -> Result<()> {
    let sec = cron.split_whitespace().next().unwrap_or("");
    if sec.is_empty() || !sec.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!(
            "cron 秒位必须为固定值（不支持 * 或范围），以避免过于频繁的推送，当前为 '{sec}'"
        ));
    }
    Ok(())
}

/// URL 类渠道（dingtalk/feishu/wework）的 webhook 主机白名单，防 SSRF：
/// 用户提交的 webhook 只能指向各机器人平台的官方域名，不得指向内网/元数据地址。
pub fn require_allowed_host(cfg: &PushConfig) -> Result<()> {
    if !URL_CHANNELS.contains(&cfg.channel.kind.as_str()) {
        return Ok(()); // serverchan 等非 URL 渠道无需校验
    }
    let url = super::channels::canonical_webhook(&cfg.channel.kind, &cfg.channel.webhook);
    let host = host_of(&url).unwrap_or_default();
    let allowed: &[&str] = match cfg.channel.kind.as_str() {
        "dingtalk" => &["oapi.dingtalk.com"],
        "feishu" => &["open.feishu.cn", "open.larksuite.com"],
        "wework" => &["qyapi.weixin.qq.com"],
        _ => &[],
    };
    if allowed.contains(&host.as_str()) {
        Ok(())
    } else {
        Err(anyhow!(
            "{} 的 webhook 主机 '{}' 不在允许列表 {:?} 内（仅允许对应机器人平台官方域名，防止指向内网地址）",
            cfg.channel.kind, host, allowed
        ))
    }
}

/// 用与实际发送相同的 URL 解析器（reqwest/url crate, WHATWG）提取小写主机名，
/// 避免解析差异导致的白名单绕过（如 `https://evil.com\@allowed.host/`）。解析失败或无主机返回 None。
fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok()?.host_str().map(|h| h.to_ascii_lowercase())
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

    #[test]
    fn harden_forces_cache_dir() {
        let mut c = default_config();
        c.channel.cache_dir = PathBuf::from("/etc/evil");
        harden(&mut c);
        assert_eq!(c.channel.cache_dir, default_cache_dir());
    }

    #[test]
    fn fixed_seconds_accepts_and_rejects() {
        assert!(require_fixed_seconds("0 30 8 * * *").is_ok());
        assert!(require_fixed_seconds("30 0 12 * * *").is_ok());
        for bad in ["* 30 8 * * *", "*/5 30 8 * * *", "0-30 0 8 * * *", "1,2 0 8 * * *", ""] {
            assert!(require_fixed_seconds(bad).is_err(), "应拒绝 {bad}");
        }
    }

    fn with_channel(kind: &str, webhook: &str) -> PushConfig {
        let mut cfg: PushConfig = toml::from_str(SAMPLE).unwrap();
        cfg.channel.kind = kind.to_string();
        cfg.channel.webhook = webhook.to_string();
        cfg
    }

    #[test]
    fn allowed_host_accepts_official_feishu_domains() {
        assert!(require_allowed_host(&with_channel("feishu", "https://open.feishu.cn/open-apis/bot/v2/hook/xxx")).is_ok());
        assert!(require_allowed_host(&with_channel("feishu", "097074dc-0f9c-44c0-a7ab-af8942e24143")).is_ok(),
            "飞书裸 token 经 canonical_webhook 补全为 open.feishu.cn 后应通过");
        assert!(require_allowed_host(&with_channel("feishu", "https://open.larksuite.com/open-apis/bot/v2/hook/xxx")).is_ok());
    }

    #[test]
    fn allowed_host_accepts_official_dingtalk_and_rejects_other_hosts() {
        assert!(require_allowed_host(&with_channel("dingtalk", "https://oapi.dingtalk.com/robot/send?access_token=x")).is_ok());
        assert!(require_allowed_host(&with_channel("dingtalk", "https://evil.com/x")).is_err());
    }

    #[test]
    fn allowed_host_rejects_ssrf_targets_for_wework() {
        let err = require_allowed_host(&with_channel("wework", "http://169.254.169.254/latest/meta-data")).unwrap_err().to_string();
        assert!(err.contains("不在允许列表"), "应拒绝云元数据地址: {err}");
        let err2 = require_allowed_host(&with_channel("wework", "http://127.0.0.1/x")).unwrap_err().to_string();
        assert!(err2.contains("不在允许列表"), "应拒绝回环地址: {err2}");
    }

    #[test]
    fn allowed_host_exempts_serverchan() {
        let cfg = with_channel("serverchan", "SCTKEY123");
        assert!(require_allowed_host(&cfg).is_ok(), "serverchan 非 URL 渠道应豁免主机校验");
    }

    // 以下三个用例验证 host_of 与 reqwest 实际发送时的解析器一致，
    // 杜绝手写字符串解析与 reqwest(url crate, WHATWG) 解析结果不一致导致的白名单绕过。

    #[test]
    fn allowed_host_rejects_backslash_authority_terminator_bypass() {
        // reqwest/url crate 将反斜杠视为权威部分终止符，真实连接目标是 evil.com，而非 open.feishu.cn
        let err = require_allowed_host(&with_channel("feishu", "https://evil.com\\@open.feishu.cn/"))
            .unwrap_err().to_string();
        assert!(err.contains("不在允许列表"), "应拒绝反斜杠权威终止符绕过: {err}");
    }

    #[test]
    fn allowed_host_rejects_userinfo_bypass() {
        // open.feishu.cn@evil.com 中 @ 之前是 userinfo，真实主机是 evil.com
        let err = require_allowed_host(&with_channel("feishu", "https://open.feishu.cn@evil.com/"))
            .unwrap_err().to_string();
        assert!(err.contains("不在允许列表"), "应拒绝 userinfo 绕过: {err}");
    }

    #[test]
    fn allowed_host_rejects_suffix_bypass() {
        // open.feishu.cn.evil.com 的真实主机是整个 open.feishu.cn.evil.com，而非 open.feishu.cn
        let err = require_allowed_host(&with_channel("feishu", "https://open.feishu.cn.evil.com/"))
            .unwrap_err().to_string();
        assert!(err.contains("不在允许列表"), "应拒绝域名后缀绕过: {err}");
    }

    #[test]
    fn allowed_host_accepts_plain_valid_feishu_webhook() {
        assert!(require_allowed_host(&with_channel("feishu", "https://open.feishu.cn/open-apis/bot/v2/hook/x")).is_ok());
    }
}

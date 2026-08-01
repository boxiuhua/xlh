//! OpenAI-compatible AI analysis client.  It only receives the locally computed
//! indicators supplied by the caller; it never invents a trading signal itself.
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}
fn default_model() -> String {
    "deepseek-v4-flash".into()
}

/// DeepSeek V4 enables thinking by default. For these short, structured reports
/// disable it so the output budget is reserved for the visible final answer.
fn apply_provider_options(body: &mut Value, base_url: &str) {
    if reqwest::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|host| host.eq_ignore_ascii_case("api.deepseek.com"))
    {
        body["thinking"] = serde_json::json!({"type":"disabled"});
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub system_prompt: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_base_url(),
            model: default_model(),
            api_key: String::new(),
            system_prompt: String::new(),
        }
    }
}

pub fn validate_config(cfg: &AiConfig) -> Result<()> {
    if cfg.base_url.trim().is_empty() || cfg.base_url.len() > 512 {
        return Err(anyhow!("AI 接口地址不能为空且不能超过 512 字符"));
    }
    let url = reqwest::Url::parse(cfg.base_url.trim()).context("AI 接口地址不是合法 URL")?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(anyhow!("AI 接口地址必须是 HTTPS 完整地址"));
    }
    if cfg.model.trim().is_empty() || cfg.model.len() > 128 {
        return Err(anyhow!("模型名称不能为空且不能超过 128 字符"));
    }
    if cfg.enabled && cfg.api_key.trim().is_empty() {
        return Err(anyhow!("启用 AI 分析前请填写 API Key"));
    }
    Ok(())
}

pub fn analyze(cfg: &AiConfig, asset_type: &str, code: &str, context: &Value) -> Result<String> {
    validate_config(cfg)?;
    if !cfg.enabled {
        return Err(anyhow!("AI 分析尚未启用，请先在模型配置中启用并保存"));
    }
    let endpoint = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let system = if cfg.system_prompt.trim().is_empty() {
        "你是谨慎的中文证券研究助手。只能依据提供的数据分析，明确区分事实、推断和未知；不得承诺收益或给出确定性交易指令。用简洁中文输出：结论、支撑证据、主要风险、需要继续验证的点。".to_string()
    } else {
        cfg.system_prompt.trim().to_string()
    };
    let user = format!(
        "请分析{asset_type} {code}。以下是系统本地计算的指标与回测证据（可能不完整）：\n{}",
        serde_json::to_string_pretty(context)?
    );
    let mut body = serde_json::json!({"model": cfg.model, "temperature": 0.2, "max_tokens": 900,
        "messages": [{"role":"system","content":system},{"role":"user","content":user}]});
    apply_provider_options(&mut body, &cfg.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()?;
    let body = serde_json::to_string(&body)?;
    let reply_text = client
        .post(endpoint)
        .bearer_auth(cfg.api_key.trim())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .context("调用 AI 接口失败")?
        .error_for_status()
        .context("AI 接口返回错误")?
        .text()
        .context("读取 AI 接口响应失败")?;
    let reply: Value = serde_json::from_str(&reply_text).context("AI 接口返回不是 JSON")?;
    reply
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("AI 接口未返回分析正文"))
}

/// Turn a short user requirement into a reusable system prompt.  Keeping this
/// server-side means the API key is never exposed in browser JavaScript.
pub fn optimize_prompt(cfg: &AiConfig, draft: &str) -> Result<String> {
    if draft.trim().is_empty() || draft.len() > 2_000 {
        return Err(anyhow!("请填写 1-2000 字的提示词需求"));
    }
    validate_config(cfg)?;
    if !cfg.enabled {
        return Err(anyhow!("请先启用 AI 分析并保存 API Key"));
    }
    let endpoint = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut body = serde_json::json!({"model": cfg.model, "temperature": 0.25, "max_tokens": 700,
    "messages": [
      {"role":"system","content":"你是提示词工程助手。将用户需求改写为可直接作为‘股票/基金分析助手’系统提示词的中文版本。保留用户意图，要求只依据输入数据、区分事实和推断、列示风险和待验证项、不承诺收益、不输出确定性买卖指令。只输出优化后的提示词，不要解释。"},
      {"role":"user","content": draft.trim()}
        ]});
    apply_provider_options(&mut body, &cfg.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .build()?;
    let reply_text = client
        .post(endpoint)
        .bearer_auth(cfg.api_key.trim())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(&body)?)
        .send()
        .context("调用 AI 接口失败")?
        .error_for_status()
        .context("AI 接口返回错误")?
        .text()
        .context("读取 AI 接口响应失败")?;
    let reply: Value = serde_json::from_str(&reply_text).context("AI 接口返回不是 JSON")?;
    reply
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("AI 接口未返回优化后的提示词"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_config_can_be_saved_without_key() {
        assert!(validate_config(&AiConfig::default()).is_ok());
    }
    #[test]
    fn enabled_config_requires_key() {
        let mut c = AiConfig::default();
        c.enabled = true;
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn rejects_non_https_endpoint() {
        let mut c = AiConfig::default();
        c.base_url = "http://127.0.0.1:11434/v1".into();
        assert!(validate_config(&c).is_err());
    }
}

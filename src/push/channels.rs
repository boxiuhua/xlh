//! 四渠道请求构造（钉钉/飞书/企业微信/Server酱）+ 加签 + 发送。
//! 构造(build_request)与发送(send)分离，前者纯函数便于测试。
use anyhow::{anyhow, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use super::config::ChannelCfg;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpReq {
    pub url: String,
    pub body: String,
    /// true 走 application/x-www-form-urlencoded，false 走 application/json。
    pub form: bool,
}

fn hmac_b64(key: &[u8], data: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(data);
    B64.encode(mac.finalize().into_bytes())
}

/// 百分号编码（未保留字符外一律编码）。用于钉钉 sign 与 serverchan 表单值。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 构造 HTTP 请求（不发送）。`ts_ms` 为毫秒时间戳，用于加签，测试可固定。
pub fn build_request(cfg: &ChannelCfg, title: &str, md: &str, ts_ms: i64) -> HttpReq {
    match cfg.kind.as_str() {
        // 钉钉：markdown 消息；加签走 URL 参数，timestamp 用毫秒。
        "dingtalk" => {
            let mut url = cfg.webhook.clone();
            if !cfg.secret.is_empty() {
                let sign = hmac_b64(cfg.secret.as_bytes(), format!("{ts_ms}\n{}", cfg.secret).as_bytes());
                url.push_str(&format!("&timestamp={ts_ms}&sign={}", urlencode(&sign)));
            }
            let body = serde_json::json!({"msgtype":"markdown","markdown":{"title":title,"text":md}}).to_string();
            HttpReq { url, body, form: false }
        }
        // 飞书：文本消息；加签走 body，timestamp 用秒，sign=HMAC(key="{ts}\n{secret}", data="")。
        "feishu" => {
            let mut obj = serde_json::json!({"msg_type":"text","content":{"text": md}});
            if !cfg.secret.is_empty() {
                let ts_s = ts_ms / 1000;
                let sign = hmac_b64(format!("{ts_s}\n{}", cfg.secret).as_bytes(), b"");
                obj["timestamp"] = serde_json::Value::String(ts_s.to_string());
                obj["sign"] = serde_json::Value::String(sign);
            }
            HttpReq { url: cfg.webhook.clone(), body: obj.to_string(), form: false }
        }
        // 企业微信：markdown 消息，无加签（密钥在 URL 的 key 参数里）。
        "wework" => {
            let body = serde_json::json!({"msgtype":"markdown","markdown":{"content": md}}).to_string();
            HttpReq { url: cfg.webhook.clone(), body, form: false }
        }
        // Server酱：webhook 字段即 sendkey。
        "serverchan" => {
            let url = format!("https://sctapi.ftqq.com/{}.send", cfg.webhook);
            let body = format!("title={}&desp={}", urlencode(title), urlencode(md));
            HttpReq { url, body, form: true }
        }
        _ => HttpReq { url: cfg.webhook.clone(), body: md.to_string(), form: false },
    }
}

/// 发送失败重试次数与间隔。
const SEND_RETRIES: usize = 3;
const RETRY_GAP_SECS: u64 = 5;

/// 单次发送尝试：校验 HTTP 2xx，且（能解析 JSON 时）业务码为 0。
fn send_once(req: &HttpReq) -> Result<()> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))?;
    let ct = if req.form { "application/x-www-form-urlencoded" } else { "application/json" };
    let resp = client.post(&req.url).header("Content-Type", ct).body(req.body.clone())
        .send().map_err(|e| anyhow!("推送请求失败: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    let snippet: String = text.chars().take(200).collect();
    if !status.is_success() {
        return Err(anyhow!("推送 HTTP {status}：{snippet}"));
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        let code = v.get("errcode").or_else(|| v.get("code")).and_then(|c| c.as_i64());
        if let Some(c) = code {
            if c != 0 { return Err(anyhow!("推送返回错误码 {c}：{snippet}")); }
        }
    }
    Ok(())
}

/// 发送，失败重试至多 SEND_RETRIES 次、间隔 RETRY_GAP_SECS 秒。
pub fn send(cfg: &ChannelCfg, title: &str, md: &str) -> Result<()> {
    let ts_ms = chrono::Utc::now().timestamp_millis();
    let req = build_request(cfg, title, md, ts_ms);
    let mut last = None;
    for attempt in 1..=SEND_RETRIES {
        match send_once(&req) {
            Ok(()) => return Ok(()),
            Err(e) => {
                eprintln!("推送第 {attempt}/{SEND_RETRIES} 次失败：{e}");
                last = Some(e);
                if attempt < SEND_RETRIES {
                    std::thread::sleep(std::time::Duration::from_secs(RETRY_GAP_SECS));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("推送失败")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg(kind: &str, secret: &str) -> ChannelCfg {
        ChannelCfg { kind: kind.into(), webhook: "https://hook/xyz".into(), secret: secret.into(), cache_dir: PathBuf::from(".cache") }
    }

    #[test]
    fn dingtalk_markdown_and_sign() {
        let r = build_request(&cfg("dingtalk", "sec"), "标题", "正文", 1_700_000_000_000);
        assert!(!r.form);
        assert!(r.body.contains("\"msgtype\":\"markdown\""));
        assert!(r.url.contains("&timestamp=1700000000000"));
        assert!(r.url.contains("&sign="), "应带加签");
    }

    #[test]
    fn dingtalk_no_sign_without_secret() {
        let r = build_request(&cfg("dingtalk", ""), "t", "m", 1_700_000_000_000);
        assert!(!r.url.contains("sign="));
        assert_eq!(r.url, "https://hook/xyz");
    }

    #[test]
    fn feishu_text_and_sign_in_body() {
        let r = build_request(&cfg("feishu", "sec"), "t", "正文", 1_700_000_000_000);
        assert!(r.body.contains("\"msg_type\":\"text\""));
        assert!(r.body.contains("\"sign\":"));
        assert!(r.body.contains("\"timestamp\":\"1700000000\""), "飞书用秒");
    }

    #[test]
    fn wework_markdown_content() {
        let r = build_request(&cfg("wework", ""), "t", "正文", 0);
        assert!(r.body.contains("\"msgtype\":\"markdown\""));
        assert!(r.body.contains("\"content\":\"正文\""));
    }

    #[test]
    fn serverchan_form_url() {
        let mut c = cfg("serverchan", "");
        c.webhook = "SCTKEY123".into();
        let r = build_request(&c, "标题", "正文", 0);
        assert!(r.form);
        assert_eq!(r.url, "https://sctapi.ftqq.com/SCTKEY123.send");
        assert!(r.body.starts_with("title="));
        assert!(r.body.contains("&desp="));
    }

    #[test]
    fn sign_is_deterministic_and_ts_sensitive() {
        let a = build_request(&cfg("dingtalk", "sec"), "t", "m", 1000);
        let b = build_request(&cfg("dingtalk", "sec"), "t", "m", 1000);
        let c = build_request(&cfg("dingtalk", "sec"), "t", "m", 2000);
        assert_eq!(a.url, b.url, "同输入同 sign");
        assert_ne!(a.url, c.url, "换 ts 换 sign");
    }

    #[test]
    fn hmac_known_vector() {
        // 固定 key/data 的 HMAC-SHA256 base64，防止实现回归
        assert_eq!(hmac_b64(b"key", b"The quick brown fox jumps over the lazy dog"),
            "97yD9DBThCSxMpjmqm+xQ+9NWaFJRhdZl0edvC0aPNg=");
    }
}

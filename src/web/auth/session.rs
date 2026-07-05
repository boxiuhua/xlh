use axum::http::{header::COOKIE, HeaderMap};
use base64::Engine;
use rand::RngCore;

pub const COOKIE_NAME: &str = "xlh_session";

/// 32 字节随机，base64url 无填充。
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 从请求头解析 xlh_session 的值。
pub fn read_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(v.to_string());
        }
    }
    None
}

pub fn set_cookie_header(token: &str, ttl_days: i64) -> String {
    let max_age = ttl_days * 24 * 3600;
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

pub fn clear_cookie_header() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_nonempty() {
        let a = new_token();
        let b = new_token();
        assert!(!a.is_empty());
        assert_ne!(a, b);
    }

    #[test]
    fn read_cookie_picks_our_key() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, "foo=1; xlh_session=abc123; bar=2".parse().unwrap());
        assert_eq!(read_cookie(&h).as_deref(), Some("abc123"));
    }

    #[test]
    fn read_cookie_absent() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, "foo=1".parse().unwrap());
        assert_eq!(read_cookie(&h), None);
    }

    #[test]
    fn set_and_clear_headers() {
        assert!(set_cookie_header("t", 30).contains("xlh_session=t"));
        assert!(set_cookie_header("t", 30).contains("Max-Age=2592000"));
        assert!(clear_cookie_header().contains("Max-Age=0"));
    }
}

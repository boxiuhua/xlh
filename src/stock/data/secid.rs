use anyhow::{anyhow, Result};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Secid { pub market: u16, pub code: String }

impl Secid {
    pub fn param(&self) -> String { format!("{}.{}", self.market, self.code) }
    pub fn cache_key(&self) -> String { format!("{}_{}", self.market, self.code) }
    pub fn from_cache_key(s: &str) -> Option<Secid> {
        let (m, c) = s.split_once('_')?;
        let market = m.parse().ok()?;
        if c.is_empty() { return None; }
        Some(Secid { market, code: c.to_string() })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Resolved { Ready(Secid), NeedSearch(String) }

fn pad5(digits: &str) -> String { format!("{:0>5}", digits) }

/// 离线解析代码 → secid；美股字母代码返回 NeedSearch(大写 ticker)。
pub fn resolve_offline(input: &str) -> Result<Resolved> {
    let s = input.trim();
    if s.is_empty() { return Err(anyhow!("代码为空")); }
    let lower = s.to_ascii_lowercase();

    // 显式市场前缀：仅当其后为纯数字时才生效（避免 SHOP/USB 等美股代码被误判为 sh/us 前缀）
    for (pfx, market, pad) in [("sh", 1u16, false), ("sz", 0u16, false), ("hk", 116u16, true)] {
        if let Some(rest) = lower.strip_prefix(pfx) {
            if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                let code = if pad { pad5(rest) } else { rest.to_string() };
                return Ok(Resolved::Ready(Secid { market, code }));
            }
        }
    }
    // 美股显式前缀 "us."（带点，避免与 USB 等 ticker 冲突）
    if let Some(rest) = lower.strip_prefix("us.") {
        if !rest.is_empty() { return Ok(Resolved::NeedSearch(rest.to_ascii_uppercase())); }
    }

    // 纯数字
    if s.chars().all(|c| c.is_ascii_digit()) {
        return match s.len() {
            6 => {
                let market = match s.chars().next().unwrap() { '6' | '5' | '9' => 1, _ => 0 };
                Ok(Resolved::Ready(Secid { market, code: s.to_string() }))
            }
            n if n < 6 => Ok(Resolved::Ready(Secid { market: 116, code: pad5(s) })),
            _ => Err(anyhow!("非法数字代码长度: {s}")),
        };
    }

    // 其余含字母者视为美股 ticker
    Ok(Resolved::NeedSearch(s.to_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(input: &str) -> Secid {
        match resolve_offline(input).unwrap() { Resolved::Ready(s) => s, _ => panic!("应离线可解析: {input}") }
    }

    #[test]
    fn a_share_shanghai_and_shenzhen() {
        assert_eq!(ready("600519"), Secid { market: 1, code: "600519".into() }); // 沪主板
        assert_eq!(ready("688981"), Secid { market: 1, code: "688981".into() }); // 科创
        assert_eq!(ready("000001"), Secid { market: 0, code: "000001".into() }); // 深主板
        assert_eq!(ready("300750"), Secid { market: 0, code: "300750".into() }); // 创业板
    }

    #[test]
    fn hk_zero_pads_to_five() {
        assert_eq!(ready("700"), Secid { market: 116, code: "00700".into() });
        assert_eq!(ready("09988"), Secid { market: 116, code: "09988".into() });
    }

    #[test]
    fn explicit_prefixes() {
        assert_eq!(ready("sh600519"), Secid { market: 1, code: "600519".into() });
        assert_eq!(ready("sz000001"), Secid { market: 0, code: "000001".into() });
        assert_eq!(ready("hk700"), Secid { market: 116, code: "00700".into() });
    }

    #[test]
    fn us_ticker_needs_search() {
        assert_eq!(resolve_offline("AAPL").unwrap(), Resolved::NeedSearch("AAPL".into()));
        assert_eq!(resolve_offline("us.tsla").unwrap(), Resolved::NeedSearch("TSLA".into()));
    }

    #[test]
    fn us_tickers_colliding_with_prefixes_still_search() {
        // SHOP 以 "sh" 开头、USB 以 "us" 开头，但其后非纯数字 → 应视为美股 ticker
        assert_eq!(resolve_offline("SHOP").unwrap(), Resolved::NeedSearch("SHOP".into()));
        assert_eq!(resolve_offline("USB").unwrap(), Resolved::NeedSearch("USB".into()));
    }

    #[test]
    fn rejects_overlong_or_empty() {
        assert!(resolve_offline("").is_err());
        assert!(resolve_offline("1234567").is_err());
    }

    #[test]
    fn cache_key_roundtrip() {
        let s = Secid { market: 116, code: "00700".into() };
        assert_eq!(Secid::from_cache_key(&s.cache_key()), Some(s));
    }
}

//! 报价源。监听线程经 trait 注入,测试用桩,生产用腾讯快照。

use crate::stock::realtime::snapshot::{self, Tick};
use crate::trade::model::Quote;
use anyhow::Result;

pub trait QuoteSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<Quote>>;
}

/// 6 位沪深代码 → 腾讯符号;北交所及非法代码返回 None。
pub fn tencent_symbol(code: &str) -> Option<String> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match code.as_bytes()[0] {
        b'6' | b'5' | b'9' => snapshot::symbol(1, code),
        b'0' | b'1' | b'2' | b'3' => snapshot::symbol(0, code),
        _ => None,
    }
}

pub fn tick_to_quote(t: &Tick) -> Quote {
    Quote {
        code: t.code.clone(),
        price: t.price,
        limit_up: t.limit_up,
        limit_down: t.limit_down,
        ts: t.ts,
    }
}

/// 腾讯实时快照。停牌股(成交量 0)被快照解析跳过,因而没有报价。
pub struct TencentQuotes;

impl QuoteSource for TencentQuotes {
    fn fetch(&self, codes: &[String]) -> Result<Vec<Quote>> {
        let symbols: Vec<String> = codes.iter().filter_map(|c| tencent_symbol(c)).collect();
        Ok(snapshot::fetch(&symbols)?
            .iter()
            .map(tick_to_quote)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn symbols_for_sh_sz_only() {
        assert_eq!(tencent_symbol("600519").as_deref(), Some("sh600519"));
        assert_eq!(tencent_symbol("510300").as_deref(), Some("sh510300"));
        assert_eq!(tencent_symbol("000001").as_deref(), Some("sz000001"));
        assert_eq!(tencent_symbol("300750").as_deref(), Some("sz300750"));
        assert_eq!(tencent_symbol("159915").as_deref(), Some("sz159915"));
        assert_eq!(tencent_symbol("830799"), None, "北交所暂不支持");
        assert_eq!(tencent_symbol("60051"), None);
        assert_eq!(tencent_symbol("AAPL00"), None);
    }

    #[test]
    fn tick_maps_to_quote() {
        let ts = NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(10, 0, 3)
            .unwrap();
        let t = Tick {
            code: "600519".into(),
            ts,
            price: 1500.0,
            change_pct: 1.0,
            volume: 1.0,
            amount: 1.0,
            turnover: 0.1,
            vol_ratio: 1.0,
            limit_up: Some(1650.0),
            limit_down: Some(1350.0),
        };
        assert_eq!(
            tick_to_quote(&t),
            Quote {
                code: "600519".into(),
                price: 1500.0,
                limit_up: Some(1650.0),
                limit_down: Some(1350.0),
                ts
            }
        );
    }
}

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use super::secid::Secid;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StockInfo {
    pub code: String,
    pub name: String,
    pub secid: Secid,
    pub market_name: String,
}

#[derive(Deserialize)]
struct Suggest { #[serde(rename = "QuotationCodeTable")] table: Table }
#[derive(Deserialize)]
struct Table { #[serde(rename = "Data")] data: Option<Vec<Item>> }
#[derive(Deserialize)]
struct Item {
    #[serde(rename = "Code")] code: String,
    #[serde(rename = "Name")] name: String,
    #[serde(rename = "MktNum")] mkt_num: String,
    #[serde(rename = "SecurityTypeName", default)] type_name: String,
}

const US_MARKETS: [u16; 3] = [105, 106, 107];

pub fn parse_suggest(body: &str) -> Result<Vec<StockInfo>> {
    let s: Suggest = serde_json::from_str(body).map_err(|e| anyhow!("解析suggest失败: {e}"))?;
    let Some(items) = s.table.data else { return Ok(Vec::new()); };
    let out = items.into_iter().filter_map(|it| {
        let market: u16 = it.mkt_num.parse().ok()?;
        Some(StockInfo {
            code: it.code.clone(),
            name: it.name,
            secid: Secid { market, code: it.code },
            market_name: it.type_name,
        })
    }).collect();
    Ok(out)
}

/// 从搜索结果里挑出美股 secid：优先 code 精确匹配(忽略大小写)且在美股市场，退而取首个美股结果。
pub fn pick_us(items: &[StockInfo], ticker: &str) -> Option<Secid> {
    items.iter()
        .find(|i| i.code.eq_ignore_ascii_case(ticker) && US_MARKETS.contains(&i.secid.market))
        .or_else(|| items.iter().find(|i| US_MARKETS.contains(&i.secid.market)))
        .map(|i| i.secid.clone())
}

pub fn search(query: &str) -> Result<Vec<StockInfo>> {
    let url = format!(
        "https://searchapi.eastmoney.com/api/suggest/get?input={}&type=14&count=20",
        urlencoding_min(query));
    let body = reqwest::blocking::Client::new()
        .get(&url)
        .header("Referer", "https://www.eastmoney.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send().map_err(|e| anyhow!("请求suggest失败: {e}"))?
        .text().map_err(|e| anyhow!("读取suggest响应失败: {e}"))?;
    parse_suggest(&body)
}

pub fn resolve_us(ticker: &str) -> Result<Secid> {
    let items = search(ticker)?;
    pick_us(&items, ticker).ok_or_else(|| anyhow!("未找到美股 {ticker}"))
}

/// 极简 URL 编码：仅保留非保留字符，其余按 UTF-8 百分号编码。
fn urlencoding_min(s: &str) -> String {
    let mut out = String::new();
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"QuotationCodeTable":{"Data":[
        {"Code":"600519","Name":"贵州茅台","MktNum":"1","SecurityTypeName":"沪A"},
        {"Code":"AAPL","Name":"苹果","MktNum":"105","SecurityTypeName":"美股"}],"Status":0}}"#;

    #[test]
    fn parse_suggest_builds_secid() {
        let items = parse_suggest(SAMPLE).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].secid, Secid { market: 1, code: "600519".into() });
        assert_eq!(items[0].name, "贵州茅台");
        assert_eq!(items[1].secid, Secid { market: 105, code: "AAPL".into() });
    }

    #[test]
    fn parse_suggest_empty_data() {
        assert!(parse_suggest(r#"{"QuotationCodeTable":{"Data":null}}"#).unwrap().is_empty());
    }

    #[test]
    fn pick_us_matches_ticker_case_insensitive() {
        let items = parse_suggest(SAMPLE).unwrap();
        assert_eq!(pick_us(&items, "aapl"), Some(Secid { market: 105, code: "AAPL".into() }));
    }

    #[test]
    fn pick_us_none_when_no_us_market() {
        let items = vec![StockInfo {
            code: "600519".into(), name: "贵州茅台".into(),
            secid: Secid { market: 1, code: "600519".into() }, market_name: "沪A".into(),
        }];
        assert_eq!(pick_us(&items, "600519"), None);
    }
}

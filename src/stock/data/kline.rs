use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use super::StockBar;
use super::secid::Secid;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub date: NaiveDate,
    pub open: f64, pub high: f64, pub low: f64, pub close: f64, pub volume: f64,
}

#[derive(Deserialize)]
struct KlineResp { data: Option<KlineData> }
#[derive(Deserialize)]
struct KlineData { #[serde(default)] klines: Vec<String> }

pub fn parse_one(body: &str) -> Result<Vec<Row>> {
    let resp: KlineResp = serde_json::from_str(body).map_err(|e| anyhow!("解析K线JSON失败: {e}"))?;
    let Some(data) = resp.data else { return Ok(Vec::new()); };
    let mut rows = Vec::with_capacity(data.klines.len());
    for line in &data.klines {
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 6 { continue; }
        let date = NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?;
        rows.push(Row {
            date,
            open: c[1].parse()?,
            close: c[2].parse()?,
            high: c[3].parse()?,
            low: c[4].parse()?,
            volume: c[5].parse()?,
        });
    }
    rows.sort_by_key(|r| r.date);
    Ok(rows)
}

pub fn merge(raw: Vec<Row>, adj: Vec<Row>) -> Vec<StockBar> {
    let mut adj_map = std::collections::HashMap::new();
    for r in &adj { adj_map.insert(r.date, r.close); }
    let mut bars: Vec<StockBar> = raw.into_iter().map(|r| StockBar {
        date: r.date,
        open: r.open, high: r.high, low: r.low, close: r.close, volume: r.volume,
        adj_close: adj_map.get(&r.date).copied().unwrap_or(r.close),
    }).collect();
    bars.sort_by_key(|b| b.date);
    bars
}

fn fetch_body(secid: &Secid, fqt: u8) -> Result<String> {
    let url = format!(
        "https://push2his.eastmoney.com/api/qt/stock/kline/get?secid={}&fields1=f1,f2,f3&fields2=f51,f52,f53,f54,f55,f56,f57&klt=101&fqt={}&beg=0&end=20500101",
        secid.param(), fqt);
    reqwest::blocking::Client::new()
        .get(&url)
        .header("Referer", "https://www.eastmoney.com/")
        .header("User-Agent", "Mozilla/5.0")
        .send().map_err(|e| anyhow!("请求K线失败: {e}"))?
        .text().map_err(|e| anyhow!("读取K线响应失败: {e}"))
}

/// 抓不复权 OHLCV + 后复权收盘，merge 成 StockBar。
pub fn fetch(secid: &Secid) -> Result<Vec<StockBar>> {
    let raw = parse_one(&fetch_body(secid, 0)?)?;
    if raw.is_empty() { return Err(anyhow!("{} 无K线数据", secid.param())); }
    let adj = parse_one(&fetch_body(secid, 2)?)?;
    Ok(merge(raw, adj))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    // 东财 fields2=f51..f57 顺序：date,open,close,high,low,volume,amount
    const RAW: &str = r#"{"rc":0,"data":{"code":"600519","name":"贵州茅台","klines":[
        "2024-01-02,100.0,110.0,112.0,99.0,10000,1234",
        "2024-01-03,110.0,121.0,122.0,109.0,12000,1500"]}}"#;
    const ADJ: &str = r#"{"rc":0,"data":{"code":"600519","name":"贵州茅台","klines":[
        "2024-01-02,200.0,220.0,224.0,198.0,10000,1234",
        "2024-01-03,220.0,242.0,244.0,218.0,12000,1500"]}}"#;

    #[test]
    fn parse_one_reads_ohlcv_in_eastmoney_order() {
        let rows = parse_one(RAW).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, d(2024,1,2));
        assert!((rows[0].open - 100.0).abs() < 1e-9);
        assert!((rows[0].close - 110.0).abs() < 1e-9, "close 是第2个字段");
        assert!((rows[0].high - 112.0).abs() < 1e-9);
        assert!((rows[0].low - 99.0).abs() < 1e-9);
        assert!((rows[0].volume - 10000.0).abs() < 1e-9);
    }

    #[test]
    fn parse_one_empty_when_no_data() {
        assert!(parse_one(r#"{"rc":0,"data":null}"#).unwrap().is_empty());
    }

    #[test]
    fn merge_takes_close_from_raw_and_adj_close_from_adj() {
        let bars = merge(parse_one(RAW).unwrap(), parse_one(ADJ).unwrap());
        assert_eq!(bars.len(), 2);
        assert!((bars[0].close - 110.0).abs() < 1e-9);
        assert!((bars[0].adj_close - 220.0).abs() < 1e-9);
        assert!((bars[1].close - 121.0).abs() < 1e-9);
        assert!((bars[1].adj_close - 242.0).abs() < 1e-9);
    }

    #[test]
    fn merge_falls_back_to_raw_close_when_adj_missing() {
        let raw = parse_one(RAW).unwrap();
        let bars = merge(raw, Vec::new()); // 后复权缺失
        assert!((bars[0].adj_close - bars[0].close).abs() < 1e-9);
    }
}

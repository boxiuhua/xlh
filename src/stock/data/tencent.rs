//! 腾讯行情 K线源（web.ifzq.gtimg.cn）。作为股票 K线主源：
//! push2his.eastmoney.com 的 CDN 在部分网络 TLS 握手后即被断开（IPv4/IPv6 皆然），
//! 腾讯接口在这些网络可用且覆盖 A股/港股/美股。东财降为兜底（见 kline::fetch）。
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde_json::Value;
use std::collections::HashMap;
use super::StockBar;
use super::secid::Secid;
use super::kline::Row;

/// 腾讯单次返回上限：count≥2400 会整体返回异常，2000 实测稳定。
const KLINE_COUNT: u32 = 2000;

/// Secid → 腾讯符号。US 需交易所后缀：105 NASDAQ→.OQ / 106 NYSE→.N / 107 AMEX→.A。
pub fn symbol(secid: &Secid) -> Option<String> {
    match secid.market {
        1 => Some(format!("sh{}", secid.code)),
        0 => Some(format!("sz{}", secid.code)),
        116 => Some(format!("hk{}", secid.code)),
        105 => Some(format!("us{}.OQ", secid.code)),
        106 => Some(format!("us{}.N", secid.code)),
        107 => Some(format!("us{}.A", secid.code)),
        _ => None,
    }
}

/// 从腾讯响应里取 data.<符号>.<key> 数组并解析为 Row。key: "day"(不复权) / "hfqday"(后复权)。
pub fn parse_array(body: &str, key: &str) -> Result<Vec<Row>> {
    let v: Value = serde_json::from_str(body).map_err(|e| anyhow!("腾讯JSON解析失败: {e}"))?;
    if v.get("code").and_then(|c| c.as_i64()) != Some(0) {
        let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("未知");
        return Err(anyhow!("腾讯返回异常: {msg}"));
    }
    let data = v.get("data").and_then(|d| d.as_object()).ok_or_else(|| anyhow!("腾讯响应缺 data"))?;
    // data 下唯一键即符号（如 "sh600519"）
    let node = data.values().next().and_then(|n| n.as_object()).ok_or_else(|| anyhow!("腾讯响应空"))?;
    let Some(arr) = node.get(key).and_then(|a| a.as_array()) else { return Ok(Vec::new()); };
    let mut rows = Vec::with_capacity(arr.len());
    for item in arr {
        // 每行：[date, open, close, high, low, volume, (可选分红对象)]
        let Some(cols) = item.as_array() else { continue; };
        if cols.len() < 6 { continue; }
        let num = |i: usize| cols[i].as_str().and_then(|s| s.parse::<f64>().ok());
        let date = cols[0].as_str().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
        let (Some(date), Some(open), Some(close), Some(high), Some(low), Some(volume)) =
            (date, num(1), num(2), num(3), num(4), num(5)) else { continue; };
        rows.push(Row { date, open, high, low, close, volume });
    }
    rows.sort_by_key(|r| r.date);
    Ok(rows)
}

/// 合并腾讯 raw + hfq：hfq 尺度（后复权）与 raw 不同且仅约 2.5 年，
/// 故仅保留 hfq 覆盖区间（raw OHLCV + hfq 收盘作 adj_close），避免 adj_close 尺度断层。
/// hfq 缺失时（美股/新股/抓取失败）退化为全量 raw，adj_close 回退 raw close。
pub fn merge_clip(raw: Vec<Row>, adj: Vec<Row>) -> Vec<StockBar> {
    if adj.is_empty() {
        let mut bars: Vec<StockBar> = raw.into_iter().map(|r| StockBar {
            date: r.date, open: r.open, high: r.high, low: r.low, close: r.close,
            volume: r.volume, adj_close: r.close,
        }).collect();
        bars.sort_by_key(|b| b.date);
        return bars;
    }
    let raw_map: HashMap<NaiveDate, &Row> = raw.iter().map(|r| (r.date, r)).collect();
    let mut bars: Vec<StockBar> = adj.iter().filter_map(|a| {
        raw_map.get(&a.date).map(|r| StockBar {
            date: a.date, open: r.open, high: r.high, low: r.low, close: r.close,
            volume: r.volume, adj_close: a.close,
        })
    }).collect();
    bars.sort_by_key(|b| b.date);
    bars
}

fn fetch_array(symbol: &str, fq: &str, key: &str) -> Result<Vec<Row>> {
    let url = format!(
        "https://web.ifzq.gtimg.cn/appstock/app/fqkline/get?param={symbol},day,,,{KLINE_COUNT},{fq}");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))?;
    let mut last_err = None;
    for _ in 0..3 {
        match client.get(&url).header("User-Agent", "Mozilla/5.0").send().and_then(|r| r.text()) {
            Ok(body) => return parse_array(&body, key),
            Err(e) => last_err = Some(anyhow!("{e}")),
        }
    }
    Err(anyhow!("腾讯抓取失败(重试3次): {}", last_err.unwrap()))
}

/// 抓不复权 OHLCV（深）+ 后复权收盘（约2.5年），merge_clip 成 StockBar。
pub fn fetch(secid: &Secid) -> Result<Vec<StockBar>> {
    let sym = symbol(secid).ok_or_else(|| anyhow!("腾讯不支持的市场代码: {}", secid.market))?;
    let raw = fetch_array(&sym, "", "day")?;
    if raw.is_empty() { return Err(anyhow!("{sym} 无K线数据")); }
    // 后复权失败不致命：退化为不复权（merge_clip 处理）。
    let adj = fetch_array(&sym, "hfq", "hfqday").unwrap_or_default();
    Ok(merge_clip(raw, adj))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    #[test]
    fn symbol_maps_markets() {
        assert_eq!(symbol(&Secid { market: 1, code: "600519".into() }).unwrap(), "sh600519");
        assert_eq!(symbol(&Secid { market: 0, code: "000858".into() }).unwrap(), "sz000858");
        assert_eq!(symbol(&Secid { market: 116, code: "00700".into() }).unwrap(), "hk00700");
        assert_eq!(symbol(&Secid { market: 105, code: "AAPL".into() }).unwrap(), "usAAPL.OQ");
        assert_eq!(symbol(&Secid { market: 106, code: "BABA".into() }).unwrap(), "usBABA.N");
        assert!(symbol(&Secid { market: 999, code: "X".into() }).is_none());
    }

    // 腾讯行序：date,open,close,high,low,volume（同东财 fields2 序）
    const RAW: &str = r#"{"code":0,"msg":"","data":{"sh600519":{"day":[
        ["2024-01-02","100.0","110.0","112.0","99.0","10000"],
        ["2024-01-03","110.0","121.0","122.0","109.0","12000"]]}}}"#;
    const HFQ: &str = r#"{"code":0,"msg":"","data":{"sh600519":{"hfqday":[
        ["2024-01-03","220.0","242.0","244.0","218.0","12000"]]}}}"#;

    #[test]
    fn parse_reads_ohlcv_in_tencent_order() {
        let rows = parse_array(RAW, "day").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, d(2024, 1, 2));
        assert!((rows[0].open - 100.0).abs() < 1e-9);
        assert!((rows[0].close - 110.0).abs() < 1e-9, "close 是第2字段");
        assert!((rows[0].high - 112.0).abs() < 1e-9);
        assert!((rows[0].low - 99.0).abs() < 1e-9);
        assert!((rows[0].volume - 10000.0).abs() < 1e-9);
    }

    #[test]
    fn parse_errors_on_nonzero_code() {
        assert!(parse_array(r#"{"code":-1,"msg":"参数错误","data":null}"#, "day").is_err());
    }

    #[test]
    fn parse_tolerates_trailing_dividend_object() {
        // 港股 raw 行尾常带分红对象，应按前6列解析、忽略尾部
        let body = r#"{"code":0,"msg":"","data":{"hk00700":{"day":[
            ["2024-01-02","400.0","410.0","412.0","399.0","5000",{"cqr":"2024-01-02"}]]}}}"#;
        let rows = parse_array(body, "day").unwrap();
        assert_eq!(rows.len(), 1);
        assert!((rows[0].close - 410.0).abs() < 1e-9);
    }

    #[test]
    fn merge_clip_keeps_only_hfq_covered_range() {
        // raw 2 天、hfq 仅 1 天 → 仅保留交集那天，close 取 raw、adj_close 取 hfq
        let raw = parse_array(RAW, "day").unwrap();
        let hfq = parse_array(HFQ, "hfqday").unwrap();
        let bars = merge_clip(raw, hfq);
        assert_eq!(bars.len(), 1, "仅保留 hfq 覆盖的 1 天");
        assert_eq!(bars[0].date, d(2024, 1, 3));
        assert!((bars[0].close - 121.0).abs() < 1e-9, "close 用 raw");
        assert!((bars[0].adj_close - 242.0).abs() < 1e-9, "adj_close 用 hfq");
    }

    #[test]
    fn merge_clip_falls_back_to_raw_when_no_hfq() {
        let raw = parse_array(RAW, "day").unwrap();
        let bars = merge_clip(raw, Vec::new());
        assert_eq!(bars.len(), 2, "无 hfq 时保留全部 raw");
        assert!((bars[0].adj_close - bars[0].close).abs() < 1e-9, "adj_close 回退 raw close");
    }
}

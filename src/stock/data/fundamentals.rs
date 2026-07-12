//! 基本面（财报）数据源。
//!
//! 行情（K线）与财报是两套独立数据源：腾讯后复权 K线只覆盖约 2.5 年，
//! 而选股因子要算 3/5 年营收 CAGR、ROE 持续性，必须有 10 年以上的财报历史。
//! 东财 datacenter 的财报接口能给到 20+ 年，故财报走这里，不受 K线历史长度约束。
//!
//! A股与港股是两个不同的 reportName、不同列名，但字段语义一一对应，
//! 在此归一化成同一个 `FinReport`。美股暂不支持。

use anyhow::{anyhow, Result};
use chrono::{Datelike, NaiveDate};
use serde::Deserialize;
use std::path::Path;

use super::secid::Secid;

/// 一期财报。金额单位为元（港股为港元/人民币，随原报表口径，跨市场不可直接比金额，只比率）。
///
/// 注意：营收/净利是**年初至今累计值**（YTD），不是单季值 —— Q1/H1/Q3/FY 四种口径。
/// 算同比增长要拿同口径期比（用 `annuals()` 取年报最稳）。
/// 各字段为 `Option`：银行等金融股无毛利率，接口返回 null，此时应视为"不适用"而非 0。
#[derive(Debug, Clone, PartialEq)]
pub struct FinReport {
    pub date: NaiveDate,
    /// 营业总收入（累计）
    pub revenue: Option<f64>,
    /// 营收同比 %
    pub revenue_yoy: Option<f64>,
    /// 归母净利润（累计）
    pub net_profit: Option<f64>,
    /// 归母净利同比 %
    pub net_profit_yoy: Option<f64>,
    /// 加权净资产收益率 %（累计口径：Q1 的值约为年化的 1/4，勿直接与年报比）
    pub roe: Option<f64>,
    /// 销售毛利率 %
    pub gross_margin: Option<f64>,
    /// 每股净资产
    pub bps: Option<f64>,
    /// 基本每股收益
    pub eps: Option<f64>,
}

// ---- A股 ----

#[derive(Deserialize)]
struct AResp { result: Option<AResult> }
#[derive(Deserialize)]
struct AResult { #[serde(default)] data: Vec<ARow> }
#[derive(Deserialize)]
struct ARow {
    #[serde(rename = "REPORTDATE")] report_date: String,
    #[serde(rename = "TOTAL_OPERATE_INCOME")] revenue: Option<f64>,
    #[serde(rename = "YSTZ")] revenue_yoy: Option<f64>,
    #[serde(rename = "PARENT_NETPROFIT")] net_profit: Option<f64>,
    #[serde(rename = "SJLTZ")] net_profit_yoy: Option<f64>,
    #[serde(rename = "WEIGHTAVG_ROE")] roe: Option<f64>,
    #[serde(rename = "XSMLL")] gross_margin: Option<f64>,
    #[serde(rename = "BPS")] bps: Option<f64>,
    #[serde(rename = "BASIC_EPS")] eps: Option<f64>,
}

// ---- 港股 ----

#[derive(Deserialize)]
struct HResp { result: Option<HResult> }
#[derive(Deserialize)]
struct HResult { #[serde(default)] data: Vec<HRow> }
#[derive(Deserialize)]
struct HRow {
    #[serde(rename = "STD_REPORT_DATE")] report_date: String,
    #[serde(rename = "OPERATE_INCOME")] revenue: Option<f64>,
    #[serde(rename = "OPERATE_INCOME_YOY")] revenue_yoy: Option<f64>,
    #[serde(rename = "HOLDER_PROFIT")] net_profit: Option<f64>,
    #[serde(rename = "HOLDER_PROFIT_YOY")] net_profit_yoy: Option<f64>,
    #[serde(rename = "ROE_AVG")] roe: Option<f64>,
    #[serde(rename = "GROSS_PROFIT_RATIO")] gross_margin: Option<f64>,
    #[serde(rename = "BPS")] bps: Option<f64>,
    #[serde(rename = "BASIC_EPS")] eps: Option<f64>,
}

/// "2026-03-31 00:00:00" → NaiveDate
fn parse_date(s: &str) -> Result<NaiveDate> {
    let head = s.get(..10).ok_or_else(|| anyhow!("财报日期过短: {s}"))?;
    NaiveDate::parse_from_str(head, "%Y-%m-%d").map_err(|e| anyhow!("解析财报日期 {s} 失败: {e}"))
}

pub fn parse_a(body: &str) -> Result<Vec<FinReport>> {
    let resp: AResp = serde_json::from_str(body).map_err(|e| anyhow!("解析A股财报JSON失败: {e}"))?;
    let Some(result) = resp.result else { return Ok(Vec::new()); };
    let mut out = Vec::with_capacity(result.data.len());
    for r in result.data {
        out.push(FinReport {
            date: parse_date(&r.report_date)?,
            revenue: r.revenue,
            revenue_yoy: r.revenue_yoy,
            net_profit: r.net_profit,
            net_profit_yoy: r.net_profit_yoy,
            roe: r.roe,
            gross_margin: r.gross_margin,
            bps: r.bps,
            eps: r.eps,
        });
    }
    out.sort_by_key(|r| r.date);
    Ok(out)
}

pub fn parse_hk(body: &str) -> Result<Vec<FinReport>> {
    let resp: HResp = serde_json::from_str(body).map_err(|e| anyhow!("解析港股财报JSON失败: {e}"))?;
    let Some(result) = resp.result else { return Ok(Vec::new()); };
    let mut out = Vec::with_capacity(result.data.len());
    for r in result.data {
        out.push(FinReport {
            date: parse_date(&r.report_date)?,
            revenue: r.revenue,
            revenue_yoy: r.revenue_yoy,
            net_profit: r.net_profit,
            net_profit_yoy: r.net_profit_yoy,
            roe: r.roe,
            gross_margin: r.gross_margin,
            bps: r.bps,
            eps: r.eps,
        });
    }
    out.sort_by_key(|r| r.date);
    Ok(out)
}

/// 只取年报（12-31）。营收/净利/ROE 是 YTD 累计值，只有年报之间才是同口径可比的。
pub fn annuals(reports: &[FinReport]) -> Vec<&FinReport> {
    reports.iter().filter(|r| r.date.month() == 12 && r.date.day() == 31).collect()
}

fn http() -> Result<reqwest::blocking::Client> {
    // 同 kline.rs：强制 IPv4，东财部分 IPv6 CDN 节点不可达。
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))
}

fn get(url: &str, referer: &str) -> Result<String> {
    let client = http()?;
    let mut last_err = None;
    for _ in 0..2 {
        match client.get(url)
            .header("Referer", referer)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .and_then(|r| r.text())
        {
            Ok(body) => return Ok(body),
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!("抓取财报失败(重试2次): {}", last_err.unwrap()))
}

/// 抓财报历史。pageSize=100 一次即可覆盖 25 年（每年 4 期），无需翻页。
pub fn fetch(secid: &Secid) -> Result<Vec<FinReport>> {
    match secid.market {
        // 沪(1) / 深(0)
        0 | 1 => {
            let url = format!(
                "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_LICO_FN_CPD\
                 &columns=SECURITY_CODE%2CREPORTDATE%2CTOTAL_OPERATE_INCOME%2CYSTZ%2CPARENT_NETPROFIT%2CSJLTZ%2CWEIGHTAVG_ROE%2CXSMLL%2CBPS%2CBASIC_EPS\
                 &filter=(SECURITY_CODE%3D%22{}%22)&pageNumber=1&pageSize=100\
                 &sortColumns=REPORTDATE&sortTypes=-1&source=WEB&client=WEB",
                secid.code);
            parse_a(&get(&url, "https://data.eastmoney.com/")?)
        }
        // 港股(116)
        116 => {
            let url = format!(
                "https://datacenter.eastmoney.com/securities/api/data/v1/get?reportName=RPT_HKF10_FN_MAININDICATOR\
                 &columns=SECUCODE%2CSECURITY_CODE%2CSTD_REPORT_DATE%2COPERATE_INCOME%2COPERATE_INCOME_YOY%2CHOLDER_PROFIT%2CHOLDER_PROFIT_YOY%2CROE_AVG%2CGROSS_PROFIT_RATIO%2CBPS%2CBASIC_EPS\
                 &filter=(SECUCODE%3D%22{}.HK%22)&pageNumber=1&pageSize=100\
                 &sortColumns=STD_REPORT_DATE&sortTypes=-1&source=F10&client=PC",
                secid.code);
            parse_hk(&get(&url, "https://emweb.securities.eastmoney.com/")?)
        }
        m => Err(anyhow!("市场 {m} 暂不支持财报数据（仅沪深A股与港股）")),
    }
}

// ---- CSV 缓存 ----

const HEADER: &str = "date,revenue,revenue_yoy,net_profit,net_profit_yoy,roe,gross_margin,bps,eps";

fn fmt(v: Option<f64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_default()
}

/// 空串 → None（接口对不适用字段返回 null，如银行无毛利率）
fn num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() { None } else { t.parse().ok() }
}

pub fn write_csv(path: &Path, reports: &[FinReport]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from(HEADER);
    s.push('\n');
    for r in reports {
        s.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            r.date,
            fmt(r.revenue), fmt(r.revenue_yoy),
            fmt(r.net_profit), fmt(r.net_profit_yoy),
            fmt(r.roe), fmt(r.gross_margin), fmt(r.bps), fmt(r.eps),
        ));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写财报缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<FinReport>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读财报缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 9 { continue; }
        out.push(FinReport {
            date: NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?,
            revenue: num(c[1]), revenue_yoy: num(c[2]),
            net_profit: num(c[3]), net_profit_yoy: num(c[4]),
            roe: num(c[5]), gross_margin: num(c[6]), bps: num(c[7]), eps: num(c[8]),
        });
    }
    out.sort_by_key(|r| r.date);
    Ok(out)
}

/// 财报每季度才更新一次，缓存 `max_age_days` 天内直接读盘，过期才重抓。
///
/// 与 K线的 `cache::load_or_fetch` 语义不同：K线按"是否覆盖窗口"判断，
/// 财报按"缓存新鲜度"判断 —— 因为最新一期财报的存在与否无法从已有数据推断。
pub fn load_or_fetch(input: &str, cache_dir: &Path, max_age_days: i64, today: NaiveDate) -> Result<Vec<FinReport>> {
    let secid = super::resolve_secid(input)?;
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    if path.exists() {
        if let Ok(cached) = read_csv(&path) {
            let fresh_enough = cached.last()
                .map(|_| file_age_days(&path, today).map(|a| a <= max_age_days).unwrap_or(false))
                .unwrap_or(false);
            if fresh_enough { return Ok(cached); }
        }
    }
    let fresh = fetch(&secid)?;
    if fresh.is_empty() { return Err(anyhow!("股票 {input} 无财报数据")); }
    write_csv(&path, &fresh)?;
    Ok(fresh)
}

fn file_age_days(path: &Path, today: NaiveDate) -> Option<i64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let mdate = chrono::DateTime::from_timestamp(secs, 0)?.date_naive();
    Some((today - mdate).num_days())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    // 取自东财实测响应（600519 贵州茅台）
    const A_BODY: &str = r#"{"result":{"pages":17,"data":[
        {"SECURITY_CODE":"600519","REPORTDATE":"2026-03-31 00:00:00","TOTAL_OPERATE_INCOME":54702912385.23,"YSTZ":6.3360092771,"PARENT_NETPROFIT":27242512886.45,"SJLTZ":1.47,"WEIGHTAVG_ROE":10.57,"XSMLL":89.7592176242,"BPS":216.32234994607,"BASIC_EPS":21.76},
        {"SECURITY_CODE":"600519","REPORTDATE":"2025-12-31 00:00:00","TOTAL_OPERATE_INCOME":172054171890.91,"YSTZ":-1.2000971769,"PARENT_NETPROFIT":82320067101.68,"SJLTZ":-4.53,"WEIGHTAVG_ROE":32.53,"XSMLL":91.1795516835,"BPS":195.355449727901,"BASIC_EPS":65.66}]}}"#;

    // 取自东财实测响应（00700 腾讯）
    const HK_BODY: &str = r#"{"result":{"pages":24,"data":[
        {"SECUCODE":"00700.HK","SECURITY_CODE":"00700","STD_REPORT_DATE":"2025-12-31 00:00:00","OPERATE_INCOME":751766000000,"OPERATE_INCOME_YOY":13.8596031545,"HOLDER_PROFIT":224842000000,"HOLDER_PROFIT_YOY":15.8543434687,"ROE_AVG":21.134746439818,"GROSS_PROFIT_RATIO":56.213369585749,"BPS":126.934279061592,"BASIC_EPS":24.749}]}}"#;

    #[test]
    fn parse_a_maps_eastmoney_columns_and_sorts_ascending() {
        let rs = parse_a(A_BODY).unwrap();
        assert_eq!(rs.len(), 2);
        // 接口按日期倒序返回，parse 后必须是升序
        assert_eq!(rs[0].date, d(2025, 12, 31));
        assert_eq!(rs[1].date, d(2026, 3, 31));
        let fy = &rs[0];
        assert!((fy.revenue.unwrap() - 172_054_171_890.91).abs() < 1.0);
        assert!((fy.net_profit.unwrap() - 82_320_067_101.68).abs() < 1.0);
        assert!((fy.roe.unwrap() - 32.53).abs() < 1e-9, "WEIGHTAVG_ROE → roe");
        assert!((fy.gross_margin.unwrap() - 91.1795516835).abs() < 1e-9, "XSMLL → gross_margin");
        assert!((fy.eps.unwrap() - 65.66).abs() < 1e-9);
        assert!((fy.revenue_yoy.unwrap() - (-1.2000971769)).abs() < 1e-9, "YSTZ 可为负");
    }

    #[test]
    fn parse_hk_maps_different_column_names_to_same_struct() {
        let rs = parse_hk(HK_BODY).unwrap();
        assert_eq!(rs.len(), 1);
        let r = &rs[0];
        assert_eq!(r.date, d(2025, 12, 31));
        assert!((r.revenue.unwrap() - 751_766_000_000.0).abs() < 1.0, "OPERATE_INCOME → revenue");
        assert!((r.net_profit.unwrap() - 224_842_000_000.0).abs() < 1.0, "HOLDER_PROFIT → net_profit");
        assert!((r.roe.unwrap() - 21.134746439818).abs() < 1e-9, "ROE_AVG → roe");
        assert!((r.gross_margin.unwrap() - 56.213369585749).abs() < 1e-9, "GROSS_PROFIT_RATIO → gross_margin");
    }

    #[test]
    fn null_fields_become_none_not_zero() {
        // 银行股无毛利率，接口返回 null —— 必须是 None，不能当 0（0% 毛利率会被因子误判为极差）
        let body = r#"{"result":{"data":[
            {"SECURITY_CODE":"000001","REPORTDATE":"2025-12-31 00:00:00","TOTAL_OPERATE_INCOME":1.0,"YSTZ":null,"PARENT_NETPROFIT":2.0,"SJLTZ":null,"WEIGHTAVG_ROE":11.0,"XSMLL":null,"BPS":null,"BASIC_EPS":null}]}}"#;
        let rs = parse_a(body).unwrap();
        assert_eq!(rs[0].gross_margin, None);
        assert_eq!(rs[0].revenue_yoy, None);
        assert_eq!(rs[0].roe, Some(11.0));
    }

    #[test]
    fn parse_empty_when_no_result() {
        assert!(parse_a(r#"{"result":null}"#).unwrap().is_empty());
        assert!(parse_hk(r#"{"result":null}"#).unwrap().is_empty());
    }

    #[test]
    fn annuals_keeps_only_year_end_reports() {
        let rs = parse_a(A_BODY).unwrap();
        let a = annuals(&rs);
        assert_eq!(a.len(), 1, "只有 2025-12-31 是年报，2026-03-31 是一季报");
        assert_eq!(a[0].date, d(2025, 12, 31));
    }

    /// 实网测试：默认不跑（`cargo test -- --ignored` 手动触发）。
    /// 重点验证「财报历史远长于K线历史」这个前提 —— 整个因子设计都依赖它。
    #[test]
    #[ignore]
    fn live_fetch_a_and_hk_fundamentals() {
        // A股：茅台
        let a = fetch(&Secid { market: 1, code: "600519".into() }).expect("抓茅台财报");
        let ay = annuals(&a);
        assert!(ay.len() >= 10, "茅台年报应有10年以上，实得 {} 期", ay.len());
        let last = ay.last().unwrap();
        assert!(last.roe.unwrap() > 20.0, "茅台年报ROE应>20%，实得 {:?}", last.roe);
        assert!(last.gross_margin.unwrap() > 80.0, "茅台毛利率应>80%，实得 {:?}", last.gross_margin);

        // 关键前提：财报历史必须显著长于腾讯K线的 ~2.5 年，否则算不了 5 年 CAGR
        let span = last.date.year() - ay.first().unwrap().date.year();
        assert!(span >= 10, "财报历史跨度仅 {span} 年，不足以支撑长周期CAGR因子");
        println!("茅台年报 {} 期，跨 {span} 年，最新 ROE={:?} 毛利率={:?}",
                 ay.len(), last.roe, last.gross_margin);

        // 港股：腾讯（走另一套 reportName + 列名，必须归一化到同一 struct）
        let h = fetch(&Secid { market: 116, code: "00700".into() }).expect("抓腾讯财报");
        let hy = annuals(&h);
        assert!(hy.len() >= 5, "腾讯年报应有5年以上，实得 {}", hy.len());
        let hl = hy.last().unwrap();
        assert!(hl.revenue.unwrap() > 1e11, "腾讯年营收应>1000亿");
        println!("腾讯年报 {} 期，最新营收={:?} ROE={:?}", hy.len(), hl.revenue, hl.roe);

        // 美股应明确报错而非静默返回空
        assert!(fetch(&Secid { market: 105, code: "AAPL".into() }).is_err(), "美股应明确不支持");
    }

    #[test]
    fn csv_roundtrip_preserves_none_as_empty() {
        let reports = vec![FinReport {
            date: d(2025, 12, 31),
            revenue: Some(1.5), revenue_yoy: None,
            net_profit: Some(-2.5), net_profit_yoy: Some(3.0),
            roe: Some(32.53), gross_margin: None, bps: Some(195.35), eps: Some(65.66),
        }];
        let tmp = std::env::temp_dir().join("xlh_fundamentals_test.csv");
        write_csv(&tmp, &reports).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].revenue_yoy, None, "None 存空串，读回仍是 None");
        assert_eq!(back[0].gross_margin, None);
        assert_eq!(back[0].revenue, Some(1.5));
        assert_eq!(back[0].net_profit, Some(-2.5));
        assert!((back[0].roe.unwrap() - 32.53).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }
}

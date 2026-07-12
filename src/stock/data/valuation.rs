//! 单只股票的日频估值历史（PE_TTM / PB_MRQ）。
//!
//! 与 `universe.rs` 同一张东财表（`RPT_VALUEANALYSIS_DET`），但按代码过滤、取全部交易日
//! —— 实测茅台可回溯到 2018-01-02，约 2000+ 个交易日。
//!
//! 存在的理由有两个，都做不到就没有诚实的选股/归因可言：
//!
//! 1. **估值分位**：「PE 处于自身历史 20% 分位」这类因子，必须有该股自己的 PE 时间序列。
//!    横截面 z-score（`recommend.rs` 现有做法）回答不了「相对它自己贵不贵」。
//!
//! 2. **归因分解**：把总回报拆成 盈利增长 × 估值扩张 × 分红再投资，需要起止两点的 PE。
//!    研究反复强调归因**对起点极度敏感** —— 茅台以 IPO 为锚是「87% 盈利驱动」，
//!    以 2014 年 PE 8.83 倍的塑化剂低点为锚则估值扩张单项就贡献 5–6 倍，结论直接反转。
//!    所以起点 PE 分位必须和归因结果一起展示，否则就是在误导。

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;

use super::secid::Secid;
use super::universe::{client, get, DC_BASE};

/// 某个交易日的估值点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValPoint {
    pub date: NaiveDate,
    /// 市盈率 TTM。负值 = 近12个月亏损（合法数据）。
    pub pe_ttm: Option<f64>,
    /// 市净率 MRQ
    pub pb_mrq: Option<f64>,
}

#[derive(Deserialize)]
struct Resp { result: Option<Res> }
#[derive(Deserialize)]
struct Res {
    #[serde(default)] data: Vec<Row>,
    #[serde(default)] count: usize,
}
#[derive(Deserialize)]
struct Row {
    #[serde(rename = "TRADE_DATE")] date: String,
    #[serde(rename = "PE_TTM")] pe_ttm: Option<f64>,
    #[serde(rename = "PB_MRQ")] pb_mrq: Option<f64>,
}

pub fn parse_page(body: &str) -> Result<(Vec<ValPoint>, usize)> {
    let resp: Resp = serde_json::from_str(body).map_err(|e| anyhow!("解析估值历史JSON失败: {e}"))?;
    let Some(r) = resp.result else { return Ok((Vec::new(), 0)); };
    let count = r.count;
    let mut out = Vec::with_capacity(r.data.len());
    for d in r.data {
        let head = d.date.get(..10).ok_or_else(|| anyhow!("估值日期过短: {}", d.date))?;
        out.push(ValPoint {
            date: NaiveDate::parse_from_str(head, "%Y-%m-%d")?,
            pe_ttm: d.pe_ttm,
            pb_mrq: d.pb_mrq,
        });
    }
    out.sort_by_key(|p| p.date);
    Ok((out, count))
}

const PAGE_SIZE: usize = 1000;
const MAX_PAGES: usize = 15;

/// 抓单只股票的全部估值历史。
///
/// 仅支持沪深 A 股 —— datacenter 上没有港股估值表（探测过 RPT_HK_VALUEANALYSIS_DET 等均
/// 返回「报表配置不存在」）。港股的分位/归因因子因此无数据基础，调用方须据此降级，
/// 不要用当日快照冒充历史。
pub fn fetch(secid: &Secid) -> Result<Vec<ValPoint>> {
    if !matches!(secid.market, 0 | 1) {
        return Err(anyhow!("市场 {} 无估值历史数据（仅沪深A股）", secid.market));
    }
    let c = client()?;
    let mut all = Vec::new();
    let mut total = usize::MAX;
    for page in 1..=MAX_PAGES {
        if page > 1 { std::thread::sleep(std::time::Duration::from_millis(150)); }
        let url = format!(
            "{DC_BASE}&columns=SECURITY_CODE%2CTRADE_DATE%2CPE_TTM%2CPB_MRQ\
             &filter=(SECURITY_CODE%3D%22{}%22)&pageNumber={page}&pageSize={PAGE_SIZE}\
             &sortColumns=TRADE_DATE&sortTypes=1",
            secid.code);
        let (rows, count) = parse_page(&get(&c, &url, "https://data.eastmoney.com/")?)?;
        if page == 1 { total = count; }
        let got = rows.len();
        all.extend(rows);
        if got < PAGE_SIZE || all.len() >= total { break; }
    }
    all.sort_by_key(|p| p.date);
    Ok(all)
}

/// `v` 在历史序列中的分位（0.0 = 史上最低，1.0 = 史上最高）。
///
/// 只对**正 PE** 计算：亏损期 PE 为负，把它和正 PE 放一起排序会得出「PE 越负越便宜」的
/// 荒谬结论 —— 一只巨亏股的 PE=-0.4 会排到分位 0（"史上最便宜"）。
/// 故负值一律剔除；样本不足 60 个交易日返回 None（分位本身不可信）。
pub fn percentile(history: &[f64], v: f64) -> Option<f64> {
    if v <= 0.0 { return None; }
    let mut xs: Vec<f64> = history.iter().copied().filter(|x| *x > 0.0).collect();
    if xs.len() < 60 { return None; }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let below = xs.partition_point(|x| *x < v);
    Some(below as f64 / xs.len() as f64)
}

/// 序列中所有正的 PE_TTM。
pub fn positive_pes(points: &[ValPoint]) -> Vec<f64> {
    points.iter().filter_map(|p| p.pe_ttm).filter(|x| *x > 0.0).collect()
}

/// 序列中所有正的 PB_MRQ。
pub fn positive_pbs(points: &[ValPoint]) -> Vec<f64> {
    points.iter().filter_map(|p| p.pb_mrq).filter(|x| *x > 0.0).collect()
}

/// 取 `date` 当日或之前最近一个有正 PE 的估值点（停牌/非交易日回溯）。
pub fn at_or_before(points: &[ValPoint], date: NaiveDate) -> Option<ValPoint> {
    points.iter().rev().find(|p| p.date <= date).copied()
}

// ---- CSV 缓存 ----

const HEADER: &str = "date,pe_ttm,pb_mrq";

fn fmt(v: Option<f64>) -> String { v.map(|x| x.to_string()).unwrap_or_default() }
fn num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() { None } else { t.parse().ok() }
}

pub fn write_csv(path: &Path, points: &[ValPoint]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from(HEADER);
    s.push('\n');
    for p in points {
        s.push_str(&format!("{},{},{}\n", p.date, fmt(p.pe_ttm), fmt(p.pb_mrq)));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写估值缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<ValPoint>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读估值缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 3 { continue; }
        out.push(ValPoint {
            date: NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?,
            pe_ttm: num(c[1]), pb_mrq: num(c[2]),
        });
    }
    out.sort_by_key(|p| p.date);
    Ok(out)
}

/// 估值历史每交易日新增一条；缓存最后一条日期 >= 目标交易日则直接读盘。
pub fn load_or_fetch(input: &str, cache_dir: &Path, upto: NaiveDate) -> Result<Vec<ValPoint>> {
    let secid = super::resolve_secid(input)?;
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    if path.exists() {
        if let Ok(cached) = read_csv(&path) {
            if cached.last().map(|p| p.date >= upto).unwrap_or(false) {
                return Ok(cached);
            }
        }
    }
    let fresh = fetch(&secid)?;
    if fresh.is_empty() { return Err(anyhow!("股票 {input} 无估值历史")); }
    write_csv(&path, &fresh)?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    /// 逐字取自 datacenter 实测响应（600519，回溯到 2018-01-02）
    const BODY: &str = r#"{"result":{"pages":689,"data":[
        {"SECURITY_CODE":"600519","TRADE_DATE":"2018-01-02 00:00:00","PE_TTM":36.48092785,"PB_MRQ":9.66823508},
        {"SECURITY_CODE":"600519","TRADE_DATE":"2018-01-03 00:00:00","PE_TTM":37.10341267,"PB_MRQ":9.83320703},
        {"SECURITY_CODE":"600519","TRADE_DATE":"2018-01-04 00:00:00","PE_TTM":38.20273849,"PB_MRQ":10.12455215}],"count":2066}}"#;

    #[test]
    fn parses_history_ascending_with_total_count() {
        let (ps, count) = parse_page(BODY).unwrap();
        assert_eq!(count, 2066, "count 用于判断是否还要翻页");
        assert_eq!(ps.len(), 3);
        assert_eq!(ps[0].date, d(2018, 1, 2));
        assert!((ps[0].pe_ttm.unwrap() - 36.48092785).abs() < 1e-9);
        assert!((ps[2].pb_mrq.unwrap() - 10.12455215).abs() < 1e-9);
    }

    #[test]
    fn percentile_needs_enough_samples() {
        let short: Vec<f64> = (1..=59).map(|i| i as f64).collect();
        assert_eq!(percentile(&short, 30.0), None, "样本<60 → 分位不可信");
        let ok: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        assert!(percentile(&ok, 30.0).is_some());
    }

    #[test]
    fn percentile_ranks_correctly() {
        let h: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        // 30 大于 1..29 共 29 个 → 29/100
        assert!((percentile(&h, 30.0).unwrap() - 0.29).abs() < 1e-9);
        assert!((percentile(&h, 1.0).unwrap() - 0.0).abs() < 1e-9, "史上最低 → 0");
        assert!((percentile(&h, 101.0).unwrap() - 1.0).abs() < 1e-9, "高于所有历史 → 1");
    }

    #[test]
    fn negative_pe_never_looks_cheap() {
        // 这是最容易写错、且错了不会报错的地方：
        // 巨亏股 PE=-0.42，若不剔除负值，排序后它会落在最低分位 → 被判定为「史上最便宜」
        let mut h: Vec<f64> = (10..=80).map(|i| i as f64).collect();
        h.push(-0.42);
        assert_eq!(percentile(&h, -0.42), None, "负 PE 无分位可言，必须返回 None 而非 0");

        // 负值也不能污染别人的分位：正常值的分位应只在正 PE 上算
        let with_neg = percentile(&h, 10.0).unwrap();
        let clean: Vec<f64> = (10..=80).map(|i| i as f64).collect();
        let without_neg = percentile(&clean, 10.0).unwrap();
        assert!((with_neg - without_neg).abs() < 1e-9, "负值须被剔除，不参与分母");
    }

    #[test]
    fn positive_filters_drop_losses_and_nulls() {
        let ps = vec![
            ValPoint { date: d(2024,1,1), pe_ttm: Some(20.0), pb_mrq: Some(2.0) },
            ValPoint { date: d(2024,1,2), pe_ttm: Some(-5.0), pb_mrq: Some(1.5) },
            ValPoint { date: d(2024,1,3), pe_ttm: None, pb_mrq: None },
        ];
        assert_eq!(positive_pes(&ps), vec![20.0]);
        assert_eq!(positive_pbs(&ps), vec![2.0, 1.5]);
    }

    #[test]
    fn at_or_before_backtracks_over_holidays() {
        let ps = vec![
            ValPoint { date: d(2024,1,5), pe_ttm: Some(20.0), pb_mrq: Some(2.0) },
            ValPoint { date: d(2024,1,8), pe_ttm: Some(21.0), pb_mrq: Some(2.1) },
        ];
        // 1/6、1/7 是周末 → 回溯到 1/5
        assert_eq!(at_or_before(&ps, d(2024,1,7)).unwrap().date, d(2024,1,5));
        assert_eq!(at_or_before(&ps, d(2024,1,8)).unwrap().date, d(2024,1,8));
        assert!(at_or_before(&ps, d(2024,1,1)).is_none(), "早于全部历史 → None");
    }

    #[test]
    fn csv_roundtrip() {
        let ps = vec![ValPoint { date: d(2018,1,2), pe_ttm: Some(36.48), pb_mrq: None }];
        let tmp = std::env::temp_dir().join("xlh_valuation_test.csv");
        write_csv(&tmp, &ps).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back[0].pb_mrq, None);
        assert!((back[0].pe_ttm.unwrap() - 36.48).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn hk_and_us_have_no_valuation_history() {
        assert!(fetch(&Secid { market: 116, code: "00700".into() }).is_err(), "港股无估值表");
        assert!(fetch(&Secid { market: 105, code: "AAPL".into() }).is_err());
    }

    /// 实网测试：`cargo test -- --ignored`
    #[test]
    #[ignore]
    fn live_fetch_moutai_valuation_history() {
        let ps = fetch(&Secid { market: 1, code: "600519".into() }).expect("抓茅台估值历史");
        assert!(ps.len() > 1500, "应有 2000+ 个交易日，实得 {}", ps.len());

        let pes = positive_pes(&ps);
        assert!(pes.len() > 1500);
        let last = ps.last().unwrap();
        let pct = percentile(&pes, last.pe_ttm.unwrap()).expect("应能算出分位");
        assert!((0.0..=1.0).contains(&pct));

        let lo = pes.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = pes.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        println!("茅台估值历史 {} 天（{} → {}），PE_TTM 区间 {lo:.2}~{hi:.2}，当前 {:.2}（分位 {:.1}%）",
                 ps.len(), ps[0].date, last.date, last.pe_ttm.unwrap(), pct * 100.0);
    }
}

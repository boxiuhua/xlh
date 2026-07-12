//! 全市场股票清单 + 每日估值快照。
//!
//! 项目原先没有「全市场股票全集」—— 股票池是 `src/web/stock.rs` 里硬编码的 12 个代码，
//! `sync::sync_all` 靠扫缓存目录文件名反推「已知股票」。要做真正的筛选必须有全集。
//!
//! ## 为什么不用 push2 的 clist 接口
//!
//! clist（`push2.eastmoney.com/api/qt/clist/get`）能一次拉全市场，但**限流极凶**：
//! 实测连续翻页到第 5 页即被掐断连接（`http=000`，连接被拒而非超时），且封禁会持续一段时间，
//! 期间该端点完全不可用 —— 而同一台 push2 上的单只行情、push2his 的 K线、datacenter 的财报
//! 全都正常。东财显然单独重点保护了这个批量端点。
//!
//! 改用 datacenter 的估值分析表（`RPT_VALUEANALYSIS_DET`），它严格更优：
//!   - 不在限流名单上（与已验证稳定的财报接口同一台主机）
//!   - pageSize 支持 500 → 全 A 股 5530 只只需 12 页（clist 要 55 页）
//!   - 给的是 PE_TTM / PB_MRQ，比 clist 的「动态PE」口径更规范
//!   - **带 TRADE_DATE 参数，可取历史日期** → PE/PB 历史分位、归因分解才做得出来（见 `valuation.rs`）
//!
//! 港股在 datacenter 上没有对应估值表，只能退回 clist，故为 best-effort：
//! 抓不到就只筛 A 股，不让港股接口抖动拖垮整个筛选。

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::secid::Secid;

/// 一只股票的清单条目 + 当日估值快照。
///
/// 注意这里**不存 ROE** —— ROE 由财报（`fundamentals::FinReport::roe`）唯一供给。
/// 清单接口也能给 ROE，但两个源各存一份必然对不上（口径/更新时点不同），
/// 与其调和不如让每个指标只有一个出处。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Listing {
    pub market: u16,
    pub code: String,
    pub name: String,
    /// 总市值（元）
    pub market_cap: Option<f64>,
    /// 市盈率 TTM。**负值 = 近12个月亏损**，是合法数据，不可当缺失丢掉（但筛选要挡）。
    pub pe_ttm: Option<f64>,
    /// 市净率 MRQ
    pub pb_mrq: Option<f64>,
}

impl Listing {
    pub fn secid(&self) -> Secid {
        Secid { market: self.market, code: self.code.clone() }
    }

    /// ST / *ST / 退市 / PT 壳股。财务数据失真、流动性枯竭，
    /// 任何「高增长」筛选命中它们都是假阳性，必须在入池前剔除。
    ///
    /// 注意 `国华退` 这类退市股在估值表里照样有 PE=5.16 的漂亮数字 —— 光看财务指标挡不住，
    /// 只能靠名称。
    pub fn is_risky_shell(&self) -> bool {
        let n = self.name.replace(' ', "");
        n.contains("ST") || n.contains('退') || n.starts_with("PT")
    }

    /// 可进入筛选池：非壳股、有市值。
    /// 亏损股（PE<0）**仍可入池** —— 由因子阈值去挡，不在这里预判。
    pub fn is_screenable(&self) -> bool {
        !self.is_risky_shell() && self.market_cap.map(|c| c > 0.0).unwrap_or(false)
    }
}

// ---- A股：datacenter 估值分析表 ----

#[derive(Deserialize)]
struct DcResp { result: Option<DcResult> }
#[derive(Deserialize)]
struct DcResult {
    #[serde(default)] data: Vec<DcRow>,
    #[serde(default)] count: usize,
}
#[derive(Deserialize)]
struct DcRow {
    #[serde(rename = "SECURITY_CODE")] code: String,
    #[serde(rename = "SECURITY_NAME_ABBR")] name: String,
    #[serde(rename = "PE_TTM")] pe_ttm: Option<f64>,
    #[serde(rename = "PB_MRQ")] pb_mrq: Option<f64>,
    #[serde(rename = "TOTAL_MARKET_CAP")] market_cap: Option<f64>,
}

/// 按代码首位判沪深，与 `secid::resolve_offline` 同规则（估值表不给市场号）
fn market_of(code: &str) -> u16 {
    match code.chars().next() {
        Some('6') | Some('5') | Some('9') => 1,
        _ => 0,
    }
}

/// 解析一页估值表响应，返回 (条目, 全市场总数)。
pub fn parse_snapshot_page(body: &str) -> Result<(Vec<Listing>, usize)> {
    let resp: DcResp = serde_json::from_str(body).map_err(|e| anyhow!("解析估值表JSON失败: {e}"))?;
    let Some(r) = resp.result else { return Ok((Vec::new(), 0)); };
    let count = r.count;
    let out = r.data.into_iter().map(|d| Listing {
        market: market_of(&d.code),
        name: d.name.trim().replace(['　', ' '], ""),
        code: d.code,
        market_cap: d.market_cap,
        pe_ttm: d.pe_ttm,
        pb_mrq: d.pb_mrq,
    }).collect();
    Ok((out, count))
}

pub(crate) const DC_BASE: &str =
    "https://datacenter-web.eastmoney.com/api/data/v1/get?reportName=RPT_VALUEANALYSIS_DET&source=WEB&client=WEB";

const PAGE_SIZE: usize = 500;
const MAX_PAGES: usize = 40;
const PAGE_DELAY: std::time::Duration = std::time::Duration::from_millis(150);

pub(crate) fn client() -> Result<reqwest::blocking::Client> {
    // 复用同一个 client 跨页请求：连接池保活，避免每页重开 TCP/TLS。
    // 同 kline.rs 绑定 IPv4 —— 东财部分 IPv6 CDN 节点不可达。
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))
}

pub(crate) fn get(c: &reqwest::blocking::Client, url: &str, referer: &str) -> Result<String> {
    let mut last_err = None;
    for attempt in 0..4u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(400) * 2u32.pow(attempt - 1));
        }
        match c.get(url).header("Referer", referer).header("User-Agent", "Mozilla/5.0")
            .send().and_then(|r| r.text())
        {
            Ok(b) => return Ok(b),
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!("请求失败(重试4次): {}", last_err.unwrap()))
}

/// 最近一个有估值数据的交易日。
///
/// 不能用「今天」—— 周末/节假日无数据，且盘中数据未必落库。直接问接口要最大 TRADE_DATE。
pub fn latest_trade_date() -> Result<NaiveDate> {
    let c = client()?;
    let url = format!("{DC_BASE}&columns=TRADE_DATE&pageNumber=1&pageSize=1&sortColumns=TRADE_DATE&sortTypes=-1");
    let body = get(&c, &url, "https://data.eastmoney.com/")?;
    let resp: serde_json::Value = serde_json::from_str(&body)?;
    let s = resp["result"]["data"][0]["TRADE_DATE"].as_str()
        .ok_or_else(|| anyhow!("估值表未返回 TRADE_DATE"))?;
    NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d").map_err(|e| anyhow!("解析交易日 {s} 失败: {e}"))
}

/// 拉取指定交易日的全 A 股清单 + 估值快照。
pub fn fetch_a_snapshot(date: NaiveDate) -> Result<Vec<Listing>> {
    let c = client()?;
    let mut all = Vec::new();
    let mut total = usize::MAX;
    for page in 1..=MAX_PAGES {
        if page > 1 { std::thread::sleep(PAGE_DELAY); }
        let url = format!(
            "{DC_BASE}&columns=SECURITY_CODE%2CSECURITY_NAME_ABBR%2CPE_TTM%2CPB_MRQ%2CTOTAL_MARKET_CAP\
             &filter=(TRADE_DATE%3D%27{date}%27)&pageNumber={page}&pageSize={PAGE_SIZE}\
             &sortColumns=SECURITY_CODE&sortTypes=1");
        let (rows, count) = parse_snapshot_page(&get(&c, &url, "https://data.eastmoney.com/")?)?;
        if page == 1 {
            if count == 0 { return Err(anyhow!("{date} 无估值数据（非交易日？）")); }
            total = count;
        }
        let got = rows.len();
        all.extend(rows);
        if got < PAGE_SIZE || all.len() >= total { break; }
    }
    if all.len() < total {
        // 宁可明确报错也不返回残缺全集：少一半股票的「全市场筛选」是静默错误 ——
        // 结果看起来完全正常，只是最好的标的可能根本没进池。
        return Err(anyhow!("清单不完整：应有 {total} 只，仅抓到 {}", all.len()));
    }
    Ok(all)
}

// ---- 港股：退回 clist（best-effort） ----

#[derive(Deserialize)]
struct ClistResp { data: Option<ClistData> }
#[derive(Deserialize)]
struct ClistData { #[serde(default)] diff: Option<Diff> }

/// `diff` 形状随参数而变：带 `np=1` 是数组，否则是 `{"0":{...}}` 的 map。两种都接住。
#[derive(Deserialize)]
#[serde(untagged)]
enum Diff { List(Vec<HkRow>), Map(HashMap<String, HkRow>) }

impl Diff {
    fn into_rows(self) -> Vec<HkRow> {
        match self {
            Diff::List(v) => v,
            Diff::Map(m) => {
                // key 是字符串化序号，须按数值序还原，否则 "10" < "2" 会打乱顺序
                let mut keyed: Vec<(usize, HkRow)> = m.into_iter()
                    .filter_map(|(k, v)| k.parse::<usize>().ok().map(|i| (i, v)))
                    .collect();
                keyed.sort_by_key(|(i, _)| *i);
                keyed.into_iter().map(|(_, r)| r).collect()
            }
        }
    }
}

/// 东财对停牌/无数据的标的返回字符串 "-"，有值时返回数字。
/// 变体顺序有意义：先试 Num，落不进去的由 Other 兜住。
#[derive(Deserialize)]
#[serde(untagged)]
enum Cell { Num(f64), Other(serde::de::IgnoredAny) }

impl Cell {
    fn num(&self) -> Option<f64> {
        match self { Cell::Num(v) => Some(*v), Cell::Other(_) => None }
    }
}

#[derive(Deserialize)]
struct HkRow {
    #[serde(rename = "f12")] code: String,
    #[serde(rename = "f14")] name: String,
    #[serde(rename = "f20")] market_cap: Option<Cell>,
    #[serde(rename = "f9")] pe: Option<Cell>,
    #[serde(rename = "f23")] pb: Option<Cell>,
}

/// 解析 clist 一页（港股）。
///
/// 请求带 `fltt=2`，f9/f23 是**浮点真值**（PE 4.22 就是 4.22）。
/// 别照搬 `push2/stock/get` 单只行情接口的经验 —— 那边同类字段是放大 100 倍的定点整数
/// （f162=1382 表示 PE 13.82），两套接口的编号与量纲都不通用。
pub fn parse_hk_page(body: &str) -> Result<Vec<Listing>> {
    let resp: ClistResp = serde_json::from_str(body).map_err(|e| anyhow!("解析港股清单JSON失败: {e}"))?;
    let rows = resp.data.and_then(|d| d.diff).map(|d| d.into_rows()).unwrap_or_default();
    Ok(rows.into_iter().map(|r| Listing {
        market: 116,
        name: r.name.trim().replace(['　', ' '], ""),
        code: r.code,
        market_cap: r.market_cap.as_ref().and_then(|c| c.num()),
        pe_ttm: r.pe.as_ref().and_then(|c| c.num()),
        pb_mrq: r.pb.as_ref().and_then(|c| c.num()),
    }).collect())
}

const FS_HK: &str = "m:128+t:3,m:128+t:4,m:128+t:1,m:128+t:2";

/// 港股清单（best-effort）。clist 限流严重，此处只做有限翻页，失败由调用方降级。
pub fn fetch_hk() -> Result<Vec<Listing>> {
    let c = client()?;
    let mut all = Vec::new();
    for page in 1..=30 {
        if page > 1 { std::thread::sleep(std::time::Duration::from_millis(400)); }
        let url = format!(
            "https://push2.eastmoney.com/api/qt/clist/get?pn={page}&pz=200&po=0&np=1\
             &fltt=2&invt=2&fid=f12&fs={FS_HK}&fields=f12,f14,f9,f20,f23");
        let rows = parse_hk_page(&get(&c, &url, "https://quote.eastmoney.com/")?)?;
        let got = rows.len();
        all.extend(rows);
        if got < 200 { break; }
    }
    if all.is_empty() { return Err(anyhow!("港股清单为空")); }
    Ok(all)
}

/// A股 + 港股全集。港股失败不致命，降级为只筛 A 股。
pub fn fetch_all(date: NaiveDate) -> Result<Vec<Listing>> {
    let mut all = fetch_a_snapshot(date)?;
    match fetch_hk() {
        Ok(hk) => all.extend(hk),
        // 静默降级会让用户以为筛了港股其实没筛 —— 必须出声。
        Err(e) => eprintln!("警告: 港股清单抓取失败，本次仅筛选A股 ({e})"),
    }
    Ok(all)
}

// ---- CSV 缓存 ----

const HEADER: &str = "market,code,name,market_cap,pe_ttm,pb_mrq";

fn fmt(v: Option<f64>) -> String { v.map(|x| x.to_string()).unwrap_or_default() }
fn num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() { None } else { t.parse().ok() }
}

pub fn write_csv(path: &Path, rows: &[Listing]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from(HEADER);
    s.push('\n');
    for r in rows {
        // 名称里的逗号会撕裂 CSV 列 → 统一剔除
        let name = r.name.replace([',', '，'], "");
        s.push_str(&format!("{},{},{},{},{},{}\n",
            r.market, r.code, name, fmt(r.market_cap), fmt(r.pe_ttm), fmt(r.pb_mrq)));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写清单缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<Listing>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读清单缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 6 { continue; }
        out.push(Listing {
            market: c[0].parse()?,
            code: c[1].to_string(),
            name: c[2].to_string(),
            market_cap: num(c[3]), pe_ttm: num(c[4]), pb_mrq: num(c[5]),
        });
    }
    Ok(out)
}

/// 清单+估值快照按交易日缓存（同一交易日的数据不会变）。
pub fn load_or_fetch(cache_dir: &Path, date: NaiveDate) -> Result<Vec<Listing>> {
    let path = cache_dir.join(format!("universe_{date}.csv"));
    if path.exists() {
        if let Ok(rows) = read_csv(&path) {
            if !rows.is_empty() { return Ok(rows); }
        }
    }
    let fresh = fetch_all(date)?;
    if fresh.is_empty() { return Err(anyhow!("全市场清单为空")); }
    write_csv(&path, &fresh)?;
    Ok(fresh)
}

/// code → 中文名。补上项目此前缺失的名称映射
/// （`web/stock.rs` 传空 HashMap、`push/job.rs` 直接拿 code 当 name）。
pub fn name_map(rows: &[Listing]) -> HashMap<String, String> {
    rows.iter().map(|r| (r.code.clone(), r.name.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐字取自 datacenter RPT_VALUEANALYSIS_DET 实测响应（TRADE_DATE=2026-07-10）
    const SNAP: &str = r#"{"result":{"pages":12,"data":[
        {"SECURITY_CODE":"000001","SECURITY_NAME_ABBR":"平安银行","TRADE_DATE":"2026-07-10 00:00:00","PE_TTM":4.709518,"PB_MRQ":0.43697322,"TOTAL_MARKET_CAP":202791845169.1},
        {"SECURITY_CODE":"000002","SECURITY_NAME_ABBR":"万科A","TRADE_DATE":"2026-07-10 00:00:00","PE_TTM":-0.42038851,"PB_MRQ":0.33568046,"TOTAL_MARKET_CAP":37104506454.81},
        {"SECURITY_CODE":"000004","SECURITY_NAME_ABBR":"国华退","TRADE_DATE":"2026-07-10 00:00:00","PE_TTM":5.16581984,"PB_MRQ":0.70136785,"TOTAL_MARKET_CAP":60894929.72},
        {"SECURITY_CODE":"600519","SECURITY_NAME_ABBR":"贵州茅台","TRADE_DATE":"2026-07-10 00:00:00","PE_TTM":18.21098231,"PB_MRQ":6.39,"TOTAL_MARKET_CAP":1506323327572.98}],"count":5530}}"#;

    /// clist 港股实测形状（fltt=2 → 浮点真值；"-" 占位）
    const HK: &str = r#"{"rc":0,"data":{"diff":[
        {"f9":22.5,"f12":"00700","f14":"腾讯控股","f20":4200000000000,"f23":4.1},
        {"f9":"-","f12":"08123","f14":"某停牌股","f20":"-","f23":"-"}]}}"#;

    #[test]
    fn parses_snapshot_and_total_count() {
        let (rows, count) = parse_snapshot_page(SNAP).unwrap();
        assert_eq!(count, 5530, "count 是全市场只数，用于校验分页完整性");
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn maps_pe_ttm_pb_mrq_and_infers_market_from_code() {
        let (rows, _) = parse_snapshot_page(SNAP).unwrap();
        let mt = rows.iter().find(|r| r.code == "600519").unwrap();
        assert_eq!(mt.name, "贵州茅台");
        assert_eq!(mt.market, 1, "6 开头 → 沪市");
        assert!((mt.pe_ttm.unwrap() - 18.21098231).abs() < 1e-9);
        assert!((mt.pb_mrq.unwrap() - 6.39).abs() < 1e-9);
        assert!((mt.market_cap.unwrap() - 1_506_323_327_572.98).abs() < 1.0);

        let pab = rows.iter().find(|r| r.code == "000001").unwrap();
        assert_eq!(pab.market, 0, "0 开头 → 深市");
    }

    #[test]
    fn negative_pe_is_real_data_not_missing() {
        let (rows, _) = parse_snapshot_page(SNAP).unwrap();
        let wk = rows.iter().find(|r| r.code == "000002").unwrap();
        // 万科 PE_TTM 为负 = 近12个月亏损。丢成 None 会让它在「PE 低者优先」的排序里凭空消失
        assert!((wk.pe_ttm.unwrap() - (-0.42038851)).abs() < 1e-9);
        assert!(wk.is_screenable(), "亏损股仍可入池，由因子阈值去挡");
    }

    #[test]
    fn delisted_shell_has_pretty_pe_but_must_be_excluded_by_name() {
        let (rows, _) = parse_snapshot_page(SNAP).unwrap();
        let gh = rows.iter().find(|r| r.code == "000004").unwrap();
        // 国华退 PE=5.17、PB=0.70，纯看财务指标像个便宜的好票 —— 只有名称能挡住
        assert!(gh.pe_ttm.unwrap() > 0.0 && gh.pe_ttm.unwrap() < 10.0);
        assert!(gh.is_risky_shell());
        assert!(!gh.is_screenable());
    }

    #[test]
    fn shell_detection_covers_st_pt_and_delisted() {
        let mk = |name: &str| Listing {
            market: 0, code: "000001".into(), name: name.into(),
            market_cap: Some(1e9), pe_ttm: Some(10.0), pb_mrq: Some(1.0),
        };
        assert!(mk("ST星源").is_risky_shell());
        assert!(mk("*ST海核").is_risky_shell());
        assert!(mk("国华退").is_risky_shell());
        assert!(mk("PT金田A").is_risky_shell());
        assert!(!mk("贵州茅台").is_risky_shell());
        assert!(!mk("万科A").is_risky_shell());
    }

    #[test]
    fn zero_market_cap_not_screenable() {
        let l = Listing {
            market: 0, code: "000003".into(), name: "某股".into(),
            market_cap: Some(0.0), pe_ttm: None, pb_mrq: None,
        };
        assert!(!l.is_screenable());
    }

    #[test]
    fn hk_clist_parses_both_values_and_dash_placeholder() {
        let rows = parse_hk_page(HK).unwrap();
        assert_eq!(rows.len(), 2);
        let tx = &rows[0];
        assert_eq!(tx.market, 116);
        assert_eq!(tx.name, "腾讯控股");
        // fltt=2 → 已是真值；若误除以 100，PE 会变成 0.225
        assert!((tx.pe_ttm.unwrap() - 22.5).abs() < 1e-9);
        assert_eq!(tx.secid().cache_key(), "116_00700", "须与K线缓存键一致");

        let sus = &rows[1];
        assert_eq!(sus.pe_ttm, None, "\"-\" → None");
        assert!(!sus.is_screenable(), "无市值 → 不可筛");
    }

    #[test]
    fn secid_roundtrip_matches_kline_cache_key() {
        let (rows, _) = parse_snapshot_page(SNAP).unwrap();
        let mt = rows.iter().find(|r| r.code == "600519").unwrap();
        // 筛出来的股票要能取到 K线，缓存键必须与 cache.rs 一致
        assert_eq!(mt.secid().cache_key(), "1_600519");
    }

    #[test]
    fn csv_roundtrip() {
        let rows = vec![Listing {
            market: 1, code: "600519".into(), name: "贵州茅台".into(),
            market_cap: Some(1.5e12), pe_ttm: Some(13.82), pb_mrq: None,
        }];
        let tmp = std::env::temp_dir().join("xlh_universe_test.csv");
        write_csv(&tmp, &rows).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back[0].name, "贵州茅台");
        assert_eq!(back[0].pb_mrq, None);
        assert!((back[0].pe_ttm.unwrap() - 13.82).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn name_map_fills_the_gap_project_had() {
        let (rows, _) = parse_snapshot_page(SNAP).unwrap();
        assert_eq!(name_map(&rows).get("600519").map(|s| s.as_str()), Some("贵州茅台"));
    }

    /// 实网测试：`cargo test -- --ignored`。接口漂移时能立刻发现 ——
    /// fixture 只证明 parser 自洽，证明不了东财没改字段或量纲。
    #[test]
    #[ignore]
    fn live_fetch_a_universe() {
        let date = latest_trade_date().expect("探测最新交易日");
        let rows = fetch_a_snapshot(date).expect("抓A股全集");
        assert!(rows.len() > 5000, "沪深A股应有5000+只，实得 {}", rows.len());

        let mt = rows.iter().find(|r| r.code == "600519").expect("茅台应在全集中");
        assert_eq!(mt.name, "贵州茅台");
        // 量纲哨兵：若接口改成 ×100 定点数，PE 会变成 1382，这里立刻炸
        let pe = mt.pe_ttm.expect("茅台应有PE");
        assert!((5.0..80.0).contains(&pe), "PE 量纲异常: {pe}");

        let ok = rows.iter().filter(|r| r.is_screenable()).count();
        assert!(ok > 3000 && ok < rows.len(), "可筛选 {ok} / 全集 {}", rows.len());
        println!("{date}: 全集 {} 只，可筛选 {ok} 只，茅台 PE_TTM={pe}", rows.len());
    }
}

//! 候选股主力资金流（东财 `ulist.np`）。
//!
//! # ⚠ 「主力资金」不是真实席位数据
//!
//! 东财按**单笔成交金额分档推算**：超大单(>100万)、大单(20–100万) 归「主力」，
//! 中单小单归「散户」。它**识别不了**机构 vs 游资 vs 拆单大户，**识别不了**
//! 对倒。它是代理指标，是线索，不是事实。所有对外出口都必须带这个声明 ——
//! 不能让人误以为「主力资金流入」等于「机构在买」。
//!
//! # 为什么只查候选股
//!
//! 东财批量行情端点会封禁（见 `realtime/mod.rs`）。全市场 5400 只走 clist
//! 需 27 页 × 20 时点 = 540 页/日，必封。而按设计，资金流只用于候选股的
//! 佐证/排序/背离标记 —— 给 5400 只全拉，99% 的数据永不会被读。
//!
//! 只查候选（几十只）= 1 请求/快照、20 请求/日，远低于封禁线。
//!
//! # 降级
//!
//! 本模块失败**不得**影响榜单产出。资金流是佐证，佐证拿不到不影响价量
//! 主判定成立。调用方应把资金流留空并照常出榜。
use std::collections::HashMap;
use anyhow::{anyhow, Result};
use serde::Deserialize;

/// 一只股票的资金流。
#[derive(Debug, Clone, PartialEq)]
pub struct Flow {
    pub code: String,
    pub name: String,
    /// 主力净流入额，元（东财 f62，当日累计）
    pub main_net: f64,
    /// 主力净流入占成交额比，**小数**（0.05 = 5%）。
    ///
    /// 东财 f184 给的是百分数（-1.32 表示 -1.32%），此处已 ÷100。
    pub main_net_pct: f64,
}

#[derive(Deserialize)]
struct Resp { data: Option<Data> }
#[derive(Deserialize)]
struct Data { #[serde(default)] diff: Option<Diff> }

/// `diff` 形状随参数而变：数组或 `{"0":{...}}` 的 map。两种都接住。
/// 同 `universe.rs:193` 的处理。
#[derive(Deserialize)]
#[serde(untagged)]
enum Diff { List(Vec<Row>), Map(HashMap<String, Row>) }

impl Diff {
    fn into_rows(self) -> Vec<Row> {
        match self {
            Diff::List(v) => v,
            Diff::Map(m) => m.into_values().collect(),
        }
    }
}

/// 东财对停牌/无数据的标的返回字符串 "-"，有值时返回数字。
/// 变体顺序有意义：先试 Num，落不进去的由 Other 兜住。同 `universe.rs:214`。
#[derive(Deserialize)]
#[serde(untagged)]
enum Cell { Num(f64), Other(serde::de::IgnoredAny) }

impl Cell {
    fn num(&self) -> Option<f64> {
        match self { Cell::Num(v) => Some(*v), Cell::Other(_) => None }
    }
}

#[derive(Deserialize)]
struct Row {
    #[serde(rename = "f12")] code: String,
    #[serde(rename = "f14")] name: String,
    #[serde(rename = "f62")] main_net: Option<Cell>,
    #[serde(rename = "f184")] main_net_pct: Option<Cell>,
}

/// 东财单次 secids 上限。保守取值：候选股通常只有几十只，用不到上限，
/// 而调大只会增加封禁风险 —— 那正是 clist 被弃用的原因。
pub const BATCH: usize = 200;

/// 解析 `ulist.np` 响应。
///
/// **必须在请求里带 `fltt=2`**，否则东财返回放大 100 倍的定点整数
/// （实测 600519 `f2=125899` 实为 1258.99、`f184=-132` 实为 -1.32%）。
/// `universe.rs:236-239` 对同类陷阱有明文警告。本函数无从分辨量纲，
/// 只能靠调用方带对参数 + 哨兵测试守住。
pub fn parse(body: &str) -> Result<Vec<Flow>> {
    let resp: Resp = serde_json::from_str(body).map_err(|e| anyhow!("解析资金流JSON失败: {e}"))?;
    let rows = resp.data.and_then(|d| d.diff).map(|d| d.into_rows()).unwrap_or_default();
    Ok(rows.into_iter().filter_map(|r| {
        // 资金流缺失的标的（停牌等）直接丢弃：调用方会把它当「资金流不可用」，
        // 那是正确的语义 —— 强行填 0 会伪造出「主力零流入」的假事实
        let main_net = r.main_net.as_ref().and_then(|c| c.num())?;
        let pct = r.main_net_pct.as_ref().and_then(|c| c.num())?;
        Some(Flow {
            code: r.code,
            name: r.name.trim().replace(['　', ' '], ""),
            main_net,
            // f184 是百分数（-1.32 = -1.32%），配置里的阈值是小数（0.05 = 5%）。
            // 忘了这一步，5% 的阈值会被当成 500%，背离标记永远不触发且完全静默。
            main_net_pct: pct / 100.0,
        })
    }).collect())
}

/// 抓候选股资金流。`secids` 形如 `["1.600519", "0.000001"]`。
///
/// # 为什么不复用 `universe::get`，也不重试
///
/// `universe::get` 会重试 4 次带指数退避。那对「必须拿到」的全市场清单是对的
/// （残缺清单是静默错误），但对资金流是**有害的**：
///
/// 1. 资金流失败**本来就允许降级** —— 佐证拿不到不影响价量主判定成立
/// 2. 实测东财封禁**持续 ≥600 秒**（观测窗口内未解除）。在封禁期间重试，
///    等于一次调用打 4 发，只会加深伤口。对一个可失败的功能，**快速失败**
///    比顽强重试正确。
///
/// 故这里单发不重试。IPv4 绑定仍保留（东财部分 IPv6 CDN 节点不可达），
/// 超时压到 8s —— 封禁时连接是直接被掐的，等 20s 没有意义。
fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))
}

pub fn fetch(secids: &[String]) -> Result<Vec<Flow>> {
    if secids.is_empty() { return Ok(Vec::new()) }
    let c = client()?;
    let mut all = Vec::with_capacity(secids.len());
    for (i, chunk) in secids.chunks(BATCH).enumerate() {
        if i > 0 { std::thread::sleep(std::time::Duration::from_millis(300)); }
        let url = format!(
            "https://push2.eastmoney.com/api/qt/ulist.np/get?fltt=2&secids={}&fields=f12,f14,f2,f3,f6,f62,f184",
            chunk.join(","));
        // 单发不重试 —— 见上方注释
        let body = c.get(&url)
            .header("Referer", "https://quote.eastmoney.com/")
            .header("User-Agent", "Mozilla/5.0")
            .send().and_then(|r| r.text())
            .map_err(|e| anyhow!("资金流请求失败（东财可能限流）: {e}"))?;
        all.extend(parse(&body)?);
    }
    Ok(all)
}

/// code → Flow 映射，便于按代码查。
pub fn by_code(flows: Vec<Flow>) -> HashMap<String, Flow> {
    flows.into_iter().map(|f| (f.code.clone(), f)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐字取自 ulist.np 实测响应（2026-07-16，**带 fltt=2**）。
    const RESP: &str = r#"{"rc":0,"rt":11,"svr":181669434,"lt":1,"full":1,"dlmkts":"","dsc":"0","data":{"total":3,"diff":[{"f2":1258.99,"f3":0.63,"f6":5987570858.0,"f12":"600519","f14":"贵州茅台","f62":-79273120.0,"f184":-1.32},{"f2":10.77,"f3":-0.65,"f6":864435895.0,"f12":"000001","f14":"平安银行","f62":-64265948.0,"f184":-7.43},{"f2":50.67,"f3":-0.55,"f6":4120400653.0,"f12":"601318","f14":"中国平安","f62":-225810048.0,"f184":-5.48}]}}"#;

    #[test]
    fn f184_percent_is_converted_to_fraction() {
        // 东财 f184=-1.32 表示 -1.32%，而配置阈值 main_flow_pct=0.05 是小数(5%)。
        // 忘了 ÷100 的话 5% 阈值会被当成 500%，背离标记永远不触发 —— 且完全静默。
        // 验算：f62 -79273120 元 ÷ f6 5987570858 元 = -1.324% ✓
        let f = parse(RESP).unwrap();
        let mt = f.iter().find(|f| f.code == "600519").unwrap();
        assert!((mt.main_net_pct - (-0.0132)).abs() < 1e-9,
            "f184 须 ÷100 转小数，实得 {}", mt.main_net_pct);
        assert!(mt.main_net_pct.abs() < 1.0, "小数口径下占比不可能超过 1.0（100%）");
    }

    #[test]
    fn main_net_stays_in_yuan() {
        let f = parse(RESP).unwrap();
        let mt = f.iter().find(|f| f.code == "600519").unwrap();
        assert!((mt.main_net - (-79_273_120.0)).abs() < 1.0, "净流入额是元，不做换算");
    }

    #[test]
    fn pct_is_consistent_with_net_over_amount() {
        // 交叉验证量纲：main_net / 成交额 应约等于 main_net_pct。
        // 这条是量纲哨兵 —— 若东财改了任一字段的量纲，这里会炸
        let f = parse(RESP).unwrap();
        let pa = f.iter().find(|f| f.code == "000001").unwrap();
        let amount = 864_435_895.0_f64;
        let derived = pa.main_net / amount;
        assert!((derived - pa.main_net_pct).abs() < 1e-3,
            "f62/f6={:.5} 应约等于 f184/100={:.5}", derived, pa.main_net_pct);
    }

    #[test]
    fn negative_flow_keeps_sign() {
        // 净流出是负数。丢了符号就分不出「主力在买」还是「主力在跑」，
        // 背离标记会彻底反向
        let f = parse(RESP).unwrap();
        assert!(f.iter().all(|f| f.main_net < 0.0), "样本三只当日均为净流出");
    }

    #[test]
    fn names_are_utf8_and_trimmed() {
        // 东财这里给 UTF-8 名称，正好补上腾讯（GBK，我们不取名）缺的那块
        let f = parse(RESP).unwrap();
        assert_eq!(f.iter().find(|f| f.code == "600519").unwrap().name, "贵州茅台");
    }

    #[test]
    fn dash_cells_are_dropped_not_zeroed() {
        // 东财对停牌标的返回 "-"。填 0 会伪造出「主力零流入」的假事实；
        // 丢弃则让调用方正确地视其为「资金流不可用」
        let body = r#"{"data":{"diff":[{"f12":"000003","f14":"PT金田A","f62":"-","f184":"-"}]}}"#;
        assert!(parse(body).unwrap().is_empty(), "\"-\" 须丢弃而非当 0");
    }

    #[test]
    fn map_shaped_diff_is_accepted() {
        // diff 形状随参数而变（数组 or map），两种都得接住
        let body = r#"{"data":{"diff":{"0":{"f12":"600519","f14":"贵州茅台","f62":100.0,"f184":2.0}}}}"#;
        let f = parse(body).unwrap();
        assert_eq!(f.len(), 1);
        assert!((f[0].main_net_pct - 0.02).abs() < 1e-9);
    }

    #[test]
    fn empty_and_malformed_are_handled() {
        assert!(parse(r#"{"data":null}"#).unwrap().is_empty());
        assert!(parse(r#"{"data":{"diff":[]}}"#).unwrap().is_empty());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn by_code_indexes_flows() {
        let m = by_code(parse(RESP).unwrap());
        assert!(m.contains_key("600519"));
        assert_eq!(m.len(), 3);
    }

    /// 实网哨兵：fixture 只证明 parser 自洽，证明不了东财没改字段或量纲。
    /// 尤其是 fltt=2 —— 若哪天它失效，f184 会变回 ×100 的定点整数，
    /// 而代码会静默地把 -132% 当 -1.32 处理。
    /// 手动跑：cargo test --lib realtime::flow -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_eastmoney_fltt2_still_returns_float_scale() {
        let f = fetch(&["1.600519".to_string(), "0.000001".to_string()]).unwrap();
        assert!(!f.is_empty(), "实网应返回数据（若为空可能已被封禁）");
        for x in &f {
            println!("{} {} 主力净流入={:.0}元 占比={:.2}%", x.code, x.name, x.main_net, x.main_net_pct * 100.0);
            // 量纲哨兵：小数口径下占比的绝对值不可能超过 1.0。
            // 若 fltt=2 失效返回定点整数，这里会得到 ±1.32 而非 ±0.0132 → 炸
            assert!(x.main_net_pct.abs() < 1.0,
                "{} 占比 {} 超出小数口径 —— fltt=2 可能已失效，东财改回了 ×100 定点整数",
                x.code, x.main_net_pct);
        }
    }
}

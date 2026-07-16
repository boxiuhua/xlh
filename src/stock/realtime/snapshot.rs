//! 腾讯全市场实时价量快照（`qt.gtimg.cn`）。
//!
//! # 为什么是腾讯而不是东财
//!
//! 东财批量行情端点会封禁（见模块头 `realtime/mod.rs`）。腾讯实测单请求可带
//! 800 个代码、无封禁，全市场 5400 只 → 7 请求/快照、140 请求/交易日。
//!
//! # 编码陷阱
//!
//! 腾讯返回 **GBK**（`Content-Type: text/html; charset=GBK`），而本项目的 reqwest
//! 是 `default-features = false` 未启用 `charset` feature，`.text()` 会按 UTF-8
//! 强解，中文名变成乱码。
//!
//! 解法不是加 `encoding_rs` 依赖，而是**根本不从腾讯取名称**：所有数值字段都是
//! 纯 ASCII，UTF-8 lossy 解码后完好无损；名称走既有的 `universe::name_map`
//! （datacenter 源，UTF-8）。少一个依赖，少一个 feature。
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;

/// 单只股票的一个时点快照。
///
/// 刻意不含资金流：资金流只对候选股取（见 `flow.rs`），全市场 99% 的股票
/// 抓回来也永远不会被读。
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub code: String,
    /// 行情时间戳（腾讯下标 30）。交易日自证的依据。
    pub ts: NaiveDateTime,
    /// 现价（下标 3）
    pub price: f64,
    /// 当日涨跌幅 %（下标 32）
    pub change_pct: f64,
    /// 成交量，手（下标 6）
    pub volume: f64,
    /// 成交额，元（下标 37 为万元，此处已 ×10000）
    pub amount: f64,
    /// 换手率 %（下标 38）
    pub turnover: f64,
    /// 量比（下标 49）
    pub vol_ratio: f64,
}

/// 腾讯单请求代码数上限。实测 800 只正常返回（168KB）。
pub const BATCH: usize = 800;

// 字段下标。实测自 600519 响应（2026-07-16）：
// 0=市场标志 1=名称(GBK) 2=代码 3=现价 4=昨收 5=今开 6=成交量(手)
// 7=外盘 8=内盘 9..28=买卖五档 29=最近逐笔 30=时间戳 31=涨跌 32=涨跌%
// 33=最高 34=最低 35=价/量/额 36=成交量 37=成交额(万) 38=换手率 39=市盈率
// 40=空 41=最高 42=最低 43=振幅 44=流通市值 45=总市值 46=市净率
// 47=涨停 48=跌停 49=量比 50=委差 51=均价
const I_CODE: usize = 2;
const I_PRICE: usize = 3;
const I_VOLUME: usize = 6;
const I_TS: usize = 30;
const I_CHANGE_PCT: usize = 32;
const I_AMOUNT_WAN: usize = 37;
const I_TURNOVER: usize = 38;
const I_VOL_RATIO: usize = 49;
/// 需要读到的最大下标 —— 短于此的行直接丢弃。
const MIN_FIELDS: usize = I_VOL_RATIO + 1;

/// A股 secid 市场号 → 腾讯前缀。仅沪深：本模块只做 A 股。
pub fn symbol(market: u16, code: &str) -> Option<String> {
    match market {
        1 => Some(format!("sh{code}")),
        0 => Some(format!("sz{code}")),
        _ => None,
    }
}

/// 解析腾讯响应文本。
///
/// 格式：每行 `v_sh600519="1~名称~600519~1258.99~...~";`，`~` 分隔。
///
/// 停牌股（成交量 0）**直接跳过**：它不可能有异动，且其时间戳停在 09:00:00
/// 不会随行情更新，留着只会污染交易日自证。
pub fn parse(body: &str) -> Vec<Tick> {
    let mut out = Vec::new();
    for line in body.split(';') {
        let Some(eq) = line.find('=') else { continue };
        let rest = &line[eq + 1..];
        let Some(start) = rest.find('"') else { continue };
        let Some(end) = rest.rfind('"') else { continue };
        if end <= start { continue }
        let fields: Vec<&str> = rest[start + 1..end].split('~').collect();
        if fields.len() < MIN_FIELDS { continue }

        let num = |i: usize| fields[i].trim().parse::<f64>().ok();
        let (Some(price), Some(volume)) = (num(I_PRICE), num(I_VOLUME)) else { continue };
        // 停牌：成交量为 0。PT金田A(000003) 实测即此形态，价 2.71、量 0、时间戳停在 09:00:00。
        if volume <= 0.0 { continue }
        let Some(ts) = NaiveDateTime::parse_from_str(fields[I_TS].trim(), "%Y%m%d%H%M%S").ok() else { continue };
        let code = fields[I_CODE].trim();
        if code.is_empty() { continue }

        out.push(Tick {
            code: code.to_string(),
            ts,
            price,
            change_pct: num(I_CHANGE_PCT).unwrap_or(0.0),
            volume,
            // 下标 37 单位是万元
            amount: num(I_AMOUNT_WAN).unwrap_or(0.0) * 10_000.0,
            turnover: num(I_TURNOVER).unwrap_or(0.0),
            vol_ratio: num(I_VOL_RATIO).unwrap_or(0.0),
        });
    }
    out
}

fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| anyhow!("构建HTTP客户端失败: {e}"))
}

/// 抓一批（≤BATCH 个符号）。
fn fetch_batch(c: &reqwest::blocking::Client, symbols: &[String]) -> Result<Vec<Tick>> {
    let url = format!("https://qt.gtimg.cn/q={}", symbols.join(","));
    let mut last_err = None;
    for attempt in 0..3u32 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300) * 2u32.pow(attempt - 1));
        }
        // 用 bytes() + lossy 而非 text()：腾讯是 GBK，text() 在未启用 charset feature 时
        // 按 UTF-8 强解。数值字段是 ASCII，lossy 后完好；名称我们本就不取。
        match c.get(&url).header("User-Agent", "Mozilla/5.0").send().and_then(|r| r.bytes()) {
            Ok(b) => return Ok(parse(&String::from_utf8_lossy(&b))),
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow!("腾讯快照抓取失败(重试3次): {}", last_err.unwrap()))
}

/// 抓全市场快照。symbols 形如 `["sh600519", "sz000001", ...]`，内部按 BATCH 分批。
///
/// 单批失败即整体失败：残缺的「全市场扫描」是静默错误 —— 少一半股票的异动榜
/// 看起来完全正常，但你不知道漏了什么。同 `universe::fetch_a_snapshot` 的取舍。
pub fn fetch(symbols: &[String]) -> Result<Vec<Tick>> {
    if symbols.is_empty() { return Ok(Vec::new()) }
    let c = client()?;
    let mut all = Vec::with_capacity(symbols.len());
    for (i, chunk) in symbols.chunks(BATCH).enumerate() {
        if i > 0 { std::thread::sleep(std::time::Duration::from_millis(200)); }
        all.extend(fetch_batch(&c, chunk)?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 逐字取自 qt.gtimg.cn 实测响应（2026-07-16 16:14，UTF-8 lossy 解码后的形态，
    /// 即代码实际收到的样子）。名称段是 GBK 乱码 —— 这正是我们不取名称的原因。
    /// 含三种形态：正常股(600519)、正常股(000001)、**停牌股(000003 PT金田A)**。
    const SNAP: &str = concat!(
        r#"v_sh600519="1~����ę́~600519~1258.99~1251.06~1252.00~47611~25633~21978~1258.97~1~1258.93~1~1258.92~5~1258.91~2~1258.90~29~1258.99~2~1259.00~39~1259.03~2~1259.06~1~1259.08~3~~20260716161440~7.93~0.63~1267.97~1245.05~1258.99/47611/5987570858~47611~598757~0.38~19.03~~1267.97~1245.05~1.83~15738.40~15738.40~6.76~1376.17~1125.95~0.98~-9~1257.60~14.44~19.12~~~0.29~598757.0858~226.6182~18~   A~GP-A~-6.68~6.50~4.13~30.53~26.78~1539.98~1151.01~4.65~3.88~-8.72~1250081601~1250081601~-10.59~-10.45~1250081601~~~-7.36~0.02~~CNY~0~___D__F__N~1259.15~-21~";"#,
        "\n",
        r#"v_sz000001="51~ƽ������~000001~10.77~10.84~10.85~800766~412647~388119~10.76~711~10.75~739~10.74~5151~10.73~3236~10.72~5132~10.77~1305~10.78~3406~10.79~1754~10.80~2289~10.81~3366~~20260716161418~-0.07~-0.65~10.93~10.72~10.77/800766/864435895~800766~86444~0.41~4.85~~10.93~10.72~1.94~2089.98~2090.02~0.46~11.92~9.76~0.83~2849~10.80~3.60~4.90~~~0.32~86443.5895~40.3875~375~   A~GP-A~-2.53~2.67~5.53~7.91~0.71~12.15~9.99~4.77~-0.09~1.13~19405600653~19405918198~10.52~-2.62~19405600653~~~-10.58~0.09~~CNY~0~~10.70~7453~";"#,
        "\n",
        r#"v_sz000003="51~PT����A~000003~2.71~2.71~0.00~0~0~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~0.00~0~~20260716090000~0.00~0.00~0.00~0.00~2.71/0/0~0~0~0.00~78.43~D~0.00~0.00~0.00~5.20~9.04~-0.27~-1~-1~0.00~0~0.00~-51.21~78.43~~~~0.0000~0.0000~0~ ~GP-A~0.00~0.00~0.00~-1.15~0.95~~~0.00~0.00~0.00~191808910~333433584~~0.00~191808910~~~0.00~0.00~~CNY~0~~0.00~0~";"#,
    );

    #[test]
    fn symbol_maps_only_a_share_markets() {
        assert_eq!(symbol(1, "600519").unwrap(), "sh600519");
        assert_eq!(symbol(0, "000001").unwrap(), "sz000001");
        assert!(symbol(116, "00700").is_none(), "本模块只做 A 股，港股应返回 None");
        assert!(symbol(105, "AAPL").is_none(), "美股应返回 None");
    }

    #[test]
    fn parses_price_volume_and_timestamp_at_correct_indices() {
        let ticks = parse(SNAP);
        let t = ticks.iter().find(|t| t.code == "600519").expect("应解析出 600519");
        assert!((t.price - 1258.99).abs() < 1e-9, "现价在下标 3");
        assert!((t.volume - 47611.0).abs() < 1e-9, "成交量(手)在下标 6");
        assert!((t.change_pct - 0.63).abs() < 1e-9, "涨跌幅在下标 32");
        assert!((t.turnover - 0.38).abs() < 1e-9, "换手率在下标 38");
        assert!((t.vol_ratio - 0.98).abs() < 1e-9, "量比在下标 49");
        assert_eq!(t.ts.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-07-16 16:14:40",
            "时间戳在下标 30，格式 YYYYMMDDHHMMSS");
    }

    #[test]
    fn amount_is_converted_from_wan_to_yuan() {
        // 下标 37 单位是万元(598757)，实际成交额约 59.88 亿。若忘了 ×10000，
        // 资金流占比会算大 10000 倍，「大量流入」阈值形同虚设
        let t = parse(SNAP).into_iter().find(|t| t.code == "600519").unwrap();
        assert!((t.amount - 5_987_570_000.0).abs() < 1.0, "598757 万元 → 元，实得 {}", t.amount);
    }

    #[test]
    fn suspended_stock_is_skipped_not_parsed_as_flat() {
        // PT金田A(000003)：价 2.71、量 0、时间戳停在 09:00:00。
        // 若不跳过，它会以「涨跌幅 0」进榜污染数据，且其陈旧时间戳会干扰交易日自证
        let ticks = parse(SNAP);
        assert!(!ticks.iter().any(|t| t.code == "000003"), "停牌股(量=0)必须跳过");
        assert_eq!(ticks.len(), 2, "3 行输入，停牌 1 只，应得 2 条");
    }

    #[test]
    fn negative_change_pct_is_preserved() {
        // 000001 涨跌幅 -0.65 —— 下跌也是异动，符号不能丢
        let t = parse(SNAP).into_iter().find(|t| t.code == "000001").unwrap();
        assert!((t.change_pct - (-0.65)).abs() < 1e-9, "跌幅须保留负号");
    }

    #[test]
    fn gbk_name_mojibake_does_not_break_numeric_parsing() {
        // 名称段是 GBK 被 UTF-8 lossy 解码后的乱码。这正是真实收到的形态，
        // 数值字段必须照常解析 —— 这是「不取名称」这个决策的护栏
        let ticks = parse(SNAP);
        assert_eq!(ticks.len(), 2, "乱码名称不应影响解析");
        assert!(ticks.iter().all(|t| t.price > 0.0));
    }

    #[test]
    fn malformed_lines_are_skipped_silently() {
        assert!(parse("").is_empty());
        assert!(parse("garbage").is_empty());
        assert!(parse(r#"v_sh600519="";"#).is_empty(), "空引号行应跳过");
        assert!(parse(r#"v_sh600519="1~n~600519~1.0";"#).is_empty(), "字段不足应跳过");
    }

    #[test]
    fn batch_size_matches_measured_limit() {
        // 实测 800 只/请求正常返回。全市场 5400 只 → 7 请求。
        // 调大有封禁风险（东财就是这么被封的），调小则请求数线性上升
        assert_eq!(BATCH, 800);
    }

    /// 实网哨兵：fixture 只能证明 parser 自洽，证明不了腾讯没改字段顺序或量纲。
    /// 手动跑：cargo test --lib realtime::snapshot -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_tencent_field_layout_unchanged() {
        let ticks = fetch(&["sh600519".to_string(), "sz000001".to_string()]).unwrap();
        assert!(!ticks.is_empty(), "实网应返回数据");
        let t = &ticks[0];
        println!("{} 价={} 量={}手 额={:.0}元 换手={}% 量比={} 时间={}",
            t.code, t.price, t.volume, t.amount, t.turnover, t.vol_ratio, t.ts);
        // 量纲哨兵：茅台价在 100~10000 之间。若腾讯改成分为单位，这里会炸
        assert!(t.price > 100.0 && t.price < 10000.0, "600519 价格量纲异常: {}", t.price);
        assert!(t.amount > 1e6, "成交额应为元量级(已×10000): {}", t.amount);
    }
}

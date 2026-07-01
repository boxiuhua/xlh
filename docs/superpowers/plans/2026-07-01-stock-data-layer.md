# 股票数据层 Implementation Plan（子项目 1）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 xlh 新增独立的股票行情数据管道（东财日线，A股+港股+美股，后复权），产出可直接喂给现有回测引擎的 `StockData` 适配器。

**Architecture:** 股票代码全部收敛在顶层 `src/stock/` 模块树，与基金业务代码互不 `use`；仅共用资产无关的通用件（`event::MarketEvent`、`data::DataHandler`）。股票以 `StockData`(impl DataHandler，`close→nav`、`adj_close→adj_nav`) 接入现有引擎。数据源为东方财富 K 线接口（后复权 `fqt=2`）+ suggest 搜索接口（解析美股 secid、跨市场搜索）。

**Tech Stack:** Rust、reqwest(blocking)、serde/serde_json、anyhow、chrono。全部依赖现有 Cargo.toml 已具备，**无需改 Cargo.toml**。

## Global Constraints

- 市场范围：A股、港股、美股；周期：**仅日线**（东财 `klt=101`）。
- 数据源：东方财富，免费无 key；请求头带 `Referer: https://www.eastmoney.com/`、`User-Agent: Mozilla/5.0`。
- 复权：**后复权**（K 线 `fqt=2`）；`StockBar.close`=不复权收盘价（`fqt=0`，显示用），`StockBar.adj_close`=后复权收盘价（计算用）。
- 隔离：`src/stock/**` **禁止 `use` 任何基金专属模块**（`crate::data::{eastmoney,cache,fundlist,sync}`、`crate::analyze`、`crate::recommend` 等）；只允许 `use crate::event::MarketEvent` 和 `use crate::data::DataHandler`。
- 测试：除显式 `#[ignore]` 的联网冒烟外，全部为**离线单测**，`cargo test` 必须全绿。
- 提交：每个 commit 信息结尾追加一行 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`。
- 缓存文件名用 `Secid::cache_key()`（形如 `1_600519.csv`、`116_00700.csv`、`105_AAPL.csv`），避免三市场重码。

---

## File Structure

```
src/stock/
├── mod.rs          —— pub mod data;（后续子项目在此续加 strategy/analyze 等）
└── data/
    ├── mod.rs      —— StockBar 模型 + StockData 适配器(impl DataHandler) + resolve_secid 组合入口
    ├── secid.rs    —— Secid 类型 + resolve_offline（A股/港股离线映射，美股标记 NeedSearch）
    ├── kline.rs    —— 东财 K 线抓取 + 解析 + 复权 merge
    ├── search.rs   —— 东财 suggest 搜索 + 美股 secid 解析
    ├── cache.rs    —— CSV 读写 / covers / load_or_fetch
    └── sync.rs     —— 增量同步
```
`src/lib.rs`：新增一行 `pub mod stock;`。

---

## Task 1: 脚手架 + StockBar + StockData 适配器

**Files:**
- Create: `src/stock/mod.rs`
- Create: `src/stock/data/mod.rs`
- Modify: `src/lib.rs`（新增 `pub mod stock;`）

**Interfaces:**
- Consumes: `crate::event::MarketEvent`、`crate::data::DataHandler`
- Produces:
  - `struct StockBar { date: NaiveDate, open: f64, high: f64, low: f64, close: f64, volume: f64, adj_close: f64 }`（Debug, Clone, Copy, PartialEq）
  - `struct StockData` + `StockData::new(bars: Vec<StockBar>) -> StockData`，`impl DataHandler for StockData`

- [ ] **Step 1: 注册模块**

`src/lib.rs` 在 `pub mod data;` 之后新增一行：
```rust
pub mod stock;
```
创建 `src/stock/mod.rs`：
```rust
pub mod data;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/mod.rs`，先只放测试与类型签名骨架：
```rust
use chrono::NaiveDate;
use crate::event::MarketEvent;
use crate::data::DataHandler;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StockBar {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub adj_close: f64,
}

pub struct StockData { bars: Vec<MarketEvent>, cursor: usize }

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, close: f64, adj: f64) -> StockBar {
        StockBar { date: dt, open: close, high: close, low: close, close, volume: 0.0, adj_close: adj }
    }

    #[test]
    fn maps_close_and_adj_into_market_event() {
        let bars = vec![bar(d(2024,1,2), 100.0, 200.0)];
        let mut h = StockData::new(bars);
        let ev = h.next_bar().unwrap();
        assert_eq!(ev.date, d(2024,1,2));
        assert!((ev.nav - 100.0).abs() < 1e-9, "nav 应为不复权 close");
        assert!((ev.adj_nav - 200.0).abs() < 1e-9, "adj_nav 应为后复权 adj_close");
        assert!(h.next_bar().is_none());
    }

    #[test]
    fn history_never_returns_future() {
        let bars = vec![bar(d(2024,1,1),1.0,1.0), bar(d(2024,1,2),1.1,1.1), bar(d(2024,1,3),1.2,1.2)];
        let mut h = StockData::new(bars);
        h.next_bar().unwrap();
        assert_eq!(h.history(10).len(), 1);
        h.next_bar();
        assert_eq!(h.history(10).len(), 2);
        assert_eq!(h.history(1).len(), 1);
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::mod`
Expected: 编译失败 —— `StockData::new` 与 `DataHandler` impl 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/mod.rs` 的 `pub struct StockData` 之后加入：
```rust
impl StockData {
    pub fn new(bars: Vec<StockBar>) -> Self {
        let bars = bars.into_iter()
            .map(|b| MarketEvent { date: b.date, nav: b.close, adj_nav: b.adj_close })
            .collect();
        Self { bars, cursor: 0 }
    }
}

impl DataHandler for StockData {
    fn next_bar(&mut self) -> Option<MarketEvent> {
        if self.cursor < self.bars.len() {
            let b = self.bars[self.cursor].clone();
            self.cursor += 1;
            Some(b)
        } else { None }
    }
    fn history(&self, lookback: usize) -> &[MarketEvent] {
        let end = self.cursor;
        let start = end.saturating_sub(lookback);
        &self.bars[start..end]
    }
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::`
Expected: 2 个测试 PASS，`cargo build` 通过。

- [ ] **Step 6: 提交**

```bash
git add src/lib.rs src/stock/mod.rs src/stock/data/mod.rs
git commit -m "feat(stock): 股票数据层脚手架 + StockBar + StockData 引擎适配器"
```

---

## Task 2: secid.rs —— 三市场代码映射

**Files:**
- Create: `src/stock/data/secid.rs`
- Modify: `src/stock/data/mod.rs`（新增 `pub mod secid;`）

**Interfaces:**
- Produces:
  - `struct Secid { market: u16, code: String }`（Debug, Clone, PartialEq, Serialize, Deserialize）
  - `Secid::param(&self) -> String`（`"1.600519"`）
  - `Secid::cache_key(&self) -> String`（`"1_600519"`）
  - `Secid::from_cache_key(&str) -> Option<Secid>`
  - `enum Resolved { Ready(Secid), NeedSearch(String) }`
  - `resolve_offline(input: &str) -> anyhow::Result<Resolved>`

- [ ] **Step 1: 声明子模块**

`src/stock/data/mod.rs` 顶部（`use` 之后）新增：
```rust
pub mod secid;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/secid.rs`：
```rust
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::secid`
Expected: 编译失败 —— `resolve_offline` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/secid.rs` 的 `enum Resolved` 之后加入：
```rust
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

    // 含字母且非已知前缀 → 视为美股 ticker
    Ok(Resolved::NeedSearch(s.to_ascii_uppercase()))
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::data::secid`
Expected: 6 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/data/secid.rs src/stock/data/mod.rs
git commit -m "feat(stock): secid 三市场映射（A股/港股离线 + 美股待搜索）"
```

---

## Task 3: kline.rs —— K 线抓取 + 解析 + 复权 merge

**Files:**
- Create: `src/stock/data/kline.rs`
- Modify: `src/stock/data/mod.rs`（新增 `pub mod kline;`）

**Interfaces:**
- Consumes: `super::StockBar`、`super::secid::Secid`
- Produces:
  - `struct Row { date: NaiveDate, open: f64, high: f64, low: f64, close: f64, volume: f64 }`
  - `parse_one(body: &str) -> anyhow::Result<Vec<Row>>`
  - `merge(raw: Vec<Row>, adj: Vec<Row>) -> Vec<StockBar>`
  - `fetch(secid: &Secid) -> anyhow::Result<Vec<StockBar>>`

- [ ] **Step 1: 声明子模块**

`src/stock/data/mod.rs` 新增：
```rust
pub mod kline;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/kline.rs`：
```rust
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::kline`
Expected: 编译失败 —— `parse_one`/`merge` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/kline.rs` 的 `struct KlineData` 之后加入：
```rust
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::data::kline`
Expected: 4 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/data/kline.rs src/stock/data/mod.rs
git commit -m "feat(stock): 东财K线抓取+解析+后复权merge"
```

---

## Task 4: search.rs —— suggest 搜索 + 美股 secid 解析 + resolve_secid 入口

**Files:**
- Create: `src/stock/data/search.rs`
- Modify: `src/stock/data/mod.rs`（新增 `pub mod search;` + `resolve_secid` 组合入口）

**Interfaces:**
- Consumes: `super::secid::{Secid, Resolved, resolve_offline}`
- Produces:
  - `struct StockInfo { code: String, name: String, secid: Secid, market_name: String }`（Debug, Clone, PartialEq, Serialize）
  - `parse_suggest(body: &str) -> anyhow::Result<Vec<StockInfo>>`
  - `pick_us(items: &[StockInfo], ticker: &str) -> Option<Secid>`
  - `search(query: &str) -> anyhow::Result<Vec<StockInfo>>`
  - `resolve_us(ticker: &str) -> anyhow::Result<Secid>`
  - （mod.rs）`resolve_secid(input: &str) -> anyhow::Result<Secid>`

- [ ] **Step 1: 声明子模块**

`src/stock/data/mod.rs` 新增：
```rust
pub mod search;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/search.rs`：
```rust
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
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::search`
Expected: 编译失败 —— `parse_suggest`/`pick_us` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/search.rs` 的 `const US_MARKETS` 之后加入：
```rust
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

/// 极简 URL 编码：仅转义空格与非 ASCII（东财 suggest 对中文也接受 UTF-8 百分号编码）。
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::data::search`
Expected: 4 个测试 PASS。

- [ ] **Step 6: 加 resolve_secid 组合入口**

`src/stock/data/mod.rs` 在模块声明之后（`impl StockData` 之前）加入：
```rust
use anyhow::Result;

/// 组合入口：A股/港股离线解析；美股经 suggest 搜索解析 secid。
pub fn resolve_secid(input: &str) -> Result<secid::Secid> {
    match secid::resolve_offline(input)? {
        secid::Resolved::Ready(s) => Ok(s),
        secid::Resolved::NeedSearch(t) => search::resolve_us(&t),
    }
}
```

- [ ] **Step 7: 运行确认编译通过**

Run: `cargo test --lib stock::`
Expected: 全部 PASS，`cargo build` 通过。

- [ ] **Step 8: 提交**

```bash
git add src/stock/data/search.rs src/stock/data/mod.rs
git commit -m "feat(stock): suggest搜索+美股secid解析+resolve_secid入口"
```

---

## Task 5: cache.rs —— CSV 缓存 + load_or_fetch

**Files:**
- Create: `src/stock/data/cache.rs`
- Modify: `src/stock/data/mod.rs`（新增 `pub mod cache;`）

**Interfaces:**
- Consumes: `super::StockBar`、`super::resolve_secid`、`super::kline::fetch`
- Produces:
  - `write_csv(path: &Path, bars: &[StockBar]) -> anyhow::Result<()>`
  - `read_csv(path: &Path) -> anyhow::Result<Vec<StockBar>>`
  - `covers(bars: &[StockBar], start: NaiveDate, end: NaiveDate) -> bool`
  - `load_or_fetch(input: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> anyhow::Result<Vec<StockBar>>`

- [ ] **Step 1: 声明子模块**

`src/stock/data/mod.rs` 新增：
```rust
pub mod cache;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/cache.rs`：
```rust
use std::path::Path;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use super::StockBar;

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, close: f64) -> StockBar {
        StockBar { date: dt, open: close, high: close, low: close, close, volume: 100.0, adj_close: close * 2.0 }
    }

    #[test]
    fn csv_roundtrip_preserves_ohlcv_and_adj() {
        let bars = vec![bar(d(2024,1,2), 110.0), bar(d(2024,1,3), 121.0)];
        let tmp = std::env::temp_dir().join("xlh_stock_cache_test.csv");
        write_csv(&tmp, &bars).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back.len(), 2);
        assert!((back[1].close - 121.0).abs() < 1e-9);
        assert!((back[1].adj_close - 242.0).abs() < 1e-9);
        assert!((back[0].volume - 100.0).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn covers_window() {
        let bars = vec![bar(d(2024,1,1), 1.0), bar(d(2024,6,1), 1.2)];
        assert!(covers(&bars, d(2024,1,1), d(2024,6,1)));
        assert!(covers(&bars, d(2024,2,1), d(2024,5,1)));
        assert!(!covers(&bars, d(2023,12,1), d(2024,6,1)));
        assert!(!covers(&bars, d(2024,1,1), d(2024,12,31)));
        assert!(!covers(&[], d(2024,1,1), d(2024,6,1)));
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::cache`
Expected: 编译失败 —— `write_csv`/`read_csv`/`covers` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/cache.rs` 的 `use` 之后加入：
```rust
pub fn write_csv(path: &Path, bars: &[StockBar]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from("date,open,high,low,close,volume,adj_close\n");
    for b in bars {
        s.push_str(&format!("{},{},{},{},{},{},{}\n", b.date, b.open, b.high, b.low, b.close, b.volume, b.adj_close));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<StockBar>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 7 { continue; }
        out.push(StockBar {
            date: NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?,
            open: c[1].parse()?, high: c[2].parse()?, low: c[3].parse()?,
            close: c[4].parse()?, volume: c[5].parse()?, adj_close: c[6].parse()?,
        });
    }
    Ok(out)
}

pub fn covers(bars: &[StockBar], start: NaiveDate, end: NaiveDate) -> bool {
    bars.iter().any(|b| b.date <= start) && bars.iter().any(|b| b.date >= end)
}

/// 有缓存且覆盖窗口则用缓存，否则抓取并写盘；最后按 [start,end] 过滤排序。
pub fn load_or_fetch(input: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> Result<Vec<StockBar>> {
    let secid = super::resolve_secid(input)?;
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    let mut bars = if path.exists() {
        let cached = read_csv(&path)?;
        if covers(&cached, start, end) {
            cached
        } else {
            let fresh = super::kline::fetch(&secid)?;
            write_csv(&path, &fresh)?;
            fresh
        }
    } else {
        let fresh = super::kline::fetch(&secid)?;
        write_csv(&path, &fresh)?;
        fresh
    };
    bars.retain(|b| b.date >= start && b.date <= end);
    bars.sort_by_key(|b| b.date);
    if bars.is_empty() { return Err(anyhow!("股票 {input} 在 {start}~{end} 无数据")); }
    Ok(bars)
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::data::cache`
Expected: 2 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/data/cache.rs src/stock/data/mod.rs
git commit -m "feat(stock): CSV缓存 + load_or_fetch"
```

---

## Task 6: sync.rs —— 增量同步

**Files:**
- Create: `src/stock/data/sync.rs`
- Modify: `src/stock/data/mod.rs`（新增 `pub mod sync;`）

**Interfaces:**
- Consumes: `super::StockBar`、`super::secid::Secid`、`super::resolve_secid`、`super::kline::fetch`、`super::cache::{read_csv, write_csv}`
- Produces:
  - `merge_incremental(cached: &[StockBar], fresh: Vec<StockBar>) -> (Vec<StockBar>, usize)`
  - `struct SyncOutcome { code: String, added: usize, total: usize, latest: Option<String>, error: Option<String> }`（Debug, Serialize）
  - `sync_stock(input: &str, cache_dir: &Path) -> SyncOutcome`
  - `sync_all(cache_dir: &Path) -> Vec<SyncOutcome>`

- [ ] **Step 1: 声明子模块**

`src/stock/data/mod.rs` 新增：
```rust
pub mod sync;
```

- [ ] **Step 2: 写失败测试**

创建 `src/stock/data/sync.rs`：
```rust
use std::path::Path;
use serde::Serialize;
use super::StockBar;
use super::secid::Secid;

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub code: String,
    pub added: usize,
    pub total: usize,
    pub latest: Option<String>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, c: f64) -> StockBar {
        StockBar { date: dt, open: c, high: c, low: c, close: c, volume: 0.0, adj_close: c }
    }

    #[test]
    fn appends_only_newer_points() {
        let cached = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.1)];
        let fresh = vec![bar(d(2024,1,15),1.05), bar(d(2024,2,1),1.1), bar(d(2024,3,1),1.2)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.last().unwrap().date, d(2024,3,1));
    }

    #[test]
    fn empty_cache_takes_all() {
        let (merged, added) = merge_incremental(&[], vec![bar(d(2024,1,1),1.0)]);
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 1);
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib stock::data::sync`
Expected: 编译失败 —— `merge_incremental` 未定义。

- [ ] **Step 4: 写最小实现**

在 `src/stock/data/sync.rs` 的 `struct SyncOutcome` 之后加入：
```rust
pub fn merge_incremental(cached: &[StockBar], fresh: Vec<StockBar>) -> (Vec<StockBar>, usize) {
    let mut fresh = fresh;
    fresh.sort_by_key(|b| b.date);
    let last = cached.last().map(|b| b.date);
    let new: Vec<StockBar> = fresh.into_iter()
        .filter(|b| last.is_none_or(|d| b.date > d))
        .collect();
    let mut merged = cached.to_vec();
    merged.extend(new.iter().copied());
    (merged, new.len())
}

fn sync_one(display: &str, secid: &Secid, cache_dir: &Path) -> SyncOutcome {
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    let cached = if path.exists() { super::cache::read_csv(&path).unwrap_or_default() } else { Vec::new() };
    let fresh = match super::kline::fetch(secid) {
        Ok(f) => f,
        Err(e) => return SyncOutcome {
            code: display.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|b| b.date.to_string()), error: Some(format!("抓取失败: {e}")),
        },
    };
    let (merged, added) = merge_incremental(&cached, fresh);
    if let Err(e) = super::cache::write_csv(&path, &merged) {
        return SyncOutcome {
            code: display.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|b| b.date.to_string()), error: Some(format!("写入失败: {e}")),
        };
    }
    let latest = merged.last().map(|b| b.date.to_string());
    SyncOutcome { code: display.to_string(), added, total: merged.len(), latest, error: None }
}

/// 同步单只：解析代码 → 增量抓取合并写回。
pub fn sync_stock(input: &str, cache_dir: &Path) -> SyncOutcome {
    match super::resolve_secid(input) {
        Ok(secid) => sync_one(input, &secid, cache_dir),
        Err(e) => SyncOutcome { code: input.to_string(), added: 0, total: 0, latest: None, error: Some(format!("代码解析失败: {e}")) },
    }
}

/// 同步全部：扫 cache_dir 下 *.csv（文件名为 cache_key）逐个同步。
pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome> {
    let mut keys: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("csv") {
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    keys.push(stem.to_string());
                }
            }
        }
    }
    keys.sort();
    keys.iter().filter_map(|k| Secid::from_cache_key(k).map(|s| sync_one(k, &s, cache_dir))).collect()
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib stock::data::sync`
Expected: 2 个测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/stock/data/sync.rs src/stock/data/mod.rs
git commit -m "feat(stock): 增量同步 sync_stock/sync_all"
```

---

## Task 7: 联网冒烟测试（默认 #[ignore]）+ 全量验证

**Files:**
- Create: `tests/stock_smoke.rs`

**Interfaces:**
- Consumes: `xlh::stock::data::{cache, resolve_secid}`

- [ ] **Step 1: 写冒烟测试（默认不跑）**

创建 `tests/stock_smoke.rs`：
```rust
//! 联网冒烟：验证东财三市场连通性。默认 #[ignore]，手动跑：
//!   cargo test --test stock_smoke -- --ignored --nocapture
use chrono::NaiveDate;
use xlh::stock::data::{cache, resolve_secid};

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

#[test]
#[ignore]
fn a_share_live() {
    let tmp = std::env::temp_dir().join("xlh_smoke_a");
    let bars = cache::load_or_fetch("600519", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "A股应有数据");
    println!("A股 600519: {} 条, 末日 {}", bars.len(), bars.last().unwrap().date);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore]
fn hk_live() {
    let tmp = std::env::temp_dir().join("xlh_smoke_hk");
    let bars = cache::load_or_fetch("00700", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "港股应有数据");
    println!("港股 00700: {} 条", bars.len());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore]
fn us_live() {
    // 关键风险验证：美股 secid 解析 + K线抓取
    let secid = resolve_secid("AAPL").unwrap();
    println!("美股 AAPL 解析为 secid {}", secid.param());
    let tmp = std::env::temp_dir().join("xlh_smoke_us");
    let bars = cache::load_or_fetch("AAPL", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "美股应有数据");
    println!("美股 AAPL: {} 条", bars.len());
    let _ = std::fs::remove_dir_all(&tmp);
}
```

- [ ] **Step 2: 确认离线测试全绿 + 冒烟默认跳过**

Run: `cargo test`
Expected: 所有 `stock::` 离线单测 + 既有基金测试全 PASS；`stock_smoke` 3 个显示 `ignored`。

- [ ] **Step 3: 人工跑联网冒烟，验证三市场（关键：美股）**

Run: `cargo test --test stock_smoke -- --ignored --nocapture`
Expected: A股、港股 PASS 并打印条数。**美股若失败**：记录报错，按 spec 降级路径处理（改用新浪源，或将范围缩到 A+港并在 spec/plan 标注），不阻塞其余部分。

- [ ] **Step 4: 提交**

```bash
git add tests/stock_smoke.rs
git commit -m "test(stock): 三市场联网冒烟（默认ignore，含美股连通性验证）"
```

---

## Self-Review

**1. Spec 覆盖：**
- 模块结构 `src/stock/data/{mod,secid,kline,cache,search,sync}` → Task 1-6 ✅
- StockBar 完整 OHLCV + adj_close → Task 1 ✅
- StockData 适配器接引擎 → Task 1 ✅
- secid 三市场映射（A/港离线、美股搜索）→ Task 2 + Task 4 ✅
- 后复权 fqt=2、close/adj_close 分离 → Task 3 ✅
- 缓存 load_or_fetch/covers、cache_key 防重码 → Task 5 ✅
- search 一箭双雕（美股解析 + 后续自动补全后端）→ Task 4 ✅
- 增量同步 → Task 6 ✅
- 美股连通性作首要验证、失败降级 → Task 7 ✅
- 隔离约束（不 use 基金模块）→ Global Constraints + 各任务仅 use event/DataHandler ✅
- 测试策略（离线单测 + #[ignore] 冒烟）→ 各任务 + Task 7 ✅

**2. 占位符扫描：** 无 TBD/TODO；每个 code step 均含完整代码。✅

**3. 类型一致性：** `Secid{market:u16,code:String}`、`resolve_offline`/`resolve_secid`/`resolve_us`、`parse_one`/`merge`/`fetch`、`parse_suggest`/`pick_us`、`load_or_fetch`、`merge_incremental`、`SyncOutcome` 在各任务签名一致；`Secid::cache_key`/`from_cache_key` 在 Task 2 定义、Task 5/6 使用一致。✅

**说明：** `resolve_offline` 中 `strip_prefix("us")` 需放在纯数字判断之前——已如此排序；A股 `sz`/`sh` 前缀在数字判断前处理，避免与 6 位数字冲突。

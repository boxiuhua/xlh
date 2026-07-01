# 股票数据层设计（子项目 1）

> 全局背景：为现有 Rust 基金分析/推荐工具（单 crate `xlh`）新增完整股票体系（A股+港股+美股）。
> 整体拆为 5 个子项目：**①数据层** → ②单股回测 → ③技术分析/诊断 → ④选股/推荐 → ⑤Web层。
> 核心原则：**股票与基金组织上完全分开（各自顶层模块树，业务代码互不引用），仅共用资产无关的通用引擎**；股票以适配器接入引擎，不重写、不改动能跑的基金代码。
> 本文档只覆盖**子项目 1：股票数据层**。

## 目标与范围

提供股票的行情数据管道，产出能直接喂给现有回测引擎的数据源，并为后续技术分析子项目保留完整 OHLCV。

- 市场：A股、港股、美股。
- 周期：仅日线。
- 数据源：东方财富（与现有基金同源，免费、无需 key）。
- 复权：**后复权**（回测/收益计算正确、历史值不随分红回填）。

### 明确不做（YAGNI，留给后续子项目）

分钟线/实时行情、财务基本面、期货/期权、全市场清单文件、任何回测/策略/技术指标/UI。

## 核心架构决策

### 接入方式：独立股票类型 + 适配器（方案 A）

现有引擎消费 `MarketEvent { date, nav, adj_nav }`，经 `DataHandler` trait 驱动。股票引入一等类型 `StockBar`（含完整 OHLCV），再用一个薄适配器 `StockData` 实现 `DataHandler`，把 `close→nav`、`adj_close→adj_nav` 映射进去。

- 引擎/策略/broker **零改动**即可回测个股（子项目 2 直接复用）。
- 保留 OHLCV/成交量，子项目 3 的技术指标（MACD/布林/ATR/量能）有数据可用。
- 不触碰现有基金代码，无回归风险。

被否决的备选：
- **B. 直接复用 `NavPoint`**：丢失 high/low/volume，阻塞子项目 3；`acc_nav` 对股票语义无意义。
- **C. 抽象泛型 `PriceBar`**：需重构能跑的基金代码，属无关重构，有回归风险。

### 模块边界：股票/基金分开，仅共享通用件

股票与基金**业务代码互不 `use`**，各自独立顶层模块树；两者只通过下列**资产无关的通用件**相接：

- **共用（保持顶层，无需移动）**：`event`（MarketEvent/Signal/Order/Fill）、`engine`、`broker`、`portfolio`、`metrics`、`strategy` 的 `Strategy` trait + 现有策略、`data::DataHandler` trait。
- **基金专属（保持原位不动）**：`data::{eastmoney,cache,fundlist,sync}`、`analyze`、`recommend`、`config`、`report`、`web` 的基金路由。
- **股票专属（本子项目新增，见下）**：全部收敛在 `src/stock/` 顶层模块树。

改股票业务永不触碰基金专属代码，反之亦然；唯一的公共面是稳定的通用引擎接口。

> 注：将现有基金专属文件归拢进 `src/fund/` 目录纯属美化，涉及大量文件移动与 import 改写、有回归风险，**不在本子项目范围**，可作为独立的小重构子项目按需另做。

### 复权方式：后复权

东财 K 线 `fqt` 参数：`1=前复权`、`2=后复权`。采用 `fqt=2`。`close`=不复权收盘价（显示用），`adj_close`=后复权收盘价（计算用），语义对齐基金的 `nav`/`adj_nav`。前复权会随新分红回填、老数据可能为负，不适合回测，故不用。

## 模块结构

股票代码收敛在**顶层 `src/stock/` 模块树**（与基金分开）；`src/lib.rs` 加一行 `pub mod stock;`。本子项目只落地其中的 `data` 子模块（后续子项目在 `src/stock/` 下续加 `strategy`、`analyze` 等）。

```
src/stock/
├── mod.rs          —— pub mod data; （后续子项目续加）
└── data/
    ├── mod.rs      —— StockBar 模型 + StockData 适配器(impl DataHandler) + 复权序列构造
    ├── secid.rs    —— 代码 → 东财 secid 三市场映射（纯函数，可离线单测）
    ├── kline.rs    —— 东财 K 线抓取 + 解析（对标 src/data/eastmoney.rs）
    ├── cache.rs    —— CSV 读写 / covers / load_or_fetch（对标 src/data/cache.rs）
    ├── search.rs   —— 东财 suggest 搜索：代码/名称 → secid+名称（替代 fundlist，跨三市场）
    └── sync.rs     —— 增量同步（对标 src/data/sync.rs）
```

> 说明：新增股票文件与基金数据层结构“对标”仅指参照实现风格与测试习惯，代码上**不复用、不 `use` 基金模块**，两套数据层彼此独立。

## 数据模型

```rust
pub struct StockBar {
    pub date: NaiveDate,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,      // 不复权，用于显示 / 技术分析
    pub volume: f64,     // 成交量
    pub adj_close: f64,  // 后复权收盘价，用于回测 / 收益计算
}
```

CSV 表头：`date,open,high,low,close,volume,adj_close`，与基金 CSV 同风格、同缓存目录规则。

## secid 三市场映射（`secid.rs`）

东财 secid 格式为 `市场.代码`。策略：**A股/港股离线确定性映射，美股走搜索解析（联网）**。

- **A股**（6 位纯数字）：`6/5/9` 开头 → `1.`（沪，含科创 688）；`0/1/2/3` 开头 → `0.`（深，含创业板 300）。例：`600519`→`1.600519`。
- **港股**：数字补零到 5 位 → `116.`。例：`700`/`00700`→`116.00700`。
- **美股**（字母代码，如 `AAPL`）：无法离线判定 NASDAQ(105)/NYSE(106)/AMEX(107)，调用 `search.rs` 的 suggest 接口解析真实 secid，结果缓存。
- 兼容显式前缀：`sh600519` / `hk00700` / `us.AAPL` 直接解析。

接口：`resolve_secid(code) -> Result<Secid>`。A/港离线；美股经搜索。

**美股连通性风险**：作为本子项目**第一个执行任务**做联网验证。若东财美股不通，就地退回新浪/其他源，或将范围缩到 A股+港股（已提前约定为可接受的降级路径）。

## 抓取（`kline.rs`）

东财 K 线接口 `https://push2his.eastmoney.com/api/qt/stock/kline/get`，`klt=101`（日线）。

- 一次 `fqt=0` 取不复权 OHLCV，一次 `fqt=2` 取后复权收盘价，按日期 merge 成 `StockBar`。
- 解析 `data.klines` 字符串数组（`日期,开,收,高,低,量,...`），解析风格对标 `parse_pingzhongdata`。
- 请求头带 Referer/User-Agent，`reqwest::blocking`，与基金抓取一致。

## 缓存 / 同步 / 搜索

- **cache.rs**：`load_or_fetch(code, cache_dir, start, end)`；缓存文件名用**规范化后的代码**（如 `sh600519.csv`），避免三市场重码；`covers` 覆盖判定逻辑照搬基金版。
- **sync.rs**：`merge_incremental` / `sync_stock` / `sync_all`（扫股票缓存目录取代码逐个同步），逻辑照搬基金版。
- **search.rs**：`search(query) -> Vec<StockInfo { code, name, secid, market }>`，走东财 suggest 接口。一箭双雕：既为美股解析 secid，又直接作为子项目 5 自动补全的后端。**因此不维护全市场清单文件**（比基金 fundlist 更省）。

## 适配器（`src/stock/data/mod.rs` 内）

```rust
pub struct StockData { /* bars: Vec<MarketEvent>, cursor: usize */ } // impl DataHandler
```

构造时把 `Vec<StockBar>` 映射为 `MarketEvent { date, nav: close, adj_nav: adj_close }`。引擎/策略/broker 完全不改即可回测个股（子项目 2）。技术分析（子项目 3）直接消费 `Vec<StockBar>` 拿 OHLCV。

## 测试策略

对齐现有习惯，除美股冒烟外全部离线样本、可 `cargo test` 通过。

- `secid.rs`：各市场代码 → secid 断言，含边界（科创 688、创业板 300、港股补零、显式前缀 sh/hk/us）。
- `kline.rs`：内置 JSON 样本，断言 OHLCV 解析 + 后复权 merge 正确、日期按 CST 正确。
- `cache.rs`：CSV roundtrip、covers 覆盖判定（照搬基金版用例结构）。
- `sync.rs`：增量只追加更新点（照搬基金版用例结构）。
- `mod.rs`：`StockData` 的 `history` 不含未来（照搬 `history_never_returns_future` 用例结构）。
- **美股**：单独 `#[ignore]` 联网冒烟测试，默认不跑，用于人工验证连通性。

## 交付定义（Done）

- `src/stock/` 顶层模块树落地（`mod.rs` + `data/` 六个文件），`cargo build` 通过。
- 股票代码不 `use` 任何基金专属模块（`data::eastmoney/cache/fundlist/sync`、`analyze`、`recommend` 等）；仅依赖通用件（`event`、`data::DataHandler`）。
- 上述离线单测全部通过。
- 能对一只 A股、一只港股（若美股验证通过则含一只美股）跑通 `load_or_fetch` 并生成 CSV 缓存。
- `StockData` 可被现有 `Engine::new` 接受（类型层面验证复用，实际回测在子项目 2）。

# 基金代码下拉搜索 —— 设计文档

- 日期：2026-06-19
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh Web 界面（单次/对比/寻优，已合入 master）

## 1. 目标

把 Web 界面里所有"基金代码"文本输入升级为**可搜索下拉**：用户键入代码、名称或拼音，即时过滤天天基金全量清单（约 1.8 万只），点选填入 6 位代码；同时**仍允许直接手输任意代码**（清单缺失或新基金时不阻断）。

非目标（YAGNI）：拼音全拼/首字母混合的高级排序、收藏夹、搜索历史、清单定期自动刷新、服务端分页搜索。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 清单来源 | 天天基金 `fundcode_search.js` 全量 | 用户选定；覆盖全市场 |
| 交付方式 | 后端缓存清单 + `GET /api/funds` 一次性返回；前端内存过滤 | 一次加载、本地秒级过滤，无每键请求 |
| 下拉组件 | 自定义 combobox（输入框 + 过滤结果列表） | 1.8 万项原生 `<datalist>` 会卡；自定义只渲染前 N 条 |
| 匹配 | code 前缀 / name 子串 / pinyin 子串，显示前 20 条 | 简单够用 |
| 容错 | 清单抓取失败 → `/api/funds` 返回空数组，combobox 退化为普通输入框 | 不阻断回测主流程 |
| 手输 | 不强制选列表项，input 仍可填任意值 | 新基金/清单缺失时可用；后端 charset 校验兜底 |
| 缓存刷新 | 缓存长期有效，删 `fundlist.json` 即刷新 | YAGNI，无需定时任务 |

## 3. 后端

### 3.1 新增 `src/data/fundlist.rs`

```rust
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FundInfo {
    pub code: String,
    pub name: String,
    pub pinyin: String, // 拼音简写（用于搜索）
}

/// 解析 fundcode_search.js 的 `var r = [["000001","HXCZHH","华夏成长混合","混合型-灵活","..."],...];`
/// 取每条 [0]=code、[1]=pinyin、[2]=name。
pub fn parse_fund_list(body: &str) -> anyhow::Result<Vec<FundInfo>>;

/// 抓取天天基金全量清单。
pub fn fetch_fund_list() -> anyhow::Result<Vec<FundInfo>>;

/// 缓存优先：cache_dir/fundlist.json 存在则读盘，否则抓取并写盘。
pub fn load_or_fetch_fund_list(cache_dir: &std::path::Path) -> anyhow::Result<Vec<FundInfo>>;
```

- `fetch_fund_list`：复用 `reqwest::blocking`（与 `eastmoney::fetch` 同款），GET `https://fund.eastmoney.com/js/fundcode_search.js`，带 `Referer: https://fund.eastmoney.com/`，对返回体调 `parse_fund_list`。
- `parse_fund_list`：定位 `var r = ` 后的 JSON 数组（复用 eastmoney.rs 已有的 `extract_array` 思路或就地截取 `[` 到 `];`），`serde_json` 解析为 `Vec<Vec<String>>`，每行取 0/1/2 构造 `FundInfo`；跳过列数不足的行。
- `load_or_fetch_fund_list`：`cache_dir.join("fundlist.json")` 存在→读+`serde_json` 反序列化为 `Vec<FundInfo>`；否则 `fetch_fund_list()` → 写 `fundlist.json`（`serde_json::to_string`）→ 返回。写盘失败仅 `eprintln!` 警告，不致命。

`src/data/mod.rs` 加 `pub mod fundlist;`。

### 3.2 路由 `GET /api/funds`（src/web/mod.rs）

- handler `funds_handler() -> Json<Vec<FundInfo>>`：`spawn_blocking(|| load_or_fetch_fund_list(Path::new(".cache")))`；**成功返回清单，失败返回空 `vec![]`**（记 `eprintln!`），始终 200。前端据空与否决定是否启用下拉。
- `router()` 注册 `.route("/api/funds", get(funds_handler))`。

## 4. 前端（src/web/page.rs）

### 4.1 数据加载
页面脚本启动时 `fetch('/api/funds').then(r=>r.json())` 存全局 `FUNDS`（数组）。失败或空数组 → `FUNDS=[]`，combobox 行为等同普通输入框（仅手输，无下拉）。

### 4.2 combobox 组件
一个可复用 JS 函数 `attachCombobox(input)`：
- 在 input 外包一个 `position:relative` 容器，紧随 input 加一个绝对定位 `.fund-dropdown`（默认隐藏）。
- `input` 事件：取值 `q`，若 `FUNDS` 为空或 `q` 为空则隐藏列表；否则过滤——`code.startsWith(q)` 或 `name.includes(q)` 或 `pinyin` 含大写化 `q`——取前 20 条，渲染 `代码 名称` 列表项。
- 点击列表项：把 input 值设为该项 `code`，隐藏列表，触发 input 的 `change`（保持既有随基金联动逻辑，如有）。
- `blur`（延迟 150ms 以允许点击）隐藏列表；`Esc` 隐藏。
- 键盘上下键高亮 + 回车选中（首版可选；若实现复杂则仅鼠标点选 + 手输，列表项 `mousedown` 防 blur）。
- 不修改提交逻辑：各提交仍读 input 当前值。

### 4.3 挂载点
对三处 fund 输入调用 `attachCombobox`：单次 `name="fund_code"`、寻优 `#opt-fund`、对比每个动态行的 `.rfund`（`addCompareRow` 内新建行后调用）。

### 4.4 样式
`.fund-dropdown{position:absolute;z-index:10;background:#fff;border:1px solid #cfd6e0;border-radius:6px;max-height:260px;overflow:auto;...}`，`.fund-item{padding:6px 10px;cursor:pointer}`，`.fund-item:hover{background:#f0f2f5}`。沿用现有设计语言。

## 5. 错误处理

- 清单抓取/解析失败：`/api/funds` 返回空数组（200），前端退化为手输。
- 前端 `fetch('/api/funds')` 网络失败：catch → `FUNDS=[]`，不弹错、不阻断。
- 选中项填的是纯数字 code，天然通过后端 fund_code charset 校验；手输非法值仍由后端回测时报 400（既有行为）。

## 6. 测试

- 单元（`data::fundlist`）：`parse_fund_list` 给样本 `var r = [["000001","HXCZHH","华夏成长混合","混合型","H"],["161725","ZSBJ","招商中证白酒","指数","Z"]];` → 2 条，断言 code/name/pinyin 正确；列数不足的行被跳过；空/无 `var r` 报错。
- 路由（web）：`GET /api/funds` 经 oneshot → 200，`content-type` 为 JSON，body 能 `serde_json` 解析为数组（hermetic：若 `.cache/fundlist.json` 不存在会联网——测试改为断言"200 且 body 可解析为 JSON 数组"，对空数组也成立；核心解析逻辑由 `parse_fund_list` 单测覆盖）。
- Playwright（扩展 verify_web.py）：单次 tab 基金框输入"白酒"，等待 `.fund-dropdown` 出现含 `161725` 的项，点击后断言框值为 `161725`，点运行出报告；截图 `output/web_fundsearch.png`。
- 既有 83 测试保持绿、clippy 干净。

## 7. 单元边界

- `data::fundlist`：抓取/解析/缓存清单，纯数据，`parse_fund_list` 独立可测。
- `funds_handler`：编排（spawn_blocking + 降级），薄。
- `page.rs` 的 `attachCombobox`：纯前端组件，挂任意 input。
- 各处经 `FundInfo` JSON / input 值通信。

## 8. 对既有的影响

- 仅新增 `data::fundlist`、一条只读路由、前端 combobox 与挂载；不改回测/报告/既有提交逻辑。
- page.rs 在三处 fund input 上叠加行为；输入框本身与 name 不变，提交载荷不变。

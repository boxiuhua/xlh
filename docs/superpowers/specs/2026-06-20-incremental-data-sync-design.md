# 增量数据同步（Web 同步按钮）—— 设计文档

- 日期：2026-06-20
- 状态：已定稿（用户逐步确认通过）
- 依附：xlh 回测引擎 + 数据缓存 + Web 界面（已合入 master）

## 1. 目标

在 Web 界面提供「数据同步」：把 `.cache` 里已缓存的基金净值**增量更新到最新**（只追加比缓存最后一天更新的净值），支持「同步全部」与「同步指定代码」两种。解决缓存抓一次后永不刷新、数据陈旧的问题。

非目标（YAGNI）：lsjz 真·网络增量接口、定时自动同步、并发/进度条、同步历史记录、回测时自动刷新。

## 2. 关键决策

| 决策 | 选择 | 理由 |
|------|------|------|
| 触发 | Web 一个「数据同步」常驻小卡片（标题下，不进三 Tab） | 用户选定 |
| 范围 | 同步全部已缓存 + 同步指定代码，两者都做 | 用户选定 |
| 增量机制 | 复用 `eastmoney::fetch`（全量）+ **只尾部追加** `date > 缓存最后一天` 的点 | pingzhongdata 无起始日参数；增量体现在存储/汇报层；复用现成抓取，简单稳 |
| 容错 | 单只失败记错误继续，整体 200 | 一只抓不到不影响其它 |

## 3. 后端

### 3.1 纯函数 `merge_incremental`（新增 `src/data/sync.rs`）

```rust
use crate::data::NavPoint;

/// 把 fresh 中「日期晚于 cached 最后一天」的点追加到 cached。
/// 返回 (合并后序列, 新增条数)。cached 为空时 fresh 全部计入。
/// 假定两者各自按日期升序；合并结果升序。
pub fn merge_incremental(cached: &[NavPoint], fresh: Vec<NavPoint>) -> (Vec<NavPoint>, usize);
```
实现：
- `last = cached.last().map(|p| p.date)`。
- `new: Vec<NavPoint> = fresh.into_iter().filter(|p| last.map_or(true, |d| p.date > d)).collect()`（fresh 先按日期升序排序以稳妥）。
- `merged = cached.to_vec(); merged.extend(new.iter().copied());`（cached 在前，new 追加）。
- 返回 `(merged, new.len())`。

### 3.2 `data::sync`（同文件）

```rust
#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub code: String,
    pub added: usize,            // 新增净值条数
    pub total: usize,            // 同步后总条数
    pub latest: Option<String>, // 最新净值日期 yyyy-mm-dd
    pub error: Option<String>,  // 失败原因（成功为 None）
}

/// 同步单只：读旧缓存(无则空)→ fetch 全量 → merge_incremental → 写回 → 汇总。
pub fn sync_fund(code: &str, cache_dir: &Path) -> SyncOutcome;

/// 同步全部：扫 cache_dir 下 *.csv（排除 fundlist.json）取代码，逐个 sync_fund。
pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome>;
```
- `sync_fund` 内部任何步骤出错 → 返回 `SyncOutcome{code, added:0, total:0, latest:None, error:Some(原因)}`（不 panic、不 `?` 传播到外层）。成功则写回 csv 并返回 added/total/latest。
- `code` 在 sync_fund 内先做 charset 校验（复用 web 已有规则等价：非空/≤12/alnum），非法直接返回 error 项（防越权读写）。
- `sync_all` 列目录：取 `*.csv` 文件名去后缀作为 code（`fundlist.json` 非 csv 自然排除）。空目录 → 空 Vec。
- 复用 `cache::{read_csv, write_csv}`、`eastmoney::fetch`、`NavPoint`。
- `src/data/mod.rs` 加 `pub mod sync;`。

### 3.3 路由 `POST /api/sync`（src/web/mod.rs）

```rust
#[derive(Debug, Deserialize)]
pub struct SyncRequest { #[serde(default)] pub code: Option<String> }
```
- handler `sync_handler(Json(req)) -> Json<Vec<SyncOutcome>>`：`spawn_blocking` 内——`req.code` 为 `Some(c)` → `vec![sync_fund(&c, ".cache")]`；为 `None` → `sync_all(".cache")`。始终 200。
- `router()` 注册 `.route("/api/sync", post(sync_handler))`。
- 网络 IO 全在 blocking 闭包内；`SyncRequest`(纯数据) 跨 await 安全。

## 4. 前端（src/web/page.rs）

标题 `<h1>` 之后、`.tabs` 之前，加一个常驻卡片：

```
<div class="card" id="sync-card">
  数据同步：[同步全部已缓存]  | 代码 [<input id="sync-code">] [同步此基金]
  <div id="sync-result"></div>   // 结果列表
</div>
```
- 「同步全部」按钮：`POST /api/sync` body `{}` → 渲染结果。
- 「同步此基金」：读 `#sync-code` 值，`POST /api/sync` body `{code}`；空值则提示先填代码。
- `#sync-code` 复用 `attachCombobox`（基金搜索）。
- 结果区：每项一行 `代码 +N 条新 · 最新 YYYY-MM-DD`（绿色成功）或 `代码 同步失败: 原因`（红色）。全部经 `esc()` 转义。
- 运行中禁用相关按钮、显示「同步中…」；fetch 失败 catch 显示错误。
- 同步成功后**不自动重跑回测**（用户再点运行即用上新数据，因为 load_or_fetch 在缓存覆盖区间时读缓存——新数据已落盘）。

## 5. 错误处理

- 单只失败 → 该 `SyncOutcome.error` 有值，前端红字；其它项正常。
- 整个请求始终 200（即使全失败，返回各自 error 的数组）。
- 前端 `fetch` 网络错误 → catch 显示「同步请求失败」。
- 非法 code → sync_fund 返回 error 项（不触达文件系统写非法路径）。

## 6. 测试

- 单元（`data::sync`）：`merge_incremental`
  - cached 到 2024-02-01，fresh 含 2024-01~2024-03 → 只追加 >02-01 的点，added 正确、合并升序；
  - fresh 全是 ≤ 缓存末日 → added 0、序列不变；
  - cached 空 → fresh 全部计入。
- 路由：`POST /api/sync` 经 oneshot 返回 200 且 body 是 JSON 数组（hermetic：用临时空 cache_dir 思路难，因 handler 硬编码 `.cache` 且会联网——路由测试断言「200 + body 可解析为数组」，对空也成立；核心 merge 逻辑由纯函数单测覆盖）。
- Playwright（扩展 verify_web.py）：点「同步全部已缓存」，等待 `#sync-result` 出现至少一条同步条目（含已缓存代码如 161725），断言无 console error，截图 `output/web_sync.png`。
- 既有 97 测试保持绿、clippy 干净。

## 7. 单元边界

- `merge_incremental`：纯函数，增量合并逻辑，独立可测。
- `sync_fund`/`sync_all`：编排（读缓存 + fetch + merge + 写 + 汇总/容错），无 panic。
- `sync_handler`：薄路由（spawn_blocking 分流 code/全部）。
- page.rs 同步卡片：静态前端 + fetch。
- 经 `SyncOutcome` JSON 通信。

## 8. 对既有的影响

- 纯新增：`data::sync` 模块、一条 `POST /api/sync` 路由、前端一个卡片；不改回测/报告/既有缓存读取（`load_or_fetch` 不动——它在缓存覆盖区间时读缓存，同步后的新数据已在 csv 里，下次回测自然用上）。
- 不改 `eastmoney::fetch`、`cache::{read_csv,write_csv}`（复用）。

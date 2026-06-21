# 增量数据同步（Web 同步按钮）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Web 界面加「数据同步」卡片：把已缓存基金净值增量更新到最新（同步全部 / 同步指定代码）。

**Architecture:** 新增 `data::sync`（纯函数 `merge_incremental` 尾部追加 + `sync_fund`/`sync_all` 编排容错）；`POST /api/sync` 路由（spawn_blocking 内做网络 IO）；page.rs 标题下加常驻同步卡片。复用 `eastmoney::fetch` 与 `cache::{read_csv,write_csv}`，不改既有缓存读取与回测。

**Tech Stack:** Rust（serde/axum/reqwest blocking）+ 原生 JS + Playwright。

## Global Constraints

- 纯新增：`data::sync` 模块、一条只写路由 `POST /api/sync`、前端一个卡片；不改 `load_or_fetch`/`eastmoney::fetch`/`cache::{read_csv,write_csv}`/回测/报告。
- 不破坏既有 97 测试与 clippy 干净。
- 增量机制：复用全量 `eastmoney::fetch`，缓存只**尾部追加** `date > 缓存最后一天` 的点；全新基金全量写入。
- 容错：`sync_fund` 任何步骤失败返回带 `error` 的 `SyncOutcome`（不 panic、不向外 `?`）；`sync_all` 单只失败继续；路由始终 200。
- fund code 校验（非空/≤12/ASCII 字母数字）在 `sync_fund` 最前，非法直接返回 error 项（不触达文件系统/网络）——这也让路由测试可 hermetic。
- 缓存目录固定 `.cache`。
- 非 Send 无关；网络 IO 在 spawn_blocking 内；`SyncRequest`(纯数据) 跨 await 安全。
- edition 2021；提交含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `data::NavPoint { date: NaiveDate, nav: f64, acc_nav: f64 }`（`Copy`）。
- `data::cache::{read_csv(&Path)->Result<Vec<NavPoint>>, write_csv(&Path,&[NavPoint])->Result<()>}`（均 pub）。
- `data::eastmoney::fetch(code:&str)->Result<Vec<NavPoint>>`。
- `data/mod.rs` 现有 `pub mod eastmoney; pub mod cache; pub mod fundlist;`。
- web/mod.rs `router()`：`/`(get)、`/api/run`(get)、`/api/compare`(post)、`/api/optimize`(post)、`/api/funds`(get)；`use axum::routing::{get, post}` 已在用；`axum::Json` 已用；`AppError` 存在；`esc()` 与 `attachCombobox` 在 page.rs 脚本中。
- page.rs INDEX_HTML：`<h1>xlh 基金回测</h1>` 紧接 `<div class="tabs">`；`.card` 样式已定义。

---

## Task 1: data::sync 模块

**Files:**
- Create: `src/data/sync.rs`
- Modify: `src/data/mod.rs`（加 `pub mod sync;`）
- Test: `src/data/sync.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces:
  - `pub fn merge_incremental(cached: &[NavPoint], fresh: Vec<NavPoint>) -> (Vec<NavPoint>, usize)`
  - `pub struct SyncOutcome { code:String, added:usize, total:usize, latest:Option<String>, error:Option<String> }`（`#[derive(Debug, Serialize)]`）
  - `pub fn sync_fund(code: &str, cache_dir: &Path) -> SyncOutcome`
  - `pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome>`

- [ ] **Step 1: 注册模块**

`src/data/mod.rs` 在 `pub mod fundlist;` 下加：

```rust
pub mod sync;
```

- [ ] **Step 2: 写失败测试 + 占位**

创建 `src/data/sync.rs`：

```rust
use std::path::Path;
use serde::Serialize;
use crate::data::{NavPoint, cache, eastmoney};

/// 把 fresh 中「日期晚于 cached 最后一天」的点追加到 cached。
/// 返回 (合并后序列, 新增条数)。cached 为空 → fresh 全部计入。
pub fn merge_incremental(cached: &[NavPoint], fresh: Vec<NavPoint>) -> (Vec<NavPoint>, usize) {
    let mut fresh = fresh;
    fresh.sort_by_key(|p| p.date);
    let last = cached.last().map(|p| p.date);
    let new: Vec<NavPoint> = fresh.into_iter()
        .filter(|p| last.map_or(true, |d| p.date > d))
        .collect();
    let mut merged = cached.to_vec();
    merged.extend(new.iter().copied());
    (merged, new.len())
}

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub code: String,
    pub added: usize,
    pub total: usize,
    pub latest: Option<String>,
    pub error: Option<String>,
}

fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 12 && code.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 同步单只：校验→读旧缓存→fetch 全量→merge_incremental→写回→汇总。任何失败返回带 error 的 outcome。
pub fn sync_fund(code: &str, cache_dir: &Path) -> SyncOutcome {
    if !valid_code(code) {
        return SyncOutcome { code: code.to_string(), added: 0, total: 0, latest: None, error: Some("基金代码非法".into()) };
    }
    let path = cache_dir.join(format!("{code}.csv"));
    let cached = if path.exists() { cache::read_csv(&path).unwrap_or_default() } else { Vec::new() };
    let fresh = match eastmoney::fetch(code) {
        Ok(f) => f,
        Err(e) => return SyncOutcome {
            code: code.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|p| p.date.to_string()), error: Some(format!("抓取失败: {e}")),
        },
    };
    let (merged, added) = merge_incremental(&cached, fresh);
    if let Err(e) = cache::write_csv(&path, &merged) {
        return SyncOutcome {
            code: code.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|p| p.date.to_string()), error: Some(format!("写入失败: {e}")),
        };
    }
    let latest = merged.last().map(|p| p.date.to_string());
    SyncOutcome { code: code.to_string(), added, total: merged.len(), latest, error: None }
}

/// 同步全部：扫 cache_dir 下 *.csv 取代码，逐个 sync_fund。
pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome> {
    let mut codes: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("csv") {
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    codes.push(stem.to_string());
                }
            }
        }
    }
    codes.sort();
    codes.iter().map(|c| sync_fund(c, cache_dir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn np(dt: NaiveDate, v: f64) -> NavPoint { NavPoint { date: dt, nav: v, acc_nav: v } }

    #[test]
    fn appends_only_newer_points() {
        let cached = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let fresh = vec![np(d(2024,1,15),1.05), np(d(2024,2,1),1.1), np(d(2024,3,1),1.2)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 1, "只有 2024-03-01 晚于缓存末日 2024-02-01");
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.last().unwrap().date, d(2024,3,1));
    }

    #[test]
    fn no_new_points_when_fresh_all_old() {
        let cached = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let fresh = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 0);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn empty_cache_takes_all_fresh() {
        let (merged, added) = merge_incremental(&[], vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)]);
        assert_eq!(added, 2);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn sync_fund_rejects_bad_code() {
        let o = sync_fund("../etc", Path::new(".cache"));
        assert!(o.error.is_some(), "非法代码应返回 error");
        assert_eq!(o.added, 0);
    }

    #[test]
    fn sync_all_empty_dir_is_empty() {
        let dir = std::env::temp_dir().join("xlh_sync_empty_test");
        std::fs::create_dir_all(&dir).unwrap();
        // 确保无 csv
        let out = sync_all(&dir);
        assert!(out.is_empty(), "空目录无可同步基金");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

注：`merge_incremental` 与所有函数已是完整实现（非占位）。`sync_fund_rejects_bad_code` 与 `sync_all_empty_dir_is_empty` 都 hermetic（非法代码在 fetch 前短路；空目录无 sync_fund 调用，均不联网）。

- [ ] **Step 3: 运行测试**

Run: `cargo test --lib data::sync`
Expected: 5 测全过（merge 三例 + 非法代码 + 空目录）。若失败按报错修正。

- [ ] **Step 4: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/data/mod.rs src/data/sync.rs
git commit -m "feat: data::sync 增量同步（merge_incremental + sync_fund/sync_all）"
```

---

## Task 2: POST /api/sync 路由

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `data::sync::{SyncOutcome, sync_fund, sync_all}`（Task 1）。
- Produces: `SyncRequest{code:Option<String>}`、`sync_handler`；`router()` 注册 `POST /api/sync`。

- [ ] **Step 1: 写失败的 hermetic 路由测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增（用非法代码，在 fetch 前短路，不联网）：

```rust
    #[tokio::test]
    async fn sync_route_bad_code_returns_array() {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let body = serde_json::json!({"code":"bad!!code"}).to_string();
        let resp = super::router()
            .oneshot(Request::builder().method("POST").uri("/api/sync")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
        assert_eq!(v.as_array().unwrap().len(), 1, "指定 code → 单元素");
        assert!(v[0]["error"].is_string(), "非法代码应带 error");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::sync_route_bad_code_returns_array`
Expected: 失败（路由不存在 → 404 ≠ 200，或 handler 未定义）。

- [ ] **Step 3: 实现 + 注册路由**

在 `router()` 里加（与现有 post 路由并列）：

```rust
        .route("/api/sync", axum::routing::post(sync_handler))
```

在文件合适处（其它 handler 附近）加：

```rust
#[derive(Debug, Deserialize)]
pub struct SyncRequest {
    #[serde(default)]
    pub code: Option<String>,
}

async fn sync_handler(
    axum::Json(req): axum::Json<SyncRequest>,
) -> axum::Json<Vec<crate::data::sync::SyncOutcome>> {
    let out = tokio::task::spawn_blocking(move || {
        let dir = std::path::Path::new(".cache");
        match req.code {
            Some(c) => vec![crate::data::sync::sync_fund(&c, dir)],
            None => crate::data::sync::sync_all(dir),
        }
    })
    .await
    .unwrap_or_default();
    axum::Json(out)
}
```

（`Deserialize` 已在文件 import；`axum::Json`/`tokio::task::spawn_blocking` 已在用。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 新测 + 既有全过。

- [ ] **Step 5: clippy + 全量 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add src/web/mod.rs
git commit -m "feat: POST /api/sync（同步全部/指定基金，失败降级）"
```

---

## Task 3: 前端同步卡片

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/mod.rs`（GET / 含同步卡片断言）

**Interfaces:**
- Consumes: `POST /api/sync`。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内 `index_has_rsi_option` 附近加：

```rust
    #[tokio::test]
    async fn index_has_sync_card() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("/api/sync"), "应有同步端点调用");
        assert!(body.contains("数据同步"), "应有同步卡片标题");
        assert!(body.contains("id=\"sync-result\""), "应有结果区");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::index_has_sync_card`
Expected: 失败（无同步卡片）。

- [ ] **Step 3: page.rs 加同步卡片 HTML**

在 `src/web/page.rs` INDEX_HTML 中，`<h1>xlh 基金回测</h1>` 之后、`<div class="tabs">` 之前插入：

```html
  <div class="card" id="sync-card">
    <div class="row" style="align-items:flex-end">
      <strong style="margin-right:8px">数据同步</strong>
      <button class="small" id="sync-all">同步全部已缓存</button>
      <div class="field combo"><label>基金代码</label><input id="sync-code" placeholder="如 161725"/></div>
      <button class="small" id="sync-one">同步此基金</button>
    </div>
    <div id="sync-result" class="hint" style="margin-top:8px"></div>
  </div>
```

- [ ] **Step 4: page.rs 加同步 JS**

在 INDEX_HTML 的 `<script>` 中，靠近其它 `attachCombobox(...)` 调用处（脚本主体、DOM 就绪区）加：

```javascript
function renderSync(items){
  var box = document.getElementById('sync-result');
  if(!Array.isArray(items) || !items.length){ box.innerHTML = '<span style="color:#7f8c8d">无可同步的基金（缓存为空）</span>'; return; }
  box.innerHTML = items.map(function(o){
    if(o.error) return '<div style="color:#c0392b">'+esc(o.code)+' 同步失败: '+esc(o.error)+'</div>';
    return '<div style="color:#1a7f37">'+esc(o.code)+' +'+o.added+' 条新 · 最新 '+esc(o.latest||'-')+'（共 '+o.total+'）</div>';
  }).join('');
}
function doSync(body, btn){
  var box = document.getElementById('sync-result');
  var t = btn.textContent; btn.disabled = true; btn.textContent = '同步中…'; box.textContent = '同步中…';
  fetch('/api/sync', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(body)})
    .then(function(r){ return r.json(); }).then(renderSync)
    .catch(function(e){ box.innerHTML = '<span style="color:#c0392b">同步请求失败: '+esc(String(e))+'</span>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = t; });
}
document.getElementById('sync-all').addEventListener('click', function(){ doSync({}, this); });
document.getElementById('sync-one').addEventListener('click', function(){
  var c = document.getElementById('sync-code').value.trim();
  if(!c){ document.getElementById('sync-result').innerHTML = '<span style="color:#c0392b">请先填基金代码</span>'; return; }
  doSync({code:c}, this);
});
attachCombobox(document.getElementById('sync-code'));
```

（`esc` 与 `attachCombobox` 已是脚本顶部的全局函数，此处可用。）

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: `index_has_sync_card` + 既有（含 index_serves_form/index_has_three_tabs/index_has_fund_combobox/index_has_rsi_option）全过。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat: Web 数据同步卡片（同步全部/指定基金）"
```

---

## Task 4: Playwright 端到端自测

**Files:**
- Modify: `scripts/verify_web.py`

- [ ] **Step 1: 扩展 verify_web.py 覆盖同步**

在 `scripts/verify_web.py` 现有校验之后、`browser.close()` 之前加一段：点「同步全部已缓存」，等结果出现。新增片段：

```python
        # ---- 数据同步 ----
        page.click("#sync-all")
        # 同步全部会逐只联网抓取已缓存基金，给足时间
        page.wait_for_selector("#sync-result div", timeout=120000)
        sync_text = page.locator("#sync-result").inner_text(timeout=10000)
        assert ("条新" in sync_text) or ("同步失败" in sync_text), "同步结果应出现条目"
        page.screenshot(path=str(Path("output/web_sync.png").resolve()), full_page=True)
```

（保留脚本顶部清除 `*_PROXY` 逻辑与既有校验。最终 PASS 文案补充含「数据同步」。）

- [ ] **Step 2: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 PASS；生成 `output/web_sync.png`。同步会真实联网更新已缓存基金（161725/050002/000834/003095 等）到最新。

- [ ] **Step 3: 肉眼核对截图**

Read `output/web_sync.png`：标题下「数据同步」卡片，结果区列出已缓存基金的 `代码 +N 条新 · 最新 日期`。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 4: 全量测试 + 提交**

Run: `cargo test`（全绿）。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 校验数据同步"
```

交付报告：同步结果（各基金新增条数 + 最新日期）、截图、各断言结果。

---

## Self-Review

- **Spec 覆盖**：§3.1 merge_incremental→T1；§3.2 sync_fund/sync_all/SyncOutcome→T1；§3.3 路由→T2；§4 前端卡片+JS→T3；§5 容错→T1(sync_fund/sync_all 不 panic)/T2(始终200)/T3(catch)；§6 测试→T1 纯函数+空目录、T2 hermetic 路由、T3 GET/、T4 e2e；§7/§8 边界与影响→各 Task 纯新增。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码。
- **类型一致**：`merge_incremental(&[NavPoint],Vec<NavPoint>)->(Vec,usize)`(T1)；`SyncOutcome{code,added,total,latest,error}`(T1) = T2 返回类型 = T3 前端读 `o.code/o.added/o.total/o.latest/o.error`；`sync_fund(&str,&Path)`/`sync_all(&Path)`(T1) = T2 调用；`SyncRequest{code:Option<String>}`(T2) = 前端 body `{}`/`{code}`(T3)。
- **Hermetic 测试**：T1 merge 纯函数 + 非法代码短路 + 空目录；T2 路由用非法代码（fetch 前返回 error，不联网）；live 同步由 T4 Playwright。
- **不破坏既有**：仅新增模块/路由/卡片；load_or_fetch/fetch/csv 读写复用不改；既有 web/report/strategy 测试不动。
- **YAGNI**：不做 lsjz 真增量、定时同步、并发/进度、历史记录、回测时自动刷新。

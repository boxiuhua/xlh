# 持仓建议历史 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给「持仓建议」加保存历史能力：Web 用户手动保存自己的建议（按用户隔离）、定时推送自动保存操作者建议（仅管理员可见），存 SQLite，面板/后台可回看。

**Architecture:** 新增 crate 级模块 `src/history.rs`（纯 rusqlite，asset-neutral），复用同一 `data/xlh.db`。Web 走现有 `AuthState` 的 `Arc<Mutex<Connection>>`；`xlh push` 独立进程自开一个连接连同一库。对持仓建议核心算法零改动，仅在外围加保存/查询。

**Tech Stack:** Rust 2021 · axum 0.7 · rusqlite（bundled）· serde_json · chrono（均已在依赖内）· tower（dev，oneshot 测试）

## Global Constraints

- 历史表 `advice_history`：`id / user_id(可空) / source('web'|'push') / created_at / summary / payload`。
- `created_at` 用完整时间戳 `YYYY-MM-DD HH:MM:SS`（`chrono::Local::now().format`）——区分一天多次保存；这不影响授权处仍用 `date_naive`。
- `payload` 为 JSON 字符串：`{"input": <HoldingsInput>, "report": <HoldingsReport>}`。
- Web 历史按登录用户隔离：`get_web(id, user_id)` 只返回属于该用户的记录，否则 None（→404）。
- Web 保留上限：每用户最新 100 条（`WEB_KEEP=100`），插入后删旧；push 历史不设上限。
- Web 保存走**后端重跑** `holdings_blocking`（权威），不信任前端已生成报告。
- 推送历史**仅管理员**可见（挂 `/api/admin/*`，走 require_admin）。
- 保存失败不得阻断主流程（Web 保存失败返回错误但不影响生成；push 保存失败仅告警、照常发送）。
- 复用现有 `holdings::HoldingsInput/HoldingsReport`；`summarize` 放 `holdings.rs`，Web 与 push 共用（DRY）。
- 现有 `build_message(cfg) -> Result<(String,bool)>` 的 3 个调用点行为不变。

---

### Task 1: 历史存储模块 `src/history.rs`

**Files:**
- Create: `src/history.rs`
- Modify: `src/lib.rs`（加 `pub mod history;`）

**Interfaces:**
- Produces：
  - `struct AdviceRecord { id: i64, created_at: String, summary: String }`（Serialize）
  - `history::migrate(&Connection) -> Result<()>`
  - `history::open_or_default(&Path) -> Result<Connection>`（建父目录+WAL+migrate）
  - `history::save(&Connection, user_id: Option<i64>, source: &str, summary: &str, payload: &str) -> Result<i64>`（web 记录插入后裁剪到最新 100）
  - `history::list_web(&Connection, user_id: i64, limit: i64) -> Result<Vec<AdviceRecord>>`
  - `history::get_web(&Connection, id: i64, user_id: i64) -> Result<Option<String>>`
  - `history::list_push(&Connection, limit: i64) -> Result<Vec<AdviceRecord>>`
  - `history::get_push(&Connection, id: i64) -> Result<Option<String>>`

- [ ] **Step 1: 注册模块**

在 `src/lib.rs` 的 `pub mod holdings;`（第 17 行）后加一行：

```rust
pub mod history;
```

- [ ] **Step 2: 写失败测试 + 实现**

创建 `src/history.rs`：

```rust
//! 持仓建议历史：Web(按用户) + 推送(全局，仅管理员) 的保存与查询，复用同一 SQLite 库。
use std::path::Path;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS advice_history (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER,
  source     TEXT NOT NULL,
  created_at TEXT NOT NULL,
  summary    TEXT NOT NULL,
  payload    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_advice_web  ON advice_history(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_advice_push ON advice_history(source, created_at);
"#;

/// Web 每用户保留的历史条数上限。
const WEB_KEEP: i64 = 100;

#[derive(Debug, Clone, Serialize)]
pub struct AdviceRecord {
    pub id: i64,
    pub created_at: String,
    pub summary: String,
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建历史表失败")?;
    Ok(())
}

/// 打开(或建)历史库文件并迁移；供独立进程（push）使用。
pub fn open_or_default(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).ok();
        }
    }
    let conn = Connection::open(path).with_context(|| format!("打开 {} 失败", path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    migrate(&conn)?;
    Ok(conn)
}

fn now_ts() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 保存一条历史，返回 id。source=="web" 时插入后删除该用户超出最新 WEB_KEEP 条的旧记录。
pub fn save(conn: &Connection, user_id: Option<i64>, source: &str, summary: &str, payload: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO advice_history (user_id, source, created_at, summary, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![user_id, source, now_ts(), summary, payload],
    )?;
    let id = conn.last_insert_rowid();
    if source == "web" {
        if let Some(uid) = user_id {
            conn.execute(
                "DELETE FROM advice_history
                  WHERE source='web' AND user_id=?1
                    AND id NOT IN (
                      SELECT id FROM advice_history
                       WHERE source='web' AND user_id=?1
                       ORDER BY created_at DESC, id DESC LIMIT ?2)",
                rusqlite::params![uid, WEB_KEEP],
            )?;
        }
    }
    Ok(id)
}

fn row_to_record(r: &rusqlite::Row) -> rusqlite::Result<AdviceRecord> {
    Ok(AdviceRecord { id: r.get(0)?, created_at: r.get(1)?, summary: r.get(2)? })
}

pub fn list_web(conn: &Connection, user_id: i64, limit: i64) -> Result<Vec<AdviceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, summary FROM advice_history
          WHERE source='web' AND user_id=?1 ORDER BY created_at DESC, id DESC LIMIT ?2")?;
    let rows = stmt.query_map(rusqlite::params![user_id, limit], row_to_record)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn list_push(conn: &Connection, limit: i64) -> Result<Vec<AdviceRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, created_at, summary FROM advice_history
          WHERE source='push' ORDER BY created_at DESC, id DESC LIMIT ?1")?;
    let rows = stmt.query_map(rusqlite::params![limit], row_to_record)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn get_web(conn: &Connection, id: i64, user_id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT payload FROM advice_history WHERE id=?1 AND source='web' AND user_id=?2",
        rusqlite::params![id, user_id], |r| r.get(0),
    ).optional().context("查历史失败")
}

pub fn get_push(conn: &Connection, id: i64) -> Result<Option<String>> {
    conn.query_row(
        "SELECT payload FROM advice_history WHERE id=?1 AND source='push'",
        [id], |r| r.get(0),
    ).optional().context("查历史失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    #[test]
    fn save_list_get_roundtrip() {
        let c = mem();
        let id = save(&c, Some(1), "web", "摘要A", "{\"k\":1}").unwrap();
        let list = list_web(&c, 1, 100).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].summary, "摘要A");
        assert_eq!(get_web(&c, id, 1).unwrap().as_deref(), Some("{\"k\":1}"));
    }

    #[test]
    fn user_isolation() {
        let c = mem();
        let id_a = save(&c, Some(1), "web", "A", "pa").unwrap();
        save(&c, Some(2), "web", "B", "pb").unwrap();
        // 用户2 取不到用户1 的记录
        assert!(get_web(&c, id_a, 2).unwrap().is_none());
        // 用户2 的列表不含用户1 的记录
        let l2 = list_web(&c, 2, 100).unwrap();
        assert_eq!(l2.len(), 1);
        assert_eq!(l2[0].summary, "B");
    }

    #[test]
    fn web_and_push_separated() {
        let c = mem();
        save(&c, Some(1), "web", "W", "pw").unwrap();
        save(&c, None, "push", "P", "pp").unwrap();
        assert_eq!(list_web(&c, 1, 100).unwrap().len(), 1);
        assert_eq!(list_push(&c, 100).unwrap().len(), 1);
        // web 列表不含 push；push 列表不含 web
        assert_eq!(list_web(&c, 1, 100).unwrap()[0].summary, "W");
        assert_eq!(list_push(&c, 100).unwrap()[0].summary, "P");
    }

    #[test]
    fn web_retention_caps_at_100() {
        let c = mem();
        for i in 0..105 {
            save(&c, Some(7), "web", &format!("s{i}"), "p").unwrap();
        }
        // 该用户恰保留 100 条
        assert_eq!(list_web(&c, 7, 1000).unwrap().len(), 100);
        // push 记录不受 web 裁剪影响
        save(&c, None, "push", "P", "p").unwrap();
        assert_eq!(list_push(&c, 1000).unwrap().len(), 1);
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test --lib history::`
Expected: 4 测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/history.rs
git commit -m "feat(history): 持仓建议历史存储模块"
```

---

### Task 2: `holdings.rs` 加 Serialize 与 summarize

**Files:**
- Modify: `src/holdings.rs`（`HoldingsInput` derive 加 `Serialize`；新增 `summarize`）

**Interfaces:**
- Consumes：`HoldingsReport`、`PortfolioSummary`。
- Produces：`holdings::summarize(&HoldingsReport) -> String`；`HoldingsInput` 现可 `Serialize`。

- [ ] **Step 1: 给 HoldingsInput 加 Serialize**

在 `src/holdings.rs` 找到（约第 22-23 行）：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct HoldingsInput {
```

改为：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldingsInput {
```

（`Serialize` 已在文件顶部 `use serde::{Deserialize, Serialize};` 引入。）

- [ ] **Step 2: 写失败测试 + 实现 summarize**

在 `src/holdings.rs` 末尾（`#[cfg(test)]` 模块之前；若无 tests 模块则直接加函数与新 tests 模块）加：

```rust
/// 历史列表摘要：只数 + 建议加/减仓总额。
pub fn summarize(report: &HoldingsReport) -> String {
    format!(
        "{} 只 · 加仓 {:.0} 减仓 {:.0}",
        report.summary.holding_count,
        report.summary.total_add,
        report.summary.total_trim,
    )
}

#[cfg(test)]
mod history_summary_tests {
    use super::*;

    #[test]
    fn summarize_uses_counts_and_totals() {
        let report = HoldingsReport {
            generated: "2026-07-05".into(),
            summary: PortfolioSummary {
                total_amount: 100000.0,
                total_profit: None,
                cumulative_profit: None,
                holding_count: 3,
                total_add: 1200.0,
                total_trim: 800.0,
                concentration_note: String::new(),
            },
            advices: vec![],
            skipped: vec![],
            disclaimer: String::new(),
        };
        assert_eq!(summarize(&report), "3 只 · 加仓 1200 减仓 800");
    }
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test --lib holdings::history_summary_tests`
Expected: 1 测试 PASS。（若既有 holdings 测试因 `Serialize` 派生受影响，一并 `cargo test --lib holdings::` 确认全绿。）

- [ ] **Step 4: Commit**

```bash
git add src/holdings.rs
git commit -m "feat(history): HoldingsInput 可序列化 + summarize 摘要"
```

---

### Task 3: Web 保存/查询接口 + serve 建表

**Files:**
- Modify: `src/web/mod.rs`（serve 加 `history::migrate`；新增 3 个 handler + `holdings_history_routes()`；并入 licensed 组）

**Interfaces:**
- Consumes：`history::{save,list_web,get_web,AdviceRecord}`、`holdings::{summarize, HoldingsInput}`、既有 `holdings_blocking`、`auth::{AuthState, CurrentUser}`、`AppError`。
- Produces：licensed 组新增路由 `POST /api/holdings/save`、`GET /api/holdings/history`、`GET /api/holdings/history/:id`。

- [ ] **Step 1: serve 建历史表**

在 `src/web/mod.rs` 的 `serve`（约第 375-376 行）：

```rust
    let conn = auth::store::open(&cfg.db_path).context("打开授权数据库失败")?;
    let state = auth::AuthState::new(conn, cfg);
```

改为：

```rust
    let conn = auth::store::open(&cfg.db_path).context("打开授权数据库失败")?;
    crate::history::migrate(&conn).context("建历史表失败")?;
    let state = auth::AuthState::new(conn, cfg);
```

- [ ] **Step 2: 加三个 handler + 路由函数**

在 `src/web/mod.rs` 的 `holdings_blocking`（约第 603 行）函数之后追加：

```rust
async fn holdings_save_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::Json(input): axum::Json<crate::holdings::HoldingsInput>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    let input_for_run = input.clone();
    let report = tokio::task::spawn_blocking(move || holdings_blocking(input_for_run))
        .await
        .map_err(|e| anyhow!("任务执行失败: {e}"))?;
    let summary = crate::holdings::summarize(&report);
    let payload = serde_json::to_string(&serde_json::json!({ "input": input, "report": report }))
        .map_err(|e| anyhow!("序列化历史失败: {e}"))?;
    let id = {
        let conn = st.db.lock().unwrap();
        crate::history::save(&conn, Some(user.id), "web", &summary, &payload)
            .map_err(|e| anyhow!("保存历史失败: {e}"))?
    };
    Ok(axum::Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn holdings_history_list_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
) -> axum::Json<Vec<crate::history::AdviceRecord>> {
    let rows = {
        let conn = st.db.lock().unwrap();
        crate::history::list_web(&conn, user.id, 100).unwrap_or_default()
    };
    axum::Json(rows)
}

async fn holdings_history_detail_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let found = {
        let conn = st.db.lock().unwrap();
        crate::history::get_web(&conn, id, user.id).ok().flatten()
    };
    match found {
        Some(payload) => ([(axum::http::header::CONTENT_TYPE, "application/json")], payload).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 持仓历史路由（需要 AuthState + CurrentUser，故不进泛型 core_routes）。
fn holdings_history_routes() -> Router<auth::AuthState> {
    Router::new()
        .route("/api/holdings/save", post(holdings_save_handler))
        .route("/api/holdings/history", get(holdings_history_list_handler))
        .route("/api/holdings/history/:id", get(holdings_history_detail_handler))
}
```

- [ ] **Step 3: 并入 licensed 组**

在 `router()` 里，把授权组构建（Task 11 的 `let licensed = core_routes::<AuthState>()...`）改为在套中间件前 `.merge(holdings_history_routes())`：

```rust
    let licensed = core_routes::<AuthState>()
        .merge(holdings_history_routes())
        .route_layer(from_fn_with_state(state.clone(), auth::require_license))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));
```

（保持 `require_license` 内层、`require_login` 外层的既有顺序不变。）

- [ ] **Step 4: 写集成测试**

在 `src/web/mod.rs` 的测试模块内追加（沿用既有 `super::router` / oneshot 风格；参考 `src/web/auth/routes.rs` 造会话的写法）：

```rust
    #[tokio::test]
    async fn holdings_save_requires_login() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        let state = crate::web::auth::AuthState::new(conn, Default::default());
        let app = super::router(state);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/api/holdings/save")
                .header("content-type", "application/json")
                .body(Body::from("{\"holdings\":[]}")).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn holdings_save_then_history_for_active_user() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        let state = crate::web::auth::AuthState::new(conn, Default::default());
        // 造一个已激活用户 + 会话
        let token = "tok-hist".to_string();
        {
            let c = state.db.lock().unwrap();
            let uid = crate::web::auth::store::create_user(&c, "u", "h", false).unwrap();
            crate::web::auth::store::set_expiry(&c, uid, chrono::Local::now().date_naive() + chrono::Duration::days(30)).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            crate::web::auth::store::create_session(&c, &token, uid, exp).unwrap();
        }
        // 保存（空持仓，离线可跑，报告 advices 为空但仍成功保存）
        let app = super::router(state.clone());
        let save = app.oneshot(
            Request::builder().method("POST").uri("/api/holdings/save")
                .header("content-type", "application/json")
                .header("cookie", format!("xlh_session={token}"))
                .body(Body::from("{\"holdings\":[]}")).unwrap()
        ).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        // 列表应有 1 条
        let app2 = super::router(state);
        let list = app2.oneshot(
            Request::builder().uri("/api/holdings/history")
                .header("cookie", format!("xlh_session={token}"))
                .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(list.into_body(), 1_000_000).await.unwrap();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }
```

> 说明：`store::open_in_memory`、`set_expiry`、`create_user`、`create_session` 均为既有 pub 函数。`AuthState`/`store` 通过 `crate::web::auth::*` 访问。

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib web::`
Expected: 全绿（含既有 web 测试 + 2 个新增）。

- [ ] **Step 6: Commit**

```bash
git add src/web/mod.rs
git commit -m "feat(history): Web 持仓建议保存/查询接口"
```

---

### Task 4: 管理端推送历史接口 + 后台页板块

**Files:**
- Modify: `src/web/auth/admin.rs`（2 个 handler + ADMIN_HTML 加「推送历史」板块）
- Modify: `src/web/auth/routes.rs`（admin_router 加 2 路由 + 非管理员 404 测试）

**Interfaces:**
- Consumes：`history::{list_push,get_push}`、`AuthState`。
- Produces：admin 组路由 `GET /api/admin/push-history`、`GET /api/admin/push-history/:id`。

- [ ] **Step 1: 加两个 handler**

在 `src/web/auth/admin.rs` 末尾（`ADMIN_HTML` 常量之前）追加：

```rust
pub async fn push_history_list(State(st): State<AuthState>) -> Response {
    let rows = {
        let conn = st.db.lock().unwrap();
        crate::history::list_push(&conn, 200).unwrap_or_default()
    };
    Json(rows).into_response()
}

pub async fn push_history_detail(
    State(st): State<AuthState>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let found = {
        let conn = st.db.lock().unwrap();
        crate::history::get_push(&conn, id).ok().flatten()
    };
    match found {
        Some(payload) => ([(axum::http::header::CONTENT_TYPE, "application/json")], payload).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}
```

（`State`/`Json`/`Response`/`StatusCode`/`IntoResponse` 已在 admin.rs 顶部引入；如缺 `IntoResponse` 则补 `use axum::response::IntoResponse;`。）

- [ ] **Step 2: admin_router 加路由**

在 `src/web/auth/routes.rs` 的 `admin_router()` 里追加两条：

```rust
        .route("/api/admin/push-history", get(admin::push_history_list))
        .route("/api/admin/push-history/:id", get(admin::push_history_detail))
```

- [ ] **Step 3: 后台页加「推送历史」板块**

在 `admin.rs` 的 `ADMIN_HTML` 里，「用户」表格 `<table id="users">...</table>` 之后、`<script>` 之前，插入：

```html
<h2>推送历史</h2>
<button onclick="loadPushHistory()">刷新推送历史</button>
<table id="pushhist"><thead><tr><th>时间</th><th>摘要</th><th></th></tr></thead><tbody></tbody></table>
<pre id="pushdetail" style="background:#1e293b;padding:10px;border-radius:8px;white-space:pre-wrap;display:none"></pre>
```

并在 `ADMIN_HTML` 的 `<script>` 内、初始化调用处（`ov();loadCodes('unused');loadUsers();` 一行）追加 `loadPushHistory();`，并加两个函数：

```javascript
async function loadPushHistory(){const j=await api('/api/admin/push-history');const tb=document.querySelector('#pushhist tbody');tb.innerHTML='';(j||[]).forEach(function(r){tb.innerHTML+=`<tr><td>${r.created_at}</td><td>${r.summary}</td><td><button onclick="showPush(${r.id})">详情</button></td></tr>`;});}
async function showPush(id){const r=await fetch('/api/admin/push-history/'+id);if(!r.ok){return;}const j=await r.json();const el=document.getElementById('pushdetail');el.style.display='block';el.textContent=JSON.stringify(j,null,2);}
```

（`api()` 是 ADMIN_HTML 里既有的 fetch 助手，遇 404 会优雅降级。）

- [ ] **Step 4: 非管理员 404 测试**

在 `src/web/auth/routes.rs` 的 tests 模块内追加（仿既有 `admin_route_hidden_for_non_admin`）：

```rust
    #[tokio::test]
    async fn push_history_hidden_for_non_admin() {
        let state = test_state();
        let token = "tok-ph".to_string();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "np", "h", false).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = crate::web::router(state);
        let resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/admin/push-history")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib web::`
Expected: 全绿（含新测试）。且 `cargo build` 通过（含 ADMIN_HTML 改动）。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/admin.rs src/web/auth/routes.rs
git commit -m "feat(history): 管理端推送历史接口与后台板块"
```

---

### Task 5: 推送端自动保存历史

**Files:**
- Modify: `src/push/job.rs`（抽 `build_message_full` + `BuiltMessage`；`run`/`run_forced` 加 `hist` 参数并保存）
- Modify: `src/push/schedule.rs`（`run_daemon` 加 `hist` 参数并透传）
- Modify: `src/push/mod.rs`（`run_once`/`run_daemon` 加 `hist` 参数）
- Modify: `src/main.rs`（Push 分支打开历史库并传入）

**Interfaces:**
- Consumes：`history::{open_or_default, save}`、`holdings::summarize`、`auth::config::load_auth`。
- Produces：`build_message_full(&PushConfig) -> Result<BuiltMessage>`；`job::run(&PushConfig, Option<&Connection>)`；`job::run_forced(&PushConfig, Option<&Connection>)`；`schedule::run_daemon(&PushConfig, Option<&Connection>)`；`push::run_once(&PushConfig, Option<&Connection>)`；`push::run_daemon(&PushConfig, Option<&Connection>)`。`build_message(&PushConfig) -> Result<(String,bool)>` 签名不变。

- [ ] **Step 1: job.rs 抽 build_message_full + 保存**

在 `src/push/job.rs`：顶部 `use` 追加：

```rust
use rusqlite::Connection;
```

把 `pub fn build_message(cfg: &PushConfig) -> Result<(String, bool)> {`（约第 34 行）整段的**函数签名与结尾**改造为返回结构体，并加薄包装（函数体中段不变，仅把结尾 `Ok((md, has_new))` 改为构造结构体；`input`、`report` 变量在函数中已存在）：

```rust
/// build_message 的完整产物，含基金持仓建议的输入与报告（供历史保存复用）。
pub struct BuiltMessage {
    pub md: String,
    pub has_new: bool,
    pub fund_input: HoldingsInput,
    pub fund_report: crate::holdings::HoldingsReport,
}

/// 组装完整推送消息并保留基金持仓输入/报告。
pub fn build_message_full(cfg: &PushConfig) -> Result<BuiltMessage> {
    // ……（原 build_message 函数体：从 cache_dir 到 let md = message::compose(...) 保持不变）……
    Ok(BuiltMessage { md, has_new, fund_input: input, fund_report: report })
}

/// 兼容既有调用点：只取 markdown 与 has_new。
pub fn build_message(cfg: &PushConfig) -> Result<(String, bool)> {
    let b = build_message_full(cfg)?;
    Ok((b.md, b.has_new))
}
```

> 落地方式：把现有 `build_message` 重命名为 `build_message_full`、返回类型改 `Result<BuiltMessage>`、把最后一行 `Ok((md, has_new))` 换成上面的结构体构造；再在其下新增薄包装 `build_message`。中间函数体（含 `let input = ...`、`let report = ...`、`let md = message::compose(...)`）一字不改。

在同文件加保存助手：

```rust
/// 把本次基金持仓建议存入历史（source=push, user_id=None）；失败仅告警。
fn save_push_history(hist: Option<&Connection>, b: &BuiltMessage) {
    let Some(conn) = hist else { return };
    if b.fund_report.advices.is_empty() { return; }
    let summary = crate::holdings::summarize(&b.fund_report);
    match serde_json::to_string(&serde_json::json!({ "input": &b.fund_input, "report": &b.fund_report })) {
        Ok(payload) => {
            if let Err(e) = crate::history::save(conn, None, "push", &summary, &payload) {
                eprintln!("保存推送历史失败：{e}");
            }
        }
        Err(e) => eprintln!("序列化推送历史失败：{e}"),
    }
}
```

把 `run` 与 `run_forced`（约第 107、118 行）改为：

```rust
pub fn run(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    let b = build_message_full(cfg)?;
    if cfg.schedule.only_on_new_data && !b.has_new {
        println!("无新数据，跳过推送");
        return Ok(());
    }
    save_push_history(hist, &b);
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}

pub fn run_forced(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    let b = build_message_full(cfg)?;
    save_push_history(hist, &b);
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}
```

- [ ] **Step 2: schedule.rs 透传 hist**

在 `src/push/schedule.rs`：`use super::config::PushConfig;` 下加 `use rusqlite::Connection;`。把 `run_daemon` 改为：

```rust
pub fn run_daemon(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    Schedule::from_str(&cfg.schedule.cron).map_err(|e| anyhow!("cron 非法: {e}"))?;
    println!("推送守护已启动，cron = {}（Ctrl+C 退出）", cfg.schedule.cron);
    loop {
        let now = Local::now();
        let next = next_after(&cfg.schedule.cron, &now)?;
        println!("下次推送：{}", next.format("%Y-%m-%d %H:%M:%S"));
        let wait = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
        std::thread::sleep(wait);
        match super::job::run(cfg, hist) {
            Ok(()) => println!("[{}] 推送完成", Local::now().format("%H:%M:%S")),
            Err(e) => eprintln!("[{}] 推送失败：{e}", Local::now().format("%H:%M:%S")),
        }
    }
}
```

- [ ] **Step 3: mod.rs 透传 hist**

在 `src/push/mod.rs`：顶部加 `use rusqlite::Connection;`。把两个函数改为：

```rust
pub fn run_once(cfg: &PushConfig, hist: Option<&Connection>) -> anyhow::Result<()> {
    job::run_forced(cfg, hist)
}

pub fn run_daemon(cfg: &PushConfig, hist: Option<&Connection>) -> anyhow::Result<()> {
    schedule::run_daemon(cfg, hist)
}
```

- [ ] **Step 4: main.rs 打开历史库并传入**

在 `src/main.rs` 的 `Some(Commands::Push { file, once }) => {` 分支（约第 84-86 行）改为：

```rust
        Some(Commands::Push { file, once }) => {
            let cfg = xlh::push::load(&file)?;
            let db_path = xlh::web::auth::config::load_auth(&cli.config).db_path;
            let hist = xlh::history::open_or_default(&db_path).ok();
            if once {
                xlh::push::run_once(&cfg, hist.as_ref())
            } else {
                xlh::push::run_daemon(&cfg, hist.as_ref())
            }
        }
```

- [ ] **Step 5: 加保存路径单测（job.rs）**

在 `src/push/job.rs` 末尾加（覆盖「空 advices 跳过保存」这一可离线验证的分支；非空保存路径由 `history::save` 的 Task 1 测试覆盖）：

```rust
#[cfg(test)]
mod push_history_tests {
    use super::*;
    use crate::holdings::{HoldingsInput, HoldingsReport, PortfolioSummary};

    fn empty_built() -> BuiltMessage {
        BuiltMessage {
            md: String::new(),
            has_new: false,
            fund_input: HoldingsInput { total_amount: None, total_profit: None, cumulative_profit: None, holdings: vec![] },
            fund_report: HoldingsReport {
                generated: "2026-07-05".into(),
                summary: PortfolioSummary {
                    total_amount: 0.0, total_profit: None, cumulative_profit: None,
                    holding_count: 0, total_add: 0.0, total_trim: 0.0, concentration_note: String::new(),
                },
                advices: vec![], skipped: vec![], disclaimer: String::new(),
            },
        }
    }

    #[test]
    fn empty_advices_are_not_saved() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        save_push_history(Some(&conn), &empty_built());
        assert_eq!(crate::history::list_push(&conn, 100).unwrap().len(), 0);
    }

    #[test]
    fn none_conn_is_noop() {
        // 不 panic 即可
        save_push_history(None, &empty_built());
    }
}
```

- [ ] **Step 6: 跑测试**

Run: `cargo test --lib push::`
Expected: 全绿（既有 push 测试 + 2 新增）。且 `cargo build` 通过（main.rs/mod.rs/schedule.rs 签名改动一致）。

- [ ] **Step 7: Commit**

```bash
git add src/push/job.rs src/push/schedule.rs src/push/mod.rs src/main.rs
git commit -m "feat(history): 定时推送自动保存持仓建议历史"
```

---

### Task 6: 前端——保存按钮 + 历史子区

**Files:**
- Modify: `src/web/page.rs`（持仓建议面板加「保存到历史」按钮与「历史」子区 + JS）

**Interfaces:**
- Consumes：`POST /api/holdings/save`、`GET /api/holdings/history`、`GET /api/holdings/history/:id`。

- [ ] **Step 1: 面板加 UI 元素**

在 `src/web/page.rs` 持仓建议面板里，`<div id="hd-result" style="margin-top:14px"></div>`（约第 259 行）**之后**、`</div></div>` 收尾之前插入：

```html
      <div id="hd-save-wrap" style="margin-top:10px;display:none">
        <button class="small" id="hd-save">保存到历史</button>
        <span id="hd-save-msg" style="margin-left:10px;color:#27ae60"></span>
      </div>
      <div style="margin-top:18px;border-top:1px solid #eee;padding-top:12px">
        <button class="small" id="hd-hist-load">查看历史建议</button>
        <div id="hd-hist" style="margin-top:10px"></div>
      </div>
```

- [ ] **Step 2: JS——生成后暂存 payload、显示保存按钮**

在 `src/web/page.rs` 的 run-holdings 点击处理（约第 821-825 行）中，把：

```javascript
  fetch('/api/holdings', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(renderHoldings)
    .catch(function(e){ document.getElementById('hd-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '生成建议'); });
```

改为：

```javascript
  fetch('/api/holdings', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(function(rep){
      renderHoldings(rep);
      window.hdLastPayload = payload;
      document.getElementById('hd-save-wrap').style.display = 'block';
      document.getElementById('hd-save-msg').textContent = '';
    })
    .catch(function(e){ document.getElementById('hd-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '生成建议'); });
```

- [ ] **Step 3: JS——保存按钮 + 历史列表/详情**

在 run-holdings 处理块（约第 826 行 `});`）**之后**追加：

```javascript
document.getElementById('hd-save').addEventListener('click', function(){
  if(!window.hdLastPayload){ return; }
  var btn = this; setBtn(btn, true, '保存到历史');
  fetch('/api/holdings/save', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(window.hdLastPayload)})
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(function(){ document.getElementById('hd-save-msg').textContent = '已保存到历史'; loadHdHistory(); })
    .catch(function(e){ document.getElementById('hd-save-msg').style.color='#c0392b'; document.getElementById('hd-save-msg').textContent = '保存失败：'+String(e.message||e); })
    .finally(function(){ setBtn(btn, false, '保存到历史'); });
});

function loadHdHistory(){
  fetch('/api/holdings/history')
    .then(function(res){ return res.ok ? res.json() : []; })
    .then(function(list){
      var box = document.getElementById('hd-hist');
      if(!list.length){ box.innerHTML = '<span class="hint">暂无历史记录</span>'; return; }
      var html = '<table style="width:100%;border-collapse:collapse">';
      list.forEach(function(r){
        html += '<tr style="border-bottom:1px solid #eee"><td style="padding:6px 8px;color:#7f8c8d">'+esc(r.created_at)+'</td><td style="padding:6px 8px">'+esc(r.summary)+'</td><td style="padding:6px 8px"><button class="small hd-hist-view" data-id="'+r.id+'">查看</button></td></tr>';
      });
      html += '</table>';
      box.innerHTML = html;
      box.querySelectorAll('.hd-hist-view').forEach(function(b){
        b.addEventListener('click', function(){ viewHdHistory(b.getAttribute('data-id')); });
      });
    });
}

function viewHdHistory(id){
  fetch('/api/holdings/history/'+id)
    .then(function(res){ if(!res.ok) throw new Error('记录不存在'); return res.json(); })
    .then(function(j){ renderHoldings(j.report); document.getElementById('hd-result').scrollIntoView({behavior:'smooth'}); })
    .catch(function(e){ document.getElementById('hd-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; });
}

document.getElementById('hd-hist-load').addEventListener('click', loadHdHistory);
```

> 说明：`setBtn`、`esc`、`renderHoldings` 均为 page.rs 既有 JS 助手；详情返回的 payload 结构为 `{input, report}`，`renderHoldings` 吃 `report` 部分。

- [ ] **Step 4: 编译并手动核对**

Run: `cargo build`
Expected: 通过。手动冒烟（可选）：`cargo run -- serve` → 登录激活用户 → 持仓建议生成 → 「保存到历史」→「查看历史建议」列表出现该条 → 点「查看」重渲染。

- [ ] **Step 5: Commit**

```bash
git add src/web/page.rs
git commit -m "feat(history): 持仓建议保存按钮与历史子区"
```

---

## Self-Review

**1. Spec coverage：**
- 两条来源都存 → Web（Task 3）+ 推送（Task 5）✅
- Web 手动保存按钮 → Task 6 ✅；后端重跑权威 → Task 3 save handler ✅
- 面板内历史子页 → Task 6 ✅
- 推送历史仅管理员 → Task 4（admin 组 + 非管理员 404 测试）✅
- SQLite `advice_history` 表 + 结构/接口 → Task 1 ✅
- 每用户 100 条上限 → Task 1（`web_retention_caps_at_100`）✅
- 完整时间戳 → Task 1（`now_ts`）✅
- payload = {input, report} → Task 3/Task 5（`serde_json::json!`）；HoldingsInput Serialize → Task 2 ✅
- summary 由 report 计算、Web/push 共用 → Task 2（`holdings::summarize`）✅
- 用户隔离测试 → Task 1（`user_isolation`）+ Task 3（按会话 user.id）✅
- serve 建表 → Task 3 Step 1 ✅
- push 从 [auth].db_path 开库、保存失败不阻断 → Task 5（`open_or_default` + `save_push_history` 仅告警）✅
- 既有 build_message 3 调用点不变 → Task 5（薄包装保留原签名）✅

**2. Placeholder scan：** 无 TBD/TODO；各步含完整代码。Task 5 Step 1 明确「函数体中段不变」并给出改造前后锚点，非占位。

**3. Type consistency：**
- `AdviceRecord{id,created_at,summary}` 全程一致（Task 1 定义，Task 3/4 返回）。
- `history::save(conn, Option<i64>, &str, &str, &str)`、`list_web(conn,i64,i64)`、`get_web(conn,i64,i64)`、`list_push(conn,i64)`、`get_push(conn,i64)`、`open_or_default(&Path)`、`migrate(&Connection)` 在 Task 3/4/5 调用签名一致。
- `holdings::summarize(&HoldingsReport)->String` Task 2 定义、Task 3/5 调用一致。
- `BuiltMessage{md,has_new,fund_input,fund_report}` Task 5 定义与使用一致；`build_message` 保留 `(String,bool)`。
- 路由 `/api/holdings/{save,history,history/:id}`、`/api/admin/push-history[/:id]` 前后端路径一致（Task 3/4/6）。

> 实现注意：Task 5 Step 1 是「重命名 + 改返回类型 + 加薄包装」，务必保持中段函数体逐字不变，避免误改 compose/sync 逻辑。

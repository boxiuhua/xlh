# 推送按用户隔离（多租户化）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把推送从「全局单文件单渠道」改为「每用户一份配置、各自 cron 定时投递到各自渠道」，Web 端每个授权用户自管，守护进程多租户遍历投递。

**Architecture:** 新增 `push_configs(user_id, config_json)` 表（`src/push/store.rs`），`PushConfig` 以 JSON 存。Web 推送路由从「管理员组」迁到「登录+授权组」，按 `CurrentUser` 隔离读写。守护进程 `xlh push` 改为读主库、遍历所有用户配置、按窗口判定各自 cron 到点投递。旧 `push.toml` 启动时导入首个管理员。

**Tech Stack:** Rust, axum, rusqlite (SQLite), serde_json, cron crate, tokio。

## Global Constraints

- **依赖顺序：先实施账户管理计划（`2026-07-06-account-management.md`）**。本计划 Task 5 引用 `User.cancelled`（账户管理 Task 1），Task 6 修改 `store::delete_user`（账户管理 Task 2）。Task 1–4 不依赖账户管理，可先行。
- 每用户配置存 `push_configs`，`PushConfig` 用 `serde_json` 序列化。
- Web 写入路径统一加固：`config::harden`（强制 `cache_dir` 为默认）+ `require_fixed_seconds`（cron 秒位必须固定数字）。
- 定时/手动推送落历史一律带 `user_id`（`history::save(conn, Some(uid), "push", …)`）。
- 慢速组装/网络发送**不得持有 DB 锁**（Web `test` 端点先锁外 build+send，再短锁存历史）。
- 守护仅对「授权放行」用户投递：`!disabled && !cancelled && LicenseStatus::allows_access()`。
- 测试命令：`cargo test <name>`。

## 文件结构

| 文件 | 职责 | 变更 |
|---|---|---|
| `src/push/store.rs` | push_configs 表 CRUD | 新建 |
| `src/push/config.rs` | 配置解析/校验 | 加 `harden`、`require_fixed_seconds` |
| `src/push/job.rs` | 单次任务编排 | `save_history` 带 user_id；`run`/`run_forced` 签名 |
| `src/push/schedule.rs` | 定时 | `due_users` 纯函数 + 多租户循环 |
| `src/push/mod.rs` | 模块导出 | `pub mod store`；守护入口签名 |
| `src/web/mod.rs` | 路由 + 处理器 | 推送迁到 licensed 组 + per-user 处理器 + 测试改造 |
| `src/web/auth/store.rs` | 用户库 | `first_admin_id`；`delete_user` 清 push_configs |
| `src/main.rs` | CLI | `push` 命令读 DB + 迁移；移除 `--file` |

---

### Task 1: `push::store` 模块 + `auth::store::first_admin_id`

**Files:**
- Create: `src/push/store.rs`
- Modify: `src/push/mod.rs`（`pub mod store;`）
- Modify: `src/web/auth/store.rs`（`first_admin_id`）

**Interfaces:**
- Produces:
  - `push::store::{migrate, upsert, get, delete, list_all, import_to_first_admin, migrate_legacy_push}`
  - `auth::store::first_admin_id(conn) -> Result<Option<i64>>`
  - 表 `push_configs(user_id PK, config_json, updated_at)`

- [ ] **Step 1: 写失败测试**（新建 `src/push/store.rs`，先只放 `mod tests`——但需要类型，故与实现同文件；本步创建文件含测试模块占位，实现在 Step 3）

先创建文件骨架含测试：

```rust
//! 每用户推送配置存储（push_configs 表）。
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::config::PushConfig;

// 实现见 Step 3

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::auth::store as auth_store;

    fn setup() -> Connection {
        // auth open_in_memory 建 users/sessions/codes；再建 push_configs
        let conn = auth_store::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn sample_cfg() -> PushConfig {
        let mut c = crate::push::config::default_config();
        c.channel.webhook = "https://open.feishu.cn/x".into();
        c.holdings = vec![crate::holdings::Holding { code: "161725".into(), amount: 1000.0, profit: 0.0 }];
        c
    }

    #[test]
    fn upsert_get_roundtrip() {
        let conn = setup();
        let uid = auth_store::create_user(&conn, "u", "h", false).unwrap();
        upsert(&conn, uid, &sample_cfg()).unwrap();
        let got = get(&conn, uid).unwrap().unwrap();
        assert_eq!(got.channel.webhook, "https://open.feishu.cn/x");
        // 覆盖更新
        let mut c2 = sample_cfg(); c2.channel.webhook = "https://x2".into();
        upsert(&conn, uid, &c2).unwrap();
        assert_eq!(get(&conn, uid).unwrap().unwrap().channel.webhook, "https://x2");
    }

    #[test]
    fn list_all_excludes_configless_and_orphans() {
        let conn = setup();
        let u1 = auth_store::create_user(&conn, "u1", "h", false).unwrap();
        auth_store::create_user(&conn, "u2", "h", false).unwrap(); // 无配置
        upsert(&conn, u1, &sample_cfg()).unwrap();
        // 孤儿：push_configs 指向不存在的 user_id
        conn.execute("INSERT INTO push_configs (user_id, config_json, updated_at) VALUES (9999, '{}', 'now')", []).unwrap();
        let all = list_all(&conn).unwrap();
        assert_eq!(all.len(), 1, "仅 u1 有配置且非孤儿");
        assert_eq!(all[0].0, u1);
    }

    #[test]
    fn delete_and_corrupt_json() {
        let conn = setup();
        let uid = auth_store::create_user(&conn, "u", "h", false).unwrap();
        upsert(&conn, uid, &sample_cfg()).unwrap();
        delete(&conn, uid).unwrap();
        assert!(get(&conn, uid).unwrap().is_none());
        // 损坏 JSON → None，不 panic
        conn.execute("INSERT INTO push_configs (user_id, config_json, updated_at) VALUES (?1, 'not-json', 'now')", [uid]).unwrap();
        assert!(get(&conn, uid).unwrap().is_none());
    }

    #[test]
    fn import_to_first_admin_is_idempotent() {
        let conn = setup();
        auth_store::create_user(&conn, "user", "h", false).unwrap(); // 非管理员，id 最小但非 admin
        let admin_lo = auth_store::create_user(&conn, "admin1", "h", true).unwrap();
        auth_store::create_user(&conn, "admin2", "h", true).unwrap();
        assert_eq!(auth_store::first_admin_id(&conn).unwrap(), Some(admin_lo));
        assert!(import_to_first_admin(&conn, &sample_cfg()).unwrap(), "首次导入");
        assert!(get(&conn, admin_lo).unwrap().is_some());
        assert!(!import_to_first_admin(&conn, &sample_cfg()).unwrap(), "已有配置不覆盖");
    }

    #[test]
    fn import_returns_false_without_admin() {
        let conn = setup();
        auth_store::create_user(&conn, "u", "h", false).unwrap();
        assert!(!import_to_first_admin(&conn, &sample_cfg()).unwrap());
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test upsert_get_roundtrip`
Expected: 编译失败——`migrate`/`upsert`/`get` 等未定义、`push::store` 未挂载。

- [ ] **Step 3: 实现存储函数**（`src/push/store.rs`，插到文件顶部 use 之后、`#[cfg(test)]` 之前）

```rust
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS push_configs (
  user_id     INTEGER PRIMARY KEY REFERENCES users(id),
  config_json TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建 push_configs 表失败")?;
    Ok(())
}

pub fn upsert(conn: &Connection, user_id: i64, cfg: &PushConfig) -> Result<()> {
    let json = serde_json::to_string(cfg).context("序列化推送配置失败")?;
    let now = chrono::Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO push_configs (user_id, config_json, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET config_json = ?2, updated_at = ?3",
        rusqlite::params![user_id, json, now],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, user_id: i64) -> Result<Option<PushConfig>> {
    let row: Option<String> = conn
        .query_row("SELECT config_json FROM push_configs WHERE user_id = ?1", [user_id], |r| r.get(0))
        .optional()?;
    Ok(row.and_then(|j| serde_json::from_str(&j).ok()))
}

pub fn delete(conn: &Connection, user_id: i64) -> Result<()> {
    conn.execute("DELETE FROM push_configs WHERE user_id = ?1", [user_id])?;
    Ok(())
}

/// JOIN users 排除孤儿行；损坏 JSON 行跳过。
pub fn list_all(conn: &Connection) -> Result<Vec<(i64, PushConfig)>> {
    let mut stmt = conn.prepare(
        "SELECT pc.user_id, pc.config_json FROM push_configs pc \
         JOIN users u ON u.id = pc.user_id ORDER BY pc.user_id",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|x| x.ok())
        .filter_map(|(uid, j)| serde_json::from_str::<PushConfig>(&j).ok().map(|c| (uid, c)))
        .collect();
    Ok(rows)
}

/// 若首个管理员尚无配置则导入 cfg，返回是否发生导入（幂等）。
pub fn import_to_first_admin(conn: &Connection, cfg: &PushConfig) -> Result<bool> {
    let Some(admin_id) = crate::web::auth::store::first_admin_id(conn)? else { return Ok(false); };
    if get(conn, admin_id)?.is_some() { return Ok(false); }
    upsert(conn, admin_id, cfg)?;
    Ok(true)
}

/// 启动迁移：旧全局 push.toml 存在且可解析 → 导入首个管理员（幂等）。
pub fn migrate_legacy_push(conn: &Connection, path: &std::path::Path) -> Result<()> {
    if !path.exists() { return Ok(()); }
    match super::config::load(path) {
        Ok(cfg) => {
            if import_to_first_admin(conn, &cfg)? {
                println!("已将 {} 导入首个管理员的推送配置", path.display());
            }
        }
        Err(e) => eprintln!("旧 push.toml 解析失败，跳过导入：{e}"),
    }
    Ok(())
}
```

- [ ] **Step 4: 挂载模块**（`src/push/mod.rs`，在 `pub mod config;` 附近）

```rust
pub mod store;
```

- [ ] **Step 5: 实现 `first_admin_id`**（`src/web/auth/store.rs`，`count_admins` 附近）

```rust
pub fn first_admin_id(conn: &Connection) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM users WHERE is_admin = 1 ORDER BY id LIMIT 1",
        [],
        |r| r.get(0),
    )
    .optional()
    .context("查询首个管理员失败")
}
```

- [ ] **Step 6: 运行测试**

Run: `cargo test upsert_get_roundtrip list_all_excludes_configless_and_orphans delete_and_corrupt_json import_to_first_admin_is_idempotent import_returns_false_without_admin`
Expected: 5 passed。

- [ ] **Step 7: Commit**

```bash
git add src/push/store.rs src/push/mod.rs src/web/auth/store.rs
git commit -m "feat(push): push_configs 每用户存储 + first_admin_id + 旧配置导入"
```

---

### Task 2: `config` 加固——`harden` + `require_fixed_seconds`

**Files:**
- Modify: `src/push/config.rs`（新增两函数 + 测试）

**Interfaces:**
- Produces: `config::harden(&mut PushConfig)`；`config::require_fixed_seconds(cron: &str) -> Result<()>`。

- [ ] **Step 1: 写失败测试**（`src/push/config.rs` 的 `mod tests` 追加）

```rust
    #[test]
    fn harden_forces_cache_dir() {
        let mut c = default_config();
        c.channel.cache_dir = PathBuf::from("/etc/evil");
        harden(&mut c);
        assert_eq!(c.channel.cache_dir, default_cache_dir());
    }

    #[test]
    fn fixed_seconds_accepts_and_rejects() {
        assert!(require_fixed_seconds("0 30 8 * * *").is_ok());
        assert!(require_fixed_seconds("30 0 12 * * *").is_ok());
        for bad in ["* 30 8 * * *", "*/5 30 8 * * *", "0-30 0 8 * * *", "1,2 0 8 * * *", ""] {
            assert!(require_fixed_seconds(bad).is_err(), "应拒绝 {bad}");
        }
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test harden_forces_cache_dir fixed_seconds_accepts_and_rejects`
Expected: 编译失败——函数未定义。

- [ ] **Step 3: 实现两函数**（`src/push/config.rs`，`validate` 之后）

```rust
/// 覆盖用户不可控字段：缓存目录固定为服务端默认，杜绝路径滥用。
pub fn harden(cfg: &mut PushConfig) {
    cfg.channel.cache_dir = default_cache_dir();
}

/// 要求 cron 秒位为固定数字（拒绝 * / 范围 / 列表 / 步进），避免每秒级狂刷。
pub fn require_fixed_seconds(cron: &str) -> Result<()> {
    let sec = cron.split_whitespace().next().unwrap_or("");
    if sec.is_empty() || !sec.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow!(
            "cron 秒位必须为固定值（不支持 * 或范围），以避免过于频繁的推送，当前为 '{sec}'"
        ));
    }
    Ok(())
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test harden_forces_cache_dir fixed_seconds_accepts_and_rejects`
Expected: 2 passed。

- [ ] **Step 5: Commit**

```bash
git add src/push/config.rs
git commit -m "feat(push): 配置加固 harden + cron 秒位固定校验"
```

---

### Task 3: `job` 落历史带 user_id

**Files:**
- Modify: `src/push/job.rs`（`save_history` 带 user_id + `run`/`run_forced` 签名 + 测试）
- Modify: `src/push/mod.rs`（`run_once` 签名 / 导出）
- Modify: `src/push/schedule.rs`（`run_daemon` 内 `job::run` 调用）
- Modify: `src/main.rs`（`run_once`/`run_daemon` 调用点）
- Modify: `src/web/mod.rs`（`push_test` 内 `run_once` 调用点）

**Interfaces:**
- Produces: `job::save_history(conn: &Connection, user_id: Option<i64>, b: &BuiltMessage)`；`job::run(cfg, hist: Option<&Connection>, user_id: Option<i64>)`；`job::run_forced(cfg, hist, user_id)`；`mod::run_once(cfg, hist, user_id)`。

- [ ] **Step 1: 改测试**（`src/push/job.rs` 的 `mod push_history_tests`：把 `empty_advices_are_not_saved` 改用新签名，删除 `none_conn_is_noop`）

```rust
    #[test]
    fn empty_advices_are_not_saved() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        save_history(&conn, None, &empty_built());
        assert_eq!(crate::history::list_push(&conn, 100).unwrap().len(), 0);
    }
```

（删除原 `none_conn_is_noop` 测试。）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test empty_advices_are_not_saved`
Expected: 编译失败——`save_history` 未定义。

- [ ] **Step 3: 改 `job.rs`**（把私有 `save_push_history(hist: Option<&Connection>, b)` 替换为公开 `save_history(conn, user_id, b)`，并改 `run`/`run_forced`）

```rust
/// 把本次基金持仓建议存入历史（source=push）。advices 为空则不存；失败仅告警。
pub fn save_history(conn: &Connection, user_id: Option<i64>, b: &BuiltMessage) {
    if b.fund_report.advices.is_empty() { return; }
    let summary = crate::holdings::summarize(&b.fund_report);
    match serde_json::to_string(&serde_json::json!({ "input": &b.fund_input, "report": &b.fund_report })) {
        Ok(payload) => {
            if let Err(e) = crate::history::save(conn, user_id, "push", &summary, &payload) {
                eprintln!("保存推送历史失败：{e}");
            }
        }
        Err(e) => eprintln!("序列化推送历史失败：{e}"),
    }
}

pub fn run(cfg: &PushConfig, hist: Option<&Connection>, user_id: Option<i64>) -> Result<()> {
    let b = build_message_full(cfg)?;
    if cfg.schedule.only_on_new_data && !b.has_new {
        println!("无新数据，跳过推送");
        return Ok(());
    }
    if let Some(conn) = hist { save_history(conn, user_id, &b); }
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}

pub fn run_forced(cfg: &PushConfig, hist: Option<&Connection>, user_id: Option<i64>) -> Result<()> {
    let b = build_message_full(cfg)?;
    if let Some(conn) = hist { save_history(conn, user_id, &b); }
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}
```

- [ ] **Step 4: 改 `mod.rs`**（`run_once` 加 user_id；`run_daemon` 暂留但透传 None）

```rust
/// 立即跑一次任务（手动触发，强制发送，忽略 only_on_new_data）。
pub fn run_once(cfg: &PushConfig, hist: Option<&Connection>, user_id: Option<i64>) -> anyhow::Result<()> {
    job::run_forced(cfg, hist, user_id)
}

/// 单配置按 cron 常驻守护（本任务暂留，Task 5 由多租户版取代）。
pub fn run_daemon(cfg: &PushConfig, hist: Option<&Connection>) -> anyhow::Result<()> {
    schedule::run_daemon(cfg, hist)
}
```

- [ ] **Step 5: 改各调用点**

`src/push/schedule.rs` 第 30 行 `super::job::run(cfg, hist)` → `super::job::run(cfg, hist, None)`。

`src/main.rs` push 分支：

```rust
            if once {
                xlh::push::run_once(&cfg, hist.as_ref(), None)
            } else {
                xlh::push::run_daemon(&cfg, hist.as_ref())
            }
```

`src/web/mod.rs` `push_test` 内 `crate::push::run_once(&cfg, None)` → `crate::push::run_once(&cfg, None, None)`。

- [ ] **Step 6: 运行测试**

Run: `cargo test empty_advices_are_not_saved && cargo build`
Expected: 测试通过，全项目编译通过。

- [ ] **Step 7: Commit**

```bash
git add src/push/job.rs src/push/mod.rs src/push/schedule.rs src/main.rs src/web/mod.rs
git commit -m "feat(push): 推送历史按 user_id 记录（save_history/run 签名）"
```

---

### Task 4: Web 推送路由迁到「登录+授权」组，按用户隔离

**Files:**
- Modify: `src/web/mod.rs`（处理器改写、路由迁移、`serve` 迁移、测试改造）

**Interfaces:**
- Consumes: `push::store::{get,upsert,migrate}`、`config::{validate,harden,require_fixed_seconds}`、`job::{build_message_full,save_history}`、`channels::send`、`CurrentUser`。
- Produces: `/api/push/{config,preview,test}` 挂在 licensed 组，按 `CurrentUser` 隔离。

- [ ] **Step 1: 改测试**（`src/web/mod.rs` 的 `mod tests`：改写两个既有 push 测试并新增，均走生产 `router` + 已授权会话）

先在 tests 模块加辅助（放到 `post_json` 附近）：

```rust
    fn push_state() -> crate::web::auth::AuthState {
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        crate::push::store::migrate(&conn).unwrap();
        crate::web::auth::AuthState::new(conn, Default::default())
    }
    /// 造一个已授权用户 + 会话，返回 token。
    fn seed_licensed(state: &crate::web::auth::AuthState, name: &str, token: &str) -> i64 {
        let c = state.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, name, "h", false).unwrap();
        let now = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, now + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, token, uid, now + chrono::Duration::days(1)).unwrap();
        uid
    }
    async fn push_post(state: crate::web::auth::AuthState, uri: &str, token: &str, body: serde_json::Value) -> axum::http::StatusCode {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        super::router(state).oneshot(
            Request::builder().method("POST").uri(uri)
                .header("content-type", "application/json")
                .header("cookie", format!("xlh_session={token}"))
                .body(Body::from(body.to_string())).unwrap()
        ).await.unwrap().status()
    }
```

改写既有两测试 + 新增：

```rust
    #[tokio::test]
    async fn push_config_get_returns_default_for_new_user() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let state = push_state();
        seed_licensed(&state, "pu", "ptok");
        let resp = super::router(state).oneshot(
            Request::builder().uri("/api/push/config")
                .header("cookie", "xlh_session=ptok").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["schedule"]["cron"].is_string());
        assert!(v["channel"]["kind"].is_string());
    }

    #[tokio::test]
    async fn push_config_requires_auth() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        // 未登录 → 401
        let state = push_state();
        let r1 = super::router(state).oneshot(
            Request::builder().uri("/api/push/config").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(r1.status(), axum::http::StatusCode::UNAUTHORIZED);
        // 已登录未授权 → 403
        let state = push_state();
        {
            let c = state.db.lock().unwrap();
            let uid = crate::web::auth::store::create_user(&c, "np", "h", false).unwrap();
            let now = chrono::Local::now().date_naive();
            crate::web::auth::store::create_session(&c, "ntok", uid, now + chrono::Duration::days(1)).unwrap();
        }
        let r2 = super::router(state).oneshot(
            Request::builder().uri("/api/push/config").header("cookie", "xlh_session=ntok").body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(r2.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn push_config_save_empty_webhook_is_400() {
        let state = push_state();
        seed_licensed(&state, "pu", "ptok");
        let body = serde_json::json!({
            "schedule": {"cron": "0 30 8 * * *"},
            "channel": {"kind": "feishu", "webhook": ""},
            "holdings": [{"code": "161725", "amount": 1000.0, "profit": 0.0}]
        });
        assert_eq!(push_post(state, "/api/push/config", "ptok", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn push_config_rejects_wildcard_seconds() {
        let state = push_state();
        seed_licensed(&state, "pu", "ptok");
        let body = serde_json::json!({
            "schedule": {"cron": "* 30 8 * * *"},
            "channel": {"kind": "feishu", "webhook": "https://open.feishu.cn/x"},
            "holdings": [{"code": "161725", "amount": 1000.0, "profit": 0.0}]
        });
        assert_eq!(push_post(state, "/api/push/config", "ptok", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn push_config_save_then_get_and_cache_dir_hardened() {
        use axum::body::Body; use axum::http::Request; use tower::ServiceExt;
        let state = push_state();
        seed_licensed(&state, "pu", "ptok");
        let body = serde_json::json!({
            "schedule": {"cron": "0 30 8 * * *"},
            "channel": {"kind": "feishu", "webhook": "https://open.feishu.cn/x", "cache_dir": "/etc/evil"},
            "holdings": [{"code": "161725", "amount": 1000.0, "profit": 0.0}]
        });
        assert_eq!(push_post(state.clone(), "/api/push/config", "ptok", body).await, axum::http::StatusCode::OK);
        let resp = super::router(state).oneshot(
            Request::builder().uri("/api/push/config").header("cookie", "xlh_session=ptok").body(Body::empty()).unwrap()
        ).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["channel"]["webhook"], "https://open.feishu.cn/x", "读回一致");
        assert_eq!(v["channel"]["cache_dir"], ".cache", "cache_dir 被服务端覆盖");
    }
```

删除原 `push_config_get_returns_json`（被 `push_config_get_returns_default_for_new_user` 取代）。`index_has_push_tab` 保持不变（仍走 `core_router` 取 `/`）。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test push_config_get_returns_default_for_new_user`
Expected: 失败——路由仍在 admin 组 / 处理器未按用户隔离（401 或 404）。

- [ ] **Step 3: 改处理器**（`src/web/mod.rs`，把 `push_config_get/save/preview/test` 四个函数整体替换；删除 `const PUSH_TOML`）

```rust
async fn push_config_get(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
) -> axum::Json<crate::push::PushConfig> {
    let cfg = {
        let conn = st.db.lock().unwrap();
        crate::push::store::get(&conn, user.id).ok().flatten()
    }
    .unwrap_or_else(crate::push::config::default_config);
    axum::Json(cfg)
}

async fn push_config_save(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::Json(mut cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    crate::push::config::validate(&cfg)?;
    crate::push::config::require_fixed_seconds(&cfg.schedule.cron)?;
    crate::push::config::harden(&mut cfg);
    let conn = st.db.lock().unwrap();
    crate::push::store::upsert(&conn, user.id, &cfg)?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

async fn push_preview(
    axum::Json(mut cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    crate::push::config::harden(&mut cfg);
    let (md, has_new) = tokio::task::spawn_blocking(move || crate::push::build_message(&cfg))
        .await.map_err(|e| anyhow!("任务执行失败: {e}"))??;
    Ok(axum::Json(serde_json::json!({"markdown": md, "has_new": has_new})))
}

async fn push_test(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::Json(mut cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    crate::push::config::validate(&cfg)?;
    crate::push::config::require_fixed_seconds(&cfg.schedule.cron)?;
    crate::push::config::harden(&mut cfg);
    let uid = user.id;
    // 锁外组装 + 网络发送，避免持库锁阻塞其它请求；成功后再短锁存历史。
    let sent = tokio::task::spawn_blocking(move || {
        let b = crate::push::job::build_message_full(&cfg)?;
        crate::push::channels::send(&cfg.channel, "基金持仓建议", &b.md)?;
        anyhow::Ok(b)
    })
    .await
    .map_err(|e| anyhow!("任务执行失败: {e}"))?;
    match sent {
        Ok(b) => {
            let conn = st.db.lock().unwrap();
            crate::push::job::save_history(&conn, Some(uid), &b);
            Ok(axum::Json(serde_json::json!({"ok": true})))
        }
        Err(e) => Ok(axum::Json(serde_json::json!({"ok": false, "error": e.to_string()}))),
    }
}
```

- [ ] **Step 4: 迁移路由**（`src/web/mod.rs`）

删除泛型 `push_routes` 函数（第 286–296 行整块），改为具体类型：

```rust
/// 每用户推送配置路由（挂在 licensed 组，按 CurrentUser 隔离）。
fn push_user_routes() -> Router<auth::AuthState> {
    Router::new()
        .route("/api/push/config", get(push_config_get).post(push_config_save))
        .route("/api/push/preview", post(push_preview))
        .route("/api/push/test", post(push_test))
}
```

`core_router`（测试用）删除 `.merge(push_routes::<()>())`：

```rust
#[cfg(test)]
pub(crate) fn core_router() -> Router {
    core_routes::<()>()
        .route("/", get(index_page))
}
```

licensed 组加入 push；admin 组移除 push：

```rust
    // 需登录 + 授权：核心业务 + 每用户推送
    let licensed = core_routes::<AuthState>()
        .merge(holdings_history_routes())
        .merge(push_user_routes())
        .route_layer(from_fn_with_state(state.clone(), auth::require_license))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    // 需登录 + 管理员：后台（不再含推送配置）
    let admin = auth::routes::admin_router()
        .route_layer(from_fn_with_state(state.clone(), auth::require_admin))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));
```

- [ ] **Step 5: `serve` 建表 + 迁移旧配置**（`src/web/mod.rs::serve`，`history::migrate` 之后）

```rust
    crate::history::migrate(&conn).context("建历史表失败")?;
    crate::push::store::migrate(&conn).context("建推送配置表失败")?;
    crate::push::store::migrate_legacy_push(&conn, std::path::Path::new("push.toml")).ok();
```

- [ ] **Step 6: 运行测试**

Run: `cargo test push_config_get_returns_default_for_new_user push_config_requires_auth push_config_save_empty_webhook_is_400 push_config_rejects_wildcard_seconds push_config_save_then_get_and_cache_dir_hardened index_has_push_tab`
Expected: 全部 PASS。再 `cargo build` 确认无遗留引用 `push_routes`/`PUSH_TOML`。

- [ ] **Step 7: Commit**

```bash
git add src/web/mod.rs
git commit -m "feat(push): 推送配置迁到登录+授权组，按用户隔离读写"
```

---

### Task 5: 多租户定时守护

**Files:**
- Modify: `src/push/schedule.rs`（`due_users` 纯函数 + `run_multi` + `run_all_once`；移除单配置 `run_daemon`）
- Modify: `src/push/mod.rs`（导出 `run_multi_daemon`/`run_all_once`；移除单配置 `run_daemon`/旧 `run_once` 视需要保留）
- Modify: `src/main.rs`（`push` 命令读 DB + 迁移；`Push` 去掉 `--file`）

**依赖：** 账户管理 Task 1（`User.cancelled`）必须已实现。

**Interfaces:**
- Consumes: `store::list_all`、`auth::store::find_user_by_id`、`auth::model::LicenseStatus`、`job::{run,run_forced}`、`schedule::next_after`。
- Produces: `schedule::due_users(configs, last_tick, now) -> Vec<i64>`；`mod::run_multi_daemon(conn, warn, grace)`；`mod::run_all_once(conn, warn, grace)`。

- [ ] **Step 1: 写失败测试**（`src/push/schedule.rs` 的 `mod tests` 追加）

```rust
    use chrono::Local;

    #[test]
    fn due_users_window_hit_and_miss() {
        // 每天 08:30:00 触发
        let cfgs = vec![(1i64, "0 30 8 * * *".to_string()), (2i64, "0 0 9 * * *".to_string())];
        let last = Local.with_ymd_and_hms(2026, 1, 1, 8, 29, 0).unwrap();
        let now  = Local.with_ymd_and_hms(2026, 1, 1, 8, 31, 0).unwrap();
        assert_eq!(due_users(&cfgs, last, now), vec![1], "仅 08:30 落在窗口内");
        // 窗口内无触发
        let last2 = Local.with_ymd_and_hms(2026, 1, 1, 8, 31, 0).unwrap();
        let now2  = Local.with_ymd_and_hms(2026, 1, 1, 8, 32, 0).unwrap();
        assert!(due_users(&cfgs, last2, now2).is_empty());
    }
```

（`Local` 与 `TimeZone` 已在文件顶部 `use chrono::{DateTime, Local, TimeZone};`。）

- [ ] **Step 2: 运行确认失败**

Run: `cargo test due_users_window_hit_and_miss`
Expected: 失败——`due_users` 未定义。

- [ ] **Step 3: 实现 `due_users` + 多租户循环**（`src/push/schedule.rs`，替换原 `run_daemon`）

```rust
use rusqlite::Connection;
use crate::web::auth::model::LicenseStatus;

/// 纯函数：给定各用户 (uid, cron) 与时间窗口 (last_tick, now]，返回本轮应触发的 uid。
pub fn due_users(configs: &[(i64, String)], last_tick: DateTime<Local>, now: DateTime<Local>) -> Vec<i64> {
    configs
        .iter()
        .filter_map(|(uid, cron)| match next_after(cron, &last_tick) {
            Ok(t) if t <= now => Some(*uid),
            _ => None,
        })
        .collect()
}

/// 该用户是否允许被投递：启用、未注销、且授权放行。
fn user_allowed(conn: &Connection, uid: i64, today: chrono::NaiveDate, warn: i64, grace: i64) -> bool {
    match crate::web::auth::store::find_user_by_id(conn, uid) {
        Ok(Some(u)) => {
            !u.disabled
                && !u.cancelled
                && LicenseStatus::of(u.expires_at, today, warn, grace).allows_access()
        }
        _ => false,
    }
}

/// 多租户守护：每 60s 一轮，对窗口内到点且授权放行的用户投递。单用户失败仅记日志。
pub fn run_multi(conn: &Connection, warn: i64, grace: i64) -> Result<()> {
    println!("多用户推送守护已启动（Ctrl+C 退出）");
    let mut last_tick = Local::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let now = Local::now();
        let all = super::store::list_all(conn).unwrap_or_default();
        let crons: Vec<(i64, String)> = all.iter().map(|(u, c)| (*u, c.schedule.cron.clone())).collect();
        for uid in due_users(&crons, last_tick, now) {
            if !user_allowed(conn, uid, now.date_naive(), warn, grace) { continue; }
            if let Some((_, cfg)) = all.iter().find(|(u, _)| *u == uid) {
                if let Err(e) = super::job::run(cfg, Some(conn), Some(uid)) {
                    eprintln!("用户 {uid} 推送失败：{e}");
                }
            }
        }
        last_tick = now;
    }
}

/// 对所有授权放行用户强制跑一次（忽略 only_on_new_data），用于 `xlh push --once`。
pub fn run_all_once(conn: &Connection, warn: i64, grace: i64) -> Result<()> {
    let today = Local::now().date_naive();
    for (uid, cfg) in super::store::list_all(conn).unwrap_or_default() {
        if !user_allowed(conn, uid, today, warn, grace) { continue; }
        if let Err(e) = super::job::run_forced(&cfg, Some(conn), Some(uid)) {
            eprintln!("用户 {uid} 推送失败：{e}");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: 改 `mod.rs` 导出**（`src/push/mod.rs`：移除单配置 `run_daemon`；`run_once` 保留供内部/测试）

```rust
use rusqlite::Connection;

pub fn run_once(cfg: &PushConfig, hist: Option<&Connection>, user_id: Option<i64>) -> anyhow::Result<()> {
    job::run_forced(cfg, hist, user_id)
}

/// 多用户 cron 守护。
pub fn run_multi_daemon(conn: &Connection, warn_days: i64, grace_days: i64) -> anyhow::Result<()> {
    schedule::run_multi(conn, warn_days, grace_days)
}

/// 对所有授权用户强制跑一次。
pub fn run_all_once(conn: &Connection, warn_days: i64, grace_days: i64) -> anyhow::Result<()> {
    schedule::run_all_once(conn, warn_days, grace_days)
}
```

（删除原 `pub fn run_daemon(cfg, hist)`。）

- [ ] **Step 5: 改 `main.rs` push 命令**（`Push` 去掉 `--file`，读 DB + 迁移）

`Commands::Push` 定义改为：

```rust
    /// 定时推送持仓建议（多用户；读主库中各用户配置）
    Push {
        /// 立即对所有授权用户跑一次即退出（否则按各自 cron 常驻守护）
        #[arg(long)]
        once: bool,
    },
```

分支改为：

```rust
        Some(Commands::Push { once }) => {
            let auth_cfg = xlh::web::auth::config::load_auth(&cli.config);
            let conn = xlh::web::auth::store::open(&auth_cfg.db_path)?;
            xlh::history::migrate(&conn)?;
            xlh::push::store::migrate(&conn)?;
            xlh::push::store::migrate_legacy_push(&conn, std::path::Path::new("push.toml")).ok();
            if once {
                xlh::push::run_all_once(&conn, auth_cfg.warn_days, auth_cfg.grace_days)
            } else {
                xlh::push::run_multi_daemon(&conn, auth_cfg.warn_days, auth_cfg.grace_days)
            }
        }
```

- [ ] **Step 6: 运行测试 + 编译**

Run: `cargo test due_users_window_hit_and_miss next_after_daily_noon && cargo build`
Expected: 测试通过（含既有 `next_after_*`）；全项目编译通过（无残留 `run_daemon` 调用）。

- [ ] **Step 7: Commit**

```bash
git add src/push/schedule.rs src/push/mod.rs src/main.rs
git commit -m "feat(push): 多租户定时守护（遍历用户配置按各自 cron 投递）"
```

---

### Task 6: 删号级联清理 `push_configs`

**Files:**
- Modify: `src/web/auth/store.rs`（`delete_user` 事务内清 push_configs）
- Modify: `src/push/store.rs`（`mod tests` 加级联删除测试）

**依赖：** 账户管理 Task 2（`store::delete_user`）必须已实现。

**Interfaces:**
- Consumes: `store::delete_user`（账户管理）。

- [ ] **Step 1: 写失败测试**（`src/push/store.rs` 的 `mod tests` 追加）

```rust
    #[test]
    fn delete_user_cascades_push_config() {
        let mut conn = setup();
        let uid = auth_store::create_user(&conn, "u", "h", false).unwrap();
        upsert(&conn, uid, &sample_cfg()).unwrap();
        auth_store::delete_user(&mut conn, uid).unwrap();
        assert!(get(&conn, uid).unwrap().is_none(), "删号应连带清除推送配置");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test delete_user_cascades_push_config`
Expected: 失败——`delete_user` 未清 push_configs（`get` 仍返回 Some）。

- [ ] **Step 3: `delete_user` 加级联删除**（`src/web/auth/store.rs::delete_user` 事务内，删 sessions 之后、删 users 之前）

```rust
pub fn delete_user(conn: &mut Connection, user_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    // push_configs 可能尚未建表（如纯 auth 测试）；容错删除。
    let _ = tx.execute("DELETE FROM push_configs WHERE user_id = ?1", [user_id]);
    tx.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
    tx.commit()?;
    Ok(())
}
```

- [ ] **Step 4: 运行测试**

Run: `cargo test delete_user_cascades_push_config && cargo test`
Expected: 该测试通过；全量 `cargo test` 通过。

- [ ] **Step 5: Commit**

```bash
git add src/web/auth/store.rs src/push/store.rs
git commit -m "feat(push): 删号级联清理 push_configs"
```

---

## Self-Review

**Spec 覆盖核对：**
- push_configs 表 + JSON 存储 → Task 1 ✅
- `list_all` JOIN 排孤儿 → Task 1 ✅
- 旧 push.toml 导入首个管理员（幂等）→ Task 1（`import_to_first_admin`/`migrate_legacy_push`）+ Task 4/5 启动调用 ✅
- 加固 harden + cron 秒位固定 → Task 2，Web 写入路径调用 → Task 4 ✅
- 历史按 user_id → Task 3 ✅
- Web 路由迁 licensed 组 + per-user 读写 + preview/test → Task 4 ✅
- 未登录 401 / 未授权 403 → Task 4 测试 ✅
- 多租户守护（窗口判定 + 授权放行过滤）→ Task 5 ✅
- `xlh push` 读 DB + 移除 --file + 启动迁移 → Task 5 ✅
- 删号级联清理 → Task 6 ✅

**类型一致性：** `save_history(&Connection, Option<i64>, &BuiltMessage)`、`run/run_forced(cfg, Option<&Connection>, Option<i64>)`、`run_once(cfg, hist, user_id)`、`store::list_all -> Vec<(i64, PushConfig)>`、`due_users(&[(i64,String)], DateTime<Local>, DateTime<Local>) -> Vec<i64>`、`run_multi_daemon/run_all_once(&Connection, i64, i64)` 各任务间一致。

**Placeholder 扫描：** 无 TBD/TODO；每步给出完整代码与预期命令输出。

**依赖提示：** Task 5（`User.cancelled`）与 Task 6（`delete_user`）依赖账户管理计划，须先实施账户管理；Task 1–4 可独立先行。

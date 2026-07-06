# 用户账户管理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 xlh SaaS 增加用户自助改密、管理员重置密码、账号注销/删除（两级规则）、未激活账号注册上限 10000。

**Architecture:** 沿用现有 `src/web/auth/` 分层——`store.rs`（SQLite）+ `model.rs`（数据结构）+ `handlers.rs`（用户 API）+ `admin.rs`（管理 API+后台 HTML）+ `routes.rs`（管理路由与集成测试）+ `mod.rs`（中间件与 AuthState）+ `page.rs`（主界面）。新增一列 `users.cancelled_at` 表达「已注销」独立状态；访问控制层把「已注销」与「封禁」同等拦截。

**Tech Stack:** Rust, axum, rusqlite (SQLite), argon2, tokio, tower（测试用 `oneshot`）。

## Global Constraints

- 密码最短 **6 位**（字符计数 `chars().count()`），与现有 register 一致。
- 慢速 argon2 校验/哈希期间**不得持有 DB 锁**（先取数释放锁，锁外校验，再短暂持锁写库）——沿用 `login` 模式。
- 未激活账号注册上限：**10000**（口径 `expires_at IS NULL AND cancelled_at IS NULL`）。
- 「已注销」= 不可登录、不可用（会话失效）；与「封禁」语义独立但访问效果相同。
- 末位管理员保护：不可注销/删除唯一「启用中」管理员，复用 `count_admins`。
- 错误码：`invalid_password`(400) / `wrong_password`(400) / `registration_full`(403) / `must_cancel_first`(400) / `last_admin`(400) / `user_not_found`(404) / `hash_failed`(500) / `update_failed`(500)。
- 测试命令统一：`cargo test <name>`（Windows PowerShell 下直接运行）。

## 文件结构

| 文件 | 职责 | 变更 |
|---|---|---|
| `src/web/auth/store.rs` | SQLite schema/迁移/CRUD | 加列+迁移+新函数+单测 |
| `src/web/auth/model.rs` | `User` 结构 | 加 `cancelled` 字段 |
| `src/web/auth/mod.rs` | `CurrentUser`、`require_login` | 加 `cancelled`、拦截已注销 |
| `src/web/auth/handlers.rs` | 用户 API | `change_password`、register 上限、login 拦截已注销 |
| `src/web/auth/admin.rs` | 管理 API + 后台 HTML | `reset_password`/`cancel_user`/`delete_user`、list_users 加 cancelled、后台按钮 |
| `src/web/auth/routes.rs` | 管理路由 + 集成测试 | 3 条新路由 + 测试 |
| `src/web/mod.rs` | 路由组装 | authed 组加 change_password 路由 |
| `src/web/page.rs` | 主界面 | 改密模态 UI |

---

### Task 1: 数据模型——`cancelled_at` 列 + 迁移 + `User.cancelled` + `CurrentUser.cancelled`

**Files:**
- Modify: `src/web/auth/store.rs`（SCHEMA、`migrate`、三处读查询、测试）
- Modify: `src/web/auth/model.rs`（`User`）
- Modify: `src/web/auth/mod.rs`（`CurrentUser`、`From<User>`）

**Interfaces:**
- Produces: `User { …, cancelled: bool }`；`CurrentUser { …, cancelled: bool }`；`store::migrate` 对旧库幂等加 `cancelled_at`。

- [ ] **Step 1: 写失败测试**（`src/web/auth/store.rs` 的 `mod tests` 末尾追加）

```rust
    #[test]
    fn migration_adds_cancelled_column_to_legacy_db() {
        // 旧库 schema：不含 cancelled_at
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, \
             pw_hash TEXT NOT NULL, expires_at TEXT, is_admin INTEGER NOT NULL DEFAULT 0, \
             disabled INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL);",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let id = create_user(&conn, "u", "h", false).unwrap();
        let u = find_user_by_id(&conn, id).unwrap().unwrap();
        assert!(!u.cancelled, "新用户默认未注销");
    }

    #[test]
    fn cancelled_flag_reflects_column() {
        let conn = open_in_memory().unwrap();
        let id = create_user(&conn, "c", "h", false).unwrap();
        conn.execute("UPDATE users SET cancelled_at = '2026-07-06' WHERE id = ?1", [id]).unwrap();
        assert!(find_user_by_id(&conn, id).unwrap().unwrap().cancelled);
        let (_, _, u) = find_user_by_name(&conn, "c").unwrap().unwrap();
        assert!(u.cancelled);
        assert!(list_users(&conn).unwrap().iter().find(|x| x.id == id).unwrap().cancelled);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test cancelled_flag_reflects_column`
Expected: 编译失败——`User` 无字段 `cancelled`。

- [ ] **Step 3: 加 `cancelled` 到 `User`**（`src/web/auth/model.rs`，`struct User`）

```rust
#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub expires_at: Option<NaiveDate>,
    pub is_admin: bool,
    pub disabled: bool,
    pub cancelled: bool,
}
```

- [ ] **Step 4: SCHEMA 加列 + 迁移**（`src/web/auth/store.rs`）

在 SCHEMA 常量的 `users` 表 `disabled` 行后加 `cancelled_at TEXT,`：

```rust
CREATE TABLE IF NOT EXISTS users (
  id         INTEGER PRIMARY KEY,
  username   TEXT NOT NULL UNIQUE,
  pw_hash    TEXT NOT NULL,
  expires_at TEXT,
  is_admin   INTEGER NOT NULL DEFAULT 0,
  disabled   INTEGER NOT NULL DEFAULT 0,
  cancelled_at TEXT,
  created_at TEXT NOT NULL
);
```

把 `migrate` 改为在建表后幂等加列，并新增 `ensure_column` 辅助：

```rust
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建表失败")?;
    ensure_column(conn, "users", "cancelled_at", "TEXT")?;
    Ok(())
}

/// 幂等加列：仅当目标列不存在时执行 ALTER TABLE（SQLite 无 IF NOT EXISTS 语法）。
fn ensure_column(conn: &Connection, table: &str, col: &str, ty: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == col);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {ty}"), [])?;
    }
    Ok(())
}
```

- [ ] **Step 5: 三处读查询补 `cancelled`**（`src/web/auth/store.rs`）

`find_user_by_name`：SELECT 末尾加 `, cancelled_at`（成为列 6），闭包内加字段：

```rust
    conn.query_row(
        "SELECT id, username, pw_hash, expires_at, is_admin, disabled, cancelled_at FROM users WHERE username = ?1",
        [username],
        |r| {
            let id: i64 = r.get(0)?;
            let pw_hash: String = r.get(2)?;
            let user = User {
                id,
                username: r.get(1)?,
                expires_at: parse_date(r.get(3)?),
                is_admin: r.get::<_, i64>(4)? != 0,
                disabled: r.get::<_, i64>(5)? != 0,
                cancelled: r.get::<_, Option<String>>(6)?.is_some(),
            };
            Ok((id, pw_hash, user))
        },
    )
    .optional()
    .context("查询用户失败")
```

`find_user_by_id`：SELECT 加 `, cancelled_at`（列 5）：

```rust
    conn.query_row(
        "SELECT id, username, expires_at, is_admin, disabled, cancelled_at FROM users WHERE id = ?1",
        [id],
        |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                expires_at: parse_date(r.get(2)?),
                is_admin: r.get::<_, i64>(3)? != 0,
                disabled: r.get::<_, i64>(4)? != 0,
                cancelled: r.get::<_, Option<String>>(5)?.is_some(),
            })
        },
    )
    .optional()
    .context("查询用户失败")
```

`list_users`：SELECT 加 `, cancelled_at`（列 5）：

```rust
    let mut stmt = conn.prepare(
        "SELECT id, username, expires_at, is_admin, disabled, cancelled_at FROM users ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                expires_at: parse_date(r.get(2)?),
                is_admin: r.get::<_, i64>(3)? != 0,
                disabled: r.get::<_, i64>(4)? != 0,
                cancelled: r.get::<_, Option<String>>(5)?.is_some(),
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
```

- [ ] **Step 6: `CurrentUser` 加 `cancelled`**（`src/web/auth/mod.rs`）

```rust
#[derive(Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub expires_at: Option<NaiveDate>,
    pub disabled: bool,
    pub cancelled: bool,
}

impl From<User> for CurrentUser {
    fn from(u: User) -> Self {
        CurrentUser { id: u.id, username: u.username, is_admin: u.is_admin, expires_at: u.expires_at, disabled: u.disabled, cancelled: u.cancelled }
    }
}
```

- [ ] **Step 7: 运行测试**

Run: `cargo test -p xlh migration_adds_cancelled_column_to_legacy_db cancelled_flag_reflects_column`
（若 crate 名不确定，用 `cargo test migration_adds_cancelled cancelled_flag_reflects_column`）
Expected: 2 passed。同时 `cargo build` 通过（所有 `User {…}` 构造点已补字段）。

- [ ] **Step 8: Commit**

```bash
git add src/web/auth/store.rs src/web/auth/model.rs src/web/auth/mod.rs
git commit -m "feat(account): users.cancelled_at 列 + 迁移 + User/CurrentUser.cancelled"
```

---

### Task 2: Store 层账户操作函数 + `count_admins` 排除已注销

**Files:**
- Modify: `src/web/auth/store.rs`（新增函数 + `count_admins` + 单测）

**Interfaces:**
- Consumes: `User`（Task 1）。
- Produces:
  - `pw_hash_by_id(conn: &Connection, user_id: i64) -> Result<Option<String>>`
  - `update_password(conn: &Connection, user_id: i64, new_hash: &str) -> Result<()>`
  - `delete_sessions_except(conn: &Connection, user_id: i64, keep: Option<&str>) -> Result<usize>`
  - `set_cancelled(conn: &Connection, user_id: i64, cancelled: bool) -> Result<()>`
  - `delete_user(conn: &mut Connection, user_id: i64) -> Result<()>`
  - `count_unactivated(conn: &Connection) -> Result<i64>`

- [ ] **Step 1: 写失败测试**（`store.rs` 的 `mod tests` 追加）

```rust
    #[test]
    fn password_and_session_helpers() {
        let conn = open_in_memory().unwrap();
        let uid = create_user(&conn, "u", "old", false).unwrap();
        // 改密
        update_password(&conn, uid, "new").unwrap();
        assert_eq!(pw_hash_by_id(&conn, uid).unwrap().unwrap(), "new");
        // 会话：留一删其余
        create_session(&conn, "keep", uid, chrono::Local::now().date_naive() + chrono::Duration::days(1)).unwrap();
        create_session(&conn, "drop", uid, chrono::Local::now().date_naive() + chrono::Duration::days(1)).unwrap();
        assert_eq!(delete_sessions_except(&conn, uid, Some("keep")).unwrap(), 1);
        let now = chrono::Local::now().date_naive();
        assert!(lookup_session_user(&conn, "keep", now).unwrap().is_some());
        assert!(lookup_session_user(&conn, "drop", now).unwrap().is_none());
        // 全删
        create_session(&conn, "x", uid, now + chrono::Duration::days(1)).unwrap();
        assert_eq!(delete_sessions_except(&conn, uid, None).unwrap(), 2); // keep + x
    }

    #[test]
    fn cancel_delete_and_count() {
        let mut conn = open_in_memory().unwrap();
        let a = create_user(&conn, "act", "h", false).unwrap();
        set_expiry(&conn, a, "2026-08-01".parse().unwrap()).unwrap(); // 已激活
        let n1 = create_user(&conn, "n1", "h", false).unwrap();       // 未激活
        create_user(&conn, "n2", "h", false).unwrap();                // 未激活
        assert_eq!(count_unactivated(&conn).unwrap(), 2, "已激活不计入");
        // 注销 n1 → 不再计入未激活
        set_cancelled(&conn, n1, true).unwrap();
        assert!(find_user_by_id(&conn, n1).unwrap().unwrap().cancelled);
        assert_eq!(count_unactivated(&conn).unwrap(), 1, "已注销不计入");
        // 恢复
        set_cancelled(&conn, n1, false).unwrap();
        assert!(!find_user_by_id(&conn, n1).unwrap().unwrap().cancelled);
        // 删除用户 + 其会话
        create_session(&conn, "s", a, chrono::Local::now().date_naive() + chrono::Duration::days(1)).unwrap();
        delete_user(&mut conn, a).unwrap();
        assert!(find_user_by_id(&conn, a).unwrap().is_none());
        assert!(lookup_session_user(&conn, "s", chrono::Local::now().date_naive()).unwrap().is_none());
    }

    #[test]
    fn count_admins_excludes_cancelled() {
        let conn = open_in_memory().unwrap();
        create_user(&conn, "a1", "h", true).unwrap();
        let a2 = create_user(&conn, "a2", "h", true).unwrap();
        set_cancelled(&conn, a2, true).unwrap();
        assert_eq!(count_admins(&conn).unwrap(), 1, "已注销管理员不计入");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test password_and_session_helpers cancel_delete_and_count count_admins_excludes_cancelled`
Expected: 编译失败——函数未定义。

- [ ] **Step 3: 实现函数**（`store.rs`，放在 `set_admin` 附近）

```rust
pub fn pw_hash_by_id(conn: &Connection, user_id: i64) -> Result<Option<String>> {
    conn.query_row("SELECT pw_hash FROM users WHERE id = ?1", [user_id], |r| r.get(0))
        .optional()
        .context("查询口令失败")
}

pub fn update_password(conn: &Connection, user_id: i64, new_hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET pw_hash = ?1 WHERE id = ?2",
        rusqlite::params![new_hash, user_id],
    )?;
    Ok(())
}

/// 删除该用户的会话；keep=Some(token) 保留当前会话，None 全删。返回删除行数。
pub fn delete_sessions_except(conn: &Connection, user_id: i64, keep: Option<&str>) -> Result<usize> {
    let n = match keep {
        Some(tok) => conn.execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token <> ?2",
            rusqlite::params![user_id, tok],
        )?,
        None => conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?,
    };
    Ok(n)
}

pub fn set_cancelled(conn: &Connection, user_id: i64, cancelled: bool) -> Result<()> {
    if cancelled {
        let now = chrono::Local::now().date_naive().to_string();
        conn.execute(
            "UPDATE users SET cancelled_at = ?1 WHERE id = ?2",
            rusqlite::params![now, user_id],
        )?;
    } else {
        conn.execute("UPDATE users SET cancelled_at = NULL WHERE id = ?1", [user_id])?;
    }
    Ok(())
}

/// 事务内删除用户及其会话。codes.used_by 保留作历史，不清理。
pub fn delete_user(conn: &mut Connection, user_id: i64) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?;
    tx.execute("DELETE FROM users WHERE id = ?1", [user_id])?;
    tx.commit()?;
    Ok(())
}

pub fn count_unactivated(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM users WHERE expires_at IS NULL AND cancelled_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}
```

- [ ] **Step 4: `count_admins` 排除已注销**（`store.rs`，改 WHERE）

```rust
pub fn count_admins(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND disabled = 0 AND cancelled_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}
```

- [ ] **Step 5: 运行测试**

Run: `cargo test password_and_session_helpers cancel_delete_and_count count_admins_excludes_cancelled count_admins_excludes_disabled`
Expected: 全部 passed（旧测试 `count_admins_excludes_disabled` 仍通过）。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/store.rs
git commit -m "feat(account): store 改密/会话/注销/删除/计数函数 + count_admins 排除已注销"
```

---

### Task 3: 访问控制——`login` 与 `require_login` 拦截已注销

**Files:**
- Modify: `src/web/auth/handlers.rs`（`login`）
- Modify: `src/web/auth/mod.rs`（`require_login`）
- Modify: `src/web/auth/routes.rs`（`mod tests` 加集成测试）

**Interfaces:**
- Consumes: `CurrentUser.cancelled`、`User.cancelled`、`store::set_cancelled`（Task 1/2）。

- [ ] **Step 1: 写失败测试**（`src/web/auth/routes.rs` 的 `mod tests` 追加）

```rust
    #[tokio::test]
    async fn login_rejects_cancelled_user() {
        let state = test_state();
        {
            let conn = state.db.lock().unwrap();
            let h = crate::web::auth::password::hash("pw123456").unwrap();
            let uid = store::create_user(&conn, "cx", &h, false).unwrap();
            store::set_cancelled(&conn, uid, true).unwrap();
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"cx","password":"pw123456"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn require_login_rejects_cancelled_session() {
        // 已注销用户即便持有有效会话，也应被 require_login 拦截（401）。
        let state = test_state();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "cs", "h", false).unwrap();
            store::set_cancelled(&conn, uid, true).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, "cstok", uid, exp).unwrap();
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/auth/me")
                    .header("cookie", "xlh_session=cstok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test login_rejects_cancelled_user require_login_rejects_cancelled_session`
Expected: `login_rejects_cancelled_user` 失败（当前返回 200——已注销用户仍能登录）。

- [ ] **Step 3: `login` 拦截已注销**（`src/web/auth/handlers.rs`，`login` 的匹配分支）

```rust
        Some((uid, hash, user)) => {
            let ok = super::password::verify(&cred.password, &hash);
            if user.disabled || user.cancelled || !ok {
                return json_error(StatusCode::UNAUTHORIZED, "invalid_login", None);
            }
            uid
        }
```

- [ ] **Step 4: `require_login` 拦截已注销**（`src/web/auth/mod.rs`，放行分支）

```rust
    match user {
        Some(u) if !u.disabled && !u.cancelled => {
            req.extensions_mut().insert(CurrentUser::from(u));
            next.run(req).await
        }
        _ => json_error(StatusCode::UNAUTHORIZED, "unauthorized", None),
    }
```

- [ ] **Step 5: 运行测试**

Run: `cargo test login_rejects_cancelled_user require_login_rejects_cancelled_session`
Expected: 2 passed。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/handlers.rs src/web/auth/mod.rs src/web/auth/routes.rs
git commit -m "feat(account): 已注销用户禁止登录与会话访问"
```

---

### Task 4: 自助改密 `change_password` + 路由

**Files:**
- Modify: `src/web/auth/handlers.rs`（`ChangePasswordReq` + `change_password`）
- Modify: `src/web/mod.rs`（authed 组加路由）
- Modify: `src/web/auth/routes.rs`（`mod tests` 加集成测试）

**Interfaces:**
- Consumes: `store::pw_hash_by_id`、`store::update_password`、`store::delete_sessions_except`、`session::read_cookie`。
- Produces: `POST /api/auth/change_password { current_password, new_password }`。

- [ ] **Step 1: 写失败测试**（`routes.rs` 的 `mod tests` 追加；复用现有 `post_admin` 作为「带 cookie 的 POST」）

```rust
    #[tokio::test]
    async fn change_password_flow() {
        let state = test_state();
        let uid = {
            let conn = state.db.lock().unwrap();
            let h = crate::web::auth::password::hash("old123").unwrap();
            let uid = store::create_user(&conn, "pu", &h, false).unwrap();
            let now = chrono::Local::now().date_naive();
            store::create_session(&conn, "ptok", uid, now + chrono::Duration::days(1)).unwrap();
            store::create_session(&conn, "other", uid, now + chrono::Duration::days(1)).unwrap();
            uid
        };
        // 旧密码错 → 400
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"bad","new_password":"new123"})).await,
            StatusCode::BAD_REQUEST);
        // 新密码过短 → 400
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"old123","new_password":"ab"})).await,
            StatusCode::BAD_REQUEST);
        // 成功 → 200
        assert_eq!(
            post_admin(router(state.clone()), "/api/auth/change_password", "ptok",
                serde_json::json!({"current_password":"old123","new_password":"new123"})).await,
            StatusCode::OK);
        let conn = state.db.lock().unwrap();
        let now = chrono::Local::now().date_naive();
        assert!(store::lookup_session_user(&conn, "other", now).unwrap().is_none(), "其他会话应失效");
        assert!(store::lookup_session_user(&conn, "ptok", now).unwrap().is_some(), "当前会话应保留");
        let h = store::pw_hash_by_id(&conn, uid).unwrap().unwrap();
        assert!(crate::web::auth::password::verify("new123", &h), "新密码可校验");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test change_password_flow`
Expected: 编译失败——路由 `/api/auth/change_password` 与处理函数不存在（404 或未定义）。

- [ ] **Step 3: 实现 `change_password`**（`src/web/auth/handlers.rs` 末尾）

```rust
#[derive(Deserialize)]
pub struct ChangePasswordReq {
    pub current_password: String,
    pub new_password: String,
}

pub async fn change_password(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    headers: HeaderMap,
    Json(req): Json<ChangePasswordReq>,
) -> Response {
    if req.new_password.chars().count() < 6 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_password", None);
    }
    // 锁外做慢速 argon2 校验：先取 hash 立即释放锁。
    let hash = {
        let conn = st.db.lock().unwrap();
        match store::pw_hash_by_id(&conn, user.id) {
            Ok(Some(h)) => h,
            _ => return json_error(StatusCode::UNAUTHORIZED, "unauthorized", None),
        }
    };
    if !super::password::verify(&req.current_password, &hash) {
        return json_error(StatusCode::BAD_REQUEST, "wrong_password", None);
    }
    let new_hash = match super::password::hash(&req.new_password) {
        Ok(h) => h,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "hash_failed", None),
    };
    let keep = session::read_cookie(&headers);
    let conn = st.db.lock().unwrap();
    if store::update_password(&conn, user.id, &new_hash).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    let _ = store::delete_sessions_except(&conn, user.id, keep.as_deref());
    (StatusCode::OK, Json(json!({"ok": true}))).into_response()
}
```

（`HeaderMap`、`session`、`CurrentUser`、`store` 均已在 `handlers.rs` 顶部 `use`。）

- [ ] **Step 4: 注册路由**（`src/web/mod.rs`，authed 组）

```rust
    let authed = Router::new()
        .route("/api/auth/logout", post(auth::handlers::logout))
        .route("/api/auth/activate", post(auth::handlers::activate))
        .route("/api/auth/me", get(auth::handlers::me))
        .route("/api/auth/change_password", post(auth::handlers::change_password))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));
```

- [ ] **Step 5: 运行测试**

Run: `cargo test change_password_flow`
Expected: PASS。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/handlers.rs src/web/mod.rs src/web/auth/routes.rs
git commit -m "feat(account): 自助修改密码 /api/auth/change_password"
```

---

### Task 5: 注册上限 10000

**Files:**
- Modify: `src/web/auth/handlers.rs`（`register`）
- Modify: `src/web/auth/routes.rs`（`mod tests` 加测试）

**Interfaces:**
- Consumes: `store::count_unactivated`（Task 2）。

- [ ] **Step 1: 写失败测试**（`routes.rs` 的 `mod tests` 追加）

```rust
    #[tokio::test]
    async fn register_blocked_at_cap() {
        let state = test_state(); // 默认 open_registration = true
        {
            let conn = state.db.lock().unwrap();
            for i in 0..10000 {
                store::create_user(&conn, &format!("u{i}"), "h", false).unwrap();
            }
        }
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/auth/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"username":"newbie","password":"pw123456"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let j: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(j["error"], "registration_full");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test register_blocked_at_cap`
Expected: 失败——上限未实现，返回 200（创建成功）。

- [ ] **Step 3: 加上限检查**（`src/web/auth/handlers.rs`，`register` 内取锁后、`create_user` 前）

```rust
    let conn = st.db.lock().unwrap();
    if store::count_unactivated(&conn).unwrap_or(0) >= 10000 {
        return json_error(StatusCode::FORBIDDEN, "registration_full", None);
    }
    match store::create_user(&conn, &cred.username, &hash, false) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(_) => json_error(StatusCode::CONFLICT, "username_taken", None),
    }
```

- [ ] **Step 4: 运行测试**

Run: `cargo test register_blocked_at_cap`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/web/auth/handlers.rs src/web/auth/routes.rs
git commit -m "feat(account): 未激活账号注册上限 10000"
```

---

### Task 6: 管理员重置密码 `reset_password` + 路由

**Files:**
- Modify: `src/web/auth/admin.rs`（`ResetPasswordReq` + `reset_password`）
- Modify: `src/web/auth/routes.rs`（`admin_router` 加路由 + 测试）

**Interfaces:**
- Consumes: `store::find_user_by_id`、`store::update_password`、`store::delete_sessions_except`、`super::password::hash`。
- Produces: `POST /api/admin/users/reset_password { user_id, new_password }`。

- [ ] **Step 1: 写失败测试**（`routes.rs` 的 `mod tests` 追加）

```rust
    #[tokio::test]
    async fn admin_reset_password_clears_sessions() {
        let state = test_state();
        seed_user(&state, "root", "atok", true, true);       // 管理员执行者
        let uid = seed_user(&state, "cust", "ctok", false, true); // 目标（已建会话 ctok）
        let s = post_admin(router(state.clone()), "/api/admin/users/reset_password", "atok",
            serde_json::json!({"user_id": uid, "new_password": "reset123"})).await;
        assert_eq!(s, StatusCode::OK);
        let conn = state.db.lock().unwrap();
        let now = chrono::Local::now().date_naive();
        assert!(store::lookup_session_user(&conn, "ctok", now).unwrap().is_none(), "目标会话应被清空");
        let h = store::pw_hash_by_id(&conn, uid).unwrap().unwrap();
        assert!(crate::web::auth::password::verify("reset123", &h));
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test admin_reset_password_clears_sessions`
Expected: 失败——路由不存在（404）。

- [ ] **Step 3: 实现 `reset_password`**（`src/web/auth/admin.rs`，放在 `set_admin` 之后）

```rust
#[derive(Deserialize)]
pub struct ResetPasswordReq { pub user_id: i64, pub new_password: String }

pub async fn reset_password(State(st): State<AuthState>, Json(req): Json<ResetPasswordReq>) -> Response {
    if req.new_password.chars().count() < 6 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_password", None);
    }
    let new_hash = match super::password::hash(&req.new_password) {
        Ok(h) => h,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "hash_failed", None),
    };
    let conn = st.db.lock().unwrap();
    if !matches!(store::find_user_by_id(&conn, req.user_id), Ok(Some(_))) {
        return json_error(StatusCode::NOT_FOUND, "user_not_found", None);
    }
    if store::update_password(&conn, req.user_id, &new_hash).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    let _ = store::delete_sessions_except(&conn, req.user_id, None);
    Json(json!({"ok": true})).into_response()
}
```

- [ ] **Step 4: 注册路由**（`src/web/auth/routes.rs`，`admin_router` 链上追加）

```rust
        .route("/api/admin/users/reset_password", post(admin::reset_password))
```

- [ ] **Step 5: 运行测试**

Run: `cargo test admin_reset_password_clears_sessions`
Expected: PASS。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/admin.rs src/web/auth/routes.rs
git commit -m "feat(account): 管理员重置用户密码"
```

---

### Task 7: 管理员注销/删除账号 + list_users 输出 cancelled + 路由

**Files:**
- Modify: `src/web/auth/admin.rs`（`CancelReq`/`cancel_user`、`DeleteReq`/`delete_user`、`list_users` JSON 加 `cancelled`）
- Modify: `src/web/auth/routes.rs`（`admin_router` 加 2 路由 + 测试）

**Interfaces:**
- Consumes: `store::find_user_by_id`、`store::set_cancelled`、`store::delete_user`、`store::delete_sessions_except`、`store::count_admins`。
- Produces: `POST /api/admin/users/cancel { user_id, cancelled }`；`POST /api/admin/users/delete { user_id }`。

- [ ] **Step 1: 写失败测试**（`routes.rs` 的 `mod tests` 追加）

```rust
    #[tokio::test]
    async fn admin_cancel_and_delete_rules() {
        let state = test_state();
        seed_user(&state, "root", "atok", true, true); // 管理员执行者
        // 未激活账号可直接删
        let free = seed_user(&state, "free", "ftok", false, false);
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": free})).await,
            StatusCode::OK);
        // 已激活未注销 → 必须先注销
        let paid = seed_user(&state, "paid", "ptok", false, true);
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": paid})).await,
            StatusCode::BAD_REQUEST);
        // 注销 paid（会话应被清）
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/cancel", "atok",
                serde_json::json!({"user_id": paid, "cancelled": true})).await,
            StatusCode::OK);
        {
            let conn = state.db.lock().unwrap();
            let now = chrono::Local::now().date_naive();
            assert!(store::lookup_session_user(&conn, "ptok", now).unwrap().is_none());
        }
        // 已注销 → 可删
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": paid})).await,
            StatusCode::OK);
    }

    #[tokio::test]
    async fn cannot_cancel_or_delete_sole_admin() {
        let state = test_state();
        let uid = seed_user(&state, "root", "atok", true, true);
        // 注销唯一管理员被拒
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/cancel", "atok",
                serde_json::json!({"user_id": uid, "cancelled": true})).await,
            StatusCode::BAD_REQUEST);
        // 删除唯一管理员被拒（已激活且未注销，先命中 last_admin）
        assert_eq!(
            post_admin(router(state.clone()), "/api/admin/users/delete", "atok",
                serde_json::json!({"user_id": uid})).await,
            StatusCode::BAD_REQUEST);
        let conn = state.db.lock().unwrap();
        assert!(store::find_user_by_id(&conn, uid).unwrap().is_some(), "唯一管理员仍存在");
    }

    #[tokio::test]
    async fn cancel_delete_hidden_for_non_admin() {
        let state = test_state();
        seed_user(&state, "cust", "ctok", false, true); // 非管理员
        let resp = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/users/delete")
                    .header("cookie", "xlh_session=ctok")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"user_id": 1}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test admin_cancel_and_delete_rules cannot_cancel_or_delete_sole_admin cancel_delete_hidden_for_non_admin`
Expected: 失败——路由不存在。

- [ ] **Step 3: 实现 `cancel_user` 与 `delete_user`**（`src/web/auth/admin.rs`，放在 `reset_password` 之后）

```rust
#[derive(Deserialize)]
pub struct CancelReq { pub user_id: i64, pub cancelled: bool }

pub async fn cancel_user(State(st): State<AuthState>, Json(req): Json<CancelReq>) -> Response {
    let conn = st.db.lock().unwrap();
    // 注销启用中的唯一管理员会锁死后台，拒绝。
    if req.cancelled {
        if let Ok(Some(u)) = store::find_user_by_id(&conn, req.user_id) {
            if u.is_admin && !u.disabled && !u.cancelled && store::count_admins(&conn).unwrap_or(0) <= 1 {
                return json_error(StatusCode::BAD_REQUEST, "last_admin", None);
            }
        }
    }
    if store::set_cancelled(&conn, req.user_id, req.cancelled).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    if req.cancelled {
        let _ = store::delete_sessions_except(&conn, req.user_id, None); // 立即踢下线
    }
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub struct DeleteReq { pub user_id: i64 }

pub async fn delete_user(State(st): State<AuthState>, Json(req): Json<DeleteReq>) -> Response {
    let mut conn = st.db.lock().unwrap();
    let u = match store::find_user_by_id(&conn, req.user_id) {
        Ok(Some(u)) => u,
        _ => return json_error(StatusCode::NOT_FOUND, "user_not_found", None),
    };
    // 末位管理员保护。
    if u.is_admin && !u.disabled && !u.cancelled && store::count_admins(&conn).unwrap_or(0) <= 1 {
        return json_error(StatusCode::BAD_REQUEST, "last_admin", None);
    }
    // 已激活且未注销 → 必须先注销。
    if u.expires_at.is_some() && !u.cancelled {
        return json_error(StatusCode::BAD_REQUEST, "must_cancel_first", None);
    }
    if store::delete_user(&mut conn, req.user_id).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    Json(json!({"ok": true})).into_response()
}
```

- [ ] **Step 4: `list_users` JSON 加 `cancelled`**（`src/web/auth/admin.rs`，`list_users` 的 `json!` 内）

```rust
                json!({
                    "id": u.id, "username": u.username, "expires_at": u.expires_at,
                    "is_admin": u.is_admin, "disabled": u.disabled, "cancelled": u.cancelled,
                    "status": status,
                })
```

- [ ] **Step 5: 注册路由**（`src/web/auth/routes.rs`，`admin_router` 追加）

```rust
        .route("/api/admin/users/cancel", post(admin::cancel_user))
        .route("/api/admin/users/delete", post(admin::delete_user))
```

- [ ] **Step 6: 运行测试**

Run: `cargo test admin_cancel_and_delete_rules cannot_cancel_or_delete_sole_admin cancel_delete_hidden_for_non_admin`
Expected: 全部 PASS。

- [ ] **Step 7: Commit**

```bash
git add src/web/auth/admin.rs src/web/auth/routes.rs
git commit -m "feat(account): 管理员注销/删除账号（两级规则+末位管理员保护）"
```

---

### Task 8: 前端——账户栏改密模态（`page.rs`）

**Files:**
- Modify: `src/web/page.rs`（`#xlh-bar` 加按钮、模态标记、JS）

**Interfaces:**
- Consumes: `POST /api/auth/change_password`（Task 4）。

- [ ] **Step 1: 账户栏加「修改密码」按钮**（`src/web/page.rs`，在 `退出` 按钮那一行前插入）

```html
  <button onclick="xlhPwOpen()" style="padding:4px 10px;border:0;border-radius:6px;background:#374151;color:#fff;cursor:pointer">修改密码</button>
```

- [ ] **Step 2: 插入改密模态标记**（紧接 `<div id="xlh-bar" …>…</div>` 结束标签之后）

```html
<div id="xlh-pw-mask" style="display:none;position:fixed;inset:0;z-index:100;background:rgba(0,0,0,.5);align-items:center;justify-content:center">
  <div style="background:#111827;color:#e5e7eb;border:1px solid #374151;border-radius:10px;padding:18px;width:300px;font:13px system-ui">
    <div style="font-size:15px;margin-bottom:12px">修改密码</div>
    <input id="pw-old" type="password" placeholder="旧密码" style="width:100%;box-sizing:border-box;margin-bottom:8px;padding:6px 8px;border-radius:6px;border:1px solid #374151;background:#0b1220;color:#e5e7eb">
    <input id="pw-new" type="password" placeholder="新密码（至少6位）" style="width:100%;box-sizing:border-box;margin-bottom:8px;padding:6px 8px;border-radius:6px;border:1px solid #374151;background:#0b1220;color:#e5e7eb">
    <input id="pw-new2" type="password" placeholder="确认新密码" style="width:100%;box-sizing:border-box;margin-bottom:12px;padding:6px 8px;border-radius:6px;border:1px solid #374151;background:#0b1220;color:#e5e7eb">
    <div style="text-align:right">
      <button onclick="xlhPwClose()" style="padding:5px 12px;border:0;border-radius:6px;background:#374151;color:#fff;cursor:pointer;margin-right:6px">取消</button>
      <button onclick="xlhPwSubmit()" style="padding:5px 12px;border:0;border-radius:6px;background:#3b82f6;color:#fff;cursor:pointer">提交</button>
    </div>
  </div>
</div>
```

- [ ] **Step 3: 加 JS 函数**（在账户栏 `<script>` 块内、`xlhMe();` 调用之前插入）

```js
function xlhPwOpen(){document.getElementById('pw-old').value='';document.getElementById('pw-new').value='';document.getElementById('pw-new2').value='';document.getElementById('xlh-pw-mask').style.display='flex';}
function xlhPwClose(){document.getElementById('xlh-pw-mask').style.display='none';}
async function xlhPwSubmit(){
  const cur=document.getElementById('pw-old').value,nw=document.getElementById('pw-new').value,nw2=document.getElementById('pw-new2').value;
  if(nw.length<6){alert('新密码至少 6 位');return;}
  if(nw!==nw2){alert('两次新密码不一致');return;}
  try{
    const r=await fetch('/api/auth/change_password',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({current_password:cur,new_password:nw})});
    const j=await r.json().catch(()=>({}));
    if(r.ok){alert('密码已修改');xlhPwClose();}
    else{alert(({wrong_password:'旧密码错误',invalid_password:'新密码至少 6 位'})[j.error]||('修改失败: '+(j.error||r.status)));}
  }catch(e){alert('请求失败：'+((e&&e.message)||e));}
}
```

- [ ] **Step 4: 编译**

Run: `cargo build`
Expected: 通过（HTML 为静态字符串，无编译错误）。

- [ ] **Step 5: 手动验证**

Run: `cargo run`（或项目既定启动方式），浏览器登录后：点击账户栏「修改密码」→ 弹出模态；输入错误旧密码提示「旧密码错误」；输入正确旧密码 + 新密码（≥6，两次一致）→ 提示「密码已修改」；用新密码重新登录成功。
Expected: 上述流程全部符合。

- [ ] **Step 6: Commit**

```bash
git add src/web/page.rs
git commit -m "feat(account): 主界面账户栏内嵌改密模态"
```

---

### Task 9: 前端——管理后台用户操作（`admin.rs`）

**Files:**
- Modify: `src/web/auth/admin.rs`（`ADMIN_HTML` 的 `loadUsers` 行模板 + JS 函数）

**Interfaces:**
- Consumes: `/api/admin/users/reset_password`、`/api/admin/users/cancel`、`/api/admin/users/delete`、`list_users` 输出的 `cancelled`（Task 6/7）。

- [ ] **Step 1: 改 `loadUsers` 行模板**（`src/web/auth/admin.rs`，`ADMIN_HTML` 内 `async function loadUsers` 的 `tb.innerHTML+=` 模板整体替换为）

```js
async function loadUsers(){const j=await api('/api/admin/users');const tb=document.querySelector('#users tbody');tb.innerHTML='';(j.users||[]).forEach(u=>{tb.innerHTML+=`<tr><td>${u.id}</td><td>${u.username}${u.is_admin?' 👑':''}</td><td>${u.status}${u.disabled?' (封禁)':''}${u.cancelled?' (已注销)':''}</td><td>${u.expires_at||'—'}</td><td>
  <input type="number" value="30" style="width:64px" id="d${u.id}"><button onclick="ext(${u.id})">续期</button>
  <button onclick="dis(${u.id},${!u.disabled})">${u.disabled?'解封':'封禁'}</button>
  <button onclick="adm(${u.id},${!u.is_admin})">${u.is_admin?'撤管理':'设管理'}</button>
  <button onclick="rst(${u.id})">重置密码</button>
  <button onclick="cxl(${u.id},${!u.cancelled})">${u.cancelled?'恢复':'注销'}</button>
  <button onclick="del(${u.id})">删除</button></td></tr>`;});}
```

- [ ] **Step 2: 加 JS 函数**（`ADMIN_HTML` 内，紧接 `adm` 函数之后）

```js
async function rst(id){const p=prompt('输入新密码（至少6位）');if(!p)return;if(p.length<6){alert('至少6位');return;}const j=await api('/api/admin/users/reset_password','POST',{user_id:id,new_password:p});if(j&&j.ok){alert('已重置该用户密码');}else{alert('失败: '+((j&&j.error)||''));}}
async function cxl(id,c){const j=await api('/api/admin/users/cancel','POST',{user_id:id,cancelled:c});if(j&&j.error==='last_admin'){alert('不能注销唯一管理员');}loadUsers();}
async function del(id){if(!confirm('确认删除该账号？此操作不可恢复'))return;const j=await api('/api/admin/users/delete','POST',{user_id:id});if(j&&j.error){alert(({must_cancel_first:'请先注销该已激活账号',last_admin:'不能删除唯一管理员',user_not_found:'用户不存在'})[j.error]||('删除失败: '+j.error));}loadUsers();ov();}
```

- [ ] **Step 3: 编译**

Run: `cargo build`
Expected: 通过。

- [ ] **Step 4: 手动验证**

Run: 启动后以管理员登录 `/admin`，用户表每行应有「重置密码 / 注销 / 删除」按钮：
- 重置密码 → prompt 输入 → 成功提示，目标用户被踢下线。
- 对已激活未注销用户点「删除」→ 提示「请先注销该已激活账号」；点「注销」→ 行显示「(已注销)」，再「删除」→ 成功移除。
- 对未激活用户点「删除」→ 直接移除。
- 对唯一管理员「注销/删除」→ 提示不可操作。
Expected: 全部符合。

- [ ] **Step 5: 全量回归**

Run: `cargo test`
Expected: 全部 PASS。

- [ ] **Step 6: Commit**

```bash
git add src/web/auth/admin.rs
git commit -m "feat(account): 管理后台重置密码/注销/删除用户操作"
```

---

## Self-Review

**Spec 覆盖核对：**
- 数据模型 `cancelled_at` + 迁移 + `User.cancelled` → Task 1 ✅
- Store 函数（`pw_hash_by_id`/`update_password`/`delete_sessions_except`/`set_cancelled`/`delete_user`/`count_unactivated`）→ Task 2 ✅
- `count_admins` 排除已注销 → Task 2 ✅
- 访问控制（login + require_login 拦截已注销、`CurrentUser.cancelled`）→ Task 1（字段）+ Task 3（拦截）✅
- 自助改密 + 路由 + 注销他会话 → Task 4 ✅
- 注册上限 10000 → Task 5 ✅
- 管理员重置密码 → Task 6 ✅
- 注销/删除 + 两级规则 + 末位管理员保护 + list_users 输出 cancelled → Task 7 ✅
- 前端账户栏改密模态 → Task 8 ✅
- 前端管理后台操作 → Task 9 ✅
- 全部错误码均在对应任务实现并被测试断言 ✅

**类型一致性：** `User.cancelled: bool`、`CurrentUser.cancelled: bool`、`delete_user(&mut Connection, i64)`（事务，故 admin 处理器用 `let mut conn`）、`delete_sessions_except(..., Option<&str>) -> usize`、`count_unactivated -> i64` 在各任务间一致引用。

**Placeholder 扫描：** 无 TBD/TODO；每个代码步骤均给出完整代码与预期输出。

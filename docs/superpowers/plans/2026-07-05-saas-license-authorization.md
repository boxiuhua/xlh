# xlh SaaS 授权收费体系 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 xlh Web 服务加一层「注册账号 + 授权码激活」的收费授权体系，未授权/到期时拦截核心 API，并提供网页管理后台发码与用户管理。

**Architecture:** 新增 `src/web/auth/` 模块，用 SQLite（rusqlite）持久化 users/codes/sessions 三表；axum 中间件按路由分组拦截（公开 / 需登录 / 需授权 / 需管理员）；密码用 argon2，会话用 DB 存储的随机不透明 token（Cookie）。对现有基金/股票分析代码零改动，仅在 `web::router()` 外层加认证/管理路由与中间件；首个管理员用 CLI 引导。

**Tech Stack:** Rust 2021 · axum 0.7 · rusqlite（bundled）· argon2 0.5 · rand 0.8 · base64 0.22（已有）· chrono（已有）· tower（dev，已有，用于 oneshot 测试）

## Global Constraints

- 语言/版本：Rust edition 2021；沿用现有 crate 结构（单 crate，lib+bin）。
- 授权状态唯一事实来源：`user.expires_at: Option<NaiveDate>`；`warn_days` 默认 7、`grace_days` 默认 3、`session_ttl_days` 默认 30。
- 中间件放行集合 `{Active, Warning, Grace}`；拦截 `{Inactive, Expired}`；`disabled` 用户一律拦截。
- 激活续期语义：`new_expiry = max(now, 原expiry.unwrap_or(now)) + days`。
- 一次性授权码：靠 SQL 条件更新 `WHERE used_by IS NULL AND revoked=0` + 检查 `changes()==1` 保证并发不重复占用。
- 密码只存 argon2 PHC 串（含盐），绝不明文/裸 SHA。
- 会话 token 为 CSPRNG 随机串，存 DB；Cookie 名 `xlh_session`，属性 `HttpOnly; SameSite=Lax; Path=/`。
- 对现有基金/股票业务代码零改动；只改 `src/web/mod.rs`（router/serve/index）、`src/config.rs`（无关，不动）、`src/main.rs`（加子命令）、`src/web/page.rs`（注入状态栏）。
- 现日期取 `chrono::Local::now().date_naive()`。
- 测试沿用现有风格；联网/端到端保持 `#[ignore]`。

---

### Task 1: 依赖与授权配置 `AuthCfg`

**Files:**
- Modify: `Cargo.toml:6-21`（`[dependencies]` 追加）
- Create: `src/web/auth/mod.rs`
- Create: `src/web/auth/config.rs`
- Modify: `src/web/mod.rs:1`（`pub mod auth;`）

**Interfaces:**
- Produces: `xlh::web::auth::config::{AuthCfg, load_auth}`；`AuthCfg{ db_path: PathBuf, open_registration: bool, warn_days: i64, grace_days: i64, session_ttl_days: i64 }`（`Clone`）。`load_auth(path: &Path) -> AuthCfg`（文件/段缺失即返回默认值）。

- [ ] **Step 1: 加依赖**

编辑 `Cargo.toml`，在 `[dependencies]` 末尾（`base64 = "0.22"` 之后）追加：

```toml
rusqlite = { version = "0.31", features = ["bundled"] }
argon2 = "0.5"
rand = "0.8"
```

- [ ] **Step 2: 建模块骨架**

创建 `src/web/auth/mod.rs`：

```rust
pub mod config;
```

在 `src/web/mod.rs` 顶部现有 `pub mod page;` / `pub mod stock;` 旁加一行：

```rust
pub mod auth;
```

- [ ] **Step 3: 写失败测试**

创建 `src/web/auth/config.rs`：

```rust
use std::path::{Path, PathBuf};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AuthCfg {
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_true")]
    pub open_registration: bool,
    #[serde(default = "default_warn")]
    pub warn_days: i64,
    #[serde(default = "default_grace")]
    pub grace_days: i64,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_days: i64,
}

fn default_db_path() -> PathBuf { PathBuf::from("data/xlh.db") }
fn default_true() -> bool { true }
fn default_warn() -> i64 { 7 }
fn default_grace() -> i64 { 3 }
fn default_session_ttl() -> i64 { 30 }

impl Default for AuthCfg {
    fn default() -> Self {
        AuthCfg {
            db_path: default_db_path(),
            open_registration: default_true(),
            warn_days: default_warn(),
            grace_days: default_grace(),
            session_ttl_days: default_session_ttl(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AuthFile {
    #[serde(default)]
    auth: AuthCfg,
}

/// 从 config.toml 宽松读取 `[auth]` 段；文件不存在或段缺失时返回默认值。
pub fn load_auth(path: &Path) -> AuthCfg {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str::<AuthFile>(&s).ok())
        .map(|f| f.auth)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = load_auth(Path::new("does/not/exist.toml"));
        assert_eq!(cfg.warn_days, 7);
        assert_eq!(cfg.grace_days, 3);
        assert!(cfg.open_registration);
        assert_eq!(cfg.db_path, PathBuf::from("data/xlh.db"));
    }

    #[test]
    fn parses_partial_section() {
        let toml = "[auth]\nwarn_days = 14\n";
        let f: AuthFile = toml::from_str(toml).unwrap();
        assert_eq!(f.auth.warn_days, 14);
        assert_eq!(f.auth.grace_days, 3); // 未写字段回落默认
    }
}
```

- [ ] **Step 4: 编译并跑测试**

Run: `cargo test --lib auth::config`
Expected: 编译通过，2 测试 PASS。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/web/mod.rs src/web/auth/
git commit -m "feat(auth): 依赖与 AuthCfg 配置加载"
```

---

### Task 2: 授权状态机 `LicenseStatus` 与续期计算

**Files:**
- Create: `src/web/auth/model.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod model;`）

**Interfaces:**
- Produces:
  - `enum LicenseStatus { Inactive, Active, Warning, Grace, Expired }`（`Serialize`, snake_case, `Copy`）。
  - `LicenseStatus::of(expires_at: Option<NaiveDate>, now: NaiveDate, warn_days: i64, grace_days: i64) -> LicenseStatus`
  - `LicenseStatus::allows_access(self) -> bool`
  - `renew_expiry(current: Option<NaiveDate>, now: NaiveDate, days: i64) -> NaiveDate`
  - `struct User { id: i64, username: String, expires_at: Option<NaiveDate>, is_admin: bool, disabled: bool }`（`Clone`）
  - `struct CodeRow { code: String, days: i64, used_by: Option<i64>, used_at: Option<String>, revoked: bool, created_at: String }`

- [ ] **Step 1: 写失败测试**

创建 `src/web/auth/model.rs`：

```rust
use chrono::{Duration, NaiveDate};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseStatus {
    Inactive,
    Active,
    Warning,
    Grace,
    Expired,
}

impl LicenseStatus {
    pub fn of(expires_at: Option<NaiveDate>, now: NaiveDate, warn_days: i64, grace_days: i64) -> Self {
        match expires_at {
            None => LicenseStatus::Inactive,
            Some(exp) => {
                if now <= exp - Duration::days(warn_days) {
                    LicenseStatus::Active
                } else if now <= exp {
                    LicenseStatus::Warning
                } else if now <= exp + Duration::days(grace_days) {
                    LicenseStatus::Grace
                } else {
                    LicenseStatus::Expired
                }
            }
        }
    }

    pub fn allows_access(self) -> bool {
        matches!(self, LicenseStatus::Active | LicenseStatus::Warning | LicenseStatus::Grace)
    }
}

/// 激活续期：从「now 与原到期日的较大者」叠加 days 天。
pub fn renew_expiry(current: Option<NaiveDate>, now: NaiveDate, days: i64) -> NaiveDate {
    let base = current.map(|c| c.max(now)).unwrap_or(now);
    base + Duration::days(days)
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub expires_at: Option<NaiveDate>,
    pub is_admin: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeRow {
    pub code: String,
    pub days: i64,
    pub used_by: Option<i64>,
    pub used_at: Option<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate { s.parse().unwrap() }

    #[test]
    fn inactive_when_no_expiry() {
        assert_eq!(LicenseStatus::of(None, d("2026-07-05"), 7, 3), LicenseStatus::Inactive);
    }

    #[test]
    fn status_boundaries() {
        let exp = d("2026-07-10");
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-03"), 7, 3), LicenseStatus::Active);  // == exp-7
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-04"), 7, 3), LicenseStatus::Warning); // 进入临期
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-10"), 7, 3), LicenseStatus::Warning); // == exp
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-11"), 7, 3), LicenseStatus::Grace);   // exp+1
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-13"), 7, 3), LicenseStatus::Grace);   // == exp+3
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-14"), 7, 3), LicenseStatus::Expired); // exp+4
    }

    #[test]
    fn allows_access_set() {
        assert!(LicenseStatus::Active.allows_access());
        assert!(LicenseStatus::Warning.allows_access());
        assert!(LicenseStatus::Grace.allows_access());
        assert!(!LicenseStatus::Inactive.allows_access());
        assert!(!LicenseStatus::Expired.allows_access());
    }

    #[test]
    fn renew_from_now_when_inactive_or_expired() {
        let now = d("2026-07-05");
        assert_eq!(renew_expiry(None, now, 30), d("2026-08-04"));
        assert_eq!(renew_expiry(Some(d("2026-06-01")), now, 30), d("2026-08-04")); // 过期→从今天
    }

    #[test]
    fn renew_stacks_when_still_valid() {
        let now = d("2026-07-05");
        assert_eq!(renew_expiry(Some(d("2026-08-01")), now, 30), d("2026-08-31")); // 未过期→叠加
    }
}
```

- [ ] **Step 2: 注册模块**

在 `src/web/auth/mod.rs` 追加：`pub mod model;`

- [ ] **Step 3: 跑测试**

Run: `cargo test --lib auth::model`
Expected: 5 测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 授权状态机与续期计算"
```

---

### Task 3: 密码哈希 `password.rs`

**Files:**
- Create: `src/web/auth/password.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod password;`）

**Interfaces:**
- Produces：`password::hash(plain: &str) -> anyhow::Result<String>`（返回 PHC 串）、`password::verify(plain: &str, phc: &str) -> bool`。

- [ ] **Step 1: 写失败测试**

创建 `src/web/auth/password.rs`：

```rust
use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// 生成 argon2 PHC 串（含随机盐）。
pub fn hash(plain: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow!("hash 失败: {e}"))?
        .to_string();
    Ok(phc)
}

/// 校验明文口令与 PHC 串是否匹配；任何解析/校验错误都视为不匹配。
pub fn verify(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(plain.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrip() {
        let h = hash("s3cret").unwrap();
        assert!(verify("s3cret", &h));
        assert!(!verify("wrong", &h));
    }

    #[test]
    fn salt_makes_hashes_differ() {
        assert_ne!(hash("same").unwrap(), hash("same").unwrap());
    }

    #[test]
    fn garbage_phc_is_not_verified() {
        assert!(!verify("x", "not-a-phc-string"));
    }
}
```

- [ ] **Step 2: 注册模块**：`src/web/auth/mod.rs` 追加 `pub mod password;`
- [ ] **Step 3: 跑测试**

Run: `cargo test --lib auth::password`
Expected: 3 测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): argon2 密码哈希"
```

---

### Task 4: 会话 token 与 Cookie 助手 `session.rs`

**Files:**
- Create: `src/web/auth/session.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod session;`）

**Interfaces:**
- Produces：
  - `session::new_token() -> String`（32 字节 CSPRNG，base64url 无填充）
  - `session::COOKIE_NAME: &str = "xlh_session"`
  - `session::read_cookie(headers: &axum::http::HeaderMap) -> Option<String>`
  - `session::set_cookie_header(token: &str, ttl_days: i64) -> String`（返回 Set-Cookie 值）
  - `session::clear_cookie_header() -> String`

- [ ] **Step 1: 写失败测试**

创建 `src/web/auth/session.rs`：

```rust
use axum::http::{header::COOKIE, HeaderMap};
use base64::Engine;
use rand::RngCore;

pub const COOKIE_NAME: &str = "xlh_session";

/// 32 字节随机，base64url 无填充。
pub fn new_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// 从请求头解析 xlh_session 的值。
pub fn read_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{COOKIE_NAME}=")) {
            return Some(v.to_string());
        }
    }
    None
}

pub fn set_cookie_header(token: &str, ttl_days: i64) -> String {
    let max_age = ttl_days * 24 * 3600;
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

pub fn clear_cookie_header() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_nonempty() {
        let a = new_token();
        let b = new_token();
        assert!(!a.is_empty());
        assert_ne!(a, b);
    }

    #[test]
    fn read_cookie_picks_our_key() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, "foo=1; xlh_session=abc123; bar=2".parse().unwrap());
        assert_eq!(read_cookie(&h).as_deref(), Some("abc123"));
    }

    #[test]
    fn read_cookie_absent() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, "foo=1".parse().unwrap());
        assert_eq!(read_cookie(&h), None);
    }

    #[test]
    fn set_and_clear_headers() {
        assert!(set_cookie_header("t", 30).contains("xlh_session=t"));
        assert!(set_cookie_header("t", 30).contains("Max-Age=2592000"));
        assert!(clear_cookie_header().contains("Max-Age=0"));
    }
}
```

- [ ] **Step 2: 注册模块**：`src/web/auth/mod.rs` 追加 `pub mod session;`
- [ ] **Step 3: 跑测试**

Run: `cargo test --lib auth::session`
Expected: 4 测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 会话 token 与 Cookie 助手"
```

---

### Task 5: SQLite 存储——建表与用户 CRUD `store.rs`

**Files:**
- Create: `src/web/auth/store.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod store;`）

**Interfaces:**
- Consumes：`model::User`、`password`（测试里造哈希）。
- Produces（均在 `store` 内）：
  - `open(path: &Path) -> Result<Connection>`（建父目录、开 WAL、跑 migrate）
  - `open_in_memory() -> Result<Connection>`（测试用）
  - `create_user(conn, username, pw_hash, is_admin) -> Result<i64>`
  - `find_user_by_name(conn, username) -> Result<Option<(i64, String, User)>>`（返回 id、pw_hash、User）
  - `find_user_by_id(conn, id) -> Result<Option<User>>`
  - `set_expiry(conn, user_id, expires_at: NaiveDate) -> Result<()>`
  - `set_disabled(conn, user_id, disabled: bool) -> Result<()>`
  - `set_admin(conn, user_id, is_admin: bool) -> Result<()>`
  - `count_admins(conn) -> Result<i64>`
  - `list_users(conn) -> Result<Vec<User>>`

- [ ] **Step 1: 写失败测试（含建表与用户 CRUD）**

创建 `src/web/auth/store.rs`：

```rust
use std::path::Path;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};

use super::model::User;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  id         INTEGER PRIMARY KEY,
  username   TEXT NOT NULL UNIQUE,
  pw_hash    TEXT NOT NULL,
  expires_at TEXT,
  is_admin   INTEGER NOT NULL DEFAULT 0,
  disabled   INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS codes (
  code       TEXT PRIMARY KEY,
  days       INTEGER NOT NULL,
  used_by    INTEGER REFERENCES users(id),
  used_at    TEXT,
  revoked    INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  token      TEXT PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users(id),
  expires_at TEXT NOT NULL,
  created_at TEXT NOT NULL
);
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建表失败")?;
    Ok(())
}

pub fn open(path: &Path) -> Result<Connection> {
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

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

fn parse_date(s: Option<String>) -> Option<NaiveDate> {
    s.and_then(|s| s.parse().ok())
}

pub fn create_user(conn: &Connection, username: &str, pw_hash: &str, is_admin: bool) -> Result<i64> {
    let now = chrono::Local::now().date_naive().to_string();
    conn.execute(
        "INSERT INTO users (username, pw_hash, is_admin, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![username, pw_hash, is_admin as i64, now],
    )
    .context("创建用户失败（用户名可能已存在）")?;
    Ok(conn.last_insert_rowid())
}

pub fn find_user_by_name(conn: &Connection, username: &str) -> Result<Option<(i64, String, User)>> {
    conn.query_row(
        "SELECT id, username, pw_hash, expires_at, is_admin, disabled FROM users WHERE username = ?1",
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
            };
            Ok((id, pw_hash, user))
        },
    )
    .optional()
    .context("查询用户失败")
}

pub fn find_user_by_id(conn: &Connection, id: i64) -> Result<Option<User>> {
    conn.query_row(
        "SELECT id, username, expires_at, is_admin, disabled FROM users WHERE id = ?1",
        [id],
        |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                expires_at: parse_date(r.get(2)?),
                is_admin: r.get::<_, i64>(3)? != 0,
                disabled: r.get::<_, i64>(4)? != 0,
            })
        },
    )
    .optional()
    .context("查询用户失败")
}

pub fn set_expiry(conn: &Connection, user_id: i64, expires_at: NaiveDate) -> Result<()> {
    conn.execute(
        "UPDATE users SET expires_at = ?1 WHERE id = ?2",
        rusqlite::params![expires_at.to_string(), user_id],
    )?;
    Ok(())
}

pub fn set_disabled(conn: &Connection, user_id: i64, disabled: bool) -> Result<()> {
    conn.execute(
        "UPDATE users SET disabled = ?1 WHERE id = ?2",
        rusqlite::params![disabled as i64, user_id],
    )?;
    Ok(())
}

pub fn set_admin(conn: &Connection, user_id: i64, is_admin: bool) -> Result<()> {
    conn.execute(
        "UPDATE users SET is_admin = ?1 WHERE id = ?2",
        rusqlite::params![is_admin as i64, user_id],
    )?;
    Ok(())
}

pub fn count_admins(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM users WHERE is_admin = 1", [], |r| r.get(0))?)
}

pub fn list_users(conn: &Connection) -> Result<Vec<User>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, expires_at, is_admin, disabled FROM users ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(User {
                id: r.get(0)?,
                username: r.get(1)?,
                expires_at: parse_date(r.get(2)?),
                is_admin: r.get::<_, i64>(3)? != 0,
                disabled: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_find_and_mutate_user() {
        let conn = open_in_memory().unwrap();
        let id = create_user(&conn, "alice", "phc", false).unwrap();

        let (fid, hash, user) = find_user_by_name(&conn, "alice").unwrap().unwrap();
        assert_eq!(fid, id);
        assert_eq!(hash, "phc");
        assert_eq!(user.expires_at, None);
        assert!(!user.is_admin);

        set_expiry(&conn, id, "2026-08-01".parse().unwrap()).unwrap();
        set_admin(&conn, id, true).unwrap();
        set_disabled(&conn, id, true).unwrap();
        let u = find_user_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(u.expires_at, Some("2026-08-01".parse().unwrap()));
        assert!(u.is_admin && u.disabled);
    }

    #[test]
    fn duplicate_username_rejected() {
        let conn = open_in_memory().unwrap();
        create_user(&conn, "bob", "h", false).unwrap();
        assert!(create_user(&conn, "bob", "h2", false).is_err());
    }

    #[test]
    fn count_admins_and_list() {
        let conn = open_in_memory().unwrap();
        create_user(&conn, "a", "h", true).unwrap();
        create_user(&conn, "b", "h", false).unwrap();
        assert_eq!(count_admins(&conn).unwrap(), 1);
        assert_eq!(list_users(&conn).unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: 注册模块**：`src/web/auth/mod.rs` 追加 `pub mod store;`
- [ ] **Step 3: 跑测试**

Run: `cargo test --lib auth::store`
Expected: 3 测试 PASS。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/ Cargo.lock
git commit -m "feat(auth): SQLite 建表与用户 CRUD"
```

---

### Task 6: 授权码存储与原子激活

**Files:**
- Modify: `src/web/auth/store.rs`（追加码相关函数与测试）

**Interfaces:**
- Consumes：`model::{CodeRow, renew_expiry}`。
- Produces（`store` 内）：
  - `issue_code(conn, code, days) -> Result<()>`
  - `list_codes(conn, filter: CodeFilter) -> Result<Vec<CodeRow>>`；`enum CodeFilter { Unused, Used, All }`
  - `revoke_code(conn, code) -> Result<bool>`（作废未用码，返回是否命中）
  - `activate(conn, code, user_id, warn/grace 无关) -> Result<NaiveDate>`：事务内一次性占用 + 续期，返回新到期日
  - `enum ActivateError`（`thiserror`）：`NotFound`、`AlreadyUsed`、`Revoked`

- [ ] **Step 1: 写失败测试（追加到 store.rs 的 tests 模块外新增函数 + 新测试）**

在 `src/web/auth/store.rs` 顶部 `use` 追加：

```rust
use super::model::{renew_expiry, CodeRow};
```

在文件（tests 模块之前）追加：

```rust
#[derive(Debug, Clone, Copy)]
pub enum CodeFilter { Unused, Used, All }

#[derive(Debug, thiserror::Error)]
pub enum ActivateError {
    #[error("授权码不存在")]
    NotFound,
    #[error("授权码已被使用")]
    AlreadyUsed,
    #[error("授权码已作废")]
    Revoked,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

pub fn issue_code(conn: &Connection, code: &str, days: i64) -> Result<()> {
    let now = chrono::Local::now().date_naive().to_string();
    conn.execute(
        "INSERT INTO codes (code, days, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![code, days, now],
    )?;
    Ok(())
}

pub fn list_codes(conn: &Connection, filter: CodeFilter) -> Result<Vec<CodeRow>> {
    let where_clause = match filter {
        CodeFilter::Unused => "WHERE used_by IS NULL AND revoked = 0",
        CodeFilter::Used => "WHERE used_by IS NOT NULL",
        CodeFilter::All => "",
    };
    let sql = format!(
        "SELECT code, days, used_by, used_at, revoked, created_at FROM codes {where_clause} ORDER BY created_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CodeRow {
                code: r.get(0)?,
                days: r.get(1)?,
                used_by: r.get(2)?,
                used_at: r.get(3)?,
                revoked: r.get::<_, i64>(4)? != 0,
                created_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn revoke_code(conn: &Connection, code: &str) -> Result<bool> {
    let n = conn.execute(
        "UPDATE codes SET revoked = 1 WHERE code = ?1 AND used_by IS NULL",
        [code],
    )?;
    Ok(n == 1)
}

/// 事务内：一次性占用授权码 + 续期用户到期日。返回新到期日。
pub fn activate(conn: &mut Connection, code: &str, user_id: i64) -> std::result::Result<NaiveDate, ActivateError> {
    let now = chrono::Local::now().date_naive();
    let tx = conn.transaction()?;

    // 读码天数与状态
    let row: Option<(i64, Option<i64>, i64)> = tx
        .query_row(
            "SELECT days, used_by, revoked FROM codes WHERE code = ?1",
            [code],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (days, used_by, revoked) = row.ok_or(ActivateError::NotFound)?;
    if revoked != 0 { return Err(ActivateError::Revoked); }
    if used_by.is_some() { return Err(ActivateError::AlreadyUsed); }

    // 条件占用：并发下只有一方 changes()==1
    let claimed = tx.execute(
        "UPDATE codes SET used_by = ?1, used_at = ?2 WHERE code = ?3 AND used_by IS NULL AND revoked = 0",
        rusqlite::params![user_id, now.to_string(), code],
    )?;
    if claimed != 1 { return Err(ActivateError::AlreadyUsed); }

    // 读当前到期日并续期
    let cur: Option<String> = tx.query_row(
        "SELECT expires_at FROM users WHERE id = ?1", [user_id], |r| r.get(0),
    )?;
    let new_exp = renew_expiry(cur.and_then(|s| s.parse().ok()), now, days);
    tx.execute(
        "UPDATE users SET expires_at = ?1 WHERE id = ?2",
        rusqlite::params![new_exp.to_string(), user_id],
    )?;

    tx.commit()?;
    Ok(new_exp)
}
```

在 `tests` 模块内追加：

```rust
    #[test]
    fn activate_consumes_code_and_renews() {
        let mut conn = open_in_memory().unwrap();
        let uid = create_user(&conn, "u", "h", false).unwrap();
        issue_code(&conn, "CODE1", 30).unwrap();

        let exp = activate(&mut conn, "CODE1", uid).unwrap();
        assert_eq!(exp, renew_expiry(None, chrono::Local::now().date_naive(), 30));

        // 二次使用同码失败
        let err = activate(&mut conn, "CODE1", uid).unwrap_err();
        assert!(matches!(err, ActivateError::AlreadyUsed));
    }

    #[test]
    fn activate_unknown_and_revoked() {
        let mut conn = open_in_memory().unwrap();
        let uid = create_user(&conn, "u", "h", false).unwrap();
        assert!(matches!(activate(&mut conn, "NOPE", uid).unwrap_err(), ActivateError::NotFound));

        issue_code(&conn, "R", 10).unwrap();
        assert!(revoke_code(&conn, "R").unwrap());
        assert!(matches!(activate(&mut conn, "R", uid).unwrap_err(), ActivateError::Revoked));
    }

    #[test]
    fn list_and_revoke_filters() {
        let conn = open_in_memory().unwrap();
        issue_code(&conn, "A", 30).unwrap();
        issue_code(&conn, "B", 30).unwrap();
        assert_eq!(list_codes(&conn, CodeFilter::Unused).unwrap().len(), 2);
        assert!(revoke_code(&conn, "A").unwrap());
        assert_eq!(list_codes(&conn, CodeFilter::Unused).unwrap().len(), 1);
    }
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --lib auth::store`
Expected: 6 测试 PASS（含 Task 5 的 3 个）。

- [ ] **Step 3: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 授权码存储与原子激活"
```

---

### Task 7: 会话存储

**Files:**
- Modify: `src/web/auth/store.rs`（追加 session 函数与测试）

**Interfaces:**
- Produces（`store` 内）：
  - `create_session(conn, token, user_id, expires_at: NaiveDate) -> Result<()>`
  - `lookup_session_user(conn, token, now: NaiveDate) -> Result<Option<User>>`（会话未过期才返回其 User）
  - `delete_session(conn, token) -> Result<()>`

- [ ] **Step 1: 写失败测试 + 实现**

在 `src/web/auth/store.rs`（tests 之前）追加：

```rust
pub fn create_session(conn: &Connection, token: &str, user_id: i64, expires_at: NaiveDate) -> Result<()> {
    let now = chrono::Local::now().date_naive().to_string();
    conn.execute(
        "INSERT INTO sessions (token, user_id, expires_at, created_at) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![token, user_id, expires_at.to_string(), now],
    )?;
    Ok(())
}

pub fn lookup_session_user(conn: &Connection, token: &str, now: NaiveDate) -> Result<Option<User>> {
    let uid: Option<(i64, String)> = conn
        .query_row(
            "SELECT user_id, expires_at FROM sessions WHERE token = ?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((user_id, exp)) = uid else { return Ok(None) };
    let session_exp: NaiveDate = exp.parse().unwrap_or(now);
    if session_exp < now {
        return Ok(None); // 会话过期
    }
    find_user_by_id(conn, user_id)
}

pub fn delete_session(conn: &Connection, token: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE token = ?1", [token])?;
    Ok(())
}
```

在 `tests` 模块内追加：

```rust
    #[test]
    fn session_roundtrip_and_expiry() {
        let conn = open_in_memory().unwrap();
        let uid = create_user(&conn, "u", "h", false).unwrap();
        let now = chrono::Local::now().date_naive();

        create_session(&conn, "tok", uid, now + chrono::Duration::days(30)).unwrap();
        assert_eq!(lookup_session_user(&conn, "tok", now).unwrap().unwrap().id, uid);

        // 过期会话不返回用户
        assert!(lookup_session_user(&conn, "tok", now + chrono::Duration::days(31)).unwrap().is_none());

        delete_session(&conn, "tok").unwrap();
        assert!(lookup_session_user(&conn, "tok", now).unwrap().is_none());
    }
```

- [ ] **Step 2: 跑测试**

Run: `cargo test --lib auth::store`
Expected: 7 测试 PASS。

- [ ] **Step 3: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 会话存储"
```

---

### Task 8: `AuthState` 与三个中间件

**Files:**
- Modify: `src/web/auth/mod.rs`（`AuthState`、`CurrentUser`、`require_login`/`require_license`/`require_admin`、JSON 错误助手）

**Interfaces:**
- Consumes：`store`、`model::{User, LicenseStatus}`、`config::AuthCfg`、`session`。
- Produces：
  - `struct AuthState { db: Arc<Mutex<Connection>>, cfg: AuthCfg }`（`Clone`）
  - `struct CurrentUser { pub id: i64, pub username: String, pub is_admin: bool, pub expires_at: Option<NaiveDate>, pub disabled: bool }`（`Clone`）
  - `async fn require_login(State<AuthState>, Request, Next) -> Response`（设 `CurrentUser` extension，401 若无）
  - `async fn require_license(Extension<CurrentUser>, State<AuthState>, Request, Next) -> Response`（403 若 disabled 或状态不放行）
  - `async fn require_admin(Extension<CurrentUser>, Request, Next) -> Response`（404 若非 admin）
  - `fn json_error(code: StatusCode, err: &str, status: Option<LicenseStatus>) -> Response`

- [ ] **Step 1: 写实现**

在 `src/web/auth/mod.rs` 顶部（`pub mod` 之后）追加：

```rust
use std::sync::{Arc, Mutex};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use config::AuthCfg;
use model::{LicenseStatus, User};

#[derive(Clone)]
pub struct AuthState {
    pub db: Arc<Mutex<Connection>>,
    pub cfg: AuthCfg,
}

impl AuthState {
    pub fn new(conn: Connection, cfg: AuthCfg) -> Self {
        AuthState { db: Arc::new(Mutex::new(conn)), cfg }
    }
}

#[derive(Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub expires_at: Option<NaiveDate>,
    pub disabled: bool,
}

impl From<User> for CurrentUser {
    fn from(u: User) -> Self {
        CurrentUser { id: u.id, username: u.username, is_admin: u.is_admin, expires_at: u.expires_at, disabled: u.disabled }
    }
}

pub fn json_error(code: StatusCode, err: &str, status: Option<LicenseStatus>) -> Response {
    let body = json!({ "error": err, "status": status });
    (code, Json(body)).into_response()
}

/// 拦截：必须有有效会话，否则 401。通过则注入 CurrentUser。
pub async fn require_login(State(st): State<AuthState>, mut req: Request, next: Next) -> Response {
    let token = session::read_cookie(req.headers());
    let now = chrono::Local::now().date_naive();
    let user = match token {
        Some(t) => {
            let conn = st.db.lock().unwrap();
            store::lookup_session_user(&conn, &t, now).ok().flatten()
        }
        None => None,
    };
    match user {
        Some(u) if !u.disabled => {
            req.extensions_mut().insert(CurrentUser::from(u));
            next.run(req).await
        }
        _ => json_error(StatusCode::UNAUTHORIZED, "unauthorized", None),
    }
}

/// 在 require_login 之后运行：授权状态须放行，否则 403。
pub async fn require_license(Extension(user): Extension<CurrentUser>, State(st): State<AuthState>, req: Request, next: Next) -> Response {
    let now = chrono::Local::now().date_naive();
    let status = LicenseStatus::of(user.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
    if user.disabled || !status.allows_access() {
        let err = if user.expires_at.is_none() { "license_required" } else { "expired" };
        return json_error(StatusCode::FORBIDDEN, err, Some(status));
    }
    next.run(req).await
}

/// 在 require_login 之后运行：非管理员一律 404（不暴露后台存在）。
pub async fn require_admin(Extension(user): Extension<CurrentUser>, req: Request, next: Next) -> Response {
    if !user.is_admin {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    next.run(req).await
}
```

> **中间件叠放顺序（关键）**：axum 中后添加的 layer 处于外层、先执行。因此在子路由上要写
> `.route_layer(require_license).route_layer(require_login)`——`require_login` 后加、最外层先跑，
> 设好 `CurrentUser` extension，`require_license` 再读它。顺序写反会 panic（缺 extension）。

- [ ] **Step 2: 编译**

Run: `cargo build --lib`
Expected: 通过（中间件将在 Task 11 接线并测试）。

- [ ] **Step 3: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): AuthState 与登录/授权/管理员中间件"
```

---

### Task 9: 认证 API 处理器 `handlers.rs`

**Files:**
- Create: `src/web/auth/handlers.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod handlers;`）

**Interfaces:**
- Consumes：`AuthState`、`CurrentUser`、`store`、`password`、`session`、`model::LicenseStatus`。
- Produces（`handlers` 内 axum handler）：
  - `register(State<AuthState>, Json<Credentials>) -> Response`
  - `login(State<AuthState>, Json<Credentials>) -> Response`（成功下发 Set-Cookie）
  - `logout(State<AuthState>, headers) -> Response`
  - `activate(State<AuthState>, Extension<CurrentUser>, Json<ActivateReq>) -> Response`
  - `me(State<AuthState>, Extension<CurrentUser>) -> Response`
  - `struct Credentials { username: String, password: String }`
  - `struct ActivateReq { code: String }`

- [ ] **Step 1: 写实现**

创建 `src/web/auth/handlers.rs`：

```rust
use axum::extract::State;
use axum::http::{header::SET_COOKIE, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use super::model::LicenseStatus;
use super::{json_error, session, store, AuthState, CurrentUser};

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ActivateReq {
    pub code: String,
}

fn valid_username(u: &str) -> bool {
    let n = u.chars().count();
    (3..=32).contains(&n) && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub async fn register(State(st): State<AuthState>, Json(cred): Json<Credentials>) -> Response {
    if !st.cfg.open_registration {
        return json_error(StatusCode::FORBIDDEN, "registration_closed", None);
    }
    if !valid_username(&cred.username) || cred.password.chars().count() < 6 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_credentials", None);
    }
    let hash = match super::password::hash(&cred.password) {
        Ok(h) => h,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "hash_failed", None),
    };
    let conn = st.db.lock().unwrap();
    match store::create_user(&conn, &cred.username, &hash, false) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(_) => json_error(StatusCode::CONFLICT, "username_taken", None),
    }
}

pub async fn login(State(st): State<AuthState>, Json(cred): Json<Credentials>) -> Response {
    let conn = st.db.lock().unwrap();
    let found = store::find_user_by_name(&conn, &cred.username).ok().flatten();
    // 统一失败文案，避免用户名枚举
    let (uid, hash, user) = match found {
        Some(t) => t,
        None => return json_error(StatusCode::UNAUTHORIZED, "invalid_login", None),
    };
    if user.disabled || !super::password::verify(&cred.password, &hash) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid_login", None);
    }
    let token = session::new_token();
    let exp = chrono::Local::now().date_naive() + chrono::Duration::days(st.cfg.session_ttl_days);
    if store::create_session(&conn, &token, uid, exp).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session_failed", None);
    }
    let mut headers = HeaderMap::new();
    headers.insert(SET_COOKIE, session::set_cookie_header(&token, st.cfg.session_ttl_days).parse().unwrap());
    (StatusCode::OK, headers, Json(json!({"ok": true}))).into_response()
}

pub async fn logout(State(st): State<AuthState>, headers: HeaderMap) -> Response {
    if let Some(token) = session::read_cookie(&headers) {
        let conn = st.db.lock().unwrap();
        let _ = store::delete_session(&conn, &token);
    }
    let mut out = HeaderMap::new();
    out.insert(SET_COOKIE, session::clear_cookie_header().parse().unwrap());
    (StatusCode::OK, out, Json(json!({"ok": true}))).into_response()
}

pub async fn activate(State(st): State<AuthState>, Extension(user): Extension<CurrentUser>, Json(req): Json<ActivateReq>) -> Response {
    let mut conn = st.db.lock().unwrap();
    match store::activate(&mut conn, req.code.trim(), user.id) {
        Ok(new_exp) => {
            let now = chrono::Local::now().date_naive();
            let status = LicenseStatus::of(Some(new_exp), now, st.cfg.warn_days, st.cfg.grace_days);
            (StatusCode::OK, Json(json!({"ok": true, "expires_at": new_exp, "status": status}))).into_response()
        }
        Err(store::ActivateError::NotFound) => json_error(StatusCode::BAD_REQUEST, "code_not_found", None),
        Err(store::ActivateError::AlreadyUsed) => json_error(StatusCode::BAD_REQUEST, "code_used", None),
        Err(store::ActivateError::Revoked) => json_error(StatusCode::BAD_REQUEST, "code_revoked", None),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "activate_failed", None),
    }
}

pub async fn me(State(st): State<AuthState>, Extension(user): Extension<CurrentUser>) -> Response {
    let now = chrono::Local::now().date_naive();
    let status = LicenseStatus::of(user.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
    let remaining = user.expires_at.map(|e| (e - now).num_days());
    Json(json!({
        "username": user.username,
        "is_admin": user.is_admin,
        "expires_at": user.expires_at,
        "status": status,
        "warn_days": st.cfg.warn_days,
        "grace_days": st.cfg.grace_days,
        "remaining_days": remaining,
    })).into_response()
}
```

- [ ] **Step 2: 注册模块**：`src/web/auth/mod.rs` 追加 `pub mod handlers;`
- [ ] **Step 3: 编译**

Run: `cargo build --lib`
Expected: 通过。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 注册/登录/登出/激活/me 处理器"
```

---

### Task 10: 管理 API 处理器 `admin.rs`

**Files:**
- Create: `src/web/auth/admin.rs`
- Modify: `src/web/auth/mod.rs`（`pub mod admin;`）

**Interfaces:**
- Consumes：`AuthState`、`store`、`model::CodeFilter`、`session`（不需要）。
- Produces（`admin` 内 axum handler）：
  - `create_codes(State<AuthState>, Json<CreateCodes>) -> Response`（返回码列表）
  - `list_codes(State<AuthState>, Query<CodesQuery>) -> Response`
  - `revoke_code(State<AuthState>, Json<CodeReq>) -> Response`
  - `list_users(State<AuthState>) -> Response`
  - `extend_user(State<AuthState>, Json<ExtendReq>) -> Response`
  - `disable_user(State<AuthState>, Json<DisableReq>) -> Response`
  - `set_admin(State<AuthState>, Json<SetAdminReq>) -> Response`
  - `overview(State<AuthState>) -> Response`
  - `gen_code() -> String`（16+ 字符随机码，去易混字符）

- [ ] **Step 1: 写实现**

创建 `src/web/auth/admin.rs`：

```rust
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;

use super::model::{renew_expiry, LicenseStatus};
use super::store::{self, CodeFilter};
use super::{json_error, AuthState};

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // 去掉易混 O0I1

pub fn gen_code() -> String {
    let mut rng = rand::thread_rng();
    let raw: String = (0..16).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect();
    format!("{}-{}-{}-{}", &raw[0..4], &raw[4..8], &raw[8..12], &raw[12..16])
}

#[derive(Deserialize)]
pub struct CreateCodes { pub days: i64, pub count: u32 }

pub async fn create_codes(State(st): State<AuthState>, Json(req): Json<CreateCodes>) -> Response {
    if req.days <= 0 || req.count == 0 || req.count > 500 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_params", None);
    }
    let conn = st.db.lock().unwrap();
    let mut codes = Vec::new();
    for _ in 0..req.count {
        let code = gen_code();
        if store::issue_code(&conn, &code, req.days).is_ok() {
            codes.push(code);
        }
    }
    (StatusCode::OK, Json(json!({"codes": codes}))).into_response()
}

#[derive(Deserialize)]
pub struct CodesQuery { #[serde(default)] pub filter: Option<String> }

pub async fn list_codes(State(st): State<AuthState>, Query(q): Query<CodesQuery>) -> Response {
    let filter = match q.filter.as_deref() {
        Some("used") => CodeFilter::Used,
        Some("all") => CodeFilter::All,
        _ => CodeFilter::Unused,
    };
    let conn = st.db.lock().unwrap();
    match store::list_codes(&conn, filter) {
        Ok(rows) => Json(rows).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "list_failed", None),
    }
}

#[derive(Deserialize)]
pub struct CodeReq { pub code: String }

pub async fn revoke_code(State(st): State<AuthState>, Json(req): Json<CodeReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let hit = store::revoke_code(&conn, &req.code).unwrap_or(false);
    Json(json!({"ok": hit})).into_response()
}

pub async fn list_users(State(st): State<AuthState>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    match store::list_users(&conn) {
        Ok(users) => {
            let rows: Vec<_> = users.into_iter().map(|u| {
                let status = LicenseStatus::of(u.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
                json!({
                    "id": u.id, "username": u.username, "expires_at": u.expires_at,
                    "is_admin": u.is_admin, "disabled": u.disabled, "status": status,
                })
            }).collect();
            Json(json!({"users": rows})).into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "list_failed", None),
    }
}

#[derive(Deserialize)]
pub struct ExtendReq { pub user_id: i64, pub days: i64 }

pub async fn extend_user(State(st): State<AuthState>, Json(req): Json<ExtendReq>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    let cur = match store::find_user_by_id(&conn, req.user_id) {
        Ok(Some(u)) => u.expires_at,
        _ => return json_error(StatusCode::NOT_FOUND, "user_not_found", None),
    };
    let new_exp = renew_expiry(cur, now, req.days);
    if store::set_expiry(&conn, req.user_id, new_exp).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    Json(json!({"ok": true, "expires_at": new_exp})).into_response()
}

#[derive(Deserialize)]
pub struct DisableReq { pub user_id: i64, pub disabled: bool }

pub async fn disable_user(State(st): State<AuthState>, Json(req): Json<DisableReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let _ = store::set_disabled(&conn, req.user_id, req.disabled);
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub struct SetAdminReq { pub user_id: i64, pub is_admin: bool }

pub async fn set_admin(State(st): State<AuthState>, Json(req): Json<SetAdminReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let _ = store::set_admin(&conn, req.user_id, req.is_admin);
    Json(json!({"ok": true})).into_response()
}

pub async fn overview(State(st): State<AuthState>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    let users = store::list_users(&conn).unwrap_or_default();
    let total = users.len();
    let mut active = 0;
    let mut warning = 0;
    for u in &users {
        let s = LicenseStatus::of(u.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
        if s.allows_access() { active += 1; }
        if s == LicenseStatus::Warning { warning += 1; }
    }
    Json(json!({"total": total, "active": active, "warning": warning})).into_response()
}
```

- [ ] **Step 2: 注册模块**：`src/web/auth/mod.rs` 追加 `pub mod admin;`
- [ ] **Step 3: 编译**

Run: `cargo build --lib`
Expected: 通过。

- [ ] **Step 4: Commit**

```bash
git add src/web/auth/
git commit -m "feat(auth): 管理 API 处理器"
```

---

### Task 11: 路由接线、serve 装配与登录重定向 + 中间件集成测试

**Files:**
- Modify: `src/web/mod.rs`（`router` 改签名、`serve` 装配、`index` 重定向）
- Create: `src/web/auth/routes.rs`（构建认证/授权/管理子路由 + 集成测试）
- Modify: `src/web/auth/mod.rs`（`pub mod routes;`）

**Interfaces:**
- Consumes：全部前置 Task。
- Produces：
  - `routes::mount(base: Router<AuthState>, state: AuthState) -> Router<AuthState>`（把公开/需登录/授权/管理路由并入并挂中间件；core 业务路由由调用方在 base 上先注册好并已包在授权分组内）。
  - `web::router(state: AuthState) -> Router`（不带泛型 state，已 `.with_state`）
  - `web::serve(config_path, port)` 改为加载 `[auth]` + 开库 + 建 state。

实现约定：把现有 core 业务路由从 `router()` 抽到 `licensed_routes()`，套 `require_license`+`require_login`；`index` 改为需登录组的一员但**不**要求授权（未授权也能看页面，功能靠 API 拦）。

- [ ] **Step 1: 改 `web::router` 与 `serve`**

把 `src/web/mod.rs:259-278` 的 `router()` 整体替换为：

```rust
use axum::routing::post;
use axum::middleware::from_fn_with_state;
use auth::AuthState;

/// 核心业务路由（需登录 + 授权有效）。
fn licensed_routes() -> Router<AuthState> {
    Router::new()
        .route("/api/run", get(run_handler))
        .route("/api/funds", get(funds_handler))
        .route("/api/regime", get(regime_handler))
        .route("/api/recommend", get(recommend_handler))
        .route("/api/holdings", post(holdings_handler))
        .route("/api/push/config", get(push_config_get).post(push_config_save))
        .route("/api/push/preview", post(push_preview))
        .route("/api/push/test", post(push_test))
        .route("/api/compare", post(compare_handler))
        .route("/api/optimize", post(optimize_handler))
        .route("/api/sync", post(sync_handler))
        .route("/api/stock/search", get(stock::search_handler))
        .route("/api/stock/diagnose", get(stock::diagnose_handler))
        .route("/api/stock/run", get(stock::run_handler))
        .route("/api/stock/recommend", get(stock::recommend_handler))
        .route("/api/stock/sync", post(stock::sync_handler))
}

pub fn router(state: AuthState) -> Router {
    // 公开：登录/注册页与其 API
    let public = Router::new()
        .route("/login", get(page::LOGIN_HTML_handler))
        .route("/api/auth/register", post(auth::handlers::register))
        .route("/api/auth/login", post(auth::handlers::login));

    // 需登录（不要求授权）：主页面、logout、activate、me
    let authed = Router::new()
        .route("/", get(index))
        .route("/api/auth/logout", post(auth::handlers::logout))
        .route("/api/auth/activate", post(auth::handlers::activate))
        .route("/api/auth/me", get(auth::handlers::me))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    // 需登录 + 授权：核心业务
    let licensed = licensed_routes()
        .route_layer(from_fn_with_state(state.clone(), auth::require_license))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    // 需登录 + 管理员
    let admin = auth::routes::admin_router()
        .route_layer(from_fn_with_state(state.clone(), auth::require_admin))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    public.merge(authed).merge(licensed).merge(admin).with_state(state)
}
```

> `page::LOGIN_HTML_handler` 与 `auth::routes::admin_router` 在 Task 12/14 定义；本任务先建最小占位（见 Step 2）。

把 `serve` 签名与体（`src/web/mod.rs:316-328`）改为：

```rust
pub async fn serve(config_path: std::path::PathBuf, port: u16) -> Result<()> {
    let cfg = auth::config::load_auth(&config_path);
    let conn = auth::store::open(&cfg.db_path).context("打开授权数据库失败")?;
    let state = auth::AuthState::new(conn, cfg);

    let host: std::net::IpAddr = std::env::var("XLH_BIND")
        .ok().and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let addr = std::net::SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr).await
        .with_context(|| format!("绑定 {addr} 失败"))?;
    println!("回测界面已启动：http://{addr}  (Ctrl+C 退出)");
    axum::serve(listener, router(state)).await.context("服务运行失败")?;
    Ok(())
}
```

改 `index`（`src/web/mod.rs:330-332`）为需登录组成员，未登录时由 `require_login` 拦成 401；但主页面希望**未登录跳登录页**而非 401。做法：`index` 不进 `authed` 的中间件，改为自行判断：

```rust
async fn index(State(st): State<auth::AuthState>, headers: axum::http::HeaderMap) -> Response {
    let now = chrono::Local::now().date_naive();
    let logged_in = auth::session::read_cookie(&headers)
        .and_then(|t| {
            let conn = st.db.lock().unwrap();
            auth::store::lookup_session_user(&conn, &t, now).ok().flatten()
        })
        .is_some();
    if logged_in {
        Html(page::INDEX_HTML).into_response()
    } else {
        axum::response::Redirect::to("/login").into_response()
    }
}
```

因此把 `index` 从 `authed` 移到 `public` 组（它自带判断），并在文件顶部补 `use axum::extract::State;`、`use axum::response::Redirect;`（若未引入）。更新 `router()` 里 `authed` 去掉 `/`，`public` 加 `.route("/", get(index))`。

- [ ] **Step 2: 建占位以便编译**

在 `src/web/page.rs` 末尾加登录页占位 handler（Task 12 会替换为真 HTML）：

```rust
pub async fn login_html_handler() -> axum::response::Html<&'static str> {
    axum::response::Html("<!doctype html><title>登录</title><p>login page placeholder</p>")
}
```

把上面 `router()` 中的 `page::LOGIN_HTML_handler` 改为 `page::login_html_handler`。

创建 `src/web/auth/routes.rs`：

```rust
use axum::routing::{get, post};
use axum::Router;

use super::{admin, AuthState};

/// 管理路由（挂载前由调用方套 require_admin + require_login）。
pub fn admin_router() -> Router<AuthState> {
    Router::new()
        .route("/admin", get(admin::admin_page))
        .route("/api/admin/codes", post(admin::create_codes).get(admin::list_codes))
        .route("/api/admin/codes/revoke", post(admin::revoke_code))
        .route("/api/admin/users", get(admin::list_users))
        .route("/api/admin/users/extend", post(admin::extend_user))
        .route("/api/admin/users/disable", post(admin::disable_user))
        .route("/api/admin/users/set_admin", post(admin::set_admin))
        .route("/api/admin/overview", get(admin::overview))
}
```

在 `admin.rs` 加占位后台页（Task 14 替换）：

```rust
pub async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html("<!doctype html><title>管理后台</title><p>admin placeholder</p>")
}
```

`src/web/auth/mod.rs` 追加 `pub mod routes;`。

- [ ] **Step 3: 更新 `main.rs` 的 serve 调用**

`src/main.rs:44-48` 改为传 config 路径：

```rust
        Some(Commands::Serve { port }) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(xlh::web::serve(cli.config.clone(), port))?;
            Ok(())
        }
```

（`cli.config` 是全局参数，`serve` 用它读 `[auth]`。）

- [ ] **Step 4: 写中间件集成测试**

在 `src/web/auth/routes.rs` 追加：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::auth::{store, AuthState};
    use crate::web::router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_state() -> AuthState {
        let conn = store::open_in_memory().unwrap();
        AuthState::new(conn, Default::default())
    }

    #[tokio::test]
    async fn core_api_requires_login() {
        let app = router(test_state());
        let resp = app
            .oneshot(Request::builder().uri("/api/funds").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn logged_in_but_inactive_gets_403() {
        let state = test_state();
        let (uid, token);
        {
            let conn = state.db.lock().unwrap();
            uid = store::create_user(&conn, "u", "h", false).unwrap();
            token = "tok".to_string();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/funds")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN); // 未激活 license_required
    }

    #[tokio::test]
    async fn admin_route_hidden_for_non_admin() {
        let state = test_state();
        let token = "tok2".to_string();
        {
            let conn = state.db.lock().unwrap();
            let uid = store::create_user(&conn, "u2", "h", false).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            store::create_session(&conn, &token, uid, exp).unwrap();
        }
        let app = router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/admin/overview")
                    .header("cookie", format!("xlh_session={token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib`
Expected: 全绿（含既有 200+ 测试与新增授权测试）。

- [ ] **Step 6: Commit**

```bash
git add src/web/ src/main.rs
git commit -m "feat(auth): 路由分组接线、serve 装配与中间件集成测试"
```

---

### Task 12: 登录/注册/激活前端页 + 主页状态栏

**Files:**
- Modify: `src/web/page.rs`（替换 `login_html_handler` 为完整登录页；在 `INDEX_HTML` 顶部注入状态栏脚本）

**Interfaces:**
- Consumes：`/api/auth/{register,login,logout,activate,me}`。
- Produces：`page::login_html_handler`（真实 HTML）；`INDEX_HTML` 顶部状态栏。

- [ ] **Step 1: 替换登录页 handler**

把 Task 11 加的占位 `login_html_handler` 换成：

```rust
pub async fn login_html_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(LOGIN_HTML)
}

pub const LOGIN_HTML: &str = r##"<!doctype html>
<html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>xlh · 登录</title>
<style>
 body{font-family:system-ui,sans-serif;background:#0f172a;color:#e2e8f0;display:flex;min-height:100vh;align-items:center;justify-content:center}
 .card{background:#1e293b;padding:32px;border-radius:12px;width:320px;box-shadow:0 8px 30px rgba(0,0,0,.4)}
 h1{font-size:20px;margin:0 0 16px} input{width:100%;box-sizing:border-box;margin:6px 0;padding:10px;border-radius:8px;border:1px solid #334155;background:#0f172a;color:#e2e8f0}
 button{width:100%;padding:10px;margin-top:10px;border:0;border-radius:8px;background:#3b82f6;color:#fff;font-weight:600;cursor:pointer}
 .tab{display:flex;gap:8px;margin-bottom:12px} .tab button{background:#334155} .tab button.on{background:#3b82f6}
 .msg{min-height:18px;font-size:13px;color:#f87171;margin-top:8px}
</style></head><body>
<div class="card">
  <div class="tab"><button id="tlogin" class="on" onclick="mode('login')">登录</button><button id="treg" onclick="mode('register')">注册</button></div>
  <h1 id="title">登录</h1>
  <input id="u" placeholder="用户名（3-32 位字母数字）" autocomplete="username">
  <input id="p" type="password" placeholder="密码（≥6 位）" autocomplete="current-password">
  <button onclick="submit()">提交</button>
  <div class="msg" id="msg"></div>
</div>
<script>
let M='login';
function mode(m){M=m;document.getElementById('tlogin').className=m=='login'?'on':'';document.getElementById('treg').className=m=='register'?'on':'';document.getElementById('title').textContent=m=='login'?'登录':'注册';document.getElementById('msg').textContent='';}
async function submit(){
  const u=document.getElementById('u').value.trim(),p=document.getElementById('p').value;
  const url=M=='login'?'/api/auth/login':'/api/auth/register';
  const r=await fetch(url,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({username:u,password:p})});
  const j=await r.json().catch(()=>({}));
  if(r.ok){ if(M=='register'){mode('login');document.getElementById('msg').style.color='#4ade80';document.getElementById('msg').textContent='注册成功，请登录';} else {location.href='/';} }
  else { document.getElementById('msg').style.color='#f87171';document.getElementById('msg').textContent=({invalid_login:'用户名或密码错误',username_taken:'用户名已被占用',invalid_credentials:'用户名或密码格式不符',registration_closed:'当前未开放注册'})[j.error]||('失败: '+(j.error||r.status)); }
}
</script></body></html>"##;
```

- [ ] **Step 2: 在 `INDEX_HTML` 顶部注入状态栏**

在 `page.rs` 的 `INDEX_HTML` 常量里，`<body>` 之后紧接着插入下列标记与脚本（若 `INDEX_HTML` 为 `include_str!` 的外部文件，则改对应 HTML 文件）：

```html
<div id="xlh-bar" style="position:sticky;top:0;z-index:50;display:flex;gap:12px;align-items:center;padding:8px 14px;font:13px system-ui;background:#111827;color:#e5e7eb;border-bottom:1px solid #374151">
  <span id="xlh-user"></span>
  <span id="xlh-status"></span>
  <span style="flex:1"></span>
  <input id="xlh-code" placeholder="授权码" style="padding:4px 8px;border-radius:6px;border:1px solid #374151;background:#0b1220;color:#e5e7eb">
  <button onclick="xlhActivate()" style="padding:4px 10px;border:0;border-radius:6px;background:#3b82f6;color:#fff;cursor:pointer">激活/续期</button>
  <a id="xlh-admin" href="/admin" style="display:none;color:#93c5fd">管理后台</a>
  <button onclick="xlhLogout()" style="padding:4px 10px;border:0;border-radius:6px;background:#374151;color:#fff;cursor:pointer">退出</button>
</div>
<script>
async function xlhMe(){
  const r=await fetch('/api/auth/me'); if(!r.ok){location.href='/login';return;}
  const j=await r.json();
  document.getElementById('xlh-user').textContent='👤 '+j.username;
  const color={active:'#4ade80',warning:'#facc15',grace:'#f87171',inactive:'#f87171',expired:'#f87171'}[j.status]||'#e5e7eb';
  const text={active:'授权正常 · 到期 '+j.expires_at,warning:'⚠ '+j.remaining_days+' 天后到期，请续期',grace:'⚠ 已到期，宽限期内（尽快续期）',inactive:'未激活，请输入授权码',expired:'已过期，请续期'}[j.status]||j.status;
  const s=document.getElementById('xlh-status'); s.textContent=text; s.style.color=color;
  document.getElementById('xlh-admin').style.display=j.is_admin?'inline':'none';
}
async function xlhActivate(){
  const code=document.getElementById('xlh-code').value.trim(); if(!code)return;
  const r=await fetch('/api/auth/activate',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code})});
  const j=await r.json().catch(()=>({}));
  if(r.ok){alert('激活成功，到期日 '+j.expires_at);location.reload();}
  else{alert(({code_not_found:'授权码不存在',code_used:'授权码已被使用',code_revoked:'授权码已作废'})[j.error]||('激活失败: '+(j.error||r.status)));}
}
async function xlhLogout(){await fetch('/api/auth/logout',{method:'POST'});location.href='/login';}
// 核心请求 403 时提示激活
const _f=window.fetch; window.fetch=async(...a)=>{const r=await _f(...a);if(r.status===403){const c=r.clone();const j=await c.json().catch(()=>({}));if(j.error==='license_required'||j.error==='expired'){document.getElementById('xlh-code').focus();}}return r;};
xlhMe();
</script>
```

- [ ] **Step 3: 编译并手动核对**

Run: `cargo build`
Expected: 通过。

手动冒烟（可选，联网无关）：

```bash
cargo run -- serve --port 8080 &
# 浏览器打开 http://127.0.0.1:8080 应跳转 /login；注册→登录→顶栏显示“未激活”。
```

- [ ] **Step 4: Commit**

```bash
git add src/web/page.rs
git commit -m "feat(auth): 登录注册激活页与主页授权状态栏"
```

---

### Task 13: 管理后台页面 `admin_page`

**Files:**
- Modify: `src/web/auth/admin.rs`（替换占位 `admin_page` 为完整 HTML）

**Interfaces:**
- Consumes：`/api/admin/*`。
- Produces：`admin::admin_page`（真实 HTML）。

- [ ] **Step 1: 替换 `admin_page`**

把 Task 11 加的占位 `admin_page` 换为：

```rust
pub async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(ADMIN_HTML)
}

const ADMIN_HTML: &str = r##"<!doctype html>
<html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>xlh · 管理后台</title>
<style>
 body{font-family:system-ui,sans-serif;background:#0f172a;color:#e2e8f0;margin:0;padding:20px}
 h1{font-size:20px} h2{font-size:16px;margin-top:28px;border-bottom:1px solid #334155;padding-bottom:6px}
 table{width:100%;border-collapse:collapse;margin-top:10px;font-size:13px} th,td{text-align:left;padding:6px 8px;border-bottom:1px solid #1e293b}
 input,button{padding:6px 10px;border-radius:6px;border:1px solid #334155;background:#0b1220;color:#e2e8f0}
 button{background:#3b82f6;border:0;color:#fff;cursor:pointer;margin-left:4px}
 code{background:#1e293b;padding:2px 6px;border-radius:4px}
 a{color:#93c5fd}
</style></head><body>
<h1>xlh 管理后台 · <a href="/">返回主界面</a></h1>
<div id="ov"></div>

<h2>发码</h2>
<div>天数 <input id="days" type="number" value="365" style="width:90px"> 数量 <input id="count" type="number" value="1" style="width:70px">
<button onclick="gen()">生成</button></div>
<pre id="newcodes" style="background:#1e293b;padding:10px;border-radius:8px;white-space:pre-wrap"></pre>
<div><button onclick="loadCodes('unused')">未用</button><button onclick="loadCodes('used')">已用</button><button onclick="loadCodes('all')">全部</button></div>
<table id="codes"><thead><tr><th>码</th><th>天数</th><th>使用者</th><th>状态</th><th></th></tr></thead><tbody></tbody></table>

<h2>用户</h2>
<table id="users"><thead><tr><th>ID</th><th>用户名</th><th>状态</th><th>到期</th><th>操作</th></tr></thead><tbody></tbody></table>

<script>
async function api(u,m,b){const r=await fetch(u,{method:m||'GET',headers:b?{'content-type':'application/json'}:{},body:b?JSON.stringify(b):undefined});if(r.status===404){document.body.innerHTML='<h1>403</h1>';return null;}return r.json().catch(()=>({}));}
async function ov(){const j=await api('/api/admin/overview');if(j)document.getElementById('ov').textContent=`用户 ${j.total} · 在用 ${j.active} · 临期 ${j.warning}`;}
async function gen(){const days=+document.getElementById('days').value,count=+document.getElementById('count').value;const j=await api('/api/admin/codes','POST',{days,count});document.getElementById('newcodes').textContent=(j.codes||[]).join('\n');loadCodes('unused');}
async function loadCodes(f){const j=await api('/api/admin/codes?filter='+f);const tb=document.querySelector('#codes tbody');tb.innerHTML='';(j||[]).forEach(c=>{const st=c.revoked?'已作废':(c.used_by?'已用':'未用');tb.innerHTML+=`<tr><td><code>${c.code}</code></td><td>${c.days}</td><td>${c.used_by||''}</td><td>${st}</td><td>${c.used_by||c.revoked?'':`<button onclick="revoke('${c.code}')">作废</button>`}</td></tr>`;});}
async function revoke(code){await api('/api/admin/codes/revoke','POST',{code});loadCodes('unused');}
async function loadUsers(){const j=await api('/api/admin/users');const tb=document.querySelector('#users tbody');tb.innerHTML='';(j.users||[]).forEach(u=>{tb.innerHTML+=`<tr><td>${u.id}</td><td>${u.username}${u.is_admin?' 👑':''}</td><td>${u.status}${u.disabled?' (封禁)':''}</td><td>${u.expires_at||'—'}</td><td>
  <input type="number" value="30" style="width:64px" id="d${u.id}"><button onclick="ext(${u.id})">续期</button>
  <button onclick="dis(${u.id},${!u.disabled})">${u.disabled?'解封':'封禁'}</button>
  <button onclick="adm(${u.id},${!u.is_admin})">${u.is_admin?'撤管理':'设管理'}</button></td></tr>`;});}
async function ext(id){const days=+document.getElementById('d'+id).value;await api('/api/admin/users/extend','POST',{user_id:id,days});loadUsers();ov();}
async function dis(id,d){await api('/api/admin/users/disable','POST',{user_id:id,disabled:d});loadUsers();}
async function adm(id,a){await api('/api/admin/users/set_admin','POST',{user_id:id,is_admin:a});loadUsers();}
ov();loadCodes('unused');loadUsers();
</script></body></html>"##;
```

- [ ] **Step 2: 编译**

Run: `cargo build`
Expected: 通过。

- [ ] **Step 3: Commit**

```bash
git add src/web/auth/admin.rs
git commit -m "feat(auth): 网页管理后台页面"
```

---

### Task 14: CLI 子命令（引导管理员 / 发码 / 列表）

**Files:**
- Modify: `src/main.rs`（`Commands` 加子命令 + 分发）
- Create: `src/web/auth/cli.rs`（CLI 逻辑，复用 store）
- Modify: `src/web/auth/mod.rs`（`pub mod cli;`）

**Interfaces:**
- Consumes：`config::load_auth`、`store`、`password`、`admin::gen_code`。
- Produces：`cli::{admin_create, license_issue, license_list, user_list}`（同步函数，`anyhow::Result<()>`）。

- [ ] **Step 1: 写 CLI 逻辑**

创建 `src/web/auth/cli.rs`：

```rust
use std::path::Path;
use anyhow::{anyhow, Result};

use super::store::{self, CodeFilter};
use super::{admin, config, password};

fn open_db(config_path: &Path) -> Result<rusqlite::Connection> {
    let cfg = config::load_auth(config_path);
    store::open(&cfg.db_path)
}

/// 创建首个/追加管理员；从环境变量 XLH_ADMIN_PASSWORD 读密码（避免交互 TTY 依赖）。
pub fn admin_create(config_path: &Path, username: &str) -> Result<()> {
    let pw = std::env::var("XLH_ADMIN_PASSWORD")
        .map_err(|_| anyhow!("请用环境变量 XLH_ADMIN_PASSWORD 提供管理员密码"))?;
    if pw.chars().count() < 6 { return Err(anyhow!("密码至少 6 位")); }
    let conn = open_db(config_path)?;
    let hash = password::hash(&pw)?;
    let id = store::create_user(&conn, username, &hash, true)
        .map_err(|_| anyhow!("创建失败：用户名 {username} 可能已存在"))?;
    println!("✓ 管理员已创建：{username} (id={id})");
    Ok(())
}

pub fn license_issue(config_path: &Path, days: i64, count: u32) -> Result<()> {
    let conn = open_db(config_path)?;
    for _ in 0..count {
        let code = admin::gen_code();
        store::issue_code(&conn, &code, days)?;
        println!("{code}  (+{days}天)");
    }
    Ok(())
}

pub fn license_list(config_path: &Path, filter: &str) -> Result<()> {
    let conn = open_db(config_path)?;
    let f = match filter { "used" => CodeFilter::Used, "all" => CodeFilter::All, _ => CodeFilter::Unused };
    for c in store::list_codes(&conn, f)? {
        let st = if c.revoked { "作废" } else if c.used_by.is_some() { "已用" } else { "未用" };
        println!("{}  {:>4}天  {}  用户{}", c.code, c.days, st, c.used_by.map(|u| u.to_string()).unwrap_or_else(|| "-".into()));
    }
    Ok(())
}

pub fn user_list(config_path: &Path) -> Result<()> {
    let conn = open_db(config_path)?;
    for u in store::list_users(&conn)? {
        println!("{:>3}  {:<20} 到期 {}  {}{}",
            u.id, u.username,
            u.expires_at.map(|e| e.to_string()).unwrap_or_else(|| "未激活".into()),
            if u.is_admin { "[管理员]" } else { "" },
            if u.disabled { "[封禁]" } else { "" });
    }
    Ok(())
}
```

`src/web/auth/mod.rs` 追加 `pub mod cli;`。

- [ ] **Step 2: 接线 main.rs 子命令**

在 `src/main.rs` 的 `enum Commands` 内追加：

```rust
    /// 创建管理员（密码经环境变量 XLH_ADMIN_PASSWORD 传入）
    Admin {
        #[command(subcommand)]
        action: AdminCmd,
    },
    /// 授权码管理
    License {
        #[command(subcommand)]
        action: LicenseCmd,
    },
    /// 列出用户
    User {
        #[command(subcommand)]
        action: UserCmd,
    },
```

在文件加子枚举：

```rust
#[derive(Subcommand)]
enum AdminCmd {
    /// 创建管理员账号
    Create { #[arg(long)] username: String },
}

#[derive(Subcommand)]
enum LicenseCmd {
    /// 生成授权码
    Issue { #[arg(long)] days: i64, #[arg(long, default_value_t = 1)] count: u32 },
    /// 列出授权码
    List { #[arg(long, default_value = "unused")] filter: String },
}

#[derive(Subcommand)]
enum UserCmd {
    /// 列出所有用户
    List,
}
```

在 `main()` 的 `match cli.command` 增加分支：

```rust
        Some(Commands::Admin { action }) => match action {
            AdminCmd::Create { username } => xlh::web::auth::cli::admin_create(&cli.config, &username),
        },
        Some(Commands::License { action }) => match action {
            LicenseCmd::Issue { days, count } => xlh::web::auth::cli::license_issue(&cli.config, days, count),
            LicenseCmd::List { filter } => xlh::web::auth::cli::license_list(&cli.config, &filter),
        },
        Some(Commands::User { action }) => match action {
            UserCmd::List => xlh::web::auth::cli::user_list(&cli.config),
        },
```

- [ ] **Step 3: 编译并冒烟**

Run: `cargo build`
Expected: 通过。

冒烟：

```bash
XLH_ADMIN_PASSWORD=secret123 cargo run -- admin create --username admin
cargo run -- license issue --days 365 --count 3
cargo run -- license list
cargo run -- user list
```
Expected: 依次打印管理员创建成功、3 个授权码、码列表、用户列表（含 admin）。

- [ ] **Step 4: Commit**

```bash
git add src/main.rs src/web/auth/
git commit -m "feat(auth): CLI 引导管理员/发码/列表子命令"
```

---

### Task 15: 部署与文档

**Files:**
- Modify: `docker-compose.prod.yml`（挂 `data` 卷）
- Modify: `docker-compose.yml`（同步）
- Modify: `.gitignore`（忽略 `data/`）
- Modify: `config.toml`（追加 `[auth]` 注释示例）
- Modify: `README.md`（授权体系使用说明）

**Interfaces:** 无代码接口，纯部署/文档。

- [ ] **Step 1: 挂 data 卷**

`docker-compose.prod.yml` 的 `xlh-web.volumes` 追加一行：

```yaml
      - ./data:/app/data
```

`docker-compose.yml` 若有 web 服务卷，同样追加 `- ./data:/app/data`。

- [ ] **Step 2: gitignore**

在 `.gitignore` 追加：

```
/data/
```

- [ ] **Step 3: config.toml 示例**

在 `config.toml` 末尾追加（注释形式，缺省即用默认值）：

```toml
# [auth]
# db_path = "data/xlh.db"
# open_registration = true
# warn_days = 7
# grace_days = 3
# session_ttl_days = 30
```

- [ ] **Step 4: README**

在 `README.md` 加一节「授权与收费」，写明：
- 首次部署引导管理员：`XLH_ADMIN_PASSWORD=... xlh admin create --username admin`
- 发码：`xlh license issue --days 365 --count 10` 或进 `/admin` 网页后台
- 客户流程：注册 → 顶栏输入授权码激活 → 到期前顶栏提醒、到期后进宽限期、宽限结束锁核心功能
- `data/` 目录需持久化（docker 卷）

- [ ] **Step 5: 全量测试 + 提交**

Run: `cargo test`
Expected: 全绿。

```bash
git add docker-compose.prod.yml docker-compose.yml .gitignore config.toml README.md
git commit -m "docs(auth): 部署卷与授权体系使用说明"
```

---

## Self-Review

**1. Spec coverage：**
- SaaS/账号/授权码激活/开放注册/手动发码 → Task 9（register/login/activate）+ Task 14（发码 CLI）✅
- 状态机（含临期/宽限）→ Task 2 + Task 8（require_license）✅
- SQLite 三表 → Task 5/6/7 ✅
- 一次性码并发保护 → Task 6（条件更新 + changes()==1 断言，测试覆盖二次激活）✅
- argon2 密码 → Task 3 ✅
- 会话 Cookie → Task 4 + Task 9 ✅
- 中间件分组拦截 → Task 8 + Task 11 ✅
- 网页管理后台（发码/用户/概览）→ Task 10 + Task 13 ✅
- 首管理员 CLI 引导 → Task 14 ✅
- 到期前提醒 + 宽限期 → Task 2（状态）+ Task 12（顶栏文案）✅
- 配置 `[auth]` → Task 1 + Task 15 ✅
- 部署卷持久化 → Task 15 ✅
- 对现有代码零改动（仅 mod/main/page/config 接线）→ 各 Task 未触碰基金/股票业务逻辑 ✅

**2. Placeholder scan：** Task 11 显式用「占位 handler」并在 Task 12/13 替换为真实实现，属于有意的分步接线，非 TODO 遗留；其余步骤均含完整代码。

**3. Type consistency：**
- `AuthState{db,cfg}`、`CurrentUser{id,username,is_admin,expires_at,disabled}` 全程一致。
- `store::activate(&mut Connection, code, user_id)` 在 Task 6 定义、Task 9 调用签名一致。
- `LicenseStatus::of(expires_at, now, warn_days, grace_days)` 在 Task 2/8/9/10 调用一致。
- `session::{new_token, read_cookie, set_cookie_header, clear_cookie_header, COOKIE_NAME}` 定义与使用一致。
- `router(state: AuthState)` 在 Task 11 改签名、Task 11 serve 与集成测试、main.rs 调用一致。

> 注意实现时的两个易错点已在正文标红：①中间件 `.route_layer` 叠放顺序（login 后加=外层先跑）；②`index` 自带登录判断以实现「未登录跳 /login」而非 401。

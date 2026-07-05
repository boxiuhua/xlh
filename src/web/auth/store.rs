use std::path::Path;
use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension};

use super::model::{renew_expiry, CodeRow, User};

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
    // 无法解析的到期时间视为已过期（fail-closed），绝不放行损坏会话。
    let session_exp: NaiveDate = match exp.parse() {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };
    if session_exp < now {
        return Ok(None); // 会话过期
    }
    find_user_by_id(conn, user_id)
}

pub fn delete_session(conn: &Connection, token: &str) -> Result<()> {
    conn.execute("DELETE FROM sessions WHERE token = ?1", [token])?;
    Ok(())
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

    #[test]
    fn corrupt_session_expiry_fails_closed() {
        // 无法解析的 expires_at 必须被视为过期（fail-closed），而非放行。
        let conn = open_in_memory().unwrap();
        let uid = create_user(&conn, "u", "h", false).unwrap();
        let now = chrono::Local::now().date_naive();
        let created = now.to_string();
        conn.execute(
            "INSERT INTO sessions (token, user_id, expires_at, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["bad", uid, "not-a-date", created],
        )
        .unwrap();
        assert!(
            lookup_session_user(&conn, "bad", now).unwrap().is_none(),
            "损坏的到期时间必须失败关闭（返回 None）"
        );
    }
}

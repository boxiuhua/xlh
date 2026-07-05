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

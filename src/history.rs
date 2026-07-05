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
    conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
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

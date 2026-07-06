//! 每用户推送配置存储（push_configs 表）。
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::config::PushConfig;

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

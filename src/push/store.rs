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

-- 守护进程心跳。单行表（id 恒为 1）。
--
-- 为什么需要它：推送守护是**独立进程**（xlh-push 容器，compose 里还是可选 profile）。
-- 没有它，Web 上改 cron、点保存、看到「已保存」，一切都像正常 —— 但根本没有进程在读配置，
-- 于是永远不会推送，而用户完全无从知晓。这种静默失败最伤人。
-- 有了心跳，Web 就能直接告诉他「守护没在跑，配置存了也不会推」。
CREATE TABLE IF NOT EXISTS push_heartbeat (
  id      INTEGER PRIMARY KEY CHECK (id = 1),
  beat_at TEXT NOT NULL
);
"#;

/// 心跳超过这个秒数就判定守护已死。守护每 60s 一跳，取 3 倍余量避免误报。
pub const HEARTBEAT_STALE_SECS: i64 = 180;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建 push_configs / push_heartbeat 表失败")?;
    Ok(())
}

/// 守护进程每轮调用一次，宣告自己还活着。
pub fn beat(conn: &Connection) -> Result<()> {
    let now = chrono::Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO push_heartbeat (id, beat_at) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET beat_at = ?1",
        rusqlite::params![now],
    )?;
    Ok(())
}

/// 最近一次心跳时刻。守护从未跑过 → `None`。
pub fn last_beat(conn: &Connection) -> Result<Option<chrono::DateTime<chrono::Local>>> {
    let s: Option<String> = conn
        .query_row("SELECT beat_at FROM push_heartbeat WHERE id = 1", [], |r| r.get(0))
        .optional()?;
    Ok(s.and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|t| t.with_timezone(&chrono::Local)))
}

/// 守护是否活着（心跳在 `HEARTBEAT_STALE_SECS` 内）。
pub fn daemon_alive(conn: &Connection, now: chrono::DateTime<chrono::Local>) -> bool {
    match last_beat(conn) {
        Ok(Some(t)) => (now - t).num_seconds() <= HEARTBEAT_STALE_SECS,
        _ => false,
    }
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
mod heartbeat_tests {
    use super::*;
    use chrono::Duration;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::web::auth::store::migrate(&c).unwrap();
        migrate(&c).unwrap();
        c
    }

    /// 守护从未跑过 → 必须判定为「未运行」。
    ///
    /// 这是最要命的一种情况：推送守护是可选容器，很多人只部署了 Web。
    /// 若这里错判成「活着」，用户会继续以为是 cron 不生效，永远找不到真正的原因。
    #[test]
    fn never_started_means_dead() {
        let c = db();
        assert_eq!(last_beat(&c).unwrap(), None);
        assert!(!daemon_alive(&c, chrono::Local::now()), "从未心跳过 → 必须判定为未运行");
    }

    #[test]
    fn fresh_beat_means_alive() {
        let c = db();
        beat(&c).unwrap();
        let now = chrono::Local::now();
        assert!(last_beat(&c).unwrap().is_some());
        assert!(daemon_alive(&c, now));
    }

    /// 守护挂了（容器被杀 / 崩溃）→ 心跳变陈旧 → 必须判定为已死。
    #[test]
    fn stale_beat_means_dead() {
        let c = db();
        beat(&c).unwrap();
        let now = chrono::Local::now();
        // 守护每 60s 一跳；容忍 180s。刚过阈值 → 死
        assert!(daemon_alive(&c, now + Duration::seconds(HEARTBEAT_STALE_SECS)), "阈值内仍算活");
        assert!(!daemon_alive(&c, now + Duration::seconds(HEARTBEAT_STALE_SECS + 1)),
                "超过 {HEARTBEAT_STALE_SECS}s 无心跳 → 必须判定为已死");
    }

    /// 心跳是单行表：反复 beat 只更新，不堆积。
    #[test]
    fn beat_is_idempotent_single_row() {
        let c = db();
        for _ in 0..5 { beat(&c).unwrap(); }
        let n: i64 = c.query_row("SELECT count(*) FROM push_heartbeat", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "心跳表恒为单行");
    }
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

    #[test]
    fn delete_user_cascades_push_config() {
        let mut conn = setup();
        let uid = auth_store::create_user(&conn, "u", "h", false).unwrap();
        upsert(&conn, uid, &sample_cfg()).unwrap();
        auth_store::delete_user(&mut conn, uid).unwrap();
        assert!(get(&conn, uid).unwrap().is_none(), "删号应连带清除推送配置");
    }
}

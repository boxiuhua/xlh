//! 交易全局设置(存于 `trade_settings`):管理员总开关、工单签名密钥。
//! Web 进程与推送守护进程共用同一个库,因此两边读到的是同一份设置。

use crate::trade::model::fmt_ts;
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};

const KILL_SWITCH: &str = "kill_switch";
const LINK_SECRET: &str = "link_secret";

fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM trade_settings WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

fn put(conn: &Connection, key: &str, value: &str, now: NaiveDateTime) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, fmt_ts(now)],
    )?;
    Ok(())
}

/// 管理员总开关:打开后不再生成任何工单、不允许确认(回填成交不受限)。
pub fn kill_switch(conn: &Connection) -> Result<bool> {
    Ok(get(conn, KILL_SWITCH)?.as_deref() == Some("1"))
}

pub fn set_kill_switch(conn: &Connection, on: bool, now: NaiveDateTime) -> Result<()> {
    put(conn, KILL_SWITCH, if on { "1" } else { "0" }, now)
}

/// 工单签名密钥(32 字节)。首次调用生成;`INSERT OR IGNORE` 后再读,
/// 两个进程同时首次调用也只会留下一把。
pub fn link_secret(conn: &Connection) -> Result<Vec<u8>> {
    if get(conn, LINK_SECRET)?.is_none() {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        conn.execute(
            "INSERT OR IGNORE INTO trade_settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![
                LINK_SECRET,
                hex(&buf),
                fmt_ts(chrono::Local::now().naive_local())
            ],
        )?;
    }
    let s = get(conn, LINK_SECRET)?.ok_or_else(|| anyhow!("签名密钥写入失败"))?;
    unhex(&s)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return Err(anyhow!("签名密钥格式错误"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("签名密钥格式错误: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::store;
    use chrono::NaiveDateTime;
    use rusqlite::Connection;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }
    fn now() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 23)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
    }

    #[test]
    fn kill_switch_defaults_off_and_toggles() {
        let c = db();
        assert!(!kill_switch(&c).unwrap());
        set_kill_switch(&c, true, now()).unwrap();
        assert!(kill_switch(&c).unwrap());
        set_kill_switch(&c, false, now()).unwrap();
        assert!(!kill_switch(&c).unwrap());
    }

    #[test]
    fn link_secret_is_generated_once_and_stable() {
        let c = db();
        let a = link_secret(&c).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(link_secret(&c).unwrap(), a, "第二次读到同一把");
    }
}

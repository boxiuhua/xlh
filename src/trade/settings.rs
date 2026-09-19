//! 交易全局设置(存于 `trade_settings`):管理员总开关、工单签名密钥。
//! Web 进程与推送守护进程共用同一个库,因此两边读到的是同一份设置。

use crate::trade::model::{fmt_ts, parse_ts};
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

const KILL_SWITCH: &str = "kill_switch";
/// 总开关最近一次操作人(用户 id 的字符串形式;未记录时写空串)。
const KILL_SWITCH_BY: &str = "kill_switch_by";
const LINK_SECRET: &str = "link_secret";
/// 签名密钥字节数(design decision 3)。
const SECRET_LEN: usize = 32;

fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT value FROM trade_settings WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?)
}

/// 同 `get`,但一并取回该键的 `updated_at`(留痕读取用,一次查询取齐)。
fn get_with_meta(conn: &Connection, key: &str) -> Result<Option<(String, NaiveDateTime)>> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT value, updated_at FROM trade_settings WHERE key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(v, t)| Ok((v, parse_ts(&t)?))).transpose()
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

/// 总开关留痕:`by` 为操作的管理员 id(`None` 写空串,例如非管理员触发的内部调用)。
pub fn set_kill_switch(
    conn: &Connection,
    on: bool,
    by: Option<i64>,
    now: NaiveDateTime,
) -> Result<()> {
    put(conn, KILL_SWITCH, if on { "1" } else { "0" }, now)?;
    let by_str = by.map(|id| id.to_string()).unwrap_or_default();
    put(conn, KILL_SWITCH_BY, &by_str, now)
}

/// 总开关状态:开关本身、最近一次操作人、该次操作的时间。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KillSwitchState {
    pub on: bool,
    pub by: Option<i64>,
    pub updated_at: Option<NaiveDateTime>,
}

pub fn kill_switch_state(conn: &Connection) -> Result<KillSwitchState> {
    let on = kill_switch(conn)?;
    let by_meta = get_with_meta(conn, KILL_SWITCH_BY)?;
    let (by, updated_at) = match by_meta {
        Some((v, t)) if !v.is_empty() => (Some(v.parse::<i64>()?), Some(t)),
        Some((_, t)) => (None, Some(t)),
        None => (None, None),
    };
    Ok(KillSwitchState { on, by, updated_at })
}

/// 工单签名密钥(32 字节)。首次调用生成;`INSERT OR IGNORE` 后再读,
/// 两个进程同时首次调用也只会留下一把。
pub fn link_secret(conn: &Connection) -> Result<Vec<u8>> {
    if get(conn, LINK_SECRET)?.is_none() {
        let mut buf = [0u8; SECRET_LEN];
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

/// 解码十六进制密钥,且必须恰好 32 字节:过短 / 空的密钥会让签名可被伪造。
/// 按字节处理,非 ASCII 输入返回错误而不会在字符中间切片 panic。
fn unhex(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.len() != SECRET_LEN * 2 {
        return Err(anyhow!("签名密钥长度错误:须为 {SECRET_LEN} 字节"));
    }
    let nibble = |b: u8| {
        (b as char)
            .to_digit(16)
            .ok_or_else(|| anyhow!("签名密钥格式错误"))
    };
    bytes
        .chunks_exact(2)
        .map(|p| Ok((nibble(p[0])? * 16 + nibble(p[1])?) as u8))
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
        set_kill_switch(&c, true, None, now()).unwrap();
        assert!(kill_switch(&c).unwrap());
        set_kill_switch(&c, false, None, now()).unwrap();
        assert!(!kill_switch(&c).unwrap());
    }

    #[test]
    fn kill_switch_records_who_and_when() {
        let c = db();
        assert!(!kill_switch_state(&c).unwrap().on);
        set_kill_switch(&c, true, Some(9), now()).unwrap();
        let s = kill_switch_state(&c).unwrap();
        assert_eq!((s.on, s.by, s.updated_at), (true, Some(9), Some(now())));
    }

    #[test]
    fn kill_switch_state_defaults_and_clears_by_on_none() {
        let c = db();
        let s = kill_switch_state(&c).unwrap();
        assert_eq!((s.on, s.by, s.updated_at), (false, None, None));
        set_kill_switch(&c, true, Some(9), now()).unwrap();
        let later = now() + chrono::Duration::minutes(1);
        set_kill_switch(&c, false, None, later).unwrap();
        let s = kill_switch_state(&c).unwrap();
        assert_eq!((s.on, s.by, s.updated_at), (false, None, Some(later)));
    }

    #[test]
    fn link_secret_rejects_stored_secret_that_is_not_32_bytes() {
        let c = db();
        // 过短 / 空的密钥会让签名可被伪造:必须报错而不是照用
        for bad in ["", "abcd", &"ab".repeat(31), &"ab".repeat(33)] {
            put(&c, LINK_SECRET, bad, now()).unwrap();
            assert!(link_secret(&c).is_err(), "长度不对应报错: {bad:?}");
        }
        // 非 ASCII(按字节切片会在字符中间切断)不得 panic
        put(&c, LINK_SECRET, &"é".repeat(32), now()).unwrap();
        assert!(link_secret(&c).is_err());
        put(&c, LINK_SECRET, &"zz".repeat(32), now()).unwrap();
        assert!(link_secret(&c).is_err());
        put(&c, LINK_SECRET, &"0f".repeat(32), now()).unwrap();
        assert_eq!(link_secret(&c).unwrap(), vec![0x0f; 32]);
    }

    #[test]
    fn link_secret_is_generated_once_and_stable() {
        let c = db();
        let a = link_secret(&c).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(link_secret(&c).unwrap(), a, "第二次读到同一把");
    }
}

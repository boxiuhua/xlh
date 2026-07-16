//! 实时数据存储（`data/realtime.db`）。
//!
//! # 为什么独立于 data/xlh.db
//!
//! 盘中每 10 分钟写 5400 行，不应与登录会话争同一 WAL 锁；且此库是纯派生
//! 数据，损坏可直接删除重建，不涉及用户资产。
//!
//! # 分层保留：ticks 10 天，signals 永久
//!
//! 两者性质完全不同：
//!
//! - `ticks` 108 万行滚动，只为算异动与量能基准。10 天足够，再多是浪费 ——
//!   其中 99.9% 是「未触发任何信号的普通股票的普通快照」，写完永不再读。
//! - `signals` 每天几十行、一年约 1 万行。它是**验证阈值有效性的唯一依据**。
//!
//! `signals` 的保留期刻意不做成配置项：不给「一改配置就把验证依据清掉」
//! 留任何路径。若它随 ticks 一起滚动删除，就永久失去了回答「这套阈值到底
//! 有没有用」的能力 —— 而那是本项目的头号风险。
use std::path::Path;
use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime, Timelike};
use rusqlite::{Connection, OptionalExtension};

use super::snapshot::Tick;
use super::movers::{Divergence, Horizon, Mover};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS ticks (
  code       TEXT NOT NULL,
  ts         INTEGER NOT NULL,
  price      REAL NOT NULL,
  change_pct REAL NOT NULL,
  volume     REAL NOT NULL,
  amount     REAL NOT NULL,
  turnover   REAL NOT NULL,
  vol_ratio  REAL NOT NULL,
  PRIMARY KEY (code, ts)
);
CREATE INDEX IF NOT EXISTS idx_ticks_ts ON ticks(ts);
CREATE TABLE IF NOT EXISTS signals (
  id            INTEGER PRIMARY KEY,
  code          TEXT NOT NULL,
  name          TEXT NOT NULL,
  ts            INTEGER NOT NULL,
  trigger_price REAL NOT NULL,
  jump_pct      REAL NOT NULL,
  vol_surge_x   REAL NOT NULL,
  main_net      REAL,
  main_net_pct  REAL,
  divergence    TEXT NOT NULL,
  horizon_tag   TEXT NOT NULL,
  baseline      TEXT NOT NULL,
  pushed        INTEGER NOT NULL DEFAULT 0,
  close_ret     REAL,
  ret_t1        REAL,
  ret_t5        REAL,
  UNIQUE (code, ts)
);
CREATE INDEX IF NOT EXISTS idx_signals_ts ON signals(ts);
CREATE TABLE IF NOT EXISTS non_trading_days (
  day TEXT PRIMARY KEY
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
    conn.busy_timeout(std::time::Duration::from_secs(5)).ok();
    // auto_vacuum 须在建表前设置才生效，且必须是 INCREMENTAL 才能配合 prune 里的
    // incremental_vacuum —— 否则删了 10 天前的数据，SQLite 只标记页空闲、文件不缩，
    // 长期运行磁盘只涨不落。
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL").ok();
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL").ok();
    migrate(&conn)?;
    Ok(conn)
}

/// 批量写入快照。同一时点重复抓取（重试）不会写重 —— (code, ts) 是主键。
pub fn insert_ticks(conn: &mut Connection, ticks: &[Tick]) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO ticks (code, ts, price, change_pct, volume, amount, turnover, vol_ratio)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)")?;
        for t in ticks {
            stmt.execute(rusqlite::params![
                t.code, t.ts.and_utc().timestamp(), t.price, t.change_pct,
                t.volume, t.amount, t.turnover, t.vol_ratio])?;
            n += 1;
        }
    }
    tx.commit()?;
    Ok(n)
}

/// 取某只股票最近 n 个快照，按时间倒序（最新在前）：(ts, price, volume)。
pub fn recent_ticks(conn: &Connection, code: &str, n: usize) -> Result<Vec<(NaiveDateTime, f64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT ts, price, volume FROM ticks WHERE code = ?1 ORDER BY ts DESC LIMIT ?2")?;
    let rows = stmt.query_map(rusqlite::params![code, n as i64], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?))
    })?;
    Ok(rows.filter_map(|r| r.ok())
        .filter_map(|(ts, p, v)| chrono::DateTime::from_timestamp(ts, 0).map(|d| (d.naive_utc(), p, v)))
        .collect())
}

/// 某只股票在「历史同一时点」（同 时:分，**不含今天**）的成交量序列，用于量能基准。
///
/// 排除今天是硬性的：基准必须是历史。把今天算进去等于用当下解释当下 ——
/// 一只正在异动的股票会自己抬高自己的基准，把自己的异动抹平。
pub fn same_slot_volumes(
    conn: &Connection, code: &str, hhmm: (u32, u32), today: NaiveDate, since: NaiveDate,
) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT ts, volume FROM ticks WHERE code = ?1 AND ts >= ?2 ORDER BY ts ASC")?;
    let since_ts = since.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let rows = stmt.query_map(rusqlite::params![code, since_ts], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;
    let mut out = Vec::new();
    for (ts, vol) in rows.filter_map(|r| r.ok()) {
        let Some(dt) = chrono::DateTime::from_timestamp(ts, 0).map(|d| d.naive_utc()) else { continue };
        if dt.date() == today { continue }
        if (dt.hour(), dt.minute()) == hhmm { out.push(vol) }
    }
    Ok(out)
}

/// 删除 retain_days 之前的 ticks，并回收文件空间。
///
/// **只动 ticks**。signals 永久保留 —— 它是验证阈值的唯一依据。
pub fn prune(conn: &Connection, now: NaiveDateTime, retain_days: i64) -> Result<usize> {
    let cutoff = (now - chrono::Duration::days(retain_days)).and_utc().timestamp();
    let n = conn.execute("DELETE FROM ticks WHERE ts < ?1", [cutoff])?;
    // 不可省略：SQLite 删行后只标记页为空闲、文件不缩。
    conn.pragma_update(None, "incremental_vacuum", 0).ok();
    Ok(n)
}

pub fn mark_non_trading(conn: &Connection, day: NaiveDate) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO non_trading_days (day) VALUES (?1)", [day.to_string()])?;
    Ok(())
}

pub fn is_non_trading(conn: &Connection, day: NaiveDate) -> Result<bool> {
    let n: Option<i64> = conn.query_row(
        "SELECT 1 FROM non_trading_days WHERE day = ?1", [day.to_string()], |r| r.get(0))
        .optional()?;
    Ok(n.is_some())
}

/// 写入信号。同一 (code, ts) 幂等 —— 重跑同一时点不会写重。
pub fn insert_signal(conn: &Connection, m: &Mover, pushed: bool) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO signals
         (code, name, ts, trigger_price, jump_pct, vol_surge_x, main_net, main_net_pct,
          divergence, horizon_tag, baseline, pushed)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        rusqlite::params![
            m.code, m.name, m.ts.and_utc().timestamp(), m.price, m.jump_pct, m.vol_surge_x,
            m.main_net, m.main_net_pct, m.divergence.as_str(), m.horizon.as_str(),
            m.baseline.as_str(), pushed as i64])?;
    Ok(())
}

/// 今天已推送过的股票代码集合。用于「同一只股票当日只推一次」的限流。
pub fn pushed_today(conn: &Connection, today: NaiveDate) -> Result<std::collections::HashSet<String>> {
    let start = today.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let end = start + 86400;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT code FROM signals WHERE pushed = 1 AND ts >= ?1 AND ts < ?2")?;
    let rows = stmt.query_map([start, end], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SignalRow {
    pub id: i64,
    pub code: String,
    pub name: String,
    pub ts: NaiveDateTime,
    pub trigger_price: f64,
    pub jump_pct: f64,
    pub vol_surge_x: f64,
    pub main_net_pct: Option<f64>,
    pub divergence: String,
    pub horizon_tag: String,
    pub close_ret: Option<f64>,
}

/// 当日全部信号（用于收盘汇总与 Web 榜单）。
pub fn signals_on(conn: &Connection, day: NaiveDate) -> Result<Vec<SignalRow>> {
    let start = day.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let end = start + 86400;
    let mut stmt = conn.prepare(
        "SELECT id, code, name, ts, trigger_price, jump_pct, vol_surge_x, main_net_pct,
                divergence, horizon_tag, close_ret
         FROM signals WHERE ts >= ?1 AND ts < ?2 ORDER BY ts ASC")?;
    let rows = stmt.query_map([start, end], |r| {
        Ok(SignalRow {
            id: r.get(0)?,
            code: r.get(1)?,
            name: r.get(2)?,
            ts: chrono::DateTime::from_timestamp(r.get::<_, i64>(3)?, 0)
                .map(|d| d.naive_utc()).unwrap_or_default(),
            trigger_price: r.get(4)?,
            jump_pct: r.get(5)?,
            vol_surge_x: r.get(6)?,
            main_net_pct: r.get(7)?,
            divergence: r.get(8)?,
            horizon_tag: r.get(9)?,
            close_ret: r.get(10)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome { Close, T1, T5 }

/// 回填结局。
///
/// `ret` 为 None 时写 NULL 而非 0 —— 「没数据」和「零收益」是两回事，
/// 混同会让日后的统计检验把缺失样本当成平局，系统性歪曲信号效果。
pub fn backfill_outcome(conn: &Connection, id: i64, field: Outcome, ret: Option<f64>) -> Result<()> {
    let sql = match field {
        Outcome::Close => "UPDATE signals SET close_ret = ?1 WHERE id = ?2",
        Outcome::T1 => "UPDATE signals SET ret_t1 = ?1 WHERE id = ?2",
        Outcome::T5 => "UPDATE signals SET ret_t5 = ?1 WHERE id = ?2",
    };
    conn.execute(sql, rusqlite::params![ret, id])?;
    Ok(())
}

/// 结局待回填的信号（close_ret 仍为空）。
pub fn signals_missing_close(conn: &Connection, day: NaiveDate) -> Result<Vec<SignalRow>> {
    Ok(signals_on(conn, day)?.into_iter().filter(|s| s.close_ret.is_none()).collect())
}

impl Divergence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Divergence::None => "none",
            Divergence::RetailChasing => "retail_chasing",
            Divergence::MainAccumulating => "main_accumulating",
            Divergence::Unknown => "unknown",
        }
    }
}

impl Horizon {
    pub fn as_str(&self) -> &'static str {
        match self {
            Horizon::Short => "short",
            Horizon::Long => "long",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::movers::Baseline;

    fn dt(y: i32, mo: u32, da: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, da).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    fn db() -> Connection { open_in_memory().unwrap() }

    fn tick(code: &str, ts: NaiveDateTime, price: f64, volume: f64) -> Tick {
        Tick { code: code.into(), ts, price, change_pct: 0.0, volume,
               amount: volume * price, turnover: 0.5, vol_ratio: 1.0 }
    }

    fn mover(code: &str, ts: NaiveDateTime) -> Mover {
        Mover {
            code: code.into(), name: "测试股".into(), ts, price: 10.0,
            jump_pct: 0.03, vol_surge_x: 4.0, main_net: None, main_net_pct: None,
            divergence: Divergence::Unknown, horizon: Horizon::Short, baseline: Baseline::History,
        }
    }

    #[test]
    fn same_tick_written_twice_does_not_duplicate() {
        // 重试同一时点是正常的（网络抖动），(code, ts) 主键必须挡住重复
        let mut c = db();
        let t = tick("600519", dt(2026, 7, 16, 10, 0), 1258.99, 47611.0);
        insert_ticks(&mut c, &[t.clone()]).unwrap();
        insert_ticks(&mut c, &[t]).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "同一 (code, ts) 只应有一行");
    }

    #[test]
    fn prune_keeps_boundary_day_and_deletes_beyond() {
        let mut c = db();
        let now = dt(2026, 7, 16, 15, 0);
        insert_ticks(&mut c, &[
            tick("A", now - chrono::Duration::days(10) + chrono::Duration::minutes(1), 1.0, 1.0),
            tick("B", now - chrono::Duration::days(11), 1.0, 1.0),
        ]).unwrap();
        prune(&c, now, 10).unwrap();
        let codes: Vec<String> = c.prepare("SELECT code FROM ticks").unwrap()
            .query_map([], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect();
        assert_eq!(codes, vec!["A"], "第 10 天内保留，第 11 天删除");
    }

    #[test]
    fn prune_never_touches_signals() {
        // 本模块最重要的不变式：signals 一旦被当缓存清掉，就永久失去验证阈值的
        // 能力 —— 那是项目头号风险的唯一解法。用一个远古信号来锁死它
        let mut c = db();
        let ancient = dt(2020, 1, 1, 10, 0);
        insert_ticks(&mut c, &[tick("A", ancient, 1.0, 1.0)]).unwrap();
        insert_signal(&c, &mover("A", ancient), true).unwrap();

        prune(&c, dt(2026, 7, 16, 15, 0), 10).unwrap();

        let ticks: i64 = c.query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0)).unwrap();
        let sigs: i64 = c.query_row("SELECT COUNT(*) FROM signals", [], |r| r.get(0)).unwrap();
        assert_eq!(ticks, 0, "6 年前的 tick 应被清理");
        assert_eq!(sigs, 1, "signals 永久保留，清理 ticks 不得波及");
    }

    #[test]
    fn same_slot_volumes_excludes_today_and_other_slots() {
        let mut c = db();
        let today = d(2026, 7, 16);
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 14, 10, 0), 1.0, 100.0),  // 同时点，历史 ✓
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 200.0),  // 同时点，历史 ✓
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 999.0), // 不同时点 ✗
            tick("A", dt(2026, 7, 16, 10, 0), 1.0, 888.0),  // 今天 ✗
        ]).unwrap();
        let v = same_slot_volumes(&c, "A", (10, 0), today, d(2026, 7, 1)).unwrap();
        assert_eq!(v, vec![100.0, 200.0], "只取历史同时点；今天与其他时点须排除");
    }

    #[test]
    fn same_slot_volumes_respects_since_window() {
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 1, 10, 0), 1.0, 111.0),   // baseline 窗口外
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 222.0),  // 窗口内
        ]).unwrap();
        let v = same_slot_volumes(&c, "A", (10, 0), d(2026, 7, 16), d(2026, 7, 10)).unwrap();
        assert_eq!(v, vec![222.0], "baseline_days 窗口外的样本不得参与基准");
    }

    #[test]
    fn non_trading_day_roundtrips_and_is_idempotent() {
        let c = db();
        assert!(!is_non_trading(&c, d(2026, 10, 1)).unwrap());
        mark_non_trading(&c, d(2026, 10, 1)).unwrap();
        assert!(is_non_trading(&c, d(2026, 10, 1)).unwrap());
        mark_non_trading(&c, d(2026, 10, 1)).unwrap();
    }

    #[test]
    fn pushed_today_only_counts_pushed_and_only_today() {
        let c = db();
        insert_signal(&c, &mover("PUSHED", dt(2026, 7, 16, 10, 0)), true).unwrap();
        insert_signal(&c, &mover("SILENT", dt(2026, 7, 16, 10, 10)), false).unwrap();
        insert_signal(&c, &mover("YDAY", dt(2026, 7, 15, 10, 0)), true).unwrap();

        let set = pushed_today(&c, d(2026, 7, 16)).unwrap();
        assert!(set.contains("PUSHED"));
        assert!(!set.contains("SILENT"), "只进库未推送的不算，否则它会被永久静音");
        assert!(!set.contains("YDAY"), "昨天推过的今天应能再推");
    }

    #[test]
    fn signal_insert_is_idempotent_per_code_and_ts() {
        let c = db();
        let m = mover("A", dt(2026, 7, 16, 10, 0));
        insert_signal(&c, &m, false).unwrap();
        insert_signal(&c, &m, false).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM signals", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "重跑同一时点不应产生重复信号");
    }

    #[test]
    fn backfill_writes_null_not_zero_when_no_data() {
        // 「没数据」≠「零收益」。混同会让统计检验把缺失样本当平局，歪曲信号效果
        let c = db();
        insert_signal(&c, &mover("A", dt(2026, 7, 16, 10, 0)), false).unwrap();
        let id: i64 = c.query_row("SELECT id FROM signals", [], |r| r.get(0)).unwrap();

        backfill_outcome(&c, id, Outcome::Close, None).unwrap();
        let v: Option<f64> = c.query_row("SELECT close_ret FROM signals WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(v, None, "无日线数据时须写 NULL 而非 0");

        backfill_outcome(&c, id, Outcome::Close, Some(0.05)).unwrap();
        let v: Option<f64> = c.query_row("SELECT close_ret FROM signals WHERE id=?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(v, Some(0.05));
    }

    #[test]
    fn signals_missing_close_finds_only_unfilled() {
        let c = db();
        insert_signal(&c, &mover("A", dt(2026, 7, 16, 10, 0)), false).unwrap();
        insert_signal(&c, &mover("B", dt(2026, 7, 16, 10, 10)), false).unwrap();
        let id: i64 = c.query_row("SELECT id FROM signals WHERE code='A'", [], |r| r.get(0)).unwrap();
        backfill_outcome(&c, id, Outcome::Close, Some(0.01)).unwrap();

        let missing = signals_missing_close(&c, d(2026, 7, 16)).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].code, "B");
    }

    #[test]
    fn recent_ticks_returns_newest_first() {
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 16, 10, 0), 1.0, 10.0),
            tick("A", dt(2026, 7, 16, 10, 10), 2.0, 20.0),
        ]).unwrap();
        let r = recent_ticks(&c, "A", 2).unwrap();
        assert_eq!(r.len(), 2);
        assert!((r[0].1 - 2.0).abs() < 1e-9, "最新的在前 —— 异动检测靠它取当前时点");
    }
}

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
//!
//! # ⚠ `ts` 列的口径：本地朴素时间，不是真 Unix 时间戳
//!
//! `ts` 存的是 `naive_local.and_utc().timestamp()` —— 即把**本地**朴素时间
//! 当作 UTC 编码。读回时 `from_timestamp(ts,0).naive_utc()` 反向解，
//! 与写入精确互逆。
//!
//! 全模块统一此口径（写入、读取、prune 的时间比较、pushed_today 的日界），
//! 故内部完全自洽。A 股只有一个时区，不需要真正的时区换算。
//!
//! **但这意味着 `ts` 不能当 Unix 时间戳解读**：
//! `sqlite3 "SELECT datetime(ts,'unixepoch') FROM ticks"` 会得到偏移 8 小时
//! 的错误时间（CST=UTC+8），正确的读法是 `datetime(ts,'unixepoch')` 的结果
//! 直接当本地时间看，不要再做时区转换。外部脚本写入本表时同理 ——
//! 用 `datetime.combine(day, t).timestamp()`（本地→epoch）会与本模块差 8 小时。
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

/// 某只股票在「历史同一时点」（同 时:分，**不含今天**）的成交量**增量**序列，
/// 用于量能基准。
///
/// # 返回增量而非累计量
///
/// 腾讯给的 volume 是**当日累计**。检测侧算的是「本时点增量」
/// （`compute` 里 `vol - prev_vol`）。基准必须与被比较的量同口径 ——
/// 拿增量去除以累计量毫无意义，且会随时间推移越来越离谱：
/// 尾盘累计量是早盘的十几倍，同一个增量在 14:50 算出的倍数会比 10:10 小一个量级。
///
/// 故这里对每个历史日，取该时点的累计量减去**同日前一个时点**的累计量。
/// 当天首个时点（无前值）没有增量可言，跳过。
///
/// # 排除今天
///
/// 硬性：基准必须是历史。把今天算进去等于用当下解释当下 ——
/// 一只正在异动的股票会自己抬高自己的基准，把自己的异动抹平。
/// 日内一个时点：((时, 分), 当日累计成交量)
type Slot = ((u32, u32), f64);

pub fn same_slot_deltas(
    conn: &Connection, code: &str, hhmm: (u32, u32), today: NaiveDate, since: NaiveDate,
) -> Result<Vec<f64>> {
    let mut stmt = conn.prepare(
        "SELECT ts, volume FROM ticks WHERE code = ?1 AND ts >= ?2 ORDER BY ts ASC")?;
    let since_ts = since.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let rows = stmt.query_map(rusqlite::params![code, since_ts], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
    })?;

    // 按日分组（已按 ts 升序），日内保留 (时分, 累计量)
    let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<Slot>> = Default::default();
    for (ts, vol) in rows.filter_map(|r| r.ok()) {
        let Some(dt) = chrono::DateTime::from_timestamp(ts, 0).map(|d| d.naive_utc()) else { continue };
        if dt.date() == today { continue }
        by_day.entry(dt.date()).or_default().push(((dt.hour(), dt.minute()), vol));
    }

    let mut out = Vec::new();
    for (_, day) in by_day {
        let Some(i) = day.iter().position(|(slot, _)| *slot == hhmm) else { continue };
        // 当日首个时点无前值，无增量可言
        if i == 0 { continue }
        let delta = day[i].1 - day[i - 1].1;
        // 累计量回退（数据源偶发）→ 负增量，不可当基准
        if delta > 0.0 { out.push(delta) }
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
    fn timestamp_roundtrips_exactly_as_local_naive() {
        // ts 的口径是「本地朴素时间当 UTC 编码」，写入与读取必须精确互逆。
        // 若哪天有人把写入改成 Local::now().timestamp()（真 epoch）而读取不变，
        // 所有时间会偏移 8 小时：10:30 的信号显示成 02:30，日界判断跟着错乱，
        // pushed_today 会在下午 4 点「跨日」把限流状态清掉。
        let mut c = db();
        let t = dt(2026, 7, 16, 10, 30);
        insert_ticks(&mut c, &[tick("A", t, 10.0, 100.0)]).unwrap();
        let back = recent_ticks(&c, "A", 1).unwrap();
        assert_eq!(back[0].0, t, "写入读取须精确互逆，不得有时区偏移");
    }

    #[test]
    fn pushed_today_day_boundary_uses_same_convention_as_writes() {
        // 日界必须与 ts 口径一致。若二者用不同口径，会差 8 小时 ——
        // 下午的信号被算进「明天」，当日限流形同虚设
        let c = db();
        insert_signal(&c, &mover("LATE", dt(2026, 7, 16, 14, 50)), true).unwrap();
        insert_signal(&c, &mover("EARLY", dt(2026, 7, 16, 10, 0)), true).unwrap();
        let set = pushed_today(&c, d(2026, 7, 16)).unwrap();
        assert!(set.contains("LATE"), "下午的信号必须算作今天");
        assert!(set.contains("EARLY"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn same_tick_written_twice_does_not_duplicate() {
        // 重试同一时点是正常的（网络抖动），(code, ts) 主键必须挡住重复
        let mut c = db();
        let t = tick("600519", dt(2026, 7, 16, 10, 0), 1258.99, 47611.0);
        insert_ticks(&mut c, std::slice::from_ref(&t)).unwrap();
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
    fn same_slot_returns_delta_not_cumulative() {
        // 回归测试。腾讯的 volume 是当日累计，而检测侧算的是「本时点增量」。
        // 基准必须同口径 —— 拿增量除以累计量毫无意义，且随时间越来越离谱：
        // 尾盘累计量是早盘的十几倍，同一增量在 14:50 算出的倍数会小一个量级。
        //
        // 这个 bug 单测抓不到（单测直接把 history 当增量喂），只有端到端组装才暴露。
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 1000.0),   // 累计 1000
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 2000.0),  // 累计 2000 → 增量 1000
        ]).unwrap();
        let v = same_slot_deltas(&c, "A", (10, 10), d(2026, 7, 16), d(2026, 7, 1)).unwrap();
        assert_eq!(v, vec![1000.0], "须返回增量 1000，而非累计量 2000");
    }

    #[test]
    fn same_slot_skips_first_tick_of_day_which_has_no_delta() {
        // 当日首个时点没有前值，无增量可言。若误把累计量当增量，
        // 10:00 的「增量」会等于开盘至今的全部成交量 —— 一个巨大的假基准
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 5000.0),
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 6000.0),
        ]).unwrap();
        let v = same_slot_deltas(&c, "A", (10, 0), d(2026, 7, 16), d(2026, 7, 1)).unwrap();
        assert!(v.is_empty(), "当日首个时点无前值，须跳过而非把累计量当增量");
    }

    #[test]
    fn same_slot_computes_delta_per_day_independently() {
        // 累计量每天从 0 重来。若不按日分组、直接对全序列做差，
        // 跨日那一笔会得到巨大的负数（今日开盘累计 − 昨日收盘累计）
        let mut c = db();
        for day in [14u32, 15] {
            insert_ticks(&mut c, &[
                tick("A", dt(2026, 7, day, 10, 0), 1.0, 1000.0),
                tick("A", dt(2026, 7, day, 10, 10), 1.0, 1500.0),
            ]).unwrap();
        }
        let v = same_slot_deltas(&c, "A", (10, 10), d(2026, 7, 16), d(2026, 7, 1)).unwrap();
        assert_eq!(v, vec![500.0, 500.0], "每日独立做差，不得跨日");
    }

    #[test]
    fn same_slot_excludes_today() {
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 100.0),
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 300.0),   // 历史增量 200 ✓
            tick("A", dt(2026, 7, 16, 10, 0), 1.0, 100.0),
            tick("A", dt(2026, 7, 16, 10, 10), 1.0, 9999.0),  // 今天 ✗
        ]).unwrap();
        let v = same_slot_deltas(&c, "A", (10, 10), d(2026, 7, 16), d(2026, 7, 1)).unwrap();
        assert_eq!(v, vec![200.0], "今天的量不得进基准 —— 否则异动股自己抹平自己");
    }

    #[test]
    fn same_slot_respects_since_window() {
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 1, 10, 0), 1.0, 100.0),     // baseline 窗口外
            tick("A", dt(2026, 7, 1, 10, 10), 1.0, 211.0),
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 100.0),    // 窗口内
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 322.0),
        ]).unwrap();
        let v = same_slot_deltas(&c, "A", (10, 10), d(2026, 7, 16), d(2026, 7, 10)).unwrap();
        assert_eq!(v, vec![222.0], "baseline_days 窗口外的样本不得参与基准");
    }

    #[test]
    fn same_slot_drops_negative_delta_from_source_glitch() {
        // 腾讯偶发累计量回退。负增量不可当基准
        let mut c = db();
        insert_ticks(&mut c, &[
            tick("A", dt(2026, 7, 15, 10, 0), 1.0, 2000.0),
            tick("A", dt(2026, 7, 15, 10, 10), 1.0, 1500.0),  // 回退
        ]).unwrap();
        let v = same_slot_deltas(&c, "A", (10, 10), d(2026, 7, 16), d(2026, 7, 1)).unwrap();
        assert!(v.is_empty(), "负增量须丢弃");
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

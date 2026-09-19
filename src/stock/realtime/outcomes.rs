//! Same-source signal labels, with an append-only audit of every correction.
//!
//! ticks 默认永久保留,所以这里绝不能全表扫描:只处理仍有标签待填的信号,
//! 且只按 `ts` 区间(本地朴素时间当 UTC 编码,见 store 模块说明)查
//! 收盘窗口 15:00:00–15:05:59 的快照,走 ticks 的主键 (code, ts) 与 idx_ticks_ts。
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Default, Serialize)]
pub struct RepairReport {
    pub close: usize,
    pub t1: usize,
    pub t5: usize,
    pub skipped: usize,
}

const TICKS_METHOD: &str = "ticks-close-v1; raw trigger to 15:00-15:05 snapshot; observed market sessions; not adjusted total return";

/// 某股某日收盘窗口内最后一笔快照。走主键 (code, ts)。
const CLOSE_TICK_SQL: &str =
    "SELECT ts,price FROM ticks WHERE code=?1 AND ts BETWEEN ?2 AND ?3 ORDER BY ts DESC LIMIT 1";
/// 某日收盘窗口内是否有任何有效快照(= 已观测到的交易日)。走 idx_ticks_ts。
const SESSION_PROBE_SQL: &str =
    "SELECT 1 FROM ticks WHERE ts BETWEEN ?1 AND ?2 AND price>0 LIMIT 1";

fn epoch(day: NaiveDate, h: u32, m: u32, s: u32) -> i64 {
    day.and_hms_opt(h, m, s)
        .expect("valid time")
        .and_utc()
        .timestamp()
}

/// 收盘窗口 [15:00:00, 15:05:59] 的 ts 区间(闭区间)。
fn close_window(day: NaiveDate) -> (i64, i64) {
    (epoch(day, 15, 0, 0), epoch(day, 15, 5, 59))
}

fn ensure_audit_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS signal_outcome_audit (
      id INTEGER PRIMARY KEY, signal_id INTEGER NOT NULL, horizon INTEGER NOT NULL,
      old_return REAL, new_return REAL NOT NULL, trigger_price REAL NOT NULL,
      target_price REAL NOT NULL, target_ts INTEGER NOT NULL, repaired_at TEXT NOT NULL,
      method TEXT NOT NULL);",
    )?;
    Ok(())
}

/// 该股该日收盘窗口内最后一笔快照;价格无效时视同没有(绝不当 0 用)。
fn close_tick(conn: &Connection, code: &str, day: NaiveDate) -> Result<Option<(i64, f64)>> {
    let (lo, hi) = close_window(day);
    let row = conn
        .query_row(CLOSE_TICK_SQL, params![code, lo, hi], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })
        .optional()?;
    Ok(row.filter(|(_, p)| p.is_finite() && *p > 0.0))
}

/// [from, through] 内已观测到收盘快照的交易日(升序)。逐日按 ts 区间探测。
fn sessions(conn: &Connection, from: NaiveDate, through: NaiveDate) -> Result<Vec<NaiveDate>> {
    let mut stmt = conn.prepare(SESSION_PROBE_SQL)?;
    let mut out = Vec::new();
    let mut day = from;
    while day <= through {
        let (lo, hi) = close_window(day);
        if stmt.exists(params![lo, hi])? {
            out.push(day);
        }
        day = day.succ_opt().expect("date in range");
    }
    Ok(out)
}

struct SignalRow {
    id: i64,
    code: String,
    day: NaiveDate,
    ts: i64,
    trigger: f64,
    old: [Option<f64>; 3],
}

/// `through` 当日及之前的信号;`all = false` 时只取仍有标签为空的。走 idx_signals_ts。
fn load_signals(conn: &Connection, through: NaiveDate, all: bool) -> Result<Vec<SignalRow>> {
    let end = epoch(through.succ_opt().expect("date in range"), 0, 0, 0);
    let filter = if all {
        ""
    } else {
        " AND (close_ret IS NULL OR ret_t1 IS NULL OR ret_t5 IS NULL)"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id,code,ts,trigger_price,close_ret,ret_t1,ret_t5 FROM signals WHERE ts<?1{filter} ORDER BY ts"
    ))?;
    let rows = stmt.query_map([end], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, f64>(3)?,
            [
                r.get::<_, Option<f64>>(4)?,
                r.get::<_, Option<f64>>(5)?,
                r.get::<_, Option<f64>>(6)?,
            ],
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, code, ts, trigger, old) = row?;
        let Some(day) = chrono::DateTime::from_timestamp(ts, 0).map(|t| t.naive_utc().date())
        else {
            continue;
        };
        out.push(SignalRow {
            id,
            code,
            day,
            ts,
            trigger,
            old,
        });
    }
    Ok(out)
}

const HORIZONS: [(usize, usize, &str); 3] =
    [(0, 0, "close_ret"), (1, 1, "ret_t1"), (2, 5, "ret_t5")];

/// 日常修复:只处理仍有标签为空的信号。
///
/// Use only snapshots near 15:00. Never replace an unknown outcome with zero.
/// T+1/T+5 refer to subsequent observed market sessions, not calendar days.
/// 被处理的信号上已有的值若与快照不一致,照旧纠正并写审计。
pub fn repair(conn: &Connection, through: NaiveDate) -> Result<RepairReport> {
    run_repair(conn, through, false)
}

/// 全量审计:连三个标签都已填的信号也重新核对(CLI `repair-outcomes` 用)。
/// 开销随历史增长,不应放进每日例行任务。
pub fn repair_all(conn: &Connection, through: NaiveDate) -> Result<RepairReport> {
    run_repair(conn, through, true)
}

fn run_repair(conn: &Connection, through: NaiveDate, all: bool) -> Result<RepairReport> {
    ensure_audit_table(conn)?;
    let tx = conn.unchecked_transaction()?;
    let mut report = RepairReport::default();
    let signals = load_signals(&tx, through, all)?;
    let Some(first) = signals.iter().map(|s| s.day).min() else {
        return Ok(report);
    };
    let days = sessions(&tx, first, through)?;
    let mut ticks: HashMap<(String, NaiveDate), Option<(i64, f64)>> = HashMap::new();
    let mut lookup = |code: &str, day: NaiveDate| -> Result<Option<(i64, f64)>> {
        let key = (code.to_string(), day);
        if let Some(v) = ticks.get(&key) {
            return Ok(*v);
        }
        let v = close_tick(&tx, code, day)?;
        ticks.insert(key, v);
        Ok(v)
    };
    let mut writes = Vec::new();
    for s in &signals {
        if !s.trigger.is_finite() || s.trigger <= 0.0 {
            report.skipped += 1;
            continue;
        }
        let Ok(origin) = days.binary_search(&s.day) else {
            report.skipped += 1;
            continue;
        };
        let Some((close_ts, _)) = lookup(&s.code, s.day)? else {
            report.skipped += 1;
            continue;
        };
        if close_ts < s.ts {
            report.skipped += 1;
            continue;
        }
        for (slot, horizon, column) in HORIZONS {
            let Some(target_day) = days.get(origin + horizon) else {
                continue;
            };
            let Some((ts, price)) = lookup(&s.code, *target_day)? else {
                continue;
            };
            let ret = price / s.trigger - 1.0;
            if s.old[slot].is_some_and(|v| (v - ret).abs() < 1e-10) {
                continue;
            }
            writes.push((
                s.id,
                horizon,
                column,
                s.old[slot],
                ret,
                s.trigger,
                price,
                ts,
            ));
        }
    }
    for (id, horizon, column, old, ret, trigger, price, ts) in writes {
        tx.execute("INSERT INTO signal_outcome_audit(signal_id,horizon,old_return,new_return,trigger_price,target_price,target_ts,repaired_at,method) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,horizon as i64,old,ret,trigger,price,ts,chrono::Utc::now().to_rfc3339(),TICKS_METHOD])?;
        tx.execute(
            &format!("UPDATE signals SET {column}=?1 WHERE id=?2"),
            params![ret, id],
        )?;
        match horizon {
            0 => report.close += 1,
            1 => report.t1 += 1,
            _ => report.t5 += 1,
        };
    }
    tx.commit()?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::realtime::store;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    /// 与 store 口径一致:本地朴素时间当 UTC 编码。
    fn ts(day: NaiveDate, h: u32, m: u32, s: u32) -> i64 {
        day.and_hms_opt(h, m, s).unwrap().and_utc().timestamp()
    }
    fn tick(c: &Connection, code: &str, t: i64, price: f64) {
        c.execute(
            "INSERT INTO ticks VALUES (?1,?2,?3,0,1,1,1,1)",
            params![code, t, price],
        )
        .unwrap();
    }
    fn signal(c: &Connection, code: &str, t: i64, trigger: f64, old: [Option<f64>; 3]) -> i64 {
        c.execute(
            "INSERT INTO signals(code,name,ts,trigger_price,jump_pct,vol_surge_x,divergence,horizon_tag,baseline,close_ret,ret_t1,ret_t5)
             VALUES (?1,'t',?2,?3,0.1,3,'none','short','history',?4,?5,?6)",
            params![code, t, trigger, old[0], old[1], old[2]],
        )
        .unwrap();
        c.last_insert_rowid()
    }
    fn labels(c: &Connection, id: i64) -> [Option<f64>; 3] {
        c.query_row(
            "SELECT close_ret,ret_t1,ret_t5 FROM signals WHERE id=?1",
            [id],
            |r| Ok([r.get(0)?, r.get(1)?, r.get(2)?]),
        )
        .unwrap()
    }
    fn audits(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM signal_outcome_audit", [], |r| {
            r.get(0)
        })
        .unwrap()
    }
    /// 连续 6 个交易日(周四起,跳过周末),每日 15:00 一笔收盘快照。
    fn six_sessions() -> Vec<NaiveDate> {
        vec![d(7, 16), d(7, 17), d(7, 20), d(7, 21), d(7, 22), d(7, 23)]
    }

    #[test]
    fn fully_labelled_signals_are_not_reprocessed() {
        let c = store::open_in_memory().unwrap();
        let days = six_sessions();
        for (i, day) in days.iter().enumerate() {
            tick(&c, "600519", ts(*day, 15, 0, 0), 11.0 + i as f64);
        }
        // 三个标签都已有值(且与快照不一致):日常修复不应再碰它
        let id = signal(
            &c,
            "600519",
            ts(days[0], 10, 0, 0),
            10.0,
            [Some(0.5), Some(0.5), Some(0.5)],
        );
        let r = repair(&c, days[5]).unwrap();
        assert_eq!((r.close, r.t1, r.t5, r.skipped), (0, 0, 0, 0));
        assert_eq!(labels(&c, id), [Some(0.5), Some(0.5), Some(0.5)]);
        assert_eq!(audits(&c), 0);
        // 全量审计入口(CLI)仍按原语义纠正已有值
        let r = repair_all(&c, days[5]).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 1, 1));
        assert_eq!(audits(&c), 3);
    }

    #[test]
    fn ticks_outside_the_needed_window_are_ignored() {
        let c = store::open_in_memory().unwrap();
        let days = six_sessions();
        // 只有前 5 个交易日在 through 之内
        let through = days[4];
        for (i, day) in days[..5].iter().enumerate() {
            tick(&c, "600519", ts(*day, 15, 0, 0), 11.0 + i as f64);
        }
        // through 之后的收盘快照不算已观测交易日,否则 T+5 会被提前填上
        tick(&c, "600519", ts(days[5], 15, 0, 0), 99.0);
        // 收盘窗口 15:00:00–15:05:59 之外的快照不算收盘价
        tick(&c, "600519", ts(days[0], 14, 59, 59), 50.0);
        tick(&c, "600519", ts(days[0], 15, 6, 0), 50.0);
        // 最早待处理信号之前的快照与本信号无关
        tick(&c, "600519", ts(d(7, 1), 15, 0, 0), 77.0);
        let id = signal(&c, "600519", ts(days[0], 10, 0, 0), 10.0, [None; 3]);
        let r = repair(&c, through).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 1, 0));
        let [close, t1, t5] = labels(&c, id);
        assert!((close.unwrap() - 0.1).abs() < 1e-10);
        assert!((t1.unwrap() - 0.2).abs() < 1e-10);
        assert!(t5.is_none(), "T+5 尚未观测到,不得写入");
    }

    #[test]
    fn tick_lookups_use_indexes() {
        let c = store::open_in_memory().unwrap();
        let code = "600519".to_string();
        let cases: [(&str, Vec<&dyn rusqlite::ToSql>); 2] = [
            (CLOSE_TICK_SQL, vec![&code, &0i64, &0i64]),
            (SESSION_PROBE_SQL, vec![&0i64, &0i64]),
        ];
        for (sql, args) in cases {
            let plan: Vec<String> = c
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .unwrap()
                .query_map(args.as_slice(), |r| r.get::<_, String>(3))
                .unwrap()
                .map(|r| r.unwrap())
                .collect();
            assert!(
                plan.iter().all(|p| !p.starts_with("SCAN")),
                "{sql} 不应全表扫描: {plan:?}"
            );
        }
    }
}

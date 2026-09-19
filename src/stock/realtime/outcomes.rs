//! Same-source signal labels, with an append-only audit of every correction.
use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, Serialize)]
pub struct RepairReport {
    pub close: usize,
    pub t1: usize,
    pub t5: usize,
    pub skipped: usize,
}

/// Use only snapshots near 15:00. Never replace an unknown outcome with zero.
/// T+1/T+5 refer to subsequent observed market sessions, not calendar days.
pub fn repair(conn: &Connection, through: NaiveDate) -> Result<RepairReport> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS signal_outcome_audit (
      id INTEGER PRIMARY KEY, signal_id INTEGER NOT NULL, horizon INTEGER NOT NULL,
      old_return REAL, new_return REAL NOT NULL, trigger_price REAL NOT NULL,
      target_price REAL NOT NULL, target_ts INTEGER NOT NULL, repaired_at TEXT NOT NULL,
      method TEXT NOT NULL);",
    )?;
    let tx = conn.unchecked_transaction()?;
    let mut by_day = BTreeMap::new();
    let mut days = BTreeSet::new();
    {
        let mut stmt = tx.prepare(
            "SELECT t.code,date(t.ts,'unixepoch'),t.ts,t.price FROM ticks t JOIN (
          SELECT code,date(ts,'unixepoch') day,MAX(ts) ts FROM ticks
          WHERE date(ts,'unixepoch')<=?1 AND time(ts,'unixepoch') BETWEEN '15:00:00' AND '15:05:59'
          GROUP BY code,date(ts,'unixepoch')) d ON t.code=d.code AND t.ts=d.ts",
        )?;
        let rows = stmt.query_map([through.to_string()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        for row in rows {
            let (code, day, ts, price) = row?;
            if price.is_finite() && price > 0.0 {
                days.insert(day.clone());
                by_day.insert((code, day), (ts, price));
            }
        }
    }
    let days: Vec<_> = days.into_iter().collect();
    let signals = {
        let mut stmt=tx.prepare("SELECT id,code,date(ts,'unixepoch'),ts,trigger_price,close_ret,ret_t1,ret_t5 FROM signals WHERE date(ts,'unixepoch')<=?1")?;
        let rows = stmt.query_map([through.to_string()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, f64>(4)?,
                [
                    r.get::<_, Option<f64>>(5)?,
                    r.get::<_, Option<f64>>(6)?,
                    r.get::<_, Option<f64>>(7)?,
                ],
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut report = RepairReport::default();
    for (id, code, day, trigger_ts, trigger, old) in signals {
        if !trigger.is_finite() || trigger <= 0.0 {
            report.skipped += 1;
            continue;
        }
        let Ok(origin) = days.binary_search(&day) else {
            report.skipped += 1;
            continue;
        };
        let Some((close_ts, _)) = by_day.get(&(code.clone(), day)) else {
            report.skipped += 1;
            continue;
        };
        if *close_ts < trigger_ts {
            report.skipped += 1;
            continue;
        }
        for (slot, horizon, column) in [(0, 0, "close_ret"), (1, 1, "ret_t1"), (2, 5, "ret_t5")] {
            let Some(target_day) = days.get(origin + horizon) else {
                continue;
            };
            let Some((ts, price)) = by_day.get(&(code.clone(), target_day.clone())) else {
                continue;
            };
            let ret = price / trigger - 1.0;
            if old[slot].is_some_and(|v| (v - ret).abs() < 1e-10) {
                continue;
            }
            tx.execute("INSERT INTO signal_outcome_audit(signal_id,horizon,old_return,new_return,trigger_price,target_price,target_ts,repaired_at,method) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,horizon as i64,old[slot],ret,trigger,price,ts,chrono::Utc::now().to_rfc3339(),"ticks-close-v1; raw trigger to 15:00-15:05 snapshot; observed market sessions; not adjusted total return"])?;
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
    }
    tx.commit()?;
    Ok(report)
}

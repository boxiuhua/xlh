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

/// 某股某日是否有任何快照(信号当日数据是否还在)。走主键 (code, ts)。
const HAS_TICKS_SQL: &str = "SELECT 1 FROM ticks WHERE code=?1 AND ts BETWEEN ?2 AND ?3 LIMIT 1";

/// 日常路径(收盘汇总)只看信号日在 `through` 前这么多自然日内的信号;
/// 更早的留给 CLI `repair-outcomes`(`repair_all`)。
pub const DAILY_LOOKBACK_DAYS: i64 = 30;
/// 日 K 回退只针对 `through` 前这么多自然日内的目标日,避免停牌股每天联网重试。
pub const FALLBACK_WINDOW_DAYS: i64 = 10;
/// 从信号日数到 T+N 途中,相邻已观测交易日间隔超过这么多自然日即视为
/// 历史有缺口(采集中断/已清理),不计算 T+N,免得把几个月后的价格当 T+1。
/// 须容下 A 股最长的正常休市:春节前后最后/首个交易日可相隔 11 个自然日
/// (如 2024-02-08 → 2024-02-19),国庆约 8 天,故取 12。
pub const MAX_SESSION_GAP_DAYS: i64 = 12;

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

/// `through` 当日及之前(且给了 `since` 时不早于该日)的信号;`all = false` 时
/// 只取仍有标签为空的。走 idx_signals_ts。
fn load_signals(
    conn: &Connection,
    through: NaiveDate,
    all: bool,
    since: Option<NaiveDate>,
) -> Result<Vec<SignalRow>> {
    let end = epoch(through.succ_opt().expect("date in range"), 0, 0, 0);
    let start = since.map_or(i64::MIN, |d| epoch(d, 0, 0, 0));
    let filter = if all {
        ""
    } else {
        " AND (close_ret IS NULL OR ret_t1 IS NULL OR ret_t5 IS NULL)"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id,code,ts,trigger_price,close_ret,ret_t1,ret_t5 FROM signals WHERE ts>=?1 AND ts<?2{filter} ORDER BY ts"
    ))?;
    let rows = stmt.query_map([start, end], |r| {
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

/// `day` 之后、相邻间隔都不超过 [`MAX_SESSION_GAP_DAYS`] 的已观测交易日
/// (最多 5 个,即到 T+5)。遇到更大的缺口即截断:缺口后的交易日不能当 T+N。
fn sessions_after(days: &[NaiveDate], day: NaiveDate) -> Vec<NaiveDate> {
    let mut chain = Vec::new();
    let mut prev = day;
    for d in days.iter().copied().filter(|d| *d > day) {
        if chain.len() == 5 || (d - prev).num_days() > MAX_SESSION_GAP_DAYS {
            break;
        }
        chain.push(d);
        prev = d;
    }
    chain
}

const HORIZONS: [(usize, usize, &str); 3] =
    [(0, 0, "close_ret"), (1, 1, "ret_t1"), (2, 5, "ret_t5")];

/// 日常修复:只处理仍有标签为空、且信号日在 `through` 前
/// [`DAILY_LOOKBACK_DAYS`] 个自然日内的信号。
///
/// Use only snapshots near 15:00. Never replace an unknown outcome with zero.
/// T+1/T+5 refer to subsequent observed market sessions, not calendar days.
/// 被处理的信号上已有的值若与快照不一致,照旧纠正并写审计。
pub fn repair(conn: &Connection, through: NaiveDate) -> Result<RepairReport> {
    run_repair(conn, through, false, Some(lookback_start(through)))
}

fn lookback_start(through: NaiveDate) -> NaiveDate {
    through - chrono::Duration::days(DAILY_LOOKBACK_DAYS)
}

/// 全量审计:连三个标签都已填的信号也重新核对(CLI `repair-outcomes` 用)。
/// 开销随历史增长,不应放进每日例行任务。
pub fn repair_all(conn: &Connection, through: NaiveDate) -> Result<RepairReport> {
    run_repair(conn, through, true, None)
}

fn run_repair(
    conn: &Connection,
    through: NaiveDate,
    all: bool,
    since: Option<NaiveDate>,
) -> Result<RepairReport> {
    ensure_audit_table(conn)?;
    let tx = conn.unchecked_transaction()?;
    let mut report = RepairReport::default();
    let signals = load_signals(&tx, through, all, since)?;
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
        if days.binary_search(&s.day).is_err() {
            report.skipped += 1;
            continue;
        }
        let Some((close_ts, _)) = lookup(&s.code, s.day)? else {
            report.skipped += 1;
            continue;
        };
        if close_ts < s.ts {
            report.skipped += 1;
            continue;
        }
        let chain = sessions_after(&days, s.day);
        for (slot, horizon, column) in HORIZONS {
            let target_day = if horizon == 0 {
                s.day
            } else {
                match chain.get(horizon - 1) {
                    Some(d) => *d,
                    None => continue,
                }
            };
            let Some((ts, price)) = lookup(&s.code, target_day)? else {
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

/// 回退值的审计口径:未复权日 K 收盘价。
pub const DAILY_CLOSE_METHOD: &str = "daily_close";

/// 补填 `repair` 之后仍为空的标签,在它之后调用(日常收盘路径)。
///
/// 只看信号日在 `through` 前 [`DAILY_LOOKBACK_DAYS`] 个自然日内的信号。
/// 目标交易日:收盘为信号当日;T+N 为信号当日之后第 N 个已观测交易日。
/// T+N 仅在本股信号当日仍有快照(历史未被清理)、且信号日到目标日之间
/// 相邻交易日间隔都不超过 [`MAX_SESSION_GAP_DAYS`] 时才计算——否则历史
/// 缺口会把 T+N 挪到几个月后。
/// 取价:目标日有收盘快照就用快照(同源优先);没有快照、该日严格早于
/// `through` 且在其前 [`FALLBACK_WINDOW_DAYS`] 个自然日内时,回退到
/// `daily_close(code, start, end)` 给出的未复权日收盘价,审计 method 记
/// `daily_close`。日线也没有该日时留空,绝不写 0。
///
/// 先只读地定出计划,再在事务外调用 `daily_close`(按股票最多一次,只取
/// 需要回退的日期区间;返回 None 视为无数据),最后开事务写入。只填空值,
/// 从不改已有值。
pub fn fill_missing(
    conn: &Connection,
    through: NaiveDate,
    mut daily_close: impl FnMut(&str, NaiveDate, NaiveDate) -> Option<Vec<(NaiveDate, f64)>>,
) -> Result<RepairReport> {
    ensure_audit_table(conn)?;
    let mut report = RepairReport::default();
    let signals = load_signals(conn, through, false, Some(lookback_start(through)))?;
    let Some(first) = signals.iter().map(|s| s.day).min() else {
        return Ok(report);
    };
    let days = sessions(conn, first, through)?;
    let fallback_from = through - chrono::Duration::days(FALLBACK_WINDOW_DAYS);

    // 1) 只读定计划
    struct Plan<'a> {
        id: i64,
        code: &'a str,
        horizon: usize,
        column: &'static str,
        trigger: f64,
        day: NaiveDate,
        tick: Option<(i64, f64)>,
    }
    let mut plans = Vec::new();
    let mut need: HashMap<&str, (NaiveDate, NaiveDate)> = HashMap::new();
    {
        let mut has_ticks = conn.prepare(HAS_TICKS_SQL)?;
        for s in &signals {
            if !s.trigger.is_finite() || s.trigger <= 0.0 {
                report.skipped += 1;
                continue;
            }
            // 当日有收盘快照却早于触发时刻:与 repair 一致,整条跳过
            if close_tick(conn, &s.code, s.day)?.is_some_and(|(ts, _)| ts < s.ts) {
                report.skipped += 1;
                continue;
            }
            let (lo, hi) = (epoch(s.day, 0, 0, 0), epoch(s.day, 23, 59, 59));
            let day_intact = has_ticks.exists(params![s.code, lo, hi])?;
            // 从信号日往后、相邻间隔不超限的已观测交易日链(最多到 T+5)
            let chain = sessions_after(&days, s.day);
            for (slot, horizon, column) in HORIZONS {
                if s.old[slot].is_some() {
                    continue;
                }
                let target = if horizon == 0 {
                    s.day
                } else if !day_intact {
                    continue;
                } else {
                    match chain.get(horizon - 1) {
                        Some(d) => *d,
                        None => continue,
                    }
                };
                let tick = close_tick(conn, &s.code, target)?;
                if tick.is_none() {
                    if target >= through || target < fallback_from {
                        continue;
                    }
                    let e = need.entry(s.code.as_str()).or_insert((target, target));
                    e.0 = e.0.min(target);
                    e.1 = e.1.max(target);
                }
                plans.push(Plan {
                    id: s.id,
                    code: &s.code,
                    horizon,
                    column,
                    trigger: s.trigger,
                    day: target,
                    tick,
                });
            }
        }
    }

    // 2) 事务外取日线(可能联网)
    let mut bars: HashMap<&str, HashMap<NaiveDate, f64>> = HashMap::new();
    for (code, (start, end)) in need {
        let closes = daily_close(code, start, end).unwrap_or_default();
        bars.insert(code, closes.into_iter().collect());
    }

    // 3) 写入
    let tx = conn.unchecked_transaction()?;
    for p in plans {
        let (price, ts, method) = match p.tick {
            Some((ts, price)) => (price, ts, TICKS_METHOD),
            None => match bars.get(p.code).and_then(|m| m.get(&p.day)) {
                Some(c) if c.is_finite() && *c > 0.0 => {
                    (*c, close_window(p.day).0, DAILY_CLOSE_METHOD)
                }
                _ => continue,
            },
        };
        let ret = price / p.trigger - 1.0;
        let column = p.column;
        let changed = tx.execute(
            &format!("UPDATE signals SET {column}=?1 WHERE id=?2 AND {column} IS NULL"),
            params![ret, p.id],
        )?;
        if changed == 0 {
            continue;
        }
        tx.execute("INSERT INTO signal_outcome_audit(signal_id,horizon,old_return,new_return,trigger_price,target_price,target_ts,repaired_at,method) VALUES (?1,?2,NULL,?3,?4,?5,?6,?7,?8)",params![p.id,p.horizon as i64,ret,p.trigger,price,ts,chrono::Utc::now().to_rfc3339(),method])?;
        match p.horizon {
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
        let cases: [(&str, Vec<&dyn rusqlite::ToSql>); 3] = [
            (CLOSE_TICK_SQL, vec![&code, &0i64, &0i64]),
            (SESSION_PROBE_SQL, vec![&0i64, &0i64]),
            (HAS_TICKS_SQL, vec![&code, &0i64, &0i64]),
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

    fn audit_methods(c: &Connection) -> Vec<(i64, String)> {
        c.prepare("SELECT horizon,method FROM signal_outcome_audit ORDER BY horizon")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn missing_close_snapshot_falls_back_to_daily_close_for_finished_days() {
        let c = store::open_in_memory().unwrap();
        let days = six_sessions();
        let through = days[5];
        // 市场每日都有收盘快照(交易日已观测),但本股只在第 2–4 日有
        for day in &days {
            tick(&c, "000001", ts(*day, 15, 0, 0), 5.0);
        }
        for (i, day) in days.iter().enumerate().skip(2).take(3) {
            tick(&c, "600519", ts(*day, 15, 0, 0), 11.0 + i as f64);
        }
        // 触发信号的那笔盘中快照还在(信号当日历史完整)
        tick(&c, "600519", ts(days[0], 10, 0, 0), 10.0);
        let id = signal(&c, "600519", ts(days[0], 10, 0, 0), 10.0, [None; 3]);
        assert_eq!(repair(&c, through).unwrap().skipped, 1);
        let mut calls = 0;
        let r = fill_missing(&c, through, |code, start, end| {
            calls += 1;
            assert_eq!(code, "600519");
            assert_eq!(
                (start, end),
                (days[0], days[1]),
                "只取需要回退的已结束交易日"
            );
            // 第 1 日无日线(停牌),第 5 日 = through 当天的盘中价不得使用
            Some(vec![(days[0], 20.0), (days[2], 99.0), (days[5], 30.0)])
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!((r.close, r.t1, r.t5), (1, 0, 0));
        let [close, t1, t5] = labels(&c, id);
        assert!((close.unwrap() - 1.0).abs() < 1e-10);
        assert!(t1.is_none(), "日线也没有时留空,不写 0");
        assert!(t5.is_none(), "through 当天未结束,不回退日线");
        assert_eq!(audit_methods(&c), vec![(0, DAILY_CLOSE_METHOD.to_string())]);
        // 再跑一次:已填的不动,留空的仍留空
        let r = fill_missing(&c, through, |_, _, _| Some(vec![])).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (0, 0, 0));
        assert_eq!(audit_methods(&c).len(), 1);
    }

    #[test]
    fn fill_uses_later_snapshots_when_signal_day_has_no_session() {
        let c = store::open_in_memory().unwrap();
        let days = six_sessions();
        // 信号当天守护进程没赶上收盘,全市场无收盘快照;之后每天都有
        for (i, day) in days.iter().enumerate().skip(1) {
            tick(&c, "600519", ts(*day, 15, 0, 0), 11.0 + i as f64);
        }
        tick(&c, "600519", ts(days[0], 10, 0, 0), 10.0);
        let id = signal(&c, "600519", ts(days[0], 10, 0, 0), 10.0, [None; 3]);
        repair(&c, days[5]).unwrap();
        assert_eq!(labels(&c, id), [None; 3]);
        let r = fill_missing(&c, days[5], |_, _, _| Some(vec![(days[0], 20.0)])).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 1, 1));
        let [close, t1, t5] = labels(&c, id);
        assert!((close.unwrap() - 1.0).abs() < 1e-10);
        assert!((t1.unwrap() - 0.2).abs() < 1e-10);
        assert!((t5.unwrap() - 0.6).abs() < 1e-10);
        let methods = audit_methods(&c);
        assert_eq!(methods[0].1, DAILY_CLOSE_METHOD);
        assert_eq!(methods[1].1, TICKS_METHOD);
        assert_eq!(methods[2].1, TICKS_METHOD);
    }

    #[test]
    fn fill_waits_for_todays_close_without_touching_the_loader() {
        let c = store::open_in_memory().unwrap();
        let today = d(7, 16);
        tick(&c, "000001", ts(today, 14, 50, 0), 5.0);
        let id = signal(&c, "600519", ts(today, 10, 0, 0), 10.0, [None; 3]);
        let r = fill_missing(&c, today, |_, _, _| -> Option<Vec<(NaiveDate, f64)>> {
            panic!("当天未结束,不应取日线")
        })
        .unwrap();
        assert_eq!((r.close, r.t1, r.t5), (0, 0, 0));
        assert_eq!(labels(&c, id), [None; 3]);
    }

    /// 每个工作日 15:00 一笔快照,[from, to] 闭区间。
    fn weekday_closes(c: &Connection, code: &str, from: NaiveDate, to: NaiveDate, price: f64) {
        let mut day = from;
        while day <= to {
            if !crate::stock::realtime::calendar::is_weekend(day) {
                tick(c, code, ts(day, 15, 0, 0), price);
            }
            day = day.succ_opt().unwrap();
        }
    }

    #[test]
    fn pruned_history_signal_gets_no_t_plus_n() {
        let c = store::open_in_memory().unwrap();
        // 信号当日的快照已被清理(旧库 retain_days=10),之后的快照都在
        let sig_day = d(9, 7);
        let through = d(9, 16);
        weekday_closes(&c, "600519", d(9, 8), through, 12.0);
        let id = signal(&c, "600519", ts(sig_day, 10, 0, 0), 10.0, [None; 3]);
        repair(&c, through).unwrap();
        let r = fill_missing(&c, through, |_, _, _| Some(vec![(sig_day, 11.0)])).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 0, 0));
        let [close, t1, t5] = labels(&c, id);
        assert!((close.unwrap() - 0.1).abs() < 1e-10, "当日收盘仍可回退日线");
        assert!(t1.is_none() && t5.is_none(), "信号当日无快照,不得数 T+N");
    }

    #[test]
    fn session_gap_after_signal_blocks_t_plus_n() {
        let c = store::open_in_memory().unwrap();
        let sig_day = d(8, 3);
        // 信号当日完整,但之后采集中断 3 周
        tick(&c, "600519", ts(sig_day, 10, 0, 0), 10.0);
        tick(&c, "600519", ts(sig_day, 15, 0, 0), 11.0);
        weekday_closes(&c, "600519", d(8, 24), d(8, 31), 20.0);
        let id = signal(&c, "600519", ts(sig_day, 10, 0, 0), 10.0, [None; 3]);
        let through = d(8, 31);
        repair(&c, through).unwrap();
        fill_missing(&c, through, |_, _, _| -> Option<Vec<(NaiveDate, f64)>> {
            panic!("无需回退")
        })
        .unwrap();
        let [close, t1, t5] = labels(&c, id);
        assert!((close.unwrap() - 0.1).abs() < 1e-10);
        assert!(t1.is_none() && t5.is_none(), "缺口后的交易日不能当 T+N");
        // 全量审计同样不跨缺口
        repair_all(&c, through).unwrap();
        assert!(labels(&c, id)[1].is_none());
    }

    #[test]
    fn spring_festival_break_is_not_treated_as_a_gap() {
        let c = store::open_in_memory().unwrap();
        let ymd = |m, d| NaiveDate::from_ymd_opt(2024, m, d).unwrap();
        // 2024 春节:2 月 8 日收盘后休市,2 月 19 日复市(相隔 11 个自然日)
        let sig_day = ymd(2, 8);
        tick(&c, "600519", ts(sig_day, 15, 0, 0), 11.0);
        weekday_closes(&c, "600519", ymd(2, 19), ymd(2, 26), 12.0);
        let id = signal(&c, "600519", ts(sig_day, 10, 0, 0), 10.0, [None; 3]);
        let r = repair(&c, ymd(2, 26)).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 1, 1));
        assert!((labels(&c, id)[1].unwrap() - 0.2).abs() < 1e-10);
    }

    #[test]
    fn daily_path_ignores_signals_older_than_lookback() {
        let c = store::open_in_memory().unwrap();
        let through = d(9, 16);
        // 早于回看窗口的一个工作日(8 月 12 日,周三)
        let old_day = through - chrono::Duration::days(DAILY_LOOKBACK_DAYS + 5);
        weekday_closes(&c, "600519", old_day, through, 11.0);
        let id = signal(&c, "600519", ts(old_day, 10, 0, 0), 10.0, [None; 3]);
        let r = repair(&c, through).unwrap();
        assert_eq!((r.close, r.t1, r.t5, r.skipped), (0, 0, 0, 0));
        let r = fill_missing(&c, through, |_, _, _| -> Option<Vec<(NaiveDate, f64)>> {
            panic!("超出回看窗口的信号不应触发日线回退")
        })
        .unwrap();
        assert_eq!((r.close, r.t1, r.t5, r.skipped), (0, 0, 0, 0));
        assert_eq!(labels(&c, id), [None; 3]);
        // 更早的交给 CLI 全量审计
        let r = repair_all(&c, through).unwrap();
        assert_eq!((r.close, r.t1, r.t5), (1, 1, 1));
    }

    #[test]
    fn daily_close_fallback_only_for_recent_targets() {
        let c = store::open_in_memory().unwrap();
        let through = d(9, 18);
        // recent = 9 月 8 日(周二),stale = 9 月 7 日(周一)
        let recent = through - chrono::Duration::days(FALLBACK_WINDOW_DAYS);
        let stale = recent - chrono::Duration::days(1);
        // 两只股票信号当日盘中有快照、收盘快照缺失;之后每天收盘快照齐全
        tick(&c, "600519", ts(stale, 10, 0, 0), 10.0);
        tick(&c, "600036", ts(recent, 10, 0, 0), 10.0);
        weekday_closes(&c, "600519", recent, through, 11.0);
        weekday_closes(&c, "600036", recent.succ_opt().unwrap(), through, 11.0);
        let old = signal(&c, "600519", ts(stale, 10, 0, 0), 10.0, [None; 3]);
        let new = signal(&c, "600036", ts(recent, 10, 0, 0), 10.0, [None; 3]);
        let mut asked = Vec::new();
        fill_missing(&c, through, |code, start, end| {
            asked.push((code.to_string(), start, end));
            Some(vec![(recent, 12.0)])
        })
        .unwrap();
        assert_eq!(asked, vec![("600036".to_string(), recent, recent)]);
        assert!(labels(&c, old)[0].is_none(), "超出回退窗口的目标日不联网");
        assert!((labels(&c, new)[0].unwrap() - 0.2).abs() < 1e-10);
    }
}

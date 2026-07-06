//! cron 解析 + 守护循环（普通阻塞线程，不引入 tokio）。
use std::str::FromStr;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, TimeZone};
use cron::Schedule;

use rusqlite::Connection;

/// 给定 cron 与起点，算下一次触发时刻。
pub fn next_after<Tz: TimeZone>(cron: &str, after: &DateTime<Tz>) -> Result<DateTime<Tz>>
where
    Tz::Offset: Copy,
{
    let sched = Schedule::from_str(cron).map_err(|e| anyhow!("cron 非法: {e}"))?;
    sched.after(after).next().ok_or_else(|| anyhow!("cron 无后续触发时间"))
}

use crate::web::auth::model::LicenseStatus;

/// 纯函数：给定各用户 (uid, cron) 与时间窗口 (last_tick, now]，返回本轮应触发的 uid。
pub fn due_users(configs: &[(i64, String)], last_tick: DateTime<Local>, now: DateTime<Local>) -> Vec<i64> {
    configs
        .iter()
        .filter_map(|(uid, cron)| match next_after(cron, &last_tick) {
            Ok(t) if t <= now => Some(*uid),
            _ => None,
        })
        .collect()
}

/// 该用户是否允许被投递：启用、未注销、且授权放行。
fn user_allowed(conn: &Connection, uid: i64, today: chrono::NaiveDate, warn: i64, grace: i64) -> bool {
    match crate::web::auth::store::find_user_by_id(conn, uid) {
        Ok(Some(u)) => {
            !u.disabled
                && !u.cancelled
                && LicenseStatus::of(u.expires_at, today, warn, grace).allows_access()
        }
        _ => false,
    }
}

/// 多租户守护：每 60s 一轮，对窗口内到点且授权放行的用户投递。单用户失败仅记日志。
pub fn run_multi(conn: &Connection, warn: i64, grace: i64) -> Result<()> {
    println!("多用户推送守护已启动（Ctrl+C 退出）");
    let mut last_tick = Local::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let now = Local::now();
        let all = super::store::list_all(conn).unwrap_or_default();
        let crons: Vec<(i64, String)> = all.iter().map(|(u, c)| (*u, c.schedule.cron.clone())).collect();
        for uid in due_users(&crons, last_tick, now) {
            if !user_allowed(conn, uid, now.date_naive(), warn, grace) { continue; }
            if let Some((_, cfg)) = all.iter().find(|(u, _)| *u == uid) {
                if let Err(e) = super::job::run(cfg, Some(conn), Some(uid)) {
                    eprintln!("用户 {uid} 推送失败：{e}");
                }
            }
        }
        last_tick = now;
    }
}

/// 对所有授权放行用户强制跑一次（忽略 only_on_new_data），用于 `xlh push --once`。
pub fn run_all_once(conn: &Connection, warn: i64, grace: i64) -> Result<()> {
    let today = Local::now().date_naive();
    for (uid, cfg) in super::store::list_all(conn).unwrap_or_default() {
        if !user_allowed(conn, uid, today, warn, grace) { continue; }
        if let Err(e) = super::job::run_forced(&cfg, Some(conn), Some(uid)) {
            eprintln!("用户 {uid} 推送失败：{e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn next_after_daily_noon() {
        // 每天 12:00:00；从 08:00 起下一次应是当天 12:00
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();
        let next = next_after("0 0 12 * * *", &from).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap());
    }

    #[test]
    fn next_after_rolls_to_next_day() {
        // 从 13:00 起，12:00 的任务应落到次日
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 13, 0, 0).unwrap();
        let next = next_after("0 0 12 * * *", &from).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).unwrap());
    }

    #[test]
    fn bad_cron_errors() {
        let from = Utc.with_ymd_and_hms(2026, 1, 1, 8, 0, 0).unwrap();
        assert!(next_after("nonsense", &from).is_err());
    }

    use chrono::Local;

    #[test]
    fn due_users_window_hit_and_miss() {
        // 每天 08:30:00 触发
        let cfgs = vec![(1i64, "0 30 8 * * *".to_string()), (2i64, "0 0 9 * * *".to_string())];
        let last = Local.with_ymd_and_hms(2026, 1, 1, 8, 29, 0).unwrap();
        let now  = Local.with_ymd_and_hms(2026, 1, 1, 8, 31, 0).unwrap();
        assert_eq!(due_users(&cfgs, last, now), vec![1], "仅 08:30 落在窗口内");
        // 窗口内无触发
        let last2 = Local.with_ymd_and_hms(2026, 1, 1, 8, 31, 0).unwrap();
        let now2  = Local.with_ymd_and_hms(2026, 1, 1, 8, 32, 0).unwrap();
        assert!(due_users(&cfgs, last2, now2).is_empty());
    }
}

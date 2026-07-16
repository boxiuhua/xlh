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
///
/// 每轮都重新 `list_all(conn)` —— 所以在 Web 上改完 cron **无需重启**，下一轮即生效。
/// （已实测：守护运行期间改 cron，它按新表达式触发。）
///
/// 每轮也写一次心跳。这不是锦上添花：推送守护是**独立进程**（可选的 xlh-push 容器），
/// 没启动时 Web 上一切看着正常（配置能存、提示「已保存」），却永远不会推送。
/// 心跳让 Web 能明确告诉用户「守护没在跑」，把这个静默失败变成可见的。
pub fn run_multi(conn: &Connection, warn: i64, grace: i64) -> Result<()> {
    println!("多用户推送守护已启动（Ctrl+C 退出）");
    // 启动即先跳一次，别让 Web 在头 60 秒里误报「未运行」
    if let Err(e) = super::store::beat(conn) { eprintln!("写心跳失败：{e}"); }

    // 实时抓取挂在同一个 60s 循环上（本项目不引入 tokio）。
    // 它用独立的库连接：盘中每 10 分钟写 5400 行，不该和账号/会话争同一个 WAL 锁。
    let mut rt = realtime_init();

    let mut last_tick = Local::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        let now = Local::now();
        if let Err(e) = super::store::beat(conn) { eprintln!("写心跳失败：{e}"); }

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

        // 实时抓取失败绝不能拖垮既有的推送守护 —— 那是已上线、用户依赖的功能，
        // 而实时抓取是新增的、可选的。任何错误只记日志。
        if let Some((d, rc)) = rt.as_mut() {
            if let Err(e) = realtime_tick(d, rc, conn, now, warn, grace) {
                eprintln!("实时抓取本轮失败：{e}");
            }
        }

        last_tick = now;
    }
}

/// 打开实时库。失败返回 None —— 实时抓取是可选功能，不该让守护起不来。
///
/// 用进程级配置（由 `run_multi_daemon` 的调用方经 `config::init` 装载），
/// **不硬编码 config.toml 路径** —— 否则 `--config` 对实时抓取形同虚设。
fn realtime_init() -> Option<(crate::stock::realtime::job::Daemon, Connection)> {
    let cfg = crate::stock::realtime::config::get().clone();
    match crate::stock::realtime::store::open(&cfg.db_path) {
        Ok(c) => {
            println!("实时抓取已启用（库 {}，ticks 保留 {} 天）", cfg.db_path.display(), cfg.retain_days);
            Some((crate::stock::realtime::job::Daemon::new(cfg), c))
        }
        Err(e) => {
            eprintln!("[realtime] 打开库失败，实时抓取未启用：{e}");
            None
        }
    }
}

/// 一轮实时抓取 + 推送 + 收盘汇总。
fn realtime_tick(
    d: &mut crate::stock::realtime::job::Daemon,
    rc: &mut Connection,
    push_conn: &Connection,
    now: DateTime<Local>,
    warn: i64,
    grace: i64,
) -> Result<()> {
    use crate::stock::realtime::job;

    let naive = now.naive_local();

    if job::is_summary_time(naive) {
        let md = job::close_summary(rc, naive.date())?;
        broadcast(push_conn, "盘中异动汇总", &md, now.date_naive(), warn, grace);
        return Ok(());
    }

    let Some(out) = d.tick(rc, naive)? else { return Ok(()) };
    println!("[{}] 快照 {} 条，异动 {} 只，推送 {} 只{}",
        naive.format("%H:%M"), out.ticks, out.movers.len(), out.pushed.len(),
        if out.flow_ok { "" } else { "（资金流不可用）" });

    if out.pushed.is_empty() { return Ok(()) }
    let md = job::render_movers(&out.pushed, out.flow_ok);
    broadcast(push_conn, "盘中异动", &md, now.date_naive(), warn, grace);
    Ok(())
}

/// 向所有授权放行、且配了推送渠道的用户广播。单用户失败仅记日志。
fn broadcast(conn: &Connection, title: &str, md: &str, today: chrono::NaiveDate, warn: i64, grace: i64) {
    for (uid, cfg) in super::store::list_all(conn).unwrap_or_default() {
        if !user_allowed(conn, uid, today, warn, grace) { continue; }
        if cfg.channel.webhook.trim().is_empty() { continue; }
        if let Err(e) = super::channels::send(&cfg.channel, title, md) {
            eprintln!("用户 {uid} 实时推送失败：{e}");
        }
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

//! cron 解析 + 守护循环（普通阻塞线程，不引入 tokio）。
use std::str::FromStr;
use anyhow::{anyhow, Result};
use chrono::{DateTime, Local, TimeZone};
use cron::Schedule;

use super::config::PushConfig;
use rusqlite::Connection;

/// 给定 cron 与起点，算下一次触发时刻。
pub fn next_after<Tz: TimeZone>(cron: &str, after: &DateTime<Tz>) -> Result<DateTime<Tz>>
where
    Tz::Offset: Copy,
{
    let sched = Schedule::from_str(cron).map_err(|e| anyhow!("cron 非法: {e}"))?;
    sched.after(after).next().ok_or_else(|| anyhow!("cron 无后续触发时间"))
}

/// 守护：解析 cron，循环 sleep 到下一次触发并跑任务。单次失败仅记日志、继续。
pub fn run_daemon(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    // 提前校验一次表达式
    Schedule::from_str(&cfg.schedule.cron).map_err(|e| anyhow!("cron 非法: {e}"))?;
    println!("推送守护已启动，cron = {}（Ctrl+C 退出）", cfg.schedule.cron);
    loop {
        let now = Local::now();
        let next = next_after(&cfg.schedule.cron, &now)?;
        println!("下次推送：{}", next.format("%Y-%m-%d %H:%M:%S"));
        let wait = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);
        std::thread::sleep(wait);
        match super::job::run(cfg, hist, None) {
            Ok(()) => println!("[{}] 推送完成", Local::now().format("%H:%M:%S")),
            Err(e) => eprintln!("[{}] 推送失败：{e}", Local::now().format("%H:%M:%S")),
        }
    }
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
}

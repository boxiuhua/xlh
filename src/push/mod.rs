//! 定时推送模块：按 cron 定时同步基金净值，生成持仓建议 + 诊断，推送到
//! 钉钉/飞书/企业微信 群机器人或 Server酱（个人微信）。
pub mod config;
pub mod store;
pub mod message;
pub mod channels;
pub mod stock_advice;
pub mod schedule;
pub mod job;

pub use config::{load, PushConfig};
pub use job::build_message;
use rusqlite::Connection;

pub fn run_once(cfg: &PushConfig, hist: Option<&Connection>, user_id: Option<i64>) -> anyhow::Result<()> {
    job::run_forced(cfg, hist, user_id)
}

/// 多用户 cron 守护。
pub fn run_multi_daemon(conn: &Connection, warn_days: i64, grace_days: i64) -> anyhow::Result<()> {
    schedule::run_multi(conn, warn_days, grace_days)
}

/// 对所有授权用户强制跑一次。
pub fn run_all_once(conn: &Connection, warn_days: i64, grace_days: i64) -> anyhow::Result<()> {
    schedule::run_all_once(conn, warn_days, grace_days)
}

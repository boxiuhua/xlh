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

/// 立即跑一次任务（同步+建议+诊断+推送）。手动触发，强制发送，忽略 only_on_new_data。
pub fn run_once(cfg: &PushConfig, hist: Option<&Connection>) -> anyhow::Result<()> {
    job::run_forced(cfg, hist)
}

/// 按 cron 常驻守护。
pub fn run_daemon(cfg: &PushConfig, hist: Option<&Connection>) -> anyhow::Result<()> {
    schedule::run_daemon(cfg, hist)
}

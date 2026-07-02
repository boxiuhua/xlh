//! 定时推送模块：按 cron 定时同步基金净值，生成持仓建议 + 诊断，推送到
//! 钉钉/飞书/企业微信 群机器人或 Server酱（个人微信）。
pub mod config;
pub mod message;
pub mod channels;
pub mod schedule;
pub mod job;

pub use config::{load, PushConfig};

/// 立即跑一次任务（同步+建议+诊断+推送）。
pub fn run_once(cfg: &PushConfig) -> anyhow::Result<()> {
    job::run(cfg)
}

/// 按 cron 常驻守护。
pub fn run_daemon(cfg: &PushConfig) -> anyhow::Result<()> {
    schedule::run_daemon(cfg)
}

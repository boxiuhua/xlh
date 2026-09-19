//! 量化交易核心：信号 → 闸门 → 工单 → 成交 → 持仓。
//! 设计见 docs/superpowers/specs/2026-09-15-quant-trading-design.md

pub mod admission;
pub mod calendar;
pub mod config;
pub mod daemon;
pub mod daily_signals;
pub mod exits;
pub mod gate;
pub mod link;
pub mod model;
pub mod monitor;
pub mod movers;
pub mod notify;
pub mod plans;
pub mod quotes;
pub mod router;
pub mod service;
pub mod settings;
pub mod store;
pub mod strategy_signal;
pub mod ticket;

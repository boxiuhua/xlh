//! 策略准入:前推回测、阈值判定、状态机。
//! 设计见 docs/superpowers/specs/2026-09-15-quant-trading-design.md §10

pub mod judge;
pub mod scorecard;
pub mod state;
pub mod stats;
pub mod walk_forward;

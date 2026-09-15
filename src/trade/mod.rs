//! 量化交易核心：信号 → 闸门 → 工单 → 成交 → 持仓。
//! 设计见 docs/superpowers/specs/2026-09-15-quant-trading-design.md

pub mod model;
pub mod store;

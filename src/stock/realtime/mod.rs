//! 盘中实时行情抓取与短线异动检测。
//!
//! 与 `stock::data`（日线）并列、互不干扰：那边全是 `klt=101` 日线，`StockBar`
//! 只有 date 没有时间戳；这里是分钟级快照，独立库、独立表、独立保留期。
//!
//! # 双源分工（不是设计偏好，是实测封禁逼出来的）
//!
//! - **全市场价量 → 腾讯 `qt.gtimg.cn`**：单请求 800 只，7 请求/快照，实测无封禁
//! - **候选股资金流 → 东财 `ulist.np`**：只查几十只候选，1 请求/快照
//!
//! 2026-07-16 实测：东财批量行情端点（clist / ulist.np）不到 15 次请求即触发
//! 封禁，`http=000`，且会从 clist 蔓延到 ulist.np，180 秒观测窗口内未解封。
//! 全市场 5400 只按 clist `pz=200` 需 27 页 × 20 时点 = 540 页/日，必封。
//! 此结论与 `data/universe.rs:6-20` 记录的历史一致 —— 作者当初正因此把 A 股
//! 清单从 clist 迁到 datacenter。
//!
//! **clist 路径在本模块禁止使用。** 若后续维护者想「简化」回单源东财，
//! 请先重读 `docs/superpowers/specs/2026-07-16-stock-realtime-design.md` 4.2。
//!
//! # 未经验证声明
//!
//! 本模块产出的是**线索，不是已验证策略**。所有阈值均为拍脑袋起点，
//! `signals` 表永久留档信号与其结局，正是为了数月后能用真实数据回答
//! 「这套阈值到底有没有用」。在那之前，不要把它当交易依据。
pub mod config;
pub mod snapshot;
pub mod calendar;
pub mod store;
pub mod flow;
pub mod movers;
pub mod job;

pub use config::RealtimeCfg;
pub use snapshot::Tick;

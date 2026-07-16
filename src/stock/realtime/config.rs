//! 实时抓取配置。
//!
//! 阈值一律可配置、不写死：`stock_advice.rs:3-11` 记录了基金侧同款择时逻辑
//! 上线后被证伪删除的教训，其魔数被作者自述为「拍脑袋」。这里的每个阈值
//! 同样**未经前瞻检验**，配置化是为了日后能用 signals 表的真实结局去调，
//! 而不是改代码重编译。
use std::path::PathBuf;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RealtimeCfg {
    /// 实时库路径。与账号库 data/xlh.db 物理隔离：盘中每 10 分钟写 5400 行，
    /// 不应与登录会话争同一 WAL 锁；且此库是纯派生数据，损坏可直接删除重建。
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    /// ticks 保留天数（自然日）。仅作用于 ticks —— signals 永久保留，刻意无对应开关。
    #[serde(default = "default_retain_days")]
    pub retain_days: i64,
    /// 量能基准回看窗口（自然日）。须 ≤ retain_days。
    ///
    /// 与 retain_days 解耦是有意的：前者是磁盘决策，后者是统计决策。窗口拉长
    /// 中位数更稳，但也更迟钝 —— 刚进入活跃期的股票，其「正常量」已抬升，
    /// 过长的窗口仍拿冷清期的旧量当基准，会把它每个时点都误判成异动。
    #[serde(default = "default_baseline_days")]
    pub baseline_days: i64,
    /// 价格突变阈值（10 分钟）。待验证。
    #[serde(default = "default_price_jump_pct")]
    pub price_jump_pct: f64,
    /// 量能放大阈值（相对同时点历史中位数）。待验证。
    #[serde(default = "default_volume_surge_x")]
    pub volume_surge_x: f64,
    /// 主力净流入占成交额比，「大量」阈值。待验证。
    #[serde(default = "default_main_flow_pct")]
    pub main_flow_pct: f64,
    /// 强信号 = 阈值的几倍。仅强信号进推送，弱信号只进库。
    #[serde(default = "default_strong_signal_x")]
    pub strong_signal_x: f64,
    /// 每时点最多推送几只。
    #[serde(default = "default_max_push_per_tick")]
    pub max_push_per_tick: usize,
}

fn default_db_path() -> PathBuf { PathBuf::from("data/realtime.db") }
fn default_retain_days() -> i64 { 10 }
fn default_baseline_days() -> i64 { 10 }
fn default_price_jump_pct() -> f64 { 0.02 }
fn default_volume_surge_x() -> f64 { 3.0 }
fn default_main_flow_pct() -> f64 { 0.05 }
fn default_strong_signal_x() -> f64 { 1.5 }
fn default_max_push_per_tick() -> usize { 5 }

impl Default for RealtimeCfg {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            retain_days: default_retain_days(),
            baseline_days: default_baseline_days(),
            price_jump_pct: default_price_jump_pct(),
            volume_surge_x: default_volume_surge_x(),
            main_flow_pct: default_main_flow_pct(),
            strong_signal_x: default_strong_signal_x(),
            max_push_per_tick: default_max_push_per_tick(),
        }
    }
}

/// 校验配置。`baseline_days > retain_days` 必须报错退出而非静默降级 ——
/// 否则基准会偷偷只用实际存在的数据，行为与配置不符，且无人察觉。
pub fn validate(c: &RealtimeCfg) -> Result<()> {
    if c.retain_days < 1 {
        return Err(anyhow!("[realtime] retain_days 须 ≥ 1，当前 {}", c.retain_days));
    }
    if c.baseline_days < 1 {
        return Err(anyhow!("[realtime] baseline_days 须 ≥ 1，当前 {}", c.baseline_days));
    }
    if c.baseline_days > c.retain_days {
        return Err(anyhow!(
            "[realtime] baseline_days({}) 不得大于 retain_days({}) —— 基准回看不到已被清理的数据",
            c.baseline_days, c.retain_days));
    }
    if c.price_jump_pct <= 0.0 {
        return Err(anyhow!("[realtime] price_jump_pct 须 > 0，当前 {}", c.price_jump_pct));
    }
    if c.volume_surge_x <= 1.0 {
        return Err(anyhow!(
            "[realtime] volume_surge_x 须 > 1，当前 {} —— ≤1 意味着「量能没放大也算放大」",
            c.volume_surge_x));
    }
    if c.main_flow_pct <= 0.0 {
        return Err(anyhow!("[realtime] main_flow_pct 须 > 0，当前 {}", c.main_flow_pct));
    }
    if c.strong_signal_x < 1.0 {
        return Err(anyhow!(
            "[realtime] strong_signal_x 须 ≥ 1，当前 {} —— <1 会让强信号比普通信号更宽松",
            c.strong_signal_x));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_validation() {
        validate(&RealtimeCfg::default()).unwrap();
    }

    #[test]
    fn baseline_longer_than_retain_is_rejected_not_silently_clamped() {
        // 基准回看 30 天但只存 10 天 —— 静默降级会让实际行为与配置不符，必须报错
        let c = RealtimeCfg { baseline_days: 30, retain_days: 10, ..Default::default() };
        let e = validate(&c).unwrap_err().to_string();
        assert!(e.contains("baseline_days"), "错误信息应点名 baseline_days: {e}");
    }

    #[test]
    fn baseline_equal_to_retain_is_allowed() {
        // 默认即此情形：用满保留窗口
        let c = RealtimeCfg { baseline_days: 10, retain_days: 10, ..Default::default() };
        validate(&c).unwrap();
    }

    #[test]
    fn volume_surge_at_or_below_one_is_rejected() {
        // ≤1 倍等于「没放大也算异动」，会把全市场每个时点都判成异动
        let c = RealtimeCfg { volume_surge_x: 1.0, ..Default::default() };
        assert!(validate(&c).is_err());
    }

    #[test]
    fn strong_signal_below_one_is_rejected() {
        // <1 会让「强信号」门槛低于普通信号，限流逻辑反而放大刷屏
        let c = RealtimeCfg { strong_signal_x: 0.5, ..Default::default() };
        assert!(validate(&c).is_err());
    }

    #[test]
    fn missing_section_yields_defaults() {
        // config.toml 里没有 [realtime] 段时，全字段走默认，不报错
        let c: RealtimeCfg = toml::from_str("").unwrap();
        assert_eq!(c, RealtimeCfg::default());
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        // 只覆盖一个字段，其余保持默认 —— 用户不必抄全量配置
        let c: RealtimeCfg = toml::from_str("price_jump_pct = 0.03").unwrap();
        assert!((c.price_jump_pct - 0.03).abs() < 1e-9);
        assert_eq!(c.retain_days, 10, "未指定的字段应保持默认");
    }
}

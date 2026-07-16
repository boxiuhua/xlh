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

/// 进程级配置。
///
/// # 为什么是全局而不是穿参
///
/// Web 层的 `core_routes<S>` 是泛型 state（测试用 `()` 直连 `.oneshot`），
/// handler 无法取到 `AuthState`。而配置本来就是**进程级**的：一个进程只服务
/// 一份 config.toml。
///
/// 关键是**不得硬编码路径**。`web/stock.rs:19` 那种 `Path::new(".cache/stock")`
/// 会让 `--config` 参数形同虚设 —— 实测踩过：用 `--config /tmp/x/config.toml`
/// 起服务，Web 层却仍读当前目录的 config.toml，打开了错误的库，榜单恒为空。
static CFG: std::sync::OnceLock<RealtimeCfg> = std::sync::OnceLock::new();

/// 启动时装载。由 `web::serve` 与 push 守护调用，各自传入真实的 `--config` 路径。
///
/// 段缺失 → 默认值；段非法 → Err（调用方决定是报错退出还是禁用实时抓取）。
pub fn init(path: &std::path::Path) -> Result<&'static RealtimeCfg> {
    let cfg = load_from_toml(path)?;
    Ok(CFG.get_or_init(|| cfg))
}

/// 取进程级配置。未 init 过则为默认值（测试路径）。
pub fn get() -> &'static RealtimeCfg {
    CFG.get_or_init(RealtimeCfg::default)
}

/// 从 config.toml 读 `[realtime]` 段。
///
/// 段缺失 → 默认值（不报错：多数用户不跑实时抓取，不该逼他们写这段）。
/// 段存在但非法 → **报错**（写了就得写对，静默用默认会让人以为自己的配置生效了）。
pub fn load_from_toml(path: &std::path::Path) -> Result<RealtimeCfg> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取配置 {} 失败: {e}", path.display()))?;
    from_toml_str(&text)
}

/// 供测试与 load_from_toml 复用的纯解析。
pub fn from_toml_str(text: &str) -> Result<RealtimeCfg> {
    #[derive(Deserialize)]
    struct Root { realtime: Option<RealtimeCfg> }
    let root: Root = toml::from_str(text).map_err(|e| anyhow!("[realtime] 段解析失败: {e}"))?;
    let cfg = root.realtime.unwrap_or_default();
    validate(&cfg)?;
    Ok(cfg)
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
    fn toml_without_realtime_section_yields_defaults() {
        // 多数用户不跑实时抓取，不该逼他们在 config.toml 里写这一段
        let c = from_toml_str("[data]\nfund_code = \"161725\"").unwrap();
        assert_eq!(c, RealtimeCfg::default());
    }

    #[test]
    fn toml_with_realtime_section_is_parsed_and_validated() {
        let c = from_toml_str("[realtime]\nretain_days = 20\nbaseline_days = 5").unwrap();
        assert_eq!(c.retain_days, 20);
        assert_eq!(c.baseline_days, 5);
    }

    #[test]
    fn invalid_section_errors_instead_of_silently_defaulting() {
        // 写了就得写对：静默回退默认会让人以为自己的配置生效了
        let e = from_toml_str("[realtime]\nbaseline_days = 30\nretain_days = 10");
        assert!(e.is_err(), "非法配置须报错而非用默认值");
    }

    #[test]
    fn partial_section_keeps_other_defaults() {
        // 只覆盖一个字段，其余保持默认 —— 用户不必抄全量配置
        let c: RealtimeCfg = toml::from_str("price_jump_pct = 0.03").unwrap();
        assert!((c.price_jump_pct - 0.03).abs() < 1e-9);
        assert_eq!(c.retain_days, 10, "未指定的字段应保持默认");
    }
}

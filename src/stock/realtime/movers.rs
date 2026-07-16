//! 盘中异动检测、资金流佐证、短/长线标签。
//!
//! # ⚠ 未经前瞻检验
//!
//! 本模块的每个阈值都是**拍脑袋的起点，不是结论**。`stock_advice.rs:3-11`
//! 记录了基金侧同款择时逻辑上线后被证伪、删除的教训。这里的输出是**线索**，
//! 不是策略。`signals` 表永久留档信号与其结局，正是为了数月后能用真实数据
//! 判断这套阈值到底有没有用 —— 在那之前，别拿它当交易依据。
//!
//! # 触发只由价量决定
//!
//! 资金流不是触发条件，理由有二：
//!
//! 1. 东财资金流需先按单笔金额分档统计后推送，比价量慢半拍。设为必要条件会
//!    漏掉「已拉升但资金流数据尚未跟上」的信号。
//! 2. **架构上不可能** —— 资金流只对候选股取，而候选正是价量筛出来的。
//!    触发发生在拿到资金流之前。
use chrono::{NaiveDateTime, Timelike};

/// 量能基准的来源。冷启动时历史样本不足，须显式标记而非静默降级 ——
/// 否则你无从判断一个信号是「与 10 天历史比出来的」还是「与今天自己比出来的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Baseline {
    /// 过去 baseline_days 内同时点成交量的中位数（正常路径）
    History,
    /// 历史样本为 0，降级为当日累计均值（首次部署 / 次新股）
    Fallback,
}

impl Baseline {
    pub fn as_str(&self) -> &'static str {
        match self { Baseline::History => "history", Baseline::Fallback => "fallback" }
    }
}

/// 资金流与价格方向的背离。单看价量看不出来，这是资金流最有信息量的用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Divergence {
    /// 价与资金流同向，或资金流未达「大量」阈值
    None,
    /// 涨 + 主力大量净流出 → 疑似散户抬轿
    RetailChasing,
    /// 跌 + 主力大量净流入 → 疑似主力吸筹
    MainAccumulating,
    /// 资金流数据缺失（东财封禁/请求失败）。**不是 None** ——
    /// 「没查到」和「查到了没背离」是两回事，混同会让日后的统计把缺失当无背离
    Unknown,
}

/// 该异动更像短线还是长线。启发式，未经检验。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Horizon {
    /// 纯消息面/情绪驱动
    Short,
    /// 估值处历史低分位且趋势非下跌 → 异动有基本面支撑
    Long,
}

/// 一条异动信号。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Mover {
    pub code: String,
    pub name: String,
    pub ts: NaiveDateTime,
    pub price: f64,
    /// 10 分钟价格突变（0.03 = +3%）
    pub jump_pct: f64,
    /// 量能放大倍数（相对基准）
    pub vol_surge_x: f64,
    /// 主力净流入额（元）。None = 资金流不可用
    pub main_net: Option<f64>,
    /// 主力净流入占成交额比（0.05 = 5%）。None = 资金流不可用
    pub main_net_pct: Option<f64>,
    pub divergence: Divergence,
    pub horizon: Horizon,
    pub baseline: Baseline,
}

/// 中位数。用中位数而非均值：单日异常值（比如上次异动本身的放量）不应污染基准。
pub fn median(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() { return None }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 })
}

/// 量能基准：历史同时点成交量的中位数；样本为空时降级为当日均值。
///
/// # 为什么不用「当日累计均值」当正常基准
///
/// A 股成交量有显著的 U 型日内分布：开盘半小时与尾盘放量，午间萎缩。
/// 以当日累计均值为基准，10:00 的基准被开盘高量拉高（异动难触发）、
/// 14:00 的基准被午间低量拉低（异动易误触发）—— **同一套阈值在一天之内的
/// 实际严格程度会漂移**。以历史同时点为基准，U 型分布被自然消去。
///
/// # 样本不足 ≠ 无样本
///
/// 保留期是自然日口径，10 天窗口内的实际交易日数会波动（平常 6–8 个，
/// 长假期间可能仅 3–4 个）。故 baseline_days 是**上界而非定数**：样本少了
/// 照常算，只有**一个都没有**时才 fallback。否则每年春节后会静默失效。
pub fn baseline_volume(history: &[f64], today_avg: Option<f64>) -> Option<(f64, Baseline)> {
    match median(history) {
        Some(m) if m > 0.0 => Some((m, Baseline::History)),
        _ => today_avg.filter(|v| *v > 0.0).map(|v| (v, Baseline::Fallback)),
    }
}

/// 价量双阈值判定。两者**同时**超阈值才算异动。
///
/// 只看价不看量 → 捞进开盘半小时的正常波动；只看量不看价 → 捞到大宗横盘。
pub fn is_mover(jump_pct: f64, vol_surge_x: f64, jump_th: f64, surge_th: f64) -> bool {
    jump_pct.abs() >= jump_th && vol_surge_x >= surge_th
}

/// 强信号？仅强信号进推送，弱信号只进库 —— 一天 20 时点 × 数十只会把飞书刷到静音。
pub fn is_strong(jump_pct: f64, vol_surge_x: f64, jump_th: f64, surge_th: f64, strong_x: f64) -> bool {
    jump_pct.abs() >= jump_th * strong_x && vol_surge_x >= surge_th * strong_x
}

/// 背离判定。
///
/// `main_net_pct` 为 None（东财封禁/失败）时返回 `Unknown` 而非 `None`：
/// 「没查到」和「查到了没背离」必须可区分。
pub fn divergence(jump_pct: f64, main_net_pct: Option<f64>, flow_th: f64) -> Divergence {
    let Some(pct) = main_net_pct else { return Divergence::Unknown };
    if jump_pct > 0.0 && pct <= -flow_th { return Divergence::RetailChasing }
    if jump_pct < 0.0 && pct >= flow_th { return Divergence::MainAccumulating }
    Divergence::None
}

/// 短/长线标签。
///
/// 长线 = 估值处历史低分位 **且** 趋势非下跌 → 异动有基本面支撑。
/// 其余全部归短线（纯消息面/情绪驱动）。
///
/// `pe_pct` 为 None（次新股无分位、亏损股 PE 为负无分位可言）时归短线：
/// 无从证明有基本面支撑，就不该声称有。
pub fn classify(pe_pct: Option<f64>, trend: &str, low_pct_th: f64) -> Horizon {
    let cheap = pe_pct.map(|p| p <= low_pct_th).unwrap_or(false);
    let not_falling = !trend.contains("下跌");
    if cheap && not_falling { Horizon::Long } else { Horizon::Short }
}

/// 排序权重：主力净流入占比高的排前面。
///
/// 资金流缺失时退化为按突变幅度排序 —— 不能因为查不到资金流就把这批信号沉底，
/// 它们的价量证据和别的信号一样硬。
pub fn rank_key(m: &Mover) -> f64 {
    match m.main_net_pct {
        Some(p) => p,
        None => m.jump_pct.abs(),
    }
}

/// 按权重降序排序并截断到 limit。
pub fn rank_top(mut movers: Vec<Mover>, limit: usize) -> Vec<Mover> {
    movers.sort_by(|a, b| rank_key(b).partial_cmp(&rank_key(a)).unwrap_or(std::cmp::Ordering::Equal));
    movers.truncate(limit);
    movers
}

/// 从最近两点（倒序：[0] 最新）算突变与量能放大。
///
/// 返回 `(jump_pct, vol_surge_x, baseline_kind, ts, price)`。
/// None 的情形：不足两点、上一时点价格非正、基准不可得。
pub fn compute(
    recent: &[(NaiveDateTime, f64, f64)], history: &[f64], today_avg: Option<f64>,
) -> Option<(f64, f64, Baseline, NaiveDateTime, f64)> {
    if recent.len() < 2 { return None }
    let (ts, price, vol) = recent[0];
    let (_, prev_price, prev_vol) = recent[1];
    if prev_price <= 0.0 { return None }
    let jump = (price - prev_price) / prev_price;
    // 成交量是当日累计，做差得本时点增量
    let vol_delta = (vol - prev_vol).max(0.0);
    let (base, kind) = baseline_volume(history, today_avg)?;
    Some((jump, vol_delta / base, kind, ts, price))
}

/// 快照时点的 (时, 分)，用于对齐历史同时点。
pub fn slot_of(ts: NaiveDateTime) -> (u32, u32) { (ts.hour(), ts.minute()) }

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 16).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }

    fn m(jump: f64, pct: Option<f64>) -> Mover {
        Mover {
            code: "A".into(), name: "测试".into(), ts: dt(10, 0), price: 10.0,
            jump_pct: jump, vol_surge_x: 4.0, main_net: None, main_net_pct: pct,
            divergence: Divergence::None, horizon: Horizon::Short, baseline: Baseline::History,
        }
    }

    #[test]
    fn median_uses_middle_not_mean() {
        // 关键：一个极端值（上次异动的放量）不该抬高基准。均值会被拉到 2525
        assert_eq!(median(&[100.0, 100.0, 100.0, 9900.0]), Some(100.0));
        assert_eq!(median(&[1.0, 2.0, 3.0]), Some(2.0));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn price_alone_does_not_trigger() {
        // 只看价会把开盘半小时的正常波动全捞进来
        assert!(!is_mover(0.05, 1.2, 0.02, 3.0), "价超阈值但量没放大，不是异动");
    }

    #[test]
    fn volume_alone_does_not_trigger() {
        // 只看量会捞到大宗交易导致的放量横盘
        assert!(!is_mover(0.001, 10.0, 0.02, 3.0), "量放大但价没动，不是异动");
    }

    #[test]
    fn both_thresholds_together_trigger() {
        assert!(is_mover(0.03, 4.0, 0.02, 3.0));
    }

    #[test]
    fn downward_jump_triggers_too() {
        // 「涨跌较大」含跌 —— 用绝对值判定，跌 3% 同样是异动
        assert!(is_mover(-0.03, 4.0, 0.02, 3.0), "跌幅也是异动");
    }

    #[test]
    fn strong_signal_requires_multiple_of_threshold() {
        // 弱信号只进库不推送，否则一天数百条刷屏
        assert!(!is_strong(0.029, 4.0, 0.02, 3.0, 1.5), "2.9% < 2%×1.5=3%，不算强");
        assert!(is_strong(0.031, 4.6, 0.02, 3.0, 1.5), "超 1.5 倍阈值才算强");
    }

    #[test]
    fn baseline_prefers_history_over_today_average() {
        let (v, kind) = baseline_volume(&[100.0, 200.0, 300.0], Some(9999.0)).unwrap();
        assert!((v - 200.0).abs() < 1e-9, "有历史就用历史中位数");
        assert_eq!(kind, Baseline::History);
    }

    #[test]
    fn baseline_falls_back_only_when_history_empty_not_when_sparse() {
        // 长假后窗口内可能只剩 3 个交易日 —— 样本少≠没样本，照常用历史。
        // 若实现要求样本满 baseline_days，每年春节后会静默失效
        let (v, kind) = baseline_volume(&[500.0], Some(9999.0)).unwrap();
        assert!((v - 500.0).abs() < 1e-9, "仅 1 个历史样本也该用它");
        assert_eq!(kind, Baseline::History);

        let (v, kind) = baseline_volume(&[], Some(777.0)).unwrap();
        assert!((v - 777.0).abs() < 1e-9);
        assert_eq!(kind, Baseline::Fallback, "无历史才降级，且须显式标记");
    }

    #[test]
    fn baseline_returns_none_when_nothing_available() {
        // 首次部署第一个时点：无历史、无当日均值 → 不判定，而非除零
        assert!(baseline_volume(&[], None).is_none());
        assert!(baseline_volume(&[0.0], Some(0.0)).is_none(), "零基准会导致除零得 inf");
    }

    #[test]
    fn divergence_flags_retail_chasing_on_up_with_outflow() {
        assert_eq!(divergence(0.03, Some(-0.08), 0.05), Divergence::RetailChasing);
    }

    #[test]
    fn divergence_flags_main_accumulating_on_down_with_inflow() {
        assert_eq!(divergence(-0.03, Some(0.08), 0.05), Divergence::MainAccumulating);
    }

    #[test]
    fn divergence_none_when_flow_same_direction_as_price() {
        assert_eq!(divergence(0.03, Some(0.08), 0.05), Divergence::None, "涨+流入不是背离");
        assert_eq!(divergence(-0.03, Some(-0.08), 0.05), Divergence::None, "跌+流出不是背离");
    }

    #[test]
    fn divergence_none_when_flow_below_threshold() {
        // 涨但只流出 1%，达不到「大量」，不算背离
        assert_eq!(divergence(0.03, Some(-0.01), 0.05), Divergence::None);
    }

    #[test]
    fn missing_flow_is_unknown_not_none() {
        // 东财封禁时资金流为 None。「没查到」必须与「查到了没背离」可区分，
        // 否则日后统计会把缺失样本当成「无背离」，污染结论
        assert_eq!(divergence(0.03, None, 0.05), Divergence::Unknown);
        assert_ne!(divergence(0.03, None, 0.05), Divergence::None);
    }

    #[test]
    fn classify_long_requires_both_cheap_and_not_falling() {
        assert_eq!(classify(Some(0.15), "震荡", 0.30), Horizon::Long, "低分位+非下跌=长线");
        assert_eq!(classify(Some(0.15), "下跌", 0.30), Horizon::Short, "低分位但下跌→短线");
        assert_eq!(classify(Some(0.80), "上涨", 0.30), Horizon::Short, "高分位→短线");
    }

    #[test]
    fn classify_without_valuation_defaults_to_short() {
        // 次新股无分位、亏损股 PE 为负无分位可言。无从证明有基本面支撑，
        // 就不该声称有 —— 默认短线是保守的正确方向
        assert_eq!(classify(None, "上涨", 0.30), Horizon::Short);
    }

    #[test]
    fn rank_sorts_by_flow_pct_descending() {
        let out = rank_top(vec![m(0.03, Some(0.02)), m(0.03, Some(0.09)), m(0.03, Some(0.05))], 5);
        assert!((out[0].main_net_pct.unwrap() - 0.09).abs() < 1e-9, "资金流占比高的排前");
    }

    #[test]
    fn rank_truncates_to_limit() {
        let out = rank_top(vec![m(0.03, Some(0.02)); 10], 5);
        assert_eq!(out.len(), 5, "每时点最多 5 只，否则飞书刷屏");
    }

    #[test]
    fn rank_falls_back_to_jump_when_flow_unavailable() {
        // 资金流查不到时不能把这批信号沉底 —— 它们的价量证据一样硬
        let out = rank_top(vec![m(0.02, None), m(0.09, None), m(0.05, None)], 3);
        assert!((out[0].jump_pct - 0.09).abs() < 1e-9, "无资金流时按突变幅度排");
    }

    #[test]
    fn compute_uses_volume_delta_not_cumulative() {
        // 成交量是当日累计。若直接拿累计量比基准，尾盘每只股票都会「放量」——
        // 因为累计量本来就随时间单调增
        let recent = vec![(dt(10, 10), 11.0, 1500.0), (dt(10, 0), 10.0, 1000.0)];
        let (jump, surge, kind, ts, price) = compute(&recent, &[100.0], None).unwrap();
        assert!((jump - 0.1).abs() < 1e-9, "10→11 是 +10%");
        assert!((surge - 5.0).abs() < 1e-9, "增量 500 ÷ 基准 100 = 5 倍，不是 1500÷100");
        assert_eq!(kind, Baseline::History);
        assert_eq!(ts, dt(10, 10), "时间戳取最新那点");
        assert!((price - 11.0).abs() < 1e-9);
    }

    #[test]
    fn compute_needs_two_points() {
        assert!(compute(&[(dt(10, 0), 10.0, 100.0)], &[50.0], None).is_none(), "首个时点无从做差");
        assert!(compute(&[], &[50.0], None).is_none());
    }

    #[test]
    fn compute_guards_against_zero_prev_price() {
        // 停牌复牌首笔可能出现 0 价，直接除会得 inf 并伪造出天量异动
        let recent = vec![(dt(10, 10), 11.0, 1500.0), (dt(10, 0), 0.0, 1000.0)];
        assert!(compute(&recent, &[100.0], None).is_none());
    }

    #[test]
    fn compute_clamps_negative_volume_delta() {
        // 腾讯偶有回退数据（累计量变小）。负增量会算出负放大倍数
        let recent = vec![(dt(10, 10), 11.0, 500.0), (dt(10, 0), 10.0, 1000.0)];
        let (_, surge, _, _, _) = compute(&recent, &[100.0], None).unwrap();
        assert!(surge >= 0.0, "负增量须夹到 0，不得产生负放大倍数");
    }

    #[test]
    fn slot_of_extracts_hour_minute() {
        assert_eq!(slot_of(dt(14, 30)), (14, 30));
    }
}

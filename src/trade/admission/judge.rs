//! 准入阈值判定(spec §10.4)。纯函数:比较指标与阈值,列出不达标原因。

use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::config::AdmissionCfg;

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub passed: bool,
    /// 不达标项说明;通过时为空
    pub reasons: Vec<String>,
}

impl Verdict {
    fn from(reasons: Vec<String>) -> Self {
        Self {
            passed: reasons.is_empty(),
            reasons,
        }
    }
}

/// 回测关(spec §10.4)。
pub fn judge_backtest(m: &PoolMetrics, cfg: &AdmissionCfg) -> Verdict {
    let mut r = Vec::new();
    if m.data_years < cfg.min_years {
        r.push(format!(
            "数据不足:{:.1} 年 < {:.1} 年",
            m.data_years, cfg.min_years
        ));
    }
    if m.oos_return <= 0.0 {
        r.push(format!("样本外收益 {:.1}% ≤ 0", m.oos_return * 100.0));
    } else if m.oos_return <= m.buy_hold_return {
        r.push(format!(
            "样本外收益 {:.1}% 未跑赢买入持有 {:.1}%",
            m.oos_return * 100.0,
            m.buy_hold_return * 100.0
        ));
    }
    if m.oos_sharpe < cfg.min_oos_sharpe {
        r.push(format!(
            "样本外夏普 {:.2} < {:.2}",
            m.oos_sharpe, cfg.min_oos_sharpe
        ));
    }
    if m.oos_max_drawdown > cfg.max_oos_drawdown {
        r.push(format!(
            "样本外最大回撤 {:.1}% > {:.1}%",
            m.oos_max_drawdown * 100.0,
            cfg.max_oos_drawdown * 100.0
        ));
    }
    // F6:按每只中位数判定,而不是池内求和——否则池子越大,阈值形同虚设
    // (60 只票各 1 笔求和 60 笔也能过 30 笔的默认阈值,但没有一只票有效交易过)。
    if m.median_code_trades < cfg.min_oos_trades {
        r.push(format!(
            "样本外每只中位 {} 笔 < {} 笔",
            m.median_code_trades, cfg.min_oos_trades
        ));
    }
    // F5:池内可评估比例太低说明数据覆盖不足,指标不能代表整个股票池。
    if m.requested > 0 && (m.codes.len() as f64 / m.requested as f64) < cfg.min_evaluated_ratio {
        r.push(format!(
            "池内仅 {}/{} 只可评估,低于 {:.0}%",
            m.codes.len(),
            m.requested,
            cfg.min_evaluated_ratio * 100.0
        ));
    }
    if m.is_sharpe > 0.0 {
        let decay = 1.0 - m.oos_sharpe / m.is_sharpe;
        if decay > cfg.max_sharpe_decay {
            r.push(format!(
                "夏普衰减 {:.0}% > {:.0}%(疑似过拟合)",
                decay * 100.0,
                cfg.max_sharpe_decay * 100.0
            ));
        }
    } else {
        r.push(format!("样本内夏普 {:.2} ≤ 0,无法评估过拟合", m.is_sharpe));
    }
    if m.positive_ratio < cfg.min_positive_ratio {
        r.push(format!(
            "池内正收益占比 {:.0}% < {:.0}%",
            m.positive_ratio * 100.0,
            cfg.min_positive_ratio * 100.0
        ));
    }
    Verdict::from(r)
}

/// 观察期实际表现。
#[derive(Debug, Clone, PartialEq)]
pub struct PaperStats {
    pub days: i64,
    pub trades: usize,
    pub avg_trade_return: f64,
    pub max_drawdown: f64,
}

/// 回测基线:观察期表现要与之比较。异动类策略无回测,传 None。
#[derive(Debug, Clone, PartialEq)]
pub struct BacktestBaseline {
    pub avg_trade_return: f64,
    pub trade_return_sd: f64,
    pub max_drawdown: f64,
}

impl BacktestBaseline {
    /// 从池内回测指标取基线:逐笔收益均值与标准差来自 `trade_baseline`,回撤取样本外最大回撤。
    pub fn from_pool(m: &PoolMetrics) -> Self {
        Self {
            avg_trade_return: m.trade_baseline.avg_return,
            trade_return_sd: m.trade_baseline.return_sd,
            max_drawdown: m.oos_max_drawdown,
        }
    }
}

/// 观察期关(spec §10.5):时长、笔数、平均每笔收益不低于回测均值 − 1σ、回撤不超过回测。
/// 异动类策略无历史回测,`baseline` 传 `None` 时只检查时长与笔数。
pub fn judge_paper(
    stats: &PaperStats,
    baseline: Option<&BacktestBaseline>,
    is_mover: bool,
    cfg: &AdmissionCfg,
) -> Verdict {
    let (min_days, min_trades) = if is_mover {
        (cfg.mover_paper_days, cfg.mover_paper_trades)
    } else {
        (cfg.paper_days, cfg.paper_trades)
    };
    let mut r = Vec::new();
    if stats.days < min_days {
        r.push(format!("观察期 {} 个交易日 < {} 日", stats.days, min_days));
    }
    if stats.trades < min_trades {
        r.push(format!("观察期 {} 笔 < {} 笔", stats.trades, min_trades));
    }
    if let Some(baseline) = baseline {
        let floor = baseline.avg_trade_return - baseline.trade_return_sd;
        if stats.avg_trade_return < floor {
            r.push(format!(
                "模拟盘平均每笔收益 {:.2}% 低于回测均值 − 1σ({:.2}%)",
                stats.avg_trade_return * 100.0,
                floor * 100.0
            ));
        }
        if stats.max_drawdown > baseline.max_drawdown {
            r.push(format!(
                "模拟盘最大回撤 {:.1}% 超过回测 {:.1}%",
                stats.max_drawdown * 100.0,
                baseline.max_drawdown * 100.0
            ));
        }
    }
    Verdict::from(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::walk_forward::{CodeMetrics, PoolMetrics, TradeBaseline};

    fn pool(
        sharpe: f64,
        mdd: f64,
        trades: usize,
        ratio: f64,
        years: f64,
        ret: f64,
        is_sharpe: f64,
    ) -> PoolMetrics {
        PoolMetrics {
            codes: vec![CodeMetrics {
                code: "600000".into(),
                windows: 4,
                oos_return: ret,
                oos_annualized: ret,
                oos_sharpe: sharpe,
                oos_max_drawdown: mdd,
                oos_trades: trades,
                is_sharpe,
                years,
                data_years: years,
                buy_hold_return: 0.05,
                trade_baseline: TradeBaseline::default(),
                window_details: Vec::new(),
            }],
            oos_return: ret,
            oos_sharpe: sharpe,
            oos_max_drawdown: mdd,
            oos_trades: trades,
            median_code_trades: trades,
            is_sharpe,
            positive_ratio: ratio,
            years,
            data_years: years,
            buy_hold_return: 0.05,
            trade_baseline: TradeBaseline::default(),
            // 单只股票、全部可评估:覆盖率关不应在这些通用夹具里被触发。
            requested: 1,
            skipped: Vec::new(),
        }
    }

    #[test]
    fn backtest_passes_when_every_threshold_is_met() {
        let v = judge_backtest(
            &pool(1.0, 0.20, 40, 0.60, 4.0, 0.30, 1.4),
            &AdmissionCfg::default(),
        );
        assert!(v.passed, "{:?}", v.reasons);
        assert!(v.reasons.is_empty());
    }

    #[test]
    fn backtest_lists_every_failed_threshold() {
        let v = judge_backtest(
            &pool(0.5, 0.40, 10, 0.30, 2.0, 0.02, 2.0),
            &AdmissionCfg::default(),
        );
        assert!(!v.passed);
        // 夏普、回撤、笔数、正收益占比、数据年限、跑输买入持有、夏普衰减
        assert_eq!(v.reasons.len(), 7, "{:?}", v.reasons);
        assert!(v.reasons.iter().any(|r| r.contains("夏普")));
        assert!(v.reasons.iter().any(|r| r.contains("买入持有")));
        assert!(v.reasons.iter().any(|r| r.contains("衰减")));
    }

    /// F1:数据年限关按 `data_years`(K 线总跨度)判定,与 `years`(样本外检验总跨度)无关。
    #[test]
    fn three_years_of_data_passes_the_data_gate() {
        let mut passing = pool(1.0, 0.20, 40, 0.60, 1.0, 0.30, 1.4);
        passing.data_years = 3.0;
        for c in &mut passing.codes {
            c.data_years = 3.0;
        }
        let v = judge_backtest(&passing, &AdmissionCfg::default());
        assert!(
            !v.reasons.iter().any(|r| r.contains("数据不足")),
            "data_years=3.0、years=1.0 不应触发数据不足: {:?}",
            v.reasons
        );

        let mut failing = passing.clone();
        failing.data_years = 2.9;
        for c in &mut failing.codes {
            c.data_years = 2.9;
        }
        let v2 = judge_backtest(&failing, &AdmissionCfg::default());
        assert_eq!(v2.reasons.len(), 1, "{:?}", v2.reasons);
        assert!(v2.reasons[0].contains("数据不足"), "{:?}", v2.reasons);
    }

    /// F5:请求了 10 只但只有 2 只跑出有效结果,可评估占比 20% < 默认 60%,应报出覆盖率不足。
    #[test]
    fn pool_coverage_is_gated() {
        let mut m = pool(1.0, 0.20, 40, 1.0, 4.0, 0.30, 1.4);
        m.codes.push(m.codes[0].clone());
        m.requested = 10;
        m.skipped = (0..8)
            .map(|i| (format!("00000{i}"), "加载失败".into()))
            .collect();
        let v = judge_backtest(&m, &AdmissionCfg::default());
        assert!(!v.passed);
        assert!(
            v.reasons.iter().any(|r| r.contains("可评估")),
            "{:?}",
            v.reasons
        );
    }

    /// F6:池子越大,「≥30 笔」这道关不能越松——60 只票各只有 1 笔样本外交易(求和 60 笔,
    /// 若按池内求和判定会通过默认阈值 30),中位数判定应仍然拒绝。
    #[test]
    fn median_trades_gate_does_not_weaken_as_pool_grows() {
        let codes: Vec<CodeMetrics> = (0..60)
            .map(|i| CodeMetrics {
                code: format!("60000{i}"),
                windows: 4,
                oos_return: 0.30,
                oos_annualized: 0.30,
                oos_sharpe: 1.0,
                oos_max_drawdown: 0.10,
                oos_trades: 1,
                is_sharpe: 1.4,
                years: 4.0,
                data_years: 4.0,
                buy_hold_return: 0.05,
                trade_baseline: TradeBaseline::default(),
                window_details: Vec::new(),
            })
            .collect();
        let m = crate::trade::admission::walk_forward::aggregate(codes);
        assert_eq!(m.oos_trades, 60, "求和仍然是 60,只是不再用它判定");
        let v = judge_backtest(&m, &AdmissionCfg::default());
        assert!(!v.passed);
        assert_eq!(v.reasons.len(), 1, "{:?}", v.reasons);
        assert!(v.reasons[0].contains("中位"), "{:?}", v.reasons);
    }

    #[test]
    fn negative_return_fails_even_with_good_sharpe() {
        let v = judge_backtest(
            &pool(1.5, 0.10, 50, 0.80, 5.0, -0.10, 1.6),
            &AdmissionCfg::default(),
        );
        assert!(!v.passed);
        assert!(
            v.reasons.iter().any(|r| r.contains("样本外收益")),
            "{:?}",
            v.reasons
        );
    }

    #[test]
    fn nonpositive_is_sharpe_is_flagged_not_waived() {
        // is_sharpe = 0 不能豁免衰减检查,必须单独报出「无法评估过拟合」
        let v = judge_backtest(
            &pool(1.5, 0.10, 50, 0.80, 5.0, 0.30, 0.0),
            &AdmissionCfg::default(),
        );
        assert!(!v.passed);
        assert_eq!(v.reasons.len(), 1, "{:?}", v.reasons);
        assert!(v.reasons[0].contains("样本内夏普"), "{:?}", v.reasons);
    }

    #[test]
    fn thresholds_are_inclusive_at_the_boundary() {
        // 每项都恰好卡在默认阈值上(衰减恰好 = 0.5):应视为通过。
        let v = judge_backtest(
            &pool(0.8, 0.25, 30, 0.55, 3.0, 0.10, 1.6),
            &AdmissionCfg::default(),
        );
        assert!(v.passed, "{:?}", v.reasons);
        assert!(v.reasons.is_empty());

        // 样本外收益恰好等于买入持有:仍算未跑赢。
        let v2 = judge_backtest(
            &pool(0.8, 0.25, 30, 0.55, 3.0, 0.05, 1.6),
            &AdmissionCfg::default(),
        );
        assert!(!v2.passed);
        assert_eq!(v2.reasons.len(), 1, "{:?}", v2.reasons);
        assert!(v2.reasons[0].contains("买入持有"), "{:?}", v2.reasons);
    }

    #[test]
    fn paper_stage_uses_stricter_bar_for_movers() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats {
            days: 25,
            trades: 12,
            avg_trade_return: 0.02,
            max_drawdown: 0.15,
        };
        let baseline = BacktestBaseline {
            avg_trade_return: 0.03,
            trade_return_sd: 0.02,
            max_drawdown: 0.20,
        };
        assert!(judge_paper(&stats, Some(&baseline), false, &cfg).passed);
        let v = judge_paper(&stats, Some(&baseline), true, &cfg);
        assert!(!v.passed, "异动类要求 40 日 / 30 笔");
        assert_eq!(v.reasons.len(), 2, "{:?}", v.reasons);
    }

    #[test]
    fn paper_stage_flags_underperformance_and_deeper_drawdown() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats {
            days: 30,
            trades: 20,
            avg_trade_return: 0.001,
            max_drawdown: 0.35,
        };
        let baseline = BacktestBaseline {
            avg_trade_return: 0.03,
            trade_return_sd: 0.01,
            max_drawdown: 0.20,
        };
        let v = judge_paper(&stats, Some(&baseline), false, &cfg);
        assert!(!v.passed);
        assert!(
            v.reasons.iter().any(|r| r.contains("每笔收益")),
            "{:?}",
            v.reasons
        );
        assert!(
            v.reasons.iter().any(|r| r.contains("回撤")),
            "{:?}",
            v.reasons
        );
    }

    #[test]
    fn paper_avg_trade_return_exactly_at_floor_passes() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats {
            days: 30,
            trades: 20,
            avg_trade_return: 0.02, // == 0.03 - 0.01
            max_drawdown: 0.20,
        };
        let baseline = BacktestBaseline {
            avg_trade_return: 0.03,
            trade_return_sd: 0.01,
            max_drawdown: 0.20,
        };
        let v = judge_paper(&stats, Some(&baseline), false, &cfg);
        assert!(v.passed, "{:?}", v.reasons);
    }

    #[test]
    fn mover_without_baseline_only_checks_days_and_trades() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats {
            days: 45,
            trades: 35,
            avg_trade_return: -0.05,
            max_drawdown: 0.60,
        };
        assert!(
            judge_paper(&stats, None, true, &cfg).passed,
            "无回测基线,收益/回撤不参与判定"
        );
        let short = PaperStats { days: 30, ..stats };
        let v = judge_paper(&short, None, true, &cfg);
        assert!(!v.passed);
        assert_eq!(v.reasons.len(), 1, "{:?}", v.reasons);
        assert!(v.reasons[0].contains("交易日"), "{:?}", v.reasons);
    }

    #[test]
    fn baseline_is_derived_from_pool_metrics() {
        let mut m = pool(1.0, 0.20, 40, 0.60, 4.0, 0.30, 1.4);
        m.trade_baseline.avg_return = 0.03;
        m.trade_baseline.return_sd = 0.01;
        let b = BacktestBaseline::from_pool(&m);
        assert!((b.avg_trade_return - 0.03).abs() < 1e-9);
        assert!((b.trade_return_sd - 0.01).abs() < 1e-9);
        assert!((b.max_drawdown - 0.20).abs() < 1e-9);
    }
}

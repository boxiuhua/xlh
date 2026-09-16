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
    if m.years < cfg.min_years {
        r.push(format!(
            "数据不足:{:.1} 年 < {:.1} 年",
            m.years, cfg.min_years
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
    if m.oos_trades < cfg.min_oos_trades {
        r.push(format!(
            "样本外交易 {} 笔 < {} 笔",
            m.oos_trades, cfg.min_oos_trades
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

/// 观察期关(spec §10.5):时长、笔数、平均每笔收益不低于回测均值 − 1σ、回撤不超过回测。
#[allow(clippy::too_many_arguments)]
pub fn judge_paper(
    stats: &PaperStats,
    backtest_avg_trade_return: f64,
    backtest_trade_return_sd: f64,
    backtest_max_drawdown: f64,
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
    let floor = backtest_avg_trade_return - backtest_trade_return_sd;
    if stats.avg_trade_return < floor {
        r.push(format!(
            "模拟盘平均每笔收益 {:.2}% 低于回测均值 − 1σ({:.2}%)",
            stats.avg_trade_return * 100.0,
            floor * 100.0
        ));
    }
    if stats.max_drawdown > backtest_max_drawdown {
        r.push(format!(
            "模拟盘最大回撤 {:.1}% 超过回测 {:.1}%",
            stats.max_drawdown * 100.0,
            backtest_max_drawdown * 100.0
        ));
    }
    Verdict::from(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::walk_forward::{CodeMetrics, PoolMetrics};

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
                buy_hold_return: 0.05,
            }],
            oos_return: ret,
            oos_sharpe: sharpe,
            oos_max_drawdown: mdd,
            oos_trades: trades,
            is_sharpe,
            positive_ratio: ratio,
            years,
            buy_hold_return: 0.05,
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
    fn paper_stage_uses_stricter_bar_for_movers() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats {
            days: 25,
            trades: 12,
            avg_trade_return: 0.02,
            max_drawdown: 0.15,
        };
        assert!(judge_paper(&stats, 0.03, 0.02, 0.20, false, &cfg).passed);
        let v = judge_paper(&stats, 0.03, 0.02, 0.20, true, &cfg);
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
        let v = judge_paper(&stats, 0.03, 0.01, 0.20, false, &cfg);
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
}

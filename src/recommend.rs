//! 跨基金推荐：综合多因子评分 + 样本外验证 + 形态择时。纯逻辑，IO 由调用方注入。
#[allow(unused_imports)]
use std::collections::HashMap;
use serde::Serialize;

#[allow(unused_imports)]
use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
#[allow(unused_imports)]
use crate::broker::{FeeModel, SellTier};
#[allow(unused_imports)]
use crate::config::build_strategy_from;
use crate::data::NavPoint;
#[allow(unused_imports)]
use crate::metrics::Summary;
#[allow(unused_imports)]
use crate::runner;

pub const DISCLAIMER: &str =
    "基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。";

#[allow(dead_code)]
const MIN_TRAIN: usize = 120;
#[allow(dead_code)]
const MIN_TEST: usize = 30;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScoreWeights {
    pub w_return: f64,
    pub w_sharpe: f64,
    pub w_mdd: f64,
}
impl Default for ScoreWeights {
    fn default() -> Self { Self { w_return: 0.4, w_sharpe: 0.4, w_mdd: 0.2 } }
}

pub struct RecommendParams {
    pub top_n: usize,
    pub split_ratio: f64,
    pub weights: ScoreWeights,
}
impl Default for RecommendParams {
    fn default() -> Self { Self { top_n: 5, split_ratio: 0.70, weights: ScoreWeights::default() } }
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyEval {
    pub kind: String,
    pub name: String,
    pub is_return: f64,
    pub is_sharpe: f64,
    pub is_mdd: f64,
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_mdd: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FundRecommendation {
    pub code: String,
    pub name: String,
    pub fund_score: f64,
    pub best_strategy: StrategyEval,
    pub all_strategies: Vec<StrategyEval>,
    pub regime: RegimeReport,
    pub cadence_hint: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecommendReport {
    pub generated: String,
    pub pool_size: usize,
    pub analyzed: usize,
    pub skipped: Vec<String>,
    pub top: Vec<FundRecommendation>,
    pub weights: ScoreWeights,
    pub split_ratio: f64,
    pub disclaimer: String,
}

/// 总体标准分（population std）。长度<1 返回空；σ≈0 返回全 0（不除零）。
#[allow(dead_code)]
fn zscores(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    if n == 0 { return Vec::new(); }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    let sd = var.sqrt();
    if sd < 1e-12 { return vec![0.0; n]; }
    xs.iter().map(|x| (x - mean) / sd).collect()
}

/// 按比例切训练/检验段；任一段不足最小阈值返回 None。
#[allow(dead_code)]
fn split_history(points: &[NavPoint], split_ratio: f64) -> Option<(&[NavPoint], &[NavPoint])> {
    let cut = (points.len() as f64 * split_ratio).floor() as usize;
    let (train, test) = points.split_at(cut);
    if train.len() >= MIN_TRAIN && test.len() >= MIN_TEST { Some((train, test)) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    /// 构造 acc_nav==nav 的净值序列。
    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    #[test]
    fn zscores_constant_is_zero() {
        assert_eq!(zscores(&[2.0, 2.0, 2.0]), vec![0.0, 0.0, 0.0]);
        assert!(zscores(&[]).is_empty());
    }

    #[test]
    fn zscores_centered_and_scaled() {
        let z = zscores(&[1.0, 2.0, 3.0]);
        assert!((z.iter().sum::<f64>()).abs() < 1e-9, "z 均值应≈0");
        assert!(z[2] > z[1] && z[1] > z[0], "应保序");
    }

    #[test]
    fn split_history_ok_and_too_short() {
        let pts = series(&(0..220).map(|i| 1.0 + i as f64 * 0.001).collect::<Vec<_>>());
        let (tr, te) = split_history(&pts, 0.70).expect("220 点应可切分");
        assert_eq!(tr.len(), 154);
        assert_eq!(te.len(), 66);
        // 160 点 → cut=112 < MIN_TRAIN(120) → None
        let short = series(&(0..160).map(|i| 1.0 + i as f64 * 0.001).collect::<Vec<_>>());
        assert!(split_history(&short, 0.70).is_none());
    }
}

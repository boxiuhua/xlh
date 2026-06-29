//! 跨基金推荐：综合多因子评分 + 样本外验证 + 形态择时。纯逻辑，IO 由调用方注入。
#[allow(unused_imports)]
use std::collections::HashMap;
use serde::Serialize;

#[allow(unused_imports)]
use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
use crate::broker::{FeeModel, SellTier};
use crate::config::build_strategy_from;
use crate::data::NavPoint;
use crate::metrics::Summary;
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

/// 5 个候选策略（kind, 中文名），顺序即展示顺序。
#[allow(dead_code)]
const CANDIDATES: &[(&str, &str)] = &[
    ("dca", "普通定投"),
    ("smart_dca", "智能定投"),
    ("trend", "均线择时"),
    ("rsi", "RSI超买超卖"),
    ("adaptive", "自适应"),
];

/// 各策略固定稳健默认参数（不逐基金寻优，降低过拟合）。
#[allow(dead_code)]
fn default_params(kind: &str) -> toml::Value {
    let mut t = toml::Table::new();
    let s = |x: &str| toml::Value::String(x.to_string());
    match kind {
        "dca" | "adaptive" => {
            t.insert("period".into(), s("monthly"));
            t.insert("day".into(), toml::Value::Integer(1));
            t.insert("base_amount".into(), toml::Value::Float(1000.0));
        }
        "smart_dca" => {
            t.insert("period".into(), s("monthly"));
            t.insert("day".into(), toml::Value::Integer(1));
            t.insert("base_amount".into(), toml::Value::Float(1000.0));
            t.insert("ma_window".into(), toml::Value::Integer(250));
            t.insert("k".into(), toml::Value::Float(1.0));
        }
        "trend" => {
            t.insert("short_window".into(), toml::Value::Integer(20));
            t.insert("long_window".into(), toml::Value::Integer(60));
            t.insert("amount".into(), toml::Value::Float(1000.0));
        }
        "rsi" => {
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(30.0));
            t.insert("overbought".into(), toml::Value::Float(70.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
        }
        _ => {}
    }
    toml::Value::Table(t)
}

/// 评分用费率：买入 0、卖出标准阶梯。
#[allow(dead_code)]
fn rec_fee() -> FeeModel {
    FeeModel {
        buy_rate: 0.0,
        sell_tiers: vec![
            SellTier { max_days: 7, rate: 0.015 },
            SellTier { max_days: 365, rate: 0.005 },
            SellTier { max_days: 0, rate: 0.0 },
        ],
    }
}

/// 在给定净值段上跑某策略，返回绩效摘要。固定默认参数保证 build 不失败。
#[allow(dead_code)]
fn run_metrics(kind: &str, points: &[NavPoint]) -> Summary {
    let strat = build_strategy_from(kind, &Some(default_params(kind)), &[])
        .expect("固定默认参数构建策略不应失败");
    let outcome = runner::run_one(
        kind.to_string(), String::new(), points.to_vec(), strat, rec_fee(), 0.0);
    outcome.summary
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

    #[test]
    fn default_params_build_all_candidates() {
        for (kind, _) in CANDIDATES {
            let r = build_strategy_from(kind, &Some(default_params(kind)), &[]);
            assert!(r.is_ok(), "{kind} 默认参数应能构建策略: {:?}", r.err());
        }
    }

    #[test]
    fn run_metrics_finite_on_uptrend() {
        // 300 点温和上涨，足够各策略均线/RSI 窗口
        let vals: Vec<f64> = (0..300).map(|i| 1.0 + i as f64 * 0.003).collect();
        let s = run_metrics("smart_dca", &series(&vals));
        assert!(s.total_return.is_finite() && s.sharpe.is_finite());
        assert!(s.max_drawdown >= 0.0);
    }
}

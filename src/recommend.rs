//! 跨基金推荐：综合多因子评分 + 样本外验证 + 形态择时。纯逻辑，IO 由调用方注入。
use std::collections::HashMap;
use serde::Serialize;

use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
use crate::broker::{FeeModel, SellTier};
use crate::config::build_strategy_from;
use crate::data::NavPoint;
use crate::metrics::Summary;
use crate::runner;

pub const DISCLAIMER: &str =
    "基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。";

pub const MIN_TRAIN: usize = 120;
pub const MIN_TEST: usize = 30;

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
///
/// `optimize.rs` 也复用它 —— 寻优此前在**同一段数据**上既选参数又报绩效（纯 in-sample
/// argmax），而本模块早就做了 70/30 切分。同一个项目不该有两套标准。
pub fn split_history(points: &[NavPoint], split_ratio: f64) -> Option<(&[NavPoint], &[NavPoint])> {
    let cut = (points.len() as f64 * split_ratio).floor() as usize;
    let (train, test) = points.split_at(cut);
    if train.len() >= MIN_TRAIN && test.len() >= MIN_TEST { Some((train, test)) } else { None }
}

/// 5 个候选策略（kind, 中文名），顺序即展示顺序。
const CANDIDATES: &[(&str, &str)] = &[
    ("dca", "普通定投"),
    ("smart_dca", "智能定投"),
    ("trend", "均线择时"),
    ("rsi", "RSI超买超卖"),
    ("adaptive", "自适应"),
];

/// 各策略固定稳健默认参数（不逐基金寻优，降低过拟合）。
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

/// 申购费率。0.15% 是各代销渠道打一折后的常见档位，与 Web 表单默认值一致。
///
/// 这里曾经是 **0**。后果不是"少算了一点钱"，而是**扭曲了策略之间的比较**：
/// 5 个候选里 `trend`/`rsi`/`adaptive` 会频繁进出（几十上百笔），`dca`/`smart_dca`
/// 每月一笔 —— 申购费按笔收，免掉它等于系统性补贴高换手策略。
/// 而"哪个策略最优"正是推荐 tab 的核心产出。
pub const REC_BUY_RATE: f64 = 0.0015;

/// 评分用费率：申购 0.15% + 卖出标准阶梯（<7日 1.5% 惩罚性赎回费是监管强制的）。
fn rec_fee() -> FeeModel {
    FeeModel {
        buy_rate: REC_BUY_RATE,
        sell_tiers: vec![
            SellTier { max_days: 7, rate: 0.015 },
            SellTier { max_days: 365, rate: 0.005 },
            SellTier { max_days: 0, rate: 0.0 },
        ],
    }
}

/// 在给定净值段上跑某策略，返回绩效摘要。固定默认参数保证 build 不失败。
fn run_metrics(kind: &str, points: &[NavPoint]) -> Summary {
    let strat = build_strategy_from(kind, &Some(default_params(kind)), &[])
        .expect("固定默认参数构建策略不应失败");
    let outcome = runner::run_one(
        kind.to_string(), String::new(), points.to_vec(), strat, rec_fee(), 0.0);
    outcome.summary
}

/// 形态+行动计划；数据不足/波动为零时降级为占位报告（不致命）。
fn regime_or_fallback(points: &[NavPoint]) -> RegimeReport {
    let rp = RegimeParams::default();
    let pp = PlanParams::default();
    if let Ok(r) = analyze::detect_regime_with_plan(points, &rp, &pp) { return r; }
    if let Ok(r) = analyze::detect_regime(points, &rp) { return r; }
    RegimeReport {
        regime: "数据不足".into(), window: 0, window_return: 0.0, annualized_vol: 0.0,
        ma_short: 0.0, ma_long: 0.0, ma_relation: "未知".into(),
        rec_strategy: String::new(), rec_name: String::new(),
        rationale: "数据不足，暂不给出形态与择时点".into(), plan: None,
    }
}

/// 按形态给投资节奏建议。
fn cadence_for(regime: &str) -> String {
    match regime {
        "上涨趋势" => "顺势持有 / 坚持定投，勿过早下车",
        "下跌趋势" => "谨慎，仅 −2σ 小额试探或观望",
        "震荡" => "按波动带分批：低吸线买、高抛线减",
        _ => "数据不足，暂不给择时节奏",
    }.to_string()
}

/// 对单只基金净值产出推荐（fund_score 留待 rank_top 跨基金标准化）。
/// 数据不足返回 Err，调用方据此跳过。
pub fn evaluate_fund(
    code: &str, name: &str, points: &[NavPoint], p: &RecommendParams,
) -> anyhow::Result<FundRecommendation> {
    let (train, test) = split_history(points, p.split_ratio).ok_or_else(|| {
        anyhow::anyhow!("数据不足: 需训练≥{} 检验≥{} 个净值点（当前 {}）", MIN_TRAIN, MIN_TEST, points.len())
    })?;

    // 每个候选在训练段与检验段各回测一次。
    let mut evals: Vec<StrategyEval> = Vec::with_capacity(CANDIDATES.len());
    for (kind, name_cn) in CANDIDATES {
        let is_s = run_metrics(kind, train);
        let oos_s = run_metrics(kind, test);
        evals.push(StrategyEval {
            kind: (*kind).to_string(), name: (*name_cn).to_string(),
            is_return: is_s.total_return, is_sharpe: is_s.sharpe, is_mdd: is_s.max_drawdown,
            oos_return: oos_s.total_return, oos_sharpe: oos_s.sharpe, oos_mdd: oos_s.max_drawdown,
            score: 0.0,
        });
    }

    // 训练段三项指标跨候选标准化 → 综合评分。
    let z_ret = zscores(&evals.iter().map(|e| e.is_return).collect::<Vec<_>>());
    let z_sh = zscores(&evals.iter().map(|e| e.is_sharpe).collect::<Vec<_>>());
    let z_mdd = zscores(&evals.iter().map(|e| e.is_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, e) in evals.iter_mut().enumerate() {
        e.score = w.w_return * z_ret[i] + w.w_sharpe * z_sh[i] - w.w_mdd * z_mdd[i];
    }

    let best_idx = evals.iter().enumerate()
        .max_by(|(_, a), (_, b)| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i).unwrap_or(0);
    let best = evals[best_idx].clone();

    let regime = regime_or_fallback(points);
    let cadence_hint = cadence_for(&regime.regime);
    let rationale = format!(
        "训练段(前{:.0}%)在 5 个候选策略中『{}』综合评分最高（收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%）；\
         检验段(后{:.0}%)样本外实测 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%。当前形态：{}。",
        p.split_ratio * 100.0, best.name, best.is_return * 100.0, best.is_sharpe, best.is_mdd * 100.0,
        (1.0 - p.split_ratio) * 100.0, best.oos_return * 100.0, best.oos_sharpe, best.oos_mdd * 100.0,
        regime.regime,
    );

    Ok(FundRecommendation {
        code: code.to_string(), name: name.to_string(), fund_score: 0.0,
        best_strategy: best, all_strategies: evals, regime, cadence_hint, rationale,
    })
}

/// 用各基金「最优策略的样本外三项指标」跨基金 z-score 综合评分，降序取 top_n。
pub fn rank_top(mut recs: Vec<FundRecommendation>, p: &RecommendParams) -> Vec<FundRecommendation> {
    if recs.is_empty() { return recs; }
    let zr = zscores(&recs.iter().map(|r| r.best_strategy.oos_return).collect::<Vec<_>>());
    let zs = zscores(&recs.iter().map(|r| r.best_strategy.oos_sharpe).collect::<Vec<_>>());
    let zm = zscores(&recs.iter().map(|r| r.best_strategy.oos_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, r) in recs.iter_mut().enumerate() {
        r.fund_score = w.w_return * zr[i] + w.w_sharpe * zs[i] - w.w_mdd * zm[i];
    }
    recs.sort_by(|a, b| b.fund_score.partial_cmp(&a.fund_score).unwrap_or(std::cmp::Ordering::Equal));
    recs.truncate(p.top_n);
    recs
}

/// 遍历基金池：用注入的 `load` 取净值 → `evaluate_fund` → `rank_top`，装配整页报告。
/// IO（联网/读盘）经闭包注入，便于离线单测。
pub fn build_report<F>(
    pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams, mut load: F,
) -> RecommendReport
where
    F: FnMut(&str) -> anyhow::Result<Vec<NavPoint>>,
{
    let mut recs = Vec::new();
    let mut skipped = Vec::new();
    for &code in pool {
        match load(code) {
            Ok(points) => {
                let name = names.get(code).cloned().unwrap_or_else(|| code.to_string());
                match evaluate_fund(code, &name, &points, p) {
                    Ok(r) => recs.push(r),
                    Err(_) => skipped.push(code.to_string()),
                }
            }
            Err(_) => skipped.push(code.to_string()),
        }
    }
    let analyzed = recs.len();
    let top = rank_top(recs, p);
    RecommendReport {
        generated: today.to_string(),
        pool_size: pool.len(),
        analyzed,
        skipped,
        top,
        weights: p.weights,
        split_ratio: p.split_ratio,
        disclaimer: DISCLAIMER.to_string(),
    }
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

    #[test]
    fn evaluate_fund_ok_on_uptrend() {
        let vals: Vec<f64> = (0..300).map(|i| 1.0 + i as f64 * 0.004).collect();
        let r = evaluate_fund("000001", "测试基金", &series(&vals), &RecommendParams::default())
            .expect("300 点上涨应可评估");
        assert_eq!(r.all_strategies.len(), 5, "应评估全部 5 候选");
        assert!(!r.rationale.is_empty(), "应有依据文案");
        assert!(!r.regime.regime.is_empty(), "应有形态标签");
        // best_strategy 必在候选集合内
        assert!(CANDIDATES.iter().any(|(k, _)| *k == r.best_strategy.kind));
        // best 的 score 应为各候选最大
        let max = r.all_strategies.iter().map(|e| e.score).fold(f64::MIN, f64::max);
        assert!((r.best_strategy.score - max).abs() < 1e-9, "best 应为最高分");
    }

    #[test]
    fn evaluate_fund_too_short_errors() {
        let vals: Vec<f64> = (0..100).map(|i| 1.0 + i as f64 * 0.001).collect();
        let err = evaluate_fund("000001", "x", &series(&vals), &RecommendParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn build_report_ranks_and_skips() {
        // 池：两只可分析（不同斜率）+ 一只加载失败
        let names: HashMap<String, String> = [("000001".to_string(), "甲".to_string())].into_iter().collect();
        let p = RecommendParams::default();
        let rep = build_report(&["000001", "000002", "BADX"], &names, "2026-06-29", &p, |code| {
            match code {
                "000001" => Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.005).collect::<Vec<_>>())),
                "000002" => Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.002).collect::<Vec<_>>())),
                _ => Err(anyhow::anyhow!("加载失败")),
            }
        });
        assert_eq!(rep.pool_size, 3);
        assert_eq!(rep.analyzed, 2, "两只成功");
        assert_eq!(rep.skipped, vec!["BADX".to_string()]);
        assert_eq!(rep.top.len(), 2);
        // 按 fund_score 降序
        assert!(rep.top[0].fund_score >= rep.top[1].fund_score);
        assert_eq!(rep.generated, "2026-06-29");
        assert!(!rep.top[0].name.is_empty());
    }

    #[test]
    fn report_serializes_frontend_keys() {
        let names = HashMap::new();
        let p = RecommendParams::default();
        let rep = build_report(&["000001"], &names, "2026-06-29", &p, |_| {
            Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.004).collect::<Vec<_>>()))
        });
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"top\"", "\"best_strategy\"", "\"all_strategies\"", "\"regime\"",
                    "\"rationale\"", "\"cadence_hint\"", "\"weights\"", "\"split_ratio\"",
                    "\"disclaimer\"", "\"fund_score\"", "\"skipped\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }

    #[test]
    fn build_report_empty_pool_is_valid() {
        let names = HashMap::new();
        let rep = build_report(&[], &names, "2026-06-29", &RecommendParams::default(), |_| {
            Ok(Vec::new())
        });
        assert_eq!(rep.analyzed, 0);
        assert!(rep.top.is_empty());
    }
}

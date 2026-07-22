//! 跨股票选股/推荐：多策略样本外评分 + z-score 排名 + 技术诊断。纯逻辑，IO 注入。
use std::collections::HashMap;
use serde::Serialize;

use crate::metrics::Summary;
use crate::strategy::{Period, Strategy};
use crate::strategy::dca::Dca;
use crate::strategy::smart_dca::SmartDca;
use crate::strategy::trend::Trend;
use crate::strategy::rsi::Rsi;
use crate::strategy::adaptive::Adaptive;
use crate::stock::backtest;
use crate::stock::fee::StockFee;
use crate::stock::data::StockBar;
use crate::stock::diagnose::{self, DiagnoseParams, StockDiagnosis};
use crate::stock::screen::{Exclusion, Profile};

pub const DISCLAIMER: &str =
    "基于历史行情的统计回测与技术指标启发式，不预测未来走势，不构成任何投资建议。";

const MIN_TRAIN: usize = 120;
const MIN_TEST: usize = 30;

/// 基本面闸门判定结果（随推荐项序列化给前端）。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum GateStatus {
    /// A 股，已过基本面闸门。
    Passed,
    /// 港/美股或数据缺失，未经闸门 —— 附原因。
    NotApplicable { reason: String },
}

/// 闸门对单只股票的裁定（`build_report` 内部消费，不序列化）。
#[derive(Debug)]
pub enum GateOutcome {
    /// 过闸，携带质量画像。
    Passed(Profile),
    /// 被排除（不进技术排名）。
    Excluded(Exclusion),
    /// 不适用（保留、标注、无画像）。
    NotApplicable(String),
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScoreWeights { pub w_return: f64, pub w_sharpe: f64, pub w_mdd: f64 }
impl Default for ScoreWeights {
    fn default() -> Self { Self { w_return: 0.4, w_sharpe: 0.4, w_mdd: 0.2 } }
}

pub struct RecommendParams { pub top_n: usize, pub split_ratio: f64, pub weights: ScoreWeights, pub fee: StockFee }
impl Default for RecommendParams {
    fn default() -> Self { Self { top_n: 5, split_ratio: 0.70, weights: ScoreWeights::default(), fee: StockFee::a_share() } }
}

#[derive(Debug, Clone, Serialize)]
pub struct StockStrategyEval {
    pub kind: String, pub name: String,
    pub is_return: f64, pub is_sharpe: f64, pub is_mdd: f64,
    pub oos_return: f64, pub oos_sharpe: f64, pub oos_mdd: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendation {
    pub code: String, pub name: String, pub stock_score: f64,
    pub best_strategy: StockStrategyEval,
    pub all_strategies: Vec<StockStrategyEval>,
    pub diagnosis: StockDiagnosis,
    pub rationale: String,
    /// 基本面质量画像（A 股过闸者 Some）。**独立上下文，绝不折算进 stock_score。**
    pub profile: Option<Profile>,
    /// 基本面闸门状态。
    pub gate: GateStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct StockRecommendReport {
    pub generated: String, pub pool_size: usize, pub analyzed: usize,
    pub skipped: Vec<String>,
    /// 被基本面闸门在技术排名前踢掉的股票：(代码, 排除理由)。
    pub gate_excluded: Vec<(String, String)>,
    pub top: Vec<StockRecommendation>,
    pub weights: ScoreWeights, pub split_ratio: f64, pub disclaimer: String,
}

/// 总体标准分：空→空；σ≈0→全0。
fn zscores(xs: &[f64]) -> Vec<f64> {
    let n = xs.len();
    if n == 0 { return Vec::new(); }
    let mean = xs.iter().sum::<f64>() / n as f64;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
    let sd = var.sqrt();
    if sd < 1e-12 { return vec![0.0; n]; }
    xs.iter().map(|x| (x - mean) / sd).collect()
}

fn split_history(bars: &[StockBar], split_ratio: f64) -> Option<(&[StockBar], &[StockBar])> {
    let cut = (bars.len() as f64 * split_ratio).floor() as usize;
    let (train, test) = bars.split_at(cut);
    if train.len() >= MIN_TRAIN && test.len() >= MIN_TEST { Some((train, test)) } else { None }
}

/// 5 个候选（kind, 中文名），顺序即展示顺序。
const CANDIDATES: &[(&str, &str)] = &[
    ("dca", "普通定投"),
    ("smart_dca", "智能定投"),
    ("trend", "均线择时"),
    ("rsi", "RSI超买超卖"),
    ("adaptive", "自适应"),
];

/// 用共用策略构造器建候选（固定稳健默认参数）。
fn candidate(kind: &str) -> Box<dyn Strategy> {
    match kind {
        "smart_dca" => Box::new(SmartDca::new(Period::Monthly, 1, 1000.0, 250, 1.0)),
        "trend" => Box::new(Trend::new(20, 60, 1000.0)),
        "rsi" => Box::new(Rsi::new(14, 30.0, 70.0, 1000.0)),
        "adaptive" => Box::new(Adaptive::new(Period::Monthly, 1, 1000.0)),
        _ => Box::new(Dca::new(Period::Monthly, 1, 1000.0)),
    }
}

fn run_metrics(kind: &str, bars: &[StockBar], fee: StockFee) -> Summary {
    let strat = candidate(kind);
    backtest::run_one(kind.to_string(), String::new(), bars.to_vec(), strat, fee, 0.0).summary
}

fn diagnose_or_fallback(code: &str, name: &str, bars: &[StockBar]) -> StockDiagnosis {
    diagnose::diagnose(code.to_string(), name.to_string(), bars, &DiagnoseParams::default())
        .unwrap_or_else(|_| StockDiagnosis {
            code: code.to_string(), name: name.to_string(),
            trend: "数据不足".into(), signal: "观望".into(), ma_relation: "未知".into(),
            rationale: "数据不足，暂不给出技术诊断".into(),
            ..Default::default()
        })
}

/// 对单只股票产出推荐（stock_score 留待 rank_top 跨股标准化）。
pub fn evaluate_stock(code: &str, name: &str, bars: &[StockBar], p: &RecommendParams) -> anyhow::Result<StockRecommendation> {
    let (train, test) = split_history(bars, p.split_ratio).ok_or_else(|| {
        anyhow::anyhow!("数据不足: 需训练≥{} 检验≥{} 个交易日（当前 {}）", MIN_TRAIN, MIN_TEST, bars.len())
    })?;

    let mut evals: Vec<StockStrategyEval> = Vec::with_capacity(CANDIDATES.len());
    for (kind, name_cn) in CANDIDATES {
        let is_s = run_metrics(kind, train, p.fee);
        let oos_s = run_metrics(kind, test, p.fee);
        evals.push(StockStrategyEval {
            kind: (*kind).to_string(), name: (*name_cn).to_string(),
            is_return: is_s.total_return, is_sharpe: is_s.sharpe, is_mdd: is_s.max_drawdown,
            oos_return: oos_s.total_return, oos_sharpe: oos_s.sharpe, oos_mdd: oos_s.max_drawdown,
            score: 0.0,
        });
    }

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

    let diagnosis = diagnose_or_fallback(code, name, bars);
    let rationale = format!(
        "训练段(前{:.0}%)5 候选中『{}』综合评分最高（收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%）；\
         检验段(后{:.0}%)样本外 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%。技术面：{}（{}）。",
        p.split_ratio * 100.0, best.name, best.is_return * 100.0, best.is_sharpe, best.is_mdd * 100.0,
        (1.0 - p.split_ratio) * 100.0, best.oos_return * 100.0, best.oos_sharpe, best.oos_mdd * 100.0,
        diagnosis.trend, diagnosis.signal,
    );

    Ok(StockRecommendation {
        code: code.to_string(), name: name.to_string(), stock_score: 0.0,
        best_strategy: best, all_strategies: evals, diagnosis, rationale,
        profile: None,
        gate: GateStatus::Passed,
    })
}

/// 用各股「最优策略样本外三指标」跨股 z-score 综合评分，降序取 top_n。
pub fn rank_top(mut recs: Vec<StockRecommendation>, p: &RecommendParams) -> Vec<StockRecommendation> {
    if recs.is_empty() { return recs; }
    let zr = zscores(&recs.iter().map(|r| r.best_strategy.oos_return).collect::<Vec<_>>());
    let zs = zscores(&recs.iter().map(|r| r.best_strategy.oos_sharpe).collect::<Vec<_>>());
    let zm = zscores(&recs.iter().map(|r| r.best_strategy.oos_mdd).collect::<Vec<_>>());
    let w = &p.weights;
    for (i, r) in recs.iter_mut().enumerate() {
        r.stock_score = w.w_return * zr[i] + w.w_sharpe * zs[i] - w.w_mdd * zm[i];
    }
    recs.sort_by(|a, b| b.stock_score.partial_cmp(&a.stock_score).unwrap_or(std::cmp::Ordering::Equal));
    recs.truncate(p.top_n);
    recs
}

/// 遍历股票池：先过基本面闸门（gate），过闸/不适用者再注入 loader 取 bars → evaluate_stock
/// → rank_top，装配整页报告。被 gate 排除者不进技术排名。
pub fn build_report<F, G>(
    pool: &[&str], names: &HashMap<String, String>, today: &str, p: &RecommendParams,
    mut load: F, mut gate: G,
) -> StockRecommendReport
where
    F: FnMut(&str) -> anyhow::Result<Vec<StockBar>>,
    G: FnMut(&str) -> GateOutcome,
{
    let mut recs = Vec::new();
    let mut skipped = Vec::new();
    let mut gate_excluded: Vec<(String, String)> = Vec::new();
    for &code in pool {
        let (gate_status, profile) = match gate(code) {
            GateOutcome::Excluded(e) => { gate_excluded.push((code.to_string(), e.reason())); continue; }
            GateOutcome::Passed(prof) => (GateStatus::Passed, Some(prof)),
            GateOutcome::NotApplicable(reason) => (GateStatus::NotApplicable { reason }, None),
        };
        match load(code) {
            Ok(bars) => {
                let name = names.get(code).cloned().unwrap_or_else(|| code.to_string());
                match evaluate_stock(code, &name, &bars, p) {
                    Ok(mut r) => { r.profile = profile; r.gate = gate_status; recs.push(r); }
                    Err(_) => skipped.push(code.to_string()),
                }
            }
            Err(_) => skipped.push(code.to_string()),
        }
    }
    let analyzed = recs.len();
    let top = rank_top(recs, p);
    StockRecommendReport {
        generated: today.to_string(),
        pool_size: pool.len(),
        analyzed,
        skipped,
        gate_excluded,
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

    fn series(vals: &[f64]) -> Vec<StockBar> {
        vals.iter().enumerate().map(|(i, v)| StockBar {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            open: *v, high: *v, low: *v, close: *v, volume: 0.0, adj_close: *v,
        }).collect()
    }

    fn load_two(code: &str) -> anyhow::Result<Vec<StockBar>> {
        match code {
            "600519" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.5).collect::<Vec<_>>())),
            _ => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.2).collect::<Vec<_>>())),
        }
    }

    fn dummy_profile() -> Profile {
        Profile {
            code: "600519".into(), name: "茅台".into(), years: 10,
            roe_median: Some(30.0), roe_streak: 10, revenue_cagr: Some(20.0),
            profit_cagr: Some(20.0), gross_margin: Some(90.0),
            market_cap: 1.5e12, pe_ttm: Some(18.0), pe_percentile: Some(0.3),
            note: "测试画像".into(),
        }
    }

    #[test]
    fn zscores_constant_and_empty() {
        assert_eq!(zscores(&[2.0, 2.0, 2.0]), vec![0.0, 0.0, 0.0]);
        assert!(zscores(&[]).is_empty());
    }

    #[test]
    fn zscores_ordered() {
        let z = zscores(&[1.0, 2.0, 3.0]);
        assert!((z.iter().sum::<f64>()).abs() < 1e-9);
        assert!(z[2] > z[1] && z[1] > z[0]);
    }

    #[test]
    fn split_history_ok_and_short() {
        let pts = series(&(0..220).map(|i| 100.0 + i as f64 * 0.1).collect::<Vec<_>>());
        let (tr, te) = split_history(&pts, 0.70).expect("220 点应可切分");
        assert_eq!(tr.len(), 154);
        assert_eq!(te.len(), 66);
        let short = series(&(0..160).map(|i| 100.0 + i as f64 * 0.1).collect::<Vec<_>>());
        assert!(split_history(&short, 0.70).is_none());
    }

    #[test]
    fn evaluate_stock_ok_on_uptrend() {
        let vals: Vec<f64> = (0..300).map(|i| 100.0 + i as f64 * 0.4).collect();
        let r = evaluate_stock("600519", "茅台", &series(&vals), &RecommendParams::default())
            .expect("300 点上涨应可评估");
        assert_eq!(r.all_strategies.len(), 5);
        assert!(!r.rationale.is_empty());
        assert!(!r.diagnosis.trend.is_empty());
        assert!(CANDIDATES.iter().any(|(k, _)| *k == r.best_strategy.kind));
        let max = r.all_strategies.iter().map(|e| e.score).fold(f64::MIN, f64::max);
        assert!((r.best_strategy.score - max).abs() < 1e-9, "best 应为最高分");
    }

    #[test]
    fn evaluate_stock_too_short_errors() {
        let vals: Vec<f64> = (0..100).map(|i| 100.0 + i as f64 * 0.1).collect();
        let err = evaluate_stock("x", "x", &series(&vals), &RecommendParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn build_report_ranks_and_skips() {
        let names: HashMap<String, String> = [("600519".to_string(), "茅台".to_string())].into_iter().collect();
        let p = RecommendParams::default();
        let rep = build_report(&["600519", "000001", "BADX"], &names, "2026-07-01", &p, |code| {
            match code {
                "600519" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.5).collect::<Vec<_>>())),
                "000001" => Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.2).collect::<Vec<_>>())),
                _ => Err(anyhow::anyhow!("加载失败")),
            }
        }, |_| GateOutcome::NotApplicable("测试".into()));
        assert_eq!(rep.pool_size, 3);
        assert_eq!(rep.analyzed, 2);
        assert_eq!(rep.skipped, vec!["BADX".to_string()]);
        assert_eq!(rep.top.len(), 2);
        assert!(rep.top[0].stock_score >= rep.top[1].stock_score);
        assert_eq!(rep.generated, "2026-07-01");
    }

    #[test]
    fn build_report_empty_pool() {
        let names = HashMap::new();
        let rep = build_report(&[], &names, "2026-07-01", &RecommendParams::default(),
            |_| Ok(Vec::new()), |_| GateOutcome::NotApplicable("测试".into()));
        assert_eq!(rep.analyzed, 0);
        assert!(rep.top.is_empty());
    }

    #[test]
    fn report_serializes_frontend_keys() {
        let names = HashMap::new();
        let rep = build_report(&["600519"], &names, "2026-07-01", &RecommendParams::default(), |_| {
            Ok(series(&(0..300).map(|i| 100.0 + i as f64 * 0.4).collect::<Vec<_>>()))
        }, |_| GateOutcome::Passed(dummy_profile()));
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"top\"", "\"best_strategy\"", "\"all_strategies\"", "\"diagnosis\"",
                    "\"rationale\"", "\"weights\"", "\"split_ratio\"", "\"disclaimer\"",
                    "\"stock_score\"", "\"skipped\"", "\"gate_excluded\"", "\"gate\"", "\"profile\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }

    #[test]
    fn gate_excludes_before_technical_ranking() {
        let names: HashMap<String, String> = HashMap::new();
        let rep = build_report(&["600519", "LOSSY"], &names, "2026-07-01", &RecommendParams::default(),
            load_two,
            |code| if code == "LOSSY" { GateOutcome::Excluded(Exclusion::LossMaking) }
                   else { GateOutcome::Passed(dummy_profile()) });
        assert!(rep.top.iter().all(|r| r.code != "LOSSY"), "被排除者不进技术排名");
        assert!(rep.gate_excluded.iter().any(|(c, _)| c == "LOSSY"), "排除应记入 gate_excluded");
        assert!(rep.top.iter().any(|r| r.code == "600519" && r.profile.is_some()), "过闸者应带 Profile");
    }

    #[test]
    fn non_a_share_kept_and_marked_not_applicable() {
        let names: HashMap<String, String> = HashMap::new();
        let rep = build_report(&["AAPL"], &names, "2026-07-01", &RecommendParams::default(),
            load_two, |_| GateOutcome::NotApplicable("非A股".into()));
        assert_eq!(rep.top.len(), 1, "不适用者保留在推荐里");
        assert!(rep.top[0].profile.is_none());
        match &rep.top[0].gate {
            GateStatus::NotApplicable { reason } => assert!(reason.contains("非A股")),
            GateStatus::Passed => panic!("应为 NotApplicable"),
        }
    }

    #[test]
    fn profile_does_not_affect_ranking_score() {
        // 同样两只股票：一次带 Profile 过闸、一次 NotApplicable，stock_score 必须完全一致
        // —— 证明 Profile 是旁证、绝没被折算进排名分（铁律）。
        let names: HashMap<String, String> = HashMap::new();
        let with_prof = build_report(&["600519", "000001"], &names, "2026-07-01",
            &RecommendParams::default(), load_two, |_| GateOutcome::Passed(dummy_profile()));
        let without = build_report(&["600519", "000001"], &names, "2026-07-01",
            &RecommendParams::default(), load_two, |_| GateOutcome::NotApplicable("非A股".into()));
        assert_eq!(with_prof.top.len(), without.top.len());
        for (a, b) in with_prof.top.iter().zip(without.top.iter()) {
            assert_eq!(a.code, b.code, "排名顺序不受 Profile 影响");
            assert!((a.stock_score - b.stock_score).abs() < 1e-12, "分数不受 Profile 影响");
        }
    }
}

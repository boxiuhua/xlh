//! 持仓建议：录入用户持仓 → 逐只多策略评估 + 当下择时 → 操作建议(加仓/持有/减仓/止盈/观望)+建议金额。
//! 纯逻辑，净值 IO 由调用方注入（同 recommend.rs 风格）。
use serde::{Deserialize, Serialize};

use crate::analyze::RegimeReport;
use crate::data::NavPoint;
use crate::recommend::{self, RecommendParams, StrategyEval, DISCLAIMER};

/// 集中度警示阈值：单只权重超过此值即提示。
const CONCENTRATION_LIMIT: f64 = 0.40;

/// 单只持仓输入。
#[derive(Debug, Clone, Deserialize)]
pub struct Holding {
    pub code: String,
    #[serde(default)]
    pub amount: f64,
    #[serde(default)]
    pub profit: f64,
}

/// 组合输入：顶部三项选填，缺失时由各只推导/留空。
#[derive(Debug, Clone, Deserialize)]
pub struct HoldingsInput {
    #[serde(default)]
    pub total_amount: Option<f64>,
    #[serde(default)]
    pub total_profit: Option<f64>,
    #[serde(default)]
    pub cumulative_profit: Option<f64>,
    #[serde(default)]
    pub holdings: Vec<Holding>,
}

/// 逐只建议。
#[derive(Debug, Clone, Serialize)]
pub struct HoldingAdvice {
    pub code: String,
    pub name: String,
    pub amount: f64,
    pub profit: f64,
    pub weight: f64,
    pub action: String,
    /// 建议金额（方向由 action 表意，数值恒 ≥0）。
    pub suggest_amount: f64,
    pub signal: String,
    pub z: f64,
    pub best_strategy: StrategyEval,
    pub all_strategies: Vec<StrategyEval>,
    pub regime: RegimeReport,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioSummary {
    pub total_amount: f64,
    pub total_profit: Option<f64>,
    pub cumulative_profit: Option<f64>,
    pub holding_count: usize,
    pub total_add: f64,
    pub total_trim: f64,
    pub concentration_note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoldingsReport {
    pub generated: String,
    pub summary: PortfolioSummary,
    pub advices: Vec<HoldingAdvice>,
    pub skipped: Vec<String>,
    pub disclaimer: String,
}

/// 加仓/减仓比例：随信号强度 |z| 放大，clamp 到 [10%, 30%]。
fn size_pct(z_abs: f64) -> f64 {
    (0.10 + 0.10 * z_abs.min(2.0)).clamp(0.10, 0.30)
}

fn round_yuan(x: f64) -> f64 { x.round() }

/// 由形态/信号/持仓推导动作与建议金额。返回 (action, suggest_amount, 动作依据补充)。
fn decide(regime: &str, signal: &str, z: f64, amount: f64, profit: f64) -> (String, f64, String) {
    let amt = amount.max(0.0);
    // 下跌趋势优先观望；仅深跌小额试探（pct 减半）。
    if regime == "下跌趋势" {
        if z <= -1.5 {
            let s = round_yuan(amt * size_pct(z.abs()) * 0.5);
            return ("加仓".into(), s, "下跌趋势但已深跌，仅小额试探".into());
        }
        return ("观望".into(), 0.0, "下跌趋势，暂观望不追加".into());
    }
    if signal.contains("低吸") {
        let s = round_yuan(amt * size_pct(z.abs()));
        return ("加仓".into(), s, "当前处低吸区间，逢低分批加仓".into());
    }
    if signal.contains("高抛") {
        let s = round_yuan(amt * size_pct(z.abs()));
        return if profit > 0.0 {
            ("止盈".into(), s, "当前处高抛区间且持有盈利，部分止盈".into())
        } else {
            ("减仓".into(), s, "当前处高抛区间，适度减仓控制风险".into())
        };
    }
    ("持有".into(), 0.0, "处于持有区间，维持仓位/继续定投".into())
}

/// 对单只持仓产出建议：复用 evaluate_fund 取多策略+择时，再叠加持仓驱动的动作。
/// 数据不足/无择时计划返回 Err，调用方据此跳过。
pub fn advise_holding(
    h: &Holding, name: &str, points: &[NavPoint], p: &RecommendParams,
) -> anyhow::Result<HoldingAdvice> {
    let rec = recommend::evaluate_fund(&h.code, name, points, p)?;
    let plan = rec.regime.plan.as_ref()
        .ok_or_else(|| anyhow::anyhow!("{} 无择时计划（数据不足）", h.code))?;
    let z = plan.current.z;
    let signal = plan.current.signal.clone();
    let (action, mut suggest, mut extra) = decide(&rec.regime.regime, &signal, z, h.amount, h.profit);
    if h.amount <= 0.0 {
        suggest = 0.0;
        extra = "未填持有金额，仅给方向".into();
    }
    let rationale = format!("{extra}。{}", rec.rationale);
    Ok(HoldingAdvice {
        code: h.code.clone(), name: name.to_string(),
        amount: h.amount, profit: h.profit, weight: 0.0,
        action, suggest_amount: suggest, signal, z,
        best_strategy: rec.best_strategy, all_strategies: rec.all_strategies,
        regime: rec.regime, rationale,
    })
}

/// 组装整份持仓报告：注入 `load` 逐只取净值 → advise_holding → 汇总。
pub fn build_report<F>(
    input: &HoldingsInput, names_of: F, today: &str, p: &RecommendParams,
    mut load: impl FnMut(&str) -> anyhow::Result<Vec<NavPoint>>,
) -> HoldingsReport
where
    F: Fn(&str) -> String,
{
    let mut advices = Vec::new();
    let mut skipped = Vec::new();
    for h in &input.holdings {
        if h.code.trim().is_empty() { continue; }
        match load(&h.code) {
            Ok(points) => match advise_holding(h, &names_of(&h.code), &points, p) {
                Ok(a) => advices.push(a),
                Err(_) => skipped.push(h.code.clone()),
            },
            Err(_) => skipped.push(h.code.clone()),
        }
    }

    // 权重按各只持有金额占比（总额优先用输入，否则取各只之和）。
    let sum_amount: f64 = advices.iter().map(|a| a.amount.max(0.0)).sum();
    let total_amount = input.total_amount.unwrap_or(sum_amount);
    let mut max_weight = 0.0_f64;
    if sum_amount > 0.0 {
        for a in &mut advices {
            a.weight = a.amount.max(0.0) / sum_amount;
            if a.weight > max_weight { max_weight = a.weight; }
        }
    }

    let total_add: f64 = advices.iter().filter(|a| a.action == "加仓").map(|a| a.suggest_amount).sum();
    let total_trim: f64 = advices.iter()
        .filter(|a| a.action == "减仓" || a.action == "止盈").map(|a| a.suggest_amount).sum();
    let concentration_note = if max_weight > CONCENTRATION_LIMIT {
        format!("单只权重最高达 {:.0}%，集中度偏高，注意分散", max_weight * 100.0)
    } else { String::new() };

    HoldingsReport {
        generated: today.to_string(),
        summary: PortfolioSummary {
            total_amount,
            total_profit: input.total_profit,
            cumulative_profit: input.cumulative_profit,
            holding_count: advices.len(),
            total_add, total_trim, concentration_note,
        },
        advices,
        skipped,
        disclaimer: DISCLAIMER.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    #[test]
    fn size_pct_scales_and_clamps() {
        assert!((size_pct(0.0) - 0.10).abs() < 1e-9);
        assert!((size_pct(1.0) - 0.20).abs() < 1e-9);
        assert!((size_pct(5.0) - 0.30).abs() < 1e-9, "上限 30%");
    }

    #[test]
    fn decide_low_buy_adds() {
        let (act, s, _) = decide("震荡", "低吸(−1σ)", -1.0, 10000.0, 0.0);
        assert_eq!(act, "加仓");
        assert!(s > 0.0, "加仓额应>0");
    }

    #[test]
    fn decide_high_sell_takes_profit_when_gain() {
        let (act, s, _) = decide("震荡", "高抛(+1σ)", 1.0, 10000.0, 500.0);
        assert_eq!(act, "止盈");
        assert!(s > 0.0);
        let (act2, _, _) = decide("震荡", "高抛(+1σ)", 1.0, 10000.0, -200.0);
        assert_eq!(act2, "减仓", "亏损时高抛为减仓");
    }

    #[test]
    fn decide_hold_zero_amount() {
        let (act, s, _) = decide("震荡", "持有区间", 0.2, 10000.0, 0.0);
        assert_eq!(act, "持有");
        assert_eq!(s, 0.0);
    }

    #[test]
    fn decide_downtrend_watches_unless_deep() {
        let (act, s, _) = decide("下跌趋势", "低吸", -0.5, 10000.0, 0.0);
        assert_eq!(act, "观望");
        assert_eq!(s, 0.0);
        let (act2, s2, _) = decide("下跌趋势", "低吸", -1.8, 10000.0, 0.0);
        assert_eq!(act2, "加仓", "深跌小额试探");
        assert!(s2 > 0.0);
    }

    fn load_ok(_c: &str) -> anyhow::Result<Vec<NavPoint>> {
        // 300 点温和上涨，足够评估与择时
        Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.004).collect::<Vec<_>>()))
    }

    #[test]
    fn build_report_weights_sum_to_one_and_skips_bad() {
        let input = HoldingsInput {
            total_amount: None, total_profit: Some(1200.0), cumulative_profit: Some(3000.0),
            holdings: vec![
                Holding { code: "000001".into(), amount: 6000.0, profit: 800.0 },
                Holding { code: "000002".into(), amount: 4000.0, profit: 400.0 },
                Holding { code: "BADX".into(), amount: 1000.0, profit: 0.0 },
            ],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-02", &RecommendParams::default(),
            |c| if c == "BADX" { Err(anyhow::anyhow!("加载失败")) } else { load_ok(c) });
        assert_eq!(rep.advices.len(), 2, "两只成功");
        assert_eq!(rep.skipped, vec!["BADX".to_string()]);
        let wsum: f64 = rep.advices.iter().map(|a| a.weight).sum();
        assert!((wsum - 1.0).abs() < 1e-9, "权重和应≈1");
        assert_eq!(rep.summary.total_amount, 10000.0, "总额取成功两只之和");
        assert_eq!(rep.summary.total_profit, Some(1200.0));
    }

    #[test]
    fn build_report_flags_concentration() {
        let input = HoldingsInput {
            total_amount: None, total_profit: None, cumulative_profit: None,
            holdings: vec![
                Holding { code: "000001".into(), amount: 9000.0, profit: 0.0 },
                Holding { code: "000002".into(), amount: 1000.0, profit: 0.0 },
            ],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-02", &RecommendParams::default(), load_ok);
        assert!(!rep.summary.concentration_note.is_empty(), "90% 集中应提示");
    }

    #[test]
    fn build_report_empty_is_valid() {
        let input = HoldingsInput {
            total_amount: None, total_profit: None, cumulative_profit: None, holdings: vec![],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-02", &RecommendParams::default(), load_ok);
        assert_eq!(rep.summary.holding_count, 0);
        assert!(rep.advices.is_empty());
    }

    #[test]
    fn report_serializes_frontend_keys() {
        let input = HoldingsInput {
            total_amount: Some(10000.0), total_profit: Some(500.0), cumulative_profit: Some(900.0),
            holdings: vec![Holding { code: "000001".into(), amount: 10000.0, profit: 500.0 }],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-02", &RecommendParams::default(), load_ok);
        let j = serde_json::to_string(&rep).unwrap();
        for key in ["\"summary\"", "\"advices\"", "\"action\"", "\"suggest_amount\"", "\"signal\"",
                    "\"weight\"", "\"best_strategy\"", "\"total_add\"", "\"total_trim\"",
                    "\"concentration_note\"", "\"disclaimer\"", "\"skipped\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }
}

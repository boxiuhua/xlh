//! 持仓概览：录入持仓 → 逐只评估 → 组合层面的风险提示。
//!
//! ## 这里曾经给出择时驱动的下单金额，现在不给了
//!
//! 旧实现的「加仓 1200 元 / 止盈 800 元」金额 = `持仓金额 × size_pct(|z|)`，
//! 而这个 `z` 直接取自诊断 tab 的 ±σ 波动带（低吸线/高抛线）。
//!
//! 我们给那条线做了无未来函数的前瞻检验（`analyze::evaluate_triggers`：滚动重算波动带、
//! 统计每次触发后 20 日收益、与"随便哪天买入持有 20 日"的无条件基准对比）。
//! 实测 8 年历史、五只基金**无一例外**：
//!
//! ```text
//!                低吸触发   低吸后20日    基准      超额
//!   招商白酒        50      -0.67%    +0.55%   -1.23%
//!   易方达消费      58      -0.42%    +0.63%   -1.04%
//!   天弘沪深300     63      +0.07%    +0.77%   -0.69%
//!   易方达优质精选  72      -0.06%    +0.63%   -0.68%
//!   银河创新        59      +0.56%    +2.67%   -2.11%
//! ```
//!
//! 低吸线**不但没有价值，还比"随便哪天买"更差** —— 净值跌破 MA−1σ 往往意味着趋势正在
//! 向下，"低吸"实为在下跌趋势里接刀子；权益基金是动量的，不是均值回归的。
//! 高抛线同样：其后平均仍在上涨，照它减仓会错过后续涨幅。
//!
//! 一个已被自身数据证伪的信号，不该继续驱动具体的下单金额。所以：
//!   - **择时金额一律不给**（`suggest_amount` 为 `None`），并如实告知该基金的历史超额；
//!   - **只保留集中度减仓的金额** —— 那是纯风险规则（单只超 40% 就减到 40%），
//!     不预测收益、不依赖任何择时信号，站得住。
//!
//! 纯逻辑，净值 IO 由调用方注入（同 recommend.rs 风格）。
use serde::{Deserialize, Serialize};

use crate::analyze::RegimeReport;
use crate::data::NavPoint;
use crate::recommend::{self, RecommendParams, StrategyEval, DISCLAIMER};

/// 集中度上限：单只权重超过此值即建议减到该线。
const CONCENTRATION_LIMIT: f64 = 0.40;

/// 低吸线超额低于此值时不给任何择时金额。取 0 —— 跑不赢"随便哪天买"就是没有价值。
const MIN_USEFUL_EDGE: f64 = 0.0;
/// 触发次数低于此值时，超额估计本身就是噪声，同样不给金额。
const MIN_SIGNALS: usize = 10;

/// 单只持仓输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Holding {
    pub code: String,
    #[serde(default)]
    pub amount: f64,
    #[serde(default)]
    pub profit: f64,
}

/// 组合输入：顶部三项选填，缺失时由各只推导/留空。
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// 逐只情况。
///
/// 注意 `action` 是**描述性状态**，不是操作指令；`suggest_amount` 只有在有
/// 非择时依据（目前仅集中度超限）时才有值。
#[derive(Debug, Clone, Serialize)]
pub struct HoldingAdvice {
    pub code: String,
    pub name: String,
    pub amount: f64,
    pub profit: f64,
    pub weight: f64,
    /// 描述性状态：持有 / 减仓(集中度) / 数据不足。**不是择时指令。**
    pub action: String,
    /// 建议金额。`None` = 不给金额。
    ///
    /// 择时信号（低吸/高抛线）经前瞻检验为负超额，**不再据此给出任何金额**。
    /// 唯一会有值的情形：单只权重超过集中度上限，建议减到该线 —— 那是风险规则，不是择时。
    pub suggest_amount: Option<f64>,
    /// 波动带信号标签（保留为描述，如"低吸"），仅供参考，不驱动金额。
    pub signal: String,
    pub z: f64,
    /// 该基金低吸线相对「随便哪天买」的历史超额 %。`None` = 样本不足，无法检验。
    pub timing_edge: Option<f64>,
    /// 为什么不据此给金额。
    pub timing_note: String,
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
    /// 集中度超限建议减仓的总额（唯一还会给出的金额）
    pub total_trim: f64,
    pub concentration_note: String,
    /// 为什么不再给择时加仓/减仓金额 —— 必须让用户看到，否则会以为是功能坏了
    pub timing_disclosure: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoldingsReport {
    pub generated: String,
    pub summary: PortfolioSummary,
    pub advices: Vec<HoldingAdvice>,
    pub skipped: Vec<String>,
    pub disclaimer: String,
}

/// 金额四舍五入到整数元。
pub fn round_yuan(x: f64) -> f64 { x.round() }

/// 该基金的低吸线相对「随便哪天买」的历史超额，以及能否据此下单。
///
/// 返回 (超额%, 说明)。超额为 `None` 表示样本不足、无法检验。
fn timing_verdict(regime: &RegimeReport) -> (Option<f64>, String) {
    let Some(e) = regime.plan.as_ref().and_then(|p| p.evidence.as_ref()) else {
        return (None, "历史数据不足，无法检验低吸/高抛线是否有效 —— 不据此给出金额。".into());
    };
    let (Some(buy), Some(base)) = (e.buy_mean_forward, e.baseline_mean_forward) else {
        return (None, "低吸线在历史上从未触发，无从检验 —— 不据此给出金额。".into());
    };
    if e.buy_signals < MIN_SIGNALS {
        return (None, format!(
            "低吸线历史仅触发 {} 次，样本不足以判断有效性 —— 不据此给出金额。", e.buy_signals));
    }
    let edge = buy - base;
    let note = if edge <= MIN_USEFUL_EDGE {
        format!(
            "低吸线触发 {} 次，其后 {} 日平均 {buy:+.2}%，而「随便哪天买」是 {base:+.2}% \
             —— 超额 {edge:+.2}%，**没跑赢随便买**。这条线不提供择时价值，故不据此给出加仓/减仓金额。",
            e.buy_signals, e.horizon_days)
    } else {
        format!(
            "低吸线触发 {} 次，超额 {edge:+.2}%。但这是单只基金的样本内统计、未经样本外检验，\
             仍不足以支撑具体下单金额。",
            e.buy_signals)
    };
    (Some(edge), note)
}

/// 对单只持仓产出情况说明。
///
/// **不再由择时信号推导下单金额** —— 见模块文档。这里只如实呈现：持仓、形态、
/// 波动带信号（作为描述）、以及那条线经检验到底有没有用。
/// 金额留给 `build_report` 按集中度这一条非择时规则来给。
pub fn advise_holding(
    h: &Holding, name: &str, points: &[NavPoint], p: &RecommendParams,
) -> anyhow::Result<HoldingAdvice> {
    let rec = recommend::evaluate_fund(&h.code, name, points, p)?;
    let plan = rec.regime.plan.as_ref()
        .ok_or_else(|| anyhow::anyhow!("{} 无择时计划（数据不足）", h.code))?;
    let z = plan.current.z;
    let signal = plan.current.signal.clone();
    let (timing_edge, timing_note) = timing_verdict(&rec.regime);

    Ok(HoldingAdvice {
        code: h.code.clone(), name: name.to_string(),
        amount: h.amount, profit: h.profit, weight: 0.0,
        action: "持有".into(),          // 默认状态；集中度超限时由 build_report 改写
        suggest_amount: None,           // 择时不给金额
        signal, z,
        timing_edge, timing_note,
        best_strategy: rec.best_strategy, all_strategies: rec.all_strategies,
        regime: rec.regime,
        rationale: rec.rationale,
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

    // 唯一还会给出金额的规则：单只权重超过上限 → 建议减到上限。
    // 这是纯风险规则（控制单一标的的暴露），**不预测收益**，也不依赖任何择时信号。
    let mut total_trim = 0.0;
    if sum_amount > 0.0 {
        for a in &mut advices {
            if a.weight > CONCENTRATION_LIMIT {
                let target = sum_amount * CONCENTRATION_LIMIT;
                let trim = round_yuan(a.amount.max(0.0) - target);
                if trim > 0.0 {
                    a.action = "减仓(集中度)".into();
                    a.suggest_amount = Some(trim);
                    total_trim += trim;
                }
            }
        }
    }

    let concentration_note = if max_weight > CONCENTRATION_LIMIT {
        format!("单只权重最高达 {:.0}%，超过 {:.0}% 上限 —— 建议减到该线以控制单一标的暴露。\
                 这是风险规则，与看涨看跌无关。",
                max_weight * 100.0, CONCENTRATION_LIMIT * 100.0)
    } else { String::new() };

    HoldingsReport {
        generated: today.to_string(),
        summary: PortfolioSummary {
            total_amount,
            total_profit: input.total_profit,
            cumulative_profit: input.cumulative_profit,
            holding_count: advices.len(),
            total_trim,
            concentration_note,
            timing_disclosure: TIMING_DISCLOSURE.to_string(),
        },
        advices,
        skipped,
        disclaimer: DISCLAIMER.to_string(),
    }
}

/// 为什么不再给择时金额。必须出现在报告里 —— 否则用户会以为功能坏了。
pub const TIMING_DISCLOSURE: &str =
    "本页不再给出「加仓 X 元 / 止盈 Y 元」这类择时金额。原因：这些金额此前完全由诊断页的 \
     ±σ 波动带（低吸线/高抛线）驱动，而我们对该信号做了无未来函数的前瞻检验 —— \
     在所测的每一只基金上，低吸线其后 20 日的平均收益都**低于「随便哪天买」的基准**\
     （超额 −0.7% ~ −2.1%）。一个跑不赢随机买入的信号，不该继续驱动具体的下单金额。\
     下方仍会给出的唯一金额是「集中度减仓」—— 那是风险规则，不预测涨跌。";

/// 历史列表摘要：只数 + 集中度减仓总额。
pub fn summarize(report: &HoldingsReport) -> String {
    format!(
        "{} 只 · 集中度减仓 {:.0}",
        report.summary.holding_count,
        report.summary.total_trim,
    )
}

#[cfg(test)]
mod history_summary_tests {
    use super::*;

    #[test]
    fn summarize_uses_counts_and_totals() {
        let report = HoldingsReport {
            generated: "2026-07-05".into(),
            summary: PortfolioSummary {
                total_amount: 100000.0,
                total_profit: None,
                cumulative_profit: None,
                holding_count: 3,
                total_trim: 800.0,
                concentration_note: String::new(),
                timing_disclosure: String::new(),
            },
            advices: vec![],
            skipped: vec![],
            disclaimer: String::new(),
        };
        // 摘要不再有"加仓 X"—— 择时金额已移除
        assert_eq!(summarize(&report), "3 只 · 集中度减仓 800");
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

    /// 核心不变量：**择时信号绝不产生下单金额**。
    ///
    /// 旧实现里「低吸 → 加仓 amount×size_pct(|z|) 元」。那条低吸线经前瞻检验在每一只
    /// 测试基金上都跑不赢「随便哪天买」（超额 −0.7% ~ −2.1%）。谁要是把这段逻辑加回来，
    /// 这个测试必须炸。
    #[test]
    fn timing_signal_never_produces_a_trade_amount() {
        // 三只均分 → 各 33%，都未超 40% 集中度上限，故不该有任何金额
        let input = HoldingsInput {
            total_amount: None, total_profit: None, cumulative_profit: None,
            holdings: vec![
                Holding { code: "000001".into(), amount: 5000.0, profit: 300.0 },
                Holding { code: "000002".into(), amount: 5000.0, profit: -200.0 },
                Holding { code: "000003".into(), amount: 5000.0, profit: 0.0 },
            ],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-12",
                               &RecommendParams::default(), load_ok);
        assert_eq!(rep.advices.len(), 3);
        for a in &rep.advices {
            // 权重各 50%，未超集中度上限 → 不该有任何金额
            assert_eq!(a.suggest_amount, None,
                       "{} 未超集中度，却给出了金额 —— 择时不得驱动金额", a.code);
            assert_eq!(a.action, "持有");
            assert!(!a.action.contains("加仓"), "不得再出现「加仓」这类择时指令");
            assert!(!a.action.contains("止盈"));
        }
        assert_eq!(rep.summary.total_trim, 0.0);
    }

    /// 唯一还会给金额的规则：集中度超限 → 减到上限。这是风险规则，不预测涨跌。
    #[test]
    fn only_concentration_produces_an_amount() {
        let input = HoldingsInput {
            total_amount: None, total_profit: None, cumulative_profit: None,
            holdings: vec![
                Holding { code: "000001".into(), amount: 9000.0, profit: 0.0 },  // 90%
                Holding { code: "000002".into(), amount: 1000.0, profit: 0.0 },  // 10%
            ],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-12",
                               &RecommendParams::default(), load_ok);

        let big = rep.advices.iter().find(|a| a.code == "000001").unwrap();
        let small = rep.advices.iter().find(|a| a.code == "000002").unwrap();

        // 9000 → 减到 40% × 10000 = 4000，即减 5000
        assert_eq!(big.action, "减仓(集中度)");
        assert_eq!(big.suggest_amount, Some(5000.0));
        assert_eq!(rep.summary.total_trim, 5000.0);

        // 未超限的那只不给金额
        assert_eq!(small.suggest_amount, None);
        assert_eq!(small.action, "持有");

        // 措辞必须说明这是风险规则，而不是看空
        assert!(rep.summary.concentration_note.contains("风险规则"));
    }

    /// 必须告诉用户"为什么没有加仓金额了"，否则会被当成功能坏了。
    #[test]
    fn discloses_why_timing_amounts_are_gone() {
        let input = HoldingsInput {
            total_amount: None, total_profit: None, cumulative_profit: None,
            holdings: vec![Holding { code: "000001".into(), amount: 5000.0, profit: 0.0 }],
        };
        let rep = build_report(&input, |c| c.to_string(), "2026-07-12",
                               &RecommendParams::default(), load_ok);
        let d = &rep.summary.timing_disclosure;
        assert!(d.contains("不再给出"), "须明说不再给择时金额");
        assert!(d.contains("随便哪天买"), "须说明是跑不赢随机买入");
        assert!(d.contains("集中度"), "须说明哪种金额还保留");
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
                    "\"weight\"", "\"best_strategy\"", "\"total_trim\"", "\"timing_edge\"",
                    "\"timing_note\"", "\"timing_disclosure\"",
                    "\"concentration_note\"", "\"disclaimer\"", "\"skipped\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
        // 择时加仓总额已彻底移除，字段不该再存在
        assert!(!j.contains("\"total_add\""), "total_add 已移除（择时不再驱动金额）");
    }
}

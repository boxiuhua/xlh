//! 把基金/股票的持仓建议 + 诊断 + 同步简报组装成 Markdown（纯函数，可测）。
use crate::analyze::RegimeReport;
use crate::holdings::HoldingsReport;
use crate::stock::diagnose::StockDiagnosis;

use super::stock_advice::StockAdvice;

/// 归一后的同步行（基金/股票 SyncOutcome 各自映射进来）。
#[derive(Debug, Clone)]
pub struct SyncNote {
    pub code: String,
    pub added: usize,
    pub latest: Option<String>,
    pub error: Option<String>,
}

fn fmt0(x: f64) -> String { format!("{:.0}", x) }

fn timing_line(r: &RegimeReport) -> String {
    match &r.plan {
        Some(p) => format!(" · 低吸线 {:.4} · 高抛线 {:.4}", p.buy, p.sell),
        None => String::new(),
    }
}

/// 组装完整推送消息。
pub fn compose(
    fund: &HoldingsReport,
    fund_diags: &[(String, String, RegimeReport)],
    stock_adv: &[StockAdvice],
    stock_diags: &[StockDiagnosis],
    sync: &[SyncNote],
) -> String {
    let mut s = String::new();

    // 基金持仓建议
    s.push_str("## 基金持仓建议\n");
    let sm = &fund.summary;
    s.push_str(&format!("**组合汇总**：总持仓 {} 元 · 持仓 {} 只", fmt0(sm.total_amount), sm.holding_count));
    if let Some(p) = sm.total_profit { s.push_str(&format!(" · 持有收益 {} 元", fmt0(p))); }
    if let Some(p) = sm.cumulative_profit { s.push_str(&format!(" · 累计收益 {} 元", fmt0(p))); }
    s.push('\n');
    s.push_str(&format!("合计建议：加仓 {} 元 · 减仓/止盈 {} 元\n", fmt0(sm.total_add), fmt0(sm.total_trim)));
    if !sm.concentration_note.is_empty() { s.push_str(&format!("> {}\n", sm.concentration_note)); }
    s.push('\n');
    for a in &fund.advices {
        let amt = if a.suggest_amount > 0.0 { format!(" {} 元", fmt0(a.suggest_amount)) } else { String::new() };
        s.push_str(&format!("**{} {}** — **{}{}**\n", a.name, a.code, a.action, amt));
        s.push_str(&format!("- 持仓 {} 元 · 收益 {} 元 · 权重 {:.1}%\n", fmt0(a.amount), fmt0(a.profit), a.weight * 100.0));
        s.push_str(&format!("- 择时：{} · 形态 {}{}\n", a.signal, a.regime.regime, timing_line(&a.regime)));
        let b = &a.best_strategy;
        s.push_str(&format!("- 最优策略 {}：样本外 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%\n\n",
            b.name, b.oos_return * 100.0, b.oos_sharpe, b.oos_mdd * 100.0));
    }
    if fund.advices.is_empty() {
        s.push_str("_无可分析基金持仓（数据不足或加载失败）_\n\n");
    }

    // 股票持仓建议
    if !stock_adv.is_empty() {
        s.push_str("## 股票持仓建议\n");
        for a in stock_adv {
            let amt = if a.suggest_amount > 0.0 { format!(" {} 元", fmt0(a.suggest_amount)) } else { String::new() };
            s.push_str(&format!("**{} {}** — **{}{}**\n", a.name, a.code, a.action, amt));
            s.push_str(&format!("- 持仓 {} 元 · 收益 {} 元\n", fmt0(a.amount), fmt0(a.profit)));
            s.push_str(&format!("- 技术面：{} · 形态 {} · 价 {:.3} · RSI {:.1} · 布林z {:.2}\n\n",
                a.signal, a.trend, a.price, a.rsi, a.z));
        }
    }

    // 基金诊断
    if !fund_diags.is_empty() {
        s.push_str("## 基金诊断\n");
        for (code, name, r) in fund_diags {
            s.push_str(&format!("**{} {}** — 形态 {}{}\n", name, code, r.regime, timing_line(r)));
            if let Some(pl) = &r.plan {
                s.push_str(&format!("- 当下：{}（{}）· {}\n", pl.current.signal, pl.current.action, pl.current.next_hint));
            }
            s.push_str(&format!("- {}\n\n", r.rationale));
        }
    }

    // 股票诊断
    if !stock_diags.is_empty() {
        s.push_str("## 股票诊断\n");
        for d in stock_diags {
            s.push_str(&format!("**{} {}** — 形态 {} · {}\n", d.name, d.code, d.trend, d.signal));
            s.push_str(&format!("- 价 {:.3} · RSI {:.1} · 布林z {:.2}\n", d.price, d.rsi, d.boll_z));
            s.push_str(&format!("- {}\n\n", d.rationale));
        }
    }

    // 数据同步简报
    if !sync.is_empty() {
        s.push_str("## 数据同步\n");
        for o in sync {
            match &o.error {
                Some(e) => s.push_str(&format!("- {} 同步失败：{}\n", o.code, e)),
                None => s.push_str(&format!("- {} +{} 条 · 最新 {}\n", o.code, o.added, o.latest.clone().unwrap_or_else(|| "-".into()))),
            }
        }
        s.push('\n');
    }

    s.push_str(&format!("_{}_\n", fund.disclaimer));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::data::NavPoint;
    use crate::holdings::{self, Holding, HoldingsInput};
    use crate::recommend::RecommendParams;
    use crate::stock::diagnose::StockDiagnosis;
    use crate::push::stock_advice;

    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    fn sample_report() -> HoldingsReport {
        let input = HoldingsInput {
            total_amount: Some(10000.0), total_profit: Some(500.0), cumulative_profit: Some(900.0),
            holdings: vec![Holding { code: "000001".into(), amount: 10000.0, profit: 500.0 }],
        };
        holdings::build_report(&input, |c| format!("基金{c}"), "2026-07-02", &RecommendParams::default(),
            |_c| Ok(series(&(0..300).map(|i| 1.0 + i as f64 * 0.004).collect::<Vec<_>>())))
    }

    fn stock_diag() -> StockDiagnosis {
        StockDiagnosis {
            code: "600519".into(), name: "贵州茅台".into(),
            trend: "震荡".into(), signal: "买入(超卖)".into(), boll_z: -1.0,
            price: 1500.0, rsi: 28.0, rationale: "布林下轨低吸".into(),
            ..Default::default()
        }
    }

    #[test]
    fn compose_has_core_and_stock_sections() {
        let rep = sample_report();
        let adv = vec![stock_advice::advise(
            &Holding { code: "600519".into(), amount: 20000.0, profit: 1500.0 }, &stock_diag())];
        let sync = vec![SyncNote { code: "000001".into(), added: 3, latest: Some("2026-07-01".into()), error: None }];
        let md = compose(&rep, &[], &adv, &[stock_diag()], &sync);
        assert!(md.contains("## 基金持仓建议"));
        assert!(md.contains("## 股票持仓建议"));
        assert!(md.contains("贵州茅台"));
        assert!(md.contains("## 股票诊断"));
        assert!(md.contains("## 数据同步"));
        assert!(md.contains("+3 条"));
        assert!(md.contains(&rep.disclaimer));
    }

    #[test]
    fn compose_reports_sync_failure() {
        let sync = vec![SyncNote { code: "BADX".into(), added: 0, latest: None, error: Some("抓取失败".into()) }];
        let md = compose(&sample_report(), &[], &[], &[], &sync);
        assert!(md.contains("BADX 同步失败：抓取失败"));
    }

    #[test]
    fn compose_omits_optional_sections_when_empty() {
        let md = compose(&sample_report(), &[], &[], &[], &[]);
        assert!(!md.contains("## 基金诊断"));
        assert!(!md.contains("## 股票持仓建议"));
        assert!(!md.contains("## 股票诊断"));
    }
}

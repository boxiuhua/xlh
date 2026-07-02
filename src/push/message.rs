//! 把持仓建议 + 额外诊断 + 同步简报组装成 Markdown（纯函数，可测）。
use crate::analyze::RegimeReport;
use crate::data::sync::SyncOutcome;
use crate::holdings::HoldingsReport;

fn fmt0(x: f64) -> String { format!("{:.0}", x) }

fn timing_line(r: &RegimeReport) -> String {
    match &r.plan {
        Some(p) => format!(" · 低吸线 {:.4} · 高抛线 {:.4}", p.buy, p.sell),
        None => String::new(),
    }
}

/// diags: (code, name, 诊断报告)。sync: 各基金同步结果。
pub fn compose(
    report: &HoldingsReport,
    diags: &[(String, String, RegimeReport)],
    sync: &[SyncOutcome],
) -> String {
    let mut s = String::new();
    s.push_str("## 基金持仓建议\n");
    let sm = &report.summary;
    s.push_str(&format!("**组合汇总**：总持仓 {} 元 · 持仓 {} 只", fmt0(sm.total_amount), sm.holding_count));
    if let Some(p) = sm.total_profit { s.push_str(&format!(" · 持有收益 {} 元", fmt0(p))); }
    if let Some(p) = sm.cumulative_profit { s.push_str(&format!(" · 累计收益 {} 元", fmt0(p))); }
    s.push('\n');
    s.push_str(&format!("合计建议：加仓 {} 元 · 减仓/止盈 {} 元\n", fmt0(sm.total_add), fmt0(sm.total_trim)));
    if !sm.concentration_note.is_empty() { s.push_str(&format!("> {}\n", sm.concentration_note)); }
    s.push('\n');

    for a in &report.advices {
        let amt = if a.suggest_amount > 0.0 { format!(" {} 元", fmt0(a.suggest_amount)) } else { String::new() };
        s.push_str(&format!("**{} {}** — **{}{}**\n", a.name, a.code, a.action, amt));
        s.push_str(&format!("- 持仓 {} 元 · 收益 {} 元 · 权重 {:.1}%\n", fmt0(a.amount), fmt0(a.profit), a.weight * 100.0));
        s.push_str(&format!("- 择时：{} · 形态 {}{}\n", a.signal, a.regime.regime, timing_line(&a.regime)));
        let b = &a.best_strategy;
        s.push_str(&format!("- 最优策略 {}：样本外 收益 {:.1}% · 夏普 {:.2} · 回撤 {:.1}%\n\n",
            b.name, b.oos_return * 100.0, b.oos_sharpe, b.oos_mdd * 100.0));
    }
    if report.advices.is_empty() {
        s.push_str("_无可分析持仓（数据不足或加载失败）_\n\n");
    }

    if !diags.is_empty() {
        s.push_str("## 基金诊断\n");
        for (code, name, r) in diags {
            s.push_str(&format!("**{} {}** — 形态 {}{}\n", name, code, r.regime, timing_line(r)));
            if let Some(pl) = &r.plan {
                s.push_str(&format!("- 当下：{}（{}）· {}\n", pl.current.signal, pl.current.action, pl.current.next_hint));
            }
            s.push_str(&format!("- {}\n\n", r.rationale));
        }
    }

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

    s.push_str(&format!("_{}_\n", report.disclaimer));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::data::NavPoint;
    use crate::holdings::{self, Holding, HoldingsInput};
    use crate::recommend::RecommendParams;

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

    #[test]
    fn compose_has_core_sections() {
        let rep = sample_report();
        let sync = vec![SyncOutcome { code: "000001".into(), added: 3, total: 300, latest: Some("2026-07-01".into()), error: None }];
        let md = compose(&rep, &[], &sync);
        assert!(md.contains("## 基金持仓建议"));
        assert!(md.contains("组合汇总"));
        assert!(md.contains("合计建议"));
        assert!(md.contains("## 数据同步"));
        assert!(md.contains("+3 条"));
        assert!(md.contains(&rep.disclaimer));
    }

    #[test]
    fn compose_reports_sync_failure() {
        let rep = sample_report();
        let sync = vec![SyncOutcome { code: "BADX".into(), added: 0, total: 0, latest: None, error: Some("抓取失败".into()) }];
        let md = compose(&rep, &[], &sync);
        assert!(md.contains("BADX 同步失败：抓取失败"));
    }

    #[test]
    fn compose_omits_diag_section_when_empty() {
        let md = compose(&sample_report(), &[], &[]);
        assert!(!md.contains("## 基金诊断"), "无额外诊断则不出该区块");
    }
}

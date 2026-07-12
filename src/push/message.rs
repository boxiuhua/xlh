//! 把基金/股票的持仓建议 + 诊断 + 同步简报组装成 Markdown（纯函数，可测）。
use crate::analyze::RegimeReport;
use crate::holdings::HoldingsReport;
use crate::stock::diagnose::StockDiagnosis;
use crate::stock::screen::ScreenReport;

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

/// 低吸线相对「随便哪天买」的超额。推送里给出精确点位和具体金额，
/// 就必须同时给出这条线到底有没有用 —— 否则是在诱导人照着一个没有证据的数字下单。
fn evidence_line(r: &RegimeReport) -> String {
    let Some(p) = &r.plan else { return String::new() };
    let Some(e) = &p.evidence else {
        return "\n- ⚠ 历史数据不足，无法检验低吸/高抛线是否有效 —— 上面的点位没有证据支持\n".into();
    };
    let (Some(buy), Some(base)) = (e.buy_mean_forward, e.baseline_mean_forward) else {
        return String::new();
    };
    if e.buy_signals < 10 {
        return format!("\n- ⚠ 低吸线历史仅触发 {} 次，样本不足以判断它是否有效\n", e.buy_signals);
    }
    let edge = buy - base;
    let tail = if edge <= 0.0 {
        "**没跑赢「随便哪天买」，这条线不提供择时价值**"
    } else {
        "（单只基金的样本内统计，未经样本外检验）"
    };
    format!(
        "\n- 线的有效性（{} 日前瞻）：低吸触发 {} 次，其后平均 {buy:+.2}%；基准（随便哪天买）{base:+.2}% \
         → 超额 **{edge:+.2}%** {tail}\n",
        e.horizon_days, e.buy_signals)
}

/// 组装完整推送消息。
pub fn compose(
    fund: &HoldingsReport,
    fund_diags: &[(String, String, RegimeReport)],
    stock_adv: &[StockAdvice],
    stock_diags: &[StockDiagnosis],
    screen: Option<&ScreenReport>,
    sync: &[SyncNote],
) -> String {
    let mut s = String::new();

    // 基金持仓概览
    s.push_str("## 基金持仓概览\n");
    let sm = &fund.summary;
    s.push_str(&format!("**组合汇总**：总持仓 {} 元 · 持仓 {} 只", fmt0(sm.total_amount), sm.holding_count));
    if let Some(p) = sm.total_profit { s.push_str(&format!(" · 持有收益 {} 元", fmt0(p))); }
    if let Some(p) = sm.cumulative_profit { s.push_str(&format!(" · 累计收益 {} 元", fmt0(p))); }
    s.push('\n');
    if sm.total_trim > 0.0 {
        s.push_str(&format!("集中度减仓合计：{} 元（风险规则，非择时）\n", fmt0(sm.total_trim)));
    }
    if !sm.concentration_note.is_empty() { s.push_str(&format!("> {}\n", sm.concentration_note)); }
    // 为什么不再给择时金额 —— 不说清楚，用户会以为功能坏了
    s.push_str(&format!("> {}\n", sm.timing_disclosure));
    s.push('\n');
    for a in &fund.advices {
        let amt = match a.suggest_amount {
            Some(v) if v > 0.0 => format!(" {} 元", fmt0(v)),
            _ => String::new(),
        };
        s.push_str(&format!("**{} {}** — **{}{}**\n", a.name, a.code, a.action, amt));
        s.push_str(&format!("- 持仓 {} 元 · 收益 {} 元 · 权重 {:.1}%\n", fmt0(a.amount), fmt0(a.profit), a.weight * 100.0));
        s.push_str(&format!("- 形态 {}｜波动带信号 {}（仅描述）{}\n", a.regime.regime, a.signal, timing_line(&a.regime)));
        s.push_str(&format!("- {}\n", a.timing_note));
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
            // 给了点位和金额，就必须同时给出这条线到底有没有用
            s.push_str(&evidence_line(r));
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

    // 质量筛选
    //
    // 这一节的措辞是刻意的。一份「优质股清单」出现在推送里，天然会被读成「翻倍名单」——
    // 所以标题不叫「选股推荐」，正文不给分数、不给买卖信号，且**必须**附上历史基础发生率。
    // 少了基础发生率，这一节就是在诱导人；`screen::BaseRate` 的存在就是为了堵这个口子。
    if let Some(sc) = screen {
        s.push_str("## 质量筛选（排除法，非推荐）\n");
        s.push_str(&format!("交易日 {} · 池 {} 只 · 通过 {} 只\n\n",
                            sc.trade_date, sc.pool_size, sc.passed));

        for p in &sc.top {
            s.push_str(&format!("**{} {}**\n", p.name, p.code));
            let roe = p.roe_median.map(|v| format!("{v:.1}%")).unwrap_or_else(|| "-".into());
            s.push_str(&format!("- {} 年年报 · ROE中位数 {} · ROE连续≥15% {} 年\n",
                                p.years, roe, p.roe_streak));
            if let Some(g) = p.profit_cagr {
                s.push_str(&format!("- 净利 5 年 CAGR {g:.1}%"));
                if let Some(r) = p.revenue_cagr { s.push_str(&format!(" · 营收 {r:.1}%")); }
                s.push('\n');
            }
            if let Some(pe) = p.pe_ttm {
                let pct = p.pe_percentile
                    .map(|v| format!("（自身历史 {:.0}% 分位）", v * 100.0))
                    .unwrap_or_default();
                s.push_str(&format!("- PE(TTM) {pe:.1}{pct}\n"));
            }
            s.push('\n');
        }
        if sc.top.is_empty() {
            s.push_str("_本轮无标的通过筛选_\n\n");
        }

        // 被排除了什么，和筛出了什么一样重要 —— 否则无从判断这份清单是否可信
        if !sc.excluded.is_empty() {
            s.push_str("**排除明细**\n");
            for (reason, n) in &sc.excluded {
                s.push_str(&format!("- {n} 只：{reason}\n"));
            }
            s.push('\n');
        }

        s.push_str(&format!("> **{}**\n", sc.base_rate.headline));
        for f in &sc.base_rate.facts {
            s.push_str(&format!("> - {f}\n"));
        }
        s.push('\n');
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
        let md = compose(&rep, &[], &adv, &[stock_diag()], None, &sync);
        assert!(md.contains("## 基金持仓概览"));
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
        let md = compose(&sample_report(), &[], &[], &[], None, &sync);
        assert!(md.contains("BADX 同步失败：抓取失败"));
    }

    fn sample_screen() -> ScreenReport {
        use crate::stock::screen::{BaseRate, Profile, ScreenParams};
        ScreenReport {
            generated: "2026-07-12".into(),
            trade_date: "2026-07-10".into(),
            pool_size: 5,
            passed: 1,
            excluded: vec![("近四季度归母净利为负：该组历史平均最大回撤近38%".into(), 1)],
            top: vec![Profile {
                code: "600519".into(), name: "贵州茅台".into(),
                years: 26, roe_median: Some(32.2), roe_streak: 23,
                revenue_cagr: Some(11.0), profit_cagr: Some(12.0),
                gross_margin: Some(91.2), market_cap: 1.5e12,
                pe_ttm: Some(18.21), pe_percentile: Some(0.004),
                note: "以上均为历史事实，不含对未来的预测".into(),
            }],
            base_rate: BaseRate::default(),
            params: ScreenParams::default(),
            disclaimer: crate::stock::screen::DISCLAIMER.into(),
        }
    }

    /// 一份「优质股清单」出现在推送里，天然会被读成「翻倍名单」。
    /// 基础发生率是唯一的对冲 —— 哪天有人把它删了，这个测试必须炸。
    #[test]
    fn screen_section_must_carry_base_rates_and_never_read_as_a_buy_list() {
        let sc = sample_screen();
        let md = compose(&sample_report(), &[], &[], &[], Some(&sc), &[]);

        assert!(md.contains("## 质量筛选（排除法，非推荐）"), "标题须自我否定「推荐」含义");
        assert!(md.contains("贵州茅台"));
        assert!(md.contains("ROE连续≥15% 23 年"));
        assert!(md.contains("自身历史 0% 分位"));

        // 排除明细必须可见 —— 否则读者无从判断这份清单可不可信
        assert!(md.contains("**排除明细**"));
        assert!(md.contains("1 只：近四季度归母净利为负"));

        // 基础发生率：缺了它这一节就是在诱导人
        assert!(md.contains("4.9%"), "须给出十倍股基础发生率");
        assert!(md.contains("72%"), "须说明多数十倍股把涨幅还了回去");
        assert!(md.contains("Bessembinder"), "须给出个股回报分布的硬先验");
        assert!(md.contains("不是可稳定复制的策略"));

        // 绝不能出现买卖信号式措辞
        for banned in ["推荐买入", "目标价", "强烈看好"] {
            assert!(!md.contains(banned), "推送不得出现 `{banned}`");
        }
    }

    #[test]
    fn compose_omits_optional_sections_when_empty() {
        let md = compose(&sample_report(), &[], &[], &[], None, &[]);
        assert!(!md.contains("## 基金诊断"));
        assert!(!md.contains("## 股票持仓建议"));
        assert!(!md.contains("## 股票诊断"));
        assert!(!md.contains("## 质量筛选"), "未配置筛选时不该出现该章节");
    }
}

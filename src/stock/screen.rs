//! 质量下限筛选 + 期望校准。
//!
//! ## 这个模块**不是**百倍股筛选器，这一点是刻意的
//!
//! 我们做过一轮实证检索（103 个 agent、三票对抗式验证）。结论对本模块的设计是决定性的：
//!
//! **一、「百倍股事前特征」这类量化阈值没有通过验证。** 券商研究里那组最诱人的数字
//! （十倍股归母净利 CAGR 中位数 23.1%、毛利率 30.0%、上市日市值中位数 17 亿、
//! PE(TTM) 中位数 46.6 倍）在对抗式验证中被 0-3 否决，不可作为筛选阈值。
//! 根因是方法论死结：这些统计是在**幸存者集合**上做的事后描述，样本选择方式决定了
//! 它不可能有事前预测力。所以本模块**不设「买入」阈值、不给「推荐」评分**。
//!
//! **二、基础发生率极低，而且赢家会把涨幅还回去。** 2000–2020 年 A 股中曾涨超 10 倍的有
//! 637 只（17.7%），但到 2020/6/30 仍保持 10 倍的只剩 176 只（4.9%）—— **约 72% 的历史
//! 十倍股把涨幅还了回去**。达成十倍平均耗时 100 个月（约 8 年，年化 33.4%）。
//! Bessembinder (JFE 2018) 在美股 1926–2016 全样本上给出更硬的先验：仅 42.6% 的个股终身
//! 回报跑赢国债，**众数结局是亏损 100%**，全市场净财富 100% 由最好的 4% 公司贡献；
//! 随机单股集中持有在 96% 的 bootstrap 路径上跑输大盘。
//!
//! **三、但有一条证据活下来了，它指向「排除」而非「选中」**：同一份数据里，有 5 家以上
//! 机构评级的十倍股平均最大回撤 < 25%，而**近四季度归母净利为负的 37 只平均回撤近 38%**。
//! 也就是说：基本面筛选挡不住你错过茅台，但能挡住你踩中惠城环保
//! （2022–2025 复权最大涨幅 32 倍，随后回撤 73.9%）。
//!
//! 因此本模块只做两件有证据支持的事：
//!   1. **排除**（`Exclusion`）—— 把已知会显著恶化尾部风险的标的挡在池外；
//!   2. **期望校准**（`BaseRate`）—— 对留下来的标的，明说「历史上这条路要走多久、
//!      多大概率走不通」，而不是给一个让人产生错觉的分数。

use serde::Serialize;

use super::data::fundamentals::{self, FinReport};
use super::data::universe::Listing;
use super::data::valuation::{self, ValPoint};

/// 免责声明。与 `recommend.rs` 一致，每个对外报告都必须带。
pub const DISCLAIMER: &str = "本结果为基于公开历史数据的统计筛选，不构成任何投资建议。\
历史规律不保证未来重现；股票投资可能损失全部本金。";

// ---- 排除规则 ----

/// 被排除的理由。每一条都要能说清「凭什么排除」，说不清的就不该排。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum Exclusion {
    /// ST / *ST / 退市 / PT 壳股。财务失真、流动性枯竭。
    /// 注意「国华退」这类退市股在估值表里照样有 PE=5.17、PB=0.70 的漂亮数字 ——
    /// 纯看财务指标挡不住，只能靠名称。
    RiskyShell,
    /// 近四个季度归母净利为负。这是**唯一通过验证**的基本面排除项：
    /// 该组历史平均最大回撤近 38%，显著劣于有盈利支撑组（< 25%）。
    LossMaking,
    /// 财报历史不足。算不出持续性，也就无从判断质量。
    InsufficientHistory { years: usize },
    /// 市值过小。小微盘的财报噪声大、易被操纵。
    TooSmall,
    /// 缺少估值数据（停牌等）。
    NoValuation,
}

impl Exclusion {
    pub fn reason(&self) -> String {
        match self {
            Self::RiskyShell => "ST/退市/PT 壳股：财务数据失真、流动性枯竭".into(),
            Self::LossMaking => "近四季度归母净利为负：该组历史平均最大回撤近38%，显著劣于有盈利支撑组".into(),
            Self::InsufficientHistory { years } => format!("财报历史仅 {years} 年，不足以判断经营持续性"),
            Self::TooSmall => "市值低于下限：小微盘财报噪声与操纵风险显著上升".into(),
            Self::NoValuation => "缺少估值数据（停牌或数据缺失）".into(),
        }
    }
}

/// 筛选参数。**注意这里没有「买入阈值」** —— 只有入池的最低门槛。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ScreenParams {
    /// 至少要有几年年报才纳入（判断持续性的最低样本量）
    pub min_years: usize,
    /// 市值下限（元）。极小市值股的财务数据噪声大、易被操纵。
    pub min_market_cap: f64,
    /// 输出条数上限
    pub top_n: usize,
}

impl Default for ScreenParams {
    fn default() -> Self {
        Self {
            min_years: 5,
            // 30 亿：低于此的小微盘财报噪声与操纵风险显著上升。
            // 这是个工程上的保守取值，不是从「百倍股特征」里推出来的 —— 那组数字没通过验证。
            min_market_cap: 3e9,
            top_n: 30,
        }
    }
}

// ---- 质量画像（描述，不是评分） ----

/// 一只股票的质量画像。**刻意不给总分** ——
/// 加权求和出来的「85 分」会让人以为这是买入信号，而证据不支持任何这样的断言。
/// 这里只如实呈现可核验的事实，判断留给人。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Profile {
    pub code: String,
    pub name: String,

    /// 有几个年报年份
    pub years: usize,
    /// 年报 ROE 中位数 %
    pub roe_median: Option<f64>,
    /// 年报 ROE 连续 ≥15% 的年数（从最近年报往回数）
    pub roe_streak: usize,
    /// 近 5 年营收 CAGR %（不足 5 年则用全部可得年份）
    pub revenue_cagr: Option<f64>,
    /// 近 5 年归母净利 CAGR %
    pub profit_cagr: Option<f64>,
    /// 最新年报毛利率 %（银行等无此项 → None）
    pub gross_margin: Option<f64>,

    pub market_cap: f64,
    pub pe_ttm: Option<f64>,
    /// 当前 PE 在该股自身历史中的分位（0 = 史上最便宜）
    pub pe_percentile: Option<f64>,

    /// 这些事实意味着什么 —— 以及**不**意味着什么
    pub note: String,
}

/// 复合年均增长率 %。首尾任一为非正 → None（负数开根号无意义）。
pub fn cagr(first: f64, last: f64, years: f64) -> Option<f64> {
    if first <= 0.0 || last <= 0.0 || years <= 0.0 { return None; }
    Some(((last / first).powf(1.0 / years) - 1.0) * 100.0)
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    if xs.is_empty() { return None; }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = xs.len();
    Some(if n.is_multiple_of(2) { (xs[n/2 - 1] + xs[n/2]) / 2.0 } else { xs[n/2] })
}

/// 从最近年报往回数，ROE 连续 ≥ `floor`% 的年数。
fn roe_streak(annuals: &[&FinReport], floor: f64) -> usize {
    annuals.iter().rev()
        .take_while(|r| r.roe.map(|v| v >= floor).unwrap_or(false))
        .count()
}

/// 近四个季度归母净利是否为负。
///
/// 财报是**年初至今累计值**，不是单季值 —— 最新一期若是三季报，它已包含前三季度，
/// 直接看它的符号即可判断「今年到目前为止是否亏损」。这里采用更保守的口径：
/// 最新一期累计净利 < 0 即视为亏损。
fn is_loss_making(reports: &[FinReport]) -> bool {
    reports.last().and_then(|r| r.net_profit).map(|v| v < 0.0).unwrap_or(false)
}

/// 对单只股票做排除判定 + 质量画像。
///
/// 返回 `Err(Exclusion)` 表示被排除（附理由），`Ok(Profile)` 表示留在池中
/// —— **留在池中不等于推荐买入**。
pub fn evaluate(
    listing: &Listing,
    reports: &[FinReport],
    vals: &[ValPoint],
    p: &ScreenParams,
) -> Result<Profile, Exclusion> {
    if listing.is_risky_shell() { return Err(Exclusion::RiskyShell); }

    let cap = listing.market_cap.ok_or(Exclusion::NoValuation)?;
    if cap < p.min_market_cap { return Err(Exclusion::TooSmall); }

    if is_loss_making(reports) { return Err(Exclusion::LossMaking); }

    let annuals = fundamentals::annuals(reports);
    if annuals.len() < p.min_years {
        return Err(Exclusion::InsufficientHistory { years: annuals.len() });
    }

    let roes: Vec<f64> = annuals.iter().filter_map(|r| r.roe).collect();
    let roe_median = median(roes);
    let streak = roe_streak(&annuals, 15.0);

    // CAGR 取最近 6 期年报的首尾（跨 5 年）。不足则用全部可得年份。
    let window = annuals.len().min(6);
    let slice = &annuals[annuals.len() - window..];
    let span = (window - 1) as f64;
    let revenue_cagr = slice.first().and_then(|f| f.revenue)
        .zip(slice.last().and_then(|l| l.revenue))
        .and_then(|(f, l)| cagr(f, l, span));
    let profit_cagr = slice.first().and_then(|f| f.net_profit)
        .zip(slice.last().and_then(|l| l.net_profit))
        .and_then(|(f, l)| cagr(f, l, span));

    let pes = valuation::positive_pes(vals);
    let pe_percentile = listing.pe_ttm.and_then(|pe| valuation::percentile(&pes, pe));

    let note = build_note(streak, profit_cagr, pe_percentile);

    Ok(Profile {
        code: listing.code.clone(),
        name: listing.name.clone(),
        years: annuals.len(),
        roe_median,
        roe_streak: streak,
        revenue_cagr,
        profit_cagr,
        gross_margin: annuals.last().and_then(|r| r.gross_margin),
        market_cap: cap,
        pe_ttm: listing.pe_ttm,
        pe_percentile,
        note,
    })
}

/// 措辞是这个模块的核心产出之一：必须让读者拿不到「这只会涨」的错觉。
fn build_note(streak: usize, profit_cagr: Option<f64>, pe_pct: Option<f64>) -> String {
    let mut parts = Vec::new();
    if streak >= 5 {
        parts.push(format!("ROE 连续 {streak} 年 ≥15%，经营质量的持续性可核验"));
    } else if streak > 0 {
        parts.push(format!("ROE 仅连续 {streak} 年 ≥15%，持续性尚短"));
    } else {
        parts.push("近一年 ROE 未达 15%".to_string());
    }
    if let Some(g) = profit_cagr {
        parts.push(format!("净利 5 年 CAGR {g:.1}%"));
    }
    if let Some(p) = pe_pct {
        parts.push(format!("当前 PE 处于自身历史 {:.0}% 分位", p * 100.0));
    }
    // 这句话必须在，且必须每条都带。
    parts.push("以上均为历史事实，不含对未来的预测；通过筛选仅代表未触发已知的排除项".to_string());
    parts.join("；")
}

// ---- 期望校准 ----

/// 基础发生率。每份报告都必须带 —— 没有它，一份「优质股清单」天然会被读成「翻倍名单」。
///
/// 数字来自本项目的实证检索中通过三票对抗式验证的结论，来源标注在字段旁。
#[derive(Debug, Clone, Serialize)]
pub struct BaseRate {
    pub headline: String,
    pub facts: Vec<String>,
}

impl Default for BaseRate {
    fn default() -> Self {
        Self {
            headline: "历史基础发生率：几十倍是复利+极端右偏分布的产物，不是可稳定复制的策略".into(),
            facts: vec![
                "A股 2000–2020：曾涨超10倍的有 637 只(17.7%)，但至今仍保持10倍的只剩 176 只(4.9%) \
                 —— 约 72% 的历史十倍股把涨幅还了回去。（东吴证券《A股十倍股群像》2020-10-14）".into(),
                "达成十倍平均耗时 100 个月（约 8 年，对应年化 33.4%）；\"几十倍\"现实中需要 8–20 年持有期。（同上）".into(),
                "美股 1926–2016 全样本：仅 42.6% 的个股终身回报跑赢一月期国债，众数结局是亏损 100%；\
                 全市场净财富 100% 由表现最好的 4% 公司贡献。（Bessembinder, JFE 2018）".into(),
                "随机单股集中持有的 bootstrap 模拟：96% 的路径跑输大盘、73% 的路径跑输国债。（同上）".into(),
                "口径陷阱：媒体所称茅台\"70倍\"是含分红再投资的后复权总回报，裸价格只涨了 10.3 倍；\
                 \"十倍股\"榜单里的倍数多为最低点→最高点的最大浮盈(max drawup)，无人可实现。".into(),
                "本筛选的作用是\"排除\"而非\"选中\"：它挡不住你错过茅台，但能挡住你踩中惠城环保\
                 （2022–2025 复权最大涨幅 32 倍，随后回撤 73.9%）。".into(),
            ],
        }
    }
}

// ---- 报告 ----

#[derive(Debug, Clone, Serialize)]
pub struct ScreenReport {
    pub generated: String,
    pub trade_date: String,
    pub pool_size: usize,
    /// 通过全部排除项、留在池中的只数
    pub passed: usize,
    /// 各排除理由的计数 —— 让「筛掉了什么」和「筛出了什么」一样可见
    pub excluded: Vec<(String, usize)>,
    pub top: Vec<Profile>,
    pub base_rate: BaseRate,
    pub params: ScreenParams,
    pub disclaimer: String,
}

/// 跑一遍筛选。
///
/// IO 通过闭包注入（同 `recommend::build_report` 的惯例），本函数保持纯逻辑、可单测。
/// `load` 对每只股票返回 (财报, 估值历史)；失败的股票直接跳过，不让单只的网络抖动
/// 毁掉整轮筛选。
pub fn build_report<F>(
    listings: &[Listing],
    trade_date: &str,
    generated: &str,
    p: &ScreenParams,
    mut load: F,
) -> ScreenReport
where
    F: FnMut(&Listing) -> anyhow::Result<(Vec<FinReport>, Vec<ValPoint>)>,
{
    let mut passed = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();

    for l in listings {
        // 壳股在取数之前就挡掉 —— 省掉几百次没必要的网络请求
        if l.is_risky_shell() {
            *counts.entry(Exclusion::RiskyShell.reason()).or_default() += 1;
            continue;
        }
        let Ok((reports, vals)) = load(l) else {
            *counts.entry("数据获取失败".into()).or_default() += 1;
            continue;
        };
        match evaluate(l, &reports, &vals, p) {
            Ok(profile) => passed.push(profile),
            Err(e) => *counts.entry(e.reason()).or_default() += 1,
        }
    }

    let total_passed = passed.len();

    // 排序只是为了让人先看到样本量最足的，**不是**优劣排名。
    // 按 ROE 持续年数降序，同则按净利 CAGR 降序。
    passed.sort_by(|a, b| {
        b.roe_streak.cmp(&a.roe_streak)
            .then(b.profit_cagr.unwrap_or(f64::MIN)
                .partial_cmp(&a.profit_cagr.unwrap_or(f64::MIN)).unwrap())
    });
    passed.truncate(p.top_n);

    ScreenReport {
        generated: generated.to_string(),
        trade_date: trade_date.to_string(),
        pool_size: listings.len(),
        passed: total_passed,
        excluded: counts.into_iter().collect(),
        top: passed,
        base_rate: BaseRate::default(),
        params: *p,
        disclaimer: DISCLAIMER.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    fn listing(code: &str, name: &str, cap: f64, pe: Option<f64>) -> Listing {
        Listing {
            market: 1, code: code.into(), name: name.into(),
            market_cap: Some(cap), pe_ttm: pe, pb_mrq: Some(5.0),
        }
    }

    /// n 年年报，ROE 固定，营收/净利按 rate 复合增长
    fn annual_reports(n: i32, roe: f64, first_rev: f64, first_np: f64, rate: f64) -> Vec<FinReport> {
        (0..n).map(|i| {
            let g = (1.0 + rate).powi(i);
            FinReport {
                date: d(2020 - n + 1 + i, 12, 31),
                revenue: Some(first_rev * g),
                revenue_yoy: Some(rate * 100.0),
                net_profit: Some(first_np * g),
                net_profit_yoy: Some(rate * 100.0),
                roe: Some(roe),
                gross_margin: Some(90.0),
                bps: Some(100.0),
                eps: Some(10.0),
            }
        }).collect()
    }

    fn vals(pe: f64, n: i64) -> Vec<ValPoint> {
        (0..n).map(|i| ValPoint {
            date: d(2018, 1, 1) + chrono::Duration::days(i),
            pe_ttm: Some(pe + (i % 20) as f64),
            pb_mrq: Some(5.0),
        }).collect()
    }

    #[test]
    fn cagr_math() {
        // 100 → 200 用 1 年 = +100%
        assert!((cagr(100.0, 200.0, 1.0).unwrap() - 100.0).abs() < 1e-9);
        // 5 年翻 32 倍 = 每年 +100%
        assert!((cagr(1.0, 32.0, 5.0).unwrap() - 100.0).abs() < 1e-6);
        // 负数/零无定义 —— 从亏损算"增长了几倍"是没有意义的
        assert_eq!(cagr(-10.0, 100.0, 5.0), None);
        assert_eq!(cagr(100.0, -10.0, 5.0), None);
        assert_eq!(cagr(0.0, 100.0, 5.0), None);
    }

    #[test]
    fn excludes_shells_by_name_even_with_pretty_financials() {
        // 国华退：PE 5.17、PB 0.70，财务指标看起来是个便宜的好票
        let l = listing("000004", "国华退", 1e10, Some(5.17));
        let r = annual_reports(10, 25.0, 1e9, 2e8, 0.2);
        assert_eq!(
            evaluate(&l, &r, &vals(20.0, 100), &ScreenParams::default()),
            Err(Exclusion::RiskyShell)
        );
    }

    #[test]
    fn excludes_loss_making_the_one_verified_fundamental_filter() {
        let l = listing("600010", "包钢股份", 1e11, Some(-53.0));
        let mut r = annual_reports(10, 20.0, 1e9, 2e8, 0.1);
        r.last_mut().unwrap().net_profit = Some(-5e8); // 最新一期亏损
        assert_eq!(
            evaluate(&l, &r, &vals(20.0, 100), &ScreenParams::default()),
            Err(Exclusion::LossMaking)
        );
    }

    #[test]
    fn excludes_short_history() {
        let l = listing("300750", "宁德时代", 1e12, Some(30.0));
        let r = annual_reports(3, 25.0, 1e9, 2e8, 0.3); // 只有 3 年年报
        assert_eq!(
            evaluate(&l, &r, &vals(30.0, 100), &ScreenParams::default()),
            Err(Exclusion::InsufficientHistory { years: 3 })
        );
    }

    #[test]
    fn profiles_a_quality_name_without_scoring_it() {
        let l = listing("600519", "贵州茅台", 1.5e12, Some(18.21));
        let r = annual_reports(10, 30.0, 1e10, 4e9, 0.2);
        let p = evaluate(&l, &r, &vals(20.0, 200), &ScreenParams::default()).unwrap();

        assert_eq!(p.name, "贵州茅台");
        assert_eq!(p.years, 10);
        assert_eq!(p.roe_streak, 10, "ROE 全程 30% ≥ 15%");
        assert!((p.roe_median.unwrap() - 30.0).abs() < 1e-9);
        // 6 期年报跨 5 年、年增 20% → CAGR 20%
        assert!((p.profit_cagr.unwrap() - 20.0).abs() < 1e-6);
        assert!((p.revenue_cagr.unwrap() - 20.0).abs() < 1e-6);

        // 关键：Profile 里没有任何「总分」字段。加权总分会被读成买入信号，
        // 而证据不支持任何这样的断言。
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("\"score\""), "不得出现总分字段");
        assert!(!json.contains("\"signal\""), "不得出现买卖信号字段");
        assert!(p.note.contains("不含对未来的预测"), "措辞必须堵死「这只会涨」的错觉");
    }

    #[test]
    fn note_never_promises_upside() {
        let l = listing("600519", "贵州茅台", 1.5e12, Some(18.21));
        let r = annual_reports(10, 30.0, 1e10, 4e9, 0.2);
        let p = evaluate(&l, &r, &vals(20.0, 200), &ScreenParams::default()).unwrap();
        for banned in ["推荐", "买入", "看好", "将上涨", "目标价"] {
            assert!(!p.note.contains(banned), "note 不得出现 `{banned}`");
        }
    }

    #[test]
    fn report_shows_what_was_filtered_out_not_just_what_survived() {
        let listings = vec![
            listing("600519", "贵州茅台", 1.5e12, Some(18.0)),
            listing("000004", "国华退", 1e10, Some(5.0)),
            listing("600010", "包钢股份", 1e11, Some(-53.0)),
        ];
        let rep = build_report(&listings, "2026-07-10", "2026-07-12", &ScreenParams::default(), |l| {
            let mut r = annual_reports(10, 30.0, 1e10, 4e9, 0.2);
            if l.code == "600010" { r.last_mut().unwrap().net_profit = Some(-5e8); }
            Ok((r, vals(20.0, 200)))
        });

        assert_eq!(rep.pool_size, 3);
        assert_eq!(rep.passed, 1);
        assert_eq!(rep.top.len(), 1);
        assert_eq!(rep.top[0].code, "600519");

        // 「筛掉了什么」必须和「筛出了什么」一样可见 —— 否则用户无从判断筛选是否合理
        let reasons: Vec<&str> = rep.excluded.iter().map(|(r, _)| r.as_str()).collect();
        assert!(reasons.iter().any(|r| r.contains("壳股")));
        assert!(reasons.iter().any(|r| r.contains("归母净利为负")));
        assert_eq!(rep.excluded.iter().map(|(_, n)| n).sum::<usize>(), 2);
    }

    #[test]
    fn every_report_carries_base_rates_and_disclaimer() {
        let rep = build_report(&[], "2026-07-10", "2026-07-12", &ScreenParams::default(), |_| {
            Ok((vec![], vec![]))
        });
        assert!(rep.disclaimer.contains("不构成"));

        // 没有基础发生率，一份「优质股清单」天然会被读成「翻倍名单」
        let joined = rep.base_rate.facts.join("");
        assert!(joined.contains("4.9%"), "须给出十倍股基础发生率");
        assert!(joined.contains("72%"), "须说明多数十倍股把涨幅还了回去");
        assert!(joined.contains("8 年") || joined.contains("8–20 年"), "须说明所需年限");
        assert!(joined.contains("Bessembinder"), "须给出个股回报分布的硬先验");
        assert!(joined.contains("10.3 倍"), "须点破茅台70倍的口径陷阱");
        assert!(rep.base_rate.headline.contains("不是可稳定复制的策略"));
    }

    /// 实网端到端：`cargo test -- --ignored`。
    /// 用真实全集 + 真实财报 + 真实估值跑一遍，验证各数据源的**代码/字段/缓存键真的对得上** ——
    /// fixture 只能证明各模块自洽，证明不了它们拼在一起是通的。
    #[test]
    #[ignore]
    fn live_screen_real_pool() {
        use crate::stock::data::{fundamentals as fu, universe as un, valuation as va};

        let date = un::latest_trade_date().expect("探测交易日");
        let all = un::fetch_a_snapshot(date).expect("抓全集");

        // 全市场逐只抓财报要几千次请求，实测里取一个有代表性的小样本：
        // 白马、银行(无毛利率)、亏损股、退市壳、次新股 —— 每一类都该走到不同的排除分支
        let want = ["600519", "600036", "600010", "000004", "300750"];
        let pool: Vec<Listing> = all.iter()
            .filter(|l| want.contains(&l.code.as_str()))
            .cloned().collect();
        assert_eq!(pool.len(), want.len(), "样本股应都在全集里");

        let rep = build_report(&pool, &date.to_string(), &date.to_string(),
                               &ScreenParams::default(), |l| {
            let secid = l.secid();
            let reports = fu::fetch(&secid)?;
            let vals = va::fetch(&secid).unwrap_or_default(); // 港股无估值历史 → 空
            Ok((reports, vals))
        });

        println!("\n交易日 {date}｜池 {} 只｜通过 {} 只", rep.pool_size, rep.passed);
        for (reason, n) in &rep.excluded {
            println!("  排除 {n} 只：{reason}");
        }
        for p in &rep.top {
            println!("\n{} {} ｜{} 年年报｜ROE中位数 {:?}｜ROE连续≥15% {} 年｜净利CAGR {:?}",
                     p.code, p.name, p.years, p.roe_median.map(|v| (v*10.0).round()/10.0),
                     p.roe_streak, p.profit_cagr.map(|v| (v*10.0).round()/10.0));
            println!("  PE {:?}｜历史分位 {:?}", p.pe_ttm.map(|v| (v*100.0).round()/100.0),
                     p.pe_percentile.map(|v| format!("{:.0}%", v*100.0)));
            println!("  {}", p.note);
        }

        // 退市壳必须被名称挡掉（它的财务指标是漂亮的：PE 5.17、PB 0.70）
        assert!(rep.excluded.iter().any(|(r, _)| r.contains("壳股")), "国华退应被排除");
        // 茅台应通过（10+ 年年报、ROE 长期 >15%、盈利）
        assert!(rep.top.iter().any(|p| p.code == "600519"), "茅台应通过筛选");
        // 报告必须自带基础发生率与免责声明，否则会被读成「翻倍名单」
        assert!(!rep.base_rate.facts.is_empty());
        assert!(rep.disclaimer.contains("不构成"));
    }

    #[test]
    fn single_stock_failure_does_not_kill_the_run() {
        let listings = vec![
            listing("600519", "贵州茅台", 1.5e12, Some(18.0)),
            listing("600036", "招商银行", 1e12, Some(6.0)),
        ];
        let rep = build_report(&listings, "2026-07-10", "2026-07-12", &ScreenParams::default(), |l| {
            if l.code == "600036" { anyhow::bail!("网络抖动"); }
            Ok((annual_reports(10, 30.0, 1e10, 4e9, 0.2), vals(20.0, 200)))
        });
        assert_eq!(rep.passed, 1, "一只失败不该毁掉整轮");
        assert!(rep.excluded.iter().any(|(r, n)| r == "数据获取失败" && *n == 1));
    }
}

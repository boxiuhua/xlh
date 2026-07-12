//! 回报归因分解：把「涨了多少倍」拆成 盈利增长 × 估值扩张 × 分红再投资。
//!
//! ## 恒等式
//!
//! 股价 = 每股盈利 × 市盈率，所以对任意区间 [t0, t1]：
//!
//! ```text
//!   裸价格倍数  = (EPS₁/EPS₀) × (PE₁/PE₀)
//!   总回报倍数  = 裸价格倍数 × 分红再投资因子
//! ```
//!
//! 取对数后三项可加，各自占比即为归因权重（这正是「茅台 87% 靠盈利、13% 靠估值」的算法）。
//!
//! ## 两个必须内建的诚实性约束
//!
//! **一、起点敏感。** 研究里最重要的警告：茅台以 IPO(PE 23.9) 为锚是「87% 盈利驱动」，
//! 以 2014 年塑化剂危机低点(PE 8.83) 为锚则估值扩张单项就贡献 5–6 倍，结论完全反转。
//! 所以 `Attribution` 强制带上 `start_pe_percentile` —— 起点估值分位不和归因一起看，
//! 归因就是误导。
//!
//! **二、口径。** 媒体说的茅台「70 倍」是**含分红再投资的后复权总回报**，
//! 裸价格只涨了 10.3 倍。本模块把两者分开报（`total_multiple` vs `price_multiple`），
//! 绝不混用。
//!
//! EPS 不从财报的累计值反推（Q3 YTD → TTM 要拿去年年报减去年三季报再加今年三季报，
//! 易错且有滞后），而是用恒等式 **EPS_TTM = 收盘价 / PE_TTM** 从估值序列直接取 ——
//! 这样恒等式按构造精确成立，不会出现「三项乘起来对不上总数」的尴尬。

use serde::Serialize;

use super::data::valuation::{self, ValPoint};
use super::data::StockBar;

/// 占比是相对谁算的。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ShareBasis {
    /// 相对总回报（含分红再投资）—— 有后复权数据时的完整口径
    TotalReturn,
    /// 相对裸价格 —— 后复权数据不覆盖该区间时的降级口径，**不含分红**
    PriceOnly,
}

/// 一次归因分解的结果。所有 `*_multiple` 都是**倍数**（2.0 = 涨了 1 倍，翻倍）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Attribution {
    pub start_date: String,
    pub end_date: String,
    pub years: f64,

    /// 总回报倍数（后复权，**含分红再投资**）。
    /// 后复权数据不覆盖该区间时为 `None` —— 见 `dividend_multiple` 的说明。
    pub total_multiple: Option<f64>,
    /// 裸价格倍数（不复权，不含分红）。总是可算。
    pub price_multiple: f64,

    /// 盈利增长倍数 EPS₁/EPS₀
    pub earnings_multiple: f64,
    /// 估值扩张倍数 PE₁/PE₀
    pub valuation_multiple: f64,

    /// 分红再投资因子 = 总回报 / 裸价格。
    ///
    /// **`None` 不等于「分红没贡献」，而是「算不出来」。** 这个区分是致命的：
    /// `kline::merge` 在后复权缺失时会让 `adj_close` 回落成 `close`，若照单全收，
    /// 这个因子会算出恰好 1.0，等于向用户断言「分红一分钱没贡献」——
    /// 而茅台的真相是总回报 70 倍、裸价格才 10.3 倍，分红再投资贡献了近 7 倍。
    /// 宁可说不知道，也不能给这个错数。
    pub dividend_multiple: Option<f64>,

    /// 对数口径占比，各项相加 ≈ 1.0。
    ///
    /// **仅在该区间为盈利（回报倍数 > 1）时有值。** 亏损区间下这个归一化会产生
    /// 严重误导：ln(回报) 为负导致分母翻号，于是「盈利增长 1.2 倍、估值收缩 0.59 倍、
    /// 净亏损」会被算成「盈利贡献 -67%、估值贡献 +230%」—— 数学上自洽（相加为 1），
    /// 读起来却像在说「盈利增长害你亏了钱」。占比这个概念在亏损时就是不成立的，
    /// 故置 `None`，改由 `explain()` 用人话说清谁正谁负。
    pub earnings_share: Option<f64>,
    pub valuation_share: Option<f64>,
    pub dividend_share: Option<f64>,
    /// 上面的占比是相对总回报还是仅相对裸价格算的
    pub share_basis: ShareBasis,

    /// 年化回报 %（口径同 `share_basis`）
    pub annualized: f64,

    pub start_pe: f64,
    pub end_pe: f64,
    /// 起点 PE 在该股自身历史中的分位（0=史上最低）。
    /// **归因结论对它极度敏感**：起点分位越低，「估值扩张」被高估得越厉害。
    pub start_pe_percentile: Option<f64>,
    /// 起点分位过低 / 缺分红数据等警告，多条以换行分隔；无警告则为空串。
    pub caveat: String,
}

/// 归因失败的原因。宁可说不知道，也不要编一个数出来。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum AttributionError {
    /// 起点或终点亏损（PE ≤ 0）—— 盈利倍数无意义（从亏损到盈利没有「增长了几倍」可言）
    NonPositiveEarnings,
    /// 区间内缺估值数据
    MissingValuation,
    /// 区间内缺行情数据
    MissingPrice,
    /// 区间太短，年化没有意义
    TooShort,
}

impl std::fmt::Display for AttributionError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let s = match self {
            Self::NonPositiveEarnings => "起点或终点为亏损（PE≤0），盈利增长倍数无定义",
            Self::MissingValuation => "区间内缺少估值(PE)数据",
            Self::MissingPrice => "区间内缺少行情数据",
            Self::TooShort => "区间不足 1 年，年化无意义",
        };
        write!(f, "{s}")
    }
}

/// 起点 PE 低于此分位时，归因结果会系统性高估「估值扩张」的贡献，必须警告。
const LOW_START_PERCENTILE: f64 = 0.20;

/// 把 [start, end] 区间的回报拆成 盈利 × 估值 × 分红。
///
/// `bars` 需含 `close`（裸价）与 `adj_close`（后复权，含分红再投资）；
/// `vals` 是同期的日频 PE 序列；`pe_history` 是该股**全部**历史正 PE，用于算起点分位。
pub fn attribute(
    bars: &[StockBar],
    vals: &[ValPoint],
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
) -> Result<Attribution, AttributionError> {
    let b0 = bars.iter().find(|b| b.date >= start).ok_or(AttributionError::MissingPrice)?;
    let b1 = bars.iter().rev().find(|b| b.date <= end).ok_or(AttributionError::MissingPrice)?;
    if b1.date <= b0.date { return Err(AttributionError::MissingPrice); }

    let days = (b1.date - b0.date).num_days() as f64;
    let years = days / 365.25;
    if years < 1.0 { return Err(AttributionError::TooShort); }

    let v0 = valuation::at_or_before(vals, b0.date).ok_or(AttributionError::MissingValuation)?;
    let v1 = valuation::at_or_before(vals, b1.date).ok_or(AttributionError::MissingValuation)?;
    let (pe0, pe1) = (
        v0.pe_ttm.ok_or(AttributionError::MissingValuation)?,
        v1.pe_ttm.ok_or(AttributionError::MissingValuation)?,
    );
    // 亏损期做归因是没有意义的：EPS 从 -0.1 涨到 +0.5，「增长了几倍」无从谈起。
    if pe0 <= 0.0 || pe1 <= 0.0 { return Err(AttributionError::NonPositiveEarnings); }

    if b0.close <= 0.0 || b0.adj_close <= 0.0 { return Err(AttributionError::MissingPrice); }

    let price_multiple = b1.close / b0.close;

    // 恒等式取 EPS：EPS_TTM = 价格 / PE_TTM。
    // 如此 earnings × valuation ≡ price_multiple 按构造精确成立。
    let eps0 = b0.close / pe0;
    let eps1 = b1.close / pe1;
    let earnings_multiple = eps1 / eps0;
    let valuation_multiple = pe1 / pe0;

    // 后复权是否真的覆盖了这个区间？
    // `kline::merge` 在 hfq 缺失时把 adj_close 填成 close，于是「无数据」和「不分红」
    // 长得一模一样。判据：区间内若**任何一根** bar 的 adj 与 close 不同，说明确有后复权数据。
    // 全都相同则无法区分二者 —— 此时必须承认算不出来，而不是报一个 1.0。
    let has_adj = bars.iter()
        .filter(|b| b.date >= b0.date && b.date <= b1.date)
        .any(|b| (b.adj_close - b.close).abs() > 1e-9);

    let (total_multiple, dividend_multiple) = if has_adj {
        let t = b1.adj_close / b0.adj_close;
        (Some(t), Some(t / price_multiple))
    } else {
        (None, None)
    };

    // 有总回报就按总回报算占比，否则退到裸价格口径（并在 share_basis 里说清楚）。
    let (basis, base_multiple) = match total_multiple {
        Some(t) => (ShareBasis::TotalReturn, t),
        None => (ShareBasis::PriceOnly, price_multiple),
    };

    // 占比只在赚钱时成立。回报 ≤ 1 时 ln 为负或零，归一化会翻号并产出误导性数字
    // （见 earnings_share 的文档）—— 此时一律 None，让 explain() 用人话讲。
    let ln_base = base_multiple.ln();
    let (es, vs, ds) = if base_multiple > 1.0 && ln_base > 1e-9 {
        (
            Some(earnings_multiple.ln() / ln_base),
            Some(valuation_multiple.ln() / ln_base),
            dividend_multiple.map(|d| d.ln() / ln_base),
        )
    } else {
        (None, None, None)
    };
    let annualized = (base_multiple.powf(1.0 / years) - 1.0) * 100.0;

    let pes = valuation::positive_pes(vals);
    let start_pe_percentile = valuation::percentile(&pes, pe0);

    let mut caveats = Vec::new();
    match start_pe_percentile {
        Some(p) if p < LOW_START_PERCENTILE => caveats.push(format!(
            "起点 PE {pe0:.1} 处于该股历史 {:.0}% 分位（极低）。归因对起点极度敏感：\
             从估值低谷起算会把「估值修复」记成「估值扩张」的贡献，\
             换一个正常估值的起点，盈利/估值的占比可能完全反转。",
            p * 100.0)),
        None => caveats.push(
            "该股历史估值样本不足，起点分位无法计算，归因的起点敏感性无从评估。".to_string()),
        _ => {}
    }
    if total_multiple.is_none() {
        caveats.push("该区间无后复权数据，**无法计算分红再投资的贡献** —— 下列倍数与占比均为\
             裸价格口径，会系统性低估真实总回报。（参考：茅台 2001–2017 裸价格仅涨 10.3 倍，\
             含分红再投资的总回报是 70 倍。）当前数据源的后复权覆盖范围有限，\
             若需长周期总回报口径，须换用覆盖更长的后复权数据源。".to_string());
    }

    Ok(Attribution {
        start_date: b0.date.to_string(),
        end_date: b1.date.to_string(),
        years,
        total_multiple,
        price_multiple,
        earnings_multiple,
        valuation_multiple,
        dividend_multiple,
        earnings_share: es,
        valuation_share: vs,
        dividend_share: ds,
        share_basis: basis,
        annualized,
        start_pe: pe0,
        end_pe: pe1,
        start_pe_percentile,
        caveat: caveats.join("\n"),
    })
}

impl Attribution {
    /// 人话摘要，可直接进推送 markdown。
    ///
    /// 口径必须写在脸上：没有分红数据时，绝不能把裸价格倍数说成「总回报」。
    pub fn explain(&self) -> String {
        let head = match (self.total_multiple, self.dividend_multiple) {
            (Some(t), Some(d)) => format!(
                "总回报 {t:.1} 倍（含分红再投资，年化 {:.1}%）：其中裸价格 {:.1} 倍，\
                 分红再投资贡献 {d:.2} 倍。",
                self.annualized, self.price_multiple),
            _ => format!(
                "裸价格 {:.1} 倍（**不含分红**，年化 {:.1}%）。",
                self.price_multiple, self.annualized),
        };
        let val_word = if self.valuation_multiple >= 1.0 { "扩张" } else { "收缩" };

        // 赚钱时给占比；亏钱时占比无意义（会翻号误导），改用人话讲清谁正谁负。
        let breakdown = match (self.earnings_share, self.valuation_share) {
            (Some(es), Some(vs)) => {
                let mut b = format!(
                    "拆解：盈利增长 {:.1} 倍（占 {:.0}%）× 估值{val_word} {:.2} 倍（占 {:.0}%）。",
                    self.earnings_multiple, es * 100.0, self.valuation_multiple, vs * 100.0);
                if es > 1.0 {
                    b.push_str("\n（盈利占比超过 100%，是因为估值收缩在拖后腿 —— 涨幅全靠盈利增长扛出来。）");
                }
                b
            }
            _ => format!(
                "拆解：盈利增长 {:.1} 倍（正贡献），但估值{val_word}至 {:.2} 倍，\
                 把盈利增长{}。这段区间是亏损的，「各项占比」在亏损下会翻号误导，故不给出。",
                self.earnings_multiple, self.valuation_multiple,
                if self.earnings_multiple > 1.0 { "吃光还倒贴" } else { "进一步放大了亏损" }),
        };

        let mut s = format!("{} → {}（{:.1} 年）：{head}\n{breakdown}",
                            self.start_date, self.end_date, self.years);
        if !self.caveat.is_empty() {
            s.push_str("\n⚠️ ");
            s.push_str(&self.caveat.replace('\n', "\n⚠️ "));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use crate::stock::data::secid::Secid;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

    /// 实网端到端：`cargo test -- --ignored`。走**生产路径** `kline::fetch`（腾讯优先）。
    ///
    /// 归因窗口的长度受限于后复权(hfq)覆盖范围，而非估值历史：
    ///   - 估值历史（datacenter）：2018-01 起，约 2000 个交易日
    ///   - 腾讯后复权：仅约 640 根（2023-11 起）；`tencent::merge_clip` 会把 bar 裁到 hfq 区间
    ///   - 东财 K线不裁剪、能给全历史，但 push2his 限流频繁，只能当兜底
    /// 所以生产环境下长周期归因会自动缩到 hfq 能覆盖的区间 —— 这是数据源的真实约束，
    /// 本测试如实断言它，而不是绕过它。
    #[test]
    #[ignore]
    fn live_attribute_moutai() {
        use crate::stock::data::{kline, valuation as v};
        let secid = Secid { market: 1, code: "600519".into() };

        let bars = kline::fetch(&secid).expect("抓茅台K线");
        let vals = v::fetch(&secid).expect("抓茅台估值历史");
        println!("K线 {} 根（{} → {}），估值 {} 天（{} 起）",
                 bars.len(), bars[0].date, bars.last().unwrap().date,
                 vals.len(), vals[0].date);

        // 尽可能取长：从 K线 与 估值 两者都覆盖的最早日期起算
        let start = bars[0].date.max(vals[0].date);
        let end = bars.last().unwrap().date;
        let a = attribute(&bars, &vals, start, end).expect("茅台应可归因");

        println!("{}", a.explain());

        // 恒等式必须精确成立，否则归因是自相矛盾的
        assert!((a.earnings_multiple * a.valuation_multiple - a.price_multiple).abs() < 1e-6,
                "盈利 × 估值 必须等于裸价格");

        // 后复权含分红再投资，必然 ≥ 裸价格。缺后复权数据时应为 None 而非 1.0。
        if let Some(t) = a.total_multiple {
            assert!(t >= a.price_multiple);
            assert_eq!(a.share_basis, ShareBasis::TotalReturn);
        } else {
            assert_eq!(a.share_basis, ShareBasis::PriceOnly);
            assert!(a.caveat.contains("无法计算分红"));
        }

        let base = a.total_multiple.unwrap_or(a.price_multiple);
        match (a.earnings_share, a.valuation_share) {
            (Some(es), Some(vs)) => {
                assert!(base > 1.0, "有占比 ⇒ 必是盈利区间");
                let sum = es + vs + a.dividend_share.unwrap_or(0.0);
                assert!((sum - 1.0).abs() < 1e-6, "各项对数占比须相加为 1，实得 {sum}");
                // 估值收缩却仍赚钱 → 涨幅全靠盈利扛，盈利占比 > 100%、估值占比为负。
                // 这是「戴维斯双击」的反面 —— 光看「涨了多少」看不出，拆开才看得见。
                if a.valuation_multiple < 1.0 {
                    assert!(es > 1.0, "估值收缩时盈利占比应 > 100%，实得 {es}");
                    assert!(vs < 0.0, "估值收缩的占比应为负（拖后腿）");
                }
            }
            _ => {
                // 亏损区间：占比必须集体缺席，绝不能给出翻号的百分比
                assert!(base <= 1.0, "无占比 ⇒ 必是亏损区间，实得回报 {base}");
                assert_eq!(a.dividend_share, None);
                assert!(a.explain().contains("亏损"), "亏损须在人话摘要里说明");
            }
        }
    }

    fn bar(dt: NaiveDate, close: f64, adj: f64) -> StockBar {
        StockBar { date: dt, open: close, high: close, low: close, close, volume: 1.0, adj_close: adj }
    }
    fn vp(dt: NaiveDate, pe: f64) -> ValPoint {
        ValPoint { date: dt, pe_ttm: Some(pe), pb_mrq: Some(1.0) }
    }

    /// 构造一段有足够历史的 PE 序列（分位需要 ≥60 个样本）
    fn pe_history(base: f64, n: i64) -> Vec<ValPoint> {
        (0..n).map(|i| vp(d(2016, 1, 1) + chrono::Duration::days(i), base + (i % 20) as f64)).collect()
    }

    #[test]
    fn identity_holds_earnings_times_valuation_equals_price() {
        // 10 年：价格 10→100（10倍），PE 20→25（1.25倍）→ 盈利必然是 8 倍
        let bars = vec![bar(d(2010,1,4), 10.0, 12.0), bar(d(2020,1,6), 100.0, 130.0)];
        let mut vals = pe_history(20.0, 80);
        vals.push(vp(d(2010,1,4), 20.0));
        vals.push(vp(d(2020,1,6), 25.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2010,1,1), d(2020,1,10)).unwrap();
        assert!((a.price_multiple - 10.0).abs() < 1e-9);
        assert!((a.valuation_multiple - 1.25).abs() < 1e-9);
        assert!((a.earnings_multiple - 8.0).abs() < 1e-9, "10 = 8 × 1.25");
        // 恒等式：盈利 × 估值 == 裸价格
        assert!((a.earnings_multiple * a.valuation_multiple - a.price_multiple).abs() < 1e-9);
    }

    #[test]
    fn log_shares_sum_to_one() {
        let bars = vec![bar(d(2010,1,4), 10.0, 12.0), bar(d(2020,1,6), 100.0, 130.0)];
        let mut vals = pe_history(20.0, 80);
        vals.push(vp(d(2010,1,4), 20.0));
        vals.push(vp(d(2020,1,6), 25.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2010,1,1), d(2020,1,10)).unwrap();
        assert_eq!(a.share_basis, ShareBasis::TotalReturn);
        let sum = a.earnings_share.unwrap() + a.valuation_share.unwrap() + a.dividend_share.unwrap();
        assert!((sum - 1.0).abs() < 1e-9, "三项对数占比须相加为 1，实得 {sum}");
    }

    /// 亏损区间下「占比」这个概念不成立，强行归一化会翻号并输出误导性数字。
    ///
    /// 回归用例取自实测：茅台 2023-11-17 → 2026-07-10，盈利涨 1.2 倍、PE 收缩到 0.59 倍、
    /// 总回报 0.8 倍（亏损）。ln(0.8) < 0 使分母翻号，归一化会算出
    /// 「盈利贡献 -67%、估值贡献 +230%」—— 相加确实等于 1，读起来却像在说
    /// 「盈利增长害你亏了钱」。必须拒绝给出占比。
    #[test]
    fn loss_window_must_not_report_sign_flipped_shares() {
        // 价格 1000 → 700（0.7倍），PE 40 → 23.6（0.59倍）→ 盈利 1.2 倍
        let bars = vec![bar(d(2023,11,17), 1000.0, 1000.0), bar(d(2026,7,10), 700.0, 812.0)];
        let mut vals = pe_history(30.0, 80);
        vals.push(vp(d(2023,11,17), 40.0));
        vals.push(vp(d(2026,7,10), 23.6));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2023,11,1), d(2026,7,31)).unwrap();
        assert!(a.total_multiple.unwrap() < 1.0, "这是个亏损区间");
        assert!(a.earnings_multiple > 1.0, "但盈利确实是增长的");
        assert!(a.valuation_multiple < 1.0, "是估值收缩吃掉了盈利增长");

        assert_eq!(a.earnings_share, None, "亏损时不得给出占比");
        assert_eq!(a.valuation_share, None);
        assert_eq!(a.dividend_share, None);

        // 人话必须讲清是谁拖累的，且不能出现翻号的百分比
        let text = a.explain();
        assert!(text.contains("正贡献"), "须说明盈利是正贡献");
        assert!(text.contains("吃光还倒贴"), "须说明是估值收缩把盈利增长吃掉了");
        assert!(!text.contains("-67"), "不得出现翻号的误导性占比");
        assert!(!text.contains("230"), "不得出现翻号的误导性占比");
    }

    #[test]
    fn separates_total_return_from_naked_price() {
        // 研究里的核心口径陷阱：茅台「70倍」含分红再投资，裸价格只有 10.3 倍。
        let bars = vec![bar(d(2001,8,27), 35.55, 35.55), bar(d(2017,3,8), 366.2, 2572.22)];
        let mut vals = pe_history(30.0, 80);
        vals.push(vp(d(2001,8,27), 23.93));
        vals.push(vp(d(2017,3,8), 25.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2001,1,1), d(2017,12,31)).unwrap();
        assert!((a.total_multiple.unwrap() - 72.35).abs() < 0.1, "总回报(后复权) ≈ 72 倍");
        assert!((a.price_multiple - 10.3).abs() < 0.1, "裸价格只有 ≈ 10.3 倍");
        assert!(a.dividend_multiple.unwrap() > 6.0, "差额来自分红再投资");
        assert!(a.annualized > 25.0 && a.annualized < 35.0, "年化应在 30% 附近，实得 {}", a.annualized);
    }

    /// 本模块最危险的静默错误路径，单独锁死。
    ///
    /// `kline::merge` 在后复权数据缺失时会把 `adj_close` 填成 `close`。若照单全收，
    /// 分红因子会算出恰好 1.0 —— 等于向用户断言「分红一分钱没贡献」。
    /// 它不会报错，只会给出一个看起来很正常的错数，而真相可能是分红贡献了 7 倍。
    #[test]
    fn missing_hfq_must_not_masquerade_as_zero_dividends() {
        // adj_close 全等于 close = 后复权缺失被填充的特征
        let bars = vec![bar(d(2010,1,4), 10.0, 10.0), bar(d(2020,1,6), 100.0, 100.0)];
        let mut vals = pe_history(20.0, 80);
        vals.push(vp(d(2010,1,4), 20.0));
        vals.push(vp(d(2020,1,6), 25.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2010,1,1), d(2020,1,10)).unwrap();

        assert_eq!(a.dividend_multiple, None, "算不出来必须是 None，绝不能是 1.0");
        assert_eq!(a.total_multiple, None, "没有分红数据就没有总回报可言");
        assert_eq!(a.dividend_share, None);
        assert_eq!(a.share_basis, ShareBasis::PriceOnly, "占比须标明是裸价格口径");

        // 降级到裸价格口径后，两项占比仍须自洽（该区间是上涨的，占比成立）
        let sum = a.earnings_share.unwrap() + a.valuation_share.unwrap();
        assert!((sum - 1.0).abs() < 1e-9, "裸价格口径下两项占比须相加为 1，实得 {sum}");

        // 必须显式告警，且说清后果
        assert!(a.caveat.contains("无法计算分红"), "须明说算不出分红贡献");
        assert!(a.caveat.contains("低估"), "须说明后果是系统性低估真实回报");

        // 人话摘要绝不能把裸价格这个数字**标称**为「总回报」。
        // 只查告警之前的正文 —— 告警文案里出现「总回报」是正当的（它正是在解释缺了什么）。
        let text = a.explain();
        let body = text.split("⚠️").next().unwrap();
        assert!(body.contains("不含分红"), "摘要须标注口径");
        assert!(!body.contains("总回报"), "无分红数据时，正文不得把裸价格标称为「总回报」");
        assert!(text.contains("⚠️"), "告警须出现在摘要里");
    }

    #[test]
    fn warns_when_start_pe_is_at_historic_low() {
        // 研究的核心警告：从估值低谷起算，归因会把「估值修复」记成「估值扩张」
        let bars = vec![bar(d(2014,1,2), 10.0, 10.0), bar(d(2020,1,6), 100.0, 100.0)];
        // 历史 PE 主要在 30~50，起点却是 9 → 极低分位
        let mut vals: Vec<ValPoint> = (0..100)
            .map(|i| vp(d(2016,1,1) + chrono::Duration::days(i), 30.0 + (i % 20) as f64))
            .collect();
        vals.push(vp(d(2014,1,2), 9.0));
        vals.push(vp(d(2020,1,6), 45.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2014,1,1), d(2020,1,10)).unwrap();
        let p = a.start_pe_percentile.expect("应能算出起点分位");
        assert!(p < LOW_START_PERCENTILE, "起点 PE 9 应落在极低分位，实得 {p}");
        assert!(!a.caveat.is_empty(), "低起点必须给出警告");
        assert!(a.caveat.contains("反转"), "警告须点明结论可能反转");
        // 估值扩张 5 倍，占比很高 —— 正是警告要提醒的那种归因
        assert!((a.valuation_multiple - 5.0).abs() < 1e-9);
        assert!(a.valuation_share.unwrap() > 0.5);
    }

    #[test]
    fn no_warning_from_a_normal_start_valuation() {
        // adj != close → 后复权数据齐全，不该触发缺分红告警
        let bars = vec![bar(d(2010,1,4), 10.0, 10.0), bar(d(2020,1,6), 100.0, 130.0)];
        let mut vals: Vec<ValPoint> = (0..100)
            .map(|i| vp(d(2016,1,1) + chrono::Duration::days(i), 10.0 + (i % 30) as f64))
            .collect();
        vals.push(vp(d(2010,1,4), 30.0)); // 高于多数历史 → 分位不低
        vals.push(vp(d(2020,1,6), 33.0));
        vals.sort_by_key(|v| v.date);

        let a = attribute(&bars, &vals, d(2010,1,1), d(2020,1,10)).unwrap();
        assert!(a.start_pe_percentile.unwrap() >= LOW_START_PERCENTILE);
        assert_eq!(a.share_basis, ShareBasis::TotalReturn);
        assert!(a.caveat.is_empty(), "正常起点 + 有后复权数据 → 不该报警，实得: {}", a.caveat);
    }

    #[test]
    fn refuses_to_attribute_across_losses() {
        let bars = vec![bar(d(2010,1,4), 10.0, 10.0), bar(d(2020,1,6), 100.0, 100.0)];
        let mut vals = pe_history(20.0, 80);
        vals.push(vp(d(2010,1,4), -5.0)); // 起点亏损
        vals.push(vp(d(2020,1,6), 25.0));
        vals.sort_by_key(|v| v.date);

        // 从亏损到盈利，"盈利增长了几倍" 无定义 —— 必须报错而不是算出一个负倍数
        assert_eq!(
            attribute(&bars, &vals, d(2010,1,1), d(2020,1,10)),
            Err(AttributionError::NonPositiveEarnings)
        );
    }

    #[test]
    fn refuses_sub_year_windows() {
        let bars = vec![bar(d(2020,1,4), 10.0, 10.0), bar(d(2020,6,6), 20.0, 20.0)];
        let mut vals = pe_history(20.0, 80);
        vals.push(vp(d(2020,1,4), 20.0));
        vals.push(vp(d(2020,6,6), 25.0));
        vals.sort_by_key(|v| v.date);
        assert_eq!(
            attribute(&bars, &vals, d(2020,1,1), d(2020,6,30)),
            Err(AttributionError::TooShort)
        );
    }

    #[test]
    fn explain_reports_both_calibers_and_the_caveat() {
        let bars = vec![bar(d(2014,1,2), 10.0, 10.0), bar(d(2020,1,6), 100.0, 130.0)];
        let mut vals: Vec<ValPoint> = (0..100)
            .map(|i| vp(d(2016,1,1) + chrono::Duration::days(i), 30.0 + (i % 20) as f64))
            .collect();
        vals.push(vp(d(2014,1,2), 9.0));
        vals.push(vp(d(2020,1,6), 45.0));
        vals.sort_by_key(|v| v.date);

        let text = attribute(&bars, &vals, d(2014,1,1), d(2020,1,10)).unwrap().explain();
        assert!(text.contains("总回报"), "须报总回报");
        assert!(text.contains("裸价格"), "须把裸价格与总回报分开报，不可混用口径");
        assert!(text.contains("分红再投资"));
        assert!(text.contains("⚠️"), "低起点警告须出现在人话摘要里");
    }
}

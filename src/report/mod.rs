pub mod chart;
pub mod html;
pub mod compare;
pub mod optimize;

use crate::metrics;

/// Escape user-controlled strings before injecting them into HTML markup.
/// Always escape `&` first so subsequent replacements don't double-escape.
pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
     .replace('\'', "&#39;")
}
use crate::portfolio::Portfolio;

/// 生成可打印的指标摘要文本。
pub fn summary(pf: &Portfolio) -> String {
    let final_equity = pf.curve.last().map(|p| p.equity).unwrap_or(0.0);
    let total_ret = metrics::total_return(final_equity, pf.total_contributed);
    let mdd = metrics::max_drawdown(&pf.curve);
    let ann = metrics::xirr(&pf.flows).unwrap_or(0.0);
    let sharpe = metrics::sharpe(&pf.curve, 0.0);

    format!(
        "==== 回测结果 ====\n\
         累计投入   : {:.2}\n\
         期末市值   : {:.2}\n\
         总收益     : {:.2}%\n\
         年化(XIRR) : {:.2}%\n\
         最大回撤   : {:.2}%\n\
         夏普比率   : {:.2}\n\
         交易日数   : {}\n",
        pf.total_contributed,
        final_equity,
        total_ret * 100.0,
        ann * 100.0,
        mdd * 100.0,
        sharpe,
        pf.curve.len(),
    )
}

/// 打印摘要到 stdout。
pub fn print_summary(pf: &Portfolio) {
    println!("{}", summary(pf));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::Portfolio;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn summary_contains_key_metrics() {
        let mut pf = Portfolio::new(0.0);
        pf.seed(d(2024,1,1));
        pf.record_equity(d(2024,1,1), 0.0, 1.0); // 占位
        // 手工灌入一个简单曲线
        pf.curve.clear();
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2023,1,1),equity:100.0,contribution:100.0});
        pf.curve.push(crate::portfolio::EquityPoint{date:d(2024,1,1),equity:150.0,contribution:0.0});
        pf.total_contributed = 100.0;
        pf.flows = vec![(d(2023,1,1),-100.0),(d(2024,1,1),150.0)];
        let s = summary(&pf);
        assert!(s.contains("总收益"));
        assert!(s.contains("最大回撤"));
        assert!(s.contains("年化"));
    }
}

use chrono::NaiveDate;
use serde::Serialize;
use crate::portfolio::EquityPoint;

pub fn total_return(final_equity: f64, total_contributed: f64) -> f64 {
    if total_contributed > 0.0 { final_equity / total_contributed - 1.0 } else { 0.0 }
}

/// 权益曲线上的最大回撤（峰到谷的最大相对跌幅）。
pub fn max_drawdown(curve: &[EquityPoint]) -> f64 {
    let mut peak = f64::MIN;
    let mut mdd = 0.0;
    for p in curve {
        if p.equity > peak { peak = p.equity; }
        if peak > 0.0 {
            let dd = 1.0 - p.equity / peak;
            if dd > mdd { mdd = dd; }
        }
    }
    mdd
}

fn xnpv(rate: f64, flows: &[(NaiveDate, f64)], t0: NaiveDate) -> f64 {
    flows.iter().map(|(date, amt)| {
        let years = (*date - t0).num_days() as f64 / 365.0;
        amt / (1.0 + rate).powf(years)
    }).sum()
}

/// 货币加权年化收益（XIRR），二分法求根。无解返回 None。
pub fn xirr(flows: &[(NaiveDate, f64)]) -> Option<f64> {
    if flows.len() < 2 { return None; }
    let t0 = flows.iter().map(|(d, _)| *d).min()?;
    let (mut lo, mut hi) = (-0.9999_f64, 10.0_f64);
    let mut f_lo = xnpv(lo, flows, t0);
    let f_hi = xnpv(hi, flows, t0);
    if f_lo * f_hi > 0.0 { return None; } // 同号无法二分
    for _ in 0..200 {
        let mid = (lo + hi) / 2.0;
        let f_mid = xnpv(mid, flows, t0);
        if f_mid.abs() < 1e-7 { return Some(mid); }
        if f_lo * f_mid < 0.0 { hi = mid; } else { lo = mid; f_lo = f_mid; }
    }
    Some((lo + hi) / 2.0)
}

/// 年化夏普：日收益剔除当日外部投入对权益的抬升。
pub fn sharpe(curve: &[EquityPoint], rf_annual: f64) -> f64 {
    if curve.len() < 2 { return 0.0; }
    let mut rets = Vec::new();
    for w in curve.windows(2) {
        let prev = w[0].equity;
        let cur = w[1].equity;
        if prev > 0.0 {
            // 剔除当日新增投入，只看市值变动带来的收益
            rets.push((cur - w[1].contribution - prev) / prev);
        }
    }
    if rets.len() < 2 { return 0.0; }
    let n = rets.len() as f64;
    let mean = rets.iter().sum::<f64>() / n;
    let var = rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std = var.sqrt();
    if std < 1e-12 { return 0.0; }
    (mean * 252.0 - rf_annual) / (std * 252.0_f64.sqrt())
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub total_contributed: f64,
    pub final_equity: f64,
    pub total_return: f64,
    pub annualized: f64,
    pub max_drawdown: f64,
    pub sharpe: f64,
    pub trade_count: usize,
}

/// 从组合与成交数汇总绩效指标。
pub fn summarize(pf: &crate::portfolio::Portfolio, trade_count: usize) -> Summary {
    let final_equity = pf.curve.last().map(|p| p.equity).unwrap_or(0.0);
    Summary {
        total_contributed: pf.total_contributed,
        final_equity,
        total_return: total_return(final_equity, pf.total_contributed),
        annualized: xirr(&pf.flows).unwrap_or(0.0),
        max_drawdown: max_drawdown(&pf.curve),
        sharpe: sharpe(&pf.curve, 0.0),
        trade_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::EquityPoint;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn total_return_basic() {
        assert!((total_return(150.0, 100.0) - 0.5).abs() < 1e-9);
        assert!((total_return(100.0, 0.0)).abs() < 1e-9); // 防除零
    }

    #[test]
    fn max_drawdown_basic() {
        let curve = vec![
            EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0},
            EquityPoint{date:d(2024,1,2),equity:120.0,contribution:0.0},
            EquityPoint{date:d(2024,1,3),equity:90.0,contribution:0.0},
            EquityPoint{date:d(2024,1,4),equity:110.0,contribution:0.0},
        ];
        // 峰值120→谷底90，回撤 = 1 - 90/120 = 0.25
        assert!((max_drawdown(&curve) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn xirr_one_year_doubling() {
        // 年初投入100，一年后取回200 → 年化≈100%
        let flows = vec![(d(2023,1,1), -100.0), (d(2024,1,1), 200.0)];
        let r = xirr(&flows).unwrap();
        assert!((r - 1.0).abs() < 0.02);
    }

    #[test]
    fn sharpe_runs_on_curve() {
        let curve = vec![
            EquityPoint{date:d(2024,1,1),equity:100.0,contribution:100.0},
            EquityPoint{date:d(2024,1,2),equity:101.0,contribution:0.0},
            EquityPoint{date:d(2024,1,3),equity:102.0,contribution:0.0},
        ];
        let s = sharpe(&curve, 0.0);
        assert!(s.is_finite());
    }

    #[test]
    fn summarize_fields() {
        use crate::portfolio::Portfolio;
        let mut pf = Portfolio::new(0.0);
        pf.total_contributed = 2000.0;
        pf.curve = vec![
            EquityPoint{date:d(2024,1,1),equity:1000.0,contribution:1000.0},
            EquityPoint{date:d(2024,2,1),equity:2000.0,contribution:1000.0},
            EquityPoint{date:d(2024,2,15),equity:4000.0,contribution:0.0},
        ];
        pf.flows = vec![(d(2024,1,1),-1000.0),(d(2024,2,1),-1000.0),(d(2024,2,15),4000.0)];
        let s = summarize(&pf, 3);
        assert!((s.total_return - 1.0).abs() < 1e-9, "total_return should be 1.0 (100%)");
        assert!(s.max_drawdown >= 0.0 && s.max_drawdown < 0.5, "max_drawdown in valid range");
        assert_eq!(s.trade_count, 3);
        assert!((s.total_contributed - 2000.0).abs() < 1e-9);
        assert!((s.final_equity - 4000.0).abs() < 1e-9);
    }
}

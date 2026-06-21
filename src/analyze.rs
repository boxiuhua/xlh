use crate::data::NavPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Regime { Uptrend, Downtrend, Range }

#[derive(Debug, serde::Serialize)]
pub struct RegimeReport {
    pub regime: String,
    pub window: usize,
    pub window_return: f64,
    pub annualized_vol: f64,
    pub ma_short: f64,
    pub ma_long: f64,
    pub ma_relation: String,
    pub rec_strategy: String,
    pub rec_name: String,
    pub rationale: String,
}

pub struct RegimeParams {
    pub window: usize,
    pub up_threshold: f64,
    pub down_threshold: f64,
    pub ma_short: usize,
    pub ma_long: usize,
}

impl Default for RegimeParams {
    fn default() -> Self {
        Self { window: 120, up_threshold: 0.10, down_threshold: -0.10, ma_short: 20, ma_long: 60 }
    }
}

fn mean_tail(points: &[NavPoint], n: usize) -> f64 {
    let s = &points[points.len() - n..];
    s.iter().map(|p| p.acc_nav).sum::<f64>() / n as f64
}

fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 { return 0.0; }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// 基于 acc_nav 判定近 window 个交易日的行情形态并给策略建议。
pub fn detect_regime(points: &[NavPoint], p: &RegimeParams) -> anyhow::Result<RegimeReport> {
    let need = p.window.max(p.ma_long + 1);
    if points.len() < need {
        return Err(anyhow::anyhow!("数据不足: 需要至少 {} 个净值点，当前 {}", need, points.len()));
    }
    let w = &points[points.len() - p.window..];
    let window_return = w.last().unwrap().acc_nav / w.first().unwrap().acc_nav - 1.0;
    let ma_short = mean_tail(points, p.ma_short);
    let ma_long = mean_tail(points, p.ma_long);
    let eps = 0.005;
    let ma_relation = if ma_short > ma_long * (1.0 + eps) { "多头排列" }
        else if ma_short < ma_long * (1.0 - eps) { "空头排列" }
        else { "纠缠" };
    let mut rets = Vec::new();
    for win in w.windows(2) {
        if win[0].acc_nav > 0.0 { rets.push(win[1].acc_nav / win[0].acc_nav - 1.0); }
    }
    let annualized_vol = stdev(&rets) * (252_f64).sqrt();
    let regime = if window_return > p.up_threshold && ma_short > ma_long {
        Regime::Uptrend
    } else if window_return < p.down_threshold && ma_short < ma_long {
        Regime::Downtrend
    } else {
        Regime::Range
    };
    let (regime_cn, rec_strategy, rec_name, rationale) = match regime {
        Regime::Uptrend => ("上涨趋势", "smart_dca", "智能定投", "上涨趋势：顺势持有，频繁进出易踏空"),
        Regime::Downtrend => ("下跌趋势", "trend", "均线择时", "下跌趋势：趋势走坏空仓离场，避开长跌"),
        Regime::Range => ("震荡", "rsi", "RSI超买超卖", "震荡：区间内高抛低吸吃波动"),
    };
    Ok(RegimeReport {
        regime: regime_cn.to_string(),
        window: p.window,
        window_return,
        annualized_vol,
        ma_short,
        ma_long,
        ma_relation: ma_relation.to_string(),
        rec_strategy: rec_strategy.to_string(),
        rec_name: rec_name.to_string(),
        rationale: rationale.to_string(),
    })
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
    fn detects_uptrend() {
        // 130 点从 1.0 线性涨到 2.0
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "上涨趋势");
        assert_eq!(r.rec_strategy, "smart_dca");
        assert!(r.window_return > 0.10);
    }

    #[test]
    fn detects_downtrend() {
        let vals: Vec<f64> = (0..130).map(|i| 2.0 - i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "下跌趋势");
        assert_eq!(r.rec_strategy, "trend");
        assert!(r.window_return < -0.10);
    }

    #[test]
    fn detects_range() {
        // 在 1.00/1.01 间小幅交替 → 收益≈0、均线纠缠
        let vals: Vec<f64> = (0..130).map(|i| if i % 2 == 0 { 1.00 } else { 1.01 }).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "震荡");
        assert_eq!(r.rec_strategy, "rsi");
    }

    #[test]
    fn insufficient_data_errors() {
        let vals: Vec<f64> = (0..30).map(|_| 1.0).collect();
        let err = detect_regime(&series(&vals), &RegimeParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn volatility_positive_for_moving_series() {
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert!(r.annualized_vol > 0.0, "上涨序列应有正波动率");
    }
}

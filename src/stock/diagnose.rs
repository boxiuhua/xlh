use anyhow::{anyhow, Result};
use serde::Serialize;
use crate::stock::data::StockBar;
use crate::stock::indicators;

pub struct DiagnoseParams {
    pub ma_short: usize, pub ma_long: usize,
    pub boll_window: usize, pub boll_k: f64,
    pub rsi_period: usize,
    pub macd_fast: usize, pub macd_slow: usize, pub macd_signal: usize,
    pub trend_window: usize,
    pub up_threshold: f64, pub down_threshold: f64,
}

impl Default for DiagnoseParams {
    fn default() -> Self {
        Self {
            ma_short: 20, ma_long: 60,
            boll_window: 20, boll_k: 2.0,
            rsi_period: 14,
            macd_fast: 12, macd_slow: 26, macd_signal: 9,
            trend_window: 60,
            up_threshold: 0.10, down_threshold: -0.10,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StockDiagnosis {
    pub code: String, pub name: String, pub date: String,
    pub price: f64, pub adj_price: f64,
    pub ma_short: f64, pub ma_long: f64, pub ma_relation: String,
    pub macd: f64, pub macd_signal: f64, pub macd_hist: f64,
    pub boll_mid: f64, pub boll_upper: f64, pub boll_lower: f64, pub boll_z: f64,
    pub rsi: f64,
    pub trend: String,
    pub signal: String,
    pub score: i32,
    pub rationale: String,
    pub caveat: String,
    /// 这套信号在该股自身历史上到底有没有用（前瞻检验，无未来函数）。
    ///
    /// `diagnose()` 恒为 `None` —— 它必须保持纯粹，因为 `evidence::evaluate_signals`
    /// 会反过来调用它（每个时点重跑一遍），若在此处算证据就会无限递归。
    /// 要带证据请用 `diagnose_with_evidence()`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<crate::stock::evidence::SignalEvidence>,
}

/// 对单股 OHLCV 做技术诊断（基于后复权价）。纯函数。
pub fn diagnose(code: String, name: String, bars: &[StockBar], p: &DiagnoseParams) -> Result<StockDiagnosis> {
    let need = p.ma_long.max(p.boll_window).max(p.rsi_period + 1).max(p.macd_slow).max(p.trend_window);
    if bars.len() < need {
        return Err(anyhow!("数据不足: 诊断需要至少 {} 个交易日，当前 {}", need, bars.len()));
    }
    let adj: Vec<f64> = bars.iter().map(|b| b.adj_close).collect();
    let last = bars.last().unwrap();

    let ma_short = indicators::sma(&adj, p.ma_short).unwrap();
    let ma_long = indicators::sma(&adj, p.ma_long).unwrap();
    let eps = 0.005;
    let ma_relation = if ma_short > ma_long * (1.0 + eps) { "多头排列" }
        else if ma_short < ma_long * (1.0 - eps) { "空头排列" }
        else { "纠缠" };

    let m = indicators::macd(&adj, p.macd_fast, p.macd_slow, p.macd_signal).unwrap();
    let b = indicators::bollinger(&adj, p.boll_window, p.boll_k).unwrap();
    let rsi = indicators::rsi(&adj, p.rsi_period).unwrap();

    let price_adj = *adj.last().unwrap();
    let boll_z = if b.std > 1e-12 { (price_adj - b.mid) / b.std } else { 0.0 };

    // 趋势
    let w = &adj[adj.len() - p.trend_window..];
    let window_return = if w[0] > 0.0 { w[w.len() - 1] / w[0] - 1.0 } else { 0.0 };
    let trend = if window_return > p.up_threshold && ma_short > ma_long { "上涨" }
        else if window_return < p.down_threshold && ma_short < ma_long { "下跌" }
        else { "震荡" };

    // 综合打分（正=偏买）
    let boll_sig = if boll_z <= -2.0 { 2 } else if boll_z <= -1.0 { 1 }
        else if boll_z < 1.0 { 0 } else if boll_z < 2.0 { -1 } else { -2 };
    let rsi_sig = if rsi <= 30.0 { 1 } else if rsi >= 70.0 { -1 } else { 0 };
    let macd_sig = if m.hist > 0.0 { 1 } else if m.hist < 0.0 { -1 } else { 0 };
    let score = boll_sig + rsi_sig + macd_sig;
    let signal = if score >= 2 { "强力买入" } else if score == 1 { "买入" }
        else if score == 0 { "观望" } else if score == -1 { "卖出" } else { "强力卖出" };

    let rationale = format!(
        "趋势{trend}（近{}日{:+.1}%，{ma_relation}）；布林 z={:.2}；RSI={:.1}；MACD 柱={:+.4}",
        p.trend_window, window_return * 100.0, boll_z, rsi, m.hist);
    let caveat = match trend {
        "上涨" => "上涨趋势：顺势持有，回踩布林下轨可低吸，勿追高。",
        "下跌" => "下跌趋势：反弹至上轨谨慎减仓，抄底只在超卖小额试探。",
        _ => "震荡：区间内沿布林上下轨高抛低吸，按信号分档执行。",
    }.to_string();

    Ok(StockDiagnosis {
        code, name, date: last.date.to_string(),
        price: last.close, adj_price: price_adj,
        ma_short, ma_long, ma_relation: ma_relation.to_string(),
        macd: m.macd, macd_signal: m.signal, macd_hist: m.hist,
        boll_mid: b.mid, boll_upper: b.upper, boll_lower: b.lower, boll_z,
        rsi,
        trend: trend.to_string(),
        signal: signal.to_string(),
        score,
        rationale,
        caveat,
        evidence: None,   // 见字段文档：此处算证据会与 evaluate_signals 无限递归
    })
}

/// 诊断 + 该信号在这只股票自身历史上的前瞻检验。
///
/// 对外展示（Web / 推送）一律走这个，而不是裸 `diagnose()` ——
/// 给出「买入」并据此报出「加仓 1200 元」，就必须同时给出这个信号到底有没有用。
///
/// 实测（5 只 A 股，2.6 年 K线）：超额 −0.67% ~ +3.06%，**3 正 2 负**。
/// 既没被证伪（不同于基金的 ±σ 波动带，那个是一边倒的负超额、金额已移除），
/// 也远谈不上被证实：样本短、只数少、样本内、均值几乎全靠宁德时代一只撑着。
/// 所以金额保留，但必须把逐只的超额摆在用户面前，由他自己判断。
pub fn diagnose_with_evidence(
    code: String, name: String, bars: &[StockBar], p: &DiagnoseParams,
) -> Result<StockDiagnosis> {
    let mut d = diagnose(code, name, bars, p)?;
    d.evidence = crate::stock::evidence::evaluate_signals(bars, p, crate::stock::evidence::HORIZON);
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<StockBar> {
        vals.iter().enumerate().map(|(i, v)| StockBar {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            open: *v, high: *v, low: *v, close: *v, volume: 0.0, adj_close: *v,
        }).collect()
    }

    #[test]
    fn uptrend_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + i as f64).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "上涨");
        assert_eq!(dgn.ma_relation, "多头排列");
    }

    #[test]
    fn downtrend_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 200.0 - i as f64).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "下跌");
        assert_eq!(dgn.ma_relation, "空头排列");
    }

    #[test]
    fn range_detected() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + if i % 2 == 0 { 0.0 } else { 0.5 }).collect();
        let dgn = diagnose("600519".into(), "茅台".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert_eq!(dgn.trend, "震荡");
    }

    #[test]
    fn spike_down_last_point_leans_buy() {
        // 80 点窄幅震荡后末点深跌 → 布林 z 很负、RSI 低 → score 偏买
        let mut vals: Vec<f64> = (0..79).map(|i| 100.0 + if i % 2 == 0 { -0.5 } else { 0.5 }).collect();
        vals.push(80.0);
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert!(dgn.score > 0, "深跌末点应偏买: score={}", dgn.score);
        assert!(dgn.signal.contains("买入"), "信号应含买入: {}", dgn.signal);
    }

    #[test]
    fn spike_up_last_point_leans_sell() {
        let mut vals: Vec<f64> = (0..79).map(|i| 100.0 + if i % 2 == 0 { -0.5 } else { 0.5 }).collect();
        vals.push(140.0);
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        assert!(dgn.score < 0, "冲高末点应偏卖: score={}", dgn.score);
        assert!(dgn.signal.contains("卖出"), "信号应含卖出: {}", dgn.signal);
    }

    #[test]
    fn insufficient_data_errors() {
        let vals: Vec<f64> = (0..30).map(|_| 100.0).collect();
        let err = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn serializes_frontend_keys() {
        let vals: Vec<f64> = (0..80).map(|i| 100.0 + 0.1 * i as f64).collect();
        let dgn = diagnose("x".into(), "x".into(), &series(&vals), &DiagnoseParams::default()).unwrap();
        let j = serde_json::to_string(&dgn).unwrap();
        for key in ["\"trend\"", "\"signal\"", "\"macd_hist\"", "\"boll_z\"", "\"rsi\"", "\"ma_relation\"", "\"score\""] {
            assert!(j.contains(key), "JSON 应含 {key}");
        }
    }
}

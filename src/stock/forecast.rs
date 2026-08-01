//! A causal, heuristic directional-probability model for the stock diagnosis page.
use serde::Serialize;

use super::{data::StockBar, indicators};

const WARMUP: usize = 60;

#[derive(Debug, Clone, Default, Serialize)]
pub struct ForecastEvidence {
    pub samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_hit_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DirectionForecast {
    pub regime: String,
    pub up_probability_5d: f64,
    pub up_probability_20d: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_filter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relative_return_20d: Option<f64>,
    pub evidence_5d: ForecastEvidence,
    pub evidence_20d: ForecastEvidence,
    pub rationale: String,
    pub caveat: String,
}

fn score(prices: &[f64], market: Option<&[f64]>, horizon: usize) -> Option<(f64, &'static str)> {
    if prices.len() < WARMUP {
        return None;
    }
    let last = *prices.last()?;
    let ma20 = indicators::sma(prices, 20)?;
    let ma60 = indicators::sma(prices, 60)?;
    let macd = indicators::macd(prices, 12, 26, 9)?.hist;
    let rsi = indicators::rsi(prices, 14)?;
    let boll = indicators::bollinger(prices, 20, 2.0)?;
    let z = if boll.std > 1e-12 {
        (last - boll.mid) / boll.std
    } else {
        0.0
    };
    let r5 = last / prices[prices.len() - 6] - 1.0;
    let r20 = last / prices[prices.len() - 21] - 1.0;
    let trending = r20.abs() >= 0.08 && (ma20 - ma60).signum() == r20.signum();
    let sign = |v: f64| if v >= 0.0 { 1.0 } else { -1.0 };
    let mut s = if horizon <= 5 {
        let reversal = if rsi <= 30.0 || z <= -1.5 {
            1.0
        } else if rsi >= 70.0 || z >= 1.5 {
            -1.0
        } else {
            0.0
        };
        if trending {
            0.6 * sign(r5) + 0.4 * sign(macd) + 0.5 * sign(r20) + 0.5 * reversal
        } else {
            1.2 * reversal + 0.4 * sign(macd)
        }
    } else {
        1.2 * sign(r20) + sign(ma20 - ma60) + 0.7 * sign(macd) + 0.3 * sign(r5)
    };
    if let Some(market) = market.filter(|m| m.len() == prices.len() && m.len() >= 21) {
        let mr5 = market[market.len() - 1] / market[market.len() - 6] - 1.0;
        let mr20 = market[market.len() - 1] / market[market.len() - 21] - 1.0;
        // 相对强弱与市场方向均只使用当日及以前价格。市场偏弱时降低多头概率，
        // 个股跑赢市场时再适度加分，二者不会覆盖原有技术信号。
        let relative = if horizon <= 5 { r5 - mr5 } else { r20 - mr20 };
        s += 0.7 * sign(relative) + 0.4 * sign(if horizon <= 5 { mr5 } else { mr20 });
    }
    Some((s, if trending { "趋势" } else { "震荡" }))
}

fn probability(score: f64) -> f64 {
    (0.5 + 0.09 * score).clamp(0.15, 0.85)
}

fn evidence(
    prices: &[f64],
    market: Option<&[f64]>,
    horizon: usize,
    current_up: bool,
) -> ForecastEvidence {
    // 最后 30% 才计分。模型规则虽然没有拟合参数，仍明确保留一段未参与
    // 评价选择的时间，以免 UI 把全历史的样本内统计误读为预测能力。
    let test_start = (prices.len() * 7 / 10).max(WARMUP);
    let (mut hits, mut samples, mut baseline_hits, mut baseline_samples) =
        (0usize, 0usize, 0usize, 0usize);
    for end in test_start..prices.len().saturating_sub(horizon) {
        let market_prefix = market.map(|m| &m[..=end]);
        let Some((s, _)) = score(&prices[..=end], market_prefix, horizon) else {
            continue;
        };
        let predicted_up = s >= 0.0;
        let actual_up = prices[end + horizon] > prices[end];
        baseline_samples += 1;
        if actual_up == current_up {
            baseline_hits += 1;
        }
        if predicted_up != current_up {
            continue;
        }
        samples += 1;
        if actual_up == predicted_up {
            hits += 1;
        }
    }
    let hit_rate = (samples >= 10).then(|| hits as f64 / samples as f64);
    let baseline_hit_rate =
        (baseline_samples >= 10).then(|| baseline_hits as f64 / baseline_samples as f64);
    ForecastEvidence {
        samples,
        hit_rate,
        baseline_hit_rate,
        edge: hit_rate.zip(baseline_hit_rate).map(|(h, b)| h - b),
    }
}

pub fn forecast(bars: &[StockBar]) -> Option<DirectionForecast> {
    forecast_with_market(bars, None, None)
}

/// `market` 须已按交易日与个股对齐。行情缺失时调用方传 None，模型退化为
/// 基础技术版本而非拿错市场数据硬凑。
pub fn forecast_with_market(
    bars: &[StockBar],
    market: Option<&[StockBar]>,
    market_name: Option<&str>,
) -> Option<DirectionForecast> {
    let prices: Vec<f64> = bars.iter().map(|b| b.adj_close).collect();
    let market_prices: Option<Vec<f64>> = market
        .filter(|m| m.len() == bars.len() && m.iter().zip(bars).all(|(a, b)| a.date == b.date))
        .map(|m| m.iter().map(|b| b.adj_close).collect());
    let market_ref = market_prices.as_deref();
    let (s5, regime) = score(&prices, market_ref, 5)?;
    let (s20, _) = score(&prices, market_ref, 20)?;
    let p5 = probability(s5);
    let p20 = probability(s20);
    Some(DirectionForecast {
        regime: regime.into(), up_probability_5d: p5, up_probability_20d: p20,
        market_filter: market_ref.map(|m| {
            let r = m[m.len()-1] / m[m.len()-21] - 1.0;
            format!("{}近20日{:+.1}%", market_name.unwrap_or("市场"), r * 100.0)
        }),
        relative_return_20d: market_ref.map(|m| (prices[prices.len()-1] / prices[prices.len()-21] - 1.0) - (m[m.len()-1] / m[m.len()-21] - 1.0)),
        evidence_5d: evidence(&prices, market_ref, 5, p5 >= 0.5),
        evidence_20d: evidence(&prices, market_ref, 20, p20 >= 0.5),
        rationale: format!("5日模型分数 {s5:+.1}；20日模型分数 {s20:+.1}。趋势以均线、动量和 MACD 为主；震荡时提高 RSI 与布林位置权重{}。", if market_ref.is_some() { "，并叠加市场趋势与相对强弱" } else { "" }),
        caveat: "概率为规则模型的方向倾向，不是收益或价格预测；命中率仅统计最后 30% 样本外区间的同方向信号，并与该区间无模型方向基准比较。样本少于 10 次不显示。".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};
    fn bars(v: Vec<f64>) -> Vec<StockBar> {
        v.into_iter()
            .enumerate()
            .map(|(i, x)| StockBar {
                date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + Duration::days(i as i64),
                open: x,
                high: x,
                low: x,
                close: x,
                volume: 1.0,
                adj_close: x,
            })
            .collect()
    }
    #[test]
    fn rising_series_is_bullish_at_twenty_days() {
        assert!(
            forecast(&bars((0..160).map(|i| 100.0 + i as f64).collect()))
                .unwrap()
                .up_probability_20d
                > 0.5
        );
    }
    #[test]
    fn forecast_needs_warmup() {
        assert!(forecast(&bars(vec![10.0; 59])).is_none());
    }

    #[test]
    fn evidence_is_reported_only_for_the_holdout_period() {
        let f = forecast(&bars((0..200).map(|i| 100.0 + i as f64).collect())).unwrap();
        assert!(f.evidence_20d.samples <= 40, "只允许末 30% 进入样本外评价");
        assert!(f.evidence_20d.baseline_hit_rate.is_some());
    }

    #[test]
    fn market_relative_strength_is_used_when_dates_align() {
        let stock = bars((0..160).map(|i| 100.0 + i as f64 * 1.2).collect());
        let market = bars((0..160).map(|i| 100.0 + i as f64 * 0.3).collect());
        let f = forecast_with_market(&stock, Some(&market), Some("上证指数")).unwrap();
        assert!(f.market_filter.is_some());
        assert!(f.relative_return_20d.unwrap() > 0.0);
    }
}

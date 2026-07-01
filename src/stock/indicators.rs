#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Macd { pub macd: f64, pub signal: f64, pub hist: f64 }

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Boll { pub mid: f64, pub upper: f64, pub lower: f64, pub std: f64 }

pub fn sma(prices: &[f64], n: usize) -> Option<f64> {
    if n == 0 || prices.len() < n { return None; }
    Some(prices[prices.len() - n..].iter().sum::<f64>() / n as f64)
}

/// EMA 序列：seed=首值，alpha=2/(n+1)。空输入→空。
pub fn ema_series(prices: &[f64], n: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(prices.len());
    if prices.is_empty() || n == 0 { return out; }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut prev = prices[0];
    out.push(prev);
    for &p in &prices[1..] {
        prev += alpha * (p - prev);
        out.push(prev);
    }
    out
}

pub fn macd(prices: &[f64], fast: usize, slow: usize, signal: usize) -> Option<Macd> {
    if prices.len() < slow.max(1) { return None; }
    let ef = ema_series(prices, fast);
    let es = ema_series(prices, slow);
    let line: Vec<f64> = ef.iter().zip(&es).map(|(a, b)| a - b).collect();
    let sig = ema_series(&line, signal);
    let m = *line.last()?;
    let s = *sig.last()?;
    Some(Macd { macd: m, signal: s, hist: m - s })
}

pub fn bollinger(prices: &[f64], n: usize, k: f64) -> Option<Boll> {
    if n == 0 || prices.len() < n { return None; }
    let s = &prices[prices.len() - n..];
    let mid = s.iter().sum::<f64>() / n as f64;
    let var = s.iter().map(|x| (x - mid).powi(2)).sum::<f64>() / n as f64; // 总体方差
    let std = var.sqrt();
    Some(Boll { mid, upper: mid + k * std, lower: mid - k * std, std })
}

/// 简单均值口径 RSI：最近 n 个日变动的平均涨/跌幅。全涨→100，全跌→0。
pub fn rsi(prices: &[f64], n: usize) -> Option<f64> {
    if n == 0 || prices.len() < n + 1 { return None; }
    let start = prices.len() - n;
    let (mut gain, mut loss) = (0.0, 0.0);
    for i in start..prices.len() {
        let ch = prices[i] - prices[i - 1];
        if ch >= 0.0 { gain += ch; } else { loss -= ch; }
    }
    let avg_gain = gain / n as f64;
    let avg_loss = loss / n as f64;
    if avg_loss < 1e-12 { return Some(100.0); }
    let rs = avg_gain / avg_loss;
    Some(100.0 - 100.0 / (1.0 + rs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sma_latest_window() {
        assert_eq!(sma(&[1.0,2.0,3.0,4.0], 2), Some(3.5));
        assert_eq!(sma(&[1.0], 2), None);
        assert_eq!(sma(&[1.0,2.0], 0), None);
    }

    #[test]
    fn ema_series_seeds_first_and_tracks() {
        let e = ema_series(&[1.0, 2.0, 3.0], 2); // alpha=2/3
        assert_eq!(e.len(), 3);
        assert!((e[0] - 1.0).abs() < 1e-9, "seed=首值");
        // e1 = 1 + 2/3*(2-1) = 1.6667; e2 = 1.6667 + 2/3*(3-1.6667)=2.5556
        assert!((e[1] - 1.6666667).abs() < 1e-6);
        assert!((e[2] - 2.5555556).abs() < 1e-6);
        assert!(ema_series(&[], 2).is_empty());
    }

    #[test]
    fn macd_hist_positive_on_uptrend() {
        let prices: Vec<f64> = (0..60).map(|i| 100.0 + i as f64).collect();
        let m = macd(&prices, 12, 26, 9).unwrap();
        assert!(m.hist > 0.0, "上升序列 MACD 柱应为正: {m:?}");
    }

    #[test]
    fn macd_hist_negative_on_downtrend() {
        let prices: Vec<f64> = (0..60).map(|i| 200.0 - i as f64).collect();
        let m = macd(&prices, 12, 26, 9).unwrap();
        assert!(m.hist < 0.0, "下降序列 MACD 柱应为负: {m:?}");
    }

    #[test]
    fn macd_none_when_insufficient() {
        assert!(macd(&[1.0,2.0,3.0], 12, 26, 9).is_none());
    }

    #[test]
    fn bollinger_constant_series_zero_std() {
        let b = bollinger(&[5.0; 20], 20, 2.0).unwrap();
        assert!((b.std).abs() < 1e-9);
        assert!((b.mid - 5.0).abs() < 1e-9);
        assert!((b.upper - 5.0).abs() < 1e-9 && (b.lower - 5.0).abs() < 1e-9);
    }

    #[test]
    fn bollinger_bands_symmetric() {
        let prices: Vec<f64> = (1..=20).map(|i| i as f64).collect();
        let b = bollinger(&prices, 20, 2.0).unwrap();
        assert!((b.mid - 10.5).abs() < 1e-9, "1..20 均值=10.5");
        assert!((b.upper - b.mid - (b.mid - b.lower)).abs() < 1e-9, "上下带对称");
        assert!(bollinger(&prices, 21, 2.0).is_none());
    }

    #[test]
    fn rsi_all_up_is_100_all_down_is_0() {
        let up: Vec<f64> = (0..20).map(|i| 100.0 + i as f64).collect();
        assert!((rsi(&up, 14).unwrap() - 100.0).abs() < 1e-9);
        let down: Vec<f64> = (0..20).map(|i| 100.0 - i as f64).collect();
        assert!((rsi(&down, 14).unwrap()).abs() < 1e-9);
    }

    #[test]
    fn rsi_mixed_in_range_and_insufficient_none() {
        let mixed: Vec<f64> = (0..20).map(|i| 100.0 + if i % 2 == 0 { 1.0 } else { -0.8 } * i as f64).collect();
        let r = rsi(&mixed, 14).unwrap();
        assert!(r > 0.0 && r < 100.0, "混合序列 RSI 应在(0,100): {r}");
        assert!(rsi(&[1.0,2.0], 14).is_none());
    }
}

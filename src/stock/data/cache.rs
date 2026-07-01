use std::path::Path;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use super::StockBar;

pub fn write_csv(path: &Path, bars: &[StockBar]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from("date,open,high,low,close,volume,adj_close\n");
    for b in bars {
        s.push_str(&format!("{},{},{},{},{},{},{}\n", b.date, b.open, b.high, b.low, b.close, b.volume, b.adj_close));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<StockBar>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 7 { continue; }
        out.push(StockBar {
            date: NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?,
            open: c[1].parse()?, high: c[2].parse()?, low: c[3].parse()?,
            close: c[4].parse()?, volume: c[5].parse()?, adj_close: c[6].parse()?,
        });
    }
    Ok(out)
}

pub fn covers(bars: &[StockBar], start: NaiveDate, end: NaiveDate) -> bool {
    bars.iter().any(|b| b.date <= start) && bars.iter().any(|b| b.date >= end)
}

/// 有缓存且覆盖窗口则用缓存，否则抓取并写盘；最后按 [start,end] 过滤排序。
pub fn load_or_fetch(input: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> Result<Vec<StockBar>> {
    let secid = super::resolve_secid(input)?;
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    let mut bars = if path.exists() {
        let cached = read_csv(&path)?;
        if covers(&cached, start, end) {
            cached
        } else {
            let fresh = super::kline::fetch(&secid)?;
            write_csv(&path, &fresh)?;
            fresh
        }
    } else {
        let fresh = super::kline::fetch(&secid)?;
        write_csv(&path, &fresh)?;
        fresh
    };
    bars.retain(|b| b.date >= start && b.date <= end);
    bars.sort_by_key(|b| b.date);
    if bars.is_empty() { return Err(anyhow!("股票 {input} 在 {start}~{end} 无数据")); }
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, close: f64) -> StockBar {
        StockBar { date: dt, open: close, high: close, low: close, close, volume: 100.0, adj_close: close * 2.0 }
    }

    #[test]
    fn csv_roundtrip_preserves_ohlcv_and_adj() {
        let bars = vec![bar(d(2024,1,2), 110.0), bar(d(2024,1,3), 121.0)];
        let tmp = std::env::temp_dir().join("xlh_stock_cache_test.csv");
        write_csv(&tmp, &bars).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back.len(), 2);
        assert!((back[1].close - 121.0).abs() < 1e-9);
        assert!((back[1].adj_close - 242.0).abs() < 1e-9);
        assert!((back[0].volume - 100.0).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn covers_window() {
        let bars = vec![bar(d(2024,1,1), 1.0), bar(d(2024,6,1), 1.2)];
        assert!(covers(&bars, d(2024,1,1), d(2024,6,1)));
        assert!(covers(&bars, d(2024,2,1), d(2024,5,1)));
        assert!(!covers(&bars, d(2023,12,1), d(2024,6,1)));
        assert!(!covers(&bars, d(2024,1,1), d(2024,12,31)));
        assert!(!covers(&[], d(2024,1,1), d(2024,6,1)));
    }
}

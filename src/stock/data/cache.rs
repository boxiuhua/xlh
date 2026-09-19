use super::StockBar;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use std::path::Path;

pub fn write_csv(path: &Path, bars: &[StockBar]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut s = String::from("date,open,high,low,close,volume,adj_close\n");
    for b in bars {
        s.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            b.date, b.open, b.high, b.low, b.close, b.volume, b.adj_close
        ));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<StockBar>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split(',').collect();
        if c.len() < 7 {
            continue;
        }
        out.push(StockBar {
            date: NaiveDate::parse_from_str(c[0], "%Y-%m-%d")?,
            open: c[1].parse()?,
            high: c[2].parse()?,
            low: c[3].parse()?,
            close: c[4].parse()?,
            volume: c[5].parse()?,
            adj_close: c[6].parse()?,
        });
    }
    Ok(out)
}

pub fn covers(bars: &[StockBar], start: NaiveDate, end: NaiveDate) -> bool {
    bars.iter().any(|b| b.date <= start) && bars.iter().any(|b| b.date >= end)
}

/// 缓存可否直接使用:必须覆盖窗口;给了 `fresh_after` 时还必须写于该时刻之后。
/// 修改时间读不出来(`modified = None`)而又要求新鲜度时按不新鲜处理,宁可多抓一次。
pub fn cache_usable(
    covers: bool,
    modified: Option<NaiveDateTime>,
    fresh_after: Option<NaiveDateTime>,
) -> bool {
    covers
        && match fresh_after {
            None => true,
            Some(t) => modified.is_some_and(|m| m >= t),
        }
}

/// 缓存文件的最后修改时刻(本地时间);取不到返回 None。
fn modified_at(path: &Path) -> Option<NaiveDateTime> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(chrono::DateTime::<chrono::Local>::from(t).naive_local())
}

/// 有缓存且覆盖窗口则用缓存，否则抓取并写盘；最后按 [start,end] 过滤排序。
pub fn load_or_fetch(
    input: &str,
    cache_dir: &Path,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<StockBar>> {
    load(input, cache_dir, start, end, None)
}

/// 同 `load_or_fetch`,但缓存文件早于 `fresh_after` 写入时一律重抓。
///
/// 给收盘后的日线信号计算用:`.cache/stock` 是全程序共用的,盘中其它路径
/// (异动分类、诊断、推送)也会以 `end = 今天` 写它,而东财日 K 在盘中就带着
/// 当天未走完的那一根——只看「覆盖到今天」会把 10 点的价格当收盘价用。
pub fn load_or_fetch_fresh(
    input: &str,
    cache_dir: &Path,
    start: NaiveDate,
    end: NaiveDate,
    fresh_after: NaiveDateTime,
) -> Result<Vec<StockBar>> {
    load(input, cache_dir, start, end, Some(fresh_after))
}

fn load(
    input: &str,
    cache_dir: &Path,
    start: NaiveDate,
    end: NaiveDate,
    fresh_after: Option<NaiveDateTime>,
) -> Result<Vec<StockBar>> {
    let secid = super::resolve_secid(input)?;
    load_resolved(&secid, cache_dir, start, end, fresh_after)
}

/// 已解析好 secid 的加载入口(美股等需在线解析的代码由调用方先解析)。
/// `fresh_after` 语义同 `load_or_fetch_fresh`;传 `None` 等同 `load_or_fetch`。
pub fn load_resolved(
    secid: &super::secid::Secid,
    cache_dir: &Path,
    start: NaiveDate,
    end: NaiveDate,
    fresh_after: Option<NaiveDateTime>,
) -> Result<Vec<StockBar>> {
    load_resolved_with(
        secid,
        cache_dir,
        start,
        end,
        fresh_after,
        super::kline::fetch,
    )
}

/// `load_resolved` 的实现,抓取函数可注入(测试不联网)。
fn load_resolved_with(
    secid: &super::secid::Secid,
    cache_dir: &Path,
    start: NaiveDate,
    end: NaiveDate,
    fresh_after: Option<NaiveDateTime>,
    fetch: impl FnOnce(&super::secid::Secid) -> Result<Vec<StockBar>>,
) -> Result<Vec<StockBar>> {
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    let cached = if path.exists() {
        let cached = read_csv(&path)?;
        let modified = fresh_after.and(modified_at(&path));
        cache_usable(covers(&cached, start, end), modified, fresh_after).then_some(cached)
    } else {
        None
    };
    let mut bars = match cached {
        Some(c) => c,
        None => {
            let fresh = fetch(secid)?;
            write_csv(&path, &fresh)?;
            fresh
        }
    };
    bars.retain(|b| b.date >= start && b.date <= end);
    bars.sort_by_key(|b| b.date);
    if bars.is_empty() {
        return Err(anyhow!("股票 {} 在 {start}~{end} 无数据", secid.param()));
    }
    Ok(bars)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn bar(dt: NaiveDate, close: f64) -> StockBar {
        StockBar {
            date: dt,
            open: close,
            high: close,
            low: close,
            close,
            volume: 100.0,
            adj_close: close * 2.0,
        }
    }

    #[test]
    fn csv_roundtrip_preserves_ohlcv_and_adj() {
        let bars = vec![bar(d(2024, 1, 2), 110.0), bar(d(2024, 1, 3), 121.0)];
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
    fn cache_usable_requires_coverage_and_a_write_after_fresh_after() {
        let t = |h: u32, m: u32| d(2026, 9, 18).and_hms_opt(h, m, 0).unwrap();
        // 不要求新鲜度:与 load_or_fetch 原语义一致,只看覆盖
        assert!(cache_usable(true, None, None));
        assert!(!cache_usable(false, Some(t(16, 0)), None));
        // 要求收盘后写入:盘中写的缓存即便覆盖到今天也不可信
        assert!(!cache_usable(true, Some(t(10, 0)), Some(t(15, 5))));
        assert!(cache_usable(true, Some(t(15, 5)), Some(t(15, 5))));
        assert!(cache_usable(true, Some(t(15, 30)), Some(t(15, 5))));
        assert!(!cache_usable(false, Some(t(15, 30)), Some(t(15, 5))));
        // 读不出修改时间:当作不新鲜,宁可重抓
        assert!(!cache_usable(true, None, Some(t(15, 5))));
    }

    fn tmp_cache_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("xlh_stock_cache_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_resolved_refetches_stale_cache_when_fresh_after_is_later_than_mtime() {
        let dir = tmp_cache_dir("stale");
        let secid = super::super::secid::Secid {
            market: 1,
            code: "600000".into(),
        };
        let path = dir.join(format!("{}.csv", secid.cache_key()));
        // 缓存覆盖窗口,但写入时刻早于 fresh_after(模拟盘中写入、收盘后要求新鲜)
        write_csv(&path, &[bar(d(2024, 1, 2), 10.0), bar(d(2024, 1, 3), 11.0)]).unwrap();
        let fresh_after = modified_at(&path).unwrap() + chrono::Duration::hours(1);
        let mut fetched = false;
        let got = load_resolved_with(
            &secid,
            &dir,
            d(2024, 1, 2),
            d(2024, 1, 3),
            Some(fresh_after),
            |_| {
                fetched = true;
                Ok(vec![bar(d(2024, 1, 2), 10.0), bar(d(2024, 1, 3), 12.5)])
            },
        )
        .unwrap();
        assert!(fetched, "陈旧缓存应触发重抓");
        assert!((got[1].close - 12.5).abs() < 1e-9);
        // 重抓结果已写回缓存
        assert!((read_csv(&path).unwrap()[1].close - 12.5).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_resolved_uses_covering_cache_when_no_freshness_required_or_fresh() {
        let dir = tmp_cache_dir("fresh");
        let secid = super::super::secid::Secid {
            market: 0,
            code: "000001".into(),
        };
        let path = dir.join(format!("{}.csv", secid.cache_key()));
        write_csv(&path, &[bar(d(2024, 1, 2), 10.0), bar(d(2024, 1, 3), 11.0)]).unwrap();
        let mtime = modified_at(&path).unwrap();
        for fa in [None, Some(mtime - chrono::Duration::hours(1))] {
            let got = load_resolved_with(&secid, &dir, d(2024, 1, 2), d(2024, 1, 3), fa, |_| {
                panic!("缓存可用时不应联网抓取")
            })
            .unwrap();
            assert_eq!(got.len(), 2);
            assert!((got[1].close - 11.0).abs() < 1e-9);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn covers_window() {
        let bars = vec![bar(d(2024, 1, 1), 1.0), bar(d(2024, 6, 1), 1.2)];
        assert!(covers(&bars, d(2024, 1, 1), d(2024, 6, 1)));
        assert!(covers(&bars, d(2024, 2, 1), d(2024, 5, 1)));
        assert!(!covers(&bars, d(2023, 12, 1), d(2024, 6, 1)));
        assert!(!covers(&bars, d(2024, 1, 1), d(2024, 12, 31)));
        assert!(!covers(&[], d(2024, 1, 1), d(2024, 6, 1)));
    }
}

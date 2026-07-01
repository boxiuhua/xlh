use std::path::Path;
use serde::Serialize;
use super::StockBar;
use super::secid::Secid;

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub code: String,
    pub added: usize,
    pub total: usize,
    pub latest: Option<String>,
    pub error: Option<String>,
}

pub fn merge_incremental(cached: &[StockBar], fresh: Vec<StockBar>) -> (Vec<StockBar>, usize) {
    let mut fresh = fresh;
    fresh.sort_by_key(|b| b.date);
    let last = cached.last().map(|b| b.date);
    let new: Vec<StockBar> = fresh.into_iter()
        .filter(|b| last.is_none_or(|d| b.date > d))
        .collect();
    let mut merged = cached.to_vec();
    merged.extend(new.iter().copied());
    (merged, new.len())
}

fn sync_one(display: &str, secid: &Secid, cache_dir: &Path) -> SyncOutcome {
    let path = cache_dir.join(format!("{}.csv", secid.cache_key()));
    let cached = if path.exists() { super::cache::read_csv(&path).unwrap_or_default() } else { Vec::new() };
    let fresh = match super::kline::fetch(secid) {
        Ok(f) => f,
        Err(e) => return SyncOutcome {
            code: display.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|b| b.date.to_string()), error: Some(format!("抓取失败: {e}")),
        },
    };
    let (merged, added) = merge_incremental(&cached, fresh);
    if let Err(e) = super::cache::write_csv(&path, &merged) {
        return SyncOutcome {
            code: display.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|b| b.date.to_string()), error: Some(format!("写入失败: {e}")),
        };
    }
    let latest = merged.last().map(|b| b.date.to_string());
    SyncOutcome { code: display.to_string(), added, total: merged.len(), latest, error: None }
}

/// 同步单只：解析代码 → 增量抓取合并写回。
pub fn sync_stock(input: &str, cache_dir: &Path) -> SyncOutcome {
    match super::resolve_secid(input) {
        Ok(secid) => sync_one(input, &secid, cache_dir),
        Err(e) => SyncOutcome { code: input.to_string(), added: 0, total: 0, latest: None, error: Some(format!("代码解析失败: {e}")) },
    }
}

/// 同步全部：扫 cache_dir 下 *.csv（文件名为 cache_key）逐个同步。
pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome> {
    let mut keys: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("csv") {
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    keys.push(stem.to_string());
                }
            }
        }
    }
    keys.sort();
    keys.iter().filter_map(|k| Secid::from_cache_key(k).map(|s| sync_one(k, &s, cache_dir))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, c: f64) -> StockBar {
        StockBar { date: dt, open: c, high: c, low: c, close: c, volume: 0.0, adj_close: c }
    }

    #[test]
    fn appends_only_newer_points() {
        let cached = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.1)];
        let fresh = vec![bar(d(2024,1,15),1.05), bar(d(2024,2,1),1.1), bar(d(2024,3,1),1.2)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.last().unwrap().date, d(2024,3,1));
    }

    #[test]
    fn empty_cache_takes_all() {
        let (merged, added) = merge_incremental(&[], vec![bar(d(2024,1,1),1.0)]);
        assert_eq!(added, 1);
        assert_eq!(merged.len(), 1);
    }
}

use std::path::Path;
use serde::Serialize;
use crate::data::{NavPoint, cache, eastmoney};

/// 把 fresh 中「日期晚于 cached 最后一天」的点追加到 cached。
/// 返回 (合并后序列, 新增条数)。cached 为空 → fresh 全部计入。
pub fn merge_incremental(cached: &[NavPoint], fresh: Vec<NavPoint>) -> (Vec<NavPoint>, usize) {
    let mut fresh = fresh;
    fresh.sort_by_key(|p| p.date);
    let last = cached.last().map(|p| p.date);
    let new: Vec<NavPoint> = fresh.into_iter()
        .filter(|p| last.is_none_or(|d| p.date > d))
        .collect();
    let mut merged = cached.to_vec();
    merged.extend(new.iter().copied());
    (merged, new.len())
}

#[derive(Debug, Serialize)]
pub struct SyncOutcome {
    pub code: String,
    pub added: usize,
    pub total: usize,
    pub latest: Option<String>,
    pub error: Option<String>,
}

fn valid_code(code: &str) -> bool {
    !code.is_empty() && code.len() <= 12 && code.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 同步单只：校验→读旧缓存→fetch 全量→merge_incremental→写回→汇总。任何失败返回带 error 的 outcome。
pub fn sync_fund(code: &str, cache_dir: &Path) -> SyncOutcome {
    if !valid_code(code) {
        return SyncOutcome { code: code.to_string(), added: 0, total: 0, latest: None, error: Some("基金代码非法".into()) };
    }
    let path = cache_dir.join(format!("{code}.csv"));
    let cached = if path.exists() { cache::read_csv(&path).unwrap_or_default() } else { Vec::new() };
    let fresh = match eastmoney::fetch(code) {
        Ok(f) => f,
        Err(e) => return SyncOutcome {
            code: code.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|p| p.date.to_string()), error: Some(format!("抓取失败: {e}")),
        },
    };
    let (merged, added) = merge_incremental(&cached, fresh);
    if let Err(e) = cache::write_csv(&path, &merged) {
        return SyncOutcome {
            code: code.to_string(), added: 0, total: cached.len(),
            latest: cached.last().map(|p| p.date.to_string()), error: Some(format!("写入失败: {e}")),
        };
    }
    let latest = merged.last().map(|p| p.date.to_string());
    SyncOutcome { code: code.to_string(), added, total: merged.len(), latest, error: None }
}

/// 同步全部：扫 cache_dir 下 *.csv 取代码，逐个 sync_fund。
pub fn sync_all(cache_dir: &Path) -> Vec<SyncOutcome> {
    let mut codes: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("csv") {
                if let Some(stem) = p.file_stem().and_then(|x| x.to_str()) {
                    codes.push(stem.to_string());
                }
            }
        }
    }
    codes.sort();
    codes.iter().map(|c| sync_fund(c, cache_dir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn np(dt: NaiveDate, v: f64) -> NavPoint { NavPoint { date: dt, nav: v, acc_nav: v } }

    #[test]
    fn appends_only_newer_points() {
        let cached = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let fresh = vec![np(d(2024,1,15),1.05), np(d(2024,2,1),1.1), np(d(2024,3,1),1.2)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 1, "只有 2024-03-01 晚于缓存末日 2024-02-01");
        assert_eq!(merged.len(), 3);
        assert_eq!(merged.last().unwrap().date, d(2024,3,1));
    }

    #[test]
    fn no_new_points_when_fresh_all_old() {
        let cached = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let fresh = vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)];
        let (merged, added) = merge_incremental(&cached, fresh);
        assert_eq!(added, 0);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn empty_cache_takes_all_fresh() {
        let (merged, added) = merge_incremental(&[], vec![np(d(2024,1,1),1.0), np(d(2024,2,1),1.1)]);
        assert_eq!(added, 2);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn sync_fund_rejects_bad_code() {
        let o = sync_fund("../etc", Path::new(".cache"));
        assert!(o.error.is_some(), "非法代码应返回 error");
        assert_eq!(o.added, 0);
    }

    #[test]
    fn sync_all_empty_dir_is_empty() {
        let dir = std::env::temp_dir().join("xlh_sync_empty_test");
        std::fs::create_dir_all(&dir).unwrap();
        // 确保无 csv
        let out = sync_all(&dir);
        assert!(out.is_empty(), "空目录无可同步基金");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::path::Path;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use crate::data::{eastmoney, NavPoint};

pub fn write_csv(path: &Path, points: &[NavPoint]) -> Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).ok(); }
    let mut s = String::from("date,nav,acc_nav\n");
    for p in points {
        s.push_str(&format!("{},{},{}\n", p.date, p.nav, p.acc_nav));
    }
    std::fs::write(path, s).map_err(|e| anyhow!("写缓存失败: {e}"))?;
    Ok(())
}

pub fn read_csv(path: &Path) -> Result<Vec<NavPoint>> {
    let text = std::fs::read_to_string(path).map_err(|e| anyhow!("读缓存失败: {e}"))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 || line.trim().is_empty() { continue; } // 跳过表头
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 3 { continue; }
        let date = NaiveDate::parse_from_str(cols[0], "%Y-%m-%d")?;
        let nav: f64 = cols[1].parse()?;
        let acc_nav: f64 = cols[2].parse()?;
        out.push(NavPoint { date, nav, acc_nav });
    }
    Ok(out)
}

/// Returns true when `points` contains at least one data point at or before `start`
/// and at least one data point at or after `end`, meaning the cache fully covers the
/// requested [start, end] window.
pub fn covers(points: &[NavPoint], start: NaiveDate, end: NaiveDate) -> bool {
    let has_start = points.iter().any(|p| p.date <= start);
    let has_end   = points.iter().any(|p| p.date >= end);
    has_start && has_end
}

/// 有缓存读缓存，否则抓取并写缓存；若缓存不覆盖请求窗口则重新抓取；最后按 [start,end] 过滤并排序。
pub fn load_or_fetch(code: &str, cache_dir: &Path, start: NaiveDate, end: NaiveDate) -> Result<Vec<NavPoint>> {
    let path = cache_dir.join(format!("{code}.csv"));
    let mut points = if path.exists() {
        let cached = read_csv(&path)?;
        if covers(&cached, start, end) {
            cached
        } else {
            // Stale cache: doesn't span the requested window — re-fetch and overwrite.
            let fresh = eastmoney::fetch(code)?;
            write_csv(&path, &fresh)?;
            fresh
        }
    } else {
        let p = eastmoney::fetch(code)?;
        write_csv(&path, &p)?;
        p
    };
    points.retain(|p| p.date >= start && p.date <= end);
    points.sort_by_key(|p| p.date);
    if points.is_empty() {
        return Err(anyhow!("基金 {code} 在 {start}~{end} 无净值数据"));
    }
    Ok(points)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::NavPoint;
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    #[test]
    fn csv_roundtrip() {
        let pts = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,1,2),nav:1.1,acc_nav:1.2},
        ];
        let tmp = std::env::temp_dir().join("xlh_cache_test.csv");
        write_csv(&tmp, &pts).unwrap();
        let back = read_csv(&tmp).unwrap();
        assert_eq!(back.len(), 2);
        assert!((back[1].acc_nav - 1.2).abs() < 1e-9);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn cache_coverage_check() {
        // Points span 2024-01-01 to 2024-06-01.
        let pts = vec![
            NavPoint{date:d(2024,1,1),nav:1.0,acc_nav:1.0},
            NavPoint{date:d(2024,3,1),nav:1.1,acc_nav:1.1},
            NavPoint{date:d(2024,6,1),nav:1.2,acc_nav:1.2},
        ];

        // Request fully inside cached range → covers returns true.
        assert!(covers(&pts, d(2024,1,1), d(2024,6,1)));
        assert!(covers(&pts, d(2024,2,1), d(2024,5,1)));

        // Request extends before cached start → covers returns false.
        assert!(!covers(&pts, d(2023,12,1), d(2024,6,1)));

        // Request extends after cached end → covers returns false.
        assert!(!covers(&pts, d(2024,1,1), d(2024,12,31)));

        // Empty points → covers returns false.
        assert!(!covers(&[], d(2024,1,1), d(2024,6,1)));
    }
}

//! 联网冒烟：验证东财三市场连通性。默认 #[ignore]，手动跑：
//!   cargo test --test stock_smoke -- --ignored --nocapture
use chrono::NaiveDate;
use xlh::stock::data::{cache, resolve_secid};

fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }

#[test]
#[ignore]
fn a_share_live() {
    let tmp = std::env::temp_dir().join("xlh_smoke_a");
    let bars = cache::load_or_fetch("600519", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "A股应有数据");
    println!("A股 600519: {} 条, 末日 {}", bars.len(), bars.last().unwrap().date);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore]
fn hk_live() {
    let tmp = std::env::temp_dir().join("xlh_smoke_hk");
    let bars = cache::load_or_fetch("00700", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "港股应有数据");
    println!("港股 00700: {} 条", bars.len());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore]
fn us_live() {
    // 关键风险验证：美股 secid 解析 + K线抓取
    let secid = resolve_secid("AAPL").unwrap();
    println!("美股 AAPL 解析为 secid {}", secid.param());
    let tmp = std::env::temp_dir().join("xlh_smoke_us");
    let bars = cache::load_or_fetch("AAPL", &tmp, d(2024,1,1), d(2024,6,30)).unwrap();
    assert!(!bars.is_empty(), "美股应有数据");
    println!("美股 AAPL: {} 条", bars.len());
    let _ = std::fs::remove_dir_all(&tmp);
}

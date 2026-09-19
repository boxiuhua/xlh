use chrono::{Duration, NaiveDate};
use xlh::stock::{
    data::StockBar,
    diagnose::{diagnose_with_evidence, DiagnoseParams},
};

#[test]
fn us_diagnosis_labels_currency_and_unverified_adjustment() {
    let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let bars: Vec<_> = (0..180)
        .map(|i| {
            let close = 100.0 + i as f64 * 0.1 + (i as f64 / 5.0).sin();
            StockBar {
                date: start + Duration::days(i),
                open: close,
                high: close + 1.0,
                low: close - 1.0,
                close,
                volume: 1000.0,
                adj_close: close,
            }
        })
        .collect();
    let result = diagnose_with_evidence(
        "us.AAPL".into(),
        "Apple".into(),
        &bars,
        &DiagnoseParams::default(),
    )
    .unwrap();
    assert_eq!(result.currency, "USD");
    assert_eq!(result.market, "美股");
    assert!(result.market_note.contains("非实时"));
    assert!(result.price_basis.contains("未确认复权"));
    assert!(result.forecast.is_some());
    assert!(result.evidence.is_some());
    for (code, market, currency) in [
        ("600519", "沪深 · 沪市", "CNY"),
        ("000001", "沪深 · 深市", "CNY"),
        ("00700", "港股", "HKD"),
    ] {
        let result =
            diagnose_with_evidence(code.into(), code.into(), &bars, &DiagnoseParams::default())
                .unwrap();
        assert_eq!(result.market, market);
        assert_eq!(result.currency, currency);
    }
}

/// Manual smoke check; preserves downloaded prices and analysis for inspection.
#[test]
#[ignore = "requires live market data"]
fn live_us_analysis_preserves_downloaded_history() {
    use xlh::stock::data::cache;
    let dir = std::path::Path::new("data/us_stock_analysis");
    let end = chrono::Local::now().date_naive();
    for ticker in ["AAPL", "NVDA", "TSLA"] {
        let bars = cache::load_or_fetch(ticker, dir, end - Duration::days(800), end).unwrap();
        let result = diagnose_with_evidence(
            ticker.into(),
            ticker.into(),
            &bars,
            &DiagnoseParams::default(),
        )
        .unwrap();
        assert_eq!(result.currency, "USD");
        assert!(result.price > 0.0);
        std::fs::write(
            dir.join(format!("{ticker}_analysis.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
        println!(
            "{ticker}: {} bars, as of {}, USD {}",
            bars.len(),
            result.date,
            result.price
        );
    }
}

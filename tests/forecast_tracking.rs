use chrono::{DateTime, Duration, NaiveDate, Utc};
use rusqlite::{params, Connection};
use xlh::stock::{
    data::{secid::Secid, StockBar},
    forecast, forecast_log as log,
    realtime::{outcomes, store},
};

fn day(i: i64) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 1, 1).unwrap() + Duration::days(i)
}
fn now(i: i64) -> DateTime<Utc> {
    day(i).and_hms_opt(12, 0, 0).unwrap().and_utc()
}
fn secid() -> Secid {
    Secid {
        market: 1,
        code: "600519".into(),
    }
}
fn bars(n: usize, adjusted: bool) -> Vec<StockBar> {
    (0..n)
        .map(|i| {
            let p = 100.0 + i as f64;
            StockBar {
                date: day(i as i64),
                open: p,
                high: p,
                low: p,
                close: p,
                adj_close: p * if adjusted { 2.0 } else { 1.0 },
                volume: 10.0,
            }
        })
        .collect()
}
fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    log::migrate(&c).unwrap();
    c
}
fn insert(c: &mut Connection, b: &[StockBar], time: DateTime<Utc>) -> String {
    let mut f = forecast::forecast(b).unwrap();
    f.up_probability_5d = 0.7;
    f.up_probability_20d = 0.6;
    log::record(c, &secid(), b, &f, time).unwrap()
}

#[test]
fn first_forecast_is_immutable_and_empty_statistics_are_unknown() {
    let mut c = db();
    let b = bars(80, true);
    let version = insert(&mut c, &b, now(80));
    let mut f = forecast::forecast(&b).unwrap();
    f.up_probability_5d = 0.2;
    log::record(&mut c, &secid(), &b, &f, now(81)).unwrap();
    let (n, p): (i64, f64) = c
        .query_row(
            "SELECT COUNT(*),MIN(probability) FROM forecast_records WHERE horizon=5",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(p, 0.7);
    let report = log::report(&c, &secid(), &version, now(81)).unwrap();
    assert_eq!(report.horizons[0].pending, 1);
    assert!(report.horizons[0].hit_rate.is_none());
}

#[test]
fn forward_verification_uses_future_endpoint_and_preserves_settled_results() {
    let mut c = db();
    let version = insert(&mut c, &bars(80, true), now(80));
    assert_eq!(
        log::verify(&mut c, &secid(), &bars(84, true), now(84)).unwrap(),
        0
    );
    assert_eq!(
        log::verify(&mut c, &secid(), &bars(86, true), now(86)).unwrap(),
        1
    );
    let report = log::report(&c, &secid(), &version, now(86)).unwrap();
    let h = &report.horizons[0];
    assert_eq!(h.samples, 1);
    assert_eq!(h.hit_rate, Some(1.0));
    assert!((h.brier.unwrap() - 0.09).abs() < 1e-10);
    assert_eq!(h.baseline_brier, Some(0.0));
    assert_eq!(h.calibration[3].samples, 1);
    assert_eq!(h.calibration[3].actual_up_rate, Some(1.0));
    assert_eq!(report.horizons[1].pending, 1);
    let before: String = c
        .query_row(
            "SELECT target_date FROM forecast_records WHERE horizon=5",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut changed = bars(87, true);
    changed[84].adj_close = 1.0;
    assert_eq!(log::verify(&mut c, &secid(), &changed, now(87)).unwrap(), 0);
    let after: String = c
        .query_row(
            "SELECT target_date FROM forecast_records WHERE horizon=5",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
}

#[test]
fn stale_origins_revised_prices_and_unverified_adjustments_are_not_scored() {
    for (adjusted, created, revised, status) in [
        (true, 84, false, "excluded"),
        (true, 80, true, "excluded"),
        (false, 80, false, "provisional"),
    ] {
        let mut c = db();
        let version = insert(&mut c, &bars(80, adjusted), now(created));
        let mut fresh = bars(86, adjusted);
        if revised {
            fresh[79].adj_close += 1.0;
        }
        log::verify(&mut c, &secid(), &fresh, now(86)).unwrap();
        let saved: String = c
            .query_row(
                "SELECT status FROM forecast_records WHERE horizon=5",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(saved, status);
        assert_eq!(
            log::report(&c, &secid(), &version, now(86))
                .unwrap()
                .horizons[0]
                .samples,
            0
        );
    }
}

#[test]
fn intraday_invalid_and_duplicate_bars_are_rejected() {
    let mut c = db();
    let b = bars(80, true);
    let f = forecast::forecast(&b).unwrap();
    assert!(log::record(&mut c, &secid(), &b, &f, now(79)).is_err());
    assert_eq!(log::completed_bars(&bars(82, true), now(80)).len(), 80);
    let mut b = b;
    b[79].date = b[78].date;
    assert!(log::record(&mut c, &secid(), &b, &f, now(80)).is_err());
}

#[test]
fn audit_repairs_same_source_labels_without_deleting_or_repeating() {
    let c = store::open_in_memory().unwrap();
    let dates = [
        "2026-07-16",
        "2026-07-17",
        "2026-07-20",
        "2026-07-21",
        "2026-07-22",
        "2026-07-23",
    ];
    let epoch = |date: &str, h| {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    };
    for (i, date) in dates.iter().enumerate() {
        c.execute(
            "INSERT INTO ticks VALUES ('600519',?1,?2,0,1,1,1,1)",
            params![epoch(date, 15), 11.0 + i as f64],
        )
        .unwrap();
    }
    c.execute("INSERT INTO signals(code,name,ts,trigger_price,jump_pct,vol_surge_x,divergence,horizon_tag,baseline,close_ret) VALUES ('600519','test',?1,10,0.1,3,'none','short','history',0.99)",[epoch(dates[0],10)]).unwrap();
    let through = NaiveDate::parse_from_str(dates[5], "%Y-%m-%d").unwrap();
    let report = outcomes::repair(&c, through).unwrap();
    assert_eq!((report.close, report.t1, report.t5), (1, 1, 1));
    let (close, t1, t5): (f64, f64, f64) = c
        .query_row("SELECT close_ret,ret_t1,ret_t5 FROM signals", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert!((close - 0.1).abs() < 1e-10);
    assert!((t1 - 0.2).abs() < 1e-10);
    assert!((t5 - 0.6).abs() < 1e-10);
    let old: f64 = c
        .query_row(
            "SELECT old_return FROM signal_outcome_audit WHERE horizon=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old, 0.99);
    let again = outcomes::repair(&c, through).unwrap();
    assert_eq!((again.close, again.t1, again.t5), (0, 0, 0));
    let ticks: i64 = c
        .query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ticks, 6);
    let audits: i64 = c
        .query_row("SELECT COUNT(*) FROM signal_outcome_audit", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(audits, 3);
}

#[test]
fn missing_close_snapshot_leaves_labels_unknown() {
    let c = store::open_in_memory().unwrap();
    let ts = day(0).and_hms_opt(10, 0, 0).unwrap().and_utc().timestamp();
    c.execute("INSERT INTO ticks VALUES ('600519',?1,10,0,1,1,1,1)", [ts])
        .unwrap();
    c.execute("INSERT INTO signals(code,name,ts,trigger_price,jump_pct,vol_surge_x,divergence,horizon_tag,baseline) VALUES ('600519','test',?1,10,0.1,3,'none','short','history')",[ts]).unwrap();
    outcomes::repair(&c, day(10)).unwrap();
    let label: Option<f64> = c
        .query_row("SELECT close_ret FROM signals", [], |r| r.get(0))
        .unwrap();
    assert!(label.is_none());
}

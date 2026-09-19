//! Immutable forward forecasts, separate from retrospective backtests.
use anyhow::{ensure, Result};
use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::path::Path;

use super::{
    data::{secid::Secid, StockBar},
    forecast::DirectionForecast,
};

pub const MODEL_VERSION: &str = "direction-rules-v1";

pub fn default_path() -> std::path::PathBuf {
    super::realtime::config::get()
        .db_path
        .with_file_name("forecast.db")
}

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS forecast_inputs (
      id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, as_of TEXT NOT NULL,
      model_version TEXT NOT NULL, created_at TEXT NOT NULL, created_day TEXT NOT NULL,
      price_basis TEXT NOT NULL, input_json TEXT NOT NULL,
      UNIQUE(symbol,as_of,model_version));
    CREATE TABLE IF NOT EXISTS forecast_records (
      id INTEGER PRIMARY KEY, input_id INTEGER NOT NULL REFERENCES forecast_inputs(id),
      horizon INTEGER NOT NULL CHECK(horizon IN (5,20)), probability REAL NOT NULL,
      baseline_probability REAL NOT NULL, origin_price REAL NOT NULL,
      status TEXT NOT NULL DEFAULT 'pending', target_date TEXT, target_price REAL,
      actual_return REAL, verified_at TEXT, note TEXT,
      UNIQUE(input_id,horizon));
    CREATE INDEX IF NOT EXISTS forecast_pending ON forecast_records(status,input_id);
    CREATE TABLE IF NOT EXISTS forecast_price_batches (
      id INTEGER PRIMARY KEY, symbol TEXT NOT NULL, fetched_at TEXT NOT NULL, bars_json TEXT NOT NULL);")?;
    Ok(())
}

/// Exclude today's date in every market. Conservative UTC boundary deliberately
/// delays fresh daily bars rather than treating an intraday candle as a close.
pub fn completed_bars(bars: &[StockBar], now: DateTime<Utc>) -> Vec<StockBar> {
    bars.iter()
        .filter(|b| b.date < now.date_naive())
        .copied()
        .collect()
}

fn valid_bars(bars: &[StockBar]) -> bool {
    !bars.is_empty()
        && bars.iter().all(|b| {
            b.close.is_finite() && b.close > 0.0 && b.adj_close.is_finite() && b.adj_close > 0.0
        })
        && bars.windows(2).all(|w| w[0].date < w[1].date)
}

/// First prediction for a symbol/date/model wins; refreshes cannot rewrite it.
pub fn record(
    conn: &mut Connection,
    secid: &Secid,
    bars: &[StockBar],
    forecast: &DirectionForecast,
    now: DateTime<Utc>,
) -> Result<String> {
    ensure!(valid_bars(bars), "预测行情无效或日期未严格递增");
    let last = bars.last().unwrap();
    ensure!(last.date < now.date_naive(), "仅登记已结束日期的日线预测");
    let version = format!(
        "{MODEL_VERSION}/{}",
        if forecast.market_filter.is_some() {
            "market"
        } else {
            "standalone"
        }
    );
    let basis = if bars.iter().any(|b| (b.close - b.adj_close).abs() > 1e-8) {
        "provider_adjusted"
    } else {
        "unverified"
    };
    let input = serde_json::json!({"bars":bars.iter().map(|b| serde_json::json!({"date":b.date,"close":b.close,"adj_close":b.adj_close})).collect::<Vec<_>>(),"forecast":forecast});
    let tx = conn.transaction()?;
    tx.execute("INSERT OR IGNORE INTO forecast_inputs(symbol,as_of,model_version,created_at,created_day,price_basis,input_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![secid.param(), last.date.to_string(), version, now.to_rfc3339(), now.date_naive().to_string(), basis, input.to_string()])?;
    let id: i64 = tx.query_row(
        "SELECT id FROM forecast_inputs WHERE symbol=?1 AND as_of=?2 AND model_version=?3",
        params![secid.param(), last.date.to_string(), version],
        |r| r.get(0),
    )?;
    for (h, p) in [
        (5, forecast.up_probability_5d),
        (20, forecast.up_probability_20d),
    ] {
        ensure!(p.is_finite() && (0.0..=1.0).contains(&p), "预测概率无效");
        let returns: Vec<_> = bars
            .windows(h + 1)
            .map(|w| w[h].adj_close > w[0].adj_close)
            .collect();
        let base = if returns.is_empty() {
            0.5
        } else {
            returns.iter().filter(|&&v| v).count() as f64 / returns.len() as f64
        };
        tx.execute("INSERT OR IGNORE INTO forecast_records(input_id,horizon,probability,baseline_probability,origin_price) VALUES (?1,?2,?3,?4,?5)", params![id,h as i64,p,base,last.adj_close])?;
    }
    tx.commit()?;
    Ok(version)
}

/// Settle only once, using origin and endpoint from one current daily series.
/// Revised historical inputs and late/backdated forecasts never enter scoring.
pub fn verify(
    conn: &mut Connection,
    secid: &Secid,
    bars: &[StockBar],
    now: DateTime<Utc>,
) -> Result<usize> {
    let bars = completed_bars(bars, now);
    if bars.is_empty() {
        return Ok(0);
    }
    ensure!(valid_bars(&bars), "核验行情无效或日期重复");
    let pending = {
        let mut stmt=conn.prepare("SELECT r.id,i.as_of,i.created_day,i.price_basis,r.horizon,r.origin_price,i.input_json FROM forecast_records r JOIN forecast_inputs i ON r.input_id=i.id WHERE i.symbol=?1 AND r.status='pending'")?;
        let rows = stmt.query_map([secid.param()], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, usize>(4)?,
                r.get::<_, f64>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let tx = conn.transaction()?;
    let mut changed = 0;
    for (id, day, created, basis, horizon, origin, input) in pending {
        let origin_day = NaiveDate::parse_from_str(&day, "%Y-%m-%d")?;
        let Some(index) = bars.iter().position(|b| b.date == origin_day) else {
            continue;
        };
        let Some(target) = bars.get(index + horizon) else {
            continue;
        };
        let created_day = NaiveDate::parse_from_str(&created, "%Y-%m-%d")?;
        let snapshot: serde_json::Value = serde_json::from_str(&input)?;
        let revised = snapshot["bars"].as_array().unwrap().iter().any(|old| {
            bars.iter()
                .find(|b| Some(b.date.to_string().as_str()) == old["date"].as_str())
                .is_some_and(|b| {
                    (b.adj_close - old["adj_close"].as_f64().unwrap()).abs() > 1e-7
                        || (b.close - old["close"].as_f64().unwrap()).abs() > 1e-7
                })
        });
        let (status, note) = if bars[index + 1].date < created_day {
            (
                "excluded",
                "登记时已有后续历史日线，起点滞后，不属于严格前瞻预测",
            )
        } else if revised {
            (
                "excluded",
                "历史价格已修订或复权尺度变化，保留原输入，不混算收益",
            )
        } else if basis == "unverified" {
            ("provisional", "复权未确认，仅保留暂定结果，不计入正式指标")
        } else {
            ("verified", "同一批复权日线核验；期限按随后可用交易记录计数")
        };
        let actual = target.adj_close / origin - 1.0;
        tx.execute("UPDATE forecast_records SET status=?1,target_date=?2,target_price=?3,actual_return=?4,verified_at=?5,note=?6 WHERE id=?7 AND status='pending'",params![status,target.date.to_string(),target.adj_close,actual,now.to_rfc3339(),note,id])?;
        changed += 1;
    }
    tx.commit()?;
    Ok(changed)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CalibrationBin {
    pub lower: f64,
    pub upper: f64,
    pub samples: usize,
    pub mean_probability: Option<f64>,
    pub actual_up_rate: Option<f64>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct HorizonStats {
    pub horizon: usize,
    pub pending: usize,
    pub excluded: usize,
    pub provisional: usize,
    pub samples: usize,
    pub origin_dates: usize,
    pub hit_rate: Option<f64>,
    pub baseline_hit_rate: Option<f64>,
    pub always_up_hit_rate: Option<f64>,
    pub brier: Option<f64>,
    pub baseline_brier: Option<f64>,
    pub calibration: Vec<CalibrationBin>,
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrackingReport {
    pub model_version: String,
    pub window_days: usize,
    pub horizons: Vec<HorizonStats>,
    pub note: String,
    pub recent: Vec<PredictionRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PredictionRow {
    pub created_at: String,
    pub as_of: String,
    pub horizon: usize,
    pub probability: f64,
    pub status: String,
    pub target_date: Option<String>,
    pub actual_return: Option<f64>,
    pub note: Option<String>,
}

pub fn report(
    conn: &Connection,
    secid: &Secid,
    version: &str,
    now: DateTime<Utc>,
) -> Result<TrackingReport> {
    let since = (now.date_naive() - chrono::Duration::days(180)).to_string();
    let mut stats = Vec::new();
    for horizon in [5, 20] {
        let mut s = HorizonStats {
            horizon,
            calibration: (0..5)
                .map(|n| CalibrationBin {
                    lower: n as f64 / 5.0,
                    upper: (n + 1) as f64 / 5.0,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let mut stmt=conn.prepare("SELECT r.status,r.probability,r.baseline_probability,r.actual_return,i.as_of FROM forecast_records r JOIN forecast_inputs i ON i.id=r.input_id WHERE i.symbol=?1 AND i.model_version=?2 AND i.created_day>=?3 AND r.horizon=?4")?;
        let rows = stmt.query_map(
            params![secid.param(), version, since, horizon as i64],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        )?;
        let (mut hits, mut base_hits, mut ups, mut brier, mut base_brier) =
            (0.0, 0.0, 0.0, 0.0, 0.0);
        let mut dates = std::collections::HashSet::new();
        let mut sums = [(0.0, 0.0); 5];
        for row in rows {
            let (status, p, base, actual, date) = row?;
            match status.as_str() {
                "pending" => {
                    s.pending += 1;
                    continue;
                }
                "excluded" => {
                    s.excluded += 1;
                    continue;
                }
                "provisional" => {
                    s.provisional += 1;
                    continue;
                }
                "verified" => {}
                _ => continue,
            }
            let Some(actual) = actual else {
                continue;
            };
            let up = actual > 0.0;
            let y = if up { 1.0 } else { 0.0 };
            s.samples += 1;
            dates.insert(date);
            ups += y;
            hits += if (p >= 0.5) == up { 1.0 } else { 0.0 };
            base_hits += if (base >= 0.5) == up { 1.0 } else { 0.0 };
            brier += (p - y).powi(2);
            base_brier += (base - y).powi(2);
            let bin = ((p * 5.0).floor() as usize).min(4);
            s.calibration[bin].samples += 1;
            sums[bin].0 += p;
            sums[bin].1 += y;
        }
        s.origin_dates = dates.len();
        if s.samples > 0 {
            let n = s.samples as f64;
            s.hit_rate = Some(hits / n);
            s.baseline_hit_rate = Some(base_hits / n);
            s.always_up_hit_rate = Some(ups / n);
            s.brier = Some(brier / n);
            s.baseline_brier = Some(base_brier / n);
        }
        for (bin, (ps, ys)) in s.calibration.iter_mut().zip(sums) {
            if bin.samples > 0 {
                bin.mean_probability = Some(ps / bin.samples as f64);
                bin.actual_up_rate = Some(ys / bin.samples as f64);
            }
        }
        stats.push(s);
    }
    let recent = {
        let mut stmt=conn.prepare("SELECT i.created_at,i.as_of,r.horizon,r.probability,r.status,r.target_date,r.actual_return,r.note FROM forecast_records r JOIN forecast_inputs i ON i.id=r.input_id WHERE i.symbol=?1 AND i.model_version=?2 ORDER BY i.created_at DESC,r.horizon LIMIT 20")?;
        let rows = stmt.query_map(params![secid.param(), version], |r| {
            Ok(PredictionRow {
                created_at: r.get(0)?,
                as_of: r.get(1)?,
                horizon: r.get(2)?,
                probability: r.get(3)?,
                status: r.get(4)?,
                target_date: r.get(5)?,
                actual_return: r.get(6)?,
                note: r.get(7)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    Ok(TrackingReport {recent,model_version:version.into(),window_days:180,horizons:stats,note:"仅统计本股、本模型版本最近180个自然日实际登记的预测，5/20日分别核验。基准为登记时历史同期限上涨频率；零收益视为未上涨。重叠期限样本相关，少于30个起点日期时证据不足。复权未确认、历史修订或滞后起点不进入正式统计。日线缺口和停牌尚未由交易所日历核对，期限按可用记录计数；这是概率校准检查，不是已经校准的概率。".into()})
}

/// Called by price syncs; verification failure must be visible to callers.
pub fn verify_default(secid: &Secid, bars: &[StockBar]) -> Result<usize> {
    let path = default_path();
    if !path.exists() {
        return Ok(0);
    }
    verify(&mut open(&path)?, secid, bars, Utc::now())
}

/// Web maintenance loop calls this without generating any new predictions.
/// Each downloaded batch is retained as evidence; missing network data stays pending.
pub fn refresh_pending() -> Result<usize> {
    let path = default_path();
    if !path.exists() {
        return Ok(0);
    }
    let mut conn = open(&path)?;
    let symbols = {
        let mut stmt=conn.prepare("SELECT DISTINCT i.symbol FROM forecast_inputs i JOIN forecast_records r ON r.input_id=i.id WHERE r.status='pending'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut settled = 0;
    let mut errors = Vec::new();
    for symbol in symbols {
        let Some((market, code)) = symbol.split_once('.') else {
            continue;
        };
        let secid = Secid {
            market: market.parse()?,
            code: code.into(),
        };
        let attempt = (|| -> Result<usize> {
            let bars = super::data::kline::fetch(&secid)?;
            let now = Utc::now();
            let snapshot=bars.iter().map(|b|serde_json::json!({"date":b.date,"open":b.open,"high":b.high,"low":b.low,"close":b.close,"volume":b.volume,"adj_close":b.adj_close})).collect::<Vec<_>>();
            conn.execute(
                "INSERT INTO forecast_price_batches(symbol,fetched_at,bars_json) VALUES (?1,?2,?3)",
                params![symbol, now.to_rfc3339(), serde_json::to_string(&snapshot)?],
            )?;
            verify(&mut conn, &secid, &bars, now)
        })();
        match attempt {
            Ok(n) => settled += n,
            Err(e) => errors.push(format!("{symbol}: {e}")),
        };
    }
    ensure!(
        errors.is_empty(),
        "已核验{settled}条；部分预测核验失败：{}",
        errors.join("；")
    );
    Ok(settled)
}

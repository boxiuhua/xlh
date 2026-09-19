//! 交易日历。只记录被证实的日子,不硬编码节假日表(理由同
//! `stock::realtime::calendar` 顶部说明:年底忘了更新就会静默出错)。
//!
//! 证据来源:监听线程每个工作日开盘后用一只几乎不停牌的 ETF 做快照自证。
//! 没有记录的工作日按开市处理——与旧的「数工作日」口径相同,不会更差。

use crate::stock::realtime::calendar::{is_weekend, stale_means_holiday};
use crate::trade::model::{fmt_ts, DATE_FMT};
use crate::trade::quotes::QuoteSource;
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};

/// 沪深 300 ETF:成交极活跃、几乎不停牌,拿它的快照时间戳判断今天开不开市。
pub const PROBE_CODE: &str = "510300";

pub fn mark_day(conn: &Connection, day: NaiveDate, open: bool, now: NaiveDateTime) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_calendar (day, is_open, checked_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(day) DO UPDATE SET is_open = excluded.is_open, checked_at = excluded.checked_at",
        params![day.format(DATE_FMT).to_string(), open as i64, fmt_ts(now)],
    )?;
    Ok(())
}

/// 已证实的开市(true)/ 休市(false);无记录为 None。
pub fn day_status(conn: &Connection, day: NaiveDate) -> Result<Option<bool>> {
    Ok(conn
        .query_row(
            "SELECT is_open FROM trade_calendar WHERE day = ?1",
            [day.format(DATE_FMT).to_string()],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|v| v != 0))
}

/// 周末一律休市;工作日除非被证实休市,否则按开市处理。
pub fn is_trading_day(conn: &Connection, day: NaiveDate) -> Result<bool> {
    Ok(!is_weekend(day) && day_status(conn, day)? != Some(false))
}

/// 含首尾的交易日数;`to` 早于 `from` 返回 0。
pub fn trading_days_between(conn: &Connection, from: NaiveDate, to: NaiveDate) -> Result<i64> {
    let mut day = from;
    let mut n = 0;
    while day <= to {
        if is_trading_day(conn, day)? {
            n += 1;
        }
        day += chrono::Duration::days(1);
    }
    Ok(n)
}

/// 严格晚于 `day` 的第一个交易日。
pub fn next_trading_day(conn: &Connection, day: NaiveDate) -> Result<NaiveDate> {
    let mut d = day + chrono::Duration::days(1);
    // A 股最长休市(春节 + 周末)不超过 2 周;给足余量仍找不到说明日历表被写坏了。
    for _ in 0..60 {
        if is_trading_day(conn, d)? {
            return Ok(d);
        }
        d += chrono::Duration::days(1);
    }
    Err(anyhow!("{day} 之后 60 天内没有交易日,交易日历数据异常"))
}

/// 快照自证今天是否开市并落库。周末、盘前(陈旧是正常的)返回 None 不下结论;
/// 探针代码没有报价是错误(网络或停牌),不是休市。
pub fn probe(
    conn: &Connection,
    source: &dyn QuoteSource,
    now: NaiveDateTime,
) -> Result<Option<bool>> {
    let today = now.date();
    if is_weekend(today) {
        return Ok(None);
    }
    let quotes = source.fetch(&[PROBE_CODE.to_string()])?;
    let Some(ts) = quotes.iter().map(|q| q.ts).max() else {
        return Err(anyhow!("日历探针 {PROBE_CODE} 无报价"));
    };
    if ts.date() == today {
        mark_day(conn, today, true, now)?;
        return Ok(Some(true));
    }
    if stale_means_holiday(now) {
        mark_day(conn, today, false, now)?;
        return Ok(Some(false));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::Quote;
    use crate::trade::store;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    fn at(m: u32, day: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(m, day).and_hms_opt(h, mi, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    struct Stub(Vec<Quote>);
    impl QuoteSource for Stub {
        fn fetch(&self, _codes: &[String]) -> Result<Vec<Quote>> {
            Ok(self.0.clone())
        }
    }
    fn probe_quote(ts: NaiveDateTime) -> Stub {
        Stub(vec![Quote {
            code: PROBE_CODE.into(),
            price: 4.0,
            limit_up: None,
            limit_down: None,
            ts,
        }])
    }

    #[test]
    fn unmarked_weekdays_count_as_open_weekends_never() {
        let c = db();
        // 2026-09-14 周一 … 2026-09-20 周日
        assert!(is_trading_day(&c, d(9, 14)).unwrap());
        assert!(!is_trading_day(&c, d(9, 19)).unwrap(), "周六");
        assert_eq!(trading_days_between(&c, d(9, 14), d(9, 18)).unwrap(), 5);
        assert_eq!(trading_days_between(&c, d(9, 14), d(9, 21)).unwrap(), 6);
        assert_eq!(
            trading_days_between(&c, d(9, 21), d(9, 14)).unwrap(),
            0,
            "倒序为 0"
        );
    }

    #[test]
    fn marked_holidays_are_skipped_and_can_be_corrected() {
        let c = db();
        // 国庆:10-01(周四)~10-08(周四)休市
        for day in 1..=8 {
            mark_day(&c, d(10, day), false, at(10, day, 9, 31)).unwrap();
        }
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), Some(false));
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), None);
        // 09-28(周一)~10-09(周五):工作日 10 个,扣掉 10-01、02、05、06、07、08 六个 → 4
        assert_eq!(trading_days_between(&c, d(9, 28), d(10, 9)).unwrap(), 4);
        assert_eq!(next_trading_day(&c, d(9, 30)).unwrap(), d(10, 9));
        assert_eq!(next_trading_day(&c, d(9, 18)).unwrap(), d(9, 21), "跨周末");
        // 误判可被后来的证据覆盖
        mark_day(&c, d(10, 8), true, at(10, 8, 10, 0)).unwrap();
        assert_eq!(day_status(&c, d(10, 8)).unwrap(), Some(true));
        assert_eq!(next_trading_day(&c, d(9, 30)).unwrap(), d(10, 8));
    }

    #[test]
    fn probe_marks_open_on_fresh_quote_and_closed_only_after_the_open() {
        let c = db();
        // 盘前陈旧:正常,不下结论
        assert_eq!(
            probe(&c, &probe_quote(at(9, 30, 15, 0)), at(10, 1, 9, 20)).unwrap(),
            None
        );
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), None);
        // 开盘后仍陈旧 → 休市
        assert_eq!(
            probe(&c, &probe_quote(at(9, 30, 15, 0)), at(10, 1, 9, 31)).unwrap(),
            Some(false)
        );
        assert_eq!(day_status(&c, d(10, 1)).unwrap(), Some(false));
        // 当天时间戳 → 开市
        assert_eq!(
            probe(&c, &probe_quote(at(10, 9, 9, 31)), at(10, 9, 9, 31)).unwrap(),
            Some(true)
        );
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), Some(true));
        // 周末不探
        assert_eq!(
            probe(&c, &probe_quote(at(9, 18, 15, 0)), at(9, 19, 10, 0)).unwrap(),
            None
        );
    }

    #[test]
    fn probe_with_no_quote_is_an_error_not_a_holiday() {
        let c = db();
        assert!(probe(&c, &Stub(Vec::new()), at(10, 9, 9, 31)).is_err());
        assert_eq!(day_status(&c, d(10, 9)).unwrap(), None);
    }
}

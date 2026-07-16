//! 交易日与抓取时点判断。
//!
//! # 为什么不用硬编码节假日表
//!
//! 每年年底必须手工更新，忘记则静默抓错 —— 而「静默」正是最坏的失败模式：
//! 节假日抓回昨天的陈旧数据，与前一时点做差得出「零异动」，看起来一切正常。
//!
//! # 为什么不用 universe::latest_trade_date()
//!
//! 它返回「最近**已收盘**日」，盘中取到的是昨天，回答不了「今天开不开市」。
//!
//! # 本模块的两道闸
//!
//! 1. 周末本地跳（chrono），不发任何请求
//! 2. 快照自证：比对腾讯返回的行情时间戳是否为今天。不是 → 节假日
//!
//! 全部是纯函数，不碰网络、不碰库 —— 调用方负责取数与落库。
use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

/// 抓取窗口：上午 10:00–11:30，下午 13:30–15:00，每 10 分钟一个时点。
///
/// 刻意不含 09:30–10:00：开盘半小时的量价波动是常态而非异动，
/// 且此时「过去 N 天同时点」的基准样本本身就最不稳。
const WINDOWS: [(u32, u32, u32, u32); 2] = [
    (10, 0, 11, 30),
    (13, 30, 15, 0),
];

/// 抓取间隔（分钟）。
pub const INTERVAL_MIN: u32 = 10;

/// 该时刻是否落在抓取窗口内的整时点上（10:00, 10:10, … 11:30, 13:30, … 15:00）。
///
/// 秒被忽略：守护是 60 秒 tick，命中的是分钟粒度。
pub fn is_tick_time(t: NaiveTime) -> bool {
    if t.minute() % INTERVAL_MIN != 0 { return false }
    let cur = t.hour() * 60 + t.minute();
    WINDOWS.iter().any(|&(sh, sm, eh, em)| {
        let (start, end) = (sh * 60 + sm, eh * 60 + em);
        cur >= start && cur <= end
    })
}

/// 一个交易日内的时点总数。用于容量估算与量能基准的样本对齐。
pub fn ticks_per_day() -> usize {
    WINDOWS.iter().map(|&(sh, sm, eh, em)| {
        let (start, end) = (sh * 60 + sm, eh * 60 + em);
        ((end - start) / INTERVAL_MIN + 1) as usize
    }).sum()
}

/// 周末？周末不发请求 —— 这是唯一能零成本本地判定的非交易日。
pub fn is_weekend(d: NaiveDate) -> bool {
    matches!(d.weekday(), Weekday::Sat | Weekday::Sun)
}

/// 是否值得为该时刻发一次抓取请求。
///
/// 仅做本地判断（周末 + 时点）。节假日需靠 [`verify_fresh`] 事后自证 ——
/// 本地无从得知今天是不是清明节。
pub fn should_fetch(now: NaiveDateTime) -> bool {
    !is_weekend(now.date()) && is_tick_time(now.time())
}

/// 快照自证：这批行情是「今天」的吗？
///
/// 取全批时间戳的**最大值**与今天比对，而非任取一只：个股可能停牌
/// （时间戳停在 09:00:00）或数据延迟，但只要有**任何一只**股票的行情
/// 时间戳是今天，市场就是开着的。
///
/// 返回 false 意味着今天是节假日，拿到的是上一交易日的陈旧数据。
pub fn verify_fresh(ticks_ts: &[NaiveDateTime], today: NaiveDate) -> bool {
    ticks_ts.iter().map(|t| t.date()).max() == Some(today)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime { NaiveTime::from_hms_opt(h, m, 0).unwrap() }
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn dt(y: i32, mo: u32, da: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(y, mo, da).and_hms_opt(h, mi, 0).unwrap()
    }

    #[test]
    fn tick_times_cover_both_windows_inclusive_of_bounds() {
        assert!(is_tick_time(t(10, 0)), "上午窗口起点");
        assert!(is_tick_time(t(11, 30)), "上午窗口终点须含");
        assert!(is_tick_time(t(13, 30)), "下午窗口起点");
        assert!(is_tick_time(t(15, 0)), "下午窗口终点须含（收盘价所在时点）");
    }

    #[test]
    fn tick_times_exclude_lunch_break_and_pre_ten() {
        assert!(!is_tick_time(t(9, 30)), "开盘半小时波动是常态，不在窗口内");
        assert!(!is_tick_time(t(11, 40)), "午休");
        assert!(!is_tick_time(t(12, 0)), "午休");
        assert!(!is_tick_time(t(13, 20)), "午休尾巴");
        assert!(!is_tick_time(t(15, 10)), "已收盘");
    }

    #[test]
    fn tick_times_reject_off_grid_minutes() {
        assert!(!is_tick_time(t(10, 5)), "非 10 分钟整点");
        assert!(!is_tick_time(t(14, 1)), "非 10 分钟整点");
    }

    #[test]
    fn day_has_twenty_ticks() {
        // 10:00–11:30 = 10 个，13:30–15:00 = 10 个。这个数字决定容量估算
        // （5400 只 × 20 时点 × 10 天 ≈ 108 万行）
        assert_eq!(ticks_per_day(), 20);
    }

    #[test]
    fn weekend_is_skipped_without_network() {
        assert!(is_weekend(d(2026, 7, 18)), "2026-07-18 是周六");
        assert!(is_weekend(d(2026, 7, 19)), "2026-07-19 是周日");
        assert!(!is_weekend(d(2026, 7, 17)), "2026-07-17 是周五");
    }

    #[test]
    fn should_fetch_requires_both_weekday_and_tick_time() {
        assert!(should_fetch(dt(2026, 7, 16, 10, 30)), "周四 10:30 应抓");
        assert!(!should_fetch(dt(2026, 7, 18, 10, 30)), "周六即便时点对也不抓");
        assert!(!should_fetch(dt(2026, 7, 16, 12, 0)), "周四午休不抓");
    }

    #[test]
    fn verify_fresh_accepts_when_any_stock_reports_today() {
        let today = d(2026, 7, 16);
        let ts = vec![dt(2026, 7, 16, 9, 0), dt(2026, 7, 16, 10, 30)];
        assert!(verify_fresh(&ts, today));
    }

    #[test]
    fn verify_fresh_rejects_stale_holiday_data() {
        // 节假日腾讯返回上一交易日的收盘数据，时间戳是昨天。这是本模块存在的
        // 全部理由 —— 不挡住它，就会拿昨天的数据算今天的异动，且毫无征兆
        let today = d(2026, 7, 16);
        let ts = vec![dt(2026, 7, 15, 15, 0), dt(2026, 7, 15, 15, 0)];
        assert!(!verify_fresh(&ts, today), "陈旧数据必须判为非交易日");
    }

    #[test]
    fn verify_fresh_uses_max_not_first_timestamp() {
        // 第一只是停牌股（陈旧），但有别的股票今天在成交 —— 市场开着。
        // 若实现取 first 而非 max，会把正常交易日误判成节假日并永久标记
        let today = d(2026, 7, 16);
        let ts = vec![dt(2026, 7, 10, 15, 0), dt(2026, 7, 16, 10, 30)];
        assert!(verify_fresh(&ts, today), "只要有任何一只是今天，市场就开着");
    }

    #[test]
    fn verify_fresh_rejects_empty_batch() {
        // 空批次说明抓取全失败。不能当交易日 —— 更不能据此把今天永久标记为节假日
        assert!(!verify_fresh(&[], d(2026, 7, 16)));
    }
}

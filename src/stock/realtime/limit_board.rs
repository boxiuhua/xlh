//! 涨停、跌停板检测。
//!
//! 直接使用腾讯快照给出的当日涨跌停价，不按股票代码硬编码 5%/10%/20%/30%
//! 规则。这样 ST、创业板、科创板以及规则调整都由行情源的当日价格边界体现。

use chrono::NaiveDateTime;

use super::snapshot::Tick;

/// 行情价格保留到分时，半个最小报价单位用于吸收二进制浮点误差。
const PRICE_EPSILON: f64 = 0.005;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LimitBoard {
    pub code: String,
    pub name: String,
    pub ts: NaiveDateTime,
    pub price: f64,
    pub limit_price: f64,
    pub direction: BoardDirection,
}

/// 检测一个快照是否已封涨停或跌停。无有效限制价时不作推断。
pub fn detect_tick(tick: &Tick) -> Option<LimitBoard> {
    let (direction, limit_price) = if tick
        .limit_up
        .is_some_and(|limit| tick.price + PRICE_EPSILON >= limit)
    {
        (BoardDirection::Up, tick.limit_up?)
    } else if tick
        .limit_down
        .is_some_and(|limit| tick.price - PRICE_EPSILON <= limit)
    {
        (BoardDirection::Down, tick.limit_down?)
    } else {
        return None;
    };

    Some(LimitBoard {
        code: tick.code.clone(),
        name: tick.code.clone(),
        ts: tick.ts,
        price: tick.price,
        limit_price,
        direction,
    })
}

pub fn detect(ticks: &[Tick]) -> Vec<LimitBoard> {
    ticks.iter().filter_map(detect_tick).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn tick(price: f64, up: Option<f64>, down: Option<f64>) -> Tick {
        Tick {
            code: "600000".into(),
            ts: NaiveDate::from_ymd_opt(2026, 8, 11)
                .unwrap()
                .and_hms_opt(10, 0, 0)
                .unwrap(),
            price,
            change_pct: 0.0,
            volume: 100.0,
            amount: 1000.0,
            turnover: 1.0,
            vol_ratio: 1.0,
            limit_up: up,
            limit_down: down,
        }
    }

    #[test]
    fn detects_limit_up_and_down() {
        assert_eq!(
            detect_tick(&tick(11.0, Some(11.0), Some(9.0)))
                .unwrap()
                .direction,
            BoardDirection::Up
        );
        assert_eq!(
            detect_tick(&tick(9.0, Some(11.0), Some(9.0)))
                .unwrap()
                .direction,
            BoardDirection::Down
        );
    }

    #[test]
    fn ordinary_price_and_missing_limits_do_not_trigger() {
        assert!(detect_tick(&tick(10.0, Some(11.0), Some(9.0))).is_none());
        assert!(detect_tick(&tick(10.0, None, None)).is_none());
    }

    #[test]
    fn accepts_half_tick_float_rounding_only() {
        assert!(detect_tick(&tick(10.996, Some(11.0), Some(9.0))).is_some());
        assert!(detect_tick(&tick(10.99, Some(11.0), Some(9.0))).is_none());
    }
}

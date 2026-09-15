//! A 股交易规则(纯函数)与 A 股成交模型。
//! 规则只在此处定义一次:回测成交模型与交易闸门共用。

fn is_star(code: &str) -> bool {
    code.starts_with("688") || code.starts_with("689")
}

fn is_chinext(code: &str) -> bool {
    code.starts_with("300") || code.starts_with("301")
}

fn is_bse(code: &str) -> bool {
    code.starts_with('8') || code.starts_with("43") || code.starts_with("92")
}

fn is_etf(code: &str) -> bool {
    ["51", "52", "56", "58", "15", "16", "18"]
        .iter()
        .any(|p| code.starts_with(p))
}

/// 涨跌幅比例。`name` 未知时传 None(视为非 ST)。
/// 创业板/科创板无论是否 ST 均为 20%。
pub fn limit_ratio(code: &str, name: Option<&str>) -> f64 {
    if is_star(code) || is_chinext(code) {
        return 0.20;
    }
    if is_bse(code) {
        return 0.30;
    }
    if name.is_some_and(|n| n.to_uppercase().contains("ST")) {
        0.05
    } else {
        0.10
    }
}

/// 最小价位小数位:场内基金 0.001,股票 0.01。
pub fn price_decimals(code: &str) -> i32 {
    if is_etf(code) {
        3
    } else {
        2
    }
}

fn round_to(x: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (x * m).round() / m
}

/// (涨停价, 跌停价),按最小价位四舍五入。
pub fn limit_prices(prev_close: f64, ratio: f64, decimals: i32) -> (f64, f64) {
    (
        round_to(prev_close * (1.0 + ratio), decimals),
        round_to(prev_close * (1.0 - ratio), decimals),
    )
}

/// 买入数量约束:至少 `min` 股,超出部分按 `step` 递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyLot {
    pub min: u64,
    pub step: u64,
}

pub fn buy_lot(code: &str) -> BuyLot {
    if is_star(code) {
        BuyLot { min: 200, step: 1 }
    } else if is_bse(code) {
        BuyLot { min: 100, step: 1 }
    } else {
        BuyLot {
            min: 100,
            step: 100,
        }
    }
}

/// 把「理论可买股数」向下取整到合法数量;不足最小数量返回 0。
pub fn round_buy_shares(raw_shares: f64, lot: BuyLot) -> u64 {
    if !raw_shares.is_finite() || raw_shares < lot.min as f64 {
        return 0;
    }
    let n = (raw_shares + 1e-6).floor() as u64;
    lot.min + (n - lot.min) / lot.step * lot.step
}

/// 减少一个步长;低于最小数量返回 0。
pub fn step_down(n: u64, lot: BuyLot) -> u64 {
    if n >= lot.min + lot.step {
        n - lot.step
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn limit_ratio_by_board_and_st() {
        assert!(close(limit_ratio("600000", None), 0.10));
        assert!(close(limit_ratio("000001", Some("ST 某某")), 0.05));
        assert!(close(limit_ratio("600001", Some("*st某某")), 0.05));
        assert!(
            close(limit_ratio("300750", Some("ST 某某")), 0.20),
            "创业板 ST 仍 20%"
        );
        assert!(close(limit_ratio("301001", None), 0.20));
        assert!(close(limit_ratio("688981", None), 0.20));
        assert!(close(limit_ratio("830799", None), 0.30));
        assert!(close(limit_ratio("430047", None), 0.30));
    }

    #[test]
    fn limit_prices_round_to_tick() {
        let (up, down) = limit_prices(10.0, 0.10, 2);
        assert!(close(up, 11.0) && close(down, 9.0));
        let (up, down) = limit_prices(3.33, 0.10, 2);
        assert!(close(up, 3.66) && close(down, 3.00), "up={up} down={down}");
        assert_eq!(price_decimals("510300"), 3);
        assert_eq!(price_decimals("600000"), 2);
        let (up, down) = limit_prices(3.456, 0.10, 3);
        assert!(
            close(up, 3.802) && close(down, 3.110),
            "up={up} down={down}"
        );
    }

    #[test]
    fn buy_lot_by_board() {
        assert_eq!(
            buy_lot("600000"),
            BuyLot {
                min: 100,
                step: 100
            }
        );
        assert_eq!(
            buy_lot("510300"),
            BuyLot {
                min: 100,
                step: 100
            }
        );
        assert_eq!(buy_lot("688001"), BuyLot { min: 200, step: 1 });
        assert_eq!(buy_lot("830799"), BuyLot { min: 100, step: 1 });
    }

    #[test]
    fn round_buy_shares_floors_to_lot() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(round_buy_shares(999.0, main), 900);
        assert_eq!(round_buy_shares(99.9, main), 0);
        assert_eq!(round_buy_shares(100.0, main), 100);
        assert_eq!(round_buy_shares(299.5, star), 299);
        assert_eq!(round_buy_shares(150.0, star), 0);
        assert_eq!(round_buy_shares(f64::NAN, main), 0);
    }

    #[test]
    fn step_down_stops_at_min() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(step_down(900, main), 800);
        assert_eq!(step_down(100, main), 0);
        assert_eq!(step_down(201, star), 200);
        assert_eq!(step_down(200, star), 0);
    }
}

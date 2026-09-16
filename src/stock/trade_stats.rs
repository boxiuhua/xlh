use crate::event::Direction;
use crate::result::TradeRecord;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TradeStats {
    pub round_trips: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub realized_pnl: f64,
}

/// 一次卖出对应的 FIFO 回合。买入费用摊入每股成本,卖出费用从收入中扣除。
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct RoundTrip {
    pub shares: f64,
    /// 匹配到的买入成本(含买入费)
    pub cost: f64,
    /// 卖出收入(已扣卖出费)
    pub proceeds: f64,
    pub pnl: f64,
}

impl RoundTrip {
    /// 本回合收益率;无成本(裸卖)返回 0。注意:`trade_baseline`(见
    /// `crate::trade::admission::walk_forward`)把收益率 <= 0 都算作亏损,
    /// 所以裸卖的 0 会被计入连亏 / 拉低胜率,而不是被当成「中性、不计入统计」。
    pub fn ret(&self) -> f64 {
        if self.cost > 1e-9 {
            self.pnl / self.cost
        } else {
            0.0
        }
    }
}

/// FIFO 还原每次卖出的成本与盈亏。`trade_stats` 在其之上聚合。
pub fn round_trips(trades: &[TradeRecord]) -> Vec<RoundTrip> {
    let mut lots: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new();
    let mut out = Vec::new();
    for t in trades {
        match t.direction {
            Direction::Buy => {
                if t.shares > 1e-9 {
                    lots.push_back((t.shares, t.price + t.fee / t.shares));
                }
            }
            Direction::Sell => {
                let mut remaining = t.shares;
                let mut cost = 0.0;
                while remaining > 1e-9 {
                    let Some((lot_shares, lot_cost)) = lots.front().copied() else {
                        break;
                    };
                    let take = remaining.min(lot_shares);
                    cost += take * lot_cost;
                    let left = lot_shares - take;
                    if left > 1e-9 {
                        lots.front_mut().expect("刚读到队首").0 = left;
                    } else {
                        lots.pop_front();
                    }
                    remaining -= take;
                }
                let matched = t.shares - remaining;
                let proceeds = matched * t.price - t.fee;
                out.push(RoundTrip {
                    shares: matched,
                    cost,
                    proceeds,
                    pnl: proceeds - cost,
                });
            }
        }
    }
    out
}

/// 对成交序列做 FIFO 成本匹配，还原每笔卖出的实现盈亏。买费摊入每股成本。
pub fn trade_stats(trades: &[TradeRecord]) -> TradeStats {
    let rts = round_trips(trades);
    let round_trips = rts.len();
    let wins = rts.iter().filter(|rt| rt.pnl > 0.0).count();
    let gross_win: f64 = rts.iter().filter(|rt| rt.pnl > 0.0).map(|rt| rt.pnl).sum();
    let gross_loss: f64 = rts
        .iter()
        .filter(|rt| rt.pnl <= 0.0)
        .map(|rt| -rt.pnl)
        .sum();

    let losses = round_trips - wins;
    let win_rate = if round_trips > 0 {
        wins as f64 / round_trips as f64
    } else {
        0.0
    };
    let profit_factor = if gross_loss > 1e-9 {
        gross_win / gross_loss
    } else if gross_win > 1e-9 {
        f64::INFINITY
    } else {
        0.0
    };
    let avg_win = if wins > 0 {
        gross_win / wins as f64
    } else {
        0.0
    };
    let avg_loss = if losses > 0 {
        gross_loss / losses as f64
    } else {
        0.0
    };
    TradeStats {
        round_trips,
        wins,
        win_rate,
        profit_factor,
        avg_win,
        avg_loss,
        realized_pnl: gross_win - gross_loss,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn buy(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord {
            date: dt,
            direction: Direction::Buy,
            shares,
            price,
            fee,
        }
    }
    fn sell(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord {
            date: dt,
            direction: Direction::Sell,
            shares,
            price,
            fee,
        }
    }

    #[test]
    fn no_sells_is_zero() {
        let s = trade_stats(&[buy(d(2024, 1, 1), 100.0, 1.0, 0.0)]);
        assert_eq!(s.round_trips, 0);
        assert_eq!(s.wins, 0);
        assert!((s.realized_pnl).abs() < 1e-9);
        assert!((s.profit_factor).abs() < 1e-9); // 无盈亏 → 0
    }

    #[test]
    fn single_winning_round_trip() {
        let s = trade_stats(&[
            buy(d(2024, 1, 1), 100.0, 1.0, 0.0),
            sell(d(2024, 2, 1), 100.0, 2.0, 0.0),
        ]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.win_rate - 1.0).abs() < 1e-9);
        assert!((s.realized_pnl - 100.0).abs() < 1e-9);
        assert!(s.profit_factor.is_infinite()); // 无亏损
        assert!((s.avg_win - 100.0).abs() < 1e-9);
    }

    #[test]
    fn single_losing_round_trip() {
        let s = trade_stats(&[
            buy(d(2024, 1, 1), 100.0, 2.0, 0.0),
            sell(d(2024, 2, 1), 100.0, 1.0, 0.0),
        ]);
        assert_eq!(s.wins, 0);
        assert!((s.win_rate).abs() < 1e-9);
        assert!((s.realized_pnl + 100.0).abs() < 1e-9);
        assert!((s.profit_factor).abs() < 1e-9); // 无盈利
        assert!((s.avg_loss - 100.0).abs() < 1e-9);
    }

    #[test]
    fn fifo_partial_consumption() {
        // 买100@1、买100@2，卖150@3：消耗100@1(成本100)+50@2(成本100)=200，
        // 收入=150*3=450 → 实现盈亏=250，一次盈利 round trip。
        let s = trade_stats(&[
            buy(d(2024, 1, 1), 100.0, 1.0, 0.0),
            buy(d(2024, 1, 2), 100.0, 2.0, 0.0),
            sell(d(2024, 1, 3), 150.0, 3.0, 0.0),
        ]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.realized_pnl - 250.0).abs() < 1e-9);
    }

    #[test]
    fn buy_fee_folds_into_cost_basis() {
        // 买100@1 费10 → 每股成本=1+0.1=1.1；卖100@1 费0 → 实现盈亏=(1-1.1)*100=-10
        let s = trade_stats(&[
            buy(d(2024, 1, 1), 100.0, 1.0, 10.0),
            sell(d(2024, 2, 1), 100.0, 1.0, 0.0),
        ]);
        assert!((s.realized_pnl + 10.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_report_cost_and_return_per_sell() {
        // 买 100 @10 费 5 → 每股成本 10.05;卖 100 @12 费 6 → pnl = 1200 - 6 - 1005 = 189
        let trades = vec![
            TradeRecord {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                shares: 100.0,
                price: 10.0,
                fee: 5.0,
            },
            TradeRecord {
                date: d(2024, 1, 3),
                direction: Direction::Sell,
                shares: 100.0,
                price: 12.0,
                fee: 6.0,
            },
        ];
        let rts = round_trips(&trades);
        assert_eq!(rts.len(), 1);
        assert!((rts[0].cost - 1005.0).abs() < 1e-9);
        assert!((rts[0].pnl - 189.0).abs() < 1e-9);
        assert!((rts[0].ret() - 189.0 / 1005.0).abs() < 1e-9);
        // 聚合口径与既有 trade_stats 一致
        let st = trade_stats(&trades);
        assert_eq!((st.round_trips, st.wins), (1, 1));
        assert!((st.realized_pnl - 189.0).abs() < 1e-9);
    }

    #[test]
    fn round_trips_without_lots_are_ignored_for_cost() {
        // 无持仓直接卖:成本 0,收益率按 0 处理,但仍计一次 round trip(与既有统计一致)
        let trades = vec![TradeRecord {
            date: d(2024, 1, 2),
            direction: Direction::Sell,
            shares: 100.0,
            price: 12.0,
            fee: 6.0,
        }];
        let rts = round_trips(&trades);
        assert_eq!(rts.len(), 1);
        assert_eq!(rts[0].ret(), 0.0);
    }
}

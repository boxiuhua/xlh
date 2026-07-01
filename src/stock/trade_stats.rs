use crate::result::TradeRecord;
use crate::event::Direction;

#[derive(Debug, Clone, PartialEq)]
pub struct TradeStats {
    pub round_trips: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub realized_pnl: f64,
}

/// 对成交序列做 FIFO 成本匹配，还原每笔卖出的实现盈亏。买费摊入每股成本。
pub fn trade_stats(trades: &[TradeRecord]) -> TradeStats {
    let mut lots: std::collections::VecDeque<(f64, f64)> = std::collections::VecDeque::new(); // (剩余份额, 每股成本)
    let mut round_trips = 0usize;
    let mut wins = 0usize;
    let mut gross_win = 0.0;
    let mut gross_loss = 0.0; // 累计正值

    for t in trades {
        match t.direction {
            Direction::Buy => {
                if t.shares > 1e-9 {
                    let cost_per_share = t.price + t.fee / t.shares;
                    lots.push_back((t.shares, cost_per_share));
                }
            }
            Direction::Sell => {
                let mut remaining = t.shares;
                let mut cost = 0.0;
                while remaining > 1e-9 {
                    let Some((lot_shares, lot_cost)) = lots.front().copied() else { break; };
                    let take = remaining.min(lot_shares);
                    cost += take * lot_cost;
                    let left = lot_shares - take;
                    if left > 1e-9 { lots.front_mut().unwrap().0 = left; } else { lots.pop_front(); }
                    remaining -= take;
                }
                let matched = t.shares - remaining;
                let pnl = matched * t.price - t.fee - cost;
                round_trips += 1;
                if pnl > 0.0 { wins += 1; gross_win += pnl; } else { gross_loss += -pnl; }
            }
        }
    }

    let losses = round_trips - wins;
    let win_rate = if round_trips > 0 { wins as f64 / round_trips as f64 } else { 0.0 };
    let profit_factor = if gross_loss > 1e-9 { gross_win / gross_loss }
        else if gross_win > 1e-9 { f64::INFINITY } else { 0.0 };
    let avg_win = if wins > 0 { gross_win / wins as f64 } else { 0.0 };
    let avg_loss = if losses > 0 { gross_loss / losses as f64 } else { 0.0 };
    TradeStats { round_trips, wins, win_rate, profit_factor, avg_win, avg_loss, realized_pnl: gross_win - gross_loss }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn buy(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord { date: dt, direction: Direction::Buy, shares, price, fee }
    }
    fn sell(dt: NaiveDate, shares: f64, price: f64, fee: f64) -> TradeRecord {
        TradeRecord { date: dt, direction: Direction::Sell, shares, price, fee }
    }

    #[test]
    fn no_sells_is_zero() {
        let s = trade_stats(&[buy(d(2024,1,1), 100.0, 1.0, 0.0)]);
        assert_eq!(s.round_trips, 0);
        assert_eq!(s.wins, 0);
        assert!((s.realized_pnl).abs() < 1e-9);
        assert!((s.profit_factor).abs() < 1e-9); // 无盈亏 → 0
    }

    #[test]
    fn single_winning_round_trip() {
        let s = trade_stats(&[buy(d(2024,1,1),100.0,1.0,0.0), sell(d(2024,2,1),100.0,2.0,0.0)]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.win_rate - 1.0).abs() < 1e-9);
        assert!((s.realized_pnl - 100.0).abs() < 1e-9);
        assert!(s.profit_factor.is_infinite()); // 无亏损
        assert!((s.avg_win - 100.0).abs() < 1e-9);
    }

    #[test]
    fn single_losing_round_trip() {
        let s = trade_stats(&[buy(d(2024,1,1),100.0,2.0,0.0), sell(d(2024,2,1),100.0,1.0,0.0)]);
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
            buy(d(2024,1,1),100.0,1.0,0.0),
            buy(d(2024,1,2),100.0,2.0,0.0),
            sell(d(2024,1,3),150.0,3.0,0.0),
        ]);
        assert_eq!(s.round_trips, 1);
        assert_eq!(s.wins, 1);
        assert!((s.realized_pnl - 250.0).abs() < 1e-9);
    }

    #[test]
    fn buy_fee_folds_into_cost_basis() {
        // 买100@1 费10 → 每股成本=1+0.1=1.1；卖100@1 费0 → 实现盈亏=(1-1.1)*100=-10
        let s = trade_stats(&[buy(d(2024,1,1),100.0,1.0,10.0), sell(d(2024,2,1),100.0,1.0,0.0)]);
        assert!((s.realized_pnl + 10.0).abs() < 1e-9);
    }
}

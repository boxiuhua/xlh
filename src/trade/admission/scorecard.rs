//! 四栏成绩单:样本外(来自最近一次前推回测)、模拟盘、实盘,以及执行损耗。

use crate::trade::admission::stats::{self, equity_drawdown, sell_pnls, trade_returns, FillRow};
use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::model::{Account, StrategyStatus};
use crate::trade::store;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// 一栏表现。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct StageMetrics {
    pub trades: usize,
    pub win_rate: f64,
    pub avg_trade_return: f64,
    pub max_drawdown: f64,
    pub realized_pnl: f64,
}

pub fn stage_metrics(fills: &[FillRow]) -> StageMetrics {
    let pnls = sell_pnls(fills);
    if pnls.is_empty() {
        return StageMetrics::default();
    }
    let returns = trade_returns(fills);
    let wins = pnls.iter().filter(|p| **p > 0.0).count();
    StageMetrics {
        trades: pnls.len(),
        win_rate: wins as f64 / pnls.len() as f64,
        avg_trade_return: if returns.is_empty() {
            0.0
        } else {
            returns.iter().sum::<f64>() / returns.len() as f64
        },
        max_drawdown: equity_drawdown(&pnls),
        realized_pnl: pnls.iter().sum(),
    }
}

/// 策略成绩单:样本外取最近一次前推回测结果,模拟盘 / 实盘取成交记录。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scorecard {
    pub strategy_id: i64,
    pub name: String,
    pub status: StrategyStatus,
    pub oos: Option<PoolMetrics>,
    pub paper: StageMetrics,
    pub real: StageMetrics,
    /// 实盘相对模拟盘多付的比例中位数
    pub execution_loss: Option<f64>,
}

pub fn scorecard(conn: &Connection, user_id: i64, strategy_id: i64) -> Result<Option<Scorecard>> {
    let Some(s) = store::get_strategy(conn, user_id, strategy_id)? else {
        return Ok(None);
    };
    let oos = match store::latest_eval(conn, strategy_id, user_id, "oos")? {
        Some((json, _)) => serde_json::from_str::<PoolMetrics>(&json).ok(),
        None => None,
    };
    let paper = stats::strategy_fills(conn, user_id, strategy_id, Account::Paper, None)?;
    let real = stats::strategy_fills(conn, user_id, strategy_id, Account::Real, None)?;
    Ok(Some(Scorecard {
        strategy_id,
        name: s.name,
        status: s.status,
        oos,
        paper: stage_metrics(&paper),
        real: stage_metrics(&real),
        execution_loss: stats::execution_loss(conn, user_id, strategy_id)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::admission::stats::FillRow;
    use crate::trade::model::Account;
    use chrono::{NaiveDate, NaiveDateTime};

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn sell(pnl: f64, price: f64, signal_id: i64) -> FillRow {
        FillRow {
            account: Account::Real,
            code: "600000".into(),
            side: Direction::Sell,
            price,
            qty: 1000,
            fee: 10.0,
            realized_pnl: Some(pnl),
            filled_at: at(16, 10, 0),
            signal_id,
        }
    }

    #[test]
    fn stage_metrics_summarise_fills() {
        let m = stage_metrics(&[
            sell(100.0, 11.0, 1),
            sell(-50.0, 11.0, 2),
            sell(20.0, 11.0, 3),
        ]);
        assert_eq!(m.trades, 3);
        assert!((m.win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.realized_pnl - 70.0).abs() < 1e-9);
        assert!(m.max_drawdown > 0.0);
        assert_eq!(stage_metrics(&[]), StageMetrics::default());
    }
}

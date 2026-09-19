//! 四栏成绩单:样本外(来自最近一次前推回测)、模拟盘、实盘,以及执行损耗。

use crate::trade::admission::stats::{
    self, realized_drawdown, sell_pnls, trade_returns, trade_units, FillRow,
};
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
    /// 盈亏比 = 平均盈利 / 平均亏损绝对值;无盈利或无亏损样本时为 None
    pub profit_factor: Option<f64>,
}

/// 入参仍是成交流水;内部先按工单合并成逻辑交易,笔数与连亏才与回测同口径。
pub fn stage_metrics(fills: &[FillRow]) -> StageMetrics {
    let units = trade_units(fills);
    let pnls = sell_pnls(&units);
    if pnls.is_empty() {
        return StageMetrics::default();
    }
    let returns = trade_returns(&units);
    let wins = pnls.iter().filter(|p| **p > 0.0).count();
    let win_pnls: Vec<f64> = pnls.iter().copied().filter(|p| *p > 0.0).collect();
    let loss_pnls: Vec<f64> = pnls.iter().copied().filter(|p| *p < 0.0).collect();
    let profit_factor = (!win_pnls.is_empty() && !loss_pnls.is_empty()).then(|| {
        (win_pnls.iter().sum::<f64>() / win_pnls.len() as f64)
            / (loss_pnls.iter().sum::<f64>().abs() / loss_pnls.len() as f64)
    });
    StageMetrics {
        trades: pnls.len(),
        win_rate: wins as f64 / pnls.len() as f64,
        avg_trade_return: if returns.is_empty() {
            0.0
        } else {
            returns.iter().sum::<f64>() / returns.len() as f64
        },
        max_drawdown: realized_drawdown(fills),
        realized_pnl: pnls.iter().sum(),
        profit_factor,
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
    // 这里的 `.ok()` 是有意的:成绩单纯展示,旧版本落库的 metrics_json 反序列化失败时
    // 少一栏样本外数据即可,不该让整张成绩单打不开。判定路径(`worker::latest_pool_metrics`)
    // 恰恰相反——那里必须 fail-closed 上抛错误,否则基线静默缺失会把策略直接放行进实盘。
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
            ticket_id: signal_id,
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

    /// 买 1000 @10、费 10 → 投入 10 010,回撤的分母。
    fn buy(signal_id: i64) -> FillRow {
        FillRow {
            side: Direction::Buy,
            realized_pnl: None,
            price: 10.0,
            filled_at: at(16, 9, 30),
            ..sell(0.0, 10.0, signal_id)
        }
    }

    #[test]
    fn stage_metrics_summarise_fills() {
        // 三轮「买 1000 @10(费 10)→ 卖 1000 @11」:投入峰值恒为 10 010
        let m = stage_metrics(&[
            buy(0),
            sell(100.0, 11.0, 1),
            buy(10),
            sell(-50.0, 11.0, 2),
            buy(20),
            sell(20.0, 11.0, 3),
        ]);
        assert_eq!(m.trades, 3, "买入不计入笔数");
        assert!((m.win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert!((m.realized_pnl - 70.0).abs() < 1e-9);
        // 权益 10 010 → 10 110(峰值)→ 10 060 → 10 080:最深回撤 50 / 10 110
        assert!(
            (m.max_drawdown - 50.0 / 10_110.0).abs() < 1e-12,
            "{}",
            m.max_drawdown
        );
        assert_eq!(stage_metrics(&[]), StageMetrics::default());
    }

    /// 裁决 5:分批成交按工单合并——三批卖出只是 1 笔。
    #[test]
    fn stage_metrics_merge_partial_fills_of_one_ticket() {
        let batch: Vec<FillRow> = (0..3)
            .map(|i| FillRow {
                filled_at: at(16, 10, i),
                ..sell(-10.0, 11.0, 9)
            })
            .collect();
        let m = stage_metrics(&batch);
        assert_eq!(m.trades, 1, "同一工单三批成交是 1 笔");
        assert!((m.realized_pnl - (-30.0)).abs() < 1e-9);
        assert_eq!(m.win_rate, 0.0);
    }

    /// 计划 4a 设计裁决 8:盈亏比 = 平均盈利 / 平均亏损绝对值。
    #[test]
    fn profit_factor_is_avg_win_over_avg_loss() {
        // 卖出盈亏 +300、+100、−100、−100 → 平均盈利 200 / 平均亏损 100 = 2.0
        let m = stage_metrics(&[
            buy(0),
            sell(300.0, 11.0, 1),
            buy(10),
            sell(100.0, 11.0, 2),
            buy(20),
            sell(-100.0, 11.0, 3),
            buy(30),
            sell(-100.0, 11.0, 4),
        ]);
        assert_eq!(m.profit_factor, Some(2.0), "{:?}", m.profit_factor);

        // 全部盈利 → 无亏损样本 → None
        let all_wins = stage_metrics(&[buy(0), sell(100.0, 11.0, 1), buy(10), sell(50.0, 11.0, 2)]);
        assert_eq!(all_wins.profit_factor, None);

        // 全部亏损 → 无盈利样本 → None
        let all_losses =
            stage_metrics(&[buy(0), sell(-100.0, 11.0, 1), buy(10), sell(-50.0, 11.0, 2)]);
        assert_eq!(all_losses.profit_factor, None);
    }
}

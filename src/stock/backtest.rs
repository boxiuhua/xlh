use crate::broker::Broker;
use crate::engine::Engine;
use crate::event::SignalEvent;
use crate::execution::{ExecutionModel, RejectedOrder};
use crate::metrics::{self, Summary};
use crate::portfolio::Portfolio;
use crate::result::{DailyRecord, TradeRecord};
use crate::stock::data::StockData;
use crate::stock::fee::StockFee;
use crate::stock::trade_stats::{self, TradeStats};
use crate::strategy::Strategy;
use chrono::NaiveDate;

#[derive(serde::Serialize)]
pub struct StockRunOutcome {
    pub name: String,
    pub code: String,
    pub summary: Summary,
    pub trade_stats: TradeStats,
    pub daily: Vec<DailyRecord>,
    pub trades: Vec<TradeRecord>,
    /// 成交口径:"a_share" | "close"
    pub execution: String,
    /// 因成交规则未能成交的订单
    pub rejected: Vec<RejectedOrder>,
}

/// 装配引擎跑单股回测:StockData + 复用策略 + StockFee + Portfolio + 成交模型。
pub fn run_one(
    name: String,
    code: String,
    data: StockData,
    strategy: Box<dyn Strategy>,
    fee: StockFee,
    initial_cash: f64,
    exec: Box<dyn ExecutionModel>,
) -> StockRunOutcome {
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio).with_execution(exec);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let stats = trade_stats::trade_stats(engine.trades());
    StockRunOutcome {
        name,
        code,
        summary,
        trade_stats: stats,
        daily: engine.daily().to_vec(),
        trades: engine.trades().to_vec(),
        execution: engine.execution_name().to_string(),
        rejected: engine.rejected().to_vec(),
    }
}

/// 回放 `data` 后追问次日决策(见 `Engine::decide_next`)。历史取 `data` 的全部 bar。
pub fn replay_and_decide(
    data: StockData,
    strategy: Box<dyn Strategy>,
    fee: StockFee,
    initial_cash: f64,
    exec: Box<dyn ExecutionModel>,
    next: NaiveDate,
) -> Vec<SignalEvent> {
    let history = data.events().to_vec();
    let mut engine = Engine::new(
        data,
        strategy,
        Broker::new(fee),
        Portfolio::new(initial_cash),
    )
    .with_execution(exec);
    engine.run();
    engine.decide_next(next, &history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::CloseExecution;
    use crate::stock::data::StockBar;
    use crate::strategy::dca::Dca;
    use crate::strategy::Period;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn bar(dt: NaiveDate, price: f64) -> StockBar {
        StockBar {
            date: dt,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: 0.0,
            adj_close: price,
        }
    }

    #[test]
    fn run_one_dca_flat_then_up() {
        // 与基金 runner::run_one_dca_flat_then_up 对齐：1/1买1000、2/1买1000、2/15价2.0
        // us() 零费 → 持有2000份，市值4000，投入2000，2笔买入，0次卖出
        let bars = vec![
            bar(d(2024, 1, 1), 1.0),
            bar(d(2024, 2, 1), 1.0),
            bar(d(2024, 2, 15), 2.0),
        ];
        let strategy: Box<dyn Strategy> = Box::new(Dca::new(Period::Monthly, 1, 1000.0));
        let out = run_one(
            "t".into(),
            "600519".into(),
            StockData::new(bars),
            strategy,
            StockFee::us(),
            0.0,
            Box::new(CloseExecution),
        );
        assert_eq!(out.daily.len(), 3);
        assert!(
            (out.summary.final_equity - 4000.0).abs() < 1e-6,
            "final_equity={}",
            out.summary.final_equity
        );
        assert!((out.summary.total_contributed - 2000.0).abs() < 1e-6);
        assert_eq!(out.summary.trade_count, 2);
        assert_eq!(out.trade_stats.round_trips, 0, "无卖出");
    }

    #[test]
    fn outcome_serializes_to_json() {
        let bars = vec![
            bar(d(2024, 1, 1), 1.0),
            bar(d(2024, 2, 1), 1.0),
            bar(d(2024, 2, 15), 2.0),
        ];
        let out = run_one(
            "t".into(),
            "600519".into(),
            StockData::new(bars),
            Box::new(Dca::new(Period::Monthly, 1, 1000.0)),
            StockFee::us(),
            0.0,
            Box::new(CloseExecution),
        );
        let j = serde_json::to_string(&out).unwrap();
        for k in [
            "\"summary\"",
            "\"trade_stats\"",
            "\"daily\"",
            "\"trades\"",
            "\"round_trips\"",
            "\"execution\"",
            "\"rejected\"",
        ] {
            assert!(j.contains(k), "JSON 应含 {k}");
        }
    }

    #[test]
    fn a_share_fee_reduces_equity_vs_free() {
        // 同一序列，A股费 vs 零费：A股费下期末权益更低（含最低佣金）
        let bars = vec![
            bar(d(2024, 1, 1), 1.0),
            bar(d(2024, 2, 1), 1.0),
            bar(d(2024, 2, 15), 2.0),
        ];
        let free = run_one(
            "f".into(),
            "600519".into(),
            StockData::new(bars.clone()),
            Box::new(Dca::new(Period::Monthly, 1, 1000.0)),
            StockFee::us(),
            0.0,
            Box::new(CloseExecution),
        );
        let paid = run_one(
            "p".into(),
            "600519".into(),
            StockData::new(bars),
            Box::new(Dca::new(Period::Monthly, 1, 1000.0)),
            StockFee::a_share(),
            0.0,
            Box::new(CloseExecution),
        );
        assert!(
            paid.summary.final_equity < free.summary.final_equity,
            "A股费应降低期末权益"
        );
    }

    #[test]
    fn a_share_execution_is_applied_and_reported() {
        use crate::stock::ashare::AShareExecution;
        // bar() 的 open == close
        let bars = vec![
            bar(d(2024, 1, 1), 10.0),
            bar(d(2024, 2, 1), 11.0), // 相对前收 10 开盘涨停
            bar(d(2024, 3, 1), 11.0),
        ];
        let out = run_one(
            "t".into(),
            "600519".into(),
            StockData::new(bars),
            Box::new(Dca::new(Period::Monthly, 1, 10000.0)),
            StockFee::a_share(),
            0.0,
            Box::new(AShareExecution::new("600519", None, 0.001)),
        );
        assert_eq!(out.execution, "a_share");
        assert_eq!(out.rejected.len(), 2);
        assert_eq!(out.trades.len(), 1);
        let j = serde_json::to_string(&out).unwrap();
        assert!(j.contains("\"limit_up\""), "拒单原因应序列化: {j}");
    }

    /// F1: 提供区间前一交易日 bar 后，首根 bar(定投日)应正常成交，而非因 NoPrevClose 被拒。
    #[test]
    fn first_bar_fills_when_prev_bar_supplied() {
        use crate::stock::ashare::AShareExecution;
        let prev = bar(d(2023, 12, 29), 10.0);
        let bars = vec![
            bar(d(2024, 1, 1), 10.0),
            bar(d(2024, 2, 1), 10.0),
            bar(d(2024, 3, 1), 10.0),
        ];
        let out = run_one(
            "t".into(),
            "600519".into(),
            StockData::with_prev_bar(bars, Some(prev)),
            Box::new(Dca::new(Period::Monthly, 1, 10000.0)),
            StockFee::a_share(),
            0.0,
            Box::new(AShareExecution::new("600519", None, 0.001)),
        );
        assert_eq!(
            out.rejected.len(),
            0,
            "提供前收后首根 bar 不应被拒: {:?}",
            out.rejected
        );
        assert_eq!(out.trades.len(), 3);
    }
}

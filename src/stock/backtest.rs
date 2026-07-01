use crate::broker::Broker;
use crate::engine::Engine;
use crate::metrics::{self, Summary};
use crate::portfolio::Portfolio;
use crate::result::{DailyRecord, TradeRecord};
use crate::strategy::Strategy;
use crate::stock::data::{StockData, StockBar};
use crate::stock::fee::StockFee;
use crate::stock::trade_stats::{self, TradeStats};

pub struct StockRunOutcome {
    pub name: String,
    pub code: String,
    pub summary: Summary,
    pub trade_stats: TradeStats,
    pub daily: Vec<DailyRecord>,
    pub trades: Vec<TradeRecord>,
}

/// 装配引擎跑单股回测：StockData + 复用策略 + StockFee + Portfolio。
pub fn run_one(
    name: String,
    code: String,
    bars: Vec<StockBar>,
    strategy: Box<dyn Strategy>,
    fee: StockFee,
    initial_cash: f64,
) -> StockRunOutcome {
    let data = StockData::new(bars);
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let stats = trade_stats::trade_stats(engine.trades());
    let daily = engine.daily().to_vec();
    let trades = engine.trades().to_vec();
    StockRunOutcome { name, code, summary, trade_stats: stats, daily, trades }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Period;
    use crate::strategy::dca::Dca;
    use chrono::NaiveDate;
    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn bar(dt: NaiveDate, price: f64) -> StockBar {
        StockBar { date: dt, open: price, high: price, low: price, close: price, volume: 0.0, adj_close: price }
    }

    #[test]
    fn run_one_dca_flat_then_up() {
        // 与基金 runner::run_one_dca_flat_then_up 对齐：1/1买1000、2/1买1000、2/15价2.0
        // us() 零费 → 持有2000份，市值4000，投入2000，2笔买入，0次卖出
        let bars = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.0), bar(d(2024,2,15),2.0)];
        let strategy: Box<dyn Strategy> = Box::new(Dca::new(Period::Monthly, 1, 1000.0));
        let out = run_one("t".into(), "600519".into(), bars, strategy, StockFee::us(), 0.0);
        assert_eq!(out.daily.len(), 3);
        assert!((out.summary.final_equity - 4000.0).abs() < 1e-6, "final_equity={}", out.summary.final_equity);
        assert!((out.summary.total_contributed - 2000.0).abs() < 1e-6);
        assert_eq!(out.summary.trade_count, 2);
        assert_eq!(out.trade_stats.round_trips, 0, "无卖出");
    }

    #[test]
    fn a_share_fee_reduces_equity_vs_free() {
        // 同一序列，A股费 vs 零费：A股费下期末权益更低（含最低佣金）
        let bars = vec![bar(d(2024,1,1),1.0), bar(d(2024,2,1),1.0), bar(d(2024,2,15),2.0)];
        let free = run_one("f".into(), "600519".into(), bars.clone(),
            Box::new(Dca::new(Period::Monthly,1,1000.0)), StockFee::us(), 0.0);
        let paid = run_one("p".into(), "600519".into(), bars,
            Box::new(Dca::new(Period::Monthly,1,1000.0)), StockFee::a_share(), 0.0);
        assert!(paid.summary.final_equity < free.summary.final_equity, "A股费应降低期末权益");
    }
}

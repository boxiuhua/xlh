use crate::broker::{Broker, FeeModel};
use crate::data::{InMemoryData, NavPoint};
use crate::engine::Engine;
use crate::metrics::{self, Summary};
use crate::portfolio::Portfolio;
use crate::result::DailyRecord;
use crate::strategy::Strategy;

pub struct RunOutcome {
    pub name: String,
    pub fund_code: String,
    pub summary: Summary,
    pub daily: Vec<DailyRecord>,
}

/// 跑单个命名回测：装配引擎→run→汇总指标。points 已按区间过滤、排序。
pub fn run_one(
    name: String,
    fund_code: String,
    points: Vec<NavPoint>,
    strategy: Box<dyn Strategy>,
    fee: FeeModel,
    initial_cash: f64,
) -> RunOutcome {
    let data = InMemoryData::new(points);
    let broker = Broker::new(fee);
    let portfolio = Portfolio::new(initial_cash);
    let mut engine = Engine::new(data, strategy, broker, portfolio);
    engine.run();
    let summary = metrics::summarize(engine.portfolio(), engine.trades().len());
    let daily = engine.daily().to_vec();
    RunOutcome { name, fund_code, summary, daily }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{FeeModel, SellTier};
    use crate::data::NavPoint;
    use crate::strategy::Period;
    use crate::strategy::dca::Dca;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn no_fee() -> FeeModel {
        FeeModel { buy_rate: 0.0, sell_tiers: vec![SellTier { max_days: 0, rate: 0.0 }] }
    }

    #[test]
    fn run_one_dca_flat_then_up() {
        let points = vec![
            NavPoint { date: d(2024, 1, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 15), nav: 2.0, acc_nav: 2.0 },
        ];
        let strategy: Box<dyn Strategy> = Box::new(Dca::new(Period::Monthly, 1, 1000.0));
        let outcome = run_one(
            "test_dca".to_string(),
            "161725".to_string(),
            points,
            strategy,
            no_fee(),
            0.0,
        );
        assert_eq!(outcome.daily.len(), 3, "expected 3 daily records");
        assert!((outcome.summary.final_equity - 4000.0).abs() < 1e-6,
            "expected final_equity≈4000, got {}", outcome.summary.final_equity);
        assert!((outcome.summary.total_contributed - 2000.0).abs() < 1e-6,
            "expected total_contributed≈2000, got {}", outcome.summary.total_contributed);
        assert_eq!(outcome.summary.trade_count, 2, "expected 2 trades");
    }
}

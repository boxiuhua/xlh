use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{Strategy, StrategyContext};

#[derive(Debug, Clone, Copy)]
pub enum Rule {
    TakeProfit { target_return: f64 }, // 相对持仓均价的收益达标 → 清仓
    StopLoss { max_drawdown: f64 },    // 自持仓市值峰值回撤达标 → 清仓
}

/// 包裹任意内层策略，叠加止盈/止损；触发时追加清仓信号。
pub struct RuleLayer {
    inner: Box<dyn Strategy>,
    rules: Vec<Rule>,
    peak_value: f64,
}

impl RuleLayer {
    pub fn new(inner: Box<dyn Strategy>, rules: Vec<Rule>) -> Self {
        Self { inner, rules, peak_value: 0.0 }
    }
}

impl Strategy for RuleLayer {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let mut sigs = self.inner.on_market(ctx);

        // 止盈/止损只能依据**最近已公布净值(T-1)**：你是看着昨天的净值决定今天赎回的，
        // 赎回则按今天的净值成交 —— 拿今天的净值判断今天该不该走，是未来函数。
        let Some(last_nav) = ctx.last_nav() else { return sigs };  // 首日无历史 → 无从判断

        let pos_value = ctx.shares * last_nav;
        if pos_value > self.peak_value { self.peak_value = pos_value; }

        if ctx.shares <= 1e-9 { return sigs; }

        let mut exit = false;
        for rule in &self.rules {
            match rule {
                Rule::TakeProfit { target_return } => {
                    if ctx.avg_cost > 0.0 && (last_nav / ctx.avg_cost - 1.0) >= *target_return {
                        exit = true;
                    }
                }
                Rule::StopLoss { max_drawdown } => {
                    if self.peak_value > 0.0 && (1.0 - pos_value / self.peak_value) >= *max_drawdown {
                        exit = true;
                    }
                }
            }
        }
        if exit {
            sigs.push(SignalEvent { date: ctx.today, direction: Direction::Sell, amount: SignalAmount::AllOut });
            self.peak_value = 0.0; // 清仓后重置峰值，便于再入场重新计
        }
        sigs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount, SignalEvent};
    use crate::strategy::{StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,day).unwrap()}

    struct Noop;
    impl Strategy for Noop { fn on_market(&mut self, _:&StrategyContext)->Vec<SignalEvent>{Vec::new()} }

    #[test]
    fn take_profit_triggers_all_out() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::TakeProfit{target_return:0.3}]);
        let today = MarketEvent{date:d(2024,6,1),nav:1.4,adj_nav:1.4};
        // 持仓成本1.0，现价1.4 → +40% ≥ 30% → 清仓
        let ctx = StrategyContext{today: today.date, history:std::slice::from_ref(&today), shares:100.0, avg_cost:1.0, cash:0.0};
        let sigs = s.on_market(&ctx);
        assert!(sigs.iter().any(|x| x.direction==Direction::Sell && x.amount==SignalAmount::AllOut));
    }

    #[test]
    fn stop_loss_on_drawdown_from_peak() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::StopLoss{max_drawdown:0.2}]);
        // 先记录高点市值，再跌破20%
        let peak = MarketEvent{date:d(2024,1,1),nav:2.0,adj_nav:2.0};
        let _ = s.on_market(&StrategyContext{today: peak.date, history:std::slice::from_ref(&peak), shares:100.0, avg_cost:1.0, cash:0.0});
        let drop = MarketEvent{date:d(2024,1,2),nav:1.5,adj_nav:1.5}; // 市值150 vs 峰200 → -25%
        let sigs = s.on_market(&StrategyContext{today: drop.date, history:std::slice::from_ref(&drop), shares:100.0, avg_cost:1.0, cash:0.0});
        assert!(sigs.iter().any(|x| x.direction==Direction::Sell));
    }

    #[test]
    fn no_trigger_without_position() {
        let mut s = RuleLayer::new(Box::new(Noop), vec![Rule::TakeProfit{target_return:0.1}]);
        let today = MarketEvent{date:d(2024,1,1),nav:5.0,adj_nav:5.0};
        let ctx = StrategyContext{today: today.date, history:std::slice::from_ref(&today), shares:0.0, avg_cost:0.0, cash:0.0};
        assert!(s.on_market(&ctx).is_empty());
    }
}

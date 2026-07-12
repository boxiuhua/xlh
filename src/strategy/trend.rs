use crate::event::{Direction, SignalAmount, SignalEvent};
use crate::strategy::{moving_average, Strategy, StrategyContext};

/// 双均线：短上穿长(金叉)买入固定金额；短下穿长(死叉)清仓。
pub struct Trend {
    short: usize,
    long: usize,
    amount: f64,
    prev_short_above: Option<bool>,
}

impl Trend {
    pub fn new(short: usize, long: usize, amount: f64) -> Self {
        Self { short, long, amount, prev_short_above: None }
    }
}

impl Strategy for Trend {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let (ms, ml) = match (moving_average(ctx.history, self.short), moving_average(ctx.history, self.long)) {
            (Some(s), Some(l)) => (s, l),
            _ => return Vec::new(),
        };
        let above = ms > ml;
        let mut out = Vec::new();
        if let Some(prev) = self.prev_short_above {
            if above && !prev {
                out.push(SignalEvent { date: ctx.today, direction: Direction::Buy, amount: SignalAmount::Cash(self.amount) });
            } else if !above && prev && ctx.shares > 1e-9 {
                out.push(SignalEvent { date: ctx.today, direction: Direction::Sell, amount: SignalAmount::AllOut });
            }
        }
        self.prev_short_above = Some(above);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount};
    use crate::strategy::{StrategyContext, Strategy};
    use chrono::NaiveDate;
    fn d(y:i32,m:u32,day:u32)->NaiveDate{NaiveDate::from_ymd_opt(y,m,(day).min(28)).unwrap()}

    fn run(prices: &[f64], short: usize, long: usize) -> Vec<crate::event::SignalEvent> {
        let mut s = Trend::new(short, long, 1000.0);
        let bars: Vec<MarketEvent> = prices.iter().enumerate()
            .map(|(i,p)| MarketEvent{date:d(2024,1,(i+1) as u32),nav:*p,adj_nav:*p}).collect();
        let mut out = Vec::new();
        for i in 0..bars.len() {
            let ctx = StrategyContext{today: bars[i].date, history:&bars[..i], shares: if i>0 {100.0} else {0.0}, avg_cost:1.0, cash:0.0};
            out.extend(s.on_market(&ctx));
        }
        out
    }

    #[test]
    fn golden_cross_buys_dead_cross_sells() {
        // 价格先跌后涨：短均线先在下方后上穿长均线→Buy；再下穿→Sell
        let prices = [3.0,2.0,1.0,1.0,2.0,3.0,4.0, 3.0,2.0,1.0];
        let sigs = run(&prices, 2, 4);
        assert!(sigs.iter().any(|s| s.direction==Direction::Buy));
        assert!(sigs.iter().any(|s| s.direction==Direction::Sell && s.amount==SignalAmount::AllOut));
    }
}

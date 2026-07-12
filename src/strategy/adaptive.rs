use crate::event::{Direction, SignalAmount, SignalEvent, MarketEvent};
use crate::strategy::{moving_average, Period, Schedule, Strategy, StrategyContext};

/// 自适应：按近端行情形态切换打法——
/// 上涨趋势→按计划定额加仓（顺势持有）；下跌趋势→清仓离场；震荡→RSI 高抛低吸。
pub struct Adaptive {
    schedule: Schedule,
    base_amount: f64,
    window: usize,
    ma_short: usize,
    ma_long: usize,
    up: f64,
    down: f64,
    rsi_window: usize,
    oversold: f64,
    overbought: f64,
    prev_rsi: Option<f64>,
}

impl Adaptive {
    pub fn new(period: Period, day: u32, base_amount: f64) -> Self {
        Self {
            schedule: Schedule::new(period, day),
            base_amount,
            window: 120, ma_short: 20, ma_long: 60,
            up: 0.10, down: -0.10,
            rsi_window: 14, oversold: 30.0, overbought: 70.0,
            prev_rsi: None,
        }
    }
}

fn window_return(history: &[MarketEvent], window: usize) -> Option<f64> {
    if history.len() < 2 { return None; }
    let n = window.min(history.len());
    let w = &history[history.len() - n..];
    let first = w.first().unwrap().adj_nav;
    if first <= 0.0 { return None; }
    Some(w.last().unwrap().adj_nav / first - 1.0)
}

fn rsi(history: &[MarketEvent], window: usize) -> Option<f64> {
    if window == 0 || history.len() < window + 1 { return None; }
    let slice = &history[history.len() - (window + 1)..];
    let (mut gain, mut loss) = (0.0, 0.0);
    for w in slice.windows(2) {
        let d = w[1].adj_nav - w[0].adj_nav;
        if d >= 0.0 { gain += d; } else { loss += -d; }
    }
    let (ag, al) = (gain / window as f64, loss / window as f64);
    if al == 0.0 { return Some(100.0); }
    Some(100.0 - 100.0 / (1.0 + ag / al))
}

impl Strategy for Adaptive {
    fn on_market(&mut self, ctx: &StrategyContext) -> Vec<SignalEvent> {
        let today = ctx.today;
        let cur_rsi = rsi(ctx.history, self.rsi_window);
        let mas = moving_average(ctx.history, self.ma_short);
        let mal = moving_average(ctx.history, self.ma_long);
        let wr = window_return(ctx.history, self.window);

        let out = match (mas, mal, wr) {
            (Some(ms), Some(ml), Some(ret)) => {
                // 全量数据就绪：正式三态判断
                if ret < self.down && ms < ml {
                    if ctx.shares > 1e-9 {
                        vec![SignalEvent { date: today, direction: Direction::Sell, amount: SignalAmount::AllOut }]
                    } else { Vec::new() }
                } else if ret > self.up && ms > ml {
                    if self.schedule.due(today) {
                        vec![SignalEvent { date: today, direction: Direction::Buy, amount: SignalAmount::Cash(self.base_amount) }]
                    } else { Vec::new() }
                } else {
                    let mut v = Vec::new();
                    if let (Some(prev), Some(cur)) = (self.prev_rsi, cur_rsi) {
                        if prev >= self.oversold && cur < self.oversold {
                            v.push(SignalEvent { date: today, direction: Direction::Buy, amount: SignalAmount::Cash(self.base_amount) });
                        } else if prev <= self.overbought && cur > self.overbought && ctx.shares > 1e-9 {
                            v.push(SignalEvent { date: today, direction: Direction::Sell, amount: SignalAmount::AllOut });
                        }
                    }
                    v
                }
            }
            _ => {
                // 数据不足时退化为定投，但要求：
                // 1) 至少有 2 根历史（能计算方向）
                // 2) 窗口收益 >= 0（未出现持续下跌才买入）
                let ok = matches!(wr, Some(r) if r >= 0.0);
                if ok && self.schedule.due(today) {
                    vec![SignalEvent { date: today, direction: Direction::Buy, amount: SignalAmount::Cash(self.base_amount) }]
                } else { Vec::new() }
            }
        };
        self.prev_rsi = cur_rsi;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Direction, MarketEvent, SignalAmount};
    use crate::strategy::{Period, StrategyContext, Strategy};
    use chrono::NaiveDate;

    fn bars(navs: &[f64]) -> Vec<MarketEvent> {
        navs.iter().enumerate().map(|(i, v)| MarketEvent {
            date: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, adj_nav: *v,
        }).collect()
    }

    fn run(navs: &[f64], shares: f64) -> Vec<SignalEvent> {
        let mut s = Adaptive::new(Period::Monthly, 1, 1000.0);
        let bs = bars(navs);
        let mut out = Vec::new();
        for i in 0..bs.len() {
            let ctx = StrategyContext { today: bs[i].date, history: &bs[..i], shares: if i > 0 { shares } else { 0.0 }, avg_cost: 1.0, cash: 0.0 };
            out.extend(s.on_market(&ctx));
        }
        out
    }

    #[test]
    fn warmup_behaves_like_dca() {
        // 30 根（不足 ma_long=60）→ 退化定投，月初(1号)买入
        let navs: Vec<f64> = (0..30).map(|_| 1.0).collect();
        let sigs = run(&navs, 0.0);
        assert!(sigs.iter().any(|s| s.direction == Direction::Buy), "预热期应像定投一样买入");
    }

    #[test]
    fn downtrend_exits() {
        // 70 根下跌 2.0->1.0：下跌趋势→清仓，无买入
        let navs: Vec<f64> = (0..70).map(|i| 2.0 - i as f64 / 69.0).collect();
        let sigs = run(&navs, 100.0);
        assert!(sigs.iter().any(|s| s.direction == Direction::Sell && s.amount == SignalAmount::AllOut), "下跌应清仓");
        assert!(!sigs.iter().any(|s| s.direction == Direction::Buy), "下跌不应买入");
    }

    #[test]
    fn uptrend_accumulates() {
        // 70 根上涨 1.0->2.0：上涨趋势→按计划买入，不清仓
        let navs: Vec<f64> = (0..70).map(|i| 1.0 + i as f64 / 69.0).collect();
        let sigs = run(&navs, 100.0);
        assert!(sigs.iter().any(|s| s.direction == Direction::Buy), "上涨应加仓");
        assert!(!sigs.iter().any(|s| s.direction == Direction::Sell), "上涨不应清仓");
    }
}

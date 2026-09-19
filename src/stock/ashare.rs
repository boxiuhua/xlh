//! A 股交易规则(纯函数)与 A 股成交模型。
//! 规则只在此处定义一次:回测成交模型与交易闸门共用。

use crate::broker::Broker;
use crate::event::{Direction, MarketEvent, OrderEvent, OrderQty};
use crate::execution::{CloseExecution, ExecBar, ExecutionModel, Prepared, RejectReason};

fn is_star(code: &str) -> bool {
    code.starts_with("688") || code.starts_with("689")
}

fn is_chinext(code: &str) -> bool {
    code.starts_with("300") || code.starts_with("301")
}

/// 北交所判定不依赖调用方的检查顺序:即使脱离 `limit_ratio`/`buy_lot` 单独调用,
/// 也不得因 `starts_with('8')` 误判科创板代码(自成一体地排除 is_star)。
fn is_bse(code: &str) -> bool {
    !is_star(code) && (code.starts_with('8') || code.starts_with("43") || code.starts_with("92"))
}

fn is_etf(code: &str) -> bool {
    ["50", "51", "52", "56", "58", "15", "16", "18"]
        .iter()
        .any(|p| code.starts_with(p))
}

/// 涨跌幅比例。`name` 未知时传 None(视为非 ST)。
/// 创业板/科创板无论是否 ST 均为 20%。
pub fn limit_ratio(code: &str, name: Option<&str>) -> f64 {
    if is_star(code) || is_chinext(code) {
        return 0.20;
    }
    if is_bse(code) {
        return 0.30;
    }
    if name.is_some_and(|n| n.to_uppercase().contains("ST")) {
        0.05
    } else {
        0.10
    }
}

/// 最小价位小数位:场内基金 0.001,股票 0.01。
pub fn price_decimals(code: &str) -> i32 {
    if is_etf(code) {
        3
    } else {
        2
    }
}

fn round_to(x: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (x * m).round() / m
}

/// 除权除息参考价:数据按后复权存储(`adj_close = close * f_t`),
/// 若今日发生除权除息(送股/转增/分红),不复权前收不能直接作为涨跌停基准，
/// 须换算成「今日除权后」的参考价 = `prev_adj_close * close / adj_close`。
/// 无复权前收(数据首根且未提供 prev bar)时回退到不复权前收。
fn prev_ref_price(bar: &ExecBar) -> Result<f64, RejectReason> {
    let prev_ref = if let Some(pa) = bar.prev_adj_close.filter(|pa| *pa > 0.0) {
        pa * bar.close / bar.adj_close
    } else {
        bar.prev_close.ok_or(RejectReason::NoPrevClose)?
    };
    if !prev_ref.is_finite() || prev_ref <= 0.0 {
        return Err(RejectReason::NoPrevClose);
    }
    Ok(prev_ref)
}

/// (涨停价, 跌停价),按最小价位四舍五入。
pub fn limit_prices(prev_close: f64, ratio: f64, decimals: i32) -> (f64, f64) {
    (
        round_to(prev_close * (1.0 + ratio), decimals),
        round_to(prev_close * (1.0 - ratio), decimals),
    )
}

/// 买入数量约束:至少 `min` 股,超出部分按 `step` 递增。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuyLot {
    pub min: u64,
    pub step: u64,
}

pub fn buy_lot(code: &str) -> BuyLot {
    if is_star(code) {
        BuyLot { min: 200, step: 1 }
    } else if is_bse(code) {
        BuyLot { min: 100, step: 1 }
    } else {
        BuyLot {
            min: 100,
            step: 100,
        }
    }
}

/// 把「理论可买股数」向下取整到合法数量;不足最小数量返回 0。
pub fn round_buy_shares(raw_shares: f64, lot: BuyLot) -> u64 {
    if !raw_shares.is_finite() || raw_shares < lot.min as f64 {
        return 0;
    }
    let n = (raw_shares + 1e-6).floor() as u64;
    lot.min + (n - lot.min) / lot.step * lot.step
}

/// 减少一个步长;低于最小数量返回 0。
pub fn step_down(n: u64, lot: BuyLot) -> u64 {
    if n >= lot.min + lot.step {
        n - lot.step
    } else {
        0
    }
}

/// 按最小价位取整,方向对交易者不利(保守):买入向上、卖出向下。
pub fn round_price_to_tick(price: f64, decimals: i32, side: Direction) -> f64 {
    let m = 10f64.powi(decimals);
    match side {
        Direction::Buy => ((price * m) - 1e-6).ceil() / m,
        Direction::Sell => ((price * m) + 1e-6).floor() / m,
    }
}

/// 含滑点的成交价:买 ×(1+s)、卖 ×(1−s),按最小价位取整,且不越过涨跌停价。
/// 无涨跌停限制时传 `f64::INFINITY` / `0.0`。
pub fn slipped_price(
    side: Direction,
    price: f64,
    slippage: f64,
    decimals: i32,
    limit_up: f64,
    limit_down: f64,
) -> f64 {
    match side {
        Direction::Buy => {
            round_price_to_tick(price * (1.0 + slippage), decimals, Direction::Buy).min(limit_up)
        }
        Direction::Sell => {
            round_price_to_tick(price * (1.0 - slippage), decimals, Direction::Sell).max(limit_down)
        }
    }
}

/// 卖出股数:想卖 ≥ 可卖 → 全部可卖(允许零股);否则按步长向下取整;
/// 科创板部分卖出不得少于 200 股。
pub fn sell_qty(code: &str, want: u64, sellable: u64) -> u64 {
    if sellable == 0 || want == 0 {
        return 0;
    }
    if want >= sellable {
        return sellable;
    }
    let lot = buy_lot(code);
    let n = want / lot.step * lot.step;
    if is_star(code) && n < lot.min {
        return 0;
    }
    n
}

/// A 股成交口径:T 日开盘价 ± 滑点(不越过涨跌停价)、整手、开盘涨停不买/跌停不卖、T+1。
///
/// 涨跌停与整手按**不复权**价计算;交给 Broker 的价格与份额换算回复权尺度,
/// 保证「份额 × 价格」等于真实成交金额。
pub struct AShareExecution {
    ratio: f64,
    decimals: i32,
    lot: BuyLot,
    slippage: f64,
}

impl AShareExecution {
    pub const DEFAULT_SLIPPAGE: f64 = 0.001;

    pub fn new(code: &str, name: Option<&str>, slippage: f64) -> Self {
        Self {
            ratio: limit_ratio(code, name),
            decimals: price_decimals(code),
            lot: buy_lot(code),
            slippage,
        }
    }
}

impl ExecutionModel for AShareExecution {
    fn name(&self) -> &'static str {
        "a_share"
    }

    fn prepare(
        &self,
        order: &OrderEvent,
        _today: &MarketEvent,
        bar: Option<&ExecBar>,
        broker: &Broker,
    ) -> Result<Prepared, RejectReason> {
        let bar = bar.ok_or(RejectReason::NoPrice)?;
        if bar.open <= 0.0 || bar.close <= 0.0 || bar.adj_close <= 0.0 {
            return Err(RejectReason::NoPrice);
        }
        let prev_ref = prev_ref_price(bar)?;
        let (up, down) = limit_prices(prev_ref, self.ratio, self.decimals);
        let eps = 0.5 * 10f64.powi(-self.decimals);
        // 复权因子:复权尺度 = 不复权 × factor
        let factor = bar.adj_close / bar.close;

        match order.direction {
            Direction::Buy => {
                if bar.open >= up - eps {
                    return Err(RejectReason::LimitUp);
                }
                let OrderQty::Cash(budget) = order.qty else {
                    return Err(RejectReason::UnsupportedQty);
                };
                let raw_price = slipped_price(
                    Direction::Buy,
                    bar.open,
                    self.slippage,
                    self.decimals,
                    up,
                    down,
                );
                let mut n = round_buy_shares(budget / raw_price, self.lot);
                while n > 0 {
                    let value = n as f64 * raw_price;
                    if value + broker.buy_fee(value) <= budget + 1e-9 {
                        break;
                    }
                    n = step_down(n, self.lot);
                }
                if n == 0 {
                    return Err(RejectReason::BelowOneLot);
                }
                Ok(Prepared {
                    order: OrderEvent {
                        date: order.date,
                        direction: Direction::Buy,
                        qty: OrderQty::Shares(n as f64 / factor),
                    },
                    price: raw_price * factor,
                })
            }
            Direction::Sell => {
                if bar.open <= down + eps {
                    return Err(RejectReason::LimitDown);
                }
                let sellable = broker.sellable_shares(order.date);
                if sellable <= 1e-9 {
                    return Err(RejectReason::NothingSellable);
                }
                let want = match order.qty {
                    OrderQty::AllShares => sellable,
                    OrderQty::Shares(s) => s.min(sellable),
                    OrderQty::Cash(_) => return Err(RejectReason::UnsupportedQty),
                };
                let shares = if want >= sellable - 1e-9 {
                    // 清掉全部可卖份额:允许零股
                    sellable
                } else {
                    let raw_n =
                        ((want * factor + 1e-6).floor() as u64) / self.lot.step * self.lot.step;
                    if raw_n == 0 {
                        return Err(RejectReason::BelowOneLot);
                    }
                    raw_n as f64 / factor
                };
                let raw_price = slipped_price(
                    Direction::Sell,
                    bar.open,
                    self.slippage,
                    self.decimals,
                    up,
                    down,
                );
                Ok(Prepared {
                    order: OrderEvent {
                        date: order.date,
                        direction: Direction::Sell,
                        qty: OrderQty::Shares(shares),
                    },
                    price: raw_price * factor,
                })
            }
        }
    }
}

/// 按市场选择成交口径:港股(116)、美股(105..=107)沿用收盘成交,其余按 A 股规则。
pub fn execution_for_market(market: u16, code: &str) -> Box<dyn ExecutionModel> {
    match market {
        116 | 105..=107 => Box::new(CloseExecution),
        _ => Box::new(AShareExecution::new(
            code,
            None,
            AShareExecution::DEFAULT_SLIPPAGE,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    use crate::broker::Broker;
    use crate::event::{Direction, MarketEvent, OrderEvent, OrderQty};
    use crate::execution::{ExecBar, ExecutionModel, RejectReason};
    use crate::stock::fee::StockFee;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    fn today() -> MarketEvent {
        MarketEvent {
            date: d(2024, 1, 3),
            nav: 0.0,
            adj_nav: 0.0,
        }
    }
    fn xbar(open: f64, close: f64, adj_close: f64, prev: Option<f64>) -> ExecBar {
        ExecBar {
            open,
            close,
            adj_close,
            prev_close: prev,
            prev_adj_close: None,
        }
    }
    fn buy(cash: f64) -> OrderEvent {
        OrderEvent {
            date: d(2024, 1, 3),
            direction: Direction::Buy,
            qty: OrderQty::Cash(cash),
        }
    }
    fn sell(qty: OrderQty, date: NaiveDate) -> OrderEvent {
        OrderEvent {
            date,
            direction: Direction::Sell,
            qty,
        }
    }
    /// 1/2 买入 `shares` 股 @10 的持仓。
    fn holding(shares: f64) -> Broker {
        let mut b = Broker::new(StockFee::a_share());
        b.execute(
            &OrderEvent {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                qty: OrderQty::Shares(shares),
            },
            10.0,
        );
        b
    }
    fn shares_of(o: &OrderEvent) -> f64 {
        match o.qty {
            OrderQty::Shares(s) => s,
            other => panic!("应为 Shares,实际 {other:?}"),
        }
    }

    #[test]
    fn buy_fills_at_open_plus_slippage_in_lots() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(
                &buy(10000.0),
                &today(),
                Some(&xbar(10.0, 10.5, 10.5, Some(10.0))),
                &b,
            )
            .unwrap();
        // 10.01 → 999 股 → 900;9009 + 5.09 ≤ 10000
        assert!(close(p.price, 10.01), "price={}", p.price);
        assert!(close(shares_of(&p.order), 900.0));
        assert_eq!(ex.name(), "a_share");
    }

    #[test]
    fn buy_steps_down_when_fee_exceeds_budget() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let r = ex.prepare(
            &buy(1005.0),
            &today(),
            Some(&xbar(10.0, 10.0, 10.0, Some(10.0))),
            &b,
        );
        // 100 股 = 1000 + 5.01 费 > 1005
        assert_eq!(r.unwrap_err(), RejectReason::BelowOneLot);
    }

    #[test]
    fn buy_rejected_at_limit_up_by_board() {
        let b = Broker::new(StockFee::a_share());
        let bar = xbar(11.0, 11.0, 11.0, Some(10.0));
        let main = AShareExecution::new("600000", None, 0.001);
        assert_eq!(
            main.prepare(&buy(10000.0), &today(), Some(&bar), &b)
                .unwrap_err(),
            RejectReason::LimitUp
        );
        let star = AShareExecution::new("688001", None, 0.001);
        assert!(
            star.prepare(&buy(10000.0), &today(), Some(&bar), &b)
                .is_ok(),
            "科创板 20%"
        );
        let st = AShareExecution::new("600000", Some("*ST某某"), 0.001);
        assert_eq!(
            st.prepare(
                &buy(10000.0),
                &today(),
                Some(&xbar(10.5, 10.5, 10.5, Some(10.0))),
                &b
            )
            .unwrap_err(),
            RejectReason::LimitUp
        );
    }

    #[test]
    fn slippage_is_capped_at_limit_up() {
        let ex = AShareExecution::new("600000", None, 0.05);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(
                &buy(100000.0),
                &today(),
                Some(&xbar(10.9, 10.9, 10.9, Some(10.0))),
                &b,
            )
            .unwrap();
        assert!(close(p.price, 11.0), "price={}", p.price);
    }

    #[test]
    fn buy_converts_lots_into_adjusted_units() {
        // 复权因子 2:不复权 10 元 ↔ 复权 20 元
        let ex = AShareExecution::new("600000", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(
                &buy(10000.0),
                &today(),
                Some(&xbar(10.0, 10.0, 20.0, Some(10.0))),
                &b,
            )
            .unwrap();
        // 1000 股需 10005.1 > 预算 → 900 股;复权尺度 450 份 @20,市值同为 9000
        assert!(close(p.price, 20.0));
        assert!(close(shares_of(&p.order), 450.0));
    }

    #[test]
    fn star_board_buys_in_single_shares_above_200() {
        let ex = AShareExecution::new("688001", None, 0.0);
        let b = Broker::new(StockFee::a_share());
        let p = ex
            .prepare(
                &buy(3000.0),
                &today(),
                Some(&xbar(10.0, 10.0, 10.0, Some(10.0))),
                &b,
            )
            .unwrap();
        // 300 股 3000+5.03 超预算 → 299 股 2990+5.0299
        assert!(close(shares_of(&p.order), 299.0));
    }

    #[test]
    fn sell_respects_t_plus_one() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let mut b = Broker::new(StockFee::a_share());
        b.execute(&buy_shares_on(d(2024, 1, 3), 1000.0), 10.0);
        let bar = xbar(10.0, 10.0, 10.0, Some(10.0));
        assert_eq!(
            ex.prepare(
                &sell(OrderQty::AllShares, d(2024, 1, 3)),
                &today(),
                Some(&bar),
                &b
            )
            .unwrap_err(),
            RejectReason::NothingSellable
        );
        let p = ex
            .prepare(
                &sell(OrderQty::AllShares, d(2024, 1, 4)),
                &today(),
                Some(&bar),
                &b,
            )
            .unwrap();
        assert!(close(shares_of(&p.order), 1000.0));
        assert!(close(p.price, 9.99));
    }

    fn buy_shares_on(date: NaiveDate, shares: f64) -> OrderEvent {
        OrderEvent {
            date,
            direction: Direction::Buy,
            qty: OrderQty::Shares(shares),
        }
    }

    #[test]
    fn sell_rejected_at_limit_down() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = holding(1000.0);
        let r = ex.prepare(
            &sell(OrderQty::AllShares, d(2024, 1, 3)),
            &today(),
            Some(&xbar(9.0, 9.0, 9.0, Some(10.0))),
            &b,
        );
        assert_eq!(r.unwrap_err(), RejectReason::LimitDown);
    }

    #[test]
    fn partial_sell_floors_to_step_but_full_exit_allows_odd_lot() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let bar = xbar(10.0, 10.0, 10.0, Some(10.0));
        let b = holding(1050.0);
        let p = ex
            .prepare(
                &sell(OrderQty::Shares(250.0), d(2024, 1, 3)),
                &today(),
                Some(&bar),
                &b,
            )
            .unwrap();
        assert!(close(shares_of(&p.order), 200.0));
        assert_eq!(
            ex.prepare(
                &sell(OrderQty::Shares(50.0), d(2024, 1, 3)),
                &today(),
                Some(&bar),
                &b
            )
            .unwrap_err(),
            RejectReason::BelowOneLot
        );
        let all = ex
            .prepare(
                &sell(OrderQty::AllShares, d(2024, 1, 3)),
                &today(),
                Some(&bar),
                &b,
            )
            .unwrap();
        assert!(close(shares_of(&all.order), 1050.0), "清仓允许零股");
    }

    /// F3: 复权因子 ≠ 1 时的部分卖出。持仓 500 复权份额(=1000 不复权股,前日以复权价 20 买入)。
    /// 卖出 Shares(125.0)(=250 不复权股)→ 按 100 股步长向下取整为 200 不复权股 = 复权 100 份，
    /// 价格 = 10 * (1-slippage) * factor = 20.0(slippage=0)。
    #[test]
    fn partial_sell_with_adjustment_factor_not_one() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let bar = xbar(10.0, 10.0, 20.0, Some(10.0)); // 今日 close=10, adj_close=20 → factor=2
        let mut b = Broker::new(StockFee::a_share());
        b.execute(
            &OrderEvent {
                date: d(2024, 1, 2),
                direction: Direction::Buy,
                qty: OrderQty::Shares(500.0), // 复权份额 500 = 不复权 1000 股
            },
            20.0, // 复权价
        );
        let p = ex
            .prepare(
                &sell(OrderQty::Shares(125.0), d(2024, 1, 3)), // 复权 125 份 = 不复权 250 股
                &today(),
                Some(&bar),
                &b,
            )
            .unwrap();
        // 250 股按 100 步长向下取整 → 200 股(不复权) = 100 份(复权)
        assert!(close(shares_of(&p.order), 100.0), "order={:?}", p.order);
        assert!(close(p.price, 20.0), "price={}", p.price);

        // 复权 20 份 = 不复权 40 股，不足一手(100 股步长)
        assert_eq!(
            ex.prepare(
                &sell(OrderQty::Shares(20.0), d(2024, 1, 3)),
                &today(),
                Some(&bar),
                &b
            )
            .unwrap_err(),
            RejectReason::BelowOneLot
        );
    }

    #[test]
    fn missing_prev_close_or_bar_is_rejected() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = Broker::new(StockFee::a_share());
        assert_eq!(
            ex.prepare(
                &buy(10000.0),
                &today(),
                Some(&xbar(10.0, 10.0, 10.0, None)),
                &b
            )
            .unwrap_err(),
            RejectReason::NoPrevClose
        );
        assert_eq!(
            ex.prepare(&buy(10000.0), &today(), None, &b).unwrap_err(),
            RejectReason::NoPrice
        );
    }

    /// F2: 除权除息日(10 送 10)——涨跌停基准须用除权参考价，而非未复权前收。
    /// 前收(不复权) 20，复权前收 20；今日不复权 open/close 10，复权 close 20(除权因子翻倍)。
    /// 除权参考价 = 20 * 10 / 20 = 10 → 卖出 open=10 不该跌停；买入 open=11.0 应涨停(10*1.10=11.00)。
    #[test]
    fn bonus_share_ex_rights_uses_adjusted_reference_price() {
        let ex = AShareExecution::new("600000", None, 0.0);
        let bar_today = ExecBar {
            open: 10.0,
            close: 10.0,
            adj_close: 20.0,
            prev_close: Some(20.0),
            prev_adj_close: Some(20.0),
        };
        // 卖出：参考价 10，跌停价 9.00，open=10 未跌停，应成交
        let b = holding(1000.0);
        let sell_p = ex
            .prepare(
                &sell(OrderQty::AllShares, d(2024, 1, 3)),
                &today(),
                Some(&bar_today),
                &b,
            )
            .expect("除权日参考价应为 10，卖出不应判跌停");
        assert!(close(sell_p.price, 20.0), "price={}", sell_p.price);

        // 买入：涨停价 11.00，open=11.0 触及涨停应拒绝
        let buy_bar = ExecBar {
            open: 11.0,
            close: 11.0,
            adj_close: 22.0,
            prev_close: Some(20.0),
            prev_adj_close: Some(20.0),
        };
        let empty = Broker::new(StockFee::a_share());
        assert_eq!(
            ex.prepare(&buy(10000.0), &today(), Some(&buy_bar), &empty)
                .unwrap_err(),
            RejectReason::LimitUp
        );
    }

    #[test]
    fn zero_prev_close_without_prev_adj_close_is_no_prev_close() {
        let ex = AShareExecution::new("600000", None, 0.001);
        let b = Broker::new(StockFee::a_share());
        let bar_today = ExecBar {
            open: 10.0,
            close: 10.0,
            adj_close: 10.0,
            prev_close: Some(0.0),
            prev_adj_close: None,
        };
        assert_eq!(
            ex.prepare(&buy(10000.0), &today(), Some(&bar_today), &b)
                .unwrap_err(),
            RejectReason::NoPrevClose
        );
    }

    #[test]
    fn execution_for_market_picks_model() {
        assert_eq!(execution_for_market(1, "600000").name(), "a_share");
        assert_eq!(execution_for_market(0, "000001").name(), "a_share");
        assert_eq!(execution_for_market(116, "00700").name(), "close");
        assert_eq!(execution_for_market(105, "AAPL").name(), "close");
    }

    /// 端到端:经引擎运行,记录拒单原因并按开盘价成交。
    #[test]
    fn engine_records_rejections_and_fills_at_open() {
        use crate::engine::Engine;
        use crate::portfolio::Portfolio;
        use crate::stock::data::{StockBar, StockData};
        use crate::strategy::dca::Dca;
        use crate::strategy::Period;
        let sb = |date: NaiveDate, open: f64, close: f64| StockBar {
            date,
            open,
            high: open.max(close),
            low: open.min(close),
            close,
            volume: 1.0,
            adj_close: close,
        };
        let bars = vec![
            sb(d(2024, 1, 1), 10.0, 10.0), // 定投日,首根无前收 → 拒
            sb(d(2024, 2, 1), 11.0, 11.0), // 定投日,开盘涨停 → 拒
            sb(d(2024, 3, 1), 11.0, 11.0), // 定投日,正常成交
        ];
        let mut e = Engine::new(
            StockData::new(bars),
            Dca::new(Period::Monthly, 1, 10000.0),
            Broker::new(StockFee::a_share()),
            Portfolio::new(0.0),
        )
        .with_execution(Box::new(AShareExecution::new("600000", None, 0.001)));
        e.run();
        let reasons: Vec<_> = e.rejected().iter().map(|r| r.reason).collect();
        assert_eq!(
            reasons,
            vec![RejectReason::NoPrevClose, RejectReason::LimitUp]
        );
        assert_eq!(e.trades().len(), 1);
        let t = &e.trades()[0];
        assert_eq!(t.date, d(2024, 3, 1));
        assert!(close(t.shares, 900.0), "shares={}", t.shares);
        assert!(close(t.price, 11.02), "price={}", t.price);
    }

    #[test]
    fn limit_ratio_by_board_and_st() {
        assert!(close(limit_ratio("600000", None), 0.10));
        assert!(close(limit_ratio("000001", Some("ST 某某")), 0.05));
        assert!(close(limit_ratio("600001", Some("*st某某")), 0.05));
        assert!(
            close(limit_ratio("300750", Some("ST 某某")), 0.20),
            "创业板 ST 仍 20%"
        );
        assert!(close(limit_ratio("301001", None), 0.20));
        assert!(close(limit_ratio("688981", None), 0.20));
        assert!(close(limit_ratio("830799", None), 0.30));
        assert!(close(limit_ratio("430047", None), 0.30));
        assert!(
            close(limit_ratio("689009", None), 0.20),
            "689 开头属科创板,不应被 is_bse 的 '8' 前缀误判为北交所"
        );
    }

    #[test]
    fn is_bse_excludes_star_board_codes() {
        assert!(!is_bse("688001"), "688 开头是科创板,不是北交所");
        assert!(!is_bse("689009"), "689 开头是科创板,不是北交所");
        assert!(is_bse("830799"));
        assert!(is_bse("430047"));
    }

    #[test]
    fn limit_prices_round_to_tick() {
        let (up, down) = limit_prices(10.0, 0.10, 2);
        assert!(close(up, 11.0) && close(down, 9.0));
        let (up, down) = limit_prices(3.33, 0.10, 2);
        assert!(close(up, 3.66) && close(down, 3.00), "up={up} down={down}");
        assert_eq!(price_decimals("510300"), 3);
        assert_eq!(price_decimals("600000"), 2);
        let (up, down) = limit_prices(3.456, 0.10, 3);
        assert!(
            close(up, 3.802) && close(down, 3.110),
            "up={up} down={down}"
        );
    }

    #[test]
    fn buy_lot_by_board() {
        assert_eq!(
            buy_lot("600000"),
            BuyLot {
                min: 100,
                step: 100
            }
        );
        assert_eq!(
            buy_lot("510300"),
            BuyLot {
                min: 100,
                step: 100
            }
        );
        assert_eq!(buy_lot("688001"), BuyLot { min: 200, step: 1 });
        assert_eq!(buy_lot("689009"), BuyLot { min: 200, step: 1 });
        assert_eq!(buy_lot("830799"), BuyLot { min: 100, step: 1 });
    }

    #[test]
    fn round_buy_shares_floors_to_lot() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(round_buy_shares(999.0, main), 900);
        assert_eq!(round_buy_shares(99.9, main), 0);
        assert_eq!(round_buy_shares(100.0, main), 100);
        assert_eq!(round_buy_shares(299.5, star), 299);
        assert_eq!(round_buy_shares(150.0, star), 0);
        assert_eq!(round_buy_shares(f64::NAN, main), 0);
    }

    #[test]
    fn step_down_stops_at_min() {
        let main = buy_lot("600000");
        let star = buy_lot("688001");
        assert_eq!(step_down(900, main), 800);
        assert_eq!(step_down(100, main), 0);
        assert_eq!(step_down(201, star), 200);
        assert_eq!(step_down(200, star), 0);
    }

    #[test]
    fn round_price_to_tick_is_adverse_to_trader() {
        assert!(close(round_price_to_tick(11.011, 2, Direction::Buy), 11.02));
        assert!(close(
            round_price_to_tick(11.011, 2, Direction::Sell),
            11.01
        ));
        assert!(close(
            round_price_to_tick(10.0 * 1.001, 2, Direction::Buy),
            10.01
        ));
        assert!(close(
            round_price_to_tick(10.0 * 0.999, 2, Direction::Sell),
            9.99
        ));
        assert!(close(round_price_to_tick(3.4561, 3, Direction::Buy), 3.457));
    }

    #[test]
    fn slipped_price_respects_limits() {
        assert!(close(
            slipped_price(Direction::Buy, 10.9, 0.05, 2, 11.0, 9.0),
            11.0
        ));
        assert!(close(
            slipped_price(Direction::Sell, 9.1, 0.05, 2, 11.0, 9.0),
            9.0
        ));
        assert!(close(
            slipped_price(Direction::Buy, 10.0, 0.001, 2, f64::INFINITY, 0.0),
            10.01
        ));
    }

    #[test]
    fn sell_qty_by_board() {
        assert_eq!(sell_qty("600000", 250, 1050), 200);
        assert_eq!(sell_qty("600000", 50, 1050), 0);
        assert_eq!(
            sell_qty("600000", 2000, 1050),
            1050,
            "超出可卖 → 全部可卖(允许零股)"
        );
        assert_eq!(sell_qty("600000", 1050, 1050), 1050);
        assert_eq!(sell_qty("688001", 150, 300), 0, "科创板部分卖出不足 200 股");
        assert_eq!(sell_qty("688001", 250, 300), 250);
        assert_eq!(sell_qty("688001", 150, 150), 150, "清仓允许不足 200 股");
        assert_eq!(sell_qty("600000", 100, 0), 0);
    }

    #[test]
    fn lof_prefix_50_uses_three_decimals() {
        assert_eq!(price_decimals("501018"), 3);
    }
}

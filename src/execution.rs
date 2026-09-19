//! 成交模型:把引擎产生的订单变成「以什么价格、成交多少」,或说明为什么不能成交。
//!
//! 基金沿用 `CloseExecution`(按当日复权净值成交,与历史行为逐位一致);
//! A 股使用 `stock::ashare::AShareExecution`(开盘价 + 滑点、整手、涨跌停、T+1)。

use crate::broker::Broker;
use crate::event::{Direction, MarketEvent, OrderEvent};
use chrono::NaiveDate;
use serde::Serialize;

/// 成交模型需要、但**策略不可见**的当日原始行情。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExecBar {
    /// 不复权开盘价
    pub open: f64,
    /// 不复权收盘价
    pub close: f64,
    /// 复权收盘价(与 `MarketEvent::adj_nav` 同尺度)
    pub adj_close: f64,
    /// 前一交易日不复权收盘价;数据首根 bar 为 None
    pub prev_close: Option<f64>,
    /// 前一交易日复权收盘价;数据首根 bar 且未提供 prev bar 时为 None
    pub prev_adj_close: Option<f64>,
}

/// 可交给 `Broker::execute` 的订单与成交价(复权尺度)。
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub order: OrderEvent,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// 开盘即涨停,买不进
    LimitUp,
    /// 开盘即跌停,卖不出
    LimitDown,
    /// 无前收,无法计算涨跌停价
    NoPrevClose,
    /// 预算不足一手 / 部分卖出不足一个步长
    BelowOneLot,
    /// 无可卖份额(含 T+1 限制)
    NothingSellable,
    /// 无有效报价
    NoPrice,
    /// 该成交模型不支持的数量类型
    UnsupportedQty,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RejectedOrder {
    pub date: NaiveDate,
    pub direction: Direction,
    pub reason: RejectReason,
}

/// 需 Send:后台评估线程会持有成交模型。
pub trait ExecutionModel: Send {
    /// 口径名称,输出到回测结果供页面展示。
    fn name(&self) -> &'static str;

    fn prepare(
        &self,
        order: &OrderEvent,
        today: &MarketEvent,
        bar: Option<&ExecBar>,
        broker: &Broker,
    ) -> Result<Prepared, RejectReason>;
}

/// 历史行为:订单原样、按当日复权价成交。
pub struct CloseExecution;

impl ExecutionModel for CloseExecution {
    fn name(&self) -> &'static str {
        "close"
    }

    fn prepare(
        &self,
        order: &OrderEvent,
        today: &MarketEvent,
        _bar: Option<&ExecBar>,
        _broker: &Broker,
    ) -> Result<Prepared, RejectReason> {
        Ok(Prepared {
            order: order.clone(),
            price: today.adj_nav,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::OrderQty;
    use crate::stock::fee::StockFee;

    #[test]
    fn close_execution_passes_order_through_at_adj_nav() {
        let date = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let order = OrderEvent {
            date,
            direction: Direction::Buy,
            qty: OrderQty::Cash(1000.0),
        };
        let today = MarketEvent {
            date,
            nav: 1.0,
            adj_nav: 1.23,
        };
        let broker = Broker::new(StockFee::a_share());
        let p = CloseExecution
            .prepare(&order, &today, None, &broker)
            .expect("CloseExecution 从不拒绝");
        assert_eq!(p.order, order);
        assert!((p.price - 1.23).abs() < 1e-12);
        assert_eq!(CloseExecution.name(), "close");
    }

    #[test]
    fn reject_reason_serializes_snake_case() {
        let j = serde_json::to_string(&RejectReason::BelowOneLot).unwrap();
        assert_eq!(j, "\"below_one_lot\"");
    }

    #[test]
    fn execution_models_are_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<Box<dyn ExecutionModel>>();
    }
}

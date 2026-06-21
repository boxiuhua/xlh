use chrono::NaiveDate;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction { Buy, Sell }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalAmount { Cash(f64), Ratio(f64), AllOut }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OrderQty { Cash(f64), Shares(f64), AllShares }

#[derive(Debug, Clone, PartialEq)]
pub struct MarketEvent { pub date: NaiveDate, pub nav: f64, pub adj_nav: f64 }

#[derive(Debug, Clone, PartialEq)]
pub struct SignalEvent { pub date: NaiveDate, pub direction: Direction, pub amount: SignalAmount }

#[derive(Debug, Clone, PartialEq)]
pub struct OrderEvent { pub date: NaiveDate, pub direction: Direction, pub qty: OrderQty }

#[derive(Debug, Clone, PartialEq)]
pub struct FillEvent { pub date: NaiveDate, pub direction: Direction, pub shares: f64, pub price: f64, pub fee: f64 }

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Market(MarketEvent),
    Signal(SignalEvent),
    Order(OrderEvent),
    Fill(FillEvent),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_wraps_market() {
        let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
        let ev = Event::Market(MarketEvent { date: d, nav: 1.0, adj_nav: 1.0 });
        assert!(matches!(ev, Event::Market(_)));
    }
}

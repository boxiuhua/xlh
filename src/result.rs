use chrono::NaiveDate;
use serde::Serialize;
use crate::event::Direction;

#[derive(Debug, Clone, Serialize)]
pub struct DailyRecord {
    pub date: NaiveDate,
    pub nav: f64,
    pub adj_nav: f64,
    pub equity: f64,
    pub contribution: f64,
    pub shares: f64,
    pub cash: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TradeRecord {
    pub date: NaiveDate,
    pub direction: Direction,
    pub shares: f64,
    pub price: f64,
    pub fee: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Direction::Buy).unwrap(), "\"buy\"");
        assert_eq!(serde_json::to_string(&Direction::Sell).unwrap(), "\"sell\"");
    }
}

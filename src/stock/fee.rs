use crate::broker::Fee;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StockFee {
    pub commission_rate: f64,
    pub min_commission: f64,
    pub stamp_tax_rate: f64,
    pub transfer_rate: f64,
}

impl StockFee {
    pub fn a_share() -> Self {
        Self {
            commission_rate: 0.00025,
            min_commission: 5.0,
            stamp_tax_rate: 0.0005,
            transfer_rate: 0.00001,
        }
    }
    pub fn hk() -> Self {
        Self {
            commission_rate: 0.0025,
            min_commission: 3.0,
            stamp_tax_rate: 0.001,
            transfer_rate: 0.0,
        }
    }
    pub fn us() -> Self {
        Self {
            commission_rate: 0.0,
            min_commission: 0.0,
            stamp_tax_rate: 0.0,
            transfer_rate: 0.0,
        }
    }
    pub fn for_market(market: u16) -> Self {
        match market {
            116 => Self::hk(),
            105..=107 => Self::us(),
            _ => Self::a_share(),
        }
    }
}

impl Fee for StockFee {
    fn buy_fee(&self, cash: f64) -> f64 {
        (cash * self.commission_rate).max(self.min_commission) + cash * self.transfer_rate
    }
    fn sell_fee(&self, shares: f64, price: f64, _holding_days: i64) -> f64 {
        let v = shares * price;
        (v * self.commission_rate).max(self.min_commission)
            + v * self.stamp_tax_rate
            + v * self.transfer_rate
    }

    /// 股票佣金按「每笔委托」计最低收费:多个 lot 合并为一单,只收一次最低佣金。
    fn sell_fee_order(&self, legs: &[(f64, i64)], price: f64) -> f64 {
        let shares: f64 = legs.iter().map(|(s, _)| s).sum();
        if shares <= 0.0 {
            return 0.0;
        }
        self.sell_fee(shares, price, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_buy_hits_min_commission() {
        // cash=1000: 佣金=0.25<5 → 取5; 过户=1000*0.00001=0.01 → 5.01
        let fee = StockFee::a_share().buy_fee(1000.0);
        assert!((fee - 5.01).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn a_share_buy_above_min() {
        // cash=100000: 佣金=25; 过户=1.0 → 26.0
        assert!((StockFee::a_share().buy_fee(100000.0) - 26.0).abs() < 1e-9);
    }

    #[test]
    fn a_share_sell_includes_stamp_tax() {
        // v=1000*100=100000: 佣金=25; 印花=50; 过户=1 → 76
        assert!((StockFee::a_share().sell_fee(1000.0, 100.0, 0) - 76.0).abs() < 1e-9);
    }

    #[test]
    fn us_is_commission_free() {
        assert!(StockFee::us().buy_fee(100000.0).abs() < 1e-9);
        assert!(StockFee::us().sell_fee(1000.0, 100.0, 0).abs() < 1e-9);
    }

    #[test]
    fn for_market_maps_each_market() {
        assert_eq!(StockFee::for_market(1), StockFee::a_share());
        assert_eq!(StockFee::for_market(0), StockFee::a_share());
        assert_eq!(StockFee::for_market(116), StockFee::hk());
        assert_eq!(StockFee::for_market(105), StockFee::us());
        assert_eq!(StockFee::for_market(106), StockFee::us());
        assert_eq!(StockFee::for_market(999), StockFee::a_share()); // 未知回退A股
    }

    #[test]
    fn sell_fee_order_charges_min_commission_once() {
        // 三个 lot 各 100 股 @10,合计 3000:佣金 0.75<5 取 5;印花 1.5;过户 0.03 → 6.53
        let legs = [(100.0, 10), (100.0, 5), (100.0, 1)];
        let fee = StockFee::a_share().sell_fee_order(&legs, 10.0);
        assert!((fee - 6.53).abs() < 1e-9, "实际 {fee}");
    }

    #[test]
    fn sell_fee_order_with_no_legs_is_zero() {
        assert!(StockFee::a_share().sell_fee_order(&[], 10.0).abs() < 1e-12);
    }
}

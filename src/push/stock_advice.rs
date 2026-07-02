//! 股票持仓建议：由 stock::diagnose 的技术信号 + 持仓，套用与基金一致的动作/金额启发式。
use serde::Serialize;

use crate::holdings::{Holding, round_yuan, size_pct};
use crate::stock::diagnose::StockDiagnosis;

#[derive(Debug, Clone, Serialize)]
pub struct StockAdvice {
    pub code: String,
    pub name: String,
    pub amount: f64,
    pub profit: f64,
    pub action: String,
    pub suggest_amount: f64,
    pub signal: String,
    pub trend: String,
    pub z: f64,
    pub price: f64,
    pub adj_price: f64,
    pub rsi: f64,
    pub rationale: String,
}

/// 动作规则同基金：下跌观望（深跌小额试探）/ 买入信号加仓 / 卖出信号止盈或减仓 / 其余持有。
fn decide(trend: &str, signal: &str, z: f64, amount: f64, profit: f64) -> (String, f64, String) {
    let amt = amount.max(0.0);
    if trend == "下跌" {
        if z <= -1.5 {
            let s = round_yuan(amt * size_pct(z.abs()) * 0.5);
            return ("加仓".into(), s, "下跌趋势但已深跌，仅小额试探".into());
        }
        return ("观望".into(), 0.0, "下跌趋势，暂观望不追高".into());
    }
    if signal.contains("买入") {
        let s = round_yuan(amt * size_pct(z.abs()));
        return ("加仓".into(), s, "技术面买入信号，逢低分批加仓".into());
    }
    if signal.contains("卖出") {
        let s = round_yuan(amt * size_pct(z.abs()));
        return if profit > 0.0 {
            ("止盈".into(), s, "技术面卖出信号且持有盈利，部分止盈".into())
        } else {
            ("减仓".into(), s, "技术面卖出信号，适度减仓控制风险".into())
        };
    }
    ("持有".into(), 0.0, "技术面中性，维持仓位".into())
}

pub fn advise(h: &Holding, diag: &StockDiagnosis) -> StockAdvice {
    let z = diag.boll_z;
    let (action, mut suggest, mut extra) = decide(&diag.trend, &diag.signal, z, h.amount, h.profit);
    if h.amount <= 0.0 {
        suggest = 0.0;
        extra = "未填持有金额，仅给方向".into();
    }
    StockAdvice {
        code: h.code.clone(),
        name: diag.name.clone(),
        amount: h.amount,
        profit: h.profit,
        action,
        suggest_amount: suggest,
        signal: diag.signal.clone(),
        trend: diag.trend.clone(),
        z,
        price: diag.price,
        adj_price: diag.adj_price,
        rsi: diag.rsi,
        rationale: format!("{extra}。{}", diag.rationale),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diag(trend: &str, signal: &str, z: f64) -> StockDiagnosis {
        StockDiagnosis {
            code: "600519".into(), name: "贵州茅台".into(),
            trend: trend.into(), signal: signal.into(), boll_z: z,
            rationale: "依据".into(),
            ..Default::default()
        }
    }
    fn hold(amount: f64, profit: f64) -> Holding {
        Holding { code: "600519".into(), amount, profit }
    }

    #[test]
    fn buy_signal_adds() {
        let a = advise(&hold(10000.0, 0.0), &diag("震荡", "买入(超卖)", -1.0));
        assert_eq!(a.action, "加仓");
        assert!(a.suggest_amount > 0.0);
    }

    #[test]
    fn sell_signal_takes_profit_or_trims() {
        let win = advise(&hold(10000.0, 500.0), &diag("震荡", "卖出(超买)", 1.0));
        assert_eq!(win.action, "止盈");
        let loss = advise(&hold(10000.0, -200.0), &diag("震荡", "卖出(超买)", 1.0));
        assert_eq!(loss.action, "减仓");
    }

    #[test]
    fn downtrend_watches_unless_deep() {
        let shallow = advise(&hold(10000.0, 0.0), &diag("下跌", "买入", -0.5));
        assert_eq!(shallow.action, "观望");
        assert_eq!(shallow.suggest_amount, 0.0);
        let deep = advise(&hold(10000.0, 0.0), &diag("下跌", "买入", -1.8));
        assert_eq!(deep.action, "加仓");
        assert!(deep.suggest_amount > 0.0);
    }

    #[test]
    fn neutral_holds() {
        let a = advise(&hold(10000.0, 0.0), &diag("震荡", "观望", 0.2));
        assert_eq!(a.action, "持有");
        assert_eq!(a.suggest_amount, 0.0);
    }

    #[test]
    fn zero_amount_only_direction() {
        let a = advise(&hold(0.0, 0.0), &diag("震荡", "买入", -1.0));
        assert_eq!(a.action, "加仓");
        assert_eq!(a.suggest_amount, 0.0);
        assert!(a.rationale.contains("仅给方向"));
    }
}

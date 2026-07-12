//! 股票持仓建议：由 stock::diagnose 的技术信号 + 持仓，套用启发式动作/金额。
//!
//! ⚠ 尚未做过前瞻检验。
//!
//! 基金侧同款的择时金额已被移除 —— 我们对基金的 ±σ 波动带做了事件研究，发现它在
//! 每一只测试基金上都跑不赢"随便哪天买"（超额 −0.7% ~ −2.1%），故不再据此给金额
//! （见 `holdings` 模块文档）。
//!
//! 股票侧这套（均线/MACD/RSI 技术信号）**是另一个信号，尚未被同样地检验过**，
//! 所以这里暂时保留。但它同样没有证据支持，`size_pct` 的 10%~30% 也同样是拍脑袋的
//! 魔数。在做完同样的前瞻检验之前，不应把这些金额当作有依据的建议。
use serde::Serialize;

use crate::holdings::{Holding, round_yuan};
use crate::stock::diagnose::StockDiagnosis;

/// 加仓/减仓比例：随信号强度 |z| 放大，clamp 到 [10%, 30%]。
///
/// 这几个数字（0.10 起、每 1σ 加 10%、上限 30%）是**经验取值，没有推导也没有回测支撑**。
/// 原本基金侧也用它，但基金的择时信号已被证伪并移除；此处保留仅因股票信号尚未检验。
pub fn size_pct(z_abs: f64) -> f64 {
    (0.10 + 0.10 * z_abs.min(2.0)).clamp(0.10, 0.30)
}

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

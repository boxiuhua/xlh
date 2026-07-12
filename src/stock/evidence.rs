//! 股票技术信号的前瞻检验。
//!
//! `stock::diagnose` 产出「强力买入 / 买入 / 观望 / 卖出 / 强力卖出」，
//! `push::stock_advice` 再据此给出「加仓 1200 元 / 止盈 800 元」这类具体金额。
//! 这套信号（布林 z + RSI + MACD 柱三者打分相加）**从未被检验过**，
//! `size_pct` 的 10%~30% 也同样是拍脑袋的魔数。
//!
//! 基金侧的 ±σ 波动带做完同样的检验后被证伪（每一只测试基金上都跑不赢「随便哪天买」），
//! 择时金额已因此移除（见 `crate::holdings` 模块文档）。股票侧是**另一套信号**，
//! 结论不能照搬 —— 必须自己测。这个模块就是来测它的。
//!
//! ## 方法（与 `analyze::evaluate_triggers` 同构）
//!
//! 在每个时点 t，**只用 `bars[..=t]` 重新跑一遍 diagnose**（不含任何未来数据），
//! 拿到当时会给出的信号；然后测「T+1 收盘买入、持有 horizon 个交易日」的收益 ——
//! 用 T+1 而非 T，是因为信号本身用到了 T 日收盘价，当日收盘价成交是理想化的
//! （同 `engine` 里已修掉的那个一日未来函数）。
//!
//! 同时统计无条件基准：每一个可检验的 t 都算一次同样的持有收益。
//! **信号跑不赢基准，它就没有价值。**

use serde::Serialize;

use super::data::StockBar;
use super::diagnose::{diagnose, DiagnoseParams};

/// 前瞻检验窗口：20 个交易日 ≈ 1 个月。与基金侧一致，便于横向比较。
pub const HORIZON: usize = 20;
/// 信号数低于此值时，任何结论都是噪声。
const MIN_SIGNALS: usize = 10;

#[derive(Debug, Clone, Serialize)]
pub struct SignalEvidence {
    pub horizon_days: usize,
    /// 可检验的时点数（= 无条件基准的样本量）
    pub sample_days: usize,

    /// 「买入 / 强力买入」的次数
    pub buy_signals: usize,
    pub buy_win_rate: Option<f64>,
    pub buy_mean_forward: Option<f64>,

    /// 「卖出 / 强力卖出」的次数
    pub sell_signals: usize,
    /// 卖出信号后**下跌**的比例（跌了才算躲对）
    pub sell_win_rate: Option<f64>,
    pub sell_mean_forward: Option<f64>,

    /// 无条件基准：任意一天买入、持有 horizon 的平均收益 %
    pub baseline_mean_forward: Option<f64>,
    pub baseline_win_rate: Option<f64>,

    /// 买入信号相对基准的超额 %（None = 样本不足）
    pub buy_edge: Option<f64>,
    pub verdict: String,
}

/// 对技术信号做无未来函数的事件研究。
///
/// `bars` 需按日期升序。返回 `None` 表示数据不足以检验。
pub fn evaluate_signals(bars: &[StockBar], p: &DiagnoseParams, horizon: usize) -> Option<SignalEvidence> {
    // diagnose 自身需要的最小窗口
    let need = p.ma_long.max(p.boll_window).max(p.rsi_period + 1).max(p.macd_slow).max(p.trend_window);
    // 还要留出 T+1 入场 + horizon 持有
    if bars.len() < need + horizon + 2 { return None; }

    let adj: Vec<f64> = bars.iter().map(|b| b.adj_close).collect();

    // 信号在 T 日收盘后产生 → T+1 收盘入场 → 持有 horizon 个交易日
    let fwd = |t: usize| -> Option<f64> {
        let entry = *adj.get(t + 1)?;
        let exit = *adj.get(t + 1 + horizon)?;
        if entry > 0.0 { Some((exit / entry - 1.0) * 100.0) } else { None }
    };

    let (mut buy_f, mut sell_f, mut base_f) = (Vec::new(), Vec::new(), Vec::new());

    let last_t = bars.len().saturating_sub(horizon + 2);
    for t in (need - 1)..=last_t {
        // 只喂 t 及之前的 bar —— diagnose 拿不到任何未来数据
        let Ok(d) = diagnose(String::new(), String::new(), &bars[..=t], p) else { continue };
        let Some(r) = fwd(t) else { continue };
        base_f.push(r);

        if d.signal.contains("买入") { buy_f.push(r); }
        if d.signal.contains("卖出") { sell_f.push(r); }
    }

    if base_f.is_empty() { return None; }

    let mean = |v: &[f64]| if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) };
    let up = |v: &[f64]| if v.is_empty() { None } else {
        Some(v.iter().filter(|x| **x > 0.0).count() as f64 / v.len() as f64 * 100.0)
    };
    let down = |v: &[f64]| if v.is_empty() { None } else {
        Some(v.iter().filter(|x| **x < 0.0).count() as f64 / v.len() as f64 * 100.0)
    };

    let buy_mean = mean(&buy_f);
    let base_mean = mean(&base_f);
    let buy_edge = match (buy_mean, base_mean) {
        (Some(b), Some(base)) if buy_f.len() >= MIN_SIGNALS => Some(b - base),
        _ => None,
    };
    let verdict = build_verdict(buy_f.len(), buy_mean, sell_f.len(), mean(&sell_f), base_mean, horizon);

    Some(SignalEvidence {
        horizon_days: horizon,
        sample_days: base_f.len(),
        buy_signals: buy_f.len(),
        buy_win_rate: up(&buy_f),
        buy_mean_forward: buy_mean,
        sell_signals: sell_f.len(),
        sell_win_rate: down(&sell_f),
        sell_mean_forward: mean(&sell_f),
        baseline_mean_forward: base_mean,
        baseline_win_rate: up(&base_f),
        buy_edge,
        verdict,
    })
}

fn build_verdict(
    n_buy: usize, buy_mean: Option<f64>,
    n_sell: usize, sell_mean: Option<f64>,
    base_mean: Option<f64>, horizon: usize,
) -> String {
    let Some(base) = base_mean else { return "样本不足，无法检验该信号是否有效。".into() };

    let mut parts = vec![format!(
        "基准：任意一天买入、持有 {horizon} 个交易日，平均收益 {base:+.2}%。")];

    match (buy_mean, n_buy >= MIN_SIGNALS) {
        (Some(b), true) => {
            let edge = b - base;
            parts.push(format!(
                "买入信号触发 {n_buy} 次，其后 {horizon} 日平均 {b:+.2}%，超额 {edge:+.2}%。"));
            if edge <= 0.0 {
                parts.push("该买入信号没有跑赢「随便哪天买」—— 按现有历史，它不提供择时价值。".into());
            } else if edge < 0.5 {
                parts.push("超额很小，与噪声难以区分；不宜据此加大仓位。".into());
            } else {
                parts.push("历史上有正超额，但这是单只股票的样本内统计、未经样本外检验，不保证延续。".into());
            }
        }
        (Some(b), false) => parts.push(format!(
            "买入信号仅触发 {n_buy} 次（其后平均 {b:+.2}%），样本量不足 {MIN_SIGNALS} 次，无法判断。")),
        _ => parts.push("买入信号在历史上从未触发，无从检验。".into()),
    }

    if let (Some(s), true) = (sell_mean, n_sell >= MIN_SIGNALS) {
        parts.push(format!(
            "卖出信号触发 {n_sell} 次，其后 {horizon} 日平均 {s:+.2}%（为负才说明躲对了下跌）。"));
        if s > 0.0 {
            parts.push("卖出信号之后平均还在涨 —— 照它减仓会错过后续上涨。".into());
        }
    }

    parts.push("以上为该股票自身历史的统计，非预测；信号阈值（布林±1σ/±2σ、RSI 30/70、MACD 柱符号）\
                均为经验取值，未经寻优验证。".into());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn bars(closes: &[f64]) -> Vec<StockBar> {
        closes.iter().enumerate().map(|(i, c)| StockBar {
            date: NaiveDate::from_ymd_opt(2022, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            open: *c, high: *c, low: *c, close: *c, volume: 1.0, adj_close: *c,
        }).collect()
    }

    /// 确定性伪随机游走 —— 不该出现稳定超额，但必须**给出基准并算出超额**。
    #[test]
    fn always_compares_against_an_unconditional_baseline() {
        let mut x = 987654321u64;
        let mut px = 100.0;
        let closes: Vec<f64> = (0..500).map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((x >> 33) as f64 / (1u64 << 31) as f64) - 1.0;
            px *= 1.0 + u * 0.015;
            px
        }).collect();

        let e = evaluate_signals(&bars(&closes), &DiagnoseParams::default(), HORIZON)
            .expect("样本足够");
        assert!(e.sample_days > 300);
        assert!(e.baseline_mean_forward.is_some(), "必须给出无条件基准");
        assert!(e.verdict.contains("基准"));
        // 有基准可比才是关键：没有它，"买入信号后平均涨 1%" 是句废话
        if e.buy_signals >= MIN_SIGNALS {
            assert!(e.buy_edge.is_some(), "样本够就必须算出超额");
            assert!(e.verdict.contains("超额"));
        }
    }

    /// 绝不能有未来函数：t 时刻的信号只能用 bars[..=t]。
    ///
    /// 构造一段"平稳后突然暴涨"的序列。若实现偷看了未来，暴涨前的时点就会被打上买入标签、
    /// 从而拿到虚高的前瞻收益。这里断言前瞻收益的量级不会离谱到只有偷看才能达到。
    #[test]
    fn signal_at_t_cannot_see_beyond_t() {
        let mut closes: Vec<f64> = (0..200).map(|i| 100.0 + ((i % 7) as f64 - 3.0) * 0.3).collect();
        closes.extend((0..200).map(|i| 100.0 + i as f64 * 2.0));   // 后段单边暴涨

        let e = evaluate_signals(&bars(&closes), &DiagnoseParams::default(), HORIZON).unwrap();
        let base = e.baseline_mean_forward.unwrap();
        if let Some(b) = e.buy_mean_forward {
            // 买入信号的前瞻收益不该系统性地把整段暴涨全吃到（那只有偷看未来才做得到）
            assert!(b < base * 5.0 + 50.0, "买入信号前瞻收益异常高({b:.1}% vs 基准 {base:.1}%)，疑似未来函数");
        }
    }

    #[test]
    fn none_when_history_too_short() {
        let closes: Vec<f64> = (0..50).map(|i| 100.0 + i as f64).collect();
        assert!(evaluate_signals(&bars(&closes), &DiagnoseParams::default(), HORIZON).is_none());
    }

    /// 实网：拿真实股票测这套技术信号到底有没有用。
    /// `cargo test --lib stock::evidence -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_evaluate_real_stocks() {
        use crate::stock::data::{cache, kline, secid::Secid};

        // 白马 / 成长 / 银行 / 周期 / 港股 —— 覆盖不同风格
        let pool = [
            ("600519", "贵州茅台", 1u16), ("300750", "宁德时代", 0),
            ("600036", "招商银行", 1),   ("000858", "五粮液", 0),
            ("601318", "中国平安", 1),
        ];
        let p = DiagnoseParams::default();
        let mut rows = Vec::new();

        for (code, name, market) in pool {
            let secid = Secid { market, code: code.into() };
            let Ok(bars) = kline::fetch(&secid) else { println!("{name} 抓取失败"); continue };
            let Some(e) = evaluate_signals(&bars, &p, HORIZON) else {
                println!("{name} 数据不足（{} 根）", bars.len()); continue;
            };
            rows.push((name, bars.len(), e));
        }
        let _ = cache::covers(&[], chrono::NaiveDate::MIN, chrono::NaiveDate::MIN); // 保持 import

        println!("\n{:<10} {:>5} {:>7} {:>10} {:>10} {:>10}",
                 "股票", "K线", "买入次", "买入后20日", "基准", "超额");
        println!("{}", "-".repeat(58));
        for (name, n, e) in &rows {
            let (b, base) = (e.buy_mean_forward, e.baseline_mean_forward);
            match (b, base) {
                (Some(b), Some(base)) => println!(
                    "{:<10} {:>5} {:>7} {:>9.2}% {:>9.2}% {:>9.2}%",
                    name, n, e.buy_signals, b, base, b - base),
                _ => println!("{:<10} {:>5} 无有效信号", name, n),
            }
        }
        assert!(!rows.is_empty(), "至少要测出一只");

        // 每只都必须给出基准 —— 没有基准的"平均收益"是句废话
        for (_, _, e) in &rows {
            assert!(e.baseline_mean_forward.is_some());
            assert!(e.verdict.contains("基准"));
        }
    }

    /// 结论必须承认阈值是拍脑袋的。
    #[test]
    fn verdict_admits_thresholds_are_arbitrary() {
        let closes: Vec<f64> = (0..500).map(|i| 100.0 + ((i % 40) as f64 - 20.0) * 0.8).collect();
        let e = evaluate_signals(&bars(&closes), &DiagnoseParams::default(), HORIZON).unwrap();
        assert!(e.verdict.contains("经验取值") && e.verdict.contains("未经寻优验证"));
    }
}

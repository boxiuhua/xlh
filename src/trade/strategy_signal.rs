//! 日线策略的次日决策:实盘参数 + 已收盘 K 线 → 明天开盘做什么。纯函数,无 IO。
//!
//! 与前推回测同一引擎、同一成交口径:回放最近 `train_days` 天建立策略状态与模拟持仓,
//! 再以下一个交易日为决策日追问一次(计划 3e 设计裁决 2)。

use crate::config::build_strategy_from;
use crate::event::{Direction, SignalAmount};
use crate::stock::ashare::AShareExecution;
use crate::stock::backtest;
use crate::stock::data::{StockBar, StockData};
use crate::stock::fee::StockFee;
use crate::trade::admission::walk_forward::WalkForwardCfg;
use anyhow::{anyhow, Result};
use chrono::{Duration, NaiveDate};

#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub side: Direction,
    /// 买入金额(策略参数里的每笔金额);卖出为 None(全部可卖)
    pub cash: Option<f64>,
    pub reason: String,
}

pub fn decide(
    kind: &str,
    code: &str,
    params: &toml::Value,
    bars: &[StockBar],
    next: NaiveDate,
    cfg: &WalkForwardCfg,
) -> Result<Option<Decision>> {
    let Some(last) = bars.last() else {
        return Err(anyhow!("{code} 无 K 线,无法决策"));
    };
    let from = last.date - Duration::days(cfg.train_days);
    let replay: Vec<StockBar> = bars.iter().copied().filter(|b| b.date > from).collect();
    let prev = bars.iter().copied().rfind(|b| b.date <= from);
    let strategy = build_strategy_from(kind, &Some(params.clone()), &[])?;
    let signals = backtest::replay_and_decide(
        StockData::with_prev_bar(replay, prev),
        strategy,
        StockFee::a_share(),
        cfg.initial_cash,
        Box::new(AShareExecution::new(code, None, cfg.slippage)),
        next,
    );
    let Some(s) = signals.into_iter().next() else {
        return Ok(None);
    };
    let (cash, action) = match (s.direction, s.amount) {
        (Direction::Buy, SignalAmount::Cash(c)) => (Some(c), "买入"),
        (Direction::Sell, SignalAmount::AllOut) => (None, "清仓卖出"),
        (dir, amount) => {
            return Err(anyhow!("{code} 暂不支持的信号数量口径 {dir:?} {amount:?}"));
        }
    };
    Ok(Some(Decision {
        side: s.direction,
        cash,
        reason: format!(
            "日线策略 {kind}:基于 {} 收盘数据,{next} 开盘{action}(参数 {params})",
            last.date
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// 连续交易日(跳过周末)。
    fn bars(start: NaiveDate, prices: &[f64]) -> Vec<StockBar> {
        let mut out = Vec::new();
        let mut date = start;
        for p in prices {
            while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                date += chrono::Duration::days(1);
            }
            out.push(StockBar {
                date,
                open: *p,
                high: *p,
                low: *p,
                close: *p,
                volume: 1.0,
                adj_close: *p,
            });
            date += chrono::Duration::days(1);
        }
        out
    }

    fn trend_params() -> toml::Value {
        toml::Value::Table(
            "short_window = 5\nlong_window = 20\namount = 100000.0"
                .parse()
                .unwrap(),
        )
    }

    #[test]
    fn golden_cross_on_the_last_bar_means_buy_next_day() {
        // 长期下跌后最后几天急拉:短均线在最后一根 K 线上穿长均线
        let mut p: Vec<f64> = (0..80).map(|i| 20.0 - i as f64 * 0.1).collect();
        p.extend([12.2, 12.3, 12.4, 12.5, 25.0]);
        let b = bars(d(2026, 5, 4), &p);
        let next = b.last().unwrap().date + chrono::Duration::days(1);
        let dec = decide(
            "trend",
            "600000",
            &trend_params(),
            &b,
            next,
            &WalkForwardCfg::default(),
        )
        .unwrap()
        .expect("应给出买入");
        assert_eq!(dec.side, Direction::Buy);
        assert_eq!(dec.cash, Some(100000.0));
        assert!(dec.reason.contains(&b.last().unwrap().date.to_string()));
    }

    #[test]
    fn flat_market_means_no_action() {
        let b = bars(d(2026, 5, 4), &[10.0; 80]);
        let next = b.last().unwrap().date + chrono::Duration::days(1);
        assert_eq!(
            decide(
                "trend",
                "600000",
                &trend_params(),
                &b,
                next,
                &WalkForwardCfg::default()
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn replay_only_uses_the_trailing_train_window() {
        // 很久以前的一段行情若被回放,会让策略在窗口起点就持仓;只回放最近 train_days 天
        // 时结论必须与「只给最近这段数据」完全一致。
        let mut p: Vec<f64> = (0..600).map(|i| 10.0 + (i % 7) as f64).collect();
        p.extend((0..80).map(|i| 20.0 - i as f64 * 0.1));
        p.extend([13.0, 15.0, 17.0, 19.0, 21.0]);
        let all = bars(d(2023, 1, 2), &p);
        let cfg = WalkForwardCfg {
            train_days: 120,
            ..WalkForwardCfg::default()
        };
        let next = all.last().unwrap().date + chrono::Duration::days(1);
        let cut = all.last().unwrap().date - chrono::Duration::days(cfg.train_days);
        let recent: Vec<StockBar> = all.iter().copied().filter(|b| b.date > cut).collect();
        assert_eq!(
            decide("trend", "600000", &trend_params(), &all, next, &cfg).unwrap(),
            decide("trend", "600000", &trend_params(), &recent, next, &cfg).unwrap()
        );
    }

    #[test]
    fn empty_bars_or_bad_params_are_errors() {
        let next = d(2026, 9, 21);
        assert!(decide(
            "trend",
            "600000",
            &trend_params(),
            &[],
            next,
            &WalkForwardCfg::default()
        )
        .is_err());
        let bad = toml::Value::Table("short_window = \"x\"".parse().unwrap());
        let b = bars(d(2026, 5, 4), &[10.0; 30]);
        assert!(decide(
            "trend",
            "600000",
            &bad,
            &b,
            next,
            &WalkForwardCfg::default()
        )
        .is_err());
    }
}

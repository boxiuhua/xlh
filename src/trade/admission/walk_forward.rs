//! 滚动前推回测:训练窗选参数、紧接的检验窗只运行,拼接所有检验窗得样本外表现。
//!
//! 与 `optimize.rs` 的单次 70/30 切分不同:每段检验数据都没参与过任何挑选,
//! 因此样本外指标不含 winner's curse。

use crate::config::build_strategy_from;
use crate::metrics::Summary;
use crate::optimize::expand_grid;
use crate::stock::ashare::AShareExecution;
use crate::stock::backtest;
use crate::stock::data::{StockBar, StockData};
use crate::stock::fee::StockFee;
use anyhow::{anyhow, Result};
use chrono::{Duration, NaiveDate};
use serde::Serialize;

/// 一个窗口内至少要有多少根 K 线才算数(约 2 个月 / 1 个月)。
const MIN_TRAIN_BARS: usize = 40;
const MIN_TEST_BARS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkForwardCfg {
    pub train_days: i64,
    pub test_days: i64,
    pub step_days: i64,
    /// 训练窗选参依据:sharpe | total_return | annualized | max_drawdown
    pub metric: String,
    pub initial_cash: f64,
    pub slippage: f64,
}

impl Default for WalkForwardCfg {
    fn default() -> Self {
        Self {
            train_days: 730,
            test_days: 182,
            step_days: 182,
            metric: "sharpe".into(),
            initial_cash: 100_000.0,
            slippage: AShareExecution::DEFAULT_SLIPPAGE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Window {
    pub train_from: NaiveDate,
    /// 训练段结束(不含),同时是检验段起点(含)
    pub train_to: NaiveDate,
    pub test_to: NaiveDate,
}

/// 滚动切分训练/检验窗口。检验窗口不允许重叠:`step_days` 必须 >= `test_days`,
/// 否则返回空——重叠会让相邻窗口的复利收益和年化天数被重复计算。
pub fn windows(first: NaiveDate, last: NaiveDate, cfg: &WalkForwardCfg) -> Vec<Window> {
    let mut out = Vec::new();
    if cfg.train_days <= 0
        || cfg.test_days <= 0
        || cfg.step_days <= 0
        || cfg.step_days < cfg.test_days
    {
        return out;
    }
    let mut train_from = first;
    loop {
        let train_to = train_from + Duration::days(cfg.train_days);
        let test_to = train_to + Duration::days(cfg.test_days);
        if test_to > last {
            break;
        }
        out.push(Window {
            train_from,
            train_to,
            test_to,
        });
        train_from += Duration::days(cfg.step_days);
    }
    out
}

/// 区间内买入持有收益(复权)。区间无数据返回 0。
pub fn buy_and_hold_return(bars: &[StockBar], from: NaiveDate, to: NaiveDate) -> f64 {
    let span: Vec<&StockBar> = bars
        .iter()
        .filter(|b| b.date >= from && b.date <= to)
        .collect();
    match (span.first(), span.last()) {
        (Some(a), Some(b)) if a.adj_close > 0.0 => b.adj_close / a.adj_close - 1.0,
        _ => 0.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowResult {
    pub window: Window,
    pub params: toml::Value,
    pub is_sharpe: f64,
    pub oos: Summary,
    pub oos_days: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeResult {
    pub code: String,
    pub windows: Vec<WindowResult>,
    pub buy_hold_return: f64,
}

/// 单只股票的样本外汇总。
///
/// 聚合口径(见计划「相对 spec 的实现细化」1):收益连乘、年化按总检验天数折算、
/// 夏普按检验天数加权平均、最大回撤取各窗最大、交易笔数求和。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeMetrics {
    pub code: String,
    pub windows: usize,
    pub oos_return: f64,
    pub oos_annualized: f64,
    pub oos_sharpe: f64,
    pub oos_max_drawdown: f64,
    pub oos_trades: usize,
    pub is_sharpe: f64,
    pub years: f64,
    pub buy_hold_return: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolMetrics {
    pub codes: Vec<CodeMetrics>,
    /// 池内中位数
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_max_drawdown: f64,
    /// 池内求和
    pub oos_trades: usize,
    pub is_sharpe: f64,
    /// 样本外收益为正的股票占比
    pub positive_ratio: f64,
    pub years: f64,
    pub buy_hold_return: f64,
}

fn metric_of(s: &Summary, metric: &str) -> f64 {
    match metric {
        "total_return" => s.total_return,
        "annualized" => s.annualized,
        // 回撤越小越好
        "max_drawdown" => -s.max_drawdown,
        _ => s.sharpe,
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn run_once(
    kind: &str,
    code: &str,
    data: StockData,
    params: &toml::Value,
    cfg: &WalkForwardCfg,
) -> Result<backtest::StockRunOutcome> {
    let strategy = build_strategy_from(kind, &Some(params.clone()), &[])?;
    Ok(backtest::run_one(
        kind.to_string(),
        code.to_string(),
        data,
        strategy,
        StockFee::a_share(),
        cfg.initial_cash,
        Box::new(AShareExecution::new(code, None, cfg.slippage)),
    ))
}

/// 对一只股票做完整前推回测:每个训练窗按 `cfg.metric` 选参,紧接的检验窗只运行。
pub fn run_code(
    kind: &str,
    code: &str,
    bars: &[StockBar],
    grid: &toml::Table,
    cfg: &WalkForwardCfg,
) -> Result<CodeResult> {
    let combos = expand_grid(grid)?;
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Err(anyhow!("{code} 无 K 线数据"));
    };
    let mut out = Vec::new();
    for w in windows(first.date, last.date, cfg) {
        let train: Vec<StockBar> = bars
            .iter()
            .copied()
            .filter(|b| b.date >= w.train_from && b.date < w.train_to)
            .collect();
        let test: Vec<StockBar> = bars
            .iter()
            .copied()
            .filter(|b| b.date >= w.train_to && b.date <= w.test_to)
            .collect();
        if train.len() < MIN_TRAIN_BARS || test.len() < MIN_TEST_BARS {
            continue;
        }
        let prev = bars.iter().copied().rfind(|b| b.date < w.train_to);

        let mut best: Option<(toml::Value, f64, f64)> = None;
        for params in &combos {
            let run = run_once(kind, code, StockData::new(train.clone()), params, cfg)?;
            let score = metric_of(&run.summary, &cfg.metric);
            if best.as_ref().is_none_or(|(_, s, _)| score > *s) {
                best = Some((params.clone(), score, run.summary.sharpe));
            }
        }
        let Some((params, _, is_sharpe)) = best else {
            continue;
        };
        let oos = run_once(
            kind,
            code,
            StockData::with_prev_bar(test.clone(), prev),
            &params,
            cfg,
        )?;
        out.push(WindowResult {
            window: w,
            params,
            is_sharpe,
            oos: oos.summary,
            oos_days: (w.test_to - w.train_to).num_days().max(1),
        });
    }
    let (from, to) = match (out.first(), out.last()) {
        (Some(a), Some(b)) => (a.window.train_to, b.window.test_to),
        _ => (first.date, last.date),
    };
    Ok(CodeResult {
        code: code.to_string(),
        windows: out,
        buy_hold_return: buy_and_hold_return(bars, from, to),
    })
}

impl CodeResult {
    pub fn metrics(&self) -> CodeMetrics {
        let days: i64 = self.windows.iter().map(|w| w.oos_days).sum();
        let years = (days as f64 / 365.0).max(1e-9);
        let oos_return = self
            .windows
            .iter()
            .fold(1.0, |acc, w| acc * (1.0 + w.oos.total_return))
            - 1.0;
        let weight = |f: fn(&WindowResult) -> f64| -> f64 {
            if days == 0 {
                return 0.0;
            }
            self.windows
                .iter()
                .map(|w| f(w) * w.oos_days as f64)
                .sum::<f64>()
                / days as f64
        };
        CodeMetrics {
            code: self.code.clone(),
            windows: self.windows.len(),
            oos_return,
            oos_annualized: if self.windows.is_empty() {
                0.0
            } else {
                (1.0 + oos_return).powf(1.0 / years) - 1.0
            },
            oos_sharpe: weight(|w| w.oos.sharpe),
            oos_max_drawdown: self
                .windows
                .iter()
                .map(|w| w.oos.max_drawdown)
                .fold(0.0, f64::max),
            oos_trades: self.windows.iter().map(|w| w.oos.trade_count).sum(),
            is_sharpe: weight(|w| w.is_sharpe),
            years: days as f64 / 365.0,
            buy_hold_return: self.buy_hold_return,
        }
    }
}

pub fn aggregate(codes: Vec<CodeMetrics>) -> PoolMetrics {
    let positive = codes.iter().filter(|c| c.oos_return > 0.0).count();
    let ratio = if codes.is_empty() {
        0.0
    } else {
        positive as f64 / codes.len() as f64
    };
    PoolMetrics {
        oos_return: median(codes.iter().map(|c| c.oos_return).collect()),
        oos_sharpe: median(codes.iter().map(|c| c.oos_sharpe).collect()),
        oos_max_drawdown: median(codes.iter().map(|c| c.oos_max_drawdown).collect()),
        oos_trades: codes.iter().map(|c| c.oos_trades).sum(),
        is_sharpe: median(codes.iter().map(|c| c.is_sharpe).collect()),
        positive_ratio: ratio,
        years: median(codes.iter().map(|c| c.years).collect()),
        buy_hold_return: median(codes.iter().map(|c| c.buy_hold_return).collect()),
        codes,
    }
}

/// 对整个股票池跑前推回测。K 线由调用方注入(测试用切片,生产用缓存加载)。
///
/// 先校验网格与策略参数是否合法(配置错误直接 `Err`,不会被误判成某只股票的问题);
/// 校验通过后逐只股票跑,单只股票的问题彼此隔离,记录到返回的跳过列表(代码、原因):
/// - K 线加载失败:"加载失败: {原因}"
/// - `run_code` 运行期出错:"回测失败: {原因}"
/// - 未产出任何有效窗口(训练/检验数据不足):"数据不足,无有效窗口"
pub fn run_pool<F>(
    kind: &str,
    pool: &[String],
    grid: &toml::Table,
    cfg: &WalkForwardCfg,
    mut load: F,
) -> Result<(PoolMetrics, Vec<(String, String)>)>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let combos = expand_grid(grid)?;
    build_strategy_from(kind, &Some(combos[0].clone()), &[])?;

    let mut metrics = Vec::new();
    let mut skipped = Vec::new();
    for code in pool {
        match load(code) {
            Ok(bars) => match run_code(kind, code, &bars, grid, cfg) {
                Ok(r) if !r.windows.is_empty() => metrics.push(r.metrics()),
                Ok(_) => skipped.push((code.clone(), "数据不足,无有效窗口".to_string())),
                Err(e) => skipped.push((code.clone(), format!("回测失败: {e:#}"))),
            },
            Err(e) => skipped.push((code.clone(), format!("加载失败: {e:#}"))),
        }
    }
    Ok((aggregate(metrics), skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// 连续交易日(跳过周末),价格按给定序列。
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

    fn cfg() -> WalkForwardCfg {
        WalkForwardCfg {
            train_days: 60,
            test_days: 30,
            step_days: 30,
            ..WalkForwardCfg::default()
        }
    }

    fn grid() -> toml::Table {
        "short_window = [3, 5]\nlong_window = [10]\namount = [20000.0]"
            .parse::<toml::Table>()
            .unwrap()
    }

    #[test]
    fn windows_roll_forward_and_stop_at_data_end() {
        // 训练 60 天 + 检验 30 天,步长 30 天:起点 1/1、1/31、3/1、3/31 共 4 窗
        let ws = windows(d(2024, 1, 1), d(2024, 6, 30), &cfg());
        assert_eq!(ws.len(), 4, "{ws:?}");
        assert_eq!(ws[0].train_from, d(2024, 1, 1));
        assert_eq!(ws[0].train_to, d(2024, 3, 1));
        assert_eq!(ws[0].test_to, d(2024, 3, 31));
        assert_eq!(ws[1].train_from, d(2024, 1, 31), "步长 30 天");
        assert!(ws.iter().all(|w| w.test_to <= d(2024, 6, 30)));
        assert!(
            windows(d(2024, 1, 1), d(2024, 2, 1), &cfg()).is_empty(),
            "数据不足一窗"
        );
        assert!(
            windows(
                d(2024, 1, 1),
                d(2024, 12, 31),
                &WalkForwardCfg {
                    train_days: 60,
                    test_days: 30,
                    step_days: 20,
                    ..WalkForwardCfg::default()
                }
            )
            .is_empty(),
            "step_days < test_days 会导致检验窗重叠,必须拒绝"
        );
    }

    #[test]
    fn buy_and_hold_uses_adjusted_span() {
        let b = bars(d(2024, 1, 1), &[10.0, 11.0, 12.0, 13.0]);
        let r = buy_and_hold_return(&b, b[1].date, b[3].date);
        assert!((r - (13.0 / 11.0 - 1.0)).abs() < 1e-9, "{r}");
        assert_eq!(
            buy_and_hold_return(&b, d(2030, 1, 1), d(2030, 2, 1)),
            0.0,
            "区间无数据"
        );
    }

    /// 构造一个内部字段自洽的 `Summary`(仅测试用)。
    fn summary(total_return: f64, sharpe: f64, max_drawdown: f64, trade_count: usize) -> Summary {
        let total_contributed = 100_000.0;
        Summary {
            total_contributed,
            final_equity: total_contributed * (1.0 + total_return),
            total_return,
            annualized: total_return,
            max_drawdown,
            sharpe,
            trade_count,
        }
    }

    #[test]
    fn metrics_compound_returns_and_weight_by_days() {
        let w = Window {
            train_from: d(2024, 1, 1),
            train_to: d(2024, 3, 1),
            test_to: d(2024, 4, 1),
        };
        let out = CodeResult {
            code: "600000".to_string(),
            windows: vec![
                WindowResult {
                    window: w,
                    params: toml::Value::Table(Default::default()),
                    is_sharpe: 2.0,
                    oos: summary(0.10, 1.0, 0.10, 5),
                    oos_days: 100,
                },
                WindowResult {
                    window: w,
                    params: toml::Value::Table(Default::default()),
                    is_sharpe: 3.0,
                    oos: summary(0.20, 2.0, 0.30, 7),
                    oos_days: 300,
                },
            ],
            buy_hold_return: 0.05,
        };
        let m = out.metrics();
        assert!((m.oos_return - 0.32).abs() < 1e-9, "复利: {}", m.oos_return);
        assert!(
            (m.oos_sharpe - 1.75).abs() < 1e-9,
            "按天数加权: {}",
            m.oos_sharpe
        );
        assert!(
            (m.is_sharpe - 2.75).abs() < 1e-9,
            "按天数加权: {}",
            m.is_sharpe
        );
        assert!((m.oos_max_drawdown - 0.30).abs() < 1e-9, "取最大");
        assert_eq!(m.oos_trades, 12, "求和");
        assert_eq!(m.windows, 2);
        assert!((m.years - 400.0 / 365.0).abs() < 1e-9);
        let expected_annualized = 1.32_f64.powf(365.0 / 400.0) - 1.0;
        assert!(
            (m.oos_annualized - expected_annualized).abs() < 1e-9,
            "{} vs {}",
            m.oos_annualized,
            expected_annualized
        );
    }

    #[test]
    fn training_window_never_sees_test_bars() {
        let base: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let bars_a = bars(d(2024, 1, 1), &base);
        let first = bars_a.first().unwrap().date;
        let last = bars_a.last().unwrap().date;
        let train_to = windows(first, last, &cfg())[0].train_to;
        // 与 bars_a 在 train_to 之前完全相同,之后价格被放大 3 倍。
        let diverged: Vec<f64> = base
            .iter()
            .zip(bars_a.iter())
            .map(|(p, b)| if b.date >= train_to { p * 3.0 } else { *p })
            .collect();
        let bars_b = bars(d(2024, 1, 1), &diverged);

        let out_a = run_code("trend", "600000", &bars_a, &grid(), &cfg()).unwrap();
        let out_b = run_code("trend", "600000", &bars_b, &grid(), &cfg()).unwrap();
        assert!(!out_a.windows.is_empty());
        assert!(!out_b.windows.is_empty());
        assert_eq!(
            out_a.windows[0].params, out_b.windows[0].params,
            "训练窗从未见过检验段数据,第一窗选参不应受后续分歧影响"
        );
        assert!(
            (out_a.windows[0].is_sharpe - out_b.windows[0].is_sharpe).abs() < 1e-9,
            "第一窗训练打分不应受检验段分歧影响"
        );
    }

    #[test]
    fn run_code_picks_params_in_train_and_measures_in_test() {
        // 190 个交易日的上涨序列,3 个窗口
        let prices: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let b = bars(d(2024, 1, 1), &prices);
        let out = run_code("trend", "600000", &b, &grid(), &cfg()).unwrap();
        assert!(!out.windows.is_empty(), "应至少产出一个窗口");
        for w in &out.windows {
            assert!(w.params.get("short_window").is_some(), "参数来自网格");
            assert!(w.oos_days > 0);
        }
        let m = out.metrics();
        assert_eq!(m.code, "600000");
        assert_eq!(m.windows, out.windows.len());
        assert!(m.years > 0.0);
        assert!(m.buy_hold_return > 0.0, "上涨行情买入持有为正");
    }

    #[test]
    fn aggregate_takes_medians_sums_and_positive_ratio() {
        let mk = |code: &str, ret: f64, sharpe: f64, mdd: f64, trades: usize| CodeMetrics {
            code: code.into(),
            windows: 2,
            oos_return: ret,
            oos_annualized: ret,
            oos_sharpe: sharpe,
            oos_max_drawdown: mdd,
            oos_trades: trades,
            is_sharpe: sharpe * 2.0,
            years: 1.0,
            buy_hold_return: 0.05,
        };
        let p = aggregate(vec![
            mk("a", 0.10, 1.0, 0.10, 20),
            mk("b", -0.05, 0.4, 0.30, 10),
            mk("c", 0.20, 1.6, 0.20, 12),
        ]);
        assert!((p.oos_return - 0.10).abs() < 1e-9, "中位数");
        assert!((p.oos_sharpe - 1.0).abs() < 1e-9);
        assert!((p.oos_max_drawdown - 0.20).abs() < 1e-9);
        assert_eq!(p.oos_trades, 42, "求和");
        assert!((p.positive_ratio - 2.0 / 3.0).abs() < 1e-9);
        assert!((p.buy_hold_return - 0.05).abs() < 1e-9);
        assert!((p.is_sharpe - 2.0).abs() < 1e-9);
        assert_eq!(aggregate(Vec::new()).positive_ratio, 0.0);
    }

    #[test]
    fn run_pool_skips_unloadable_codes() {
        let prices: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let good = bars(d(2024, 1, 1), &prices);
        let (metrics, skipped) = run_pool(
            "trend",
            &["600000".to_string(), "000001".to_string()],
            &grid(),
            &cfg(),
            |code| match code {
                "600000" => Ok(good.clone()),
                _ => Err(anyhow::anyhow!("无数据")),
            },
        )
        .unwrap();
        assert_eq!(metrics.codes.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, "000001");
        assert!(skipped[0].1.contains("加载失败"), "{}", skipped[0].1);
    }

    #[test]
    fn run_pool_rejects_invalid_grid() {
        // 非数组的网格值应被视为配置错误,直接返回 Err,而不是把它算成某只股票被跳过。
        let bad_grid = "short_window = 3".parse::<toml::Table>().unwrap();
        let result = run_pool("trend", &["600000".to_string()], &bad_grid, &cfg(), |_| {
            Ok(Vec::new())
        });
        assert!(result.is_err());
    }
}

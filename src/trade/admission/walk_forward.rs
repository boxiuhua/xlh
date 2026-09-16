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
use crate::stock::trade_stats::round_trips;
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
    /// 初始现金。默认 0:组合按需注入资金(同 stock::recommend),于是收益与回撤都相对
    /// 「实际投入的资金」度量,不被闲置现金稀释。
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
            initial_cash: 0.0,
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

/// 逐笔收益的基线统计。观察期与 watchdog 都与它比较。
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct TradeBaseline {
    pub avg_return: f64,
    /// 总体标准差
    pub return_sd: f64,
    pub win_rate: f64,
    pub max_consecutive_losses: usize,
    pub count: usize,
}

pub fn trade_baseline(returns: &[f64]) -> TradeBaseline {
    if returns.is_empty() {
        return TradeBaseline::default();
    }
    let n = returns.len() as f64;
    let avg = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - avg) * (r - avg)).sum::<f64>() / n;
    let wins = returns.iter().filter(|r| **r > 0.0).count();
    let mut streak = 0usize;
    let mut worst = 0usize;
    for r in returns {
        if *r <= 0.0 {
            streak += 1;
            worst = worst.max(streak);
        } else {
            streak = 0;
        }
    }
    TradeBaseline {
        avg_return: avg,
        return_sd: var.sqrt(),
        win_rate: wins as f64 / n,
        max_consecutive_losses: worst,
        count: returns.len(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowResult {
    pub window: Window,
    pub params: toml::Value,
    pub is_sharpe: f64,
    pub oos: Summary,
    pub oos_days: i64,
    /// 样本外每次卖出的收益率(FIFO round trip),用于逐笔收益基线(见 `trade_baseline`)。
    pub oos_returns: Vec<f64>,
}

/// 单个检验窗的精简记录(F4):供 `trade_strategy_evals.metrics_json` 保存逐窗选中的
/// 参数与指标(计划 3b 的成绩单据此展示每一窗用了什么参数、跑出什么结果)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowSummary {
    pub train_from: NaiveDate,
    pub train_to: NaiveDate,
    pub test_to: NaiveDate,
    pub params: toml::Value,
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_trades: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeResult {
    pub code: String,
    pub windows: Vec<WindowResult>,
    pub buy_hold_return: f64,
    /// 整段 K 线的起止(不是检验窗跨度),用于数据年限准入判定(见 F1)。
    pub data_from: NaiveDate,
    pub data_to: NaiveDate,
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
    /// 样本外总跨度(年):各检验窗天数之和 / 365,仅用于年化,不用于数据年限准入。
    pub years: f64,
    /// K 线总跨度(年):`data_to - data_from`,准入按它判定是否 ≥ `min_years`(见 F1)。
    pub data_years: f64,
    pub buy_hold_return: f64,
    /// 逐笔收益基线:由所有样本外窗口的 round trip 收益率拼接而成。
    pub trade_baseline: TradeBaseline,
    /// 逐窗选中的参数与指标(F4),供成绩单展示。
    pub window_details: Vec<WindowSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolMetrics {
    pub codes: Vec<CodeMetrics>,
    /// 池内中位数
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_max_drawdown: f64,
    /// 池内求和,仅展示用;准入判定改用 `median_code_trades`(见 F6)。
    pub oos_trades: usize,
    /// 每只样本外交易笔数的中位数(向下取整),准入按它判定,不随池扩大而被稀释。
    pub median_code_trades: usize,
    pub is_sharpe: f64,
    /// 样本外收益为正的股票占比
    pub positive_ratio: f64,
    pub years: f64,
    /// 池内 K 线总跨度(年)中位数,数据年限准入按它判定(见 F1)。
    pub data_years: f64,
    pub buy_hold_return: f64,
    /// 池内逐笔收益基线:avg/sd/win_rate 取各票中位数,max_consecutive_losses 取最大,count 求和。
    pub trade_baseline: TradeBaseline,
    /// 股票池长度(F5):与 `codes.len()` 的比值低于 `min_evaluated_ratio` 时准入不通过。
    pub requested: usize,
    /// 未能评估的代码及原因(加载失败 / 回测失败 / 数据不足)。
    pub skipped: Vec<(String, String)>,
}

/// 池内进度回调。`on_code(code, 已完成, 总数)` 返回 false 表示取消,剩余代码记为「已取消」。
pub struct PoolProgress<'a> {
    pub on_code: &'a mut dyn FnMut(&str, usize, usize) -> bool,
}

/// 训练窗选参依据必须是已知指标,否则静默回退到 sharpe 会让配置形同虚设。
pub fn validate_metric(metric: &str) -> Result<()> {
    match metric {
        "sharpe" | "total_return" | "annualized" | "max_drawdown" => Ok(()),
        other => Err(anyhow!(
            "未知选参指标 {other};可选 sharpe | total_return | annualized | max_drawdown"
        )),
    }
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

/// 整数中位数,偶数个时取中间两值的平均并向下取整(整数除法自动下取)(见 F6)。
fn median_usize(mut xs: Vec<usize>) -> usize {
    if xs.is_empty() {
        return 0;
    }
    xs.sort_unstable();
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2
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
    validate_metric(&cfg.metric)?;
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
        // F7:窗口跨度按实际检验 K 线首末算,而不是按日历天数——`step_days > test_days`
        // 或数据缺口都会让日历跨度偏离真实交易日跨度。此处 test 非空(已过 MIN_TEST_BARS 检查)。
        let oos_days = (test.last().unwrap().date - test.first().unwrap().date)
            .num_days()
            .max(1);
        let oos_returns = round_trips(&oos.trades).iter().map(|rt| rt.ret()).collect();
        out.push(WindowResult {
            window: w,
            params,
            is_sharpe,
            oos: oos.summary,
            oos_days,
            oos_returns,
        });
    }
    // F7:买入持有基准只累乘「被保留」的检验窗,跳过的窗口(数据不足)与
    // step_days > test_days 造成的空档都不计入,与策略侧的样本外收益覆盖同一段时间。
    let buy_hold_return = out.iter().fold(1.0, |acc, w| {
        acc * (1.0 + buy_and_hold_return(bars, w.window.train_to, w.window.test_to))
    }) - 1.0;
    Ok(CodeResult {
        code: code.to_string(),
        windows: out,
        buy_hold_return,
        data_from: first.date,
        data_to: last.date,
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
            data_years: (self.data_to - self.data_from).num_days() as f64 / 365.0,
            buy_hold_return: self.buy_hold_return,
            trade_baseline: trade_baseline(
                &self
                    .windows
                    .iter()
                    .flat_map(|w| w.oos_returns.iter().copied())
                    .collect::<Vec<_>>(),
            ),
            window_details: self
                .windows
                .iter()
                .map(|w| WindowSummary {
                    train_from: w.window.train_from,
                    train_to: w.window.train_to,
                    test_to: w.window.test_to,
                    params: w.params.clone(),
                    oos_return: w.oos.total_return,
                    oos_sharpe: w.oos.sharpe,
                    oos_trades: w.oos.trade_count,
                })
                .collect(),
        }
    }
}

/// 纯聚合:只从已评估的 `codes` 计算池内指标,不知道股票池原本请求了多少只。
/// 因此 `requested` 默认等于 `codes.len()`、`skipped` 默认为空——视作「全部请求都被评估」;
/// `run_pool` 用真实的池长度与跳过列表覆盖这两个字段(见 F5)。
pub fn aggregate(codes: Vec<CodeMetrics>) -> PoolMetrics {
    let positive = codes.iter().filter(|c| c.oos_return > 0.0).count();
    let ratio = if codes.is_empty() {
        0.0
    } else {
        positive as f64 / codes.len() as f64
    };
    let trade_baseline = TradeBaseline {
        avg_return: median(codes.iter().map(|c| c.trade_baseline.avg_return).collect()),
        return_sd: median(codes.iter().map(|c| c.trade_baseline.return_sd).collect()),
        win_rate: median(codes.iter().map(|c| c.trade_baseline.win_rate).collect()),
        max_consecutive_losses: codes
            .iter()
            .map(|c| c.trade_baseline.max_consecutive_losses)
            .max()
            .unwrap_or(0),
        count: codes.iter().map(|c| c.trade_baseline.count).sum(),
    };
    PoolMetrics {
        oos_return: median(codes.iter().map(|c| c.oos_return).collect()),
        oos_sharpe: median(codes.iter().map(|c| c.oos_sharpe).collect()),
        oos_max_drawdown: median(codes.iter().map(|c| c.oos_max_drawdown).collect()),
        oos_trades: codes.iter().map(|c| c.oos_trades).sum(),
        median_code_trades: median_usize(codes.iter().map(|c| c.oos_trades).collect()),
        is_sharpe: median(codes.iter().map(|c| c.is_sharpe).collect()),
        positive_ratio: ratio,
        years: median(codes.iter().map(|c| c.years).collect()),
        data_years: median(codes.iter().map(|c| c.data_years).collect()),
        buy_hold_return: median(codes.iter().map(|c| c.buy_hold_return).collect()),
        trade_baseline,
        requested: codes.len(),
        skipped: Vec::new(),
        codes,
    }
}

/// 对整个股票池跑前推回测。K 线由调用方注入(测试用切片,生产用缓存加载)。
///
/// 先校验网格里每一个参数组合是否合法(配置错误直接 `Err`,不会被误判成某只股票的问题,
/// 见 F9);校验通过后逐只股票跑,单只股票的问题彼此隔离,记录到 `PoolMetrics.skipped`
/// (代码、原因):
/// - K 线加载失败:"加载失败: {原因}"
/// - `run_code` 运行期出错:"回测失败: {原因}"
/// - 未产出任何有效窗口(训练/检验数据不足):"数据不足,无有效窗口"
pub fn run_pool<F>(
    kind: &str,
    pool: &[String],
    grid: &toml::Table,
    cfg: &WalkForwardCfg,
    mut load: F,
    progress: Option<&mut PoolProgress>,
) -> Result<PoolMetrics>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let combos = expand_grid(grid)?;
    // F9:逐个校验网格里的每个参数组合,而不是只看第一个——训练窗按 `cfg.metric`
    // 选出的组合可能是网格里任意一个,只有第一个合法不能保证其余的都合法。
    for c in &combos {
        build_strategy_from(kind, &Some(c.clone()), &[])?;
    }

    let mut metrics = Vec::new();
    let mut skipped = Vec::new();
    let total = pool.len();
    let mut progress = progress;
    for (done, code) in pool.iter().enumerate() {
        match load(code) {
            Ok(bars) => match run_code(kind, code, &bars, grid, cfg) {
                Ok(r) if !r.windows.is_empty() => metrics.push(r.metrics()),
                Ok(_) => skipped.push((code.clone(), "数据不足,无有效窗口".to_string())),
                Err(e) => skipped.push((code.clone(), format!("回测失败: {e:#}"))),
            },
            Err(e) => skipped.push((code.clone(), format!("加载失败: {e:#}"))),
        }
        if let Some(p) = progress.as_deref_mut() {
            if !(p.on_code)(code, done + 1, total) {
                for rest in pool.iter().skip(done + 1) {
                    skipped.push((rest.clone(), "已取消".to_string()));
                }
                break;
            }
        }
    }
    let mut m = aggregate(metrics);
    m.requested = pool.len();
    m.skipped = skipped;
    Ok(m)
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

    /// 锯齿行情:短均线会反复穿越长均线,确保回测真的产生买卖(而不是像单调
    /// 上涨序列那样短均线一直在长均线上方,从不触发 Trend 策略的买卖信号)。
    fn wave_prices(n: usize) -> Vec<f64> {
        (0..n)
            .map(|i| 10.0 + (i as f64 / 3.0).sin() * 2.0 + i as f64 * 0.01)
            .collect()
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
                    oos_returns: Vec::new(),
                },
                WindowResult {
                    window: w,
                    params: toml::Value::Table(Default::default()),
                    is_sharpe: 3.0,
                    oos: summary(0.20, 2.0, 0.30, 7),
                    oos_days: 300,
                    oos_returns: Vec::new(),
                },
            ],
            buy_hold_return: 0.05,
            data_from: d(2024, 1, 1),
            data_to: d(2025, 4, 1),
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
        assert!(
            (m.data_years - 456.0 / 365.0).abs() < 1e-9,
            "2024-01-01 到 2025-04-01 共 456 天(2024 闰年): {}",
            m.data_years
        );
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
        // 190 个交易日的锯齿行情,3 个窗口,短均线会反复穿越长均线产生买卖
        let prices = wave_prices(190);
        let b = bars(d(2024, 1, 1), &prices);
        let out = run_code("trend", "600000", &b, &grid(), &cfg()).unwrap();
        assert!(!out.windows.is_empty(), "应至少产出一个窗口");
        for w in &out.windows {
            assert!(w.params.get("short_window").is_some(), "参数来自网格");
            assert!(w.oos_days > 0);
        }
        assert!(
            out.windows.iter().any(|w| w.oos.trade_count > 0),
            "检验窗应产生成交"
        );
        let m = out.metrics();
        assert_eq!(m.code, "600000");
        assert_eq!(m.windows, out.windows.len());
        assert!(m.years > 0.0);
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
            data_years: 3.0,
            buy_hold_return: 0.05,
            trade_baseline: TradeBaseline::default(),
            window_details: Vec::new(),
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
        assert_eq!(p.median_code_trades, 12, "中位数(10,12,20 取中间)");
        assert!((p.positive_ratio - 2.0 / 3.0).abs() < 1e-9);
        assert!((p.buy_hold_return - 0.05).abs() < 1e-9);
        assert!((p.is_sharpe - 2.0).abs() < 1e-9);
        assert!((p.data_years - 3.0).abs() < 1e-9);
        assert_eq!(p.requested, 3, "纯聚合默认视作全部请求都被评估");
        assert!(p.skipped.is_empty());
        assert_eq!(aggregate(Vec::new()).positive_ratio, 0.0);
    }

    /// F2:默认 `initial_cash = 0` 按需注入资金——组合只注入「实际买入所需」的那笔钱,
    /// 收益与回撤因此相对「真正投入的资金」度量。用一笔固定 `initial_cash = 100_000`
    /// 的旧配置对比:同样的买卖,分母却掺进了从未用于持仓的闲置现金,把收益稀释、
    /// 把回撤(相对峰值权益的跌幅)也稀释变小——回撤被低估正是不安全的方向。
    #[test]
    fn position_size_does_not_dilute_return_and_drawdown() {
        let prices = wave_prices(190);
        let b = bars(d(2024, 1, 1), &prices);
        let g = "short_window = [3]\nlong_window = [10]\namount = [20000.0]"
            .parse::<toml::Table>()
            .unwrap();

        let zero_cash_cfg = cfg(); // initial_cash: 0.0(默认)
        let padded_cash_cfg = WalkForwardCfg {
            initial_cash: 100_000.0,
            ..cfg()
        };

        let a = run_code("trend", "600000", &b, &g, &zero_cash_cfg)
            .unwrap()
            .metrics();
        let b_metrics = run_code("trend", "600000", &b, &g, &padded_cash_cfg)
            .unwrap()
            .metrics();

        assert!(a.oos_trades > 0, "锯齿行情夹具应至少产生一笔样本外成交");
        assert!(
            a.oos_return.abs() > b_metrics.oos_return.abs() * 1.5,
            "零闲置现金的收益应明显大于被 10 万闲置现金稀释后的收益: {} vs {}",
            a.oos_return,
            b_metrics.oos_return
        );
        assert!(
            a.oos_max_drawdown > b_metrics.oos_max_drawdown * 1.5,
            "零闲置现金的回撤应明显大于被闲置现金稀释后的回撤(回撤被低估是不安全的方向): {} vs {}",
            a.oos_max_drawdown,
            b_metrics.oos_max_drawdown
        );
    }

    #[test]
    fn run_pool_skips_unloadable_codes() {
        let prices: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let good = bars(d(2024, 1, 1), &prices);
        let metrics = run_pool(
            "trend",
            &["600000".to_string(), "000001".to_string()],
            &grid(),
            &cfg(),
            |code| match code {
                "600000" => Ok(good.clone()),
                _ => Err(anyhow::anyhow!("无数据")),
            },
            None,
        )
        .unwrap();
        assert_eq!(metrics.codes.len(), 1);
        assert_eq!(metrics.requested, 2, "F5:池长度");
        assert_eq!(metrics.skipped.len(), 1);
        assert_eq!(metrics.skipped[0].0, "000001");
        assert!(
            metrics.skipped[0].1.contains("加载失败"),
            "{}",
            metrics.skipped[0].1
        );
    }

    #[test]
    fn run_pool_rejects_invalid_grid() {
        // 非数组的网格值应被视为配置错误,直接返回 Err,而不是把它算成某只股票被跳过。
        let bad_grid = "short_window = 3".parse::<toml::Table>().unwrap();
        let result = run_pool(
            "trend",
            &["600000".to_string()],
            &bad_grid,
            &cfg(),
            |_| Ok(Vec::new()),
            None,
        );
        assert!(result.is_err());
    }

    /// F9:网格里排在后面的组合非法(short >= long)也要在跑池前直接报错,
    /// 不能只查第一个组合就放行。
    #[test]
    fn run_pool_rejects_invalid_later_combo() {
        let bad_grid = "short_window = [3, 20]\nlong_window = [10]\namount = [20000.0]"
            .parse::<toml::Table>()
            .unwrap();
        let result = run_pool(
            "trend",
            &["600000".to_string()],
            &bad_grid,
            &cfg(),
            |_| Ok(Vec::new()),
            None,
        );
        assert!(result.is_err(), "20/10 违反 short < long,应在跑池前报错");
    }

    /// F4:逐窗选中的参数与指标应保留在 `window_details` 里,供成绩单展示。
    #[test]
    fn metrics_keep_per_window_params() {
        let w = Window {
            train_from: d(2024, 1, 1),
            train_to: d(2024, 3, 1),
            test_to: d(2024, 4, 1),
        };
        let p1 = toml::Value::try_from(toml::Table::from_iter([(
            "short_window".to_string(),
            toml::Value::Integer(3),
        )]))
        .unwrap();
        let p2 = toml::Value::try_from(toml::Table::from_iter([(
            "short_window".to_string(),
            toml::Value::Integer(5),
        )]))
        .unwrap();
        let out = CodeResult {
            code: "600000".to_string(),
            windows: vec![
                WindowResult {
                    window: w,
                    params: p1.clone(),
                    is_sharpe: 2.0,
                    oos: summary(0.10, 1.0, 0.10, 5),
                    oos_days: 100,
                    oos_returns: Vec::new(),
                },
                WindowResult {
                    window: w,
                    params: p2,
                    is_sharpe: 3.0,
                    oos: summary(0.20, 2.0, 0.30, 7),
                    oos_days: 300,
                    oos_returns: Vec::new(),
                },
            ],
            buy_hold_return: 0.05,
            data_from: d(2024, 1, 1),
            data_to: d(2025, 4, 1),
        };
        let m = out.metrics();
        assert_eq!(m.window_details.len(), 2);
        assert_eq!(m.window_details[0].params, p1);
        assert_eq!(m.window_details[0].train_from, w.train_from);
        assert_eq!(m.window_details[0].test_to, w.test_to);
        assert!((m.window_details[0].oos_return - 0.10).abs() < 1e-9);
        assert!((m.window_details[0].oos_sharpe - 1.0).abs() < 1e-9);
        assert_eq!(m.window_details[0].oos_trades, 5);
    }

    /// F7:一个中间窗口因训练数据不足被跳过时,买入持有基准只累乘被保留的窗口,
    /// 与被跳过的窗口互不重叠(用 `step_days` 远大于 `train_days + test_days` 隔开各窗)。
    #[test]
    fn buy_hold_matches_kept_windows_only() {
        let gapped_cfg = WalkForwardCfg {
            train_days: 60,
            test_days: 30,
            step_days: 150,
            ..WalkForwardCfg::default()
        };
        let start = d(2024, 1, 1);
        let prices: Vec<f64> = (0..300).map(|i| 10.0 + i as f64 * 0.03).collect();
        let full = bars(start, &prices);
        // 挖掉中间窗口(索引 1)的整个训练区间 [150,210),让它因训练数据不足被跳过;
        // 该区间与窗口 0([0,90])、窗口 2([300,390])完全不重叠。
        let gap_from = start + chrono::Duration::days(150);
        let gap_to = start + chrono::Duration::days(210);
        let with_gap: Vec<StockBar> = full
            .iter()
            .copied()
            .filter(|b| b.date < gap_from || b.date >= gap_to)
            .collect();

        let ws = windows(
            with_gap.first().unwrap().date,
            with_gap.last().unwrap().date,
            &gapped_cfg,
        );
        assert_eq!(ws.len(), 3, "{ws:?}");

        let out = run_code("trend", "600000", &with_gap, &grid(), &gapped_cfg).unwrap();
        assert_eq!(out.windows.len(), 2, "中间窗口应因训练数据不足被跳过");
        assert_eq!(out.windows[0].window, ws[0]);
        assert_eq!(out.windows[1].window, ws[2]);

        let expected = (1.0 + buy_and_hold_return(&with_gap, ws[0].train_to, ws[0].test_to))
            * (1.0 + buy_and_hold_return(&with_gap, ws[2].train_to, ws[2].test_to))
            - 1.0;
        assert!(
            (out.buy_hold_return - expected).abs() < 1e-9,
            "{} vs {}",
            out.buy_hold_return,
            expected
        );
    }

    /// 本测试模块内的小助手:构造一个 `CodeMetrics`,除 `code`/`oos_return` 外其余填 0 / 空。
    fn sample_code_metrics(code: &str, ret: f64) -> CodeMetrics {
        CodeMetrics {
            code: code.into(),
            windows: 0,
            oos_return: ret,
            oos_annualized: 0.0,
            oos_sharpe: 0.0,
            oos_max_drawdown: 0.0,
            oos_trades: 0,
            is_sharpe: 0.0,
            years: 0.0,
            data_years: 0.0,
            buy_hold_return: 0.0,
            trade_baseline: TradeBaseline::default(),
            window_details: Vec::new(),
        }
    }

    #[test]
    fn trade_baseline_summarises_returns() {
        let b = trade_baseline(&[0.10, -0.05, 0.20, -0.02, -0.03]);
        assert_eq!((b.count, b.max_consecutive_losses), (5, 2));
        assert!((b.win_rate - 0.4).abs() < 1e-9);
        assert!((b.avg_return - 0.04).abs() < 1e-9);
        // 总体标准差:sqrt(Σ(x-μ)²/n)
        let var = [0.10, -0.05, 0.20, -0.02, -0.03]
            .iter()
            .map(|x| (x - 0.04) * (x - 0.04))
            .sum::<f64>()
            / 5.0;
        assert!((b.return_sd - var.sqrt()).abs() < 1e-9);
        let empty = trade_baseline(&[]);
        assert_eq!((empty.count, empty.max_consecutive_losses), (0, 0));
        assert_eq!(
            (empty.avg_return, empty.return_sd, empty.win_rate),
            (0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn code_metrics_carry_trade_baseline_from_oos_windows() {
        let prices = wave_prices(190);
        let b = bars(d(2024, 1, 1), &prices);
        let out = run_code("trend", "600000", &b, &grid(), &cfg()).unwrap();
        let m = out.metrics();
        assert_eq!(
            m.trade_baseline.count,
            m.oos_trades.min(m.trade_baseline.count),
            "逐笔基线来自样本外成交"
        );
        assert!(m.trade_baseline.count > 0, "锯齿行情应产生成交");
        assert!(m.trade_baseline.win_rate >= 0.0 && m.trade_baseline.win_rate <= 1.0);
    }

    #[test]
    fn progress_reports_each_code_and_cancels() {
        let prices = wave_prices(190);
        let good = bars(d(2024, 1, 1), &prices);
        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        let mut cb = |code: &str, done: usize, total: usize| {
            seen.push((code.to_string(), done, total));
            done < 1 // 第一只之后取消
        };
        let mut p = PoolProgress { on_code: &mut cb };
        let m = run_pool(
            "trend",
            &["600000".into(), "000001".into()],
            &grid(),
            &cfg(),
            |_| Ok(good.clone()),
            Some(&mut p),
        )
        .unwrap();
        assert_eq!(seen.len(), 1, "取消后不再继续");
        assert_eq!(m.codes.len(), 1);
        assert!(m
            .skipped
            .iter()
            .any(|(c, r)| c == "000001" && r.contains("取消")));
    }

    #[test]
    fn unknown_metric_is_rejected() {
        assert!(validate_metric("sharpe").is_ok());
        assert!(validate_metric("sharp").is_err());
    }

    #[test]
    fn pool_baseline_takes_medians_and_worst_streak() {
        let mk = |avg: f64, sd: f64, wr: f64, streak: usize, n: usize| TradeBaseline {
            avg_return: avg,
            return_sd: sd,
            win_rate: wr,
            max_consecutive_losses: streak,
            count: n,
        };
        let mut a = sample_code_metrics("a", 0.1);
        a.trade_baseline = mk(0.02, 0.01, 0.5, 2, 10);
        let mut b = sample_code_metrics("b", 0.2);
        b.trade_baseline = mk(0.04, 0.03, 0.7, 5, 20);
        let p = aggregate(vec![a, b]);
        assert!((p.trade_baseline.avg_return - 0.03).abs() < 1e-9, "中位数");
        assert_eq!(p.trade_baseline.max_consecutive_losses, 5, "取最差");
        assert_eq!(p.trade_baseline.count, 30, "求和");
    }
}

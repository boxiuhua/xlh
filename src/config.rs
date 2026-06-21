use std::path::PathBuf;
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use crate::broker::{FeeModel, SellTier};
use crate::strategy::{Period, Strategy};
use crate::strategy::dca::Dca;
use crate::strategy::smart_dca::SmartDca;
use crate::strategy::trend::Trend;
use crate::strategy::rsi::Rsi;
use crate::strategy::adaptive::Adaptive;
use crate::strategy::rules::{Rule, RuleLayer};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub data: DataCfg,
    pub fees: FeesCfg,
    pub strategy: StrategyCfg,
    #[serde(default)]
    pub rules: Vec<RuleCfg>,
    #[serde(default)]
    pub portfolio: PortfolioCfg,
    pub report: ReportCfg,
    #[serde(default)]
    pub compare: Vec<CompareRun>,
    #[serde(default)]
    pub optimize: Option<OptimizeCfg>,
}

#[derive(Debug, Deserialize)]
pub struct OptimizeCfg {
    pub strategy: String,
    pub metric: String,
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    pub grid: toml::Table,
    #[serde(default)]
    pub rules: Vec<RuleCfg>,
}

fn default_top_n() -> usize { 5 }

#[derive(Debug, Deserialize)]
pub struct CompareRun {
    pub name: String,
    #[serde(default)]
    pub fund_code: Option<String>,
    pub strategy: StrategyCfg,
    #[serde(default)]
    pub rules: Vec<RuleCfg>,
    #[serde(default)]
    pub initial_cash: f64,
}

#[derive(Debug, Deserialize)]
pub struct DataCfg {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub cache_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct FeesCfg {
    pub buy_rate: f64,
    pub sell_tiers: Vec<SellTierCfg>,
}

#[derive(Debug, Deserialize)]
pub struct SellTierCfg { pub max_days: i64, pub rate: f64 }

#[derive(Debug, Deserialize)]
pub struct StrategyCfg {
    pub kind: String,
    #[serde(default)]
    pub params: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct RuleCfg {
    pub kind: String,
    #[serde(default)]
    pub target_return: f64,
    #[serde(default)]
    pub max_drawdown: f64,
}

#[derive(Debug, Default, Deserialize)]
pub struct PortfolioCfg {
    #[serde(default)]
    pub initial_cash: f64,
}

#[derive(Debug, Deserialize)]
pub struct ReportCfg {
    pub chart: bool,
    pub out_dir: PathBuf,
    #[serde(default)]
    pub html: bool,
}

// 各策略参数结构
#[derive(Debug, Deserialize)]
struct DcaParams { period: String, day: u32, base_amount: f64 }

#[derive(Debug, Deserialize)]
struct SmartDcaParams { period: String, day: u32, base_amount: f64, ma_window: usize, #[serde(default = "one")] k: f64 }
fn one() -> f64 { 1.0 }

#[derive(Debug, Deserialize)]
struct TrendParams { short_window: usize, long_window: usize, amount: f64 }

#[derive(Debug, Deserialize)]
struct RsiParams { rsi_window: usize, oversold: f64, overbought: f64, amount: f64 }

#[derive(Debug, Deserialize)]
struct AdaptiveParams { period: String, day: u32, base_amount: f64 }

fn parse_period(s: &str) -> Result<Period> {
    match s.to_lowercase().as_str() {
        "monthly" => Ok(Period::Monthly),
        "weekly" => Ok(Period::Weekly),
        other => Err(anyhow!("未知定投周期: {other}")),
    }
}

pub fn load(path: &std::path::Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取配置 {} 失败: {e}", path.display()))?;
    let cfg: Config = toml::from_str(&text).map_err(|e| anyhow!("配置解析失败: {e}"))?;
    validate(&cfg)?;
    Ok(cfg)
}

fn validate(cfg: &Config) -> Result<()> {
    if cfg.data.start >= cfg.data.end {
        return Err(anyhow!(
            "配置错误: data.start ({}) 必须早于 data.end ({})",
            cfg.data.start, cfg.data.end
        ));
    }
    if !(0.0..1.0).contains(&cfg.fees.buy_rate) {
        return Err(anyhow!(
            "配置错误: fees.buy_rate ({}) 必须在 [0.0, 1.0) 范围内",
            cfg.fees.buy_rate
        ));
    }
    for t in &cfg.fees.sell_tiers {
        if !(0.0..1.0).contains(&t.rate) {
            return Err(anyhow!(
                "配置错误: fees.sell_tiers 中 rate ({}) 必须在 [0.0, 1.0) 范围内",
                t.rate
            ));
        }
    }
    Ok(())
}

pub fn build_fee(cfg: &Config) -> FeeModel {
    FeeModel {
        buy_rate: cfg.fees.buy_rate,
        sell_tiers: cfg.fees.sell_tiers.iter()
            .map(|t| SellTier { max_days: t.max_days, rate: t.rate })
            .collect(),
    }
}

fn build_rules_from(rules: &[RuleCfg]) -> Result<Vec<Rule>> {
    rules.iter().map(|r| match r.kind.as_str() {
        "take_profit" => Ok(Rule::TakeProfit { target_return: r.target_return }),
        "stop_loss" => Ok(Rule::StopLoss { max_drawdown: r.max_drawdown }),
        other => Err(anyhow!("未知规则: {other}")),
    }).collect()
}

pub fn build_strategy_from(
    kind: &str,
    params: &Option<toml::Value>,
    rules: &[RuleCfg],
) -> Result<Box<dyn Strategy>> {
    let params = params.clone().unwrap_or(toml::Value::Table(toml::Table::new()));
    let base: Box<dyn Strategy> = match kind {
        "dca" => {
            let p: DcaParams = params.try_into()?;
            Box::new(Dca::new(parse_period(&p.period)?, p.day, p.base_amount))
        }
        "smart_dca" => {
            let p: SmartDcaParams = params.try_into()?;
            if p.ma_window < 1 {
                return Err(anyhow!("配置错误: smart_dca.ma_window 必须 >= 1，当前值: {}", p.ma_window));
            }
            Box::new(SmartDca::new(parse_period(&p.period)?, p.day, p.base_amount, p.ma_window, p.k))
        }
        "trend" => {
            let p: TrendParams = params.try_into()?;
            if p.short_window < 1 {
                return Err(anyhow!("配置错误: trend.short_window 必须 >= 1，当前值: {}", p.short_window));
            }
            if p.short_window >= p.long_window {
                return Err(anyhow!(
                    "配置错误: trend.short_window ({}) 必须小于 long_window ({})",
                    p.short_window, p.long_window
                ));
            }
            Box::new(Trend::new(p.short_window, p.long_window, p.amount))
        }
        "rsi" => {
            let p: RsiParams = params.try_into()?;
            if p.rsi_window < 1 {
                return Err(anyhow!("配置错误: rsi.rsi_window 必须 >= 1，当前值: {}", p.rsi_window));
            }
            if !(0.0..=100.0).contains(&p.oversold) || !(0.0..=100.0).contains(&p.overbought) {
                return Err(anyhow!("配置错误: rsi.oversold/overbought 必须在 [0,100]"));
            }
            if p.oversold >= p.overbought {
                return Err(anyhow!("配置错误: rsi.oversold ({}) 必须小于 overbought ({})", p.oversold, p.overbought));
            }
            if p.amount <= 0.0 {
                return Err(anyhow!("配置错误: rsi.amount 必须 > 0，当前值: {}", p.amount));
            }
            Box::new(Rsi::new(p.rsi_window, p.oversold, p.overbought, p.amount))
        }
        "adaptive" => {
            let p: AdaptiveParams = params.try_into()?;
            if p.base_amount <= 0.0 {
                return Err(anyhow!("配置错误: adaptive.base_amount 必须 > 0，当前值: {}", p.base_amount));
            }
            Box::new(Adaptive::new(parse_period(&p.period)?, p.day, p.base_amount))
        }
        other => return Err(anyhow!("未知策略: {other}")),
    };
    let rules = build_rules_from(rules)?;
    if rules.is_empty() { Ok(base) } else { Ok(Box::new(RuleLayer::new(base, rules))) }
}

pub fn build_strategy(cfg: &Config) -> Result<Box<dyn Strategy>> {
    build_strategy_from(&cfg.strategy.kind, &cfg.strategy.params, &cfg.rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[data]
fund_code = "161725"
start = "2020-01-01"
end = "2024-12-31"
cache_dir = ".cache"

[fees]
buy_rate = 0.0015
sell_tiers = [
  { max_days = 7, rate = 0.015 },
  { max_days = 365, rate = 0.005 },
  { max_days = 0, rate = 0.0 },
]

[strategy]
kind = "smart_dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
ma_window = 250
k = 1.0

[[rules]]
kind = "take_profit"
target_return = 0.3

[portfolio]
initial_cash = 0.0

[report]
chart = true
out_dir = "output"
"#;

    #[test]
    fn parses_full_config() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.data.fund_code, "161725");
        assert_eq!(cfg.fees.sell_tiers.len(), 3);
        assert_eq!(cfg.strategy.kind, "smart_dca");
        assert_eq!(cfg.rules.len(), 1);
        assert!(cfg.report.chart);
    }

    #[test]
    fn builds_fee_model_and_strategy() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        let fee = build_fee(&cfg);
        assert!((fee.buy_rate - 0.0015).abs() < 1e-9);
        // 构建策略不应 panic
        let _strat = build_strategy(&cfg).unwrap();
    }

    #[test]
    fn dca_minimal_config() {
        let s = r#"
[data]
fund_code="000001"
start="2020-01-01"
end="2020-12-31"
cache_dir=".cache"
[fees]
buy_rate=0.0
sell_tiers=[{max_days=0, rate=0.0}]
[strategy]
kind="dca"
[strategy.params]
period="monthly"
day=1
base_amount=500.0
[report]
chart=false
out_dir="output"
"#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert!(build_strategy(&cfg).is_ok());
    }

    fn base_cfg_text(start: &str, end: &str, buy_rate: &str, tier_rate: &str) -> String {
        format!(r#"
[data]
fund_code="000001"
start="{start}"
end="{end}"
cache_dir=".cache"
[fees]
buy_rate={buy_rate}
sell_tiers=[{{max_days=0, rate={tier_rate}}}]
[strategy]
kind="dca"
[strategy.params]
period="monthly"
day=1
base_amount=500.0
[report]
chart=false
out_dir="output"
"#)
    }

    #[test]
    fn rejects_start_after_end() {
        let text = base_cfg_text("2024-12-31", "2024-01-01", "0.0015", "0.0");
        let cfg: Config = toml::from_str(&text).unwrap();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("start"), "error should mention start: {err}");
    }

    #[test]
    fn rejects_bad_fee_rate() {
        // buy_rate >= 1.0 is invalid
        let text = base_cfg_text("2020-01-01", "2024-12-31", "1.5", "0.0");
        let cfg: Config = toml::from_str(&text).unwrap();
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("buy_rate"), "error should mention buy_rate: {err}");

        // sell tier rate >= 1.0 is invalid
        let text2 = base_cfg_text("2020-01-01", "2024-12-31", "0.0015", "1.0");
        let cfg2: Config = toml::from_str(&text2).unwrap();
        let err2 = validate(&cfg2).unwrap_err();
        assert!(err2.to_string().contains("sell_tiers"), "error should mention sell_tiers: {err2}");
    }

    #[test]
    fn parses_compare_runs() {
        let s = r#"
[data]
fund_code = "161725"
start = "2020-01-01"
end = "2024-12-31"
cache_dir = ".cache"
[fees]
buy_rate = 0.0015
sell_tiers = [{max_days = 0, rate = 0.0}]
[strategy]
kind = "dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
[report]
chart = false
out_dir = "output"

[[compare]]
name = "普通定投"
fund_code = "161725"
[compare.strategy]
kind = "dca"
[compare.strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0

[[compare]]
name = "均线择时"
[compare.strategy]
kind = "trend"
[compare.strategy.params]
short_window = 20
long_window = 60
amount = 1000.0
"#;
        let cfg: Config = toml::from_str(s).unwrap();
        assert_eq!(cfg.compare.len(), 2);
        assert_eq!(cfg.compare[0].name, "普通定投");
        assert_eq!(cfg.compare[0].strategy.kind, "dca");
        assert_eq!(cfg.compare[1].name, "均线择时");
        assert_eq!(cfg.compare[1].strategy.kind, "trend");
    }

    #[test]
    fn build_strategy_from_dca_ok() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("period".to_string(), toml::Value::String("monthly".to_string()));
            t.insert("day".to_string(), toml::Value::Integer(1));
            t.insert("base_amount".to_string(), toml::Value::Float(500.0));
            t
        });
        let result = build_strategy_from("dca", &Some(params), &[]);
        assert!(result.is_ok(), "build_strategy_from dca should succeed");
    }

    #[test]
    fn rejects_trend_short_ge_long() {
        let text = r#"
[data]
fund_code="000001"
start="2020-01-01"
end="2024-12-31"
cache_dir=".cache"
[fees]
buy_rate=0.0015
sell_tiers=[{max_days=0, rate=0.0}]
[strategy]
kind="trend"
[strategy.params]
short_window=50
long_window=20
amount=1000.0
[report]
chart=false
out_dir="output"
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        let result = build_strategy(&cfg);
        assert!(result.is_err(), "build_strategy should fail when short_window >= long_window");
        let err = result.err().unwrap();
        assert!(err.to_string().contains("short_window"), "error should mention short_window: {err}");
    }

    #[test]
    fn parses_optimize_section() {
        let s = r#"
[data]
fund_code = "161725"
start = "2020-01-01"
end = "2024-12-31"
cache_dir = ".cache"
[fees]
buy_rate = 0.0015
sell_tiers = [{max_days = 0, rate = 0.0}]
[strategy]
kind = "dca"
[strategy.params]
period = "monthly"
day = 1
base_amount = 1000.0
[report]
chart = false
out_dir = "output"

[optimize]
strategy = "smart_dca"
metric = "sharpe"
[optimize.grid]
period = ["monthly"]
day = [1]
base_amount = [1000.0]
ma_window = [120, 250, 500]
k = [0.5, 1.0, 1.5]
"#;
        let cfg: Config = toml::from_str(s).unwrap();
        let opt = cfg.optimize.expect("optimize section should parse");
        assert_eq!(opt.strategy, "smart_dca");
        assert_eq!(opt.metric, "sharpe");
        assert_eq!(opt.top_n, 5, "top_n should default to 5");
        assert_eq!(opt.grid.len(), 5, "grid should have 5 params");
        assert!(opt.grid.contains_key("ma_window"));
    }

    #[test]
    fn optimize_absent_is_none() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert!(cfg.optimize.is_none());
    }

    #[test]
    fn build_strategy_from_rsi_ok() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(30.0));
            t.insert("overbought".into(), toml::Value::Float(70.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
            t
        });
        assert!(build_strategy_from("rsi", &Some(params), &[]).is_ok());
    }

    #[test]
    fn build_strategy_from_adaptive_ok() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("period".into(), toml::Value::String("monthly".into()));
            t.insert("day".into(), toml::Value::Integer(1));
            t.insert("base_amount".into(), toml::Value::Float(1000.0));
            t
        });
        assert!(build_strategy_from("adaptive", &Some(params), &[]).is_ok());
    }

    #[test]
    fn rejects_rsi_oversold_ge_overbought() {
        let params = toml::Value::Table({
            let mut t = toml::Table::new();
            t.insert("rsi_window".into(), toml::Value::Integer(14));
            t.insert("oversold".into(), toml::Value::Float(70.0));
            t.insert("overbought".into(), toml::Value::Float(30.0));
            t.insert("amount".into(), toml::Value::Float(1000.0));
            t
        });
        let err = build_strategy_from("rsi", &Some(params), &[]).err().unwrap();
        assert!(err.to_string().contains("oversold"), "应提示 oversold: {err}");
    }
}

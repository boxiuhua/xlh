use anyhow::{anyhow, Result};
use crate::config::{build_strategy_from, OptimizeCfg};
use crate::data::NavPoint;
use crate::runner::{run_one, RunOutcome};
use crate::broker::FeeModel;

/// 把 {参数名 -> 值数组} 的网格展开成每个组合一个 `toml::Value::Table`。
/// 笛卡尔积按 grid 键的字典序（BTreeMap 迭代序）稳定展开。
pub fn expand_grid(grid: &toml::Table) -> Result<Vec<toml::Value>> {
    if grid.is_empty() {
        return Err(anyhow!("optimize.grid 不能为空"));
    }
    // 收集各维度 (键, 取值数组)，按 grid 迭代序（字典序）
    let mut dims: Vec<(&String, &Vec<toml::Value>)> = Vec::new();
    for (k, v) in grid {
        match v {
            toml::Value::Array(arr) if !arr.is_empty() => dims.push((k, arr)),
            toml::Value::Array(_) => return Err(anyhow!("optimize.grid 参数 {k} 的取值数组为空")),
            _ => return Err(anyhow!("optimize.grid 参数 {k} 必须是数组，例如 {k} = [..]")),
        }
    }
    // 笛卡尔积
    let mut combos: Vec<toml::Table> = vec![toml::Table::new()];
    for (k, arr) in &dims {
        let mut next = Vec::with_capacity(combos.len() * arr.len());
        for base in &combos {
            for val in arr.iter() {
                let mut t = base.clone();
                t.insert((*k).clone(), val.clone());
                next.push(t);
            }
        }
        combos = next;
    }
    Ok(combos.into_iter().map(toml::Value::Table).collect())
}

pub struct OptOutcome {
    pub params: toml::Value,
    pub label: String,
    pub outcome: RunOutcome,
}

pub struct OptReport {
    pub strategy: String,
    pub metric: String,
    pub top_n: usize,
    pub ranked: Vec<OptOutcome>,
    pub param_keys: Vec<String>,
}

impl std::fmt::Debug for OptReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptReport")
            .field("strategy", &self.strategy)
            .field("metric", &self.metric)
            .field("top_n", &self.top_n)
            .field("ranked_len", &self.ranked.len())
            .field("param_keys", &self.param_keys)
            .finish()
    }
}

const METRICS: [&str; 4] = ["total_return", "annualized", "sharpe", "max_drawdown"];

fn metric_value(s: &crate::metrics::Summary, metric: &str) -> f64 {
    match metric {
        "total_return" => s.total_return,
        "annualized" => s.annualized,
        "sharpe" => s.sharpe,
        "max_drawdown" => s.max_drawdown,
        other => unreachable!("metric_value 收到未经校验的 metric: {other}"),
    }
}

/// 由变化维度（取值多于一个的参数）拼出紧凑标签；全固定时用序号。
fn make_label(combo: &toml::Value, varying: &[String], idx: usize) -> String {
    let t = combo.as_table().expect("combo 应为 table");
    if varying.is_empty() {
        return format!("#{}", idx + 1);
    }
    varying.iter()
        .map(|k| format!("{}={}", k, t.get(k).map(|v| v.to_string()).unwrap_or_default()))
        .collect::<Vec<_>>()
        .join(",")
}

pub fn run_optimize(
    cfg: &OptimizeCfg,
    fund_code: &str,
    points: &[NavPoint],
    fee: FeeModel,
    initial_cash: f64,
) -> Result<OptReport> {
    if !METRICS.contains(&cfg.metric.as_str()) {
        return Err(anyhow!("未知排序 metric: {}，合法取值: {:?}", cfg.metric, METRICS));
    }
    let combos = expand_grid(&cfg.grid)?;
    if combos.len() > 200 {
        eprintln!("⚠ 参数组合数 {} 较多，回测可能较慢", combos.len());
    }
    let param_keys: Vec<String> = cfg.grid.keys().cloned().collect();
    let varying: Vec<String> = param_keys.iter()
        .filter(|k| matches!(cfg.grid.get(k.as_str()), Some(toml::Value::Array(a)) if a.len() > 1))
        .cloned()
        .collect();

    let mut ranked = Vec::with_capacity(combos.len());
    for (i, combo) in combos.into_iter().enumerate() {
        let label = make_label(&combo, &varying, i);
        let strategy = build_strategy_from(&cfg.strategy, &Some(combo.clone()), &cfg.rules)
            .map_err(|e| anyhow!("组合 [{label}] 构建策略失败: {e}"))?;
        let outcome = run_one(label.clone(), fund_code.to_string(), points.to_vec(), strategy, fee.clone(), initial_cash);
        ranked.push(OptOutcome { params: combo, label, outcome });
    }

    let descending = cfg.metric != "max_drawdown";
    ranked.sort_by(|a, b| {
        let (va, vb) = (metric_value(&a.outcome.summary, &cfg.metric), metric_value(&b.outcome.summary, &cfg.metric));
        if descending { vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal) } else { va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal) }
    });

    Ok(OptReport {
        strategy: cfg.strategy.clone(),
        metric: cfg.metric.clone(),
        top_n: cfg.top_n,
        ranked,
        param_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::{FeeModel, SellTier};
    use crate::config::OptimizeCfg;
    use crate::data::NavPoint;
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate { NaiveDate::from_ymd_opt(y, m, day).unwrap() }
    fn no_fee() -> FeeModel { FeeModel { buy_rate: 0.0, sell_tiers: vec![SellTier { max_days: 0, rate: 0.0 }] } }

    fn sample_points() -> Vec<NavPoint> {
        vec![
            NavPoint { date: d(2024, 1, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 1), nav: 1.0, acc_nav: 1.0 },
            NavPoint { date: d(2024, 2, 15), nav: 2.0, acc_nav: 2.0 },
        ]
    }

    // grid: smart_dca，ma_window 取两值（其余固定单值）
    fn smart_dca_cfg(metric: &str) -> OptimizeCfg {
        let toml_text = format!(r#"
strategy = "smart_dca"
metric = "{metric}"
[grid]
period = ["monthly"]
day = [1]
base_amount = [1000.0]
ma_window = [1, 2]
k = [1.0]
"#);
        toml::from_str(&toml_text).unwrap()
    }

    #[test]
    fn runs_all_combos_and_ranks() {
        let cfg = smart_dca_cfg("total_return");
        let report = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap();
        assert_eq!(report.ranked.len(), 2, "ma_window 两值 → 2 组合");
        assert_eq!(report.param_keys, vec!["base_amount", "day", "k", "ma_window", "period"],
            "param_keys 按字典序");
        // total_return 降序：第一个 >= 第二个
        assert!(report.ranked[0].outcome.summary.total_return
            >= report.ranked[1].outcome.summary.total_return);
        // label 只含变化维度 ma_window
        assert!(report.ranked[0].label.contains("ma_window"), "label: {}", report.ranked[0].label);
        assert!(!report.ranked[0].label.contains("period"), "固定维度不入 label: {}", report.ranked[0].label);
    }

    #[test]
    fn max_drawdown_sorts_ascending() {
        let cfg = smart_dca_cfg("max_drawdown");
        let report = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap();
        // max_drawdown 越小越优 → 升序：第一个 <= 第二个
        assert!(report.ranked[0].outcome.summary.max_drawdown
            <= report.ranked[1].outcome.summary.max_drawdown);
    }

    #[test]
    fn rejects_bad_metric() {
        let cfg = smart_dca_cfg("bogus");
        let err = run_optimize(&cfg, "161725", &sample_points(), no_fee(), 0.0).unwrap_err();
        assert!(err.to_string().contains("metric"), "error should mention metric: {err}");
    }

    fn arr_table() -> toml::Table {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]));
        t.insert("b".into(), toml::Value::Array(vec![toml::Value::String("x".into())]));
        t
    }

    #[test]
    fn expands_cartesian_product() {
        let combos = expand_grid(&arr_table()).unwrap();
        assert_eq!(combos.len(), 2, "2x1 = 2 combos");
        // 每个组合含 a 与 b
        for c in &combos {
            let t = c.as_table().unwrap();
            assert!(t.contains_key("a") && t.contains_key("b"));
            assert_eq!(t["b"].as_str(), Some("x"));
        }
        // a 取值覆盖 1 和 2
        let a_vals: Vec<i64> = combos.iter().map(|c| c.as_table().unwrap()["a"].as_integer().unwrap()).collect();
        assert!(a_vals.contains(&1) && a_vals.contains(&2));
    }

    #[test]
    fn expands_multiple_varying_dims() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2)]));
        t.insert("b".into(), toml::Value::Array(vec![toml::Value::Integer(1), toml::Value::Integer(2), toml::Value::Integer(3)]));
        let combos = expand_grid(&t).unwrap();
        assert_eq!(combos.len(), 6, "2x3 = 6 combos");
        // 所有组合两两不同（(a,b) 对去重后仍是 6）
        let mut seen = std::collections::HashSet::new();
        for c in &combos {
            let tt = c.as_table().unwrap();
            let key = (tt["a"].as_integer().unwrap(), tt["b"].as_integer().unwrap());
            assert!(seen.insert(key), "组合应唯一: {:?}", key);
        }
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn rejects_non_array_value() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Integer(1)); // 非数组
        let err = expand_grid(&t).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name param a: {err}");
    }

    #[test]
    fn rejects_empty_array() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::Array(vec![]));
        let err = expand_grid(&t).unwrap_err();
        assert!(err.to_string().contains("a"), "error should name param a: {err}");
    }

    #[test]
    fn rejects_empty_grid() {
        let err = expand_grid(&toml::Table::new()).unwrap_err();
        assert!(err.to_string().contains("grid"), "error should mention grid: {err}");
    }
}

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
    /// 训练段回测。**参数就是在这段上挑出来的**，所以这里的绩效是 argmax 的结果 ——
    /// 它是选择偏差的上界，不是任何意义上的预期收益。别拿它做决策。
    pub outcome: RunOutcome,
    /// 检验段回测：拿训练段选出的这组参数，在**没见过的数据**上实测。
    /// 数据不足以切分时为 `None`。
    ///
    /// 训练段与检验段的落差就是过拟合的量度。落差越大，这组"最优参数"越不可外推。
    pub oos: Option<RunOutcome>,
}

pub struct OptReport {
    pub strategy: String,
    pub metric: String,
    pub top_n: usize,
    pub ranked: Vec<OptOutcome>,
    pub param_keys: Vec<String>,
    /// 训练段占比；`None` 表示数据不足、未能切分（此时全部为 in-sample）
    pub split_ratio: Option<f64>,
    /// 参数组合总数。组合越多，argmax 出来的"最优"越可能只是噪声。
    pub combos: usize,
    /// 过拟合警示。**必带**，且必须渲染给用户 —— 这个 tab 的产出天然会被
    /// 当成"可用的最优参数"，不警示就是误导。
    pub caveat: String,
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

/// 训练段占比。与 `recommend.rs` 的默认切分保持一致。
pub const SPLIT_RATIO: f64 = 0.70;

/// 网格寻优。
///
/// ## 这里曾经是纯粹的数据窥探
///
/// 旧实现把**整段**数据交给每个参数组合，按指标 argmax，然后把那个最大值当绩效报出来 ——
/// 没有训练/检验切分、没有 walk-forward、报告里连一句过拟合警示都没有。
/// 讽刺的是 `recommend.rs` 早就做了 70/30 切分，注释还写着"不逐基金寻优，降低过拟合"——
/// 而寻优 tab 干的正是它刻意避免的事。
///
/// 现在：**在训练段选参数，在检验段实测**。两组数字并排给出，落差即过拟合的量度。
/// 数据不足以切分时不再假装无事发生，而是在 `caveat` 里明说这批数字全是 in-sample。
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
    let n_combos = combos.len();
    let param_keys: Vec<String> = cfg.grid.keys().cloned().collect();
    let varying: Vec<String> = param_keys.iter()
        .filter(|k| matches!(cfg.grid.get(k.as_str()), Some(toml::Value::Array(a)) if a.len() > 1))
        .cloned()
        .collect();

    // 切分：训练段选参数，检验段实测。切不动就退回全段 in-sample，但必须在 caveat 里说清。
    let split = crate::recommend::split_history(points, SPLIT_RATIO);
    let (train, test) = match split {
        Some((tr, te)) => (tr, Some(te)),
        None => (points, None),
    };

    let mut ranked = Vec::with_capacity(n_combos);
    for (i, combo) in combos.into_iter().enumerate() {
        let label = make_label(&combo, &varying, i);
        let mk = || build_strategy_from(&cfg.strategy, &Some(combo.clone()), &cfg.rules)
            .map_err(|e| anyhow!("组合 [{label}] 构建策略失败: {e}"));

        let outcome = run_one(label.clone(), fund_code.to_string(), train.to_vec(),
                              mk()?, fee.clone(), initial_cash);
        // 检验段用**同一组参数**重跑一遍（策略是有状态的，必须重新构建）
        let oos = test.map(|te| run_one(label.clone(), fund_code.to_string(), te.to_vec(),
                                        mk().expect("参数已校验"), fee.clone(), initial_cash));
        ranked.push(OptOutcome { params: combo, label, outcome, oos });
    }

    // 排序用**训练段**指标 —— 这正是真实的参数选择过程：挑参数时你看不到检验段。
    // 若改用检验段排序，就等于在检验集上挑赢家，检验段的成绩也就不再无偏（winner's curse）。
    let descending = cfg.metric != "max_drawdown";
    ranked.sort_by(|a, b| {
        let (va, vb) = (metric_value(&a.outcome.summary, &cfg.metric), metric_value(&b.outcome.summary, &cfg.metric));
        if descending { vb.partial_cmp(&va).unwrap_or(std::cmp::Ordering::Equal) } else { va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal) }
    });

    Ok(OptReport {
        strategy: cfg.strategy.clone(),
        metric: cfg.metric.clone(),
        top_n: cfg.top_n,
        caveat: build_caveat(n_combos, test.is_some(), &ranked, &cfg.metric),
        split_ratio: test.map(|_| SPLIT_RATIO),
        combos: n_combos,
        ranked,
        param_keys,
    })
}

/// 过拟合警示。内容随实际结果变化 —— 尤其是把训练/检验的落差算出来摆在用户面前。
fn build_caveat(n_combos: usize, has_oos: bool, ranked: &[OptOutcome], metric: &str) -> String {
    let mut s = String::new();

    if !has_oos {
        s.push_str(&format!(
            "⚠ 数据不足以切分训练/检验段（需训练≥{} 且检验≥{} 个净值点）。\
             下列全部为「in-sample（样本内）」结果：参数是在这同一段数据上挑出来的，\
             绩效也是在这同一段上算的 —— 这是纯粹的数据窥探，那个\"最优\"值是 {} 个组合里\
             argmax 出来的最大值，不可外推、不代表任何预期收益。请拉长时间区间后重试。",
            crate::recommend::MIN_TRAIN, crate::recommend::MIN_TEST, n_combos));
        return s;
    }

    s.push_str(&format!(
        "参数在「训练段」（前 {:.0}%）上从 {} 个组合里选出，绩效在「检验段」（后 {:.0}%，\
         选参数时未见过）上实测。请只看检验段的数字做判断。",
        SPLIT_RATIO * 100.0, n_combos, (1.0 - SPLIT_RATIO) * 100.0));

    // 把过拟合的量级直接算给用户看
    if let Some(best) = ranked.first() {
        if let Some(oos) = &best.oos {
            let is_v = metric_value(&best.outcome.summary, metric);
            let oos_v = metric_value(&oos.summary, metric);
            s.push_str(&format!(
                "\n本次最优组合 [{}] 的 {metric}：训练段 {:.3} → 检验段 {:.3}。",
                best.label, is_v, oos_v));
            // max_drawdown 越小越好，方向相反
            let degraded = if metric == "max_drawdown" { oos_v > is_v } else { oos_v < is_v };
            if degraded {
                s.push_str("训练段明显更好看 —— 这个落差就是过拟合的量度，是挑参数这个动作本身造出来的。");
            }
        }
    }

    if n_combos > 50 {
        s.push_str(&format!(
            "\n⚠ 你搜了 {n_combos} 个组合。组合越多，仅靠运气就能在训练段跑出漂亮数字的\
             概率越高（多重比较问题）—— 训练段的\"最优\"很可能只是噪声。"));
    }
    s
}

#[cfg(test)]
mod overfit_guard_tests {
    use super::*;
    use crate::config::OptimizeCfg;
    use crate::broker::SellTier;
    use chrono::NaiveDate;

    fn pts(n: usize) -> Vec<NavPoint> {
        (0..n).map(|i| {
            let nav = 1.0 + (i as f64) * 0.001;
            NavPoint {
                date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
                nav, acc_nav: nav,
            }
        }).collect()
    }

    fn cfg(values: Vec<i64>) -> OptimizeCfg {
        let mut grid = toml::Table::new();
        // smart_dca 的必填参数（固定值放单元素数组，只让 ma_window 变化）
        grid.insert("period".into(), toml::Value::Array(vec![toml::Value::String("monthly".into())]));
        grid.insert("day".into(), toml::Value::Array(vec![toml::Value::Integer(1)]));
        grid.insert("base_amount".into(), toml::Value::Array(vec![toml::Value::Float(1000.0)]));
        grid.insert("k".into(), toml::Value::Array(vec![toml::Value::Float(1.0)]));
        grid.insert("ma_window".into(),
                    toml::Value::Array(values.into_iter().map(toml::Value::Integer).collect()));
        OptimizeCfg {
            strategy: "smart_dca".into(),
            metric: "total_return".into(),
            top_n: 5,
            grid,
            rules: Default::default(),
        }
    }

    fn fee() -> FeeModel {
        FeeModel { buy_rate: 0.0, sell_tiers: vec![SellTier { max_days: 0, rate: 0.0 }] }
    }

    /// 寻优必须在训练段选参数、在检验段实测 —— 而不是在同一段上既选又报。
    #[test]
    fn splits_train_and_test_instead_of_reporting_in_sample_argmax() {
        let r = run_optimize(&cfg(vec![10, 20, 30]), "161725", &pts(400), fee(), 0.0).unwrap();

        assert_eq!(r.split_ratio, Some(SPLIT_RATIO), "数据充足时必须切分");
        assert_eq!(r.combos, 3);
        assert!(r.ranked.iter().all(|o| o.oos.is_some()), "每个组合都要有检验段实测");

        // 检验段绝不能与训练段是同一段数据（否则切分形同虚设）
        let best = &r.ranked[0];
        let tr_days = best.outcome.daily.len();
        let te_days = best.oos.as_ref().unwrap().daily.len();
        assert!(tr_days > te_days && te_days > 0, "训练 {tr_days} 天 / 检验 {te_days} 天");
        assert!((tr_days + te_days).abs_diff(400) <= 1, "两段合起来应覆盖全样本");
    }

    /// 警示是这个 tab 的核心产出之一 —— 它天然会被当成"可用的最优参数"，不警示就是误导。
    #[test]
    fn always_carries_an_overfit_caveat() {
        let r = run_optimize(&cfg(vec![10, 20, 30]), "161725", &pts(400), fee(), 0.0).unwrap();
        assert!(!r.caveat.is_empty());
        assert!(r.caveat.contains("训练段"), "须说明参数是在训练段挑的");
        assert!(r.caveat.contains("检验段"), "须指引用户看检验段");
    }

    /// 组合数多 → 多重比较问题，必须额外点名。
    #[test]
    fn warns_louder_when_the_grid_is_large() {
        let many: Vec<i64> = (5..=60).collect();      // 56 个组合
        let r = run_optimize(&cfg(many), "161725", &pts(400), fee(), 0.0).unwrap();
        assert!(r.combos > 50);
        assert!(r.caveat.contains("多重比较"), "大网格须点名多重比较问题");
    }

    /// 数据不足以切分时，不许假装无事发生 —— 必须明说这批数字全是样本内。
    #[test]
    fn admits_when_results_are_pure_in_sample() {
        let r = run_optimize(&cfg(vec![10, 20]), "161725", &pts(100), fee(), 0.0).unwrap();
        assert_eq!(r.split_ratio, None);
        assert!(r.ranked.iter().all(|o| o.oos.is_none()));
        assert!(r.caveat.contains("in-sample") || r.caveat.contains("样本内"),
                "须明说是样本内: {}", r.caveat);
        assert!(r.caveat.contains("数据窥探") || r.caveat.contains("不可外推"),
                "须说清后果: {}", r.caveat);
    }
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

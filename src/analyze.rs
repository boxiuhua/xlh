use crate::data::NavPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Regime { Uptrend, Downtrend, Range }

#[derive(Debug, Clone, serde::Serialize)]
pub struct RegimeReport {
    pub regime: String,
    pub window: usize,
    pub window_return: f64,
    pub annualized_vol: f64,
    pub ma_short: f64,
    pub ma_long: f64,
    pub ma_relation: String,
    pub rec_strategy: String,
    pub rec_name: String,
    pub rationale: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<ActionPlan>,
}

/// 单条触发线及其对应操作（累计净值阈值 + 等价单位净值 + 文案）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct Tier {
    pub label: String,
    /// 触发累计净值（波动带口径）。
    pub nav: f64,
    /// 等价单位净值 = 累计净值 − 分红累计偏移（窗口内无分红时近似恒定）。
    pub unit_nav: f64,
    pub action: String,
}

/// 当下判读：最新净值落在哪一档、该做什么、距下一档还差多少。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CurrentRead {
    /// 当前累计净值。
    pub nav: f64,
    /// 当前单位净值（基金App显示口径）。
    pub unit_nav: f64,
    pub date: String,
    pub z: f64,
    pub signal: String,
    pub action: String,
    pub next_hint: String,
}

/// 高抛低吸行动计划：均线 ±k·σ 波动带 + 分档仓位 + 当下指引 + 形态修正。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ActionPlan {
    pub ma: f64,
    pub sigma: f64,
    pub band_window: usize,
    pub buy_strong: f64,
    pub buy: f64,
    pub sell: f64,
    pub sell_strong: f64,
    pub tiers: Vec<Tier>,
    pub current: CurrentRead,
    pub buy_hits: usize,
    pub sell_hits: usize,
    pub caveat: String,
}

/// 行动计划参数：波动带窗口、买入基准金额、卖出基准比例。
pub struct PlanParams {
    pub band_window: usize,
    pub base_amount: f64,
    pub sell_pct: f64,
}

impl Default for PlanParams {
    fn default() -> Self {
        Self { band_window: 60, base_amount: 1000.0, sell_pct: 0.20 }
    }
}

pub struct RegimeParams {
    pub window: usize,
    pub up_threshold: f64,
    pub down_threshold: f64,
    pub ma_short: usize,
    pub ma_long: usize,
}

impl Default for RegimeParams {
    fn default() -> Self {
        Self { window: 120, up_threshold: 0.10, down_threshold: -0.10, ma_short: 20, ma_long: 60 }
    }
}

fn mean_tail(points: &[NavPoint], n: usize) -> f64 {
    let s = &points[points.len() - n..];
    s.iter().map(|p| p.acc_nav).sum::<f64>() / n as f64
}

fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 { return 0.0; }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// 基于 acc_nav 判定近 window 个交易日的行情形态并给策略建议。
pub fn detect_regime(points: &[NavPoint], p: &RegimeParams) -> anyhow::Result<RegimeReport> {
    let need = p.window.max(p.ma_long + 1);
    if points.len() < need {
        return Err(anyhow::anyhow!("数据不足: 需要至少 {} 个净值点，当前 {}", need, points.len()));
    }
    let w = &points[points.len() - p.window..];
    let window_return = w.last().unwrap().acc_nav / w.first().unwrap().acc_nav - 1.0;
    let ma_short = mean_tail(points, p.ma_short);
    let ma_long = mean_tail(points, p.ma_long);
    let eps = 0.005;
    let ma_relation = if ma_short > ma_long * (1.0 + eps) { "多头排列" }
        else if ma_short < ma_long * (1.0 - eps) { "空头排列" }
        else { "纠缠" };
    let mut rets = Vec::new();
    for win in w.windows(2) {
        if win[0].acc_nav > 0.0 { rets.push(win[1].acc_nav / win[0].acc_nav - 1.0); }
    }
    let annualized_vol = stdev(&rets) * (252_f64).sqrt();
    let regime = if window_return > p.up_threshold && ma_short > ma_long {
        Regime::Uptrend
    } else if window_return < p.down_threshold && ma_short < ma_long {
        Regime::Downtrend
    } else {
        Regime::Range
    };
    let (regime_cn, rec_strategy, rec_name, rationale) = match regime {
        Regime::Uptrend => ("上涨趋势", "smart_dca", "智能定投", "上涨趋势：顺势持有，频繁进出易踏空"),
        Regime::Downtrend => ("下跌趋势", "trend", "均线择时", "下跌趋势：趋势走坏空仓离场，避开长跌"),
        Regime::Range => ("震荡", "rsi", "RSI超买超卖", "震荡：区间内高抛低吸吃波动"),
    };
    Ok(RegimeReport {
        regime: regime_cn.to_string(),
        window: p.window,
        window_return,
        annualized_vol,
        ma_short,
        ma_long,
        ma_relation: ma_relation.to_string(),
        rec_strategy: rec_strategy.to_string(),
        rec_name: rec_name.to_string(),
        rationale: rationale.to_string(),
        plan: None,
    })
}

/// 由历史净值与已判定的行情形态，构建高抛低吸行动计划。纯函数，不做 IO。
pub fn build_action_plan(
    points: &[NavPoint],
    report: &RegimeReport,
    p: &PlanParams,
) -> anyhow::Result<ActionPlan> {
    let n = p.band_window;
    if points.len() < n {
        return Err(anyhow::anyhow!("数据不足: 行动计划需要至少 {} 个净值点，当前 {}", n, points.len()));
    }
    let slice = &points[points.len() - n..];
    let navs: Vec<f64> = slice.iter().map(|q| q.acc_nav).collect();
    let ma = navs.iter().sum::<f64>() / n as f64;
    let sigma = stdev(&navs);
    if sigma <= 0.0 {
        return Err(anyhow::anyhow!("波动为零，无法构建波动带（窗口内净值长期不变）"));
    }
    let buy_strong = ma - 2.0 * sigma;
    let buy = ma - sigma;
    let sell = ma + sigma;
    let sell_strong = ma + 2.0 * sigma;

    let base = p.base_amount;
    let pct = p.sell_pct;
    let buy_amt = |x: f64| format!("买入 {:.0} 元", x);
    let sell_amt = |q: f64| format!("卖出持仓 {:.0}%", q * 100.0);

    let last = points.last().unwrap();
    let nav = last.acc_nav;       // 累计净值（波动带口径）
    let unit = last.nav;          // 单位净值（基金App显示口径）
    let d_offset = nav - unit;    // 分红累计偏移，窗口内无分红时近似恒定
    let unit_of = |acc: f64| acc - d_offset;   // 累计净值 → 等价单位净值

    let tier = |label: &str, acc: f64, action: String| Tier {
        label: label.into(), nav: acc, unit_nav: unit_of(acc), action,
    };
    let tiers = vec![
        tier("强力低吸", buy_strong, buy_amt(base * 2.0)),
        tier("低吸",     buy,         buy_amt(base)),
        tier("观望",     ma,          "持有不动".into()),
        tier("高抛",     sell,        sell_amt(pct)),
        tier("强力高抛", sell_strong, sell_amt(pct * 2.0)),
    ];

    let z = (nav - ma) / sigma;
    let (signal, mut action) = if z <= -2.0 {
        ("强力低吸", buy_amt(base * 2.0))
    } else if z <= -1.0 {
        ("低吸", buy_amt(base))
    } else if z < 1.0 {
        ("观望", "持有不动".to_string())
    } else if z < 2.0 {
        ("高抛", sell_amt(pct))
    } else {
        ("强力高抛", sell_amt(pct * 2.0))
    };

    // 形态修正：上涨趋势削弱减仓力度，下跌趋势谨慎抄底。
    match report.regime.as_str() {
        "上涨趋势" => match signal {
            "高抛" => action = "趋势向上，建议持有观望（暂不减仓，待 +2σ 再减）".to_string(),
            "强力高抛" => action = sell_amt(pct), // 2× 减半为 1×
            _ => {}
        },
        "下跌趋势" => {
            if signal == "低吸" {
                action = "趋势向下，暂不抄底（仅 -2σ 小额试探）".to_string();
            }
        }
        _ => {}
    }

    // 百分比按单位净值（真实价格）计算，与基金App涨跌口径一致
    let pct_to = |acc_target: f64| (unit_of(acc_target) / unit - 1.0) * 100.0;
    let next_hint = if z < 1.0 {
        format!("距低吸线 累计{:.4}/单位{:.4} 需 {:+.1}%，距高抛线 累计{:.4}/单位{:.4} 需 {:+.1}%",
            buy, unit_of(buy), pct_to(buy), sell, unit_of(sell), pct_to(sell))
    } else {
        format!("距强力高抛线 累计{:.4}/单位{:.4} 需 {:+.1}%",
            sell_strong, unit_of(sell_strong), pct_to(sell_strong))
    };

    // 结合历史：窗口内净值下穿低吸线/上穿高抛线的次数。
    let mut buy_hits = 0usize;
    let mut sell_hits = 0usize;
    for w in slice.windows(2) {
        let (a, b) = (w[0].acc_nav, w[1].acc_nav);
        if a > buy && b <= buy { buy_hits += 1; }
        if a < sell && b >= sell { sell_hits += 1; }
    }

    let caveat = match report.regime.as_str() {
        "上涨趋势" => "上涨趋势：顺势持有，低吸照做、高抛减半执行，勿过早下车。",
        "下跌趋势" => "下跌趋势：高抛照做、低吸只在 -2σ 小额试探，注意下行风险。",
        _ => "震荡：区间内高抛低吸吃波动，按触发线分档执行。",
    }.to_string();

    Ok(ActionPlan {
        ma, sigma, band_window: n,
        buy_strong, buy, sell, sell_strong,
        tiers,
        current: CurrentRead {
            nav, unit_nav: unit, date: last.date.to_string(), z,
            signal: signal.to_string(), action, next_hint,
        },
        buy_hits, sell_hits, caveat,
    })
}

/// 一次性给出形态判定 + 行动计划（web 层使用）。
pub fn detect_regime_with_plan(
    points: &[NavPoint],
    rp: &RegimeParams,
    pp: &PlanParams,
) -> anyhow::Result<RegimeReport> {
    let mut report = detect_regime(points, rp)?;
    let plan = build_action_plan(points, &report, pp)?;
    report.plan = Some(plan);
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    #[test]
    fn detects_uptrend() {
        // 130 点从 1.0 线性涨到 2.0
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "上涨趋势");
        assert_eq!(r.rec_strategy, "smart_dca");
        assert!(r.window_return > 0.10);
    }

    #[test]
    fn detects_downtrend() {
        let vals: Vec<f64> = (0..130).map(|i| 2.0 - i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "下跌趋势");
        assert_eq!(r.rec_strategy, "trend");
        assert!(r.window_return < -0.10);
    }

    #[test]
    fn detects_range() {
        // 在 1.00/1.01 间小幅交替 → 收益≈0、均线纠缠
        let vals: Vec<f64> = (0..130).map(|i| if i % 2 == 0 { 1.00 } else { 1.01 }).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "震荡");
        assert_eq!(r.rec_strategy, "rsi");
    }

    #[test]
    fn insufficient_data_errors() {
        let vals: Vec<f64> = (0..30).map(|_| 1.0).collect();
        let err = detect_regime(&series(&vals), &RegimeParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn volatility_positive_for_moving_series() {
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert!(r.annualized_vol > 0.0, "上涨序列应有正波动率");
    }

    /// 构造仅 regime 字段不同的最小 RegimeReport，用于隔离测试形态修正。
    fn report_with(regime: &str) -> RegimeReport {
        RegimeReport {
            regime: regime.to_string(),
            window: 60, window_return: 0.0, annualized_vol: 0.0,
            ma_short: 1.0, ma_long: 1.0, ma_relation: "纠缠".into(),
            rec_strategy: "rsi".into(), rec_name: "RSI超买超卖".into(),
            rationale: "".into(), plan: None,
        }
    }

    #[test]
    fn plan_deep_below_is_strong_buy() {
        // 59 点在 1.0 附近小幅波动，最后一点深跌到 0.5 → 远低于 MA-2σ
        let mut vals: Vec<f64> = (0..59).map(|i| if i % 2 == 0 { 0.99 } else { 1.01 }).collect();
        vals.push(0.5);
        let plan = build_action_plan(&series(&vals), &report_with("震荡"), &PlanParams::default()).unwrap();
        assert_eq!(plan.current.signal, "强力低吸", "深跌应判强力低吸: z={}", plan.current.z);
        assert!(plan.current.action.contains("2000"), "应买入 2× 基准=2000: {}", plan.current.action);
    }

    #[test]
    fn plan_high_above_is_sell() {
        let mut vals: Vec<f64> = (0..59).map(|i| if i % 2 == 0 { 0.99 } else { 1.01 }).collect();
        vals.push(2.0);
        let plan = build_action_plan(&series(&vals), &report_with("震荡"), &PlanParams::default()).unwrap();
        assert!(plan.current.signal.contains("高抛"), "高位应判高抛: {}", plan.current.signal);
        assert!(plan.current.action.contains("卖"), "应给出卖出动作: {}", plan.current.action);
    }

    #[test]
    fn plan_counts_history_triggers() {
        // 围绕 1.0 的较大幅振荡 → 反复穿越 ±1σ 线
        let vals: Vec<f64> = (0..60).map(|i| 1.0 + 0.1 * ((i as f64) * 0.7).sin()).collect();
        let plan = build_action_plan(&series(&vals), &report_with("震荡"), &PlanParams::default()).unwrap();
        assert!(plan.buy_hits > 0, "振荡序列应有低吸触发计数");
        assert!(plan.sell_hits > 0, "振荡序列应有高抛触发计数");
    }

    #[test]
    fn plan_caveat_varies_by_regime() {
        let vals: Vec<f64> = (0..60).map(|i| 1.0 + 0.05 * ((i as f64) * 0.5).sin()).collect();
        let pts = series(&vals);
        let up = build_action_plan(&pts, &report_with("上涨趋势"), &PlanParams::default()).unwrap();
        let down = build_action_plan(&pts, &report_with("下跌趋势"), &PlanParams::default()).unwrap();
        let range = build_action_plan(&pts, &report_with("震荡"), &PlanParams::default()).unwrap();
        assert!(up.caveat.contains("顺势") || up.caveat.contains("上涨"), "上涨提示: {}", up.caveat);
        assert!(down.caveat.contains("风险") || down.caveat.contains("下跌"), "下跌提示: {}", down.caveat);
        assert!(range.caveat.contains("震荡") || range.caveat.contains("高抛低吸"), "震荡提示: {}", range.caveat);
    }

    /// 单位净值与累计净值不同的序列（恒定分红偏移 d）。
    fn series_split(acc: &[f64], d: f64) -> Vec<NavPoint> {
        acc.iter().enumerate().map(|(i, a)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *a - d, acc_nav: *a,
        }).collect()
    }

    #[test]
    fn plan_reports_unit_nav_equivalents() {
        // 累计净值在 2.2 附近波动；单位净值 = 累计 − 1.5（恒定分红偏移）
        let acc: Vec<f64> = (0..60).map(|i| 2.2 + 0.02 * ((i as f64) * 0.5).sin()).collect();
        let pts = series_split(&acc, 1.5);
        let plan = build_action_plan(&pts, &report_with("震荡"), &PlanParams::default()).unwrap();
        // 当前单位净值 = 最后一点的 nav，且≠累计净值
        assert!((plan.current.unit_nav - pts.last().unwrap().nav).abs() < 1e-9);
        assert!((plan.current.nav - plan.current.unit_nav - 1.5).abs() < 1e-6, "累计应比单位高 1.5");
        // 每条触发线的等价单位净值 = 累计净值 − 1.5
        for t in &plan.tiers {
            assert!((t.unit_nav - (t.nav - 1.5)).abs() < 1e-6, "{} 等价单位净值应为累计−1.5", t.label);
        }
    }

    #[test]
    fn detect_with_plan_serializes_frontend_keys() {
        // 130 点温和上涨，足够 detect_regime 与波动带窗口
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + 0.002 * i as f64).collect();
        let r = detect_regime_with_plan(&series(&vals), &RegimeParams::default(), &PlanParams::default()).unwrap();
        let j = serde_json::to_string(&r).unwrap();
        for key in ["\"plan\"", "\"tiers\"", "\"current\"", "\"buy_strong\"", "\"sell_strong\"", "\"caveat\"", "\"signal\""] {
            assert!(j.contains(key), "JSON 应含 {key}: {j}");
        }
    }

    #[test]
    fn plan_uptrend_softens_sell_in_high_zone() {
        // last 落在 +1σ~+2σ 高抛区：震荡下应卖出，上涨趋势下应改为持有观望
        let mut vals: Vec<f64> = (0..59).map(|i| if i % 2 == 0 { 0.97 } else { 1.03 }).collect();
        vals.push(1.05);
        let pts = series(&vals);
        let range = build_action_plan(&pts, &report_with("震荡"), &PlanParams::default()).unwrap();
        let up = build_action_plan(&pts, &report_with("上涨趋势"), &PlanParams::default()).unwrap();
        assert_eq!(range.current.signal, "高抛", "震荡高抛区: z={}", range.current.z);
        assert!(range.current.action.contains("卖"), "震荡应卖出: {}", range.current.action);
        assert_eq!(up.current.signal, "高抛", "上涨同样在高抛区: z={}", up.current.z);
        assert!(up.current.action.contains("持有"), "上涨趋势下高抛区应改持有: {}", up.current.action);
    }
}

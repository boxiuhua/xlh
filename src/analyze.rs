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

/// 触发线的历史前瞻检验。
///
/// ## 为什么需要它
///
/// 「低吸线 1.2345 · 买入 1000 元」是一条**精确到 4 位小数的价格点位 + 具体下单指令**。
/// 而此前展示给用户的唯一"证据"是**穿越次数**（"窗口内触发：低吸 12 次"）——
/// 这个数字由构造决定必然会发生若干次，它**完全不告诉你按这条线操作是否赚钱**。
///
/// 更糟的是那个计数本身也有未来函数：它拿**最终的**波动带（由最后 60 天算出）
/// 去回扫整个窗口，而当时根本不可能知道这条线在哪。
///
/// 这里改成正经的事件研究：**滚动**计算波动带（每个时点只用它之前的数据），
/// 记录每次触发，统计其后 `horizon_days` 的前瞻收益，并与"随便哪天买入持有同样长度"
/// 的无条件基准对比。**低吸信号若跑不赢基准，它就没有价值** —— 这个结论必须让用户看到。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TriggerEvidence {
    /// 前瞻窗口（交易日）
    pub horizon_days: usize,
    /// 可用于检验的样本量（需 band_window + horizon 之后才有第一个可检验点）
    pub sample_days: usize,

    pub buy_signals: usize,
    /// 低吸触发后 horizon 内上涨的比例
    pub buy_win_rate: Option<f64>,
    /// 低吸触发后 horizon 的平均收益 %
    pub buy_mean_forward: Option<f64>,

    pub sell_signals: usize,
    /// 高抛触发后 horizon 内**下跌**的比例（跌了才算"躲对了"）
    pub sell_win_rate: Option<f64>,
    pub sell_mean_forward: Option<f64>,

    /// 无条件基准：任意一天买入、持有 horizon 的平均收益 %。
    /// 低吸信号的前瞻收益必须显著高于它，这条线才算有用。
    pub baseline_mean_forward: Option<f64>,
    pub baseline_win_rate: Option<f64>,

    /// 人话结论。证据不支持时必须直说。
    pub verdict: String,
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
    /// 这条线到底有没有用 —— 数据不足时为 `None`。
    pub evidence: Option<TriggerEvidence>,
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

/// 前瞻检验窗口：20 个交易日 ≈ 1 个月。
const EVIDENCE_HORIZON: usize = 20;

/// 对触发线做无未来函数的事件研究。
///
/// 在每个时点 t，波动带**只用 t 之前的 `band_window` 个点**计算 —— 这正是当时真能拿到的信息。
/// 若 nav[t] 下穿低吸线，就记一次低吸触发，并测 t → t+horizon 的收益。高抛同理。
/// 同时统计无条件基准（每一个可检验的 t 都算一次），用来回答那个真正的问题：
/// **这条线挑出来的时点，比随便挑一天更好吗？**
pub fn evaluate_triggers(points: &[NavPoint], band_window: usize, horizon: usize) -> Option<TriggerEvidence> {
    // 需要 band_window 个点建带 + 至少一个可测的前瞻窗口
    if points.len() < band_window + horizon + 2 { return None; }

    let navs: Vec<f64> = points.iter().map(|p| p.acc_nav).collect();
    let fwd = |t: usize| -> Option<f64> {
        let (a, b) = (navs[t], *navs.get(t + horizon)?);
        if a > 0.0 { Some((b / a - 1.0) * 100.0) } else { None }
    };

    let (mut buy_f, mut sell_f, mut base_f) = (Vec::new(), Vec::new(), Vec::new());

    // t 从 band_window 起（前面不足以建带），到 len-horizon-1 止（再往后测不到前瞻收益）
    for t in band_window..navs.len().saturating_sub(horizon) {
        let win = &navs[t - band_window..t];          // 严格是 t 之前的数据，不含 t
        let ma = win.iter().sum::<f64>() / band_window as f64;
        let sigma = stdev(win);
        if sigma <= 0.0 { continue; }

        let Some(r) = fwd(t) else { continue };
        base_f.push(r);                                // 无条件基准：每个可检验点都计入

        let (prev, cur) = (navs[t - 1], navs[t]);
        let (buy_line, sell_line) = (ma - sigma, ma + sigma);
        // 下穿低吸线 / 上穿高抛线（穿越事件，不是"停留在线下"）
        if prev > buy_line && cur <= buy_line { buy_f.push(r); }
        if prev < sell_line && cur >= sell_line { sell_f.push(r); }
    }

    if base_f.is_empty() { return None; }

    let mean = |v: &[f64]| if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) };
    let win_up = |v: &[f64]| if v.is_empty() { None } else {
        Some(v.iter().filter(|x| **x > 0.0).count() as f64 / v.len() as f64 * 100.0)
    };
    let win_down = |v: &[f64]| if v.is_empty() { None } else {
        Some(v.iter().filter(|x| **x < 0.0).count() as f64 / v.len() as f64 * 100.0)
    };

    let buy_mean = mean(&buy_f);
    let base_mean = mean(&base_f);
    let verdict = build_verdict(buy_f.len(), buy_mean, sell_f.len(), mean(&sell_f), base_mean, horizon);

    Some(TriggerEvidence {
        horizon_days: horizon,
        sample_days: base_f.len(),
        buy_signals: buy_f.len(),
        buy_win_rate: win_up(&buy_f),
        buy_mean_forward: buy_mean,
        sell_signals: sell_f.len(),
        sell_win_rate: win_down(&sell_f),   // 高抛后下跌才算"躲对了"
        sell_mean_forward: mean(&sell_f),
        baseline_mean_forward: base_mean,
        baseline_win_rate: win_up(&base_f),
        verdict,
    })
}

/// 结论措辞。证据不支持这条线时必须直说 —— 用户正拿着它下单。
fn build_verdict(
    n_buy: usize, buy_mean: Option<f64>,
    n_sell: usize, sell_mean: Option<f64>,
    base_mean: Option<f64>, horizon: usize,
) -> String {
    let Some(base) = base_mean else { return "样本不足，无法检验这条线是否有效。".into() };

    // 样本太少时任何结论都是噪声，不许硬下判断
    const MIN_SIGNALS: usize = 10;
    let mut parts = vec![format!(
        "基准：任意一天买入、持有 {horizon} 个交易日，平均收益 {base:+.2}%。"
    )];

    match (buy_mean, n_buy >= MIN_SIGNALS) {
        (Some(b), true) => {
            let edge = b - base;
            parts.push(format!(
                "低吸线触发 {n_buy} 次，其后 {horizon} 日平均 {b:+.2}%，\
                 相对基准的超额为 {edge:+.2}%。"));
            if edge <= 0.0 {
                parts.push("这条低吸线没有跑赢「随便哪天买」—— 按现有历史，它不提供择时价值。".into());
            } else if edge < 0.5 {
                parts.push("超额很小，与噪声难以区分；不宜据此加大仓位。".into());
            } else {
                parts.push("历史上有正超额，但这是单只基金的样本内统计，未经样本外检验，不保证延续。".into());
            }
        }
        (Some(b), false) => parts.push(format!(
            "低吸线仅触发 {n_buy} 次（其后平均 {b:+.2}%），样本量不足 {MIN_SIGNALS} 次，\
             无法据此判断这条线是否有效。")),
        _ => parts.push("低吸线在历史上从未触发，无从检验。".into()),
    }

    match (sell_mean, n_sell >= MIN_SIGNALS) {
        (Some(s), true) => {
            parts.push(format!(
                "高抛线触发 {n_sell} 次，其后 {horizon} 日平均 {s:+.2}%（为负才说明躲对了下跌）。"));
            if s > 0.0 {
                parts.push("高抛线之后平均还在涨 —— 按现有历史，照它减仓会错过后续上涨。".into());
            }
        }
        (Some(_), false) => parts.push(format!("高抛线仅触发 {n_sell} 次，样本不足以判断。")),
        _ => {}
    }

    parts.push("以上为该基金自身历史的统计，非预测；这条线的窗口(60日)与阈值(±1σ/±2σ)是经验取值，未经寻优验证。".into());
    parts.join(" ")
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
        "下跌趋势" if signal == "低吸" => {
            action = "趋势向下，暂不抄底（仅 -2σ 小额试探）".to_string();
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

    // 窗口内净值下穿低吸线/上穿高抛线的次数。
    //
    // ⚠ 注意这个计数本身带未来函数：它拿**最终的**波动带（由最后 60 天算出）回扫整个窗口，
    // 而当时根本不可能知道这条线画在哪。它只能当作"这条线大致多久碰一次"的粗略描述，
    // **不能当作这条线有效的证据** —— 真正的证据在 `evidence`（滚动重算、无未来函数、
    // 并与"随便哪天买"的基准对比）。
    let mut buy_hits = 0usize;
    let mut sell_hits = 0usize;
    for w in slice.windows(2) {
        let (a, b) = (w[0].acc_nav, w[1].acc_nav);
        if a > buy && b <= buy { buy_hits += 1; }
        if a < sell && b >= sell { sell_hits += 1; }
    }

    // 这条线到底有没有用 —— 用**全部**历史做无未来函数的事件研究（不只是最后 60 天）
    let evidence = evaluate_triggers(points, n, EVIDENCE_HORIZON);

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
        buy_hits, sell_hits, evidence, caveat,
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
mod evidence_tests {
    use super::*;
    use chrono::NaiveDate;

    fn pts(navs: &[f64]) -> Vec<NavPoint> {
        navs.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    /// 纯随机游走：低吸线不该有任何超额。证据必须**如实报告它没用**。
    #[test]
    fn reports_no_edge_when_the_line_has_none() {
        // 确定性伪随机（不用 rand，保证可复现）
        let mut x = 12345u64;
        let mut nav = 1.0;
        let navs: Vec<f64> = (0..800).map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((x >> 33) as f64 / (1u64 << 31) as f64) - 1.0;   // ≈ [-1,1)
            nav *= 1.0 + u * 0.01;
            nav
        }).collect();

        let e = evaluate_triggers(&pts(&navs), 60, 20).expect("样本足够");
        assert!(e.sample_days > 500);
        assert!(e.baseline_mean_forward.is_some(), "必须给出无条件基准");

        // 有基准可比才是关键 —— 没有基准，"低吸后平均涨 1%" 是句废话
        // （因为随便哪天买可能也涨 1%）
        if e.buy_signals >= 10 {
            let edge = e.buy_mean_forward.unwrap() - e.baseline_mean_forward.unwrap();
            // 随机游走上不该有稳定超额；无论正负，verdict 都必须把超额算出来摆明
            assert!(e.verdict.contains("超额") || e.verdict.contains("样本量不足"),
                    "必须给出相对基准的超额: {}", e.verdict);
            assert!(edge.abs() < 5.0, "随机游走不该出现巨大超额: {edge}");
        }
        assert!(e.verdict.contains("基准"), "结论必须提到基准");
    }

    /// 事件研究**绝不能有未来函数**：波动带只能用触发时点之前的数据算。
    #[test]
    fn band_is_recomputed_from_past_data_only() {
        // 前 300 天窄幅震荡，之后突然放大波动。
        // 若用最终（宽）的带回扫前段，前段几乎不会触发；
        // 若逐点用当时的（窄）带，前段会正常触发 —— 以此区分两种实现。
        let mut navs: Vec<f64> = (0..300).map(|i| 1.0 + ((i % 10) as f64 - 4.5) * 0.002).collect();
        navs.extend((0..300).map(|i| 1.0 + ((i % 10) as f64 - 4.5) * 0.05));

        let e = evaluate_triggers(&pts(&navs), 60, 20).expect("样本足够");
        assert!(e.buy_signals > 0,
                "用当时的窄带，前段的小幅回落就该触发；若为 0 说明用了最终的宽带（未来函数）");
    }

    /// 样本太少时不许硬下结论。
    #[test]
    fn refuses_to_conclude_on_thin_samples() {
        let navs: Vec<f64> = (0..100).map(|i| 1.0 + i as f64 * 0.001).collect();
        let e = evaluate_triggers(&pts(&navs), 60, 20);
        // 100 点：60 建带 + 20 前瞻 → 仅约 20 个可检验点，触发次数必然很少
        if let Some(ev) = e {
            if ev.buy_signals < 10 && ev.buy_signals > 0 {
                assert!(ev.verdict.contains("样本量不足") || ev.verdict.contains("样本不足"),
                        "样本少时须明说不能判断: {}", ev.verdict);
            }
        }
    }

    #[test]
    fn none_when_history_too_short() {
        let navs: Vec<f64> = (0..50).map(|i| 1.0 + i as f64 * 0.001).collect();
        assert!(evaluate_triggers(&pts(&navs), 60, 20).is_none(), "不足以建带 → None");
    }

    /// 结论里必须点明这些参数是拍脑袋的，没经过寻优验证。
    #[test]
    fn verdict_admits_the_thresholds_are_arbitrary() {
        let navs: Vec<f64> = (0..800).map(|i| 1.0 + ((i % 40) as f64 - 20.0) * 0.003).collect();
        let e = evaluate_triggers(&pts(&navs), 60, 20).unwrap();
        assert!(e.verdict.contains("经验取值") && e.verdict.contains("未经寻优验证"),
                "须承认 60日/±1σ 是拍脑袋的: {}", e.verdict);
    }
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

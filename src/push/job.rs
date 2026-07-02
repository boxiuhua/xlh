//! 一次推送任务编排：同步 → 建议/诊断 → 组装 → 发送。
use std::collections::{BTreeSet, HashMap};
use anyhow::Result;

use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
use crate::data::{self, cache};
use crate::data::sync::SyncOutcome;
use crate::holdings::{self, HoldingsInput};
use crate::recommend::RecommendParams;

use super::config::PushConfig;
use super::{channels, message};

pub fn run(cfg: &PushConfig) -> Result<()> {
    let cache_dir = cfg.channel.cache_dir.as_path();

    // 1) 同步：holdings ∪ diagnose 去重后逐只同步最新净值。
    let mut codes: BTreeSet<String> = BTreeSet::new();
    for h in &cfg.holdings {
        if !h.code.trim().is_empty() { codes.insert(h.code.trim().to_string()); }
    }
    for d in &cfg.diagnose {
        if !d.trim().is_empty() { codes.insert(d.trim().to_string()); }
    }
    let sync: Vec<SyncOutcome> = codes.iter().map(|c| data::sync::sync_fund(c, cache_dir)).collect();

    // 2) 名称映射（best-effort，失败回退代码）。
    let names: HashMap<String, String> = data::fundlist::load_or_fetch_fund_list(cache_dir)
        .unwrap_or_default().into_iter().map(|f| (f.code, f.name)).collect();
    let name_of = |c: &str| names.get(c).cloned().unwrap_or_else(|| c.to_string());

    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);

    // 3) 持仓建议。
    let input = HoldingsInput {
        total_amount: cfg.portfolio.total_amount,
        total_profit: cfg.portfolio.total_profit,
        cumulative_profit: cfg.portfolio.cumulative_profit,
        holdings: cfg.holdings.clone(),
    };
    let report = holdings::build_report(
        &input, |c| name_of(c), &end.to_string(), &RecommendParams::default(),
        |c| cache::load_or_fetch(c, cache_dir, start, end),
    );

    // 4) 额外诊断（不持有、仅诊断）。失败静默跳过。
    let mut diags: Vec<(String, String, RegimeReport)> = Vec::new();
    for code in &cfg.diagnose {
        if let Ok(points) = cache::load_or_fetch(code, cache_dir, start, end) {
            if let Ok(r) = analyze::detect_regime_with_plan(&points, &RegimeParams::default(), &PlanParams::default()) {
                diags.push((code.clone(), name_of(code), r));
            }
        }
    }

    // 5) 组装并发送。
    let md = message::compose(&report, &diags, &sync);
    channels::send(&cfg.channel, "基金持仓建议", &md)
}

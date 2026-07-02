//! 一次推送任务编排：同步(基金+股票) → 建议/诊断 → 组装 → 发送。
use std::collections::{BTreeSet, HashMap};
use anyhow::Result;

use crate::analyze::{self, PlanParams, RegimeParams, RegimeReport};
use crate::data::{self, cache};
use crate::holdings::{self, HoldingsInput};
use crate::recommend::RecommendParams;
use crate::stock::data::{cache as stock_cache, sync as stock_sync};
use crate::stock::diagnose::{self as stock_diagnose, DiagnoseParams, StockDiagnosis};

use super::config::PushConfig;
use super::message::{self, SyncNote};
use super::stock_advice::{self, StockAdvice};
use super::channels;

fn note_fund(o: &data::sync::SyncOutcome) -> SyncNote {
    SyncNote { code: o.code.clone(), added: o.added, latest: o.latest.clone(), error: o.error.clone() }
}
fn note_stock(o: &stock_sync::SyncOutcome) -> SyncNote {
    SyncNote { code: o.code.clone(), added: o.added, latest: o.latest.clone(), error: o.error.clone() }
}

fn dedup_codes<'a>(iters: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for c in iters {
        let t = c.trim();
        if !t.is_empty() { set.insert(t.to_string()); }
    }
    set.into_iter().collect()
}

/// 组装完整推送消息 + 是否有新数据（供 only_on_new_data 判定）。不发送。
pub fn build_message(cfg: &PushConfig) -> Result<(String, bool)> {
    let cache_dir = cfg.channel.cache_dir.as_path();
    let stock_dir = cfg.channel.cache_dir.join("stock");
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);

    // ---- 同步（基金 + 股票）----
    let fund_codes = dedup_codes(cfg.holdings.iter().map(|h| &h.code).chain(cfg.diagnose.iter()));
    let fund_sync: Vec<data::sync::SyncOutcome> = fund_codes.iter().map(|c| data::sync::sync_fund(c, cache_dir)).collect();

    let stock_codes = dedup_codes(cfg.stocks.iter().map(|h| &h.code).chain(cfg.diagnose_stocks.iter()));
    let stock_sync_out: Vec<stock_sync::SyncOutcome> = stock_codes.iter().map(|c| stock_sync::sync_stock(c, &stock_dir)).collect();

    let has_new = fund_sync.iter().any(|o| o.added > 0) || stock_sync_out.iter().any(|o| o.added > 0);

    // ---- 基金名称映射（best-effort）----
    let names: HashMap<String, String> = data::fundlist::load_or_fetch_fund_list(cache_dir)
        .unwrap_or_default().into_iter().map(|f| (f.code, f.name)).collect();
    let name_of = |c: &str| names.get(c).cloned().unwrap_or_else(|| c.to_string());

    // ---- 基金持仓建议 ----
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

    // ---- 基金诊断 ----
    let mut fund_diags: Vec<(String, String, RegimeReport)> = Vec::new();
    for code in &cfg.diagnose {
        if let Ok(points) = cache::load_or_fetch(code, cache_dir, start, end) {
            if let Ok(r) = analyze::detect_regime_with_plan(&points, &RegimeParams::default(), &PlanParams::default()) {
                fund_diags.push((code.clone(), name_of(code), r));
            }
        }
    }

    // ---- 股票持仓建议 + 诊断 ----
    let dp = DiagnoseParams::default();
    let mut stock_adv: Vec<StockAdvice> = Vec::new();
    for h in &cfg.stocks {
        if h.code.trim().is_empty() { continue; }
        if let Ok(bars) = stock_cache::load_or_fetch(&h.code, &stock_dir, start, end) {
            if let Ok(diag) = stock_diagnose::diagnose(h.code.clone(), h.code.clone(), &bars, &dp) {
                stock_adv.push(stock_advice::advise(h, &diag));
            }
        }
    }
    let mut stock_diags: Vec<StockDiagnosis> = Vec::new();
    for code in &cfg.diagnose_stocks {
        if code.trim().is_empty() { continue; }
        if let Ok(bars) = stock_cache::load_or_fetch(code, &stock_dir, start, end) {
            if let Ok(diag) = stock_diagnose::diagnose(code.clone(), code.clone(), &bars, &dp) {
                stock_diags.push(diag);
            }
        }
    }

    // ---- 同步简报 ----
    let mut sync: Vec<SyncNote> = fund_sync.iter().map(note_fund).collect();
    sync.extend(stock_sync_out.iter().map(note_stock));

    let md = message::compose(&report, &fund_diags, &stock_adv, &stock_diags, &sync);
    Ok((md, has_new))
}

/// 跑一次：组装 → （only_on_new_data 且无新数据则跳过）→ 发送。
pub fn run(cfg: &PushConfig) -> Result<()> {
    let (md, has_new) = build_message(cfg)?;
    if cfg.schedule.only_on_new_data && !has_new {
        println!("无新数据，跳过推送");
        return Ok(());
    }
    channels::send(&cfg.channel, "基金持仓建议", &md)
}

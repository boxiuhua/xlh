//! 一次推送任务编排：同步(基金+股票) → 建议/诊断 → 组装 → 发送。
use std::collections::{BTreeSet, HashMap};
use anyhow::Result;
use rusqlite::Connection;

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

/// build_message 的完整产物，含基金持仓建议的输入与报告（供历史保存复用）。
pub struct BuiltMessage {
    pub md: String,
    pub has_new: bool,
    pub fund_input: HoldingsInput,
    pub fund_report: crate::holdings::HoldingsReport,
}

/// 组装完整推送消息并保留基金持仓输入/报告。
pub fn build_message_full(cfg: &PushConfig) -> Result<BuiltMessage> {
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
    Ok(BuiltMessage { md, has_new, fund_input: input, fund_report: report })
}

/// 兼容既有调用点：只取 markdown 与 has_new。
pub fn build_message(cfg: &PushConfig) -> Result<(String, bool)> {
    let b = build_message_full(cfg)?;
    Ok((b.md, b.has_new))
}

/// 把本次基金持仓建议存入历史（source=push, user_id=None）；失败仅告警。
fn save_push_history(hist: Option<&Connection>, b: &BuiltMessage) {
    let Some(conn) = hist else { return };
    if b.fund_report.advices.is_empty() { return; }
    let summary = crate::holdings::summarize(&b.fund_report);
    match serde_json::to_string(&serde_json::json!({ "input": &b.fund_input, "report": &b.fund_report })) {
        Ok(payload) => {
            if let Err(e) = crate::history::save(conn, None, "push", &summary, &payload) {
                eprintln!("保存推送历史失败：{e}");
            }
        }
        Err(e) => eprintln!("序列化推送历史失败：{e}"),
    }
}

/// 定时守护跑一次：组装 → （only_on_new_data 且无新数据则跳过）→ 发送。
/// only_on_new_data 只约束定时推送（规避周末/节假日空推）。
pub fn run(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    let b = build_message_full(cfg)?;
    if cfg.schedule.only_on_new_data && !b.has_new {
        println!("无新数据，跳过推送");
        return Ok(());
    }
    save_push_history(hist, &b);
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}

/// 手动「立即推送」/ CLI --once：无条件组装并发送，忽略 only_on_new_data。
/// 手动触发即明确的发送意图，不应被「无新数据」拦截。
pub fn run_forced(cfg: &PushConfig, hist: Option<&Connection>) -> Result<()> {
    let b = build_message_full(cfg)?;
    save_push_history(hist, &b);
    channels::send(&cfg.channel, "基金持仓建议", &b.md)
}

#[cfg(test)]
mod push_history_tests {
    use super::*;
    use crate::holdings::{HoldingsInput, HoldingsReport, PortfolioSummary};

    fn empty_built() -> BuiltMessage {
        BuiltMessage {
            md: String::new(),
            has_new: false,
            fund_input: HoldingsInput { total_amount: None, total_profit: None, cumulative_profit: None, holdings: vec![] },
            fund_report: HoldingsReport {
                generated: "2026-07-05".into(),
                summary: PortfolioSummary {
                    total_amount: 0.0, total_profit: None, cumulative_profit: None,
                    holding_count: 0, total_add: 0.0, total_trim: 0.0, concentration_note: String::new(),
                },
                advices: vec![], skipped: vec![], disclaimer: String::new(),
            },
        }
    }

    #[test]
    fn empty_advices_are_not_saved() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        save_push_history(Some(&conn), &empty_built());
        assert_eq!(crate::history::list_push(&conn, 100).unwrap().len(), 0);
    }

    #[test]
    fn none_conn_is_noop() {
        // 不 panic 即可
        save_push_history(None, &empty_built());
    }
}

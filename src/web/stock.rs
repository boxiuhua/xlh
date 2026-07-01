//! 股票 Web 接口：搜索/诊断/回测/选股/同步。web 作为组合根接入 stock 能力。
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;

use axum::extract::Query;
use axum::response::Json;

use super::{AppError, StrategyFields, build_strategy_from_fields};
use crate::stock::data::{self, cache, search, sync};
use crate::stock::fee::StockFee;
use crate::stock::diagnose::{self, DiagnoseParams, StockDiagnosis};
use crate::stock::backtest::{self, StockRunOutcome};
use crate::stock::recommend::{self, RecommendParams, StockRecommendReport};

fn stock_cache() -> &'static Path { Path::new(".cache/stock") }

/// 预设股票池（A股/港股/美股混合），可增删。
const STOCK_POOL: &[&str] = &[
    "600519", "000858", "601318", "300750", "000333", "600036", "002415", "600276",
    "00700", "09988", "AAPL", "MSFT",
];

fn validate_stock_code(code: &str) -> Result<()> {
    let c = code.trim();
    if c.is_empty() || c.len() > 16 || !c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '.') {
        return Err(anyhow!("股票代码非法: {code}（仅允许字母/数字/点，1-16 位）"));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery { #[serde(default)] pub q: String }

pub async fn search_handler(Query(q): Query<SearchQuery>) -> Json<Vec<search::StockInfo>> {
    let out = tokio::task::spawn_blocking(move || {
        if q.q.trim().is_empty() { return Vec::new(); }
        search::search(q.q.trim()).unwrap_or_default()
    }).await.unwrap_or_default();
    Json(out)
}

#[derive(Debug, Deserialize)]
pub struct DiagnoseQuery { pub code: String }

pub async fn diagnose_handler(Query(q): Query<DiagnoseQuery>) -> std::result::Result<Json<StockDiagnosis>, AppError> {
    let d = tokio::task::spawn_blocking(move || diagnose_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Json(d))
}

fn diagnose_blocking(q: DiagnoseQuery) -> Result<StockDiagnosis> {
    validate_stock_code(&q.code)?;
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(800);
    let bars = cache::load_or_fetch(&q.code, stock_cache(), start, end)
        .map_err(|e| anyhow!("加载行情失败: {e}"))?;
    diagnose::diagnose(q.code.clone(), q.code.clone(), &bars, &DiagnoseParams::default())
}

#[derive(Debug, Deserialize)]
pub struct StockRunQuery {
    pub code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: String,
    #[serde(default)] pub initial_cash: f64,
    #[serde(default)] pub period: Option<String>,
    #[serde(default)] pub day: Option<u32>,
    #[serde(default)] pub base_amount: Option<f64>,
    #[serde(default)] pub ma_window: Option<usize>,
    #[serde(default)] pub k: Option<f64>,
    #[serde(default)] pub short_window: Option<usize>,
    #[serde(default)] pub long_window: Option<usize>,
    #[serde(default)] pub amount: Option<f64>,
    #[serde(default)] pub rsi_window: Option<usize>,
    #[serde(default)] pub oversold: Option<f64>,
    #[serde(default)] pub overbought: Option<f64>,
}

pub async fn run_handler(Query(q): Query<StockRunQuery>) -> std::result::Result<Json<StockRunOutcome>, AppError> {
    let out = tokio::task::spawn_blocking(move || run_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Json(out))
}

fn run_blocking(q: StockRunQuery) -> Result<StockRunOutcome> {
    validate_stock_code(&q.code)?;
    if q.start >= q.end { return Err(anyhow!("回测区间错误: start ({}) 必须早于 end ({})", q.start, q.end)); }
    let secid = data::resolve_secid(&q.code).map_err(|e| anyhow!("代码解析失败: {e}"))?;
    let fee = StockFee::for_market(secid.market);
    let bars = cache::load_or_fetch(&q.code, stock_cache(), q.start, q.end)
        .map_err(|e| anyhow!("加载行情失败: {e}"))?;
    let sf = StrategyFields {
        strategy: q.strategy.clone(),
        period: q.period.clone(), day: q.day, base_amount: q.base_amount,
        ma_window: q.ma_window, k: q.k,
        short_window: q.short_window, long_window: q.long_window, amount: q.amount,
        rsi_window: q.rsi_window, oversold: q.oversold, overbought: q.overbought,
    };
    let strategy = build_strategy_from_fields(&sf)?;
    Ok(backtest::run_one(q.strategy.clone(), q.code.clone(), bars, strategy, fee, q.initial_cash))
}

#[derive(Debug, Deserialize)]
pub struct RecommendQuery { #[serde(default)] pub top_n: Option<usize> }

pub async fn recommend_handler(Query(q): Query<RecommendQuery>) -> std::result::Result<Json<StockRecommendReport>, AppError> {
    let rep = tokio::task::spawn_blocking(move || recommend_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))?;
    Ok(Json(rep))
}

fn recommend_blocking(q: RecommendQuery) -> StockRecommendReport {
    let params = RecommendParams { top_n: q.top_n.unwrap_or(5), ..Default::default() };
    let names = std::collections::HashMap::new();
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);
    recommend::build_report(STOCK_POOL, &names, &end.to_string(), &params,
        |code| cache::load_or_fetch(code, stock_cache(), start, end))
}

#[derive(Debug, Deserialize)]
pub struct SyncRequest { #[serde(default)] pub code: Option<String> }

pub async fn sync_handler(axum::Json(req): axum::Json<SyncRequest>) -> Json<Vec<sync::SyncOutcome>> {
    let out = tokio::task::spawn_blocking(move || {
        let dir = stock_cache();
        match req.code {
            Some(c) => vec![sync::sync_stock(&c, dir)],
            None => sync::sync_all(dir),
        }
    }).await.unwrap_or_default();
    Json(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower::ServiceExt;

    #[test]
    fn validate_stock_code_rules() {
        assert!(validate_stock_code("600519").is_ok());
        assert!(validate_stock_code("00700").is_ok());
        assert!(validate_stock_code("AAPL").is_ok());
        assert!(validate_stock_code("us.AAPL").is_ok());
        assert!(validate_stock_code("bad!!code").is_err());
        assert!(validate_stock_code("").is_err());
    }

    #[test]
    fn stock_pool_valid() {
        assert!(!STOCK_POOL.is_empty());
        for c in STOCK_POOL { assert!(validate_stock_code(c).is_ok(), "池内代码应合法: {c}"); }
    }

    #[tokio::test]
    async fn search_route_returns_json_array() {
        let resp = crate::web::router()
            .oneshot(Request::builder().uri("/api/stock/search?q=xyzzy_no_such").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
    }

    #[tokio::test]
    async fn diagnose_bad_code_is_400() {
        let resp = crate::web::router()
            .oneshot(Request::builder().uri("/api/stock/diagnose?code=bad!!code").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn sync_bad_code_returns_error_array() {
        let body = serde_json::json!({"code":"bad!!code"}).to_string();
        let resp = crate::web::router()
            .oneshot(Request::builder().method("POST").uri("/api/stock/sync")
                .header(header::CONTENT_TYPE, "application/json").body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array() && v.as_array().unwrap().len() == 1);
        assert!(v[0]["error"].is_string(), "非法代码应带 error");
    }
}

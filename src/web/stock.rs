//! 股票 Web 接口：搜索/诊断/回测/选股/同步。web 作为组合根接入 stock 能力。
use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use std::path::Path;

use axum::extract::Query;
use axum::response::Json;

use super::{AppError, StrategyFields, build_strategy_from_fields};
use crate::stock::data::{self, cache, fundamentals, search, sync, universe, valuation};
use crate::stock::fee::StockFee;
use crate::stock::diagnose::{self, DiagnoseParams, StockDiagnosis};
use crate::stock::backtest::{self, StockRunOutcome};
use crate::stock::recommend::{self, RecommendParams, StockRecommendReport};
use crate::stock::screen::{self, ScreenParams, ScreenReport};
use crate::stock::attribution::{self, Attribution};

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
    // 对外展示必须带证据：给了「买入」就得说清这个信号到底有没有用
    diagnose::diagnose_with_evidence(q.code.clone(), q.code.clone(), &bars, &DiagnoseParams::default())
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

// ---- 质量筛选 ----

fn fundamentals_cache() -> &'static Path { Path::new(".cache/fundamentals") }
fn valuation_cache() -> &'static Path { Path::new(".cache/valuation") }
fn universe_cache() -> &'static Path { Path::new(".cache") }

/// 财报缓存有效期（天）。财报每季度才更新一次，30 天足够新鲜。
const FUNDAMENTALS_MAX_AGE: i64 = 30;

#[derive(Debug, Deserialize)]
pub struct ScreenQuery {
    #[serde(default)] pub top_n: Option<usize>,
    /// 只筛这些代码（逗号分隔）。留空则筛全市场 —— 全市场要逐只抓财报，
    /// 5000+ 次请求会跑很久且极易被限流，故默认走预设池。
    #[serde(default)] pub codes: Option<String>,
}

pub async fn screen_handler(Query(q): Query<ScreenQuery>) -> std::result::Result<Json<ScreenReport>, AppError> {
    let rep = tokio::task::spawn_blocking(move || screen_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Json(rep))
}

fn screen_blocking(q: ScreenQuery) -> Result<ScreenReport> {
    let params = ScreenParams { top_n: q.top_n.unwrap_or(20), ..Default::default() };

    let date = universe::latest_trade_date().map_err(|e| anyhow!("探测交易日失败: {e}"))?;
    let all = universe::load_or_fetch(universe_cache(), date)
        .map_err(|e| anyhow!("加载全市场清单失败: {e}"))?;

    // 默认只筛预设池：全市场逐只抓财报是 5000+ 次请求，会跑很久且极易触发东财限流。
    // 想筛全市场应走离线批处理，而不是一个 HTTP 请求。
    let wanted: Vec<String> = match q.codes.as_deref() {
        Some(s) if !s.trim().is_empty() => {
            let codes: Vec<String> = s.split(',').map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()).collect();
            for c in &codes { validate_stock_code(c)?; }
            codes
        }
        _ => STOCK_POOL.iter().map(|s| s.to_string()).collect(),
    };
    let pool: Vec<universe::Listing> = all.into_iter()
        .filter(|l| wanted.iter().any(|w| w == &l.code))
        .collect();

    let today = chrono::Local::now().date_naive();
    Ok(screen::build_report(&pool, &date.to_string(), &today.to_string(), &params, |l| {
        let reports = fundamentals::load_or_fetch(
            &l.code, fundamentals_cache(), FUNDAMENTALS_MAX_AGE, today)?;
        // 港股无估值历史（datacenter 没有对应表）→ 空序列，分位因子自动降级为 None
        let vals = valuation::load_or_fetch(&l.code, valuation_cache(), date).unwrap_or_default();
        Ok((reports, vals))
    }))
}

// ---- 回报归因 ----

#[derive(Debug, Deserialize)]
pub struct AttributionQuery {
    pub code: String,
    /// 起始日 YYYY-MM-DD。留空则用数据覆盖的最早日期。
    #[serde(default)] pub start: Option<String>,
}

pub async fn attribution_handler(Query(q): Query<AttributionQuery>) -> std::result::Result<Json<Attribution>, AppError> {
    let a = tokio::task::spawn_blocking(move || attribution_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Json(a))
}

fn attribution_blocking(q: AttributionQuery) -> Result<Attribution> {
    validate_stock_code(&q.code)?;
    let end = chrono::Local::now().date_naive();

    // 归因窗口的上限由**后复权覆盖范围**决定，不是估值历史：
    // 腾讯的后复权只有约 2.6 年，东财不裁剪但 push2his 限流频繁。
    // 这里照常走 cache::load_or_fetch（生产主路径），拿到多少算多少 ——
    // 覆盖不足时 attribution 会自己降级为裸价格口径并显式告警，而不是伪造分红贡献。
    let bars = cache::load_or_fetch(&q.code, stock_cache(), end - chrono::Duration::days(9000), end)
        .map_err(|e| anyhow!("加载行情失败: {e}"))?;
    let vals = valuation::load_or_fetch(&q.code, valuation_cache(), end)
        .map_err(|e| anyhow!("加载估值历史失败: {e}（仅沪深A股有估值历史）"))?;

    let start = match q.start.as_deref() {
        Some(s) if !s.trim().is_empty() =>
            NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
                .map_err(|_| anyhow!("起始日格式应为 YYYY-MM-DD: {s}"))?,
        // 两个数据源都覆盖的最早日期
        _ => bars.first().map(|b| b.date).unwrap_or(end)
            .max(vals.first().map(|v| v.date).unwrap_or(end)),
    };

    attribution::attribute(&bars, &vals, start, end)
        .map_err(|e| anyhow!("无法归因: {e}"))
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

#[derive(Debug, Deserialize)]
pub struct MoversQuery {
    /// 查询日期，默认今天。格式 YYYY-MM-DD。
    #[serde(default)]
    pub day: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct MoversReport {
    pub day: String,
    pub count: usize,
    pub movers: Vec<crate::stock::realtime::store::SignalRow>,
    pub disclaimer: &'static str,
}

/// 盘中异动榜。读库，不抓网络 —— 抓取由守护进程负责。
pub async fn realtime_movers_handler(
    Query(q): Query<MoversQuery>,
) -> std::result::Result<Json<MoversReport>, AppError> {
    let r = tokio::task::spawn_blocking(move || realtime_movers_blocking(q))
        .await.map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Json(r))
}

fn realtime_movers_blocking(q: MoversQuery) -> Result<MoversReport> {
    use crate::stock::realtime::{job, store};
    let day = match q.day.as_deref() {
        Some(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|e| anyhow!("日期格式应为 YYYY-MM-DD: {e}"))?,
        None => chrono::Local::now().date_naive(),
    };
    // 库路径从 config 读，不硬编码 —— stock_cache() 那种 &'static Path 的写法
    // 绕过了配置，新代码不重复这个错。
    let cfg = realtime_cfg();
    let conn = store::open(&cfg.db_path)?;
    let movers = store::signals_on(&conn, day)?;
    Ok(MoversReport {
        day: day.to_string(),
        count: movers.len(),
        movers,
        disclaimer: job::DISCLAIMER,
    })
}

/// 读 config.toml 的 [realtime] 段；缺失或损坏时用默认值。
///
/// 宽松加载与 `AuthCfg::default()` 的处理一致：Web 层不该因为 config.toml
/// 少一段就整个挂掉。
fn realtime_cfg() -> crate::stock::realtime::RealtimeCfg {
    crate::stock::realtime::config::load_from_toml(std::path::Path::new("config.toml"))
        .unwrap_or_default()
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
        let resp = crate::web::core_router()
            .oneshot(Request::builder().uri("/api/stock/search?q=xyzzy_no_such").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
    }

    #[tokio::test]
    async fn realtime_movers_route_returns_report_with_disclaimer() {
        let resp = crate::web::core_router()
            .oneshot(Request::builder().uri("/api/stock/realtime/movers").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["movers"].is_array(), "movers 应是数组");
        assert!(v["day"].is_string());
        // 免责声明不是装饰：不能让人误以为「主力资金」是真实席位数据
        let d = v["disclaimer"].as_str().unwrap_or_default();
        assert!(d.contains("代理指标"), "榜单必须带资金流局限声明");
        assert!(d.contains("非投资建议"));
    }

    #[tokio::test]
    async fn realtime_movers_bad_date_is_400() {
        let resp = crate::web::core_router()
            .oneshot(Request::builder().uri("/api/stock/realtime/movers?day=garbage").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400, "非法日期须 400，而非静默用今天");
    }

    #[tokio::test]
    async fn diagnose_bad_code_is_400() {
        let resp = crate::web::core_router()
            .oneshot(Request::builder().uri("/api/stock/diagnose?code=bad!!code").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn sync_bad_code_returns_error_array() {
        let body = serde_json::json!({"code":"bad!!code"}).to_string();
        let resp = crate::web::core_router()
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

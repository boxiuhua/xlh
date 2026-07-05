pub mod page;
pub mod stock;
pub mod auth;

use anyhow::{anyhow, Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;
use std::collections::BTreeMap;

use crate::broker::{FeeModel, SellTier};
use crate::config::build_strategy_from;
use crate::strategy::Strategy;

#[derive(Debug, Deserialize)]
pub struct RunQuery {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: String,
    #[serde(default)] pub buy_rate: f64,
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

pub struct RunSpec {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub strategy: Box<dyn Strategy>,
    pub fee: FeeModel,
    pub initial_cash: f64,
}

impl std::fmt::Debug for RunSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunSpec")
            .field("fund_code", &self.fund_code)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("fee", &self.fee)
            .field("initial_cash", &self.initial_cash)
            .finish_non_exhaustive()
    }
}

/// 标准 A 股基金卖出费率阶梯（首版固定，不进表单）。
fn standard_sell_tiers() -> Vec<SellTier> {
    vec![
        SellTier { max_days: 7, rate: 0.015 },
        SellTier { max_days: 365, rate: 0.005 },
        SellTier { max_days: 0, rate: 0.0 },
    ]
}

#[derive(Debug, Deserialize)]
pub struct StrategyFields {
    pub strategy: String,
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

fn validate_fund_code(code: &str) -> Result<()> {
    if code.is_empty() || code.len() > 12 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(anyhow!("基金代码非法: {code} （只允许 1-12 位字母或数字）"));
    }
    Ok(())
}

fn validate_common(fund: &str, start: NaiveDate, end: NaiveDate, buy_rate: f64) -> Result<()> {
    if start >= end {
        return Err(anyhow!("回测区间错误: start ({start}) 必须早于 end ({end})"));
    }
    if !(0.0..1.0).contains(&buy_rate) {
        return Err(anyhow!("buy_rate ({buy_rate}) 必须在 [0.0, 1.0) 范围内"));
    }
    validate_fund_code(fund)
}

/// 按策略把字段拼成 build_strategy_from 所需的 toml 参数表（缺必填字段报错，含字段名）。
fn strategy_params_table(s: &StrategyFields) -> Result<toml::Table> {
    let mut t = toml::Table::new();
    let need = |o: bool, name: &str| -> Result<()> {
        if o { Ok(()) } else { Err(anyhow!("策略 {} 缺少必填参数: {}", s.strategy, name)) }
    };
    match s.strategy.as_str() {
        "dca" => {
            need(s.period.is_some(), "period")?;
            need(s.day.is_some(), "day")?;
            need(s.base_amount.is_some(), "base_amount")?;
            t.insert("period".into(), s.period.clone().unwrap().into());
            t.insert("day".into(), (s.day.unwrap() as i64).into());
            t.insert("base_amount".into(), s.base_amount.unwrap().into());
        }
        "smart_dca" => {
            need(s.period.is_some(), "period")?;
            need(s.day.is_some(), "day")?;
            need(s.base_amount.is_some(), "base_amount")?;
            need(s.ma_window.is_some(), "ma_window")?;
            t.insert("period".into(), s.period.clone().unwrap().into());
            t.insert("day".into(), (s.day.unwrap() as i64).into());
            t.insert("base_amount".into(), s.base_amount.unwrap().into());
            t.insert("ma_window".into(), (s.ma_window.unwrap() as i64).into());
            t.insert("k".into(), s.k.unwrap_or(1.0).into());
        }
        "trend" => {
            need(s.short_window.is_some(), "short_window")?;
            need(s.long_window.is_some(), "long_window")?;
            need(s.amount.is_some(), "amount")?;
            t.insert("short_window".into(), (s.short_window.unwrap() as i64).into());
            t.insert("long_window".into(), (s.long_window.unwrap() as i64).into());
            t.insert("amount".into(), s.amount.unwrap().into());
        }
        "rsi" => {
            need(s.rsi_window.is_some(), "rsi_window")?;
            need(s.oversold.is_some(), "oversold")?;
            need(s.overbought.is_some(), "overbought")?;
            need(s.amount.is_some(), "amount")?;
            t.insert("rsi_window".into(), (s.rsi_window.unwrap() as i64).into());
            t.insert("oversold".into(), s.oversold.unwrap().into());
            t.insert("overbought".into(), s.overbought.unwrap().into());
            t.insert("amount".into(), s.amount.unwrap().into());
        }
        "adaptive" => {
            need(s.period.is_some(), "period")?;
            need(s.day.is_some(), "day")?;
            need(s.base_amount.is_some(), "base_amount")?;
            t.insert("period".into(), s.period.clone().unwrap().into());
            t.insert("day".into(), (s.day.unwrap() as i64).into());
            t.insert("base_amount".into(), s.base_amount.unwrap().into());
        }
        other => return Err(anyhow!("未知策略: {other}")),
    }
    Ok(t)
}

pub fn build_strategy_from_fields(s: &StrategyFields) -> Result<Box<dyn Strategy>> {
    let t = strategy_params_table(s)?;
    build_strategy_from(&s.strategy, &Some(toml::Value::Table(t)), &[])
}

#[derive(Debug, Deserialize)]
pub struct CompareRunReq {
    pub name: String,
    #[serde(default)] pub fund_code: Option<String>,
    #[serde(flatten)] pub params: StrategyFields,
}

#[derive(Debug, Deserialize)]
pub struct CompareRequest {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    pub runs: Vec<CompareRunReq>,
}

fn default_top_n_web() -> usize { 5 }

#[derive(Debug, Deserialize)]
pub struct OptimizeRequest {
    pub fund_code: String,
    pub start: NaiveDate,
    pub end: NaiveDate,
    #[serde(default)] pub buy_rate: f64,
    #[serde(default)] pub initial_cash: f64,
    pub strategy: String,
    pub metric: String,
    #[serde(default = "default_top_n_web")] pub top_n: usize,
    pub grid: BTreeMap<String, String>,
}

/// 把 "120,250,500" 拆成 toml 值数组；每值试 i64→f64→String。空/全空白报错。
pub fn parse_csv_values(s: &str) -> Result<Vec<toml::Value>> {
    let vals: Vec<toml::Value> = s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| {
            if let Ok(i) = p.parse::<i64>() { toml::Value::Integer(i) }
            else if let Ok(f) = p.parse::<f64>() { toml::Value::Float(f) }
            else { toml::Value::String(p.to_string()) }
        })
        .collect();
    if vals.is_empty() {
        return Err(anyhow!("参数取值列表为空"));
    }
    Ok(vals)
}

/// 由寻优请求构建 OptimizeCfg：按策略取所需参数名，各 CSV → toml 数组。
pub fn build_optimize_cfg(req: &OptimizeRequest) -> Result<crate::config::OptimizeCfg> {
    let keys: &[&str] = match req.strategy.as_str() {
        "dca" => &["period", "day", "base_amount"],
        "smart_dca" => &["period", "day", "base_amount", "ma_window", "k"],
        "trend" => &["short_window", "long_window", "amount"],
        "rsi" => &["rsi_window", "oversold", "overbought", "amount"],
        "adaptive" => &["period", "day", "base_amount"],
        other => return Err(anyhow!("未知策略: {other}")),
    };
    let mut grid = toml::Table::new();
    for &name in keys {
        let csv = req.grid.get(name)
            .ok_or_else(|| anyhow!("寻优缺少参数网格: {name}"))?;
        let vals = parse_csv_values(csv)
            .map_err(|e| anyhow!("参数 {name} 取值非法: {e}"))?;
        grid.insert(name.to_string(), toml::Value::Array(vals));
    }
    Ok(crate::config::OptimizeCfg {
        strategy: req.strategy.clone(),
        metric: req.metric.clone(),
        top_n: req.top_n,
        grid,
        rules: Vec::new(),
    })
}

/// 校验 query 并组装回测所需的一切；纯函数，不做任何 IO。
pub fn build_run_from_query(q: &RunQuery) -> Result<RunSpec> {
    validate_common(&q.fund_code, q.start, q.end, q.buy_rate)?;
    let sf = StrategyFields {
        strategy: q.strategy.clone(),
        period: q.period.clone(), day: q.day, base_amount: q.base_amount,
        ma_window: q.ma_window, k: q.k,
        short_window: q.short_window, long_window: q.long_window, amount: q.amount,
        rsi_window: q.rsi_window, oversold: q.oversold, overbought: q.overbought,
    };
    let strategy = build_strategy_from_fields(&sf)?;
    let fee = FeeModel { buy_rate: q.buy_rate, sell_tiers: standard_sell_tiers() };
    Ok(RunSpec {
        fund_code: q.fund_code.clone(),
        start: q.start, end: q.end, strategy, fee, initial_cash: q.initial_cash,
    })
}

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;

use auth::AuthState;

/// 核心业务 /api 路由（不含 `/` 首页）。对任意 state 泛型：这些 handler
/// 都无需 state，故既能装进 `Router<()>`（测试直连 `core_router`），
/// 也能装进 `Router<AuthState>`（生产授权分组）。
fn core_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/run", get(run_handler))
        .route("/api/funds", get(funds_handler))
        .route("/api/regime", get(regime_handler))
        .route("/api/recommend", get(recommend_handler))
        .route("/api/holdings", post(holdings_handler))
        .route("/api/compare", post(compare_handler))
        .route("/api/optimize", post(optimize_handler))
        .route("/api/sync", post(sync_handler))
        .route("/api/stock/search", get(stock::search_handler))
        .route("/api/stock/diagnose", get(stock::diagnose_handler))
        .route("/api/stock/run", get(stock::run_handler))
        .route("/api/stock/recommend", get(stock::recommend_handler))
        .route("/api/stock/sync", post(stock::sync_handler))
}

/// 推送配置路由（含 push.toml 运营者密钥）。生产中仅挂在管理员分组（require_admin），
/// 不属于按 license 放行的核心业务组；测试用 `core_router` 中直连以复用既有 push 测试。
fn push_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/api/push/config", get(push_config_get).post(push_config_save))
        .route("/api/push/preview", post(push_preview))
        .route("/api/push/test", post(push_test))
}

/// 无授权的业务路由（首页 + 全部核心 API + 推送配置），供既有单元测试直连 `.oneshot`。
/// 不带 state、不套任何认证中间件。
#[cfg(test)]
pub(crate) fn core_router() -> Router {
    core_routes::<()>()
        .merge(push_routes::<()>())
        .route("/", get(index_page))
}

/// 生产入口：公开 / 需登录 / 需登录+授权 / 需登录+管理 四组，末尾注入 state。
pub fn router(state: AuthState) -> Router {
    // 公开：登录/注册页与其 API；`/` 自带登录判断（未登录跳 /login）
    let public = Router::new()
        .route("/", get(index))
        .route("/login", get(page::login_html_handler))
        .route("/api/auth/register", post(auth::handlers::register))
        .route("/api/auth/login", post(auth::handlers::login));

    // 需登录（不要求授权）：logout、activate、me
    let authed = Router::new()
        .route("/api/auth/logout", post(auth::handlers::logout))
        .route("/api/auth/activate", post(auth::handlers::activate))
        .route("/api/auth/me", get(auth::handlers::me))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    // 需登录 + 授权：核心业务
    let licensed = core_routes::<AuthState>()
        .merge(holdings_history_routes())
        .route_layer(from_fn_with_state(state.clone(), auth::require_license))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    // 需登录 + 管理员：后台 + 推送配置（含运营者密钥，仅运营者可见）
    let admin = auth::routes::admin_router()
        .merge(push_routes::<AuthState>())
        .route_layer(from_fn_with_state(state.clone(), auth::require_admin))
        .route_layer(from_fn_with_state(state.clone(), auth::require_login));

    public.merge(authed).merge(licensed).merge(admin).with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct SyncRequest {
    #[serde(default)]
    pub code: Option<String>,
}

async fn sync_handler(
    axum::Json(req): axum::Json<SyncRequest>,
) -> axum::Json<Vec<crate::data::sync::SyncOutcome>> {
    let out = tokio::task::spawn_blocking(move || {
        let dir = std::path::Path::new(".cache");
        match req.code {
            Some(c) => vec![crate::data::sync::sync_fund(&c, dir)],
            None => crate::data::sync::sync_all(dir),
        }
    })
    .await
    .unwrap_or_default();
    axum::Json(out)
}

/// 加载基金清单；任何失败降级为空数组（不阻断界面）。
fn funds_payload(cache_dir: &std::path::Path) -> Vec<crate::data::fundlist::FundInfo> {
    crate::data::fundlist::load_or_fetch_fund_list(cache_dir).unwrap_or_else(|e| {
        eprintln!("基金清单加载失败: {e}");
        Vec::new()
    })
}

async fn funds_handler() -> axum::Json<Vec<crate::data::fundlist::FundInfo>> {
    let funds = tokio::task::spawn_blocking(|| funds_payload(std::path::Path::new(".cache")))
        .await
        .unwrap_or_default();
    axum::Json(funds)
}

pub async fn serve(config_path: std::path::PathBuf, port: u16) -> Result<()> {
    let cfg = auth::config::load_auth(&config_path);
    let conn = auth::store::open(&cfg.db_path).context("打开授权数据库失败")?;
    crate::history::migrate(&conn).context("建历史表失败")?;
    let state = auth::AuthState::new(conn, cfg);

    // 默认只监听本机；容器内可用 XLH_BIND=0.0.0.0 对外暴露。
    let host: std::net::IpAddr = std::env::var("XLH_BIND")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::net::IpAddr::from([127, 0, 0, 1]));
    let addr = std::net::SocketAddr::new(host, port);
    let listener = tokio::net::TcpListener::bind(addr).await
        .with_context(|| format!("绑定 {addr} 失败"))?;
    println!("回测界面已启动：http://{addr}  (Ctrl+C 退出)");
    axum::serve(listener, router(state)).await.context("服务运行失败")?;
    Ok(())
}

/// 无条件返回主页面 HTML（供 `core_router` 直连测试）。
#[cfg(test)]
async fn index_page() -> Html<&'static str> {
    Html(page::INDEX_HTML)
}

/// 生产首页：已登录返回主页面，未登录跳转 /login（自带判断，不套 require_login）。
async fn index(State(st): State<AuthState>, headers: HeaderMap) -> Response {
    let now = chrono::Local::now().date_naive();
    let logged_in = auth::session::read_cookie(&headers)
        .and_then(|t| {
            let conn = st.db.lock().unwrap();
            auth::store::lookup_session_user(&conn, &t, now).ok().flatten()
        })
        .is_some();
    if logged_in {
        Html(page::INDEX_HTML).into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

async fn run_handler(Query(q): Query<RunQuery>) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || run_blocking(q))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

/// 同步跑回测并渲染报告。在 spawn_blocking 线程内执行，
/// 非 Send 的 Box<dyn Strategy> 在此创建并消费，不跨 await。
fn run_blocking(q: RunQuery) -> Result<String> {
    let spec = build_run_from_query(&q)?;
    let points = crate::data::cache::load_or_fetch(
        &spec.fund_code, std::path::Path::new(".cache"), spec.start, spec.end)
        .with_context(|| format!("加载净值失败: {}", spec.fund_code))?;
    let strategy_desc = match q.strategy.as_str() {
        "dca" => "普通定投",
        "smart_dca" => "智能定投",
        "trend" => "均线择时",
        "rsi" => "RSI超买超卖",
        "adaptive" => "自适应",
        other => other,
    }
    .to_string();
    let meta = crate::report::html::ReportMeta {
        fund_code: spec.fund_code.clone(),
        start: spec.start,
        end: spec.end,
        strategy: q.strategy.clone(),
        strategy_desc,
        initial_cash: spec.initial_cash,
    };
    let data = crate::data::InMemoryData::new(points);
    let broker = crate::broker::Broker::new(spec.fee);
    let portfolio = crate::portfolio::Portfolio::new(spec.initial_cash);
    let mut engine = crate::engine::Engine::new(data, spec.strategy, broker, portfolio);
    engine.run();
    Ok(crate::report::html::render_report_html(
        &meta, engine.portfolio(), engine.daily(), engine.trades()))
}

async fn compare_handler(
    axum::Json(req): axum::Json<CompareRequest>,
) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || compare_blocking(req))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

fn compare_blocking(req: CompareRequest) -> Result<String> {
    validate_common(&req.fund_code, req.start, req.end, req.buy_rate)?;
    if req.runs.is_empty() {
        return Err(anyhow!("对比至少需要一个策略"));
    }
    let fee = crate::broker::FeeModel { buy_rate: req.buy_rate, sell_tiers: standard_sell_tiers() };
    let mut outcomes = Vec::with_capacity(req.runs.len());
    for run in &req.runs {
        let fund = run.fund_code.clone().unwrap_or_else(|| req.fund_code.clone());
        validate_fund_code(&fund)?;
        let strategy = build_strategy_from_fields(&run.params)
            .map_err(|e| anyhow!("策略 [{}] 构建失败: {e}", run.name))?;
        let points = crate::data::cache::load_or_fetch(
            &fund, std::path::Path::new(".cache"), req.start, req.end)
            .map_err(|e| anyhow!("策略 [{}] 加载 {fund} 失败: {e}", run.name))?;
        let outcome = crate::runner::run_one(
            run.name.clone(), fund, points, strategy, fee.clone(), req.initial_cash);
        outcomes.push(outcome);
    }
    let meta = crate::report::compare::CompareMeta { start: req.start, end: req.end };
    Ok(crate::report::compare::render_compare_html(&meta, &outcomes))
}

async fn optimize_handler(
    axum::Json(req): axum::Json<OptimizeRequest>,
) -> std::result::Result<Html<String>, AppError> {
    let html = tokio::task::spawn_blocking(move || optimize_blocking(req))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(Html(html))
}

fn optimize_blocking(req: OptimizeRequest) -> Result<String> {
    validate_common(&req.fund_code, req.start, req.end, req.buy_rate)?;
    let cfg = build_optimize_cfg(&req)?;
    let fee = crate::broker::FeeModel { buy_rate: req.buy_rate, sell_tiers: standard_sell_tiers() };
    let points = crate::data::cache::load_or_fetch(
        &req.fund_code, std::path::Path::new(".cache"), req.start, req.end)
        .map_err(|e| anyhow!("加载净值失败: {e}"))?;
    let report = crate::optimize::run_optimize(&cfg, &req.fund_code, &points, fee, req.initial_cash)?;
    let meta = crate::report::optimize::OptMeta {
        start: req.start, end: req.end, fund_code: req.fund_code.clone(),
    };
    Ok(crate::report::optimize::render_optimize_html(&meta, &report))
}

#[derive(Debug, Deserialize)]
pub struct RegimeQuery {
    pub fund_code: String,
    #[serde(default)]
    pub window: Option<usize>,
    #[serde(default)]
    pub band_window: Option<usize>,
    #[serde(default)]
    pub base_amount: Option<f64>,
    #[serde(default)]
    pub sell_pct: Option<f64>,
}

async fn regime_handler(
    axum::extract::Query(q): axum::extract::Query<RegimeQuery>,
) -> std::result::Result<axum::Json<crate::analyze::RegimeReport>, AppError> {
    let report = tokio::task::spawn_blocking(move || regime_blocking(q))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(axum::Json(report))
}

fn regime_blocking(q: RegimeQuery) -> Result<crate::analyze::RegimeReport> {
    validate_fund_code(&q.fund_code)?;
    let window = q.window.unwrap_or(120);
    let default_plan = crate::analyze::PlanParams::default();
    let band_window = q.band_window.unwrap_or(default_plan.band_window);
    let end = chrono::Local::now().date_naive();
    // 多取数据以覆盖 window、均线与波动带窗口
    let lookback = (window.max(band_window) as i64) * 2 + 120;
    let start = end - chrono::Duration::days(lookback);
    let points = crate::data::cache::load_or_fetch(
        &q.fund_code, std::path::Path::new(".cache"), start, end)
        .map_err(|e| anyhow!("加载净值失败: {e}"))?;
    let params = crate::analyze::RegimeParams { window, ..Default::default() };
    let plan = crate::analyze::PlanParams {
        band_window,
        base_amount: q.base_amount.unwrap_or(default_plan.base_amount),
        sell_pct: q.sell_pct.unwrap_or(default_plan.sell_pct),
    };
    crate::analyze::detect_regime_with_plan(&points, &params, &plan)
}

/// 预设精选基金池（宽基指数 / 行业 / 口碑主动，含现有缓存）。可增删。
/// 中文名运行时从 fundlist.json 反查；净值缺失/不足则跳过。
const PRESET_POOL: &[&str] = &[
    "161725", "050002", "000834", "001427", "003095", "008888",
    "110011", "005827", "110022", "161005", "163406", "260108",
    "000961", "001593", "519674", "320007", "002001", "001714",
    "000478", "270042", "040046", "519066", "005669", "001102",
];

#[derive(Debug, Deserialize)]
pub struct RecommendQuery {
    #[serde(default)]
    pub top_n: Option<usize>,
}

async fn recommend_handler(
    axum::extract::Query(q): axum::extract::Query<RecommendQuery>,
) -> std::result::Result<axum::Json<crate::recommend::RecommendReport>, AppError> {
    let report = tokio::task::spawn_blocking(move || recommend_blocking(q))
        .await
        .map_err(|e| anyhow!("任务执行失败: {e}"))??;
    Ok(axum::Json(report))
}

fn recommend_blocking(q: RecommendQuery) -> Result<crate::recommend::RecommendReport> {
    let params = crate::recommend::RecommendParams {
        top_n: q.top_n.unwrap_or(5),
        ..Default::default()
    };
    // code → 中文名 映射（清单加载失败则空映射，名字回退为代码）
    let names: std::collections::HashMap<String, String> = funds_payload(std::path::Path::new(".cache"))
        .into_iter()
        .map(|f| (f.code, f.name))
        .collect();
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);
    let report = crate::recommend::build_report(
        PRESET_POOL, &names, &end.to_string(), &params,
        |code| crate::data::cache::load_or_fetch(code, std::path::Path::new(".cache"), start, end),
    );
    Ok(report)
}

async fn holdings_handler(
    axum::Json(input): axum::Json<crate::holdings::HoldingsInput>,
) -> std::result::Result<axum::Json<crate::holdings::HoldingsReport>, AppError> {
    let report = tokio::task::spawn_blocking(move || holdings_blocking(input))
        .await
        .map_err(|e| anyhow!("任务执行失败: {e}"))?;
    Ok(axum::Json(report))
}

fn holdings_blocking(input: crate::holdings::HoldingsInput) -> crate::holdings::HoldingsReport {
    let params = crate::recommend::RecommendParams::default();
    // code → 中文名 映射（清单加载失败则回退代码）
    let names: std::collections::HashMap<String, String> = funds_payload(std::path::Path::new(".cache"))
        .into_iter()
        .map(|f| (f.code, f.name))
        .collect();
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days(8 * 365);
    crate::holdings::build_report(
        &input,
        |code| names.get(code).cloned().unwrap_or_else(|| code.to_string()),
        &end.to_string(),
        &params,
        |code| crate::data::cache::load_or_fetch(code, std::path::Path::new(".cache"), start, end),
    )
}

async fn holdings_save_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::Json(input): axum::Json<crate::holdings::HoldingsInput>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    let input_for_run = input.clone();
    let report = tokio::task::spawn_blocking(move || holdings_blocking(input_for_run))
        .await
        .map_err(|e| anyhow!("任务执行失败: {e}"))?;
    let summary = crate::holdings::summarize(&report);
    let payload = serde_json::to_string(&serde_json::json!({ "input": input, "report": report }))
        .map_err(|e| anyhow!("序列化历史失败: {e}"))?;
    let id = {
        let conn = st.db.lock().unwrap();
        crate::history::save(&conn, Some(user.id), "web", &summary, &payload)
            .map_err(|e| anyhow!("保存历史失败: {e}"))?
    };
    Ok(axum::Json(serde_json::json!({ "ok": true, "id": id })))
}

async fn holdings_history_list_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
) -> axum::Json<Vec<crate::history::AdviceRecord>> {
    let rows = {
        let conn = st.db.lock().unwrap();
        crate::history::list_web(&conn, user.id, 100).unwrap_or_default()
    };
    axum::Json(rows)
}

async fn holdings_history_detail_handler(
    axum::extract::State(st): axum::extract::State<auth::AuthState>,
    axum::Extension(user): axum::Extension<auth::CurrentUser>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Response {
    let found = {
        let conn = st.db.lock().unwrap();
        crate::history::get_web(&conn, id, user.id).ok().flatten()
    };
    match found {
        Some(payload) => ([(axum::http::header::CONTENT_TYPE, "application/json")], payload).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// 持仓历史路由（需要 AuthState + CurrentUser，故不进泛型 core_routes）。
fn holdings_history_routes() -> Router<auth::AuthState> {
    Router::new()
        .route("/api/holdings/save", post(holdings_save_handler))
        .route("/api/holdings/history", get(holdings_history_list_handler))
        .route("/api/holdings/history/:id", get(holdings_history_detail_handler))
}

// ===== 推送配置 Tab 后端 =====
const PUSH_TOML: &str = "push.toml";

async fn push_config_get() -> axum::Json<crate::push::PushConfig> {
    let cfg = std::fs::read_to_string(PUSH_TOML).ok()
        .and_then(|t| toml::from_str::<crate::push::PushConfig>(&t).ok())
        .unwrap_or_else(crate::push::config::default_config);
    axum::Json(cfg)
}

async fn push_config_save(
    axum::Json(cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    crate::push::config::validate(&cfg)?;
    let text = toml::to_string(&cfg).map_err(|e| anyhow!("序列化 push.toml 失败: {e}"))?;
    std::fs::write(PUSH_TOML, text).map_err(|e| anyhow!("写入 {PUSH_TOML} 失败: {e}"))?;
    Ok(axum::Json(serde_json::json!({"ok": true})))
}

async fn push_preview(
    axum::Json(cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    // 预览不发送，故不校验 webhook；只组装消息。
    let (md, has_new) = tokio::task::spawn_blocking(move || crate::push::build_message(&cfg))
        .await.map_err(|e| anyhow!("任务执行失败: {e}"))??;
    Ok(axum::Json(serde_json::json!({"markdown": md, "has_new": has_new})))
}

async fn push_test(
    axum::Json(cfg): axum::Json<crate::push::PushConfig>,
) -> std::result::Result<axum::Json<serde_json::Value>, AppError> {
    crate::push::config::validate(&cfg)?;
    let res = tokio::task::spawn_blocking(move || crate::push::run_once(&cfg, None))
        .await.map_err(|e| anyhow!("任务执行失败: {e}"))?;
    Ok(axum::Json(match res {
        Ok(()) => serde_json::json!({"ok": true}),
        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
    }))
}

pub struct AppError(pub anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = format!(
            "<!doctype html><meta charset=\"utf-8\"><body style=\"font-family:sans-serif;padding:24px;color:#c0392b\"><h3>回测失败</h3><pre>{}</pre>",
            crate::report::html_escape(&self.0.to_string()));
        (StatusCode::BAD_REQUEST, Html(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self { AppError(e.into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(strategy: &str) -> StrategyFields {
        StrategyFields {
            strategy: strategy.into(),
            period: Some("monthly".into()), day: Some(1), base_amount: Some(1000.0),
            ma_window: Some(250), k: Some(1.0),
            short_window: Some(20), long_window: Some(60), amount: Some(1000.0),
            rsi_window: None, oversold: None, overbought: None,
        }
    }

    fn sf_rsi() -> StrategyFields {
        StrategyFields {
            strategy: "rsi".into(),
            period: None, day: None, base_amount: None,
            ma_window: None, k: None, short_window: None, long_window: None,
            amount: Some(1000.0),
            rsi_window: Some(14), oversold: Some(30.0), overbought: Some(70.0),
        }
    }

    #[test]
    fn build_strategy_from_fields_each() {
        for s in ["dca", "smart_dca", "trend"] {
            assert!(build_strategy_from_fields(&sf(s)).is_ok(), "{s} 应成功");
        }
    }

    #[test]
    fn build_strategy_from_fields_missing_param() {
        let mut f = sf("smart_dca");
        f.ma_window = None;
        let err = build_strategy_from_fields(&f).err().expect("应返回 Err");
        assert!(err.to_string().contains("ma_window"), "应提示缺 ma_window: {err}");
    }

    #[test]
    fn validate_fund_code_rejects_traversal() {
        assert!(validate_fund_code("../etc").is_err());
        assert!(validate_fund_code("161725").is_ok());
    }

    fn base(strategy: &str) -> RunQuery {
        RunQuery {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            strategy: strategy.into(),
            buy_rate: 0.0015,
            initial_cash: 0.0,
            period: Some("monthly".into()), day: Some(1), base_amount: Some(1000.0),
            ma_window: Some(250), k: Some(1.0),
            short_window: Some(20), long_window: Some(60), amount: Some(1000.0),
            rsi_window: None, oversold: None, overbought: None,
        }
    }

    #[test]
    fn builds_each_strategy() {
        for s in ["dca", "smart_dca", "trend"] {
            let spec = build_run_from_query(&base(s)).unwrap_or_else(|e| panic!("{s} 应成功: {e}"));
            assert_eq!(spec.fund_code, "161725");
            assert_eq!(spec.fee.sell_tiers.len(), 3);
            assert!((spec.fee.buy_rate - 0.0015).abs() < 1e-9);
        }
    }

    #[test]
    fn rejects_start_after_end() {
        let mut q = base("dca");
        q.start = NaiveDate::from_ymd_opt(2025,1,1).unwrap();
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("start") || err.to_string().contains("区间"),
            "应提示区间错误: {err}");
    }

    #[test]
    fn rejects_unknown_strategy() {
        let q = base("bogus");
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("bogus") || err.to_string().contains("策略"),
            "应提示未知策略: {err}");
    }

    #[test]
    fn rejects_bad_buy_rate() {
        let mut q = base("dca");
        q.buy_rate = 1.5;
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("buy_rate"), "应提示 buy_rate: {err}");
    }

    #[test]
    fn rejects_smart_dca_missing_ma_window() {
        let mut q = base("smart_dca");
        q.ma_window = None;
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("ma_window"), "应提示缺 ma_window: {err}");
    }

    #[test]
    fn rejects_bad_fund_code() {
        let mut q = base("dca");
        q.fund_code = "../etc/passwd".into();
        let err = build_run_from_query(&q).unwrap_err();
        assert!(err.to_string().contains("基金代码"), "应拒绝非法基金代码: {err}");
    }

    #[test]
    fn parse_csv_values_typed() {
        let ints = parse_csv_values("120,250,500").unwrap();
        assert_eq!(ints.len(), 3);
        assert!(matches!(ints[0], toml::Value::Integer(120)));
        let floats = parse_csv_values("0.5, 1.0").unwrap();
        assert_eq!(floats.len(), 2);
        assert!(matches!(floats[0], toml::Value::Float(_)));
        let strs = parse_csv_values("monthly").unwrap();
        assert!(matches!(strs[0], toml::Value::String(_)));
        assert!(parse_csv_values("").is_err());
        assert!(parse_csv_values("  , ").is_err());
    }

    fn opt_req() -> OptimizeRequest {
        let mut grid = std::collections::BTreeMap::new();
        grid.insert("period".into(), "monthly".into());
        grid.insert("day".into(), "1".into());
        grid.insert("base_amount".into(), "1000".into());
        grid.insert("ma_window".into(), "120,250".into());
        grid.insert("k".into(), "1.0".into());
        OptimizeRequest {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            buy_rate: 0.0015, initial_cash: 0.0,
            strategy: "smart_dca".into(), metric: "sharpe".into(), top_n: 5, grid,
        }
    }

    #[test]
    fn build_optimize_cfg_smart_dca() {
        let cfg = build_optimize_cfg(&opt_req()).unwrap();
        assert_eq!(cfg.strategy, "smart_dca");
        assert_eq!(cfg.metric, "sharpe");
        assert_eq!(cfg.grid.len(), 5, "smart_dca 5 个网格参数");
        match cfg.grid.get("ma_window").unwrap() {
            toml::Value::Array(a) => assert_eq!(a.len(), 2),
            _ => panic!("ma_window 应为数组"),
        }
    }

    #[test]
    fn build_optimize_cfg_missing_param() {
        let mut req = opt_req();
        req.grid.remove("base_amount");
        let err = build_optimize_cfg(&req).unwrap_err();
        assert!(err.to_string().contains("base_amount"), "应提示缺 base_amount: {err}");
    }

    async fn post_json(uri: &str, body: serde_json::Value) -> axum::http::StatusCode {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().method("POST").uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string())).unwrap())
            .await.unwrap();
        resp.status()
    }

    #[tokio::test]
    async fn compare_empty_runs_is_400() {
        let body = serde_json::json!({
            "fund_code":"161725","start":"2024-01-01","end":"2024-12-31",
            "buy_rate":0.0015,"initial_cash":0.0,"runs":[]
        });
        assert_eq!(post_json("/api/compare", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn optimize_missing_grid_param_is_400() {
        // smart_dca 缺 base_amount → build_optimize_cfg 在加载数据前即报错
        let body = serde_json::json!({
            "fund_code":"161725","start":"2024-01-01","end":"2024-12-31",
            "buy_rate":0.0015,"initial_cash":0.0,"strategy":"smart_dca","metric":"sharpe","top_n":5,
            "grid":{"period":"monthly","day":"1","ma_window":"120,250","k":"1.0"}
        });
        assert_eq!(post_json("/api/optimize", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn index_serves_form() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("name=\"fund_code\""), "应含基金代码输入");
        assert!(body.contains("id=\"result\""), "应含结果 iframe");
        assert!(body.contains("运行"), "应含运行按钮");
    }

    #[tokio::test]
    async fn index_has_three_tabs() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"single\""), "单次 tab");
        assert!(body.contains("data-tab=\"compare\""), "对比 tab");
        assert!(body.contains("data-tab=\"optimize\""), "寻优 tab");
        assert!(body.contains("/api/compare"), "对比提交端点");
        assert!(body.contains("/api/optimize"), "寻优提交端点");
        assert!(body.contains("id=\"result\""), "结果 iframe");
    }

    #[test]
    fn funds_payload_reads_cache() {
        use crate::data::fundlist::FundInfo;
        let dir = std::env::temp_dir().join("xlh_funds_payload_test");
        std::fs::create_dir_all(&dir).unwrap();
        let funds = vec![FundInfo { code: "161725".into(), name: "招商中证白酒指数".into(), pinyin: "ZSZZBJ".into() }];
        std::fs::write(dir.join("fundlist.json"), serde_json::to_string(&funds).unwrap()).unwrap();
        let got = super::funds_payload(&dir);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].code, "161725");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn index_has_sync_card() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("/api/sync"), "应有同步端点调用");
        assert!(body.contains("数据同步"), "应有同步卡片标题");
        assert!(body.contains("id=\"sync-result\""), "应有结果区");
    }

    #[test]
    fn index_has_adaptive_option() {
        let body = crate::web::page::INDEX_HTML;
        assert!(body.contains("value=\"adaptive\""), "index should have adaptive option");
        assert!(body.contains("自适应"), "index should show adaptive label");
    }

    #[tokio::test]
    async fn index_has_rsi_option() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("value=\"rsi\""), "策略下拉应含 rsi 选项");
        assert!(body.contains("RSI"), "应有 RSI 文案");
        assert!(body.contains("rsi_window"), "应有 rsi 参数字段");
    }

    #[tokio::test]
    async fn index_has_diagnose_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"diagnose\""), "应有诊断 tab");
        assert!(body.contains("/api/regime"), "应调用诊断接口");
        assert!(body.contains("id=\"diag-result\""), "应有诊断结果区");
        assert!(body.contains("不构成"), "应有免责声明");
    }

    #[tokio::test]
    async fn index_has_fund_combobox() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("/api/funds"), "应在加载时取基金清单");
        assert!(body.contains("attachCombobox"), "应有 combobox 挂载函数");
        assert!(body.contains("fund-dropdown"), "应有下拉容器样式/类");
    }

    #[tokio::test]
    async fn funds_route_returns_json_array() {
        // 路由存在且返回 JSON；用临时缓存避免联网
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/api/funds").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        // body 必须是合法 JSON 数组（空数组也可——降级场景）
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
    }

    #[test]
    fn build_strategy_from_fields_rsi_ok() {
        assert!(build_strategy_from_fields(&sf_rsi()).is_ok());
    }

    #[test]
    fn build_strategy_from_fields_rsi_missing_oversold() {
        let mut f = sf_rsi();
        f.oversold = None;
        let err = build_strategy_from_fields(&f).err().expect("应返回 Err");
        assert!(err.to_string().contains("oversold"), "应提示缺 oversold: {err}");
    }

    #[tokio::test]
    async fn regime_route_bad_code_is_400() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/api/regime?fund_code=bad!!code").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn sync_route_bad_code_returns_array() {
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let body = serde_json::json!({"code":"bad!!code"}).to_string();
        let resp = super::core_router()
            .oneshot(Request::builder().method("POST").uri("/api/sync")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
        assert_eq!(v.as_array().unwrap().len(), 1, "指定 code → 单元素");
        assert!(v[0]["error"].is_string(), "非法代码应带 error");
    }

    #[test]
    fn preset_pool_nonempty_and_valid_codes() {
        assert!(!super::PRESET_POOL.is_empty(), "精选池不应为空");
        for c in super::PRESET_POOL {
            assert!(super::validate_fund_code(c).is_ok(), "池内代码应合法: {c}");
        }
    }

    #[tokio::test]
    async fn index_has_recommend_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"recommend\""), "应有推荐 tab");
        assert!(body.contains("/api/recommend"), "应调用推荐接口");
        assert!(body.contains("id=\"rec-result\""), "应有推荐结果区");
        assert!(body.contains("综合评分"), "算法说明应含综合评分");
        assert!(body.contains("样本外"), "算法说明应含样本外");
        assert!(body.contains("不构成"), "应有免责声明");
    }

    #[tokio::test]
    async fn index_has_stock_tabs() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        for m in ["data-tab=\"s-diagnose\"", "data-tab=\"s-backtest\"", "data-tab=\"s-screen\"",
                  "/api/stock/diagnose", "/api/stock/run", "/api/stock/recommend", "/api/stock/search",
                  "id=\"sd-result\"", "id=\"sb-result\"", "id=\"ss-result\"", "attachStockCombobox"] {
            assert!(body.contains(m), "首页应含 {m}");
        }
    }

    #[tokio::test]
    async fn index_has_holdings_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        for m in ["data-tab=\"holdings\"", "持仓建议", "id=\"panel-holdings\"", "id=\"run-holdings\"",
                  "/api/holdings", "renderHoldings"] {
            assert!(body.contains(m), "首页应含 {m}");
        }
    }

    #[tokio::test]
    async fn index_has_push_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        for m in ["data-tab=\"push\"", "id=\"panel-push\"", "id=\"pu-test\"", "/api/push/config",
                  "collectPushConfig", "股票持仓"] {
            assert!(body.contains(m), "首页应含 {m}");
        }
    }

    #[tokio::test]
    async fn push_config_get_returns_json() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::core_router()
            .oneshot(Request::builder().uri("/api/push/config").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["schedule"]["cron"].is_string());
        assert!(v["channel"]["kind"].is_string());
    }

    #[tokio::test]
    async fn push_config_save_empty_webhook_is_400() {
        // webhook 为空 → validate 失败，且校验在写盘前，无副作用
        let body = serde_json::json!({
            "schedule": {"cron": "0 30 8 * * *"},
            "channel": {"kind": "feishu", "webhook": ""},
            "holdings": [{"code": "161725", "amount": 1000.0, "profit": 0.0}]
        });
        assert_eq!(post_json("/api/push/config", body).await, axum::http::StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn holdings_empty_returns_valid_report() {
        // 空持仓不触发任何加载 → 离线安全，返回合法空报告
        use axum::body::Body;
        use axum::http::{Request, header};
        use tower::ServiceExt;
        let body = serde_json::json!({"holdings": []}).to_string();
        let resp = super::core_router()
            .oneshot(Request::builder().method("POST").uri("/api/holdings")
                .header(header::CONTENT_TYPE, "application/json").body(Body::from(body)).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["summary"].is_object(), "应含 summary");
        assert!(v["advices"].as_array().unwrap().is_empty(), "空持仓 advices 为空");
        assert_eq!(v["summary"]["holding_count"], 0);
    }

    #[tokio::test]
    async fn holdings_save_requires_login() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        let state = crate::web::auth::AuthState::new(conn, Default::default());
        let app = super::router(state);
        let resp = app.oneshot(
            Request::builder().method("POST").uri("/api/holdings/save")
                .header("content-type", "application/json")
                .body(Body::from("{\"holdings\":[]}")).unwrap()
        ).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn holdings_save_then_history_for_active_user() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt;
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        let state = crate::web::auth::AuthState::new(conn, Default::default());
        // 造一个已激活用户 + 会话
        let token = "tok-hist".to_string();
        {
            let c = state.db.lock().unwrap();
            let uid = crate::web::auth::store::create_user(&c, "u", "h", false).unwrap();
            crate::web::auth::store::set_expiry(&c, uid, chrono::Local::now().date_naive() + chrono::Duration::days(30)).unwrap();
            let exp = chrono::Local::now().date_naive() + chrono::Duration::days(1);
            crate::web::auth::store::create_session(&c, &token, uid, exp).unwrap();
        }
        // 保存（空持仓，离线可跑，报告 advices 为空但仍成功保存）
        let app = super::router(state.clone());
        let save = app.oneshot(
            Request::builder().method("POST").uri("/api/holdings/save")
                .header("content-type", "application/json")
                .header("cookie", format!("xlh_session={token}"))
                .body(Body::from("{\"holdings\":[]}")).unwrap()
        ).await.unwrap();
        assert_eq!(save.status(), StatusCode::OK);
        // 列表应有 1 条
        let app2 = super::router(state);
        let list = app2.oneshot(
            Request::builder().uri("/api/holdings/history")
                .header("cookie", format!("xlh_session={token}"))
                .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(list.into_body(), 1_000_000).await.unwrap();
        let arr: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }

    #[test]
    fn build_optimize_cfg_rsi_grid() {
        let mut grid = std::collections::BTreeMap::new();
        grid.insert("rsi_window".into(), "14".into());
        grid.insert("oversold".into(), "25,30".into());
        grid.insert("overbought".into(), "70,75".into());
        grid.insert("amount".into(), "1000".into());
        let req = OptimizeRequest {
            fund_code: "161725".into(),
            start: NaiveDate::from_ymd_opt(2020,1,1).unwrap(),
            end: NaiveDate::from_ymd_opt(2024,12,31).unwrap(),
            buy_rate: 0.0015, initial_cash: 0.0,
            strategy: "rsi".into(), metric: "sharpe".into(), top_n: 5, grid,
        };
        let cfg = build_optimize_cfg(&req).unwrap();
        assert_eq!(cfg.grid.len(), 4);
        match cfg.grid.get("oversold").unwrap() {
            toml::Value::Array(a) => assert_eq!(a.len(), 2),
            _ => panic!("oversold 应为数组"),
        }
    }
}

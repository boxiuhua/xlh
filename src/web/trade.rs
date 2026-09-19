//! 交易 API(计划 4a):工单列表 / 确认 / 忽略 / 回填、被拦信号、概览。
//! Web 层不含业务规则(design decision 1):handler 只调用 `crate::trade` 的函数并映射错误。

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::broker::Fee;
use crate::event::Direction;
use crate::stock::fee::StockFee;
use crate::trade::actions::{
    self, CancelOutcome, ConfirmError, FillError, ManualOutcome, SubmitStrategyOutcome,
};
use crate::trade::admission::scorecard::{self, Scorecard};
use crate::trade::model::{
    Account, EvalJob, NewStrategy, Position, RiskRules, SignalSource, StrategyDef, StrategyStatus,
    Ticket, TicketStatus,
};
use crate::trade::quotes::{QuoteSource, TencentQuotes};
use crate::trade::ticket::{self as tk, Transition};
use crate::trade::{settings, store};
use crate::web::auth::config::AuthCfg;
use crate::web::auth::model::LicenseStatus;
use crate::web::auth::store as auth_store;
use crate::web::auth::{AuthState, CurrentUser};

/// 心跳存活阈值(秒):监听线程每轮都打心跳;评估线程空闲轮询 30 秒,另留慢任务余量。
const MONITOR_ALIVE_SECS: i64 = 120;
const EVAL_ALIVE_SECS: i64 = 300;
/// `done` 视图最多返回的工单数。
const DONE_LIMIT: usize = 100;
const REJECTED_DEFAULT: usize = 50;
const REJECTED_MAX: usize = 200;

pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    msg: String,
    extra: Option<serde_json::Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self {
            status,
            code,
            msg: msg.into(),
            extra: None,
        }
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", msg)
    }
    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "不存在")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.msg, "code": self.code });
        if let (Some(extra), Some(obj)) = (self.extra, body.as_object_mut()) {
            if let Some(e) = extra.as_object() {
                obj.extend(e.clone());
            }
        }
        (self.status, axum::Json(body)).into_response()
    }
}

/// 内部错误(数据库等):500,不把细节暴露给前端,只记日志。
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        eprintln!("[trade-api] {e:#}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "服务内部错误",
        )
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        Self::bad(format!("请求体格式错误: {}", e.body_text()))
    }
}

impl From<PathRejection> for ApiError {
    fn from(e: PathRejection) -> Self {
        Self::bad(format!("路径参数错误: {}", e.body_text()))
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        Self::bad(format!("查询参数错误: {}", e.body_text()))
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

fn ok() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

fn now() -> NaiveDateTime {
    chrono::Local::now().naive_local()
}

/// 交易 API 路由,挂在 `licensed` 组(需登录 + 授权)。
pub fn routes() -> Router<AuthState> {
    Router::new()
        .route("/api/trade/overview", get(overview))
        .route("/api/trade/tickets", get(list_tickets))
        .route("/api/trade/tickets/:id/confirm", post(confirm))
        .route("/api/trade/tickets/:id/ignore", post(ignore))
        .route("/api/trade/tickets/:id/fill", post(fill))
        .route("/api/trade/manual", post(manual_ticket))
        .route("/api/trade/signals/rejected", get(rejected_signals))
        .route("/api/trade/risk", get(get_risk).post(post_risk))
        .route("/api/trade/capital", post(set_capital))
        .route("/api/trade/positions", get(positions))
        .route("/api/trade/positions/exit-levels", post(exit_levels))
        .route("/api/trade/positions/calibrate", post(calibrate))
        .route("/api/trade/positions/adjusts", get(adjusts))
        .route(
            "/api/trade/strategies",
            get(list_strategies).post(create_strategy),
        )
        .route("/api/trade/strategies/:id", post(update_strategy))
        .route("/api/trade/strategies/:id/submit", post(submit_strategy))
        .route(
            "/api/trade/strategies/:id/scorecard",
            get(strategy_scorecard),
        )
        .route("/api/trade/strategies/:id/events", get(strategy_events))
        .route("/api/trade/jobs", get(list_jobs))
        .route("/api/trade/jobs/:id/cancel", post(cancel_job))
}

/// 工单签名链接路由(design decision 3):免登录查看与确认,挂在 `public` 组。
/// 工单不存在、签名缺失 / 错误 / 过期一律 404,不区分原因、不泄露工单是否存在。
pub fn public_routes() -> Router<AuthState> {
    Router::new()
        .route("/api/trade/t/:id", get(signed_view))
        .route("/api/trade/t/:id/confirm", post(signed_confirm))
}

// ===== 概览 =====

fn alive(conn: &Connection, name: &str, max_secs: i64, now: NaiveDateTime) -> anyhow::Result<bool> {
    Ok(store::last_beat(conn, name)?.is_some_and(|t| (now - t).num_seconds() <= max_secs))
}

async fn overview(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<serde_json::Value> {
    let now = now();
    let conn = st.db.lock().unwrap();
    let real = store::get_account(&conn, user.id, Account::Real)?;
    let paper = store::get_account(&conn, user.id, Account::Paper)?;
    let rules = store::get_risk_rules(&conn, user.id)?;
    Ok(Json(json!({
        "accounts": { "real": real, "paper": paper },
        "monitor_alive": alive(&conn, "trade-monitor", MONITOR_ALIVE_SECS, now)?,
        "eval_alive": alive(&conn, "trade-eval", EVAL_ALIVE_SECS, now)?,
        "kill_switch": settings::kill_switch(&conn)?,
        "trading_enabled": rules.enabled,
    })))
}

// ===== 工单列表 =====

#[derive(Deserialize)]
struct TicketsQuery {
    view: Option<String>,
}

#[derive(Serialize)]
struct QuoteView {
    price: f64,
    ts: NaiveDateTime,
    stale: bool,
}

#[derive(Serialize)]
struct TicketView {
    #[serde(flatten)]
    ticket: Ticket,
    source: Option<SignalSource>,
    reason: String,
    quote: Option<QuoteView>,
    deviation: Option<f64>,
    /// 预估金额:建议价 × 数量。
    est_amount: f64,
    /// 预估费用:买入 `buy_fee(est_amount)`,卖出 `sell_fee(qty, suggest_price, 0)`。
    est_fee: f64,
    /// 来自信号的 AI 备注。
    ai_note: Option<String>,
    /// 来自信号的来源策略。
    strategy_id: Option<i64>,
    /// 股票名称(来自信号,4c);手动信号未填名称时为 `None`。
    name: Option<String>,
    /// 服务端计算的工单剩余有效秒数,下限 0(4c:倒计时以服务端为准,不再由前端解析时间字符串)。
    expires_in_secs: i64,
}

fn ticket_view(conn: &Connection, t: Ticket, now: NaiveDateTime) -> anyhow::Result<TicketView> {
    // 一次查询取齐信号展示元数据(4c);信号缺失等内部错误应上抛为 500,而不是静默空字符串(4a 遗留)。
    let meta = store::signal_meta(conn, t.signal_id)?
        .ok_or_else(|| anyhow::anyhow!("信号 {} 不存在", t.signal_id))?;
    let q = store::get_quote(conn, &t.code)?;
    let deviation = q
        .as_ref()
        .map(|q| actions::deviation(q.price, t.suggest_price));
    let quote = q.map(|q| QuoteView {
        price: q.price,
        ts: q.ts,
        stale: actions::quote_is_stale(q.ts, now),
    });
    let est_amount = t.suggest_price * t.qty as f64;
    let fee_model = StockFee::a_share();
    let est_fee = match t.side {
        Direction::Buy => fee_model.buy_fee(est_amount),
        Direction::Sell => fee_model.sell_fee(t.qty as f64, t.suggest_price, 0),
    };
    let expires_in_secs = (t.expires_at - now).num_seconds().max(0);
    Ok(TicketView {
        source: Some(meta.source),
        reason: meta.reason,
        ai_note: meta.ai_note,
        strategy_id: meta.strategy_id,
        name: meta.name,
        expires_in_secs,
        ticket: t,
        quote,
        deviation,
        est_amount,
        est_fee,
    })
}

async fn list_tickets(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    q: Result<Query<TicketsQuery>, QueryRejection>,
) -> ApiResult<Vec<TicketView>> {
    let Query(q) = q?;
    let is_working = |t: &Ticket| {
        t.account == Account::Real
            && matches!(t.status, TicketStatus::Confirmed | TicketStatus::Partial)
    };
    let is_pending = |t: &Ticket| t.account == Account::Real && t.status == TicketStatus::Pending;
    let now = now();
    let conn = st.db.lock().unwrap();
    let tickets: Vec<Ticket> = match q.view.as_deref().unwrap_or("pending") {
        "pending" => tk::list_tickets(&conn, user.id, &[TicketStatus::Pending])?
            .into_iter()
            .filter(is_pending)
            .collect(),
        "working" => tk::list_tickets(
            &conn,
            user.id,
            &[TicketStatus::Confirmed, TicketStatus::Partial],
        )?
        .into_iter()
        .filter(is_working)
        .collect(),
        "done" => tk::list_done_tickets(&conn, user.id, DONE_LIMIT)?,
        other => {
            return Err(ApiError::bad(format!(
                "未知视图: {other}(可选 pending / working / done)"
            )))
        }
    };
    let views = tickets
        .into_iter()
        .map(|t| ticket_view(&conn, t, now))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Json(views))
}

// ===== 工单动作 =====

#[derive(Deserialize)]
struct ConfirmBody {
    #[serde(default)]
    ack_deviation: bool,
    /// 页面确认时按钮上显示的偏离(比例);服务端据此判断用户看到的是否仍是当前偏离(4b 终审 I1)。
    #[serde(default)]
    ack_max_deviation: Option<f64>,
}

impl ConfirmBody {
    /// 校验后的 `ack_max_deviation`:须有限且 ≥ 0,否则 400。
    fn ack_max(&self) -> Result<Option<f64>, ApiError> {
        match self.ack_max_deviation {
            Some(v) if !(v.is_finite() && v >= 0.0) => {
                Err(ApiError::bad("ack_max_deviation 须为不小于 0 的数"))
            }
            v => Ok(v),
        }
    }
}

/// `ConfirmError` → HTTP 响应,供登录态确认与签名链接确认共用(避免映射逻辑重复)。
fn confirm_response(res: Result<(), ConfirmError>) -> ApiResult<serde_json::Value> {
    match res {
        Ok(()) => Ok(ok()),
        Err(ConfirmError::NotFound) => Err(ApiError::not_found()),
        Err(ConfirmError::AlreadyHandled) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_handled",
            "工单已处理或已过期",
        )),
        Err(ConfirmError::KillSwitch) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "kill_switch",
            "管理员已暂停交易,暂不可确认",
        )),
        Err(ConfirmError::StaleQuote) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "stale_quote",
            "行情延迟或缺失,暂不可确认,请稍后重试",
        )),
        Err(ConfirmError::Deviation { price, deviation }) => {
            let mut e = ApiError::new(
                StatusCode::CONFLICT,
                "deviation",
                format!(
                    "现价 {price} 偏离建议价 {:.2}%,请确认后再提交",
                    deviation * 100.0
                ),
            );
            e.extra = Some(json!({ "price": price, "deviation": deviation }));
            Err(e)
        }
    }
}

async fn confirm(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ConfirmBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let ack_max = body.ack_max()?;
    let res = {
        let conn = st.db.lock().unwrap();
        actions::confirm_ticket(&conn, user.id, id, body.ack_deviation, ack_max, now())?
    };
    confirm_response(res)
}

#[derive(Deserialize)]
struct IgnoreBody {
    reason: String,
}

/// 工单是否属于该用户(跨用户视为不存在)。
fn owned(conn: &Connection, user_id: i64, id: i64) -> Result<Ticket, ApiError> {
    tk::get_ticket(conn, id)?
        .filter(|t| t.user_id == user_id)
        .ok_or_else(ApiError::not_found)
}

async fn ignore(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<IgnoreBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let reason = body.reason.trim();
    if reason.is_empty() {
        return Err(ApiError::bad("忽略原因不能为空"));
    }
    let conn = st.db.lock().unwrap();
    owned(&conn, user.id, id)?;
    match tk::ignore(&conn, user.id, id, reason)? {
        Transition::Applied => Ok(ok()),
        Transition::AlreadyHandled => Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_handled",
            "工单已处理,不可忽略",
        )),
    }
}

// ===== 签名链接(免登录) =====

#[derive(Deserialize)]
struct SigQuery {
    sig: Option<String>,
}

/// 取出签名合法的工单;工单不存在 / 签名缺失 / 错误 / 过期一律 404,不区分原因
/// (design decision 3)。数据库等内部错误仍通过 `?` 转为 500,不吞掉真实故障。
/// 链接绕过了登录,因此工单所属用户须仍满足登录 + 授权中间件的同一口径
/// (未禁用、未注销、授权状态放行),否则同样 404。
fn signed_ticket(
    conn: &Connection,
    cfg: &AuthCfg,
    id: i64,
    sig: Option<&str>,
    now: NaiveDateTime,
) -> Result<Ticket, ApiError> {
    let sig = sig.ok_or_else(ApiError::not_found)?;
    let t = tk::get_ticket(conn, id)?.ok_or_else(ApiError::not_found)?;
    let secret = settings::link_secret(conn)?;
    if !crate::trade::link::verify(&secret, &t, sig, now) {
        return Err(ApiError::not_found());
    }
    let owner = auth_store::find_user_by_id(conn, t.user_id)?.ok_or_else(ApiError::not_found)?;
    let license = LicenseStatus::of(owner.expires_at, now.date(), cfg.warn_days, cfg.grace_days);
    if owner.disabled || owner.cancelled || !license.allows_access() {
        return Err(ApiError::not_found());
    }
    Ok(t)
}

async fn signed_view(
    State(st): State<AuthState>,
    id: Result<Path<i64>, PathRejection>,
    q: Result<Query<SigQuery>, QueryRejection>,
) -> ApiResult<TicketView> {
    let Path(id) = id?;
    let Query(q) = q?;
    let now = now();
    let conn = st.db.lock().unwrap();
    let t = signed_ticket(&conn, &st.cfg, id, q.sig.as_deref(), now)?;
    Ok(Json(ticket_view(&conn, t, now)?))
}

/// 以工单自身的 `user_id` 作为操作用户;不需要登录(链接只能查看与确认该工单)。
async fn signed_confirm(
    State(st): State<AuthState>,
    id: Result<Path<i64>, PathRejection>,
    q: Result<Query<SigQuery>, QueryRejection>,
    body: Result<Json<ConfirmBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Query(q) = q?;
    let Json(body) = body?;
    let ack_max = body.ack_max()?;
    let now = now();
    let res = {
        let conn = st.db.lock().unwrap();
        let t = signed_ticket(&conn, &st.cfg, id, q.sig.as_deref(), now)?;
        actions::confirm_ticket(&conn, t.user_id, t.id, body.ack_deviation, ack_max, now)?
    };
    confirm_response(res)
}

#[derive(Deserialize)]
struct FillBody {
    price: f64,
    qty: u64,
}

/// 回填成交(仅实盘)。业务拒绝 → 404 / 409 / 400;其余错误为内部错误 → 500。
async fn fill(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<FillBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let res = {
        let mut conn = st.db.lock().unwrap();
        actions::manual_fill(&mut conn, user.id, id, body.price, body.qty, now())?
    };
    let out = match res {
        Ok(out) => out,
        Err(FillError::NotFound) => return Err(ApiError::not_found()),
        Err(FillError::PaperNotAllowed) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "paper_ticket",
                "模拟盘工单由系统撮合,不可人工回填",
            ))
        }
        Err(FillError::Validation(msg)) => return Err(ApiError::bad(msg)),
    };
    Ok(Json(json!({
        "ok": true,
        "status": out.status,
        "fee": out.fee,
        "realized_pnl": out.realized_pnl,
    })))
}

// ===== 手动 / AI 工单(计划 4c) =====

/// 手动 / AI 建议下单入口(design decision 1-3):与自动信号同一闸门,只在有今日行情
/// 时放行。先用缓存(≤60s)报价,锁外补拉腾讯今日快照,失败按无行情处理(不把网络
/// 错误变成 500)。
async fn manual_ticket(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<actions::ManualOrder>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(o) = body?;
    actions::validate_manual(&o).map_err(|e| ApiError::bad(e.to_string()))?;
    let now = now();
    // 1) 先看缓存(≤60s),命中则不联网。
    let cached = {
        let conn = st.db.lock().unwrap();
        store::fresh_quote(&conn, &o.code, now, actions::QUOTE_MAX_AGE_SECS)?
    };
    // 2) 未命中:锁外拉腾讯快照;失败按无行情处理。
    let quote = match cached {
        Some(q) => Some(q),
        None => {
            let code = o.code.clone();
            tokio::task::spawn_blocking(move || TencentQuotes.fetch(&[code]))
                .await
                .ok()
                .and_then(|r| r.ok())
                .and_then(|qs| qs.into_iter().find(|q| q.ts.date() == now.date()))
        }
    };
    // 3) 持锁提交;新鲜报价顺手写入缓存。
    let mut conn = st.db.lock().unwrap();
    if let Some(q) = &quote {
        store::upsert_quotes(&conn, std::slice::from_ref(q), now)?;
    }
    let outcome = actions::submit_manual(&mut conn, user.id, &o, quote.as_ref(), now)?;
    Ok(Json(match outcome {
        ManualOutcome::Ticketed {
            real_ticket,
            paper_ticket,
        } => {
            json!({ "result": "ticketed", "real_ticket": real_ticket, "paper_ticket": paper_ticket })
        }
        ManualOutcome::Duplicate => json!({ "result": "duplicate" }),
        ManualOutcome::Rejected(r) => {
            let mut e = ApiError::new(StatusCode::CONFLICT, "rejected", r.label_zh());
            e.extra = Some(json!({ "reason": r.as_str() }));
            return Err(e);
        }
        ManualOutcome::NoQuote => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "no_quote",
                "暂无今日行情(盘前、休市或停牌),无法生成工单",
            ))
        }
    }))
}

// ===== 被拦信号 =====

#[derive(Deserialize)]
struct RejectedQuery {
    limit: Option<usize>,
}

async fn rejected_signals(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    q: Result<Query<RejectedQuery>, QueryRejection>,
) -> ApiResult<Vec<store::RejectedSignal>> {
    let Query(q) = q?;
    let limit = q.limit.unwrap_or(REJECTED_DEFAULT).clamp(1, REJECTED_MAX);
    let conn = st.db.lock().unwrap();
    Ok(Json(store::list_rejected_signals(&conn, user.id, limit)?))
}

// ===== 资金、风控设置、持仓、止盈止损与持仓校准 =====

async fn get_risk(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<RiskRules> {
    let conn = st.db.lock().unwrap();
    Ok(Json(store::get_risk_rules(&conn, user.id)?))
}

async fn post_risk(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<RiskRules>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(rules) = body?;
    rules.validate().map_err(|e| ApiError::bad(e.to_string()))?;
    let conn = st.db.lock().unwrap();
    store::save_risk_rules(&conn, user.id, &rules, now())?;
    Ok(ok())
}

#[derive(Deserialize)]
struct CapitalBody {
    account: String,
    total: f64,
}

async fn set_capital(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CapitalBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(body) = body?;
    let account = Account::parse(&body.account).map_err(|e| ApiError::bad(e.to_string()))?;
    if !(body.total.is_finite() && body.total > 0.0) {
        return Err(ApiError::bad("账户总资金必须为正数"));
    }
    let conn = st.db.lock().unwrap();
    store::set_capital(&conn, user.id, account, body.total, now())?;
    Ok(ok())
}

#[derive(Serialize)]
struct PositionView {
    #[serde(flatten)]
    position: Position,
    sellable: u64,
    quote: Option<QuoteView>,
    market_value: Option<f64>,
    pnl_pct: Option<f64>,
}

fn position_view(
    conn: &Connection,
    p: Position,
    today: NaiveDate,
    now: NaiveDateTime,
) -> anyhow::Result<PositionView> {
    let sellable = p.sellable(today);
    let q = store::get_quote(conn, &p.code)?;
    let quote = q.as_ref().map(|q| QuoteView {
        price: q.price,
        ts: q.ts,
        stale: actions::quote_is_stale(q.ts, now),
    });
    let market_value = q.as_ref().map(|q| q.price * p.qty as f64);
    let pnl_pct = q.as_ref().map(|q| q.price / p.avg_cost - 1.0);
    Ok(PositionView {
        position: p,
        sellable,
        quote,
        market_value,
        pnl_pct,
    })
}

async fn positions(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<serde_json::Value> {
    let now = now();
    let today = now.date();
    let conn = st.db.lock().unwrap();
    let views = |account: Account| -> anyhow::Result<Vec<PositionView>> {
        store::list_positions(&conn, user.id, account)?
            .into_iter()
            .map(|p| position_view(&conn, p, today, now))
            .collect()
    };
    let real = views(Account::Real)?;
    let paper = views(Account::Paper)?;
    Ok(Json(json!({ "real": real, "paper": paper })))
}

#[derive(Deserialize)]
struct ExitLevelsBody {
    account: String,
    code: String,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
}

async fn exit_levels(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ExitLevelsBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(body) = body?;
    let account = Account::parse(&body.account).map_err(|e| ApiError::bad(e.to_string()))?;
    actions::validate_exit_levels(body.stop_loss, body.take_profit, body.trailing_pct)
        .map_err(|e| ApiError::bad(e.to_string()))?;
    let conn = st.db.lock().unwrap();
    let found = store::set_exit_levels(
        &conn,
        user.id,
        account,
        &body.code,
        body.stop_loss,
        body.take_profit,
        body.trailing_pct,
        now(),
    )?;
    if !found {
        return Err(ApiError::not_found());
    }
    Ok(ok())
}

#[derive(Deserialize)]
struct CalibrateBody {
    code: String,
    qty: u64,
    avg_cost: f64,
    reason: String,
}

async fn calibrate(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<CalibrateBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(body) = body?;
    let c = actions::Calibration {
        code: body.code,
        qty: body.qty,
        avg_cost: body.avg_cost,
        reason: body.reason,
    };
    actions::validate_calibration(&c).map_err(|e| ApiError::bad(e.to_string()))?;
    let mut conn = st.db.lock().unwrap();
    actions::calibrate_position(&mut conn, user.id, &c, now())?;
    Ok(ok())
}

/// `/api/trade/positions/adjusts` 最多返回的校准记录数。
const ADJUSTS_LIMIT: usize = 100;

async fn adjusts(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<Vec<store::PositionAdjust>> {
    let conn = st.db.lock().unwrap();
    Ok(Json(store::list_adjusts(&conn, user.id, ADJUSTS_LIMIT)?))
}

// ===== 策略管理、成绩单、评估任务 =====

#[derive(Deserialize)]
struct StrategyBody {
    name: String,
    kind: String,
    grid_toml: String,
    pool: Vec<String>,
}

impl StrategyBody {
    fn into_new(self, user_id: i64) -> NewStrategy {
        NewStrategy {
            user_id,
            name: self.name,
            kind: self.kind,
            grid_toml: self.grid_toml,
            pool: self.pool,
        }
    }
}

async fn list_strategies(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<Vec<StrategyDef>> {
    let conn = st.db.lock().unwrap();
    Ok(Json(store::list_strategies(&conn, user.id)?))
}

async fn create_strategy(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<StrategyBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(body) = body?;
    let s = body.into_new(user.id);
    store::validate_new_strategy(&s).map_err(|e| ApiError::bad(e.to_string()))?;
    let conn = st.db.lock().unwrap();
    let id = store::create_strategy(&conn, &s, now())?;
    Ok(Json(json!({ "id": id })))
}

async fn update_strategy(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<StrategyBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let s = body.into_new(user.id);
    store::validate_new_strategy(&s).map_err(|e| ApiError::bad(e.to_string()))?;
    let conn = st.db.lock().unwrap();
    let result = match store::update_definition(&conn, user.id, id, &s, now())? {
        store::DefinitionUpdate::NotFound => return Err(ApiError::not_found()),
        store::DefinitionUpdate::Unchanged => "unchanged",
        store::DefinitionUpdate::Renamed => "renamed",
        store::DefinitionUpdate::Reversioned => "reversioned",
    };
    Ok(Json(json!({ "result": result })))
}

async fn submit_strategy(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let conn = st.db.lock().unwrap();
    match actions::submit_strategy(&conn, user.id, id, now())? {
        SubmitStrategyOutcome::Queued { job_id } => {
            Ok(Json(json!({ "result": "queued", "job_id": job_id })))
        }
        SubmitStrategyOutcome::Paper => Ok(Json(json!({ "result": "paper" }))),
        SubmitStrategyOutcome::AlreadyHandled => Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_handled",
            "策略状态不允许提交",
        )),
        SubmitStrategyOutcome::NotFound => Err(ApiError::not_found()),
    }
}

async fn strategy_scorecard(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Scorecard> {
    let Path(id) = id?;
    let conn = st.db.lock().unwrap();
    let sc = scorecard::scorecard(&conn, user.id, id)?;
    sc.map(Json).ok_or_else(ApiError::not_found)
}

#[derive(Serialize)]
struct StatusEventView {
    from: StrategyStatus,
    to: StrategyStatus,
    reason: String,
    at: NaiveDateTime,
}

async fn strategy_events(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Vec<StatusEventView>> {
    let Path(id) = id?;
    let conn = st.db.lock().unwrap();
    if store::get_strategy(&conn, user.id, id)?.is_none() {
        return Err(ApiError::not_found());
    }
    let events = store::list_status_events(&conn, id, user.id)?
        .into_iter()
        .map(|(from, to, reason, at)| StatusEventView {
            from,
            to,
            reason,
            at,
        })
        .collect();
    Ok(Json(events))
}

/// `/api/trade/jobs` 最多返回的评估任务数(spec:最近 50 个)。
const JOBS_LIMIT: usize = 50;

async fn list_jobs(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
) -> ApiResult<Vec<EvalJob>> {
    let conn = st.db.lock().unwrap();
    Ok(Json(store::list_jobs(&conn, user.id, JOBS_LIMIT)?))
}

async fn cancel_job(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let conn = st.db.lock().unwrap();
    match actions::cancel_job(&conn, user.id, id, now())? {
        CancelOutcome::Cancelled => Ok(Json(json!({ "result": "cancelled" }))),
        CancelOutcome::Requested => Ok(Json(json!({ "result": "requested" }))),
        CancelOutcome::NotCancellable => Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_cancellable",
            "任务已结束,不可取消",
        )),
        CancelOutcome::NotFound => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state() -> AuthState {
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        crate::push::store::migrate(&conn).unwrap();
        crate::trade::store::migrate(&conn).unwrap();
        AuthState::new(conn, Default::default())
    }

    fn seed_user(st: &AuthState, name: &str, token: &str) -> i64 {
        let c = st.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, name, "h", false).unwrap();
        let today = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, today + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, token, uid, today + chrono::Duration::days(1))
            .unwrap();
        uid
    }

    /// 管理员账号(用于管理员总开关测试)。
    fn seed_admin(st: &AuthState, name: &str, token: &str) -> i64 {
        let c = st.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, name, "h", true).unwrap();
        let today = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, today + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, token, uid, today + chrono::Duration::days(1))
            .unwrap();
        uid
    }

    /// `token` 为空串时不带 cookie(匿名请求)。
    async fn call(
        st: &AuthState,
        method: &str,
        uri: &str,
        token: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if !token.is_empty() {
            req = req.header("cookie", format!("xlh_session={token}"));
        }
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = crate::web::router(st.clone())
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// 给用户造一张待确认的实盘手动买入工单(建议价 10.0),再把行情改成 `price_now`。
    /// handler 用真实时钟,所以这里的时间也取「现在」,行情才算新鲜。
    fn pending_ticket(st: &AuthState, uid: i64, code: &str, price_now: f64) -> i64 {
        use crate::event::Direction;
        use crate::trade::model::{Account, AccountScope, NewSignal, Quote, SignalSource};
        use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
        let now = chrono::Local::now().naive_local();
        let mut c = st.db.lock().unwrap();
        if crate::trade::store::get_account(&c, uid, Account::Real)
            .unwrap()
            .is_none()
        {
            crate::trade::store::set_capital(&c, uid, Account::Real, 100_000.0, now).unwrap();
        }
        let q = |price: f64| Quote {
            code: code.into(),
            price,
            limit_up: Some(11.0),
            limit_down: Some(9.0),
            ts: now,
        };
        let sig = NewSignal {
            user_id: uid,
            source: SignalSource::Manual,
            strategy_id: None,
            code: code.into(),
            name: Some("浦发银行".into()),
            side: Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: format!("web-test-{uid}-{code}"),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        let id = match submit_signal(
            &mut c,
            &sig,
            &SubmitContext {
                quote: Some(&q(10.0)),
                now,
            },
        )
        .unwrap()
        {
            SubmitOutcome::Ticketed {
                real_ticket: Some(id),
                ..
            } => id,
            other => panic!("{other:?}"),
        };
        crate::trade::store::upsert_quotes(&c, &[q(price_now)], now).unwrap();
        id
    }

    #[tokio::test]
    async fn pending_list_confirm_with_deviation_ack_then_fill() {
        let st = state();
        let uid = seed_user(&st, "u1", "t1");
        let id = pending_ticket(&st, uid, "600000", 10.3); // 偏离 3%
        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=pending", "t1", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(list[0]["deviation"].as_f64().unwrap() > 0.029);
        assert_eq!(list[0]["quote"]["stale"], false);
        assert_eq!(list[0]["source"], "manual");
        assert_eq!(list[0]["reason"], "测试");
        let qty0 = list[0]["qty"].as_u64().unwrap();
        assert!(
            (list[0]["est_amount"].as_f64().unwrap() - 10.0 * qty0 as f64).abs() < 1e-6,
            "est_amount ≈ 建议价 × 数量"
        );
        assert!(list[0]["est_fee"].as_f64().unwrap() > 0.0);
        assert!(list[0]["strategy_id"].is_null(), "手动信号无来源策略");
        assert!(
            list[0]["expires_in_secs"].as_i64().unwrap() > 0,
            "倒计时以服务端剩余秒数为准"
        );
        assert_eq!(list[0]["name"], "浦发银行");

        let url = format!("/api/trade/tickets/{id}/confirm");
        let (s, e) = call(&st, "POST", &url, "t1", Some(serde_json::json!({}))).await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("deviation"))
        );
        assert!(e["price"].as_f64().is_some() && e["deviation"].as_f64().is_some());
        let (s, _) = call(
            &st,
            "POST",
            &url,
            "t1",
            Some(serde_json::json!({"ack_deviation": true})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, e) = call(
            &st,
            "POST",
            &url,
            "t1",
            Some(serde_json::json!({"ack_deviation": true})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("already_handled"))
        );

        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=working", "t1", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
        let qty = list[0]["qty"].as_u64().unwrap();
        let (s, r) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/fill"),
            "t1",
            Some(serde_json::json!({"price": 10.3, "qty": qty})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(r["status"], "filled");
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/fill"),
            "t1",
            Some(serde_json::json!({"price": 10.3, "qty": 1})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "已成交不可再回填");

        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=done", "t1", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
        assert_eq!(list[0]["id"], id);
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .all(|t| !matches!(t["status"].as_str(), Some("pending") | Some("confirmed"))),
            "已完成视图不含 pending/confirmed 工单: {list}"
        );
    }

    #[tokio::test]
    async fn fill_rejects_paper_ticket_409_and_bad_input_400() {
        use crate::event::Direction;
        use crate::trade::model::{Account, AccountScope, NewSignal, Quote, SignalSource};
        use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let now = chrono::Local::now().naive_local();
        let paper = {
            let mut c = st.db.lock().unwrap();
            crate::trade::store::set_capital(&c, uid, Account::Paper, 100_000.0, now).unwrap();
            let q = Quote {
                code: "600000".into(),
                price: 10.0,
                limit_up: Some(11.0),
                limit_down: Some(9.0),
                ts: now,
            };
            let sig = NewSignal {
                user_id: uid,
                source: SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side: Direction::Buy,
                scope: AccountScope::PaperOnly,
                ref_price: 10.0,
                reason: "测试".into(),
                ai_note: None,
                dedup_key: "paper-fill".into(),
                suggest_cash: Some(5_000.0),
                suggest_qty: None,
            };
            match submit_signal(
                &mut c,
                &sig,
                &SubmitContext {
                    quote: Some(&q),
                    now,
                },
            )
            .unwrap()
            {
                SubmitOutcome::Ticketed {
                    paper_ticket: Some(id),
                    ..
                } => id,
                other => panic!("{other:?}"),
            }
        };
        let (s, e) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{paper}/fill"),
            "t",
            Some(serde_json::json!({"price": 10.0, "qty": 100})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("paper_ticket")),
            "模拟盘只由系统撮合"
        );

        let real = pending_ticket(&st, uid, "600001", 10.0);
        let url = format!("/api/trade/tickets/{real}/fill");
        let (s, e) = call(
            &st,
            "POST",
            &url,
            "t",
            Some(serde_json::json!({"price": 0.0, "qty": 100})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::BAD_REQUEST, Some("bad_request"))
        );
        let (s, _) = call(
            &st,
            "POST",
            &url,
            "t",
            Some(serde_json::json!({"price": 10.0, "qty": 100})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "待确认工单不可回填");
    }

    #[tokio::test]
    async fn other_users_ticket_is_not_found_and_anonymous_is_rejected() {
        let st = state();
        let a = seed_user(&st, "a", "ta");
        seed_user(&st, "b", "tb");
        let id = pending_ticket(&st, a, "600000", 10.0);
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/confirm"),
            "tb",
            Some(serde_json::json!({"ack_deviation": true})),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/ignore"),
            "tb",
            Some(serde_json::json!({"reason": "x"})),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/fill"),
            "tb",
            Some(serde_json::json!({"price": 10.0, "qty": 100})),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=pending", "tb", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 0));
        let (s, _) = call(&st, "GET", "/api/trade/tickets", "no-such-token", None).await;
        assert_ne!(s, StatusCode::OK, "未登录不可访问");
        let (s, _) = call(&st, "GET", "/api/trade/overview", "", None).await;
        assert_ne!(s, StatusCode::OK, "匿名不可访问");
    }

    #[tokio::test]
    async fn ignore_requires_a_reason_and_bad_view_is_400() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        let url = format!("/api/trade/tickets/{id}/ignore");
        let (s, e) = call(
            &st,
            "POST",
            &url,
            "t",
            Some(serde_json::json!({"reason": "  "})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert_eq!(e["code"], "bad_request");
        let (s, _) = call(
            &st,
            "POST",
            &url,
            "t",
            Some(serde_json::json!({"reason": "不看好"})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, e) = call(
            &st,
            "POST",
            &url,
            "t",
            Some(serde_json::json!({"reason": "再点一次"})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("already_handled"))
        );
        let (s, _) = call(&st, "GET", "/api/trade/tickets?view=nope", "t", None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn overview_reports_kill_switch_and_heartbeats() {
        let st = state();
        seed_user(&st, "u", "t");
        {
            let c = st.db.lock().unwrap();
            crate::trade::settings::set_kill_switch(
                &c,
                true,
                None,
                chrono::Local::now().naive_local(),
            )
            .unwrap();
            crate::trade::store::beat(&c, "trade-monitor", chrono::Local::now().naive_local())
                .unwrap();
        }
        let (s, o) = call(&st, "GET", "/api/trade/overview", "t", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(o["kill_switch"], true);
        assert_eq!(o["monitor_alive"], true);
        assert_eq!(o["eval_alive"], false);
        assert!(o["accounts"]["real"].is_null());
        assert!(o["accounts"]["paper"].is_null());
    }

    #[tokio::test]
    async fn confirm_is_blocked_by_kill_switch() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        {
            let c = st.db.lock().unwrap();
            crate::trade::settings::set_kill_switch(
                &c,
                true,
                None,
                chrono::Local::now().naive_local(),
            )
            .unwrap();
        }
        let (s, e) = call(
            &st,
            "POST",
            &format!("/api/trade/tickets/{id}/confirm"),
            "t",
            Some(serde_json::json!({"ack_deviation": true})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("kill_switch"))
        );
    }

    #[tokio::test]
    async fn risk_rules_roundtrip_and_validation() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, mut r) = call(&st, "GET", "/api/trade/risk", "t", None).await;
        assert_eq!(s, StatusCode::OK);
        r["max_order_amount"] = serde_json::json!(20000.0);
        let (s, _) = call(&st, "POST", "/api/trade/risk", "t", Some(r.clone())).await;
        assert_eq!(s, StatusCode::OK);
        let (_, back) = call(&st, "GET", "/api/trade/risk", "t", None).await;
        assert_eq!(back["max_order_amount"], 20000.0);
        r["max_position_pct"] = serde_json::json!(2.0);
        let (s, e) = call(&st, "POST", "/api/trade/risk", "t", Some(r)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    }

    #[tokio::test]
    async fn capital_positions_calibration_and_exit_levels() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, _) = call(
            &st,
            "POST",
            "/api/trade/capital",
            "t",
            Some(serde_json::json!({"account": "real", "total": 200000.0})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&st, "POST", "/api/trade/positions/calibrate", "t",
            Some(serde_json::json!({"code": "600000", "qty": 1000, "avg_cost": 10.0, "reason": "对账"}))).await;
        assert_eq!(s, StatusCode::OK);
        let (_, p) = call(&st, "GET", "/api/trade/positions", "t", None).await;
        assert_eq!(p["real"].as_array().unwrap().len(), 1);
        assert_eq!(p["real"][0]["qty"], 1000);
        let (s, _) = call(&st, "POST", "/api/trade/positions/exit-levels", "t",
            Some(serde_json::json!({"account": "real", "code": "600000", "stop_loss": 9.0, "take_profit": 12.0}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&st, "POST", "/api/trade/positions/exit-levels", "t",
            Some(serde_json::json!({"account": "real", "code": "600000", "stop_loss": 12.0, "take_profit": 9.0}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "止损须低于止盈");
        let (s, _) = call(
            &st,
            "POST",
            "/api/trade/positions/exit-levels",
            "t",
            Some(serde_json::json!({"account": "real", "code": "000001", "stop_loss": 9.0})),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, log) = call(&st, "GET", "/api/trade/positions/adjusts", "t", None).await;
        assert_eq!(log.as_array().unwrap().len(), 1);
        let (s, _) = call(
            &st,
            "POST",
            "/api/trade/capital",
            "t",
            Some(serde_json::json!({"account": "real", "total": -1.0})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rejected_signals_are_listed_per_user() {
        use crate::event::Direction;
        use crate::trade::gate::GateReject;
        use crate::trade::model::{Account, AccountScope, NewSignal, Quote, SignalSource};
        use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
        let st = state();
        let a = seed_user(&st, "a", "ta");
        seed_user(&st, "b", "tb");
        {
            let now = chrono::Local::now().naive_local();
            let mut c = st.db.lock().unwrap();
            crate::trade::store::set_capital(&c, a, Account::Real, 100_000.0, now).unwrap();
            crate::trade::settings::set_kill_switch(&c, true, None, now).unwrap();
            let q = Quote {
                code: "600000".into(),
                price: 10.0,
                limit_up: Some(11.0),
                limit_down: Some(9.0),
                ts: now,
            };
            let sig = NewSignal {
                user_id: a,
                source: SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side: Direction::Buy,
                scope: AccountScope::Both,
                ref_price: 10.0,
                reason: "手动买入".into(),
                ai_note: None,
                dedup_key: "web-test-rejected".into(),
                suggest_cash: Some(5_000.0),
                suggest_qty: None,
            };
            let out = submit_signal(
                &mut c,
                &sig,
                &SubmitContext {
                    quote: Some(&q),
                    now,
                },
            )
            .unwrap();
            assert!(
                matches!(
                    out,
                    SubmitOutcome::Rejected {
                        reason: GateReject::TradingDisabled,
                        ..
                    }
                ),
                "{out:?}"
            );
        }
        let (s, list) = call(&st, "GET", "/api/trade/signals/rejected", "ta", None).await;
        assert_eq!(s, StatusCode::OK);
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["reject_reason"], "trading_disabled");
        assert_eq!(arr[0]["source"], "manual");
        assert_eq!(arr[0]["code"], "600000");
        assert_eq!(arr[0]["side"], "buy");
        assert_eq!(arr[0]["reason"], "手动买入");

        let (s, list) = call(
            &st,
            "GET",
            "/api/trade/signals/rejected?limit=10",
            "tb",
            None,
        )
        .await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 0));
    }

    #[tokio::test]
    async fn strategy_lifecycle_create_submit_cancel_and_scorecard() {
        let st = state();
        seed_user(&st, "u", "t");
        seed_user(&st, "v", "tv");
        let body = serde_json::json!({
            "name": "趋势", "kind": "trend",
            "grid_toml": "short_window = [5]\nlong_window = [20]", "pool": ["600000"]
        });
        let (s, r) = call(
            &st,
            "POST",
            "/api/trade/strategies",
            "t",
            Some(body.clone()),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        let id = r["id"].as_i64().unwrap();
        let (s, _) = call(&st, "POST", "/api/trade/strategies", "t",
            Some(serde_json::json!({"name": "x", "kind": "nope", "grid_toml": "a = [1]", "pool": ["600000"]}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        let (s, r) = call(
            &st,
            "POST",
            &format!("/api/trade/strategies/{id}/submit"),
            "t",
            None,
        )
        .await;
        assert_eq!((s, r["result"].as_str()), (StatusCode::OK, Some("queued")));
        let job = r["job_id"].as_i64().unwrap();
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/strategies/{id}/submit"),
            "t",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT);

        let (_, jobs) = call(&st, "GET", "/api/trade/jobs", "t", None).await;
        assert_eq!(jobs.as_array().unwrap().len(), 1);
        let (_, jobs_v) = call(&st, "GET", "/api/trade/jobs", "tv", None).await;
        assert!(jobs_v.as_array().unwrap().is_empty(), "按用户隔离");
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/jobs/{job}/cancel"),
            "tv",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, r) = call(
            &st,
            "POST",
            &format!("/api/trade/jobs/{job}/cancel"),
            "t",
            None,
        )
        .await;
        assert_eq!(
            (s, r["result"].as_str()),
            (StatusCode::OK, Some("cancelled"))
        );

        let (s, sc) = call(
            &st,
            "GET",
            &format!("/api/trade/strategies/{id}/scorecard"),
            "t",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(sc["status"], "failed");
        let (s, _) = call(
            &st,
            "GET",
            &format!("/api/trade/strategies/{id}/scorecard"),
            "tv",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, ev) = call(
            &st,
            "GET",
            &format!("/api/trade/strategies/{id}/events"),
            "t",
            None,
        )
        .await;
        assert!(ev.as_array().unwrap().len() >= 2);

        let mut renamed = body.clone();
        renamed["name"] = serde_json::json!("趋势2");
        let (_, r) = call(
            &st,
            "POST",
            &format!("/api/trade/strategies/{id}"),
            "t",
            Some(renamed),
        )
        .await;
        assert_eq!(r["result"], "renamed");

        // 列表应能看到该用户的策略。
        let (s, list) = call(&st, "GET", "/api/trade/strategies", "t", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
        let (_, list_v) = call(&st, "GET", "/api/trade/strategies", "tv", None).await;
        assert!(list_v.as_array().unwrap().is_empty(), "按用户隔离");

        // 不存在的策略更新 → 404。
        let (s, _) = call(&st, "POST", "/api/trade/strategies/999999", "t", Some(body)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn signed_link_views_and_confirms_only_its_own_ticket() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        let other = pending_ticket(&st, uid, "600036", 10.0); // 同用户另一张
        let (sig, other_sig) = {
            let c = st.db.lock().unwrap();
            let secret = crate::trade::settings::link_secret(&c).unwrap();
            let t = crate::trade::ticket::get_ticket(&c, id).unwrap().unwrap();
            let o = crate::trade::ticket::get_ticket(&c, other)
                .unwrap()
                .unwrap();
            (
                crate::trade::link::sign(&secret, &t),
                crate::trade::link::sign(&secret, &o),
            )
        };
        // 不带 cookie
        let (s, v) = call(
            &st,
            "GET",
            &format!("/api/trade/t/{id}?sig={sig}"),
            "",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["id"], id);
        let (s, _) = call(
            &st,
            "GET",
            &format!("/api/trade/t/{id}?sig={other_sig}"),
            "",
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "别的工单的签名不能用");
        let (s, _) = call(&st, "GET", &format!("/api/trade/t/{id}"), "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(
            &st,
            "POST",
            &format!("/api/trade/t/{id}/confirm?sig={sig}"),
            "",
            Some(serde_json::json!({"ack_deviation": false})),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let (s, e) = call(
            &st,
            "POST",
            &format!("/api/trade/t/{id}/confirm?sig={sig}"),
            "",
            Some(serde_json::json!({"ack_deviation": false})),
        )
        .await;
        assert_eq!(
            (s, e["code"].as_str()),
            (StatusCode::CONFLICT, Some("already_handled")),
            "链接重放"
        );
    }

    #[tokio::test]
    async fn confirm_ack_is_bound_to_shown_deviation_and_validated() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.3); // 偏离 3%
        let sig = {
            let c = st.db.lock().unwrap();
            let secret = crate::trade::settings::link_secret(&c).unwrap();
            let t = crate::trade::ticket::get_ticket(&c, id).unwrap().unwrap();
            crate::trade::link::sign(&secret, &t)
        };
        let urls = [
            (format!("/api/trade/tickets/{id}/confirm"), "t"),
            (format!("/api/trade/t/{id}/confirm?sig={sig}"), ""),
        ];
        for (url, tok) in &urls {
            for bad in [serde_json::json!(-0.01), serde_json::json!(-1)] {
                let body = serde_json::json!({"ack_deviation": true, "ack_max_deviation": bad});
                let (s, e) = call(&st, "POST", url, tok, Some(body)).await;
                assert_eq!(
                    (s, e["code"].as_str()),
                    (StatusCode::BAD_REQUEST, Some("bad_request")),
                    "{url} {e}"
                );
            }
            // 用户只看到 1% → 现价 3% 超出容差 → 409 deviation,带当前偏离
            let body = serde_json::json!({"ack_deviation": true, "ack_max_deviation": 0.01});
            let (s, e) = call(&st, "POST", url, tok, Some(body)).await;
            assert_eq!(
                (s, e["code"].as_str()),
                (StatusCode::CONFLICT, Some("deviation")),
                "{url} {e}"
            );
            assert!((e["deviation"].as_f64().unwrap() - 0.03).abs() < 1e-6);
        }
        // 按当前偏离确认 → 通过
        let body = serde_json::json!({"ack_deviation": true, "ack_max_deviation": 0.03});
        let (s, e) = call(&st, "POST", &urls[1].0, "", Some(body)).await;
        assert_eq!(s, StatusCode::OK, "{e}");
    }

    #[tokio::test]
    async fn signed_link_is_404_when_owner_is_expired_or_disabled() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        let sig = {
            let c = st.db.lock().unwrap();
            let secret = crate::trade::settings::link_secret(&c).unwrap();
            let t = crate::trade::ticket::get_ticket(&c, id).unwrap().unwrap();
            crate::trade::link::sign(&secret, &t)
        };
        let view = format!("/api/trade/t/{id}?sig={sig}");
        let confirm = format!("/api/trade/t/{id}/confirm?sig={sig}");
        let body = || Some(serde_json::json!({"ack_deviation": true}));
        let (s, _) = call(&st, "GET", &view, "", None).await;
        assert_eq!(s, StatusCode::OK, "授权有效时可查看");

        // 授权过期(远超宽限期)→ 链接失效
        {
            let c = st.db.lock().unwrap();
            let long_ago = chrono::Local::now().date_naive() - chrono::Duration::days(365);
            crate::web::auth::store::set_expiry(&c, uid, long_ago).unwrap();
        }
        let (s, _) = call(&st, "GET", &view, "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "授权过期后签名链接不可查看");
        let (s, _) = call(&st, "POST", &confirm, "", body()).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "授权过期后签名链接不可确认");

        // 授权恢复但账号被禁用 → 同样失效
        {
            let c = st.db.lock().unwrap();
            let later = chrono::Local::now().date_naive() + chrono::Duration::days(30);
            crate::web::auth::store::set_expiry(&c, uid, later).unwrap();
            crate::web::auth::store::set_disabled(&c, uid, true).unwrap();
        }
        let (s, _) = call(&st, "GET", &view, "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "禁用后签名链接不可查看");
        let (s, _) = call(&st, "POST", &confirm, "", body()).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "禁用后签名链接不可确认");
        let status = {
            let c = st.db.lock().unwrap();
            crate::trade::ticket::get_ticket(&c, id)
                .unwrap()
                .unwrap()
                .status
        };
        assert_eq!(status, crate::trade::model::TicketStatus::Pending);
    }

    #[tokio::test]
    async fn kill_switch_admin_only() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, _) = call(
            &st,
            "POST",
            "/api/admin/trade/kill-switch",
            "t",
            Some(serde_json::json!({"on": true})),
        )
        .await;
        assert_ne!(s, StatusCode::OK, "普通用户不可操作");
        let (s, _) = call(&st, "GET", "/api/admin/trade/kill-switch", "t", None).await;
        assert_ne!(s, StatusCode::OK, "普通用户不可查看");

        let admin_id = seed_admin(&st, "root", "ta");
        let (s, r) = call(
            &st,
            "POST",
            "/api/admin/trade/kill-switch",
            "ta",
            Some(serde_json::json!({"on": true})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(r["ok"], true);
        let (s, r) = call(&st, "GET", "/api/admin/trade/kill-switch", "ta", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(r["on"], true);
        assert_eq!(r["by"], admin_id, "总开关留痕操作人");
        assert!(r["updated_at"].is_string(), "留痕时间");

        // 普通用户 overview 的 kill_switch 应为 true(全局开关)。
        let (_, ov) = call(&st, "GET", "/api/trade/overview", "t", None).await;
        assert_eq!(ov["kill_switch"], true);

        let (s, r) = call(
            &st,
            "POST",
            "/api/admin/trade/kill-switch",
            "ta",
            Some(serde_json::json!({"on": false})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{r}");
        let (_, r) = call(&st, "GET", "/api/admin/trade/kill-switch", "ta", None).await;
        assert_eq!(r["on"], false);
    }

    /// 手动 / AI 工单 API(计划 4c):报价缓存命中,不走联网分支;不要为联网分支写测试。
    #[tokio::test]
    async fn manual_ticket_api_creates_dedupes_rejects_and_validates() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        {
            let c = st.db.lock().unwrap();
            let now = chrono::Local::now().naive_local();
            crate::trade::store::set_capital(
                &c,
                uid,
                crate::trade::model::Account::Real,
                100_000.0,
                now,
            )
            .unwrap();
            crate::trade::store::upsert_quotes(
                &c,
                &[crate::trade::model::Quote {
                    code: "600000".into(),
                    price: 10.0,
                    limit_up: Some(11.0),
                    limit_down: Some(9.0),
                    ts: now,
                }],
                now,
            )
            .unwrap();
        }
        let body = serde_json::json!({
            "request_id": "abc-1", "code": "600000", "name": "浦发银行", "side": "buy",
            "amount": 5000.0, "reason": "看好", "ai_note": null
        });
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(body.clone())).await;
        assert_eq!(
            (s, r["result"].as_str()),
            (StatusCode::OK, Some("ticketed")),
            "{r}"
        );
        assert!(r["real_ticket"].is_i64());
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(body.clone())).await;
        assert_eq!(
            (s, r["result"].as_str()),
            (StatusCode::OK, Some("duplicate"))
        );

        let mut sell = body.clone();
        sell["request_id"] = serde_json::json!("abc-2");
        sell["side"] = serde_json::json!("sell");
        sell["amount"] = serde_json::Value::Null;
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(sell)).await;
        assert_eq!(
            (s, r["code"].as_str(), r["reason"].as_str()),
            (
                StatusCode::CONFLICT,
                Some("rejected"),
                Some("nothing_sellable")
            )
        );

        let mut bad = body.clone();
        bad["request_id"] = serde_json::json!("abc-3");
        bad["reason"] = serde_json::json!(" ");
        let (s, _) = call(&st, "POST", "/api/trade/manual", "t", Some(bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        let (s, _) = call(&st, "POST", "/api/trade/manual", "", Some(body)).await;
        assert_ne!(s, StatusCode::OK, "需登录");
    }

    /// 拦截原因中文文案一致性(计划 4c):`GateReject::label_zh` 与 `TRADE_HTML` 里
    /// `REJECT_REASONS` 的文案须逐一一致,不允许两处悄悄走样。
    #[test]
    fn gate_reject_label_zh_matches_trade_html_reject_reasons() {
        use crate::trade::gate::GateReject;
        let html = crate::web::trade_page::TRADE_HTML;
        for r in GateReject::ALL {
            assert!(
                html.contains(r.label_zh()),
                "TRADE_HTML 缺少 {:?} 的中文文案 {}",
                r,
                r.label_zh()
            );
        }
    }
}

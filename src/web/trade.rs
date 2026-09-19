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

use crate::trade::actions::{self, ConfirmError};
use crate::trade::model::{Account, Position, RiskRules, SignalSource, Ticket, TicketStatus};
use crate::trade::ticket::{self as tk, Transition};
use crate::trade::{notify, settings, store};
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
        .route("/api/trade/signals/rejected", get(rejected_signals))
        .route("/api/trade/risk", get(get_risk).post(post_risk))
        .route("/api/trade/capital", post(set_capital))
        .route("/api/trade/positions", get(positions))
        .route("/api/trade/positions/exit-levels", post(exit_levels))
        .route("/api/trade/positions/calibrate", post(calibrate))
        .route("/api/trade/positions/adjusts", get(adjusts))
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
}

fn ticket_view(conn: &Connection, t: Ticket, now: NaiveDateTime) -> anyhow::Result<TicketView> {
    let source = store::signal_source(conn, t.signal_id)?;
    let reason = notify::signal_reason(conn, t.signal_id).unwrap_or_default();
    let q = store::get_quote(conn, &t.code)?;
    let deviation = q
        .as_ref()
        .map(|q| actions::deviation(q.price, t.suggest_price));
    let quote = q.map(|q| QuoteView {
        price: q.price,
        ts: q.ts,
        stale: actions::quote_is_stale(q.ts, now),
    });
    Ok(TicketView {
        ticket: t,
        source,
        reason,
        quote,
        deviation,
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
        "done" => tk::list_tickets(&conn, user.id, &[])?
            .into_iter()
            .rev()
            .filter(|t| !is_pending(t) && !is_working(t))
            .take(DONE_LIMIT)
            .collect(),
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
}

async fn confirm(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<ConfirmBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let res = {
        let conn = st.db.lock().unwrap();
        actions::confirm_ticket(&conn, user.id, id, body.ack_deviation, now())?
    };
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

#[derive(Deserialize)]
struct FillBody {
    price: f64,
    qty: u64,
}

/// 回填成交。总开关不限制回填(design decision 4:已发生的事实必须能记)。
async fn fill(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    id: Result<Path<i64>, PathRejection>,
    body: Result<Json<FillBody>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Path(id) = id?;
    let Json(body) = body?;
    let mut conn = st.db.lock().unwrap();
    owned(&conn, user.id, id)?;
    let out = tk::record_fill(
        &mut conn,
        user.id,
        id,
        body.price,
        body.qty,
        "manual",
        now(),
    )
    .map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(Json(json!({
        "ok": true,
        "status": out.status,
        "fee": out.fee,
        "realized_pnl": out.realized_pnl,
    })))
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
            name: None,
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
            crate::trade::settings::set_kill_switch(&c, true, chrono::Local::now().naive_local())
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
            crate::trade::settings::set_kill_switch(&c, true, chrono::Local::now().naive_local())
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
            crate::trade::settings::set_kill_switch(&c, true, now).unwrap();
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
}

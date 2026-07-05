pub mod admin;
pub mod cli;
pub mod config;
pub mod handlers;
pub mod model;
pub mod password;
pub mod routes;
pub mod session;
pub mod store;

use std::sync::{Arc, Mutex};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use config::AuthCfg;
use model::{LicenseStatus, User};

#[derive(Clone)]
pub struct AuthState {
    pub db: Arc<Mutex<Connection>>,
    pub cfg: AuthCfg,
}

impl AuthState {
    pub fn new(conn: Connection, cfg: AuthCfg) -> Self {
        AuthState { db: Arc::new(Mutex::new(conn)), cfg }
    }
}

#[derive(Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub expires_at: Option<NaiveDate>,
    pub disabled: bool,
}

impl From<User> for CurrentUser {
    fn from(u: User) -> Self {
        CurrentUser { id: u.id, username: u.username, is_admin: u.is_admin, expires_at: u.expires_at, disabled: u.disabled }
    }
}

pub fn json_error(code: StatusCode, err: &str, status: Option<LicenseStatus>) -> Response {
    let body = json!({ "error": err, "status": status });
    (code, Json(body)).into_response()
}

/// 拦截：必须有有效会话，否则 401。通过则注入 CurrentUser。
pub async fn require_login(State(st): State<AuthState>, mut req: Request, next: Next) -> Response {
    let token = session::read_cookie(req.headers());
    let now = chrono::Local::now().date_naive();
    let user = match token {
        Some(t) => {
            let conn = st.db.lock().unwrap();
            store::lookup_session_user(&conn, &t, now).ok().flatten()
        }
        None => None,
    };
    match user {
        Some(u) if !u.disabled => {
            req.extensions_mut().insert(CurrentUser::from(u));
            next.run(req).await
        }
        _ => json_error(StatusCode::UNAUTHORIZED, "unauthorized", None),
    }
}

/// 在 require_login 之后运行：授权状态须放行，否则 403。
pub async fn require_license(Extension(user): Extension<CurrentUser>, State(st): State<AuthState>, req: Request, next: Next) -> Response {
    let now = chrono::Local::now().date_naive();
    let status = LicenseStatus::of(user.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
    if user.disabled || !status.allows_access() {
        let err = if user.expires_at.is_none() { "license_required" } else { "expired" };
        return json_error(StatusCode::FORBIDDEN, err, Some(status));
    }
    next.run(req).await
}

/// 在 require_login 之后运行：非管理员一律 404（不暴露后台存在）。
pub async fn require_admin(Extension(user): Extension<CurrentUser>, req: Request, next: Next) -> Response {
    if !user.is_admin {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    next.run(req).await
}

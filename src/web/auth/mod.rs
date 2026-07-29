pub mod admin;
pub mod cli;
pub mod config;
pub mod handlers;
pub mod model;
pub mod password;
pub mod routes;
pub mod session;
pub mod store;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use config::AuthCfg;
use model::{LicenseStatus, User};

#[derive(Clone)]
pub struct AuthState {
    pub db: Arc<Mutex<Connection>>,
    pub cfg: AuthCfg,
    rate_limiter: Arc<Mutex<AuthRateLimiter>>,
}

impl AuthState {
    pub fn new(conn: Connection, cfg: AuthCfg) -> Self {
        AuthState {
            db: Arc::new(Mutex::new(conn)),
            cfg,
            rate_limiter: Default::default(),
        }
    }

    /// 同一身份在窗口内连续失败时短暂拒绝，避免 Argon2 被暴力请求耗尽。
    pub fn auth_attempt_allowed(&self, key: &str) -> bool {
        self.rate_limiter
            .lock()
            .map(|mut l| l.allowed(key))
            .unwrap_or(false)
    }

    pub fn auth_attempt_failed(&self, key: &str) {
        if let Ok(mut l) = self.rate_limiter.lock() {
            l.failed(key);
        }
    }

    pub fn auth_attempt_succeeded(&self, key: &str) {
        if let Ok(mut l) = self.rate_limiter.lock() {
            l.succeeded(key);
        }
    }
}

const AUTH_WINDOW: Duration = Duration::from_secs(10 * 60);
const AUTH_MAX_FAILURES: u32 = 8;

#[derive(Default)]
struct AuthRateLimiter {
    attempts: HashMap<String, (u32, Instant)>,
}

impl AuthRateLimiter {
    fn allowed(&mut self, key: &str) -> bool {
        self.attempts
            .retain(|_, (_, since)| since.elapsed() < AUTH_WINDOW);
        self.attempts
            .get(key)
            .map(|(n, _)| *n < AUTH_MAX_FAILURES)
            .unwrap_or(true)
    }
    fn failed(&mut self, key: &str) {
        let entry = self
            .attempts
            .entry(key.to_owned())
            .or_insert((0, Instant::now()));
        if entry.1.elapsed() >= AUTH_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }
    fn succeeded(&mut self, key: &str) {
        self.attempts.remove(key);
    }
}

#[cfg(test)]
mod rate_limit_tests {
    use super::*;

    #[test]
    fn blocks_after_limit_and_clears_on_success() {
        let mut l = AuthRateLimiter::default();
        for _ in 0..AUTH_MAX_FAILURES {
            l.failed("alice");
        }
        assert!(!l.allowed("alice"));
        l.succeeded("alice");
        assert!(l.allowed("alice"));
    }
}

#[derive(Clone)]
pub struct CurrentUser {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub expires_at: Option<NaiveDate>,
    pub disabled: bool,
    pub cancelled: bool,
}

impl From<User> for CurrentUser {
    fn from(u: User) -> Self {
        CurrentUser {
            id: u.id,
            username: u.username,
            is_admin: u.is_admin,
            expires_at: u.expires_at,
            disabled: u.disabled,
            cancelled: u.cancelled,
        }
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
        Some(u) if !u.disabled && !u.cancelled => {
            req.extensions_mut().insert(CurrentUser::from(u));
            next.run(req).await
        }
        _ => json_error(StatusCode::UNAUTHORIZED, "unauthorized", None),
    }
}

/// 在 require_login 之后运行：授权状态须放行，否则 403。
pub async fn require_license(
    Extension(user): Extension<CurrentUser>,
    State(st): State<AuthState>,
    req: Request,
    next: Next,
) -> Response {
    let now = chrono::Local::now().date_naive();
    let status = LicenseStatus::of(user.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
    if user.disabled || !status.allows_access() {
        let err = if user.expires_at.is_none() {
            "license_required"
        } else {
            "expired"
        };
        return json_error(StatusCode::FORBIDDEN, err, Some(status));
    }
    next.run(req).await
}

/// 在 require_login 之后运行：非管理员一律 404（不暴露后台存在）。
pub async fn require_admin(
    Extension(user): Extension<CurrentUser>,
    req: Request,
    next: Next,
) -> Response {
    if !user.is_admin {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    }
    next.run(req).await
}

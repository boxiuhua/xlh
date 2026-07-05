use axum::extract::State;
use axum::http::{header::SET_COOKIE, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::json;

use super::model::LicenseStatus;
use super::{json_error, session, store, AuthState, CurrentUser};

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ActivateReq {
    pub code: String,
}

fn valid_username(u: &str) -> bool {
    let n = u.chars().count();
    (3..=32).contains(&n) && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub async fn register(State(st): State<AuthState>, Json(cred): Json<Credentials>) -> Response {
    if !st.cfg.open_registration {
        return json_error(StatusCode::FORBIDDEN, "registration_closed", None);
    }
    if !valid_username(&cred.username) || cred.password.chars().count() < 6 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_credentials", None);
    }
    let hash = match super::password::hash(&cred.password) {
        Ok(h) => h,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "hash_failed", None),
    };
    let conn = st.db.lock().unwrap();
    match store::create_user(&conn, &cred.username, &hash, false) {
        Ok(_) => (StatusCode::OK, Json(json!({"ok": true}))).into_response(),
        Err(_) => json_error(StatusCode::CONFLICT, "username_taken", None),
    }
}

pub async fn login(State(st): State<AuthState>, Json(cred): Json<Credentials>) -> Response {
    let conn = st.db.lock().unwrap();
    let found = store::find_user_by_name(&conn, &cred.username).ok().flatten();
    // 统一失败文案，避免用户名枚举
    let (uid, hash, user) = match found {
        Some(t) => t,
        None => return json_error(StatusCode::UNAUTHORIZED, "invalid_login", None),
    };
    if user.disabled || !super::password::verify(&cred.password, &hash) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid_login", None);
    }
    let token = session::new_token();
    let exp = chrono::Local::now().date_naive() + chrono::Duration::days(st.cfg.session_ttl_days);
    if store::create_session(&conn, &token, uid, exp).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "session_failed", None);
    }
    let mut headers = HeaderMap::new();
    headers.insert(SET_COOKIE, session::set_cookie_header(&token, st.cfg.session_ttl_days).parse().unwrap());
    (StatusCode::OK, headers, Json(json!({"ok": true}))).into_response()
}

pub async fn logout(State(st): State<AuthState>, headers: HeaderMap) -> Response {
    if let Some(token) = session::read_cookie(&headers) {
        let conn = st.db.lock().unwrap();
        let _ = store::delete_session(&conn, &token);
    }
    let mut out = HeaderMap::new();
    out.insert(SET_COOKIE, session::clear_cookie_header().parse().unwrap());
    (StatusCode::OK, out, Json(json!({"ok": true}))).into_response()
}

pub async fn activate(State(st): State<AuthState>, Extension(user): Extension<CurrentUser>, Json(req): Json<ActivateReq>) -> Response {
    let mut conn = st.db.lock().unwrap();
    match store::activate(&mut conn, req.code.trim(), user.id) {
        Ok(new_exp) => {
            let now = chrono::Local::now().date_naive();
            let status = LicenseStatus::of(Some(new_exp), now, st.cfg.warn_days, st.cfg.grace_days);
            (StatusCode::OK, Json(json!({"ok": true, "expires_at": new_exp, "status": status}))).into_response()
        }
        Err(store::ActivateError::NotFound) => json_error(StatusCode::BAD_REQUEST, "code_not_found", None),
        Err(store::ActivateError::AlreadyUsed) => json_error(StatusCode::BAD_REQUEST, "code_used", None),
        Err(store::ActivateError::Revoked) => json_error(StatusCode::BAD_REQUEST, "code_revoked", None),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "activate_failed", None),
    }
}

pub async fn me(State(st): State<AuthState>, Extension(user): Extension<CurrentUser>) -> Response {
    let now = chrono::Local::now().date_naive();
    let status = LicenseStatus::of(user.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
    let remaining = user.expires_at.map(|e| (e - now).num_days());
    Json(json!({
        "username": user.username,
        "is_admin": user.is_admin,
        "expires_at": user.expires_at,
        "status": status,
        "warn_days": st.cfg.warn_days,
        "grace_days": st.cfg.grace_days,
        "remaining_days": remaining,
    })).into_response()
}

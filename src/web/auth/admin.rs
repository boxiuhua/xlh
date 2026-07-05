use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rand::Rng;
use serde::Deserialize;
use serde_json::json;

use super::model::{renew_expiry, LicenseStatus};
use super::store::{self, CodeFilter};
use super::{json_error, AuthState};

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // 去掉易混 O0I1

pub fn gen_code() -> String {
    let mut rng = rand::thread_rng();
    let raw: String = (0..16).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect();
    format!("{}-{}-{}-{}", &raw[0..4], &raw[4..8], &raw[8..12], &raw[12..16])
}

#[derive(Deserialize)]
pub struct CreateCodes { pub days: i64, pub count: u32 }

pub async fn create_codes(State(st): State<AuthState>, Json(req): Json<CreateCodes>) -> Response {
    if req.days <= 0 || req.count == 0 || req.count > 500 {
        return json_error(StatusCode::BAD_REQUEST, "invalid_params", None);
    }
    let conn = st.db.lock().unwrap();
    let mut codes = Vec::new();
    for _ in 0..req.count {
        let code = gen_code();
        if store::issue_code(&conn, &code, req.days).is_ok() {
            codes.push(code);
        }
    }
    (StatusCode::OK, Json(json!({"codes": codes}))).into_response()
}

#[derive(Deserialize)]
pub struct CodesQuery { #[serde(default)] pub filter: Option<String> }

pub async fn list_codes(State(st): State<AuthState>, Query(q): Query<CodesQuery>) -> Response {
    let filter = match q.filter.as_deref() {
        Some("used") => CodeFilter::Used,
        Some("all") => CodeFilter::All,
        _ => CodeFilter::Unused,
    };
    let conn = st.db.lock().unwrap();
    match store::list_codes(&conn, filter) {
        Ok(rows) => Json(rows).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "list_failed", None),
    }
}

#[derive(Deserialize)]
pub struct CodeReq { pub code: String }

pub async fn revoke_code(State(st): State<AuthState>, Json(req): Json<CodeReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let hit = store::revoke_code(&conn, &req.code).unwrap_or(false);
    Json(json!({"ok": hit})).into_response()
}

pub async fn list_users(State(st): State<AuthState>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    match store::list_users(&conn) {
        Ok(users) => {
            let rows: Vec<_> = users.into_iter().map(|u| {
                let status = LicenseStatus::of(u.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
                json!({
                    "id": u.id, "username": u.username, "expires_at": u.expires_at,
                    "is_admin": u.is_admin, "disabled": u.disabled, "status": status,
                })
            }).collect();
            Json(json!({"users": rows})).into_response()
        }
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "list_failed", None),
    }
}

#[derive(Deserialize)]
pub struct ExtendReq { pub user_id: i64, pub days: i64 }

pub async fn extend_user(State(st): State<AuthState>, Json(req): Json<ExtendReq>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    let cur = match store::find_user_by_id(&conn, req.user_id) {
        Ok(Some(u)) => u.expires_at,
        _ => return json_error(StatusCode::NOT_FOUND, "user_not_found", None),
    };
    let new_exp = renew_expiry(cur, now, req.days);
    if store::set_expiry(&conn, req.user_id, new_exp).is_err() {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None);
    }
    Json(json!({"ok": true, "expires_at": new_exp})).into_response()
}

#[derive(Deserialize)]
pub struct DisableReq { pub user_id: i64, pub disabled: bool }

pub async fn disable_user(State(st): State<AuthState>, Json(req): Json<DisableReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let _ = store::set_disabled(&conn, req.user_id, req.disabled);
    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub struct SetAdminReq { pub user_id: i64, pub is_admin: bool }

pub async fn set_admin(State(st): State<AuthState>, Json(req): Json<SetAdminReq>) -> Response {
    let conn = st.db.lock().unwrap();
    let _ = store::set_admin(&conn, req.user_id, req.is_admin);
    Json(json!({"ok": true})).into_response()
}

/// 后台页占位 handler（Task 14 会替换为真页面）。
pub async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html("<!doctype html><title>管理后台</title><p>admin placeholder</p>")
}

pub async fn overview(State(st): State<AuthState>) -> Response {
    let now = chrono::Local::now().date_naive();
    let conn = st.db.lock().unwrap();
    let users = store::list_users(&conn).unwrap_or_default();
    let total = users.len();
    let mut active = 0;
    let mut warning = 0;
    for u in &users {
        let s = LicenseStatus::of(u.expires_at, now, st.cfg.warn_days, st.cfg.grace_days);
        if s.allows_access() { active += 1; }
        if s == LicenseStatus::Warning { warning += 1; }
    }
    Json(json!({"total": total, "active": active, "warning": warning})).into_response()
}

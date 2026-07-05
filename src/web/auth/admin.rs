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
    // 不允许封禁最后一个启用中的管理员，否则无人能进 /admin。
    if req.disabled {
        if let Ok(Some(u)) = store::find_user_by_id(&conn, req.user_id) {
            if u.is_admin && !u.disabled && store::count_admins(&conn).unwrap_or(0) <= 1 {
                return json_error(StatusCode::BAD_REQUEST, "last_admin", None);
            }
        }
    }
    match store::set_disabled(&conn, req.user_id, req.disabled) {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None),
    }
}

#[derive(Deserialize)]
pub struct SetAdminReq { pub user_id: i64, pub is_admin: bool }

pub async fn set_admin(State(st): State<AuthState>, Json(req): Json<SetAdminReq>) -> Response {
    let conn = st.db.lock().unwrap();
    // 不允许撤销最后一个管理员，否则永久锁死后台。
    if !req.is_admin {
        if let Ok(Some(u)) = store::find_user_by_id(&conn, req.user_id) {
            if u.is_admin && store::count_admins(&conn).unwrap_or(0) <= 1 {
                return json_error(StatusCode::BAD_REQUEST, "last_admin", None);
            }
        }
    }
    match store::set_admin(&conn, req.user_id, req.is_admin) {
        Ok(_) => Json(json!({"ok": true})).into_response(),
        Err(_) => json_error(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", None),
    }
}

pub async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(ADMIN_HTML)
}

const ADMIN_HTML: &str = r##"<!doctype html>
<html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>xlh · 管理后台</title>
<style>
 body{font-family:system-ui,sans-serif;background:#0f172a;color:#e2e8f0;margin:0;padding:20px}
 h1{font-size:20px} h2{font-size:16px;margin-top:28px;border-bottom:1px solid #334155;padding-bottom:6px}
 table{width:100%;border-collapse:collapse;margin-top:10px;font-size:13px} th,td{text-align:left;padding:6px 8px;border-bottom:1px solid #1e293b}
 input,button{padding:6px 10px;border-radius:6px;border:1px solid #334155;background:#0b1220;color:#e2e8f0}
 button{background:#3b82f6;border:0;color:#fff;cursor:pointer;margin-left:4px}
 code{background:#1e293b;padding:2px 6px;border-radius:4px}
 a{color:#93c5fd}
</style></head><body>
<h1>xlh 管理后台 · <a href="/">返回主界面</a></h1>
<div id="ov"></div>

<h2>发码</h2>
<div>天数 <input id="days" type="number" value="365" style="width:90px"> 数量 <input id="count" type="number" value="1" style="width:70px">
<button onclick="gen()">生成</button></div>
<pre id="newcodes" style="background:#1e293b;padding:10px;border-radius:8px;white-space:pre-wrap"></pre>
<div><button onclick="loadCodes('unused')">未用</button><button onclick="loadCodes('used')">已用</button><button onclick="loadCodes('all')">全部</button></div>
<table id="codes"><thead><tr><th>码</th><th>天数</th><th>使用者</th><th>状态</th><th></th></tr></thead><tbody></tbody></table>

<h2>用户</h2>
<table id="users"><thead><tr><th>ID</th><th>用户名</th><th>状态</th><th>到期</th><th>操作</th></tr></thead><tbody></tbody></table>

<script>
async function api(u,m,b){const r=await fetch(u,{method:m||'GET',headers:b?{'content-type':'application/json'}:{},body:b?JSON.stringify(b):undefined});if(r.status===404){document.body.innerHTML='<h1>403</h1>';return null;}return r.json().catch(()=>({}));}
async function ov(){const j=await api('/api/admin/overview');if(j)document.getElementById('ov').textContent=`用户 ${j.total} · 在用 ${j.active} · 临期 ${j.warning}`;}
async function gen(){const days=+document.getElementById('days').value,count=+document.getElementById('count').value;const j=await api('/api/admin/codes','POST',{days,count});document.getElementById('newcodes').textContent=(j.codes||[]).join('\n');loadCodes('unused');}
async function loadCodes(f){const j=await api('/api/admin/codes?filter='+f);const tb=document.querySelector('#codes tbody');tb.innerHTML='';(j||[]).forEach(c=>{const st=c.revoked?'已作废':(c.used_by?'已用':'未用');tb.innerHTML+=`<tr><td><code>${c.code}</code></td><td>${c.days}</td><td>${c.used_by||''}</td><td>${st}</td><td>${c.used_by||c.revoked?'':`<button onclick="revoke('${c.code}')">作废</button>`}</td></tr>`;});}
async function revoke(code){await api('/api/admin/codes/revoke','POST',{code});loadCodes('unused');}
async function loadUsers(){const j=await api('/api/admin/users');const tb=document.querySelector('#users tbody');tb.innerHTML='';(j.users||[]).forEach(u=>{tb.innerHTML+=`<tr><td>${u.id}</td><td>${u.username}${u.is_admin?' 👑':''}</td><td>${u.status}${u.disabled?' (封禁)':''}</td><td>${u.expires_at||'—'}</td><td>
  <input type="number" value="30" style="width:64px" id="d${u.id}"><button onclick="ext(${u.id})">续期</button>
  <button onclick="dis(${u.id},${!u.disabled})">${u.disabled?'解封':'封禁'}</button>
  <button onclick="adm(${u.id},${!u.is_admin})">${u.is_admin?'撤管理':'设管理'}</button></td></tr>`;});}
async function ext(id){const days=+document.getElementById('d'+id).value;await api('/api/admin/users/extend','POST',{user_id:id,days});loadUsers();ov();}
async function dis(id,d){await api('/api/admin/users/disable','POST',{user_id:id,disabled:d});loadUsers();}
async function adm(id,a){await api('/api/admin/users/set_admin','POST',{user_id:id,is_admin:a});loadUsers();}
ov();loadCodes('unused');loadUsers();
</script></body></html>"##;

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

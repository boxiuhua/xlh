use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};

use crate::web::auth::{self, AuthState};

/// `/trade` 交易页骨架：状态条 + 七个标签面板。具体标签内容见 Task 3~5，
/// 本任务只搭框架、共享 JS 工具函数与轮询逻辑，全部塞进同一个 script 标签
/// （便于后续任务原地追加代码，而不必新增 script 标签）。
pub const TRADE_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>xlh 交易</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Arial,sans-serif;background:#f5f6fa;color:#2c3e50}
#trade-bar{position:sticky;top:0;z-index:50;display:flex;flex-wrap:wrap;gap:12px;align-items:center;padding:8px 14px;font:13px system-ui;background:#111827;color:#e5e7eb;border-bottom:1px solid #374151}
#trade-bar a{color:#93c5fd;text-decoration:none}
#trade-bar b{color:#fff}
.hb{white-space:nowrap}
.hb .dot{display:inline-block}
.hb-ok{color:#34d399}
.hb-bad{color:#f87171}
#kill-banner,#trading-off{display:none;padding:3px 10px;border-radius:6px;font-weight:600;white-space:nowrap}
#kill-banner{background:#7f1d1d;color:#fecaca}
#trading-off{background:#78350f;color:#fde68a}
.wrap{max-width:1200px;margin:0 auto;padding:20px 16px}
.tabs{display:flex;flex-wrap:wrap;gap:6px;margin-bottom:14px;border-bottom:2px solid #e0e4ea}
.tab{padding:9px 18px;cursor:pointer;border:none;background:none;font-size:.95rem;color:#7f8c8d;border-bottom:2px solid transparent;margin-bottom:-2px}
.tab.active{color:#c0392b;border-bottom-color:#c0392b;font-weight:600}
.panel{display:none}
.panel.active{display:block}
.card{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:18px;margin-bottom:16px;box-shadow:0 1px 4px rgba(0,0,0,.06)}
.hint{color:#7f8c8d;font-size:.85rem}
#toast{position:fixed;right:16px;bottom:16px;z-index:200;display:flex;flex-direction:column;gap:8px}
.toast-item{padding:8px 14px;border-radius:8px;font-size:.9rem;color:#fff;box-shadow:0 2px 8px rgba(0,0,0,.15)}
.toast-ok{background:#16a34a}
.toast-err{background:#c0392b}
@media (max-width:640px){
  #trade-bar{flex-direction:column;align-items:flex-start}
  .card{padding:12px}
  .tabs{gap:2px}
  .tab{padding:8px 10px;font-size:.85rem}
}
</style>
</head>
<body>
<div id="trade-bar">
  <span id="trade-user"></span>
  <span id="hb-monitor" class="hb">监听 <span class="dot">●</span>中断</span>
  <span id="hb-eval" class="hb">评估 <span class="dot">●</span>中断</span>
  <span id="kill-banner">管理员已暂停交易：不生成新工单、不能确认</span>
  <span id="trading-off">你已关闭交易</span>
  <span style="flex:1"></span>
  <span class="hb">实盘可用 <b id="acc-real">—</b></span>
  <span class="hb">模拟盘可用 <b id="acc-paper">—</b></span>
  <a href="/">返回主页</a>
</div>
<div class="wrap">
  <div class="tabs">
    <button class="tab" data-tab="pending">待确认</button>
    <button class="tab" data-tab="working">待成交</button>
    <button class="tab" data-tab="done">已完成</button>
    <button class="tab" data-tab="rejected">被拦截的信号</button>
    <button class="tab" data-tab="strategies">策略</button>
    <button class="tab" data-tab="risk">风控设置</button>
    <button class="tab" data-tab="positions">持仓校准</button>
  </div>
  <div id="panel-pending" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-working" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-done" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-rejected" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-strategies" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-risk" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="panel-positions" class="panel"><div class="card"><div class="hint">加载中…</div></div></div>
</div>
<div id="toast"></div>
<script>
// ===== 通用工具（供 Task 3~5 各标签实现共用，名字须保持不变） =====

function esc(s){
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

async function api(path, method, body){
  const opts = { method: method || 'GET', headers: {} };
  if (body !== undefined) {
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  let resp;
  try {
    resp = await fetch(path, opts);
  } catch (e) {
    return { ok: false, status: 0, data: null };
  }
  if (resp.status === 401) {
    location.href = '/login';
    return { ok: false, status: 401, data: null };
  }
  let data = null;
  try {
    data = await resp.json();
  } catch (e) {
    data = null;
  }
  return { ok: resp.ok, status: resp.status, data };
}

function toast(msg, kind){
  const host = document.getElementById('toast');
  if (!host) return;
  const el = document.createElement('div');
  el.className = 'toast-item ' + (kind === 'err' ? 'toast-err' : 'toast-ok');
  el.textContent = msg;
  host.appendChild(el);
  setTimeout(() => { el.remove(); }, 3000);
}

function fmtMoney(x){
  if (x === null || x === undefined || typeof x !== 'number' || isNaN(x)) return '—';
  return x.toFixed(2);
}

function fmtPct(x){
  if (x === null || x === undefined || typeof x !== 'number' || isNaN(x)) return '—';
  return (x * 100).toFixed(1) + '%';
}

function fmtTime(ts){
  if (!ts) return '—';
  const d = new Date(String(ts).replace(' ', 'T'));
  if (isNaN(d.getTime())) return String(ts);
  const p = n => String(n).padStart(2, '0');
  return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function sideLabel(side){
  return { buy: '买入', sell: '卖出' }[side] || (side || '—');
}

function sourceLabel(src){
  return { exit: '止盈止损', strategy: '策略信号', mover: '实时异动', manual: '手动/AI' }[src] || (src || '—');
}

function statusLabel(st){
  return {
    pending: '待确认',
    confirmed: '待成交',
    partial: '部分成交',
    filled: '已成交',
    expired: '已过期',
    rejected: '已拒绝',
    cancelled: '已取消',
  }[st] || (st || '—');
}

// 各标签把自己的 load 函数挂在这里：LOADERS.pending = loadPending ...（Task 3~5 追加）
const LOADERS = {};

let currentTab = 'pending';
const TABS = ['pending', 'working', 'done', 'rejected', 'strategies', 'risk', 'positions'];

function showTab(name){
  if (!TABS.includes(name)) name = 'pending';
  currentTab = name;
  for (const t of TABS) {
    const panel = document.getElementById('panel-' + t);
    if (panel) panel.classList.toggle('active', t === name);
  }
  document.querySelectorAll('.tab').forEach(btn => {
    btn.classList.toggle('active', btn.getAttribute('data-tab') === name);
  });
  if (location.hash.slice(1) !== name) location.hash = name;
  const loader = LOADERS[name];
  if (typeof loader === 'function') loader();
}

document.querySelectorAll('.tab').forEach(btn => {
  btn.addEventListener('click', () => showTab(btn.getAttribute('data-tab')));
});

window.addEventListener('hashchange', () => {
  const name = location.hash.slice(1);
  if (name && name !== currentTab) showTab(name);
});

// ===== 顶部状态条 =====

async function loadMe(){
  const r = await api('/api/auth/me');
  if (r.ok && r.data) {
    document.getElementById('trade-user').textContent = r.data.username || '';
  }
}

async function loadOverview(){
  const r = await api('/api/trade/overview');
  if (!r.ok || !r.data) return;
  const o = r.data;
  const acc = o.accounts || {};
  const real = acc.real;
  const paper = acc.paper;
  document.getElementById('acc-real').textContent = real ? fmtMoney(real.available_cash) : '—';
  document.getElementById('acc-paper').textContent = paper ? fmtMoney(paper.available_cash) : '—';

  const monitorEl = document.getElementById('hb-monitor');
  monitorEl.innerHTML = o.monitor_alive
    ? '监听 <span class="dot hb-ok">●</span>正常'
    : '监听 <span class="dot hb-bad">●</span>中断';
  const evalEl = document.getElementById('hb-eval');
  evalEl.innerHTML = o.eval_alive
    ? '评估 <span class="dot hb-ok">●</span>正常'
    : '评估 <span class="dot hb-bad">●</span>中断';

  document.getElementById('kill-banner').style.display = o.kill_switch ? 'inline-block' : 'none';
  document.getElementById('trading-off').style.display =
    (!o.kill_switch && o.trading_enabled === false) ? 'inline-block' : 'none';
}

// ===== 初始化与轮询 =====

function initialTab(){
  const h = location.hash.slice(1);
  return TABS.includes(h) ? h : 'pending';
}

showTab(initialTab());
loadMe();
loadOverview();

setInterval(() => {
  if (document.hidden) return;
  loadOverview();
  if (currentTab === 'pending' && typeof LOADERS.pending === 'function') LOADERS.pending();
}, 15000);

// ===== Task 3: 待确认 / 待成交 / 已完成 / 被拦截的信号（追加区） =====

// ===== Task 4: 策略 / 风控设置（追加区） =====

// ===== Task 5: 持仓校准（追加区） =====
</script>
</body>
</html>
"##;

/// 生产 `/trade` 入口：已登录返回交易页 HTML，未登录跳转 /login。
/// 会话判断与 `index`（`src/web/mod.rs`）保持一致：读 cookie → 查会话。
pub async fn trade_page(State(st): State<AuthState>, headers: HeaderMap) -> Response {
    let now = chrono::Local::now().date_naive();
    let logged_in = auth::session::read_cookie(&headers)
        .and_then(|t| {
            let conn = st.db.lock().unwrap();
            auth::store::lookup_session_user(&conn, &t, now)
                .ok()
                .flatten()
        })
        .is_some();
    if logged_in {
        Html(TRADE_HTML).into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn state() -> crate::web::auth::AuthState {
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        crate::push::store::migrate(&conn).unwrap();
        crate::trade::store::migrate(&conn).unwrap();
        crate::web::auth::AuthState::new(conn, Default::default())
    }

    async fn get(
        st: &crate::web::auth::AuthState,
        uri: &str,
        cookie: Option<&str>,
    ) -> (StatusCode, String, axum::http::HeaderMap) {
        let mut req = Request::builder().uri(uri);
        if let Some(c) = cookie {
            req = req.header("cookie", format!("xlh_session={c}"));
        }
        let resp = crate::web::router(st.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string(), headers)
    }

    fn login(st: &crate::web::auth::AuthState) -> &'static str {
        let c = st.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, "u", "h", false).unwrap();
        let today = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, today + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, "tok", uid, today + chrono::Duration::days(1))
            .unwrap();
        "tok"
    }

    #[tokio::test]
    async fn trade_page_requires_login_and_has_all_tabs() {
        let st = state();
        let (s, _, h) = get(&st, "/trade", None).await;
        assert!(s.is_redirection(), "{s}");
        assert_eq!(h.get("location").unwrap(), "/login");
        let tok = login(&st);
        let (s, body, _) = get(&st, "/trade", Some(tok)).await;
        assert_eq!(s, StatusCode::OK);
        for tab in [
            "pending",
            "working",
            "done",
            "rejected",
            "strategies",
            "risk",
            "positions",
        ] {
            assert!(
                body.contains(&format!("data-tab=\"{tab}\"")),
                "缺标签 {tab}"
            );
            assert!(
                body.contains(&format!("id=\"panel-{tab}\"")),
                "缺面板 {tab}"
            );
        }
        for id in [
            "hb-monitor",
            "hb-eval",
            "kill-banner",
            "acc-real",
            "acc-paper",
        ] {
            assert!(body.contains(&format!("id=\"{id}\"")), "缺 {id}");
        }
        assert!(body.contains("function esc("));
        assert!(
            !body.contains("http://") && !body.contains("https://"),
            "不得引用外部资源"
        );
    }

    #[tokio::test]
    async fn index_links_to_trade() {
        assert!(crate::web::page::INDEX_HTML.contains("href=\"/trade\""));
    }
}

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
.btn{padding:6px 14px;border:1px solid #c0392b;border-radius:6px;background:#c0392b;color:#fff;cursor:pointer;font-size:.9rem}
.btn.ghost{background:#fff;color:#555;border-color:#ccd2da}
.btn.warn{background:#d35400;border-color:#d35400}
.btn:disabled{opacity:.5;cursor:not-allowed}
.side-buy{color:#c0392b;font-weight:600}
.side-sell{color:#16a34a;font-weight:600}
.tag{display:inline-block;padding:1px 8px;border-radius:10px;background:#eef1f4;color:#555;font-size:.8rem;margin-left:6px}
.tag.urgent{background:#fdecea;color:#c0392b}
.ticket-head{display:flex;flex-wrap:wrap;align-items:center;gap:4px;margin-bottom:8px}
.ticket-head .code{font-size:1.1rem;font-weight:700;margin-right:6px}
.countdown{margin-left:auto;font-variant-numeric:tabular-nums;color:#7f8c8d;font-size:.9rem}
.ticket-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(150px,1fr));gap:6px 16px;margin:8px 0;font-size:.9rem}
.ticket-grid .k{color:#7f8c8d;margin-right:4px}
.dev-bad{color:#c0392b;font-weight:600}
.ticket-reason{font-size:.9rem;margin:6px 0;white-space:pre-wrap;word-break:break-word}
.ticket-actions{display:flex;flex-wrap:wrap;gap:8px;align-items:center;margin-top:10px}
.ticket.expired{opacity:.55;filter:grayscale(1)}
.linkish{background:none;border:none;color:#2563eb;cursor:pointer;font-size:.9rem;padding:0}
details.ai-note{font-size:.9rem;margin:6px 0}
details.ai-note summary{cursor:pointer;color:#2563eb}
.tbl-wrap{overflow-x:auto}
table.tbl{width:100%;border-collapse:collapse;font-size:.9rem}
table.tbl th,table.tbl td{padding:7px 8px;border-bottom:1px solid #eef1f4;text-align:left;white-space:nowrap}
table.tbl td.wrap-cell{white-space:normal;min-width:160px}
table.tbl th{color:#7f8c8d;font-weight:500}
.fill-form{display:flex;gap:6px;align-items:center}
.fill-form input{width:90px;padding:4px 6px;border:1px solid #ccd2da;border-radius:5px}
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

// ===== Task 3: 待确认 / 待成交 / 已完成 / 被拦截的信号（追加区） =====

const REJECT_REASONS = {
  trading_disabled: '交易已关闭',
  duplicate_open_ticket: '已有未完结工单',
  cooldown: '冷却中',
  not_admitted: '策略未准入',
  no_quote: '无行情',
  limit_up: '涨停不买',
  limit_down: '跌停不卖',
  daily_ticket_cap: '超过每日工单上限',
  daily_loss_halt: '当日亏损已达上限',
  no_capital: '未设置资金',
  below_one_lot: '不足一手',
  nothing_sellable: '无可卖数量',
};

function rejectReasonLabel(r){
  return REJECT_REASONS[r] || (r || '—');
}

function accountLabel(a){
  return { real: '实盘', paper: '模拟盘' }[a] || (a || '—');
}

// 方向带颜色（买入红、卖出绿），返回已转义的 HTML 片段
function sideHtml(side){
  const cls = side === 'buy' ? 'side-buy' : (side === 'sell' ? 'side-sell' : '');
  return `<span class="${cls}">${esc(sideLabel(side))}</span>`;
}

// 服务端时间是本地时区的 "YYYY-MM-DD HH:MM:SS"，按本地时间解析
function parseLocalTs(ts){
  if (!ts) return NaN;
  return new Date(String(ts).replace(' ', 'T')).getTime();
}

function hintCard(msg){
  return `<div class="card"><div class="hint">${esc(msg)}</div></div>`;
}

function loadFailedMsg(r){
  return (r.data && r.data.error) || `加载失败(${r.status})`;
}

// ----- 待确认 -----

function confirmState(ticket) {
  if (!ticket.quote || ticket.quote.stale) return { text: '行情延迟', disabled: true, ack: false };
  const deviated = ticket.deviation != null && ticket.deviation > ticket.deviation_th;
  return deviated
    ? { text: `价格已偏离 ${fmtPct(ticket.deviation)},仍要确认`, disabled: false, ack: true, warn: true }
    : { text: '确认', disabled: false, ack: false };
}

async function onConfirm(ticket, btn, ack) {
  btn.disabled = true;
  const r = await api(`/api/trade/tickets/${ticket.id}/confirm`, 'POST', { ack_deviation: ack });
  if (r.ok) { toast('已确认,请在券商 App 下单后回来回填成交', 'ok'); return LOADERS.pending(); }
  const code = r.data && r.data.code;
  if (code === 'deviation') {
    // 服务端判出偏离而页面未预判:改为二次确认
    btn.textContent = `价格已偏离 ${fmtPct(r.data.deviation)},仍要确认`;
    btn.classList.add('warn');
    btn.disabled = false;
    btn.onclick = () => onConfirm(ticket, btn, true);
    return;
  }
  const msg = {
    already_handled: '该工单已处理或已过期',
    stale_quote: '行情延迟,暂不能确认',
    kill_switch: '管理员已暂停交易',
  }[code] || (r.data && r.data.error) || `确认失败(${r.status})`;
  toast(msg, 'err');
  LOADERS.pending();
}

async function onIgnore(ticket, btn){
  const input = prompt(`忽略 ${ticket.code} 的工单，请填写理由：`, '');
  if (input === null) return;
  const reason = input.trim();
  if (!reason) { toast('未填写理由，未提交', 'err'); return; }
  btn.disabled = true;
  const r = await api(`/api/trade/tickets/${ticket.id}/ignore`, 'POST', { reason });
  if (r.ok) {
    toast('已忽略', 'ok');
  } else {
    const code = r.data && r.data.code;
    toast(code === 'already_handled'
      ? '该工单已处理或已过期'
      : ((r.data && r.data.error) || `忽略失败(${r.status})`), 'err');
  }
  LOADERS.pending();
}

function fmtCountdown(ms){
  const s = Math.max(0, Math.floor(ms / 1000));
  const p = n => String(n).padStart(2, '0');
  return `剩 ${p(Math.floor(s / 60))}:${p(s % 60)}`;
}

// 每秒刷新所有待确认卡片的倒计时；到期置灰并禁用操作按钮。
// 定时器只在脚本加载时建一次，按 DOM 现状工作，重新渲染不会叠加定时器。
function tickCountdowns(){
  const now = Date.now();
  document.querySelectorAll('#panel-pending .ticket').forEach(card => {
    const el = card.querySelector('.countdown');
    const exp = Number(card.getAttribute('data-expires'));
    if (!exp || !isFinite(exp)) { if (el) el.textContent = '有效期未知'; return; }
    const left = exp - now;
    if (left > 0) { if (el) el.textContent = fmtCountdown(left); return; }
    if (el) el.textContent = '已过期';
    if (!card.classList.contains('expired')) {
      card.classList.add('expired');
      card.querySelectorAll('button.act').forEach(b => { b.disabled = true; });
    }
  });
}
setInterval(tickCountdowns, 1000);

function ticketCardHtml(t, noteOpen){
  const st = confirmState(t);
  const devBad = t.deviation != null && t.deviation > t.deviation_th;
  const exp = parseLocalTs(t.expires_at);
  const urgent = t.urgency > 0 ? `<span class="tag urgent">第 ${esc(t.urgency + 1)} 次提醒</span>` : '';
  const price = t.quote ? fmtMoney(t.quote.price) : '—';
  const stale = t.quote && t.quote.stale ? '<span class="tag urgent">行情延迟</span>' : '';
  const note = t.ai_note
    ? `<details class="ai-note"${noteOpen ? ' open' : ''}><summary>AI 说明</summary><div class="ticket-reason">${esc(t.ai_note)}</div></details>`
    : '';
  const strat = t.strategy_id != null
    ? `<button type="button" class="linkish js-strategy">查看策略成绩单</button>`
    : '';
  return `<div class="card ticket" data-id="${esc(t.id)}" data-expires="${esc(isFinite(exp) ? exp : '')}">
    <div class="ticket-head">
      <span class="code">${esc(t.code)}</span>${sideHtml(t.side)}
      <span class="tag">${esc(sourceLabel(t.source))}</span>${urgent}${stale}
      <span class="countdown"></span>
    </div>
    <div class="ticket-grid">
      <div><span class="k">建议价</span>${esc(fmtMoney(t.suggest_price))}</div>
      <div><span class="k">现价</span>${esc(price)}</div>
      <div><span class="k">偏离</span><span class="${devBad ? 'dev-bad' : ''}">${esc(fmtPct(t.deviation))}</span>
        <span class="hint">(阈值 ${esc(fmtPct(t.deviation_th))})</span></div>
      <div><span class="k">数量</span>${esc(t.qty)} 股</div>
      <div><span class="k">预估金额</span>${esc(fmtMoney(t.est_amount))}</div>
      <div><span class="k">预估费用</span>${esc(fmtMoney(t.est_fee))}</div>
    </div>
    <div class="ticket-reason"><span class="k hint">理由：</span>${esc(t.reason || '—')}</div>
    ${note}
    <div class="ticket-actions">
      <button type="button" class="btn act js-confirm${st.warn ? ' warn' : ''}"${st.disabled ? ' disabled' : ''}>${esc(st.text)}</button>
      <button type="button" class="btn ghost act js-ignore">忽略</button>
      ${strat}
    </div>
  </div>`;
}

let pendingSeq = 0;

async function loadPending(){
  const seq = ++pendingSeq;
  const panel = document.getElementById('panel-pending');
  const r = await api('/api/trade/tickets?view=pending');
  if (seq !== pendingSeq) return; // 已有更新的请求，丢弃过时结果
  if (!r.ok || !Array.isArray(r.data)) { panel.innerHTML = hintCard(loadFailedMsg(r)); return; }
  const list = r.data;
  if (!list.length) { panel.innerHTML = hintCard('没有待确认的工单'); return; }
  // 15 秒重绘时保留已展开的「AI 说明」
  const openNotes = new Set();
  panel.querySelectorAll('.ticket').forEach(card => {
    const d = card.querySelector('details.ai-note');
    if (d && d.open) openNotes.add(card.getAttribute('data-id'));
  });
  panel.innerHTML = list.map(t => ticketCardHtml(t, openNotes.has(String(t.id)))).join('');
  const byId = new Map(list.map(t => [String(t.id), t]));
  panel.querySelectorAll('.ticket').forEach(card => {
    const t = byId.get(card.getAttribute('data-id'));
    if (!t) return;
    const confirmBtn = card.querySelector('.js-confirm');
    const ack = confirmState(t).ack;
    confirmBtn.onclick = () => onConfirm(t, confirmBtn, ack);
    const ignoreBtn = card.querySelector('.js-ignore');
    ignoreBtn.onclick = () => onIgnore(t, ignoreBtn);
    const sBtn = card.querySelector('.js-strategy');
    if (sBtn) {
      const sid = t.strategy_id;
      sBtn.onclick = () => (typeof openStrategy === 'function' ? openStrategy(sid) : showTab('strategies'));
    }
  });
  tickCountdowns();
}
LOADERS.pending = loadPending;

// ----- 待成交 -----

async function onFill(t, row){
  const priceEl = row.querySelector('.js-fill-price');
  const qtyEl = row.querySelector('.js-fill-qty');
  const btn = row.querySelector('.js-fill');
  const price = Number(priceEl.value);
  const qty = Number(qtyEl.value);
  if (!(price > 0)) { toast('请填写有效的成交价', 'err'); return; }
  if (!Number.isInteger(qty) || qty <= 0) { toast('请填写有效的成交数量（正整数）', 'err'); return; }
  btn.disabled = true;
  const r = await api(`/api/trade/tickets/${t.id}/fill`, 'POST', { price, qty });
  if (r.ok) {
    toast('已回填', 'ok');
  } else {
    const code = r.data && r.data.code;
    toast(code === 'paper_ticket'
      ? '模拟盘工单由系统撮合，不可人工回填'
      : ((r.data && r.data.error) || `回填失败(${r.status})`), 'err');
    btn.disabled = false;
    if (r.status !== 400) LOADERS.working();
    return;
  }
  LOADERS.working();
}

let workingSeq = 0;

async function loadWorking(){
  const seq = ++workingSeq;
  const panel = document.getElementById('panel-working');
  const r = await api('/api/trade/tickets?view=working');
  if (seq !== workingSeq) return;
  const tip = '<div class="hint" style="margin-bottom:10px">当日未回填的工单将在次日 9:00 自动撤销；请在券商 App 成交后回填实际成交价与数量。</div>';
  if (!r.ok || !Array.isArray(r.data)) { panel.innerHTML = hintCard(loadFailedMsg(r)); return; }
  const list = r.data;
  if (!list.length) {
    panel.innerHTML = `<div class="card">${tip}<div class="hint">没有待成交的工单</div></div>`;
    return;
  }
  const rows = list.map(t => {
    const left = Math.max(0, (Number(t.qty) || 0) - (Number(t.filled_qty) || 0));
    return `<tr data-id="${esc(t.id)}">
      <td>${esc(t.code)}</td>
      <td>${sideHtml(t.side)}</td>
      <td>${esc(t.qty)}</td>
      <td>${esc(t.filled_qty)}</td>
      <td>${esc(statusLabel(t.status))}</td>
      <td>${esc(fmtTime(t.confirmed_at))}</td>
      <td><div class="fill-form">
        <input type="number" step="0.01" min="0" class="js-fill-price" placeholder="成交价"/>
        <input type="number" step="1" min="1" class="js-fill-qty" value="${esc(left)}"/>
        <button type="button" class="btn js-fill">回填</button>
      </div></td>
    </tr>`;
  }).join('');
  panel.innerHTML = `<div class="card">${tip}<div class="tbl-wrap"><table class="tbl">
    <thead><tr><th>代码</th><th>方向</th><th>工单数量</th><th>已成交</th><th>状态</th><th>确认时间</th><th>回填（成交价 / 数量）</th></tr></thead>
    <tbody>${rows}</tbody></table></div></div>`;
  const byId = new Map(list.map(t => [String(t.id), t]));
  panel.querySelectorAll('tr[data-id]').forEach(row => {
    const t = byId.get(row.getAttribute('data-id'));
    if (!t) return;
    row.querySelector('.js-fill').onclick = () => onFill(t, row);
  });
}
LOADERS.working = loadWorking;

// ----- 已完成 -----

let doneSeq = 0;

async function loadDone(){
  const seq = ++doneSeq;
  const panel = document.getElementById('panel-done');
  const r = await api('/api/trade/tickets?view=done');
  if (seq !== doneSeq) return;
  if (!r.ok || !Array.isArray(r.data)) { panel.innerHTML = hintCard(loadFailedMsg(r)); return; }
  const list = r.data;
  if (!list.length) { panel.innerHTML = hintCard('没有已完成的工单'); return; }
  const rows = list.map(t => `<tr>
      <td>${esc(fmtTime(t.created_at))}</td>
      <td>${esc(accountLabel(t.account))}</td>
      <td>${esc(t.code)}</td>
      <td>${sideHtml(t.side)}</td>
      <td>${esc(t.qty)} / ${esc(t.filled_qty)}</td>
      <td>${esc(statusLabel(t.status))}</td>
      <td class="wrap-cell">${esc(t.ignore_reason || '')}</td>
    </tr>`).join('');
  panel.innerHTML = `<div class="card"><div class="hint" style="margin-bottom:10px">最近 100 张终态工单（实盘与模拟盘）</div><div class="tbl-wrap"><table class="tbl">
    <thead><tr><th>时间</th><th>账户</th><th>代码</th><th>方向</th><th>数量 / 已成交</th><th>状态</th><th>忽略理由</th></tr></thead>
    <tbody>${rows}</tbody></table></div></div>`;
}
LOADERS.done = loadDone;

// ----- 被拦截的信号 -----

let rejectedSeq = 0;

async function loadRejected(){
  const seq = ++rejectedSeq;
  const panel = document.getElementById('panel-rejected');
  const r = await api('/api/trade/signals/rejected?limit=100');
  if (seq !== rejectedSeq) return;
  if (!r.ok || !Array.isArray(r.data)) { panel.innerHTML = hintCard(loadFailedMsg(r)); return; }
  const list = r.data;
  if (!list.length) { panel.innerHTML = hintCard('没有被拦截的信号'); return; }
  const rows = list.map(s => `<tr>
      <td>${esc(fmtTime(s.created_at))}</td>
      <td>${esc(sourceLabel(s.source))}</td>
      <td>${esc(s.code)}</td>
      <td>${sideHtml(s.side)}</td>
      <td>${esc(rejectReasonLabel(s.reject_reason))}</td>
      <td class="wrap-cell">${esc(s.reason || '')}</td>
    </tr>`).join('');
  panel.innerHTML = `<div class="card"><div class="tbl-wrap"><table class="tbl">
    <thead><tr><th>时间</th><th>来源</th><th>代码</th><th>方向</th><th>拦截原因</th><th>理由</th></tr></thead>
    <tbody>${rows}</tbody></table></div></div>`;
}
LOADERS.rejected = loadRejected;

// ===== Task 4: 策略 / 风控设置（追加区） =====

// ===== Task 5: 持仓校准（追加区） =====

// ===== 初始化与轮询（须在脚本末尾：各标签的 LOADERS 注册完后再首次加载） =====

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
    async fn ticket_tabs_have_confirm_flow_and_reject_reason_labels() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "function confirmState(",
            "function onConfirm(",
            "ack_deviation",
            "仍要确认",
            "行情延迟",
            "LOADERS.pending",
            "LOADERS.working",
            "LOADERS.done",
            "LOADERS.rejected",
            "/api/trade/tickets?view=pending",
            "/api/trade/signals/rejected",
            "次日 9:00 自动撤销",
            "nothing_sellable",
            "daily_loss_halt",
            "paper_ticket",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }

    #[tokio::test]
    async fn index_links_to_trade() {
        assert!(crate::web::page::INDEX_HTML.contains("href=\"/trade\""));
    }
}

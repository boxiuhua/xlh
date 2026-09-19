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
.stale-q{color:#95a5a6}
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
.st{display:inline-block;padding:1px 8px;border-radius:10px;font-size:.8rem;font-weight:600}
.st-draft{background:#eef1f4;color:#555}
.st-backtesting{background:#dbeafe;color:#1d4ed8}
.st-failed{background:#fdecea;color:#c0392b}
.st-paper{background:#fef3c7;color:#b45309}
.st-admitted{background:#dcfce7;color:#15803d}
.st-suspended{background:#ede9fe;color:#6d28d9}
.form-title{font-size:1rem;margin-bottom:10px}
.form-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:10px 16px}
.form-grid label{display:flex;flex-direction:column;gap:4px;font-size:.85rem;color:#555}
.form-grid label.full{grid-column:1/-1}
.form-grid input,.form-grid select,.form-grid textarea{padding:6px 8px;border:1px solid #ccd2da;border-radius:5px;font:inherit;font-size:.9rem;color:#2c3e50}
.form-grid textarea{font-family:ui-monospace,Consolas,monospace;resize:vertical}
.form-err{color:#c0392b;font-size:.85rem;margin-top:8px;white-space:pre-wrap}
.sc-head{display:flex;flex-wrap:wrap;align-items:center;gap:8px;margin-bottom:10px}
.sc-head b{font-size:1.05rem}
.sc-head .linkish{margin-left:auto}
.sc-events{font-size:.85rem;margin:0 0 12px 0;padding-left:18px;color:#555}
.sc-events li{margin:2px 0}
table.tbl td.num,table.tbl th.num{text-align:right;font-variant-numeric:tabular-nums}
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
  <div id="panel-strategies" class="panel">
    <div id="strategy-list"><div class="card"><div class="hint">加载中…</div></div></div>
    <div id="scorecard"></div>
    <form id="strategy-form" class="card" autocomplete="off">
      <h3 id="strategy-form-title" class="form-title">新建策略</h3>
      <div class="form-grid">
        <label>名称<input type="text" name="name" maxlength="60" required/></label>
        <label>类型<select name="kind">
          <option value="trend">均线择时</option>
          <option value="rsi">RSI</option>
          <option value="smart_dca">智能定投</option>
          <option value="dca">定投</option>
          <option value="adaptive">自适应</option>
          <option value="mover">盘中异动</option>
        </select></label>
        <label class="full">参数网格（TOML，每个参数给出候选值列表）<textarea name="grid_toml" rows="5" placeholder="short_window = [5, 10]&#10;long_window = [20, 60]&#10;amount = [10000.0]"></textarea></label>
        <label class="full">股票池（6 位代码，逗号或空白分隔）<input type="text" name="pool" placeholder="600000, 000001"/></label>
      </div>
      <div id="strategy-form-err" class="form-err"></div>
      <div class="ticket-actions">
        <button type="submit" class="btn js-save">保存</button>
        <button type="button" class="btn ghost js-cancel-edit" style="display:none">取消编辑</button>
        <span class="hint">修改类型、参数网格或股票池会让策略回到草稿，需要重新提交评估；只改名称不影响状态。</span>
      </div>
    </form>
    <div id="jobs"><div class="card"><div class="hint">加载中…</div></div></div>
  </div>
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
  return { exit: '止盈止损', strategy: '日线策略', mover: '异动', manual: '手动' }[src] || (src || '—');
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

// 代码后显示股票名称的后缀（有则显示，否则为空），返回已转义的 HTML 片段（4c）
function nameSuffixHtml(t){
  return t.name ? ` <span class="hint">${esc(t.name)}</span>` : '';
}

function hintCard(msg){
  return `<div class="card"><div class="hint">${esc(msg)}</div></div>`;
}

// 授权中间件的 403 错误码(`{error: "expired" | "license_required"}`)→ 中文提示
const LICENSE_ERR = {
  expired: '授权已过期,请在主页续期',
  license_required: '未激活授权,请在主页输入授权码',
};

// 接口失败时给用户看的文字:授权 403 映射成中文,否则取服务端 error,再退到「前缀(状态码)」
function errText(r, prefix){
  const e = r.data && r.data.error;
  if (r.status === 403 && LICENSE_ERR[e]) return LICENSE_ERR[e];
  return e || `${prefix}(${r.status})`;
}

function loadFailedMsg(r){
  return errText(r, '加载失败');
}

// 加载失败:host 已经渲染成功过(data-loaded)时保留原内容、只弹提示,
// 避免 15 秒轮询偶发失败把列表清空;从未渲染成功过才在 host 里显示失败信息。
function failInto(host, r){
  if (host.getAttribute('data-loaded') === '1') { toast(loadFailedMsg(r), 'err'); return; }
  host.innerHTML = hintCard(loadFailedMsg(r));
}

// 成功渲染后调用,标记 host 已有可保留的内容
function markLoaded(host){
  host.setAttribute('data-loaded', '1');
}

// ----- 待确认 -----

function confirmState(ticket) {
  if (!ticket.quote || ticket.quote.stale) return { text: '行情延迟', disabled: true, ack: false };
  const deviated = ticket.deviation != null && ticket.deviation > ticket.deviation_th;
  return deviated
    ? { text: `价格已偏离 ${fmtPct(ticket.deviation)},仍要确认`, disabled: false, ack: true, warn: true }
    : { text: '确认', disabled: false, ack: false };
}

// 带 ack 时同时带上按钮上显示的偏离(ack_max_deviation):服务端只在现价偏离不超过
// 用户看到的值(+0.5 个百分点容差)时放行,否则再返回 409 deviation,按新值重新确认(4b 终审 I1)。
function confirmBody(ack, shownDev){
  const body = { ack_deviation: ack };
  if (ack && typeof shownDev === 'number' && isFinite(shownDev) && shownDev >= 0) body.ack_max_deviation = shownDev;
  return body;
}

async function onConfirm(ticket, btn, ack, shownDev) {
  btn.disabled = true;
  const r = await api(`/api/trade/tickets/${ticket.id}/confirm`, 'POST', confirmBody(ack, shownDev));
  if (r.ok) {
    toast('已确认,请在券商 App 下单后回来回填成交', 'ok');
    loadOverview();
    return LOADERS.pending();
  }
  const code = r.data && r.data.code;
  if (code === 'deviation') {
    // 服务端判出偏离(页面未预判,或比页面显示的更大):按服务端返回的偏离二次确认
    const dev = r.data.deviation;
    btn.textContent = `价格已偏离 ${fmtPct(dev)},仍要确认`;
    btn.classList.add('warn');
    btn.disabled = false;
    btn.onclick = () => onConfirm(ticket, btn, true, dev);
    return;
  }
  const msg = {
    already_handled: '该工单已处理或已过期',
    stale_quote: '行情延迟,暂不能确认',
    kill_switch: '管理员已暂停交易',
  }[code] || errText(r, '确认失败');
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
      : errText(r, '忽略失败'), 'err');
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
    const deadline = Number(card.getAttribute('data-deadline'));
    if (!deadline || !isFinite(deadline)) { if (el) el.textContent = '有效期未知'; return; }
    const left = deadline - now;
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
  // 倒计时以服务端剩余秒数为准:收到响应时刻 + 秒数 = 截止时间,不再解析本地时间字符串(设计裁决 4)。
  const deadline = Date.now() + Number(t.expires_in_secs || 0) * 1000;
  const urgent = t.urgency > 0 ? `<span class="tag urgent">第 ${esc(t.urgency + 1)} 次提醒</span>` : '';
  const price = t.quote ? fmtMoney(t.quote.price) : '—';
  const stale = t.quote && t.quote.stale ? '<span class="tag urgent">行情延迟</span>' : '';
  const note = t.ai_note
    ? `<details class="ai-note"${noteOpen ? ' open' : ''}><summary>AI 说明</summary><div class="ticket-reason">${esc(t.ai_note)}</div></details>`
    : '';
  const strat = t.strategy_id != null
    ? `<button type="button" class="linkish js-strategy">查看策略成绩单</button>`
    : '';
  return `<div class="card ticket" data-id="${esc(t.id)}" data-deadline="${esc(deadline)}">
    <div class="ticket-head">
      <span class="code">${esc(t.code)}</span>${nameSuffixHtml(t)}${sideHtml(t.side)}
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
  if (!r.ok || !Array.isArray(r.data)) { failInto(panel, r); return; }
  markLoaded(panel);
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
    confirmBtn.onclick = () => onConfirm(t, confirmBtn, ack, t.deviation);
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
      : errText(r, '回填失败'), 'err');
    btn.disabled = false;
    if (r.status !== 400) LOADERS.working();
    return;
  }
  loadOverview();
  LOADERS.working();
}

let workingSeq = 0;

async function loadWorking(){
  const seq = ++workingSeq;
  const panel = document.getElementById('panel-working');
  const r = await api('/api/trade/tickets?view=working');
  if (seq !== workingSeq) return;
  const tip = '<div class="hint" style="margin-bottom:10px">当日未回填的工单将在次日 9:00 自动撤销；请在券商 App 成交后回填实际成交价与数量。</div>';
  if (!r.ok || !Array.isArray(r.data)) { failInto(panel, r); return; }
  markLoaded(panel);
  const list = r.data;
  if (!list.length) {
    panel.innerHTML = `<div class="card">${tip}<div class="hint">没有待成交的工单</div></div>`;
    return;
  }
  const rows = list.map(t => {
    const left = Math.max(0, (Number(t.qty) || 0) - (Number(t.filled_qty) || 0));
    return `<tr data-id="${esc(t.id)}">
      <td>${esc(t.code)}${nameSuffixHtml(t)}</td>
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
  if (!r.ok || !Array.isArray(r.data)) { failInto(panel, r); return; }
  markLoaded(panel);
  const list = r.data;
  if (!list.length) { panel.innerHTML = hintCard('没有已完成的工单'); return; }
  const rows = list.map(t => `<tr>
      <td>${esc(fmtTime(t.created_at))}</td>
      <td>${esc(accountLabel(t.account))}</td>
      <td>${esc(t.code)}${nameSuffixHtml(t)}</td>
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
  if (!r.ok || !Array.isArray(r.data)) { failInto(panel, r); return; }
  markLoaded(panel);
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

// ----- 策略 -----

const STRATEGY_KINDS = {
  trend: '均线择时',
  rsi: 'RSI',
  smart_dca: '智能定投',
  dca: '定投',
  adaptive: '自适应',
  mover: '盘中异动',
};

const STRATEGY_STATUS = {
  draft: '草稿',
  backtesting: '回测中',
  failed: '未通过',
  paper: '观察期',
  admitted: '已准入',
  suspended: '已暂停',
};

// 只有这些状态可以（重新）提交评估
const SUBMITTABLE = ['draft', 'failed', 'suspended'];

function kindLabel(k){
  return STRATEGY_KINDS[k] || (k || '—');
}

// 状态徽标（配色区分），返回已转义的 HTML 片段；class 只取白名单内的值
function strategyStatusHtml(st){
  const cls = STRATEGY_STATUS[st] ? ' st-' + st : '';
  return `<span class="st${cls}">${esc(STRATEGY_STATUS[st] || st || '—')}</span>`;
}

function jobKindLabel(k){
  return { walk_forward: '前推回测', paper_check: '观察期检查', watchdog: '实盘监控' }[k] || (k || '—');
}

function jobStatusLabel(s){
  return { queued: '排队', running: '运行中', done: '完成', failed: '失败' }[s] || (s || '—');
}

function poolSummary(pool){
  const list = Array.isArray(pool) ? pool : [];
  const head = list.slice(0, 5).join(', ');
  return list.length > 5 ? `${head} 等 ${list.length} 只` : (head || '—');
}

// 成绩单四栏取值（设计裁决 6）：[指标, 样本内, 样本外, 模拟盘, 实盘]。
// 字段名已与 PoolMetrics / TradeBaseline / StageMetrics 对齐；池级 PoolMetrics
// 没有 oos_annualized，故「年化收益」样本外一格恒为「—」（不自行推算）。
function scorecardRows(sc) {
  const oos = sc.oos, tb = oos && oos.trade_baseline;
  const dash = '—';
  const pct = (x) => (x == null || Number.isNaN(x) ? dash : fmtPct(x));
  const num = (x, d = 2) => (x == null || Number.isNaN(x) ? dash : Number(x).toFixed(d));
  const stage = (m) => m && m.trades > 0 ? m : null;
  const paper = stage(sc.paper), real = stage(sc.real);
  return [
    ['年化收益', dash, oos ? pct(oos.oos_annualized ?? null) : dash, dash, dash],
    ['超额(相对买入持有)', dash, oos ? pct(oos.oos_return - oos.buy_hold_return) : dash, dash, dash],
    ['夏普', oos ? num(oos.is_sharpe) : dash, oos ? num(oos.oos_sharpe) : dash, dash, dash],
    ['最大回撤', dash, oos ? pct(oos.oos_max_drawdown) : dash, paper ? pct(paper.max_drawdown) : dash, real ? pct(real.max_drawdown) : dash],
    ['胜率', dash, tb ? pct(tb.win_rate) : dash, paper ? pct(paper.win_rate) : dash, real ? pct(real.win_rate) : dash],
    ['盈亏比', dash, dash, paper ? num(paper.profit_factor) : dash, real ? num(real.profit_factor) : dash],
    ['交易笔数', dash, oos ? String(oos.oos_trades) : dash, paper ? String(paper.trades) : dash, real ? String(real.trades) : dash],
    ['平均每笔收益', dash, tb ? pct(tb.avg_return) : dash, paper ? pct(paper.avg_trade_return) : dash, real ? pct(real.avg_trade_return) : dash],
    ['已实现盈亏', dash, dash, paper ? fmtMoney(paper.realized_pnl) : dash, real ? fmtMoney(real.realized_pnl) : dash],
  ];
}

let strategiesSeq = 0;
let scorecardSeq = 0;
let scorecardId = null;   // 当前展开成绩单的策略 id（null = 未展开）
let editingId = null;     // 表单正在编辑的策略 id（null = 新建）
let scorecardScroll = false; // 成绩单下次渲染后是否滚动到可见处
let strategiesById = new Map();

function strategyForm(){
  return document.getElementById('strategy-form');
}

function resetStrategyForm(){
  const f = strategyForm();
  editingId = null;
  f.reset();
  document.getElementById('strategy-form-title').textContent = '新建策略';
  document.getElementById('strategy-form-err').textContent = '';
  f.querySelector('.js-save').textContent = '保存';
  f.querySelector('.js-cancel-edit').style.display = 'none';
}

function editStrategy(s){
  const f = strategyForm();
  editingId = s.id;
  f.elements.name.value = s.name || '';
  f.elements.kind.value = s.kind;
  f.elements.grid_toml.value = s.grid_toml || '';
  f.elements.pool.value = (Array.isArray(s.pool) ? s.pool : []).join(', ');
  document.getElementById('strategy-form-title').textContent = `编辑策略：${s.name}`;
  document.getElementById('strategy-form-err').textContent = '';
  f.querySelector('.js-save').textContent = '保存修改';
  f.querySelector('.js-cancel-edit').style.display = '';
  f.scrollIntoView({ behavior: 'smooth', block: 'start' });
}

async function onSaveStrategy(ev){
  ev.preventDefault();
  const f = strategyForm();
  const errEl = document.getElementById('strategy-form-err');
  errEl.textContent = '';
  const body = {
    name: f.elements.name.value.trim(),
    kind: f.elements.kind.value,
    grid_toml: f.elements.grid_toml.value,
    pool: f.elements.pool.value.split(/[\s,，、]+/).filter(Boolean),
  };
  const id = editingId;
  const btn = f.querySelector('.js-save');
  btn.disabled = true;
  const r = id == null
    ? await api('/api/trade/strategies', 'POST', body)
    : await api(`/api/trade/strategies/${encodeURIComponent(id)}`, 'POST', body);
  btn.disabled = false;
  if (!r.ok) {
    const msg = errText(r, '保存失败');
    if (r.status === 400) { errEl.textContent = msg; return; }
    toast(r.status === 404 ? '策略不存在' : msg, 'err');
    if (r.status === 404) { resetStrategyForm(); LOADERS.strategies(); }
    return;
  }
  if (id == null) {
    toast('已新建策略（草稿），可提交评估', 'ok');
  } else {
    const result = r.data && r.data.result;
    if (result === 'reversioned') {
      toast('定义已变更,策略回到草稿,需要重新提交评估', 'ok');
    } else if (result === 'renamed') {
      toast('已改名', 'ok');
    } else {
      toast('没有变化', 'ok');
    }
  }
  resetStrategyForm();
  LOADERS.strategies();
}

async function onSubmitStrategy(s, btn){
  btn.disabled = true;
  const r = await api(`/api/trade/strategies/${encodeURIComponent(s.id)}/submit`, 'POST');
  if (r.ok) {
    const result = r.data && r.data.result;
    toast(result === 'paper' ? '异动类策略直接进入观察期' : '已排队前推回测', 'ok');
  } else if (r.status === 409) {
    toast('当前状态不能提交', 'err');
  } else {
    toast(errText(r, '提交失败'), 'err');
  }
  LOADERS.strategies();
}

async function onCancelJob(job, btn){
  btn.disabled = true;
  const r = await api(`/api/trade/jobs/${encodeURIComponent(job.id)}/cancel`, 'POST');
  if (r.ok) {
    const result = r.data && r.data.result;
    toast(result === 'requested' ? '已请求取消,将在处理下一只股票前停止' : '已取消', 'ok');
  } else if (r.status === 409) {
    toast('任务已结束,不可取消', 'err');
  } else if (r.status === 404) {
    toast('任务不存在', 'err');
  } else {
    toast(errText(r, '取消失败'), 'err');
  }
  LOADERS.strategies();
}

function renderStrategyList(list){
  const host = document.getElementById('strategy-list');
  if (!list.length) {
    host.innerHTML = hintCard('还没有策略，用下方表单新建一个');
    return;
  }
  const rows = list.map(s => {
    const canSubmit = SUBMITTABLE.includes(s.status);
    return `<tr data-id="${esc(s.id)}">
      <td>${esc(s.name)}</td>
      <td>${esc(kindLabel(s.kind))}</td>
      <td class="wrap-cell">${esc(poolSummary(s.pool))}</td>
      <td>${strategyStatusHtml(s.status)}</td>
      <td class="wrap-cell">${esc(s.status_reason || '')}</td>
      <td>${esc(fmtTime(s.updated_at))}</td>
      <td><div class="ticket-actions" style="margin-top:0">
        <button type="button" class="btn js-submit"${canSubmit ? '' : ' disabled'}>提交评估</button>
        <button type="button" class="btn ghost js-edit">编辑</button>
        <button type="button" class="linkish js-scorecard">成绩单</button>
      </div></td>
    </tr>`;
  }).join('');
  host.innerHTML = `<div class="card"><div class="tbl-wrap"><table class="tbl">
    <thead><tr><th>名称</th><th>类型</th><th>股票池</th><th>状态</th><th>状态原因</th><th>更新时间</th><th>操作</th></tr></thead>
    <tbody>${rows}</tbody></table></div></div>`;
  host.querySelectorAll('tr[data-id]').forEach(row => {
    const s = strategiesById.get(row.getAttribute('data-id'));
    if (!s) return;
    const submitBtn = row.querySelector('.js-submit');
    submitBtn.onclick = () => onSubmitStrategy(s, submitBtn);
    row.querySelector('.js-edit').onclick = () => editStrategy(s);
    row.querySelector('.js-scorecard').onclick = () => {
      scorecardId = s.id;
      scorecardScroll = true;
      loadScorecard();
    };
  });
}

function renderJobs(list){
  const host = document.getElementById('jobs');
  if (!list.length) {
    host.innerHTML = `<div class="card"><h3 class="form-title">评估任务</h3><div class="hint">还没有评估任务</div></div>`;
    return;
  }
  const rows = list.map(j => {
    const s = strategiesById.get(String(j.strategy_id));
    const cancellable = j.kind === 'walk_forward' && (j.status === 'queued' || j.status === 'running');
    return `<tr data-id="${esc(j.id)}">
      <td>${esc(fmtTime(j.created_at))}</td>
      <td>${esc(s ? s.name : '#' + j.strategy_id)}</td>
      <td>${esc(jobKindLabel(j.kind))}</td>
      <td>${esc(jobStatusLabel(j.status))}</td>
      <td class="wrap-cell">${esc(j.progress || '')}</td>
      <td class="wrap-cell">${esc(j.error || '')}</td>
      <td>${cancellable ? '<button type="button" class="btn ghost js-cancel-job">取消</button>' : ''}</td>
    </tr>`;
  }).join('');
  host.innerHTML = `<div class="card"><h3 class="form-title">评估任务</h3><div class="hint" style="margin-bottom:8px">最近 50 个</div><div class="tbl-wrap"><table class="tbl">
    <thead><tr><th>时间</th><th>策略</th><th>类型</th><th>状态</th><th>进度</th><th>错误</th><th></th></tr></thead>
    <tbody>${rows}</tbody></table></div></div>`;
  const byId = new Map(list.map(j => [String(j.id), j]));
  host.querySelectorAll('tr[data-id]').forEach(row => {
    const j = byId.get(row.getAttribute('data-id'));
    const btn = row.querySelector('.js-cancel-job');
    if (j && btn) btn.onclick = () => onCancelJob(j, btn);
  });
}

async function loadScorecard(){
  const seq = ++scorecardSeq;
  const host = document.getElementById('scorecard');
  const id = scorecardId;
  if (id == null) { host.innerHTML = ''; return; }
  const base = `/api/trade/strategies/${encodeURIComponent(id)}`;
  const [r, ev] = await Promise.all([api(base + '/scorecard'), api(base + '/events')]);
  if (seq !== scorecardSeq) return;
  if (!r.ok || !r.data) {
    host.innerHTML = hintCard(r.status === 404 ? '策略不存在' : loadFailedMsg(r));
    return;
  }
  const sc = r.data;
  const events = ev.ok && Array.isArray(ev.data) ? ev.data.slice(-10).reverse() : [];
  const evHtml = events.length
    ? `<ul class="sc-events">${events.map(e => `<li>${esc(fmtTime(e.at))}　${esc(STRATEGY_STATUS[e.from] || e.from)} → ${esc(STRATEGY_STATUS[e.to] || e.to)}　${esc(e.reason || '')}</li>`).join('')}</ul>`
    : '<div class="hint" style="margin-bottom:12px">暂无状态变更记录</div>';
  const rows = scorecardRows(sc).map(row =>
    `<tr><td>${esc(row[0])}</td>${row.slice(1).map(c => `<td class="num">${esc(c)}</td>`).join('')}</tr>`
  ).join('');
  const loss = sc.execution_loss == null ? '—' : fmtPct(sc.execution_loss);
  host.innerHTML = `<div class="card">
    <div class="sc-head"><b>成绩单：${esc(sc.name)}</b>${strategyStatusHtml(sc.status)}
      <button type="button" class="linkish js-close-scorecard">收起</button></div>
    ${evHtml}
    <div class="tbl-wrap"><table class="tbl">
      <thead><tr><th>指标</th><th class="num">样本内</th><th class="num">样本外</th><th class="num">模拟盘</th><th class="num">实盘</th></tr></thead>
      <tbody>${rows}
        <tr><td>执行损耗(中位数,实盘相对模拟盘多付)</td><td class="num" colspan="4">${esc(loss)}</td></tr>
      </tbody></table></div>
  </div>`;
  host.querySelector('.js-close-scorecard').onclick = () => {
    scorecardId = null;
    ++scorecardSeq;
    host.innerHTML = '';
  };
  if (scorecardScroll) {
    scorecardScroll = false;
    host.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }
}

async function loadStrategies(){
  const seq = ++strategiesSeq;
  loadScorecard(); // 与列表、任务并行拉取
  const [sr, jr] = await Promise.all([api('/api/trade/strategies'), api('/api/trade/jobs')]);
  if (seq !== strategiesSeq) return; // 已有更新的请求，丢弃过时结果
  const listHost = document.getElementById('strategy-list');
  if (!sr.ok || !Array.isArray(sr.data)) {
    failInto(listHost, sr);
  } else {
    strategiesById = new Map(sr.data.map(s => [String(s.id), s]));
    renderStrategyList(sr.data);
    markLoaded(listHost);
  }
  const jobsHost = document.getElementById('jobs');
  if (!jr.ok || !Array.isArray(jr.data)) {
    failInto(jobsHost, jr);
  } else {
    renderJobs(jr.data);
    markLoaded(jobsHost);
  }
}
LOADERS.strategies = loadStrategies;

// 供「待确认」工单卡片的「查看策略成绩单」调用：切到策略标签并展开该策略成绩单
function openStrategy(id){
  scorecardId = id;
  scorecardScroll = true;
  showTab('strategies'); // showTab 总会调用 LOADERS.strategies()，其中拉取成绩单
}

strategyForm().addEventListener('submit', onSaveStrategy);
strategyForm().querySelector('.js-cancel-edit').addEventListener('click', resetStrategyForm);

// ===== Task 5: 风控设置、账户资金与持仓校准（追加区） =====

// 比例(小数)→ 百分数输入框的值,保留 2 位小数(同时去掉 7.999999 这类浮点噪声)。
// 2 位小数让 0.05% 这类滑点原样回显、原样保存,不被 1 位小数舍成 0.1%(4b 终审 M4)。
function pct2(x){
  return Math.round(x * 10000) / 100;
}

// ----- 风控设置 -----

function riskForm(){
  return document.getElementById('risk-form');
}

// GET /api/trade/risk 返回的最近一次原始对象；保存时以此为底，覆盖表单中的字段，
// 未在表单中出现的字段（如未来新增字段）原样带回。
let riskRaw = null;

function riskPanelHtml(){
  return `<div class="card">
    <form id="risk-form" autocomplete="off">
      <h3 class="form-title">风控设置</h3>
      <div class="form-grid">
        <label><input type="checkbox" name="enabled"/> 允许生成工单</label>
        <label>单笔金额上限(元)<input type="number" step="0.01" min="0" name="max_order_amount" required/></label>
        <label>单票仓位上限(%)<input type="number" step="0.01" min="0" max="100" name="max_position_pct" required/></label>
        <label>每日工单上限<input type="number" step="1" min="1" name="max_daily_tickets" required/></label>
        <label>当日亏损停止买入(%)<input type="number" step="0.01" min="0" max="50" name="daily_loss_halt_pct" required/></label>
        <label>同码冷却(分钟)<input type="number" step="1" min="0" name="cooldown_min" required/></label>
        <label>偏离提醒阈值(%)<input type="number" step="0.01" min="0" max="10" name="deviation_th" required/></label>
        <label>默认止损(%)<input type="number" step="0.01" min="0" max="50" name="default_stop_loss_pct" required/></label>
        <label>默认止盈(%)<input type="number" step="0.01" min="0" max="500" name="default_take_profit_pct" required/></label>
        <label>模拟盘滑点(%)<input type="number" step="0.01" min="0" name="slippage" required/></label>
      </div>
      <div id="risk-form-err" class="form-err"></div>
      <div class="ticket-actions"><button type="submit" class="btn js-save-risk">保存</button></div>
    </form>
  </div>
  <div class="card">
    <h3 class="form-title">账户资金</h3>
    <div class="form-grid">
      <label>实盘总资金(元)<input type="number" step="0.01" min="0" id="capital-real"/></label>
      <label>模拟盘总资金(元)<input type="number" step="0.01" min="0" id="capital-paper"/></label>
    </div>
    <div id="capital-err" class="form-err"></div>
    <div class="ticket-actions">
      <button type="button" class="btn js-save-capital" data-account="real">保存实盘资金</button>
      <button type="button" class="btn js-save-capital" data-account="paper">保存模拟盘资金</button>
    </div>
  </div>`;
}

function fillRiskForm(rules){
  const f = riskForm();
  f.elements.enabled.checked = !!rules.enabled;
  f.elements.max_order_amount.value = rules.max_order_amount;
  f.elements.max_position_pct.value = pct2(rules.max_position_pct);
  f.elements.max_daily_tickets.value = rules.max_daily_tickets;
  f.elements.daily_loss_halt_pct.value = pct2(rules.daily_loss_halt_pct);
  f.elements.cooldown_min.value = rules.cooldown_min;
  f.elements.deviation_th.value = pct2(rules.deviation_th);
  f.elements.default_stop_loss_pct.value = pct2(rules.default_stop_loss_pct);
  f.elements.default_take_profit_pct.value = pct2(rules.default_take_profit_pct);
  f.elements.slippage.value = pct2(rules.slippage);
}

async function onSaveRisk(ev){
  ev.preventDefault();
  const f = riskForm();
  const errEl = document.getElementById('risk-form-err');
  errEl.textContent = '';
  const body = Object.assign({}, riskRaw, {
    enabled: f.elements.enabled.checked,
    max_order_amount: Number(f.elements.max_order_amount.value),
    max_position_pct: Number(f.elements.max_position_pct.value) / 100,
    max_daily_tickets: Number(f.elements.max_daily_tickets.value),
    daily_loss_halt_pct: Number(f.elements.daily_loss_halt_pct.value) / 100,
    cooldown_min: Number(f.elements.cooldown_min.value),
    deviation_th: Number(f.elements.deviation_th.value) / 100,
    default_stop_loss_pct: Number(f.elements.default_stop_loss_pct.value) / 100,
    default_take_profit_pct: Number(f.elements.default_take_profit_pct.value) / 100,
    slippage: Number(f.elements.slippage.value) / 100,
  });
  const btn = f.querySelector('.js-save-risk');
  btn.disabled = true;
  const r = await api('/api/trade/risk', 'POST', body);
  btn.disabled = false;
  if (!r.ok) {
    // 400 时保留表单已填的值，只显示错误（Task 3/4 的约定）
    errEl.textContent = errText(r, '保存失败');
    return;
  }
  riskRaw = body;
  toast('已保存风控设置', 'ok');
  loadOverview();
}

async function onSaveCapital(btn){
  const account = btn.getAttribute('data-account');
  const input = document.getElementById('capital-' + account);
  const errEl = document.getElementById('capital-err');
  errEl.textContent = '';
  const total = Number(input.value);
  if (!(total > 0)) { errEl.textContent = '总资金必须为正数'; return; }
  btn.disabled = true;
  const r = await api('/api/trade/capital', 'POST', { account, total });
  btn.disabled = false;
  if (!r.ok) {
    errEl.textContent = errText(r, '保存失败');
    return;
  }
  toast('已保存账户资金', 'ok');
  loadOverview();
}

let riskSeq = 0;

async function loadRisk(){
  const seq = ++riskSeq;
  const panel = document.getElementById('panel-risk');
  const [rr, ov] = await Promise.all([api('/api/trade/risk'), api('/api/trade/overview')]);
  if (seq !== riskSeq) return; // 已有更新的请求，丢弃过时结果
  if (!rr.ok || !rr.data) { failInto(panel, rr); return; }
  panel.innerHTML = riskPanelHtml();
  markLoaded(panel);
  riskRaw = rr.data;
  fillRiskForm(rr.data);
  const acc = (ov.ok && ov.data && ov.data.accounts) || {};
  document.getElementById('capital-real').value = acc.real ? acc.real.total_capital : '';
  document.getElementById('capital-paper').value = acc.paper ? acc.paper.total_capital : '';
  riskForm().addEventListener('submit', onSaveRisk);
  panel.querySelectorAll('.js-save-capital').forEach(btn => {
    btn.onclick = () => onSaveCapital(btn);
  });
}
LOADERS.risk = loadRisk;

// ----- 持仓校准 -----

function calibrateForm(){
  return document.getElementById('calibrate-form');
}

function positionsPanelHtml(){
  return `<form id="calibrate-form" class="card" autocomplete="off">
    <h3 class="form-title">校准持仓（仅实盘）</h3>
    <div class="hint" style="margin-bottom:10px">以券商账户为准修正系统记录；数量填 0 表示已清仓</div>
    <div class="form-grid">
      <label>代码<input type="text" name="code" maxlength="6" required/></label>
      <label>数量<input type="number" step="1" min="0" name="qty" required/></label>
      <label>成本价<input type="number" step="0.001" min="0" name="avg_cost"/></label>
      <label class="full">原因<input type="text" name="reason" required/></label>
    </div>
    <div id="calibrate-form-err" class="form-err"></div>
    <div class="ticket-actions"><button type="submit" class="btn js-save">提交校准</button></div>
  </form>
  <div class="card"><h3 class="form-title">实盘持仓</h3><div id="positions-real" class="tbl-wrap"></div></div>
  <div class="card"><h3 class="form-title">模拟盘持仓</h3><div id="positions-paper" class="tbl-wrap"></div></div>
  <div class="card"><h3 class="form-title">校准记录</h3><div id="adjusts-list" class="tbl-wrap"></div></div>`;
}

// 止损 / 止盈 / 移动止盈输入框留空即视为清除（提交 null）；移动止盈以 % 展示，提交时 ÷ 100
function positionRowHtml(p){
  const q = p.quote;
  const priceHtml = q
    ? `<span class="${q.stale ? 'stale-q' : ''}">${esc(fmtMoney(q.price))}</span>${q.stale ? ' <span class="tag urgent">延迟</span>' : ''}`
    : '—';
  const trailingDisplay = p.trailing_pct == null ? '' : pct2(p.trailing_pct);
  return `<tr data-code="${esc(p.code)}">
    <td>${esc(p.code)}</td>
    <td>${esc(p.qty)}</td>
    <td>${esc(p.sellable)}</td>
    <td>${esc(fmtMoney(p.avg_cost))}</td>
    <td>${priceHtml}</td>
    <td>${esc(fmtMoney(p.market_value))}</td>
    <td>${esc(fmtPct(p.pnl_pct))}</td>
    <td><div class="fill-form">
      <input type="number" step="0.001" min="0" class="js-stop-loss" placeholder="止损" value="${esc(p.stop_loss == null ? '' : p.stop_loss)}"/>
      <input type="number" step="0.001" min="0" class="js-take-profit" placeholder="止盈" value="${esc(p.take_profit == null ? '' : p.take_profit)}"/>
      <input type="number" step="0.01" min="0" max="50" class="js-trailing" placeholder="移动止盈%" value="${esc(trailingDisplay)}"/>
      <button type="button" class="btn js-save-exit">保存</button>
    </div></td>
  </tr>`;
}

function positionsTableHtml(list){
  if (!list.length) return '<div class="hint">无持仓</div>';
  return `<table class="tbl">
    <thead><tr><th>代码</th><th>数量</th><th>可卖</th><th>成本</th><th>现价</th><th>市值</th><th>盈亏</th><th>止损 / 止盈 / 移动止盈(%)</th></tr></thead>
    <tbody>${list.map(positionRowHtml).join('')}</tbody></table>`;
}

async function onSaveExitLevels(account, p, row){
  const parseOpt = el => (el.value.trim() === '' ? null : Number(el.value));
  const stop_loss = parseOpt(row.querySelector('.js-stop-loss'));
  const take_profit = parseOpt(row.querySelector('.js-take-profit'));
  const trailingPctInput = parseOpt(row.querySelector('.js-trailing'));
  const trailing_pct = trailingPctInput === null ? null : trailingPctInput / 100;
  const btn = row.querySelector('.js-save-exit');
  btn.disabled = true;
  const r = await api('/api/trade/positions/exit-levels', 'POST', {
    account, code: p.code, stop_loss, take_profit, trailing_pct,
  });
  btn.disabled = false;
  if (!r.ok) {
    toast(errText(r, '保存失败'), 'err');
    return;
  }
  toast('已保存止盈止损', 'ok');
  LOADERS.positions();
}

function bindPositionRows(host, account, list){
  const byCode = new Map(list.map(p => [p.code, p]));
  host.querySelectorAll('tr[data-code]').forEach(row => {
    const p = byCode.get(row.getAttribute('data-code'));
    if (!p) return;
    row.querySelector('.js-save-exit').onclick = () => onSaveExitLevels(account, p, row);
  });
}

function beforeAfterCell(x){
  return x ? `${esc(x.qty)} / ${esc(fmtMoney(x.avg_cost))}` : '无';
}

function adjustsTableHtml(list){
  if (!list.length) return '<div class="hint">暂无校准记录</div>';
  const rows = list.map(a => `<tr>
      <td>${esc(fmtTime(a.at))}</td>
      <td>${esc(a.code)}</td>
      <td>${beforeAfterCell(a.before)}</td>
      <td>${beforeAfterCell(a.after)}</td>
      <td class="wrap-cell">${esc(a.reason)}</td>
    </tr>`).join('');
  return `<table class="tbl">
    <thead><tr><th>时间</th><th>代码</th><th>改前数量 / 成本</th><th>改后数量 / 成本</th><th>原因</th></tr></thead>
    <tbody>${rows}</tbody></table>`;
}

async function onCalibrate(ev){
  ev.preventDefault();
  const f = calibrateForm();
  const errEl = document.getElementById('calibrate-form-err');
  errEl.textContent = '';
  const body = {
    code: f.elements.code.value.trim(),
    qty: Number(f.elements.qty.value),
    avg_cost: f.elements.avg_cost.value.trim() === '' ? 0 : Number(f.elements.avg_cost.value),
    reason: f.elements.reason.value.trim(),
  };
  const btn = f.querySelector('.js-save');
  btn.disabled = true;
  const r = await api('/api/trade/positions/calibrate', 'POST', body);
  btn.disabled = false;
  if (!r.ok) {
    // 400 时保留表单已填的值，只显示错误
    errEl.textContent = errText(r, '提交失败');
    return;
  }
  toast('已校准持仓', 'ok');
  f.reset();
  LOADERS.positions();
}

let positionsSeq = 0;

async function loadPositions(){
  const seq = ++positionsSeq;
  const panel = document.getElementById('panel-positions');
  const [pr, ar] = await Promise.all([
    api('/api/trade/positions'),
    api('/api/trade/positions/adjusts'),
  ]);
  if (seq !== positionsSeq) return; // 已有更新的请求，丢弃过时结果
  if (!pr.ok || !pr.data) { failInto(panel, pr); return; }
  panel.innerHTML = positionsPanelHtml();
  markLoaded(panel);
  const realList = Array.isArray(pr.data.real) ? pr.data.real : [];
  const paperList = Array.isArray(pr.data.paper) ? pr.data.paper : [];
  const realHost = document.getElementById('positions-real');
  realHost.innerHTML = positionsTableHtml(realList);
  bindPositionRows(realHost, 'real', realList);
  const paperHost = document.getElementById('positions-paper');
  paperHost.innerHTML = positionsTableHtml(paperList);
  bindPositionRows(paperHost, 'paper', paperList);
  const adjustsHost = document.getElementById('adjusts-list');
  adjustsHost.innerHTML = ar.ok && Array.isArray(ar.data) ? adjustsTableHtml(ar.data) : hintCard(loadFailedMsg(ar));
  calibrateForm().addEventListener('submit', onCalibrate);
}
LOADERS.positions = loadPositions;

// ===== 初始化与轮询（须在脚本末尾：各标签的 LOADERS 注册完后再首次加载） =====

function initialTab(){
  const h = location.hash.slice(1);
  return TABS.includes(h) ? h : 'pending';
}

showTab(initialTab());
loadMe();
loadOverview();

function refreshVisible(){
  if (document.hidden) return;
  loadOverview();
  if (currentTab === 'pending' && typeof LOADERS.pending === 'function') LOADERS.pending();
}

setInterval(refreshVisible, 15000);

// 页面从后台回到前台立即刷新:切走期间价格可能已大幅变化,不能让用户对着旧的偏离值确认(4b 终审 I1)
document.addEventListener('visibilitychange', refreshVisible);
</script>
</body>
</html>
"##;

/// `/trade/t/:id` 签名链接落地页（design decision 5）：挂在 `public` 组，无需登录；
/// 页面本身不校验签名，只从 URL 取工单号与 `sig`，调用已统一处理 404 的签名 API
/// （`GET /api/trade/t/:id?sig=`、`POST /api/trade/t/:id/confirm?sig=`）。
/// 本页独立，不复用 `TRADE_HTML` 的脚本；`esc`、`confirmState` 等几个纯工具函数在本页
/// 再写一份（两页互不依赖；名单与字节级一致由 `shared_helpers_are_identical_in_both_pages` 守住）。
pub const SIGNED_TICKET_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<meta name="referrer" content="no-referrer">
<title>xlh 工单确认</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Arial,sans-serif;background:#f5f6fa;color:#2c3e50;padding:20px}
.wrap{max-width:480px;margin:0 auto}
.card{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:18px;box-shadow:0 1px 4px rgba(0,0,0,.06)}
.hint{color:#7f8c8d;font-size:.9rem}
.btn{padding:9px 16px;border:1px solid #c0392b;border-radius:6px;background:#c0392b;color:#fff;cursor:pointer;font-size:.95rem;width:100%;margin-top:12px}
.btn.warn{background:#d35400;border-color:#d35400}
.btn:disabled{opacity:.5;cursor:not-allowed}
.side-buy{color:#c0392b;font-weight:600}
.side-sell{color:#16a34a;font-weight:600}
.tag{display:inline-block;padding:1px 8px;border-radius:10px;background:#eef1f4;color:#555;font-size:.8rem;margin-left:6px}
.ticket-head{display:flex;flex-wrap:wrap;align-items:center;gap:4px;margin-bottom:8px}
.ticket-head .code{font-size:1.1rem;font-weight:700;margin-right:6px}
.countdown{margin-left:auto;font-variant-numeric:tabular-nums;color:#7f8c8d;font-size:.9rem}
.ticket-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:6px 16px;margin:8px 0;font-size:.9rem}
.ticket-grid .k{color:#7f8c8d;margin-right:4px}
.dev-bad{color:#c0392b;font-weight:600}
.ticket-reason{font-size:.9rem;margin:6px 0;white-space:pre-wrap;word-break:break-word}
.err{color:#c0392b;font-size:.85rem;margin-top:10px;white-space:pre-wrap}
.err:empty{display:none}
a{color:#2563eb}
</style>
</head>
<body>
<div class="wrap">
  <div id="app"><div class="card"><div class="hint">加载中…</div></div></div>
  <div id="load-err" class="err"></div>
</div>
<script>
// esc / confirmState / fmtMoney / fmtPct / fmtCountdown / sideLabel / sourceLabel /
// statusLabel 与 TRADE_HTML 保持一致，由测试 shared_helpers_are_identical_in_both_pages 校验
// （本页独立、不 import TRADE_HTML 的脚本，这几个小工具函数按 brief 允许重复实现，
// 用单测保证两边字节级一致，不会悄悄走样；其余逻辑均为本页独有，不与 TRADE_HTML 共享）。

function esc(s){
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

function fmtMoney(x){
  if (x === null || x === undefined || typeof x !== 'number' || isNaN(x)) return '—';
  return x.toFixed(2);
}

function fmtPct(x){
  if (x === null || x === undefined || typeof x !== 'number' || isNaN(x)) return '—';
  return (x * 100).toFixed(1) + '%';
}

function fmtCountdown(ms){
  const s = Math.max(0, Math.floor(ms / 1000));
  const p = n => String(n).padStart(2, '0');
  return `剩 ${p(Math.floor(s / 60))}:${p(s % 60)}`;
}

function sideLabel(side){
  return { buy: '买入', sell: '卖出' }[side] || (side || '—');
}

function sourceLabel(src){
  return { exit: '止盈止损', strategy: '日线策略', mover: '异动', manual: '手动' }[src] || (src || '—');
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

// 与 Task 3 待确认卡片相同的规则（设计裁决 4）：无报价或行情延迟禁用；
// 偏离超阈值时按钮初始就带二次确认文案，一次点击即带 ack。
function confirmState(ticket) {
  if (!ticket.quote || ticket.quote.stale) return { text: '行情延迟', disabled: true, ack: false };
  const deviated = ticket.deviation != null && ticket.deviation > ticket.deviation_th;
  return deviated
    ? { text: `价格已偏离 ${fmtPct(ticket.deviation)},仍要确认`, disabled: false, ack: true, warn: true }
    : { text: '确认', disabled: false, ack: false };
}

const PATH_MATCH = location.pathname.match(/\/trade\/t\/([^/]+)/);
const ticketId = PATH_MATCH ? PATH_MATCH[1] : '';
const sig = new URLSearchParams(location.search).get('sig') || '';
const apiBase = `/api/trade/t/${encodeURIComponent(ticketId)}`;

let stopped = false;
let expiresMs = NaN;
// 最近一次确认失败的提示:loadTicket 重绘 #app 后重新贴回,不被立即抹掉(4b 终审 I2)。
// 新的确认尝试,或工单状态与出错时不同(已有终态可看)时清空。
let lastErr = '';
let lastErrStatus = null;
let current = null; // 最近一次渲染的工单
let countdownTimer = null;
let refreshTimer = null;

// 到达终态（已确认 / 链接失效）后停掉倒计时与 15 秒刷新，避免徒劳的后台请求与计时器泄漏。
function stopTimers(){
  if (countdownTimer !== null) { clearInterval(countdownTimer); countdownTimer = null; }
  if (refreshTimer !== null) { clearInterval(refreshTimer); refreshTimer = null; }
}

function showInvalid(){
  stopped = true;
  stopTimers();
  document.getElementById('app').innerHTML = '<div class="card"><div class="hint">链接无效或已过期</div></div>';
}

function showDone(){
  stopped = true;
  stopTimers();
  document.getElementById('app').innerHTML =
    '<div class="card">已确认。请在券商 App 下单，完成后登录交易页回填成交<br/><a href="/trade">前往交易页</a></div>';
}

// 只有待确认且未到期的工单可以确认(4b 终审 M1);有效期未知时交给服务端判断
function canConfirm(t){
  if (!t || t.status !== 'pending') return false;
  return !isFinite(expiresMs) || expiresMs > Date.now();
}

function updateCountdown(){
  const el = document.getElementById('countdown');
  if (!el) return;
  if (!isFinite(expiresMs)) { el.textContent = '有效期未知'; return; }
  const left = expiresMs - Date.now();
  el.textContent = left > 0 ? fmtCountdown(left) : '已过期';
  const btn = document.getElementById('confirm-btn');
  if (left <= 0 && btn && !btn.disabled && current && current.status === 'pending') {
    btn.disabled = true;
    btn.classList.remove('warn');
    btn.textContent = '已过期';
  }
}
countdownTimer = setInterval(updateCountdown, 1000);

function render(t){
  current = t;
  // 倒计时以服务端剩余秒数为准:收到响应时刻 + 秒数 = 截止时间(设计裁决 4)。
  expiresMs = Date.now() + Number(t.expires_in_secs || 0) * 1000;
  const cs = confirmState(t);
  const ok = canConfirm(t);
  const btnText = ok ? cs.text : (t.status === 'pending' ? '已过期' : `工单${statusLabel(t.status)}`);
  const btnWarn = ok && cs.warn;
  const btnDisabled = !ok || cs.disabled;
  const devBad = t.deviation != null && t.deviation > t.deviation_th;
  const price = t.quote ? fmtMoney(t.quote.price) : '—';
  const note = t.ai_note
    ? `<div class="ticket-reason"><span class="k hint">AI 说明：</span>${esc(t.ai_note)}</div>`
    : '';
  const sideCls = t.side === 'buy' ? 'side-buy' : (t.side === 'sell' ? 'side-sell' : '');
  const nameHtml = t.name ? ` <span class="hint">${esc(t.name)}</span>` : '';
  document.getElementById('app').innerHTML = `<div class="card">
    <div class="ticket-head">
      <span class="code">${esc(t.code)}</span>${nameHtml}<span class="${sideCls}">${esc(sideLabel(t.side))}</span>
      <span class="tag">${esc(sourceLabel(t.source))}</span>
      <span class="tag" id="ticket-status">${esc(statusLabel(t.status))}</span>
      <span class="countdown" id="countdown"></span>
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
    <button type="button" id="confirm-btn" class="btn${btnWarn ? ' warn' : ''}"${btnDisabled ? ' disabled' : ''}>${esc(btnText)}</button>
    <div id="confirm-err" class="err"></div>
  </div>`;
  document.getElementById('confirm-btn').onclick = () => onConfirm(cs.ack, t.deviation);
  document.getElementById('confirm-err').textContent = lastErr;
  updateCountdown();
}

// 轮询 / 刷新失败(非 404):保留当前视图,只显示一行临时错误,下一次轮询继续(4b 终审 M2)
function showLoadErr(msg){
  const el = document.getElementById('load-err');
  if (el) el.textContent = msg;
}

// 每个 await 之后都重新检查 stopped:等待期间可能已确认成功或判定链接失效,
// 过时的响应不得把终态页面重新画回工单卡片(4b 终审 M3)。
async function loadTicket(){
  if (stopped) return;
  let resp;
  try {
    resp = await fetch(`${apiBase}?sig=${encodeURIComponent(sig)}`);
  } catch (e) {
    if (stopped) return;
    showLoadErr('网络错误，稍后自动重试');
    return;
  }
  if (stopped) return;
  if (resp.status === 404) { showInvalid(); return; }
  let data = null;
  try { data = await resp.json(); } catch (e) { data = null; }
  if (stopped) return;
  if (!resp.ok || !data) {
    showLoadErr(`加载失败(${resp.status})，稍后自动重试`);
    return;
  }
  showLoadErr('');
  if (lastErr && data.status !== lastErrStatus) lastErr = '';
  render(data);
}

const CONFIRM_ERR = {
  already_handled: '该工单已处理或已过期',
  stale_quote: '行情延迟，暂不能确认',
  kill_switch: '管理员已暂停交易',
};

// 与 TRADE_HTML 同一约定:带 ack 时附上按钮上显示的偏离(ack_max_deviation),
// 服务端据此拒绝「用户没看到的更大偏离」(4b 终审 I1)。
async function onConfirm(ack, shownDev){
  const btn = document.getElementById('confirm-btn');
  const errEl = document.getElementById('confirm-err');
  lastErr = '';
  errEl.textContent = '';
  btn.disabled = true;
  const body = { ack_deviation: ack };
  if (ack && typeof shownDev === 'number' && isFinite(shownDev) && shownDev >= 0) body.ack_max_deviation = shownDev;
  let resp;
  try {
    resp = await fetch(`${apiBase}/confirm?sig=${encodeURIComponent(sig)}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    });
  } catch (e) {
    if (stopped) return;
    btn.disabled = false;
    lastErr = '网络错误，请重试';
    lastErrStatus = current ? current.status : null;
    errEl.textContent = lastErr;
    return;
  }
  if (stopped) return;
  if (resp.status === 404) { showInvalid(); return; }
  let data = null;
  try { data = await resp.json(); } catch (e) { data = null; }
  if (stopped) return;
  if (resp.ok) { showDone(); return; }
  // 等待期间 15 秒轮询可能已重绘 #app:按 id 重新取当前的按钮与错误行
  const liveBtn = document.getElementById('confirm-btn') || btn;
  const liveErr = document.getElementById('confirm-err') || errEl;
  const code = data && data.code;
  if (code === 'deviation') {
    const dev = data.deviation;
    const btn = liveBtn;
    btn.textContent = `价格已偏离 ${fmtPct(dev)},仍要确认`;
    btn.classList.add('warn');
    btn.disabled = false;
    btn.onclick = () => onConfirm(true, dev);
    return;
  }
  lastErr = CONFIRM_ERR[code] || (data && data.error) || '确认失败';
  lastErrStatus = current ? current.status : null;
  liveErr.textContent = lastErr;
  loadTicket();
}

if (!ticketId) {
  showInvalid();
} else {
  loadTicket();
  refreshTimer = setInterval(loadTicket, 15000);
  // 回到前台立即刷新,不让用户对着切走前的旧偏离值确认(4b 终审 I1)
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) loadTicket();
  });
}
</script>
</body>
</html>
"##;

/// 生产签名链接落地页：不查会话、不做任何鉴权（由 JS 调 API，API 已统一 404）。
/// 响应头 `Referrer-Policy: no-referrer`、`Cache-Control: no-store`，
/// 防止签名通过 Referer 外泄（4a 终审 M8）。
pub async fn signed_ticket_page() -> Response {
    let mut resp = Html(SIGNED_TICKET_HTML).into_response();
    let headers = resp.headers_mut();
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("cache-control", "no-store".parse().unwrap());
    resp
}

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
    async fn strategy_tab_has_form_scorecard_and_jobs() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "id=\"strategy-form\"",
            "id=\"scorecard\"",
            "id=\"jobs\"",
            "function scorecardRows(",
            "function openStrategy(",
            "LOADERS.strategies",
            "/api/trade/strategies",
            "/api/trade/jobs",
            "样本内",
            "样本外",
            "模拟盘",
            "实盘",
            "执行损耗",
            "reversioned",
            "已请求取消",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }

    #[tokio::test]
    async fn index_links_to_trade() {
        assert!(crate::web::page::INDEX_HTML.contains("href=\"/trade\""));
    }

    #[tokio::test]
    async fn risk_and_positions_tabs_have_forms() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "id=\"risk-form\"",
            "id=\"calibrate-form\"",
            "LOADERS.risk",
            "LOADERS.positions",
            "/api/trade/risk",
            "/api/trade/capital",
            "/api/trade/positions/calibrate",
            "/api/trade/positions/exit-levels",
            "/api/trade/positions/adjusts",
            "max_position_pct",
            "default_stop_loss_pct",
            "数量填 0 表示已清仓",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }

    #[tokio::test]
    async fn signed_page_is_public_and_never_sends_referrer() {
        let st = state();
        let (s, body, h) = get(&st, "/trade/t/1?sig=abc", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(h.get("referrer-policy").unwrap(), "no-referrer");
        assert_eq!(h.get("cache-control").unwrap(), "no-store");
        assert!(body.contains("name=\"referrer\" content=\"no-referrer\""));
        assert!(body.contains("/api/trade/t/"));
        assert!(body.contains("链接无效或已过期"));
        assert!(body.contains("function confirmState("));
        assert!(!body.contains("http://") && !body.contains("https://"));
    }

    /// 倒计时以服务端剩余秒数为准(4c,设计裁决 4):两页都用 `expires_in_secs` 而非解析
    /// `expires_at` 字符串;`TRADE_HTML` 把截止时间存到卡片的 `data-deadline`。
    /// 来源标签文案按设计裁决 5 统一。
    #[tokio::test]
    async fn countdown_uses_server_seconds_and_source_labels_follow_spec() {
        for page in [
            crate::web::trade_page::TRADE_HTML,
            crate::web::trade_page::SIGNED_TICKET_HTML,
        ] {
            assert!(
                page.contains("expires_in_secs"),
                "倒计时应以服务端剩余秒数为准"
            );
            for label in ["止盈止损", "日线策略", "异动", "手动"] {
                assert!(page.contains(label), "缺来源文案 {label}");
            }
        }
        assert!(crate::web::trade_page::TRADE_HTML.contains("data-deadline"));
    }

    /// 4b 终审 I1 / I2 / I4 / M1~M6 的页面行为标记:偏离确认带上显示的偏离值、
    /// 页面回到前台即刷新、轮询失败不清空已渲染内容、授权 403 中文提示、
    /// 签名页展示状态 / 保留错误 / 加载失败不判链接失效。
    #[test]
    fn pages_carry_final_review_behaviour_markers() {
        let trade = crate::web::trade_page::TRADE_HTML;
        let signed = crate::web::trade_page::SIGNED_TICKET_HTML;
        for body in [trade, signed] {
            for s in ["ack_max_deviation", "visibilitychange"] {
                assert!(body.contains(s), "缺 {s}");
            }
        }
        for s in [
            "function failInto(",
            "function errText(",
            "授权已过期,请在主页续期",
            "license_required",
            "function pct2(",
            "step=\"0.01\" min=\"0\" max=\"100\" name=\"max_position_pct\"",
        ] {
            assert!(trade.contains(s), "TRADE_HTML 缺 {s}");
        }
        assert!(
            !trade.contains("function round1("),
            "比例字段不得再按 1 位小数回显"
        );
        assert!(!trade.contains("step=\"0.1\""), "比例字段步长须为 0.01");
        for s in [
            "id=\"load-err\"",
            "id=\"ticket-status\"",
            "let lastErr",
            "if (stopped) return;",
            "function canConfirm(",
        ] {
            assert!(signed.contains(s), "SIGNED_TICKET_HTML 缺 {s}");
        }
    }

    /// 比例 → 百分数只换算一次:`pct2` 自己 ×100,调用处必须传原始比例(4b 终审 M4 复审)。
    /// 钉住每个回显行的确切写法,并确认 `pct2` 的实现就是「×100 后保留 2 位小数」。
    #[test]
    fn ratio_fields_convert_to_percent_exactly_once() {
        let trade = crate::web::trade_page::TRADE_HTML;
        let pct2 = extract_fn(trade, "pct2").expect("TRADE_HTML 应有 function pct2(");
        assert!(pct2.contains("Math.round(x * 10000) / 100"), "{pct2}");
        for field in [
            "max_position_pct",
            "daily_loss_halt_pct",
            "deviation_th",
            "default_stop_loss_pct",
            "default_take_profit_pct",
            "slippage",
        ] {
            let line = format!("f.elements.{field}.value = pct2(rules.{field});");
            assert!(trade.contains(&line), "缺 {line}");
        }
        assert!(trade.contains(
            "const trailingDisplay = p.trailing_pct == null ? '' : pct2(p.trailing_pct);"
        ));
        let calls: Vec<&str> = trade
            .match_indices("pct2(")
            .map(|(i, _)| &trade[i..])
            .collect();
        for c in calls {
            let arg = &c[..c.find(')').unwrap()];
            assert!(!arg.contains("100"), "pct2 的参数不得再 ×100: {arg}");
        }
    }

    /// 从 `src` 里抽出 `function NAME(...) { ... }` 的完整源文本（从 `function NAME(`
    /// 起，用花括号计数找到与函数体开括号匹配的闭括号，含头尾）。JS 里对象字面量、
    /// 模板字符串 `${...}` 内的花括号都天然成对，计数法足够定位到函数结尾。
    fn extract_fn<'a>(src: &'a str, name: &str) -> Option<&'a str> {
        let needle = format!("function {name}(");
        let start = src.find(&needle)?;
        let rest = &src[start..];
        let brace_rel = rest.find('{')?;
        let mut depth = 0i32;
        let mut end_rel = None;
        for (i, ch) in rest[brace_rel..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_rel = Some(brace_rel + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(&rest[..end_rel?])
    }

    /// 签名链接落地页独立、不复用 `TRADE_HTML` 的脚本；brief 只明确允许 `esc` /
    /// `confirmState` 在两页各写一份，但实现里为了保持页面独立还额外复制了几个纯
    /// 格式化小工具函数。这些「允许重复」的函数必须两边字节级一致（trim 后），
    /// 否则就是重复代码走偏而不是有意的独立实现——用这个测试守住这条线，而不是
    /// 只靠代码注释自证。
    #[test]
    fn shared_helpers_are_identical_in_both_pages() {
        let trade = crate::web::trade_page::TRADE_HTML;
        let signed = crate::web::trade_page::SIGNED_TICKET_HTML;
        for name in [
            "esc",
            "confirmState",
            "fmtMoney",
            "fmtPct",
            "fmtCountdown",
            "sideLabel",
            "sourceLabel",
            "statusLabel",
        ] {
            // 名单里的函数两页都必须有：缺了就失败，而不是跳过（4b 终审 M9）
            let signed_fn = extract_fn(signed, name)
                .unwrap_or_else(|| panic!("SIGNED_TICKET_HTML 里应该有 function {name}("));
            let trade_fn = extract_fn(trade, name)
                .unwrap_or_else(|| panic!("TRADE_HTML 里应该有 function {name}("));
            assert_eq!(
                signed_fn.trim(),
                trade_fn.trim(),
                "两页的 {name} 实现应保持一致（如需故意不同，请在这里加白名单并写明原因）"
            );
        }
    }
}

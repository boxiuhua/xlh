/// 本地回测界面首页：三 Tab（单次/对比/寻优）+ 结果 iframe（纯静态，无外部依赖）。
pub const INDEX_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8"/>
<meta name="viewport" content="width=device-width,initial-scale=1"/>
<title>xlh 回测</title>
<style>
*,*::before,*::after{box-sizing:border-box;margin:0;padding:0}
body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Arial,sans-serif;background:#f5f6fa;color:#2c3e50}
.wrap{max-width:1200px;margin:0 auto;padding:20px 16px}
h1{font-size:1.5rem;color:#1a252f;margin-bottom:14px}
.tabs{display:flex;gap:6px;margin-bottom:14px;border-bottom:2px solid #e0e4ea}
.tab{padding:9px 18px;cursor:pointer;border:none;background:none;font-size:.95rem;color:#7f8c8d;border-bottom:2px solid transparent;margin-bottom:-2px}
.tab.active{color:#c0392b;border-bottom-color:#c0392b;font-weight:600}
.panel{display:none}
.panel.active{display:block}
.card{background:#fff;border:1px solid #e0e4ea;border-radius:10px;padding:18px;margin-bottom:16px;box-shadow:0 1px 4px rgba(0,0,0,.06)}
.row{display:flex;flex-wrap:wrap;gap:12px 18px;align-items:flex-end}
.field{display:flex;flex-direction:column;gap:4px}
.field label{font-size:.8rem;color:#5a6a7a}
.field input,.field select{padding:7px 9px;border:1px solid #cfd6e0;border-radius:6px;font-size:.9rem}
.field input[type=number]{width:120px}
button.run{padding:9px 22px;background:#c0392b;color:#fff;border:none;border-radius:6px;font-size:.95rem;cursor:pointer;margin-top:12px}
button.run:disabled{opacity:.5;cursor:wait}
.params.hidden{display:none}
.crow{border:1px solid #eaecef;border-radius:8px;padding:12px;margin-bottom:10px;background:#fafbfc}
.crow .row{align-items:flex-end}
button.small{padding:5px 12px;border:1px solid #cfd6e0;background:#fff;border-radius:6px;cursor:pointer;font-size:.85rem}
button.del{color:#c0392b;border-color:#e8b9b3}
#result{width:100%;height:1400px;border:1px solid #e0e4ea;border-radius:10px;background:#fff;margin-top:8px}
.hint{color:#7f8c8d;font-size:.85rem;margin-top:8px}
.combo{position:relative}
.fund-dropdown{position:absolute;left:0;top:100%;z-index:20;min-width:240px;background:#fff;border:1px solid #cfd6e0;border-radius:6px;max-height:260px;overflow:auto;box-shadow:0 4px 12px rgba(0,0,0,.1);display:none}
.fund-dropdown.show{display:block}
.fund-item{padding:6px 10px;cursor:pointer;font-size:.88rem;white-space:nowrap}
.fund-item:hover,.fund-item.active{background:#f0f2f5}
.fund-item .code{color:#c0392b;font-weight:600;margin-right:8px}
</style>
</head>
<body>
<div class="wrap">
  <h1>xlh 基金回测</h1>
  <div class="card" id="sync-card">
    <div class="row" style="align-items:flex-end">
      <strong style="margin-right:8px">数据同步</strong>
      <button class="small" id="sync-all">同步全部已缓存</button>
      <div class="field combo"><label>基金代码</label><input id="sync-code" placeholder="如 161725"/></div>
      <button class="small" id="sync-one">同步此基金</button>
    </div>
    <div id="sync-result" class="hint" style="margin-top:8px"></div>
  </div>
  <div class="tabs">
    <button class="tab active" data-tab="single">单次</button>
    <button class="tab" data-tab="compare">对比</button>
    <button class="tab" data-tab="optimize">寻优</button>
    <button class="tab" data-tab="diagnose">诊断</button>
  </div>

  <!-- 单次 -->
  <div class="panel active" id="panel-single">
    <div class="card">
      <form id="f-single" class="row">
        <div class="field"><label>基金代码</label><input name="fund_code" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" name="start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" name="end" value="2024-12-31"/></div>
        <div class="field"><label>策略</label>
          <select name="strategy" class="strat">
            <option value="dca">普通定投</option>
            <option value="smart_dca" selected>智能定投</option>
            <option value="trend">均线择时</option>
            <option value="rsi">RSI超买超卖</option>
            <option value="adaptive">自适应</option>
          </select>
        </div>
        <span class="params" data-for="dca smart_dca adaptive" style="display:contents">
          <div class="field"><label>周期</label><select name="period"><option value="monthly">月</option><option value="weekly">周</option></select></div>
          <div class="field"><label>定投日</label><input type="number" name="day" value="1"/></div>
          <div class="field"><label>每期金额</label><input type="number" name="base_amount" value="1000"/></div>
        </span>
        <span class="params" data-for="smart_dca" style="display:contents">
          <div class="field"><label>均线窗口</label><input type="number" name="ma_window" value="250"/></div>
          <div class="field"><label>k 系数</label><input type="number" step="0.1" name="k" value="1.0"/></div>
        </span>
        <span class="params" data-for="trend" style="display:contents">
          <div class="field"><label>短窗口</label><input type="number" name="short_window" value="20"/></div>
          <div class="field"><label>长窗口</label><input type="number" name="long_window" value="60"/></div>
          <div class="field"><label>每次金额</label><input type="number" name="amount" value="1000"/></div>
        </span>
        <span class="params" data-for="rsi" style="display:contents">
          <div class="field"><label>RSI周期</label><input type="number" name="rsi_window" value="14"/></div>
          <div class="field"><label>超卖线</label><input type="number" name="oversold" value="30"/></div>
          <div class="field"><label>超买线</label><input type="number" name="overbought" value="70"/></div>
          <div class="field"><label>每次金额</label><input type="number" name="amount" value="1000"/></div>
        </span>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" name="buy_rate" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" name="initial_cash" value="0"/></div>
      </form>
      <button class="run" id="run-single">运行</button>
    </div>
  </div>

  <!-- 对比 -->
  <div class="panel" id="panel-compare">
    <div class="card">
      <div class="row">
        <div class="field"><label>默认基金代码</label><input id="cmp-fund" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" id="cmp-start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" id="cmp-end" value="2024-12-31"/></div>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" id="cmp-buy" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" id="cmp-cash" value="0"/></div>
      </div>
      <div id="compare-rows" style="margin-top:14px"></div>
      <button class="small" id="add-row">+ 添加策略</button>
      <button class="run" id="run-compare">运行对比</button>
      <div class="hint">每行一个命名策略；基金代码留空则用默认基金。</div>
    </div>
  </div>

  <!-- 寻优 -->
  <div class="panel" id="panel-optimize">
    <div class="card">
      <div class="row">
        <div class="field"><label>基金代码</label><input id="opt-fund" value="161725"/></div>
        <div class="field"><label>起始日</label><input type="date" id="opt-start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" id="opt-end" value="2024-12-31"/></div>
        <div class="field"><label>策略</label>
          <select id="opt-strat" class="strat-opt">
            <option value="dca">普通定投</option>
            <option value="smart_dca" selected>智能定投</option>
            <option value="trend">均线择时</option>
            <option value="rsi">RSI超买超卖</option>
            <option value="adaptive">自适应</option>
          </select>
        </div>
        <div class="field"><label>排序指标</label>
          <select id="opt-metric">
            <option value="sharpe" selected>夏普</option>
            <option value="total_return">总收益</option>
            <option value="annualized">年化</option>
            <option value="max_drawdown">最大回撤</option>
          </select>
        </div>
        <div class="field"><label>Top-N</label><input type="number" id="opt-topn" value="5"/></div>
        <div class="field"><label>买入费率</label><input type="number" step="0.0001" id="opt-buy" value="0.0015"/></div>
        <div class="field"><label>初始现金</label><input type="number" id="opt-cash" value="0"/></div>
      </div>
      <div class="row" id="opt-grid" style="margin-top:14px"></div>
      <button class="run" id="run-optimize">运行寻优</button>
      <div class="hint">参数填逗号分隔的多个候选值，如 均线窗口 = 120,250,500。取笛卡尔积。</div>
    </div>
  </div>

  <!-- 诊断 -->
  <div class="panel" id="panel-diagnose">
    <div class="card">
      <div class="row">
        <div class="field combo"><label>基金代码</label><input id="diag-fund" value="161725"/></div>
        <div class="field"><label>窗口(交易日)</label><input type="number" id="diag-window" value="120"/></div>
        <div class="field"><label>波动带窗口</label><input type="number" id="diag-band" value="60"/></div>
        <div class="field"><label>买入基准(元)</label><input type="number" id="diag-base" value="1000"/></div>
        <div class="field"><label>卖出基准(%)</label><input type="number" id="diag-sell" value="20"/></div>
        <button class="run" id="run-diagnose">诊断</button>
      </div>
      <div id="diag-result" style="margin-top:14px"></div>
      <div class="hint" style="margin-top:10px">说明：基于历史净值的统计描述与启发式规则，不预测未来走势，不构成任何投资建议。</div>
    </div>
  </div>

  <iframe id="result" title="回测报告"></iframe>
</div>

<script>
// 结束日默认今日（本地日期），各 Tab 一致
(function(){
  var t = new Date();
  var iso = t.getFullYear() + '-' + String(t.getMonth()+1).padStart(2,'0') + '-' + String(t.getDate()).padStart(2,'0');
  ['#f-single [name="end"]', '#cmp-end', '#opt-end'].forEach(function(sel){
    var el = document.querySelector(sel);
    if(el) el.value = iso;
  });
})();
var FUNDS = [];
function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;'); }
fetch('/api/funds').then(function(r){return r.json();})
  .then(function(d){ FUNDS = Array.isArray(d) ? d : []; })
  .catch(function(){ FUNDS = []; });

// 把一个 input 升级为可搜索 combobox（FUNDS 为空时等同普通输入框）
function attachCombobox(input){
  if (input.dataset.combo) return;          // 防重复挂载
  input.dataset.combo = '1';
  input.setAttribute('autocomplete', 'off');
  var box = document.createElement('div');
  box.className = 'fund-dropdown';
  // 用相对定位容器包住 input
  var wrap = document.createElement('span');
  wrap.className = 'combo';
  input.parentNode.insertBefore(wrap, input);
  wrap.appendChild(input);
  wrap.appendChild(box);

  function hide(){ box.classList.remove('show'); box.innerHTML=''; }
  function render(q){
    if (!FUNDS.length || !q){ hide(); return; }
    var uq = q.toUpperCase();
    // 相关度打分：代码完全匹配(0)>代码前缀(1)>名称开头(2)>名称包含(3)>拼音包含(4)
    var scored = [];
    for (var i=0; i<FUNDS.length; i++){
      var f = FUNDS[i];
      var s = -1;
      if (f.code === q) s = 0;
      else if (f.code.indexOf(q)===0) s = 1;
      else if (f.name.indexOf(q)===0) s = 2;
      else if (f.name.indexOf(q)>=0) s = 3;
      else if (f.pinyin && f.pinyin.indexOf(uq)>=0) s = 4;
      if (s>=0) scored.push({f:f, s:s, i:i});
    }
    if (!scored.length){ hide(); return; }
    scored.sort(function(a,b){ return a.s-b.s || a.i-b.i; }); // 同分按原顺序
    var hits = scored.slice(0,20).map(function(x){ return x.f; });
    box.innerHTML = hits.map(function(f){
      return '<div class="fund-item" data-code="'+esc(f.code)+'"><span class="code">'+esc(f.code)+'</span>'+esc(f.name)+'</div>';
    }).join('');
    box.classList.add('show');
  }
  input.addEventListener('input', function(){ render(input.value.trim()); });
  input.addEventListener('focus', function(){ if(input.value.trim()) render(input.value.trim()); });
  input.addEventListener('blur', function(){ setTimeout(hide, 150); });
  box.addEventListener('mousedown', function(e){
    var item = e.target.closest('.fund-item');
    if (!item) return;
    e.preventDefault();
    input.value = item.getAttribute('data-code');
    hide();
    input.dispatchEvent(new Event('change'));
  });
}

var GRID_FIELDS = {
  dca: [["period","周期(月/周)","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]],
  smart_dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"],["ma_window","均线窗口(可多值)","120,250,500"],["k","k系数(可多值)","0.5,1.0,1.5"]],
  trend: [["short_window","短窗口(可多值)","10,20"],["long_window","长窗口(可多值)","60,120"],["amount","每次金额","1000"]],
  rsi: [["rsi_window","RSI周期(可多值)","14"],["oversold","超卖线(可多值)","25,30"],["overbought","超买线(可多值)","70,75"],["amount","每次金额","1000"]],
  adaptive: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]]
};
var ROW_FIELDS = {
  dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]],
  smart_dca: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"],["ma_window","均线窗口","250"],["k","k系数","1.0"]],
  trend: [["short_window","短窗口","20"],["long_window","长窗口","60"],["amount","每次金额","1000"]],
  rsi: [["rsi_window","RSI周期","14"],["oversold","超卖线","30"],["overbought","超买线","70"],["amount","每次金额","1000"]],
  adaptive: [["period","周期","monthly"],["day","定投日","1"],["base_amount","每期金额","1000"]]
};
var iframe = document.getElementById('result');

// Tab 切换
document.querySelectorAll('.tab').forEach(function(t){
  t.addEventListener('click', function(){
    document.querySelectorAll('.tab').forEach(function(x){x.classList.remove('active');});
    document.querySelectorAll('.panel').forEach(function(x){x.classList.remove('active');});
    t.classList.add('active');
    document.getElementById('panel-' + t.getAttribute('data-tab')).classList.add('active');
  });
});

// 单次：随策略显隐参数组
var singleStrat = document.querySelector('#f-single .strat');
function syncSingle(){
  var s = singleStrat.value;
  document.querySelectorAll('#f-single .params').forEach(function(g){
    var on = g.getAttribute('data-for').split(' ').indexOf(s) >= 0;
    g.style.display = on ? 'contents' : 'none';
    g.querySelectorAll('input,select').forEach(function(el){ el.disabled = !on; });
  });
}
singleStrat.addEventListener('change', syncSingle); syncSingle();
attachCombobox(document.querySelector('#f-single [name="fund_code"]'));
attachCombobox(document.getElementById('opt-fund'));
attachCombobox(document.getElementById('cmp-fund'));

function setBtn(btn, busy, label){ btn.disabled = busy; btn.textContent = busy ? '运行中…' : label; }
function showErr(e){ iframe.srcdoc = '<p style="color:#c0392b;padding:20px;font-family:sans-serif">请求失败: ' + e + '</p>'; }

// 单次运行 (GET)
document.getElementById('run-single').addEventListener('click', function(){
  var btn = this; var fd = new FormData(document.getElementById('f-single'));
  var qs = new URLSearchParams();
  for (var pair of fd.entries()) { if (pair[1] !== '') qs.append(pair[0], pair[1]); }
  setBtn(btn, true, '运行');
  fetch('/api/run?' + qs.toString()).then(function(r){return r.text();})
    .then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行'); });
});

// 对比：动态行
function strategySelect(cls){
  return '<select class="' + cls + '"><option value="dca">普通定投</option><option value="smart_dca" selected>智能定投</option><option value="trend">均线择时</option><option value="rsi">RSI超买超卖</option><option value="adaptive">自适应</option></select>';
}
function buildRowParams(div, strat){
  var holder = div.querySelector('.rowparams');
  holder.innerHTML = '';
  ROW_FIELDS[strat].forEach(function(f){
    holder.insertAdjacentHTML('beforeend',
      '<div class="field"><label>'+f[1]+'</label><input data-k="'+f[0]+'" value="'+f[2]+'"/></div>');
  });
}
function addCompareRow(){
  var div = document.createElement('div');
  div.className = 'crow';
  div.innerHTML = '<div class="row">'
    + '<div class="field"><label>名称</label><input class="rname" value="策略'+(document.querySelectorAll('.crow').length+1)+'"/></div>'
    + '<div class="field"><label>策略</label>'+strategySelect('rstrat')+'</div>'
    + '<div class="field"><label>基金(可空)</label><input class="rfund" placeholder="默认"/></div>'
    + '<span class="rowparams" style="display:contents"></span>'
    + '<button class="small del">删除</button></div>';
  document.getElementById('compare-rows').appendChild(div);
  var sel = div.querySelector('.rstrat');
  buildRowParams(div, sel.value);
  sel.addEventListener('change', function(){ buildRowParams(div, sel.value); });
  div.querySelector('.del').addEventListener('click', function(){ div.remove(); });
  attachCombobox(div.querySelector('.rfund'));
}
document.getElementById('add-row').addEventListener('click', addCompareRow);
addCompareRow(); addCompareRow();

document.getElementById('run-compare').addEventListener('click', function(){
  var btn = this;
  var runs = [];
  document.querySelectorAll('#compare-rows .crow').forEach(function(div){
    var run = { name: div.querySelector('.rname').value, strategy: div.querySelector('.rstrat').value };
    var fund = div.querySelector('.rfund').value.trim();
    if (fund) run.fund_code = fund;
    div.querySelectorAll('.rowparams input').forEach(function(inp){
      var k = inp.getAttribute('data-k'); var v = inp.value.trim();
      if (v === '') return;
      run[k] = (k === 'period') ? v : Number(v);
    });
    runs.push(run);
  });
  var payload = {
    fund_code: document.getElementById('cmp-fund').value,
    start: document.getElementById('cmp-start').value,
    end: document.getElementById('cmp-end').value,
    buy_rate: Number(document.getElementById('cmp-buy').value),
    initial_cash: Number(document.getElementById('cmp-cash').value),
    runs: runs
  };
  setBtn(btn, true, '运行对比');
  fetch('/api/compare', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(r){return r.text();}).then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行对比'); });
});

// 寻优：随策略生成 CSV 参数框
var optStrat = document.getElementById('opt-strat');
function buildOptGrid(){
  var holder = document.getElementById('opt-grid'); holder.innerHTML = '';
  GRID_FIELDS[optStrat.value].forEach(function(f){
    holder.insertAdjacentHTML('beforeend',
      '<div class="field"><label>'+f[1]+'</label><input data-k="'+f[0]+'" value="'+f[2]+'"/></div>');
  });
}
optStrat.addEventListener('change', buildOptGrid); buildOptGrid();

document.getElementById('run-optimize').addEventListener('click', function(){
  var btn = this; var grid = {};
  document.querySelectorAll('#opt-grid input').forEach(function(inp){
    var v = inp.value.trim(); if (v !== '') grid[inp.getAttribute('data-k')] = v;
  });
  var payload = {
    fund_code: document.getElementById('opt-fund').value,
    start: document.getElementById('opt-start').value,
    end: document.getElementById('opt-end').value,
    buy_rate: Number(document.getElementById('opt-buy').value),
    initial_cash: Number(document.getElementById('opt-cash').value),
    strategy: optStrat.value,
    metric: document.getElementById('opt-metric').value,
    top_n: Number(document.getElementById('opt-topn').value),
    grid: grid
  };
  setBtn(btn, true, '运行寻优');
  fetch('/api/optimize', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(r){return r.text();}).then(function(h){ iframe.srcdoc = h; }).catch(showErr)
    .finally(function(){ setBtn(btn, false, '运行寻优'); });
});

function renderSync(items){
  var box = document.getElementById('sync-result');
  if(!Array.isArray(items) || !items.length){ box.innerHTML = '<span style="color:#7f8c8d">无可同步的基金（缓存为空）</span>'; return; }
  box.innerHTML = items.map(function(o){
    if(o.error) return '<div style="color:#c0392b">'+esc(o.code)+' 同步失败: '+esc(o.error)+'</div>';
    return '<div style="color:#1a7f37">'+esc(o.code)+' +'+o.added+' 条新 · 最新 '+esc(o.latest||'-')+'（共 '+o.total+'）</div>';
  }).join('');
}
function doSync(body, btn){
  var box = document.getElementById('sync-result');
  var t = btn.textContent; btn.disabled = true; btn.textContent = '同步中…'; box.textContent = '同步中…';
  fetch('/api/sync', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(body)})
    .then(function(r){ return r.json(); }).then(renderSync)
    .catch(function(e){ box.innerHTML = '<span style="color:#c0392b">同步请求失败: '+esc(String(e))+'</span>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = t; });
}
document.getElementById('sync-all').addEventListener('click', function(){ doSync({}, this); });
document.getElementById('sync-one').addEventListener('click', function(){
  var c = document.getElementById('sync-code').value.trim();
  if(!c){ document.getElementById('sync-result').innerHTML = '<span style="color:#c0392b">请先填基金代码</span>'; return; }
  doSync({code:c}, this);
});
attachCombobox(document.getElementById('sync-code'));

function diagSignalColor(sig){
  if(sig && sig.indexOf('低吸')>=0) return '#27ae60';
  if(sig && sig.indexOf('高抛')>=0) return '#c0392b';
  return '#7f8c8d';
}
function renderPlan(p){
  if(!p || !p.current) return '';
  var c = p.current;
  var sc = diagSignalColor(c.signal);
  var rows = (p.tiers||[]).map(function(t){
    var hit = c.signal === t.label;
    var bg = hit ? 'background:#fff7e6;font-weight:700' : '';
    return '<tr style="'+bg+'"><td style="padding:4px 8px">'+esc(t.label)+'</td>'
      + '<td style="padding:4px 8px;text-align:right">'+t.nav.toFixed(4)+'</td>'
      + '<td style="padding:4px 8px;text-align:right">'+t.unit_nav.toFixed(4)+'</td>'
      + '<td style="padding:4px 8px">'+esc(t.action)+'</td></tr>';
  }).join('');
  return ''
    + '<div style="margin-top:14px;padding:12px;border:1px solid #eee;border-radius:8px">'
    + '<div style="font-size:1.1rem">当下：<strong style="color:'+sc+'">'+esc(c.signal)+'</strong>'
    + ' — '+esc(c.action)+'</div>'
    + '<div style="color:#5a6a7a;margin-top:4px">当前累计净值 <strong>'+c.nav.toFixed(4)+'</strong>'
    + ' · 单位净值 <strong>'+c.unit_nav.toFixed(4)+'</strong>（'+esc(c.date)+'）'
    + ' · z = '+c.z.toFixed(2)+'</div>'
    + '<div style="color:#5a6a7a;margin-top:2px">'+esc(c.next_hint)+'</div>'
    + '</div>'
    + '<div style="margin-top:10px;color:#34495e">波动带按<strong>累计净值</strong>口径（'+p.band_window+' 交易日）中轴 '+p.ma.toFixed(4)+' · σ '+p.sigma.toFixed(4)+'</div>'
    + '<table style="margin-top:6px;border-collapse:collapse;width:100%;font-size:.95rem">'
    + '<tr style="color:#7f8c8d;text-align:left"><th style="padding:4px 8px">信号</th>'
    + '<th style="padding:4px 8px;text-align:right">触发累计净值</th>'
    + '<th style="padding:4px 8px;text-align:right">≈单位净值</th><th style="padding:4px 8px">操作</th></tr>'
    + rows + '</table>'
    + '<div style="margin-top:8px;color:#5a6a7a">历史窗口内触发：低吸 '+p.buy_hits+' 次 · 高抛 '+p.sell_hits+' 次</div>'
    + '<div style="margin-top:8px;padding:8px 10px;background:#f3f7ff;border-radius:6px;color:#34495e">'+esc(p.caveat)+'</div>';
}
function renderDiag(r){
  var box = document.getElementById('diag-result');
  if(!r || !r.regime){ box.innerHTML = '<span style="color:#c0392b">诊断失败</span>'; return; }
  var color = r.regime === '上涨趋势' ? '#c0392b' : (r.regime === '下跌趋势' ? '#27ae60' : '#7f8c8d');
  box.innerHTML =
    '<div style="font-size:1.4rem;font-weight:700;color:'+color+'">'+esc(r.regime)+'</div>'
    + '<div style="margin-top:8px;color:#34495e">区间收益 '+(r.window_return*100).toFixed(2)+'%'
    + ' · 年化波动 '+(r.annualized_vol*100).toFixed(2)+'%'
    + ' · 均线 '+esc(r.ma_relation)+'（'+r.window+' 交易日）</div>'
    + '<div style="margin-top:10px;font-size:1.05rem">建议策略：<strong>'+esc(r.rec_name)+'</strong></div>'
    + '<div style="color:#5a6a7a;margin-top:4px">'+esc(r.rationale)+'</div>'
    + renderPlan(r.plan);
}
document.getElementById('run-diagnose').addEventListener('click', function(){
  var btn = this;
  var fund = document.getElementById('diag-fund').value.trim();
  var win = document.getElementById('diag-window').value.trim();
  var band = document.getElementById('diag-band').value.trim();
  var base = document.getElementById('diag-base').value.trim();
  var sell = document.getElementById('diag-sell').value.trim();
  if(!fund){ document.getElementById('diag-result').innerHTML = '<span style="color:#c0392b">请先填基金代码</span>'; return; }
  var qs = new URLSearchParams({fund_code: fund});
  if(win) qs.append('window', win);
  if(band) qs.append('band_window', band);
  if(base) qs.append('base_amount', base);
  if(sell) qs.append('sell_pct', (parseFloat(sell)/100).toString());
  var t = btn.textContent; btn.disabled = true; btn.textContent = '诊断中…';
  document.getElementById('diag-result').textContent = '诊断中…';
  fetch('/api/regime?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(t){ throw new Error(t); }); return res.json(); })
    .then(renderDiag)
    .catch(function(e){ document.getElementById('diag-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = t; });
});
attachCombobox(document.getElementById('diag-fund'));
</script>
</body>
</html>
"##;

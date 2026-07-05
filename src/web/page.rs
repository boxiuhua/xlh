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
.groups{display:flex;gap:8px;margin-bottom:14px}
.group{padding:8px 24px;cursor:pointer;border:1px solid #d0d7e2;background:#fff;border-radius:8px;font-size:1rem;color:#5a6a7a;font-weight:600}
.group.active{background:#c0392b;color:#fff;border-color:#c0392b}
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
<div id="xlh-bar" style="position:sticky;top:0;z-index:50;display:flex;gap:12px;align-items:center;padding:8px 14px;font:13px system-ui;background:#111827;color:#e5e7eb;border-bottom:1px solid #374151">
  <span id="xlh-user"></span>
  <span id="xlh-status"></span>
  <span style="flex:1"></span>
  <input id="xlh-code" placeholder="授权码" style="padding:4px 8px;border-radius:6px;border:1px solid #374151;background:#0b1220;color:#e5e7eb">
  <button onclick="xlhActivate()" style="padding:4px 10px;border:0;border-radius:6px;background:#3b82f6;color:#fff;cursor:pointer">激活/续期</button>
  <a id="xlh-admin" href="/admin" style="display:none;color:#93c5fd">管理后台</a>
  <button onclick="xlhLogout()" style="padding:4px 10px;border:0;border-radius:6px;background:#374151;color:#fff;cursor:pointer">退出</button>
</div>
<script>
async function xlhMe(){
  const r=await fetch('/api/auth/me'); if(!r.ok){location.href='/login';return;}
  const j=await r.json();
  document.getElementById('xlh-user').textContent='👤 '+j.username;
  const color={active:'#4ade80',warning:'#facc15',grace:'#f87171',inactive:'#f87171',expired:'#f87171'}[j.status]||'#e5e7eb';
  const text={active:'授权正常 · 到期 '+j.expires_at,warning:'⚠ '+j.remaining_days+' 天后到期，请续期',grace:'⚠ 已到期，宽限期内（尽快续期）',inactive:'未激活，请输入授权码',expired:'已过期，请续期'}[j.status]||j.status;
  const s=document.getElementById('xlh-status'); s.textContent=text; s.style.color=color;
  document.getElementById('xlh-admin').style.display=j.is_admin?'inline':'none';
}
async function xlhActivate(){
  const code=document.getElementById('xlh-code').value.trim(); if(!code)return;
  const r=await fetch('/api/auth/activate',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code})});
  const j=await r.json().catch(()=>({}));
  if(r.ok){alert('激活成功，到期日 '+j.expires_at);location.reload();}
  else{alert(({code_not_found:'授权码不存在',code_used:'授权码已被使用',code_revoked:'授权码已作废'})[j.error]||('激活失败: '+(j.error||r.status)));}
}
async function xlhLogout(){await fetch('/api/auth/logout',{method:'POST'});location.href='/login';}
// 核心请求 403 时提示激活
const _f=window.fetch; window.fetch=async(...a)=>{const r=await _f(...a);if(r.status===403){const c=r.clone();const j=await c.json().catch(()=>({}));if(j.error==='license_required'||j.error==='expired'){document.getElementById('xlh-code').focus();}}return r;};
xlhMe();
</script>
<div class="wrap">
  <h1>xlh 回测</h1>
  <div class="groups">
    <button class="group active" data-group="fund">基金</button>
    <button class="group" data-group="stock">股票</button>
  </div>
  <div class="card" id="sync-card">
    <div class="row" style="align-items:flex-end">
      <strong style="margin-right:8px">数据同步</strong>
      <button class="small" id="sync-all">同步全部已缓存</button>
      <div class="field combo"><label>基金代码</label><input id="sync-code" placeholder="如 161725"/></div>
      <button class="small" id="sync-one">同步此基金</button>
    </div>
    <div id="sync-result" class="hint" style="margin-top:8px"></div>
  </div>
  <div class="card" id="s-sync-card" style="display:none">
    <div class="row" style="align-items:flex-end">
      <strong style="margin-right:8px">数据同步</strong>
      <button class="small" id="s-sync-all">同步全部已缓存</button>
      <div class="field combo"><label>股票代码</label><input id="s-sync-code" placeholder="如 600519 / 00700 / AAPL"/></div>
      <button class="small" id="s-sync-one">同步此股票</button>
    </div>
    <div id="s-sync-result" class="hint" style="margin-top:8px"></div>
  </div>
  <div class="tabs" id="tabs-fund">
    <button class="tab active" data-tab="single">单次</button>
    <button class="tab" data-tab="compare">对比</button>
    <button class="tab" data-tab="optimize">寻优</button>
    <button class="tab" data-tab="diagnose">诊断</button>
    <button class="tab" data-tab="recommend">推荐</button>
    <button class="tab" data-tab="holdings">持仓建议</button>
    <button class="tab" data-tab="push">推送</button>
  </div>
  <div class="tabs" id="tabs-stock" style="display:none">
    <button class="tab" data-tab="s-diagnose">股诊断</button>
    <button class="tab" data-tab="s-backtest">股回测</button>
    <button class="tab" data-tab="s-screen">股选股</button>
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

  <!-- 推荐 -->
  <div class="panel" id="panel-recommend">
    <div class="card">
      <details open style="margin-bottom:12px">
        <summary style="cursor:pointer;font-weight:600;color:#1a252f">算法说明（点击展开/收起）</summary>
        <div style="margin-top:10px;color:#34495e;font-size:.9rem;line-height:1.7">
          <div>1) <strong>综合评分</strong>：score = 0.4·z(收益) + 0.4·z(夏普) − 0.2·z(最大回撤)，对池内各基金做标准化（z 分数），回撤为负向。</div>
          <div>2) <strong>样本外验证</strong>：历史按 70/30 切分；训练段（前 70%）从 5 个策略里按综合评分选最优，检验段（后 30%）实测该策略表现；<strong>Top5 排名用检验段指标</strong>，抑制过拟合。</div>
          <div>3) <strong>候选策略（固定参数）</strong>：普通定投 / 智能定投(MA250) / 均线择时(20·60) / RSI(14·30·70) / 自适应。</div>
          <div>4) <strong>当前择时</strong>：用近段净值的均线 ±σ 波动带——低吸线≈中轴−σ、高抛线≈中轴+σ，结合形态（上涨红 / 下跌绿 / 震荡灰）给出当下信号。</div>
          <div style="color:#c0392b;margin-top:6px">5) 免责声明：基于历史净值的统计回测与启发式规则，不预测未来走势，不构成任何投资建议。</div>
        </div>
      </details>
      <div class="row">
        <div class="field"><label>取 Top-N</label><input type="number" id="rec-topn" value="5"/></div>
        <button class="run" id="run-recommend">生成推荐</button>
      </div>
      <div class="hint" style="margin-top:8px">首次需联网抓取精选池全部基金净值（数十只，约几十秒）；命中缓存后秒级。</div>
      <div id="rec-result" style="margin-top:14px"></div>
    </div>
  </div>

  <!-- 持仓建议 -->
  <div class="panel" id="panel-holdings">
    <div class="card">
      <div class="row">
        <div class="field"><label>总持仓金额(元)</label><input type="number" id="hd-total" placeholder="选填"/></div>
        <div class="field"><label>持有收益(元)</label><input type="number" id="hd-profit" placeholder="选填"/></div>
        <div class="field"><label>累计收益(元)</label><input type="number" id="hd-cum" placeholder="选填"/></div>
      </div>
      <div id="hd-rows" style="margin-top:14px"></div>
      <button class="small" id="hd-add">+ 添加持仓</button>
      <button class="run" id="run-holdings">生成建议</button>
      <div class="hint" style="margin-top:8px">按你的实际持仓，逐只多策略样本外评估 + 当下择时，给出加仓/持有/减仓/止盈/观望及建议金额。首次联网抓取净值较慢，命中缓存后秒级。</div>
      <div id="hd-result" style="margin-top:14px"></div>
    </div>
  </div>

  <!-- 推送 -->
  <div class="panel" id="panel-push">
    <div class="card">
      <div class="row">
        <div class="field"><label>渠道</label>
          <select id="pu-kind">
            <option value="feishu">飞书</option>
            <option value="dingtalk">钉钉</option>
            <option value="wework">企业微信</option>
            <option value="serverchan">Server酱(微信)</option>
          </select>
        </div>
        <div class="field" style="flex:1;min-width:280px"><label>webhook / sendkey</label><input id="pu-webhook" placeholder="群机器人地址；Server酱填 sendkey"/></div>
        <div class="field"><label>加签密钥(可选)</label><input id="pu-secret" placeholder="钉钉/飞书 secret"/></div>
        <div class="field"><label>cron(6段含秒)</label><input id="pu-cron" value="0 30 8 * * *"/></div>
        <div class="field"><label>仅有新数据时推</label><select id="pu-onlynew"><option value="true">是</option><option value="false">否</option></select></div>
      </div>
      <div class="row" style="margin-top:10px">
        <div class="field"><label>总持仓金额</label><input type="number" id="pu-total" placeholder="选填"/></div>
        <div class="field"><label>持有收益</label><input type="number" id="pu-profit" placeholder="选填"/></div>
        <div class="field"><label>累计收益</label><input type="number" id="pu-cum" placeholder="选填"/></div>
      </div>

      <div style="margin-top:14px;font-weight:600;color:#1a252f">基金持仓</div>
      <div id="pu-fund-rows"></div>
      <button class="small" id="pu-add-fund">+ 基金</button>

      <div style="margin-top:14px;font-weight:600;color:#1a252f">股票持仓</div>
      <div id="pu-stock-rows"></div>
      <button class="small" id="pu-add-stock">+ 股票</button>

      <div class="row" style="margin-top:14px">
        <div class="field" style="flex:1;min-width:240px"><label>额外诊断·基金(逗号分隔)</label><input id="pu-diag-fund" placeholder="如 110022,161725"/></div>
        <div class="field" style="flex:1;min-width:240px"><label>额外诊断·股票(逗号分隔)</label><input id="pu-diag-stock" placeholder="如 600519,000001"/></div>
      </div>

      <div style="margin-top:14px">
        <button class="small" id="pu-load">读取当前配置</button>
        <button class="small" id="pu-save">保存</button>
        <button class="small" id="pu-preview">预览消息</button>
        <button class="run" id="pu-test">立即推送</button>
      </div>
      <div id="pu-msg" class="hint" style="margin-top:8px"></div>
      <pre id="pu-preview-box" style="margin-top:10px;white-space:pre-wrap;background:#fafbfc;border:1px solid #eaecef;border-radius:8px;padding:12px;display:none"></pre>
      <div class="hint" style="margin-top:8px">保存写入项目根 <code>push.toml</code>；后台 <code>xlh push</code> 守护进程需<strong>重启后生效</strong>（不热重载）。仅本机监听，secret 原样读写。</div>
    </div>
  </div>

  <!-- 股诊断 -->
  <div class="panel" id="panel-s-diagnose">
    <div class="card">
      <div class="row">
        <div class="field combo"><label>股票代码</label><input id="sd-code" placeholder="如 600519 / 00700 / AAPL"/></div>
        <button class="run" id="run-s-diagnose">诊断</button>
      </div>
      <div id="sd-result" style="margin-top:14px"></div>
      <div class="hint" style="margin-top:10px">基于后复权价的技术指标(MA/MACD/布林/RSI)启发式，不构成任何投资建议。</div>
    </div>
  </div>

  <!-- 股回测 -->
  <div class="panel" id="panel-s-backtest">
    <div class="card">
      <div class="row">
        <div class="field combo"><label>股票代码</label><input id="sb-code" placeholder="如 600519"/></div>
        <div class="field"><label>起始日</label><input type="date" id="sb-start" value="2020-01-01"/></div>
        <div class="field"><label>结束日</label><input type="date" id="sb-end" value="2024-12-31"/></div>
        <div class="field"><label>策略</label>
          <select id="sb-strat">
            <option value="dca">普通定投</option>
            <option value="smart_dca" selected>智能定投</option>
            <option value="trend">均线择时</option>
            <option value="rsi">RSI超买超卖</option>
            <option value="adaptive">自适应</option>
          </select>
        </div>
        <div class="field"><label>初始现金</label><input type="number" id="sb-cash" value="0"/></div>
      </div>
      <div class="row" id="sb-params" style="margin-top:12px"></div>
      <button class="run" id="run-s-backtest">回测</button>
      <div id="sb-result" style="margin-top:14px"></div>
      <div class="hint" style="margin-top:8px">费率按市场自动选择（A股/港股/美股）；撮合走后复权价。不构成任何投资建议。</div>
    </div>
  </div>

  <!-- 股选股 -->
  <div class="panel" id="panel-s-screen">
    <div class="card">
      <div class="row">
        <div class="field"><label>取 Top-N</label><input type="number" id="ss-topn" value="5"/></div>
        <button class="run" id="run-s-screen">选股</button>
      </div>
      <div class="hint" style="margin-top:8px">对预设股票池逐只多策略样本外评分 + 技术诊断后排名；首次联网抓取较慢，命中缓存后秒级。不构成任何投资建议。</div>
      <div id="ss-result" style="margin-top:14px"></div>
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

// 两级 Tab：先选大类（基金/股票），再选子功能
function activateTab(tab){
  document.querySelectorAll('.tab').forEach(function(x){x.classList.remove('active');});
  document.querySelectorAll('.panel').forEach(function(x){x.classList.remove('active');});
  var btn = document.querySelector('.tab[data-tab="'+tab+'"]');
  if(btn) btn.classList.add('active');
  var panel = document.getElementById('panel-' + tab);
  if(panel) panel.classList.add('active');
}
document.querySelectorAll('.tab').forEach(function(t){
  t.addEventListener('click', function(){ activateTab(t.getAttribute('data-tab')); });
});
// 大类切换：显隐对应子 Tab 栏 + 同步卡片，并激活该大类的首个子 Tab
var GROUP_DEFAULT = { fund: 'single', stock: 's-diagnose' };
function activateGroup(g){
  document.querySelectorAll('.group').forEach(function(x){ x.classList.toggle('active', x.getAttribute('data-group') === g); });
  var isStock = g === 'stock';
  document.getElementById('tabs-fund').style.display = isStock ? 'none' : '';
  document.getElementById('tabs-stock').style.display = isStock ? '' : 'none';
  document.getElementById('sync-card').style.display = isStock ? 'none' : '';
  document.getElementById('s-sync-card').style.display = isStock ? '' : 'none';
  activateTab(GROUP_DEFAULT[g]);
}
document.querySelectorAll('.group').forEach(function(gb){
  gb.addEventListener('click', function(){ activateGroup(gb.getAttribute('data-group')); });
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

function renderSyncInto(boxId, items, emptyMsg){
  var box = document.getElementById(boxId);
  if(!Array.isArray(items) || !items.length){ box.innerHTML = '<span style="color:#7f8c8d">'+esc(emptyMsg)+'</span>'; return; }
  box.innerHTML = items.map(function(o){
    if(o.error) return '<div style="color:#c0392b">'+esc(o.code)+' 同步失败: '+esc(o.error)+'</div>';
    return '<div style="color:#1a7f37">'+esc(o.code)+' +'+o.added+' 条新 · 最新 '+esc(o.latest||'-')+'（共 '+o.total+'）</div>';
  }).join('');
}
function doSyncTo(url, boxId, emptyMsg, body, btn){
  var box = document.getElementById(boxId);
  var t = btn.textContent; btn.disabled = true; btn.textContent = '同步中…'; box.textContent = '同步中…';
  fetch(url, {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(body)})
    .then(function(r){ return r.json(); }).then(function(items){ renderSyncInto(boxId, items, emptyMsg); })
    .catch(function(e){ box.innerHTML = '<span style="color:#c0392b">同步请求失败: '+esc(String(e))+'</span>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = t; });
}
document.getElementById('sync-all').addEventListener('click', function(){ doSyncTo('/api/sync', 'sync-result', '无可同步的基金（缓存为空）', {}, this); });
document.getElementById('sync-one').addEventListener('click', function(){
  var c = document.getElementById('sync-code').value.trim();
  if(!c){ document.getElementById('sync-result').innerHTML = '<span style="color:#c0392b">请先填基金代码</span>'; return; }
  doSyncTo('/api/sync', 'sync-result', '无可同步的基金（缓存为空）', {code:c}, this);
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
function regimeColor(reg){ return reg === '上涨趋势' ? '#c0392b' : (reg === '下跌趋势' ? '#27ae60' : '#7f8c8d'); }
function pct(x){ return (x*100).toFixed(1) + '%'; }
function recCard(r, rank){
  var reg = r.regime || {};
  var plan = reg.plan;
  var rc = regimeColor(reg.regime);
  var b = r.best_strategy || {};
  var timing = '';
  if (plan && plan.current){
    var c = plan.current;
    timing = '<div style="margin-top:8px;color:#34495e">当前择时：'
      + '<strong style="color:'+rc+'">'+esc(reg.regime)+'</strong>'
      + ' · 低吸线 '+plan.buy.toFixed(4)+' · 高抛线 '+plan.sell.toFixed(4)
      + ' · 当下 <strong>'+esc(c.signal)+'</strong>（'+esc(c.action)+'）</div>';
  } else {
    timing = '<div style="margin-top:8px;color:#7f8c8d">当前择时：'+esc(reg.regime||'数据不足')+'（暂无波动带）</div>';
  }
  return '<div class="card" style="border-left:4px solid '+rc+'">'
    + '<div style="display:flex;align-items:baseline;gap:10px">'
    + '<span style="font-size:1.3rem;font-weight:700;color:#c0392b">#'+rank+'</span>'
    + '<span style="font-size:1.1rem;font-weight:600">'+esc(r.name)+'</span>'
    + '<span style="color:#7f8c8d">'+esc(r.code)+'</span>'
    + '<span style="margin-left:auto;color:#5a6a7a">综合评分 '+r.fund_score.toFixed(2)+'</span></div>'
    + '<div style="margin-top:8px">推荐策略：<strong style="background:#fdecea;color:#c0392b;padding:2px 10px;border-radius:12px">'+esc(b.name)+'</strong></div>'
    + '<div style="margin-top:6px;color:#34495e">样本外：收益 '+pct(b.oos_return)+' · 夏普 '+b.oos_sharpe.toFixed(2)+' · 回撤 '+pct(b.oos_mdd)
    + '<span style="color:#95a5a6">（训练段 收益 '+pct(b.is_return)+' · 夏普 '+b.is_sharpe.toFixed(2)+'）</span></div>'
    + timing
    + '<div style="margin-top:8px;color:#5a6a7a">依据：'+esc(r.rationale)+'</div>'
    + '<div style="margin-top:4px;color:#5a6a7a">节奏：'+esc(r.cadence_hint)+'</div>'
    + '</div>';
}
function renderRec(rep){
  var box = document.getElementById('rec-result');
  if (!rep || !Array.isArray(rep.top)){ box.innerHTML = '<span style="color:#c0392b">推荐生成失败</span>'; return; }
  if (!rep.top.length){
    box.innerHTML = '<div style="color:#c0392b">暂无可分析数据（已分析 '+rep.analyzed+'/'+rep.pool_size+'）。请先在上方「数据同步」同步精选池，或检查网络。</div>';
    return;
  }
  var head = '<div style="color:#5a6a7a;margin-bottom:10px">已分析 '+rep.analyzed+'/'+rep.pool_size
    + ' · 跳过 '+(rep.skipped||[]).length+' 只 · 生成于 '+esc(rep.generated)+'</div>';
  var cards = rep.top.map(function(r, i){ return recCard(r, i+1); }).join('');
  var foot = '<div class="hint" style="margin-top:6px;color:#c0392b">'+esc(rep.disclaimer)+'</div>';
  box.innerHTML = head + cards + foot;
}
document.getElementById('run-recommend').addEventListener('click', function(){
  var btn = this;
  var topn = document.getElementById('rec-topn').value.trim();
  var qs = new URLSearchParams();
  if (topn) qs.append('top_n', topn);
  var t = btn.textContent; setBtn(btn, true, '生成推荐');
  document.getElementById('rec-result').textContent = '分析中…（首次需联网抓取，请稍候）';
  fetch('/api/recommend?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(t){ throw new Error(t); }); return res.json(); })
    .then(renderRec)
    .catch(function(e){ document.getElementById('rec-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '生成推荐'); });
});

// ===== 持仓建议 =====
function money(x){ return (x==null) ? '-' : Number(x).toLocaleString('zh-CN',{maximumFractionDigits:0}); }
function hdActionColor(a){ if(a==='加仓') return '#c0392b'; if(a==='减仓'||a==='止盈') return '#27ae60'; return '#7f8c8d'; }
function hdTimingLines(a){
  var p = a.regime && a.regime.plan;
  if(!p) return '';
  return ' · 低吸线 '+p.buy.toFixed(4)+' · 高抛线 '+p.sell.toFixed(4);
}
function hdCard(a){
  var ac = hdActionColor(a.action);
  var b = a.best_strategy || {};
  var amtTxt = a.suggest_amount>0 ? ' '+money(a.suggest_amount)+' 元' : '';
  return '<div class="card" style="border-left:4px solid '+ac+'">'
    + '<div style="display:flex;align-items:baseline;gap:10px;flex-wrap:wrap">'
    + '<span style="font-size:1.1rem;font-weight:700">'+esc(a.name)+'</span><span style="color:#7f8c8d">'+esc(a.code)+'</span>'
    + '<span style="margin-left:auto;font-size:1.05rem;font-weight:700;background:#fdecea;color:'+ac+';padding:2px 12px;border-radius:12px">'+esc(a.action)+amtTxt+'</span></div>'
    + '<div style="margin-top:8px;color:#34495e">持仓 '+money(a.amount)+' 元 · 收益 '+money(a.profit)+' 元 · 权重 '+(a.weight*100).toFixed(1)+'%</div>'
    + '<div style="margin-top:6px;color:#34495e">择时：<strong style="color:'+ac+'">'+esc(a.signal||'-')+'</strong> · 形态 '+esc((a.regime&&a.regime.regime)||'-')+hdTimingLines(a)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">最优策略：<strong>'+esc(b.name||'-')+'</strong> · 样本外 收益 '+pct(b.oos_return||0)+' · 夏普 '+(b.oos_sharpe||0).toFixed(2)+' · 回撤 '+pct(b.oos_mdd||0)+'</div>'
    + '<div style="margin-top:6px;color:#5a6a7a">'+esc(a.rationale)+'</div></div>';
}
function renderHoldings(rep){
  var box = document.getElementById('hd-result');
  if(!rep || !rep.summary){ box.innerHTML = '<span style="color:#c0392b">生成失败</span>'; return; }
  var s = rep.summary, parts = [];
  var sumLine = '总持仓 '+money(s.total_amount)+' 元 · 持仓 '+s.holding_count+' 只';
  if(s.total_profit!=null) sumLine += ' · 持有收益 '+money(s.total_profit)+' 元';
  if(s.cumulative_profit!=null) sumLine += ' · 累计收益 '+money(s.cumulative_profit)+' 元';
  parts.push('<div class="card" style="margin-top:0"><div style="font-size:1.1rem;font-weight:600">组合汇总</div>'
    + '<div style="margin-top:8px;color:#34495e">'+esc(sumLine)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">合计建议：加仓 <strong style="color:#c0392b">'+money(s.total_add)+'</strong> 元 · 减仓/止盈 <strong style="color:#27ae60">'+money(s.total_trim)+'</strong> 元</div>'
    + (s.concentration_note ? '<div style="margin-top:6px;color:#c0392b">'+esc(s.concentration_note)+'</div>' : '')
    + '</div>');
  if(!rep.advices.length){
    var sk = (rep.skipped&&rep.skipped.length) ? '（跳过 '+rep.skipped.length+' 只：'+esc(rep.skipped.join(', '))+'）' : '';
    parts.push('<div style="color:#c0392b">无可分析持仓'+sk+'。请检查基金代码，或先在「数据同步」同步净值。</div>');
  } else {
    parts.push(rep.advices.map(hdCard).join(''));
    if(rep.skipped && rep.skipped.length)
      parts.push('<div class="hint" style="margin-top:6px">跳过 '+rep.skipped.length+' 只（加载失败/数据不足）：'+esc(rep.skipped.join(', '))+'</div>');
  }
  parts.push('<div class="hint" style="margin-top:6px;color:#c0392b">'+esc(rep.disclaimer)+'</div>');
  box.innerHTML = parts.join('');
}
function addHoldingRow(){
  var div = document.createElement('div');
  div.className = 'crow';
  div.innerHTML = '<div class="row">'
    + '<div class="field"><label>基金代码</label><input class="hd-code" placeholder="如 161725"/></div>'
    + '<div class="field"><label>持有金额(元)</label><input type="number" class="hd-amt" placeholder="选填"/></div>'
    + '<div class="field"><label>持有收益(元)</label><input type="number" class="hd-pft" placeholder="选填"/></div>'
    + '<button class="small del">删除</button></div>';
  document.getElementById('hd-rows').appendChild(div);
  div.querySelector('.del').addEventListener('click', function(){ div.remove(); });
  attachCombobox(div.querySelector('.hd-code'));
}
document.getElementById('hd-add').addEventListener('click', addHoldingRow);
addHoldingRow(); addHoldingRow();

document.getElementById('run-holdings').addEventListener('click', function(){
  var btn = this, holdings = [];
  document.querySelectorAll('#hd-rows .crow').forEach(function(div){
    var code = div.querySelector('.hd-code').value.trim();
    if(!code) return;
    var amt = div.querySelector('.hd-amt').value.trim();
    var pft = div.querySelector('.hd-pft').value.trim();
    holdings.push({ code: code, amount: amt===''?0:Number(amt), profit: pft===''?0:Number(pft) });
  });
  if(!holdings.length){ document.getElementById('hd-result').innerHTML = '<span style="color:#c0392b">请先添加至少一只持仓基金</span>'; return; }
  var payload = { holdings: holdings };
  var t = document.getElementById('hd-total').value.trim(); if(t!=='') payload.total_amount = Number(t);
  var pf = document.getElementById('hd-profit').value.trim(); if(pf!=='') payload.total_profit = Number(pf);
  var cu = document.getElementById('hd-cum').value.trim(); if(cu!=='') payload.cumulative_profit = Number(cu);
  setBtn(btn, true, '生成建议');
  document.getElementById('hd-result').textContent = '分析中…（首次联网抓取，请稍候）';
  fetch('/api/holdings', {method:'POST', headers:{'Content-Type':'application/json'}, body: JSON.stringify(payload)})
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(renderHoldings)
    .catch(function(e){ document.getElementById('hd-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '生成建议'); });
});

// ===== 股票 Tab =====
// 股票代码服务端搜索补全（无全量清单，按 q 查 /api/stock/search）
function attachStockCombobox(input){
  if (input.dataset.combo) return;
  input.dataset.combo = '1';
  input.setAttribute('autocomplete','off');
  var box = document.createElement('div'); box.className = 'fund-dropdown';
  var wrap = document.createElement('span'); wrap.className = 'combo';
  input.parentNode.insertBefore(wrap, input); wrap.appendChild(input); wrap.appendChild(box);
  var timer = null;
  function hide(){ box.classList.remove('show'); box.innerHTML=''; }
  function query(q){
    if(!q){ hide(); return; }
    fetch('/api/stock/search?q=' + encodeURIComponent(q))
      .then(function(r){ return r.json(); })
      .then(function(list){
        if(!Array.isArray(list) || !list.length){ hide(); return; }
        box.innerHTML = list.slice(0,20).map(function(s){
          return '<div class="fund-item" data-code="'+esc(s.code)+'"><span class="code">'+esc(s.code)+'</span>'+esc(s.name)+' <span style="color:#95a5a6">'+esc(s.market_name||'')+'</span></div>';
        }).join('');
        box.classList.add('show');
      }).catch(function(){ hide(); });
  }
  input.addEventListener('input', function(){ var v=input.value.trim(); clearTimeout(timer); timer=setTimeout(function(){ query(v); }, 200); });
  input.addEventListener('blur', function(){ setTimeout(hide, 150); });
  box.addEventListener('mousedown', function(e){
    var it = e.target.closest('.fund-item'); if(!it) return;
    e.preventDefault(); input.value = it.getAttribute('data-code'); hide(); input.dispatchEvent(new Event('change'));
  });
}

function sSignalColor(sig){ if(sig&&sig.indexOf('买入')>=0) return '#c0392b'; if(sig&&sig.indexOf('卖出')>=0) return '#27ae60'; return '#7f8c8d'; }
function trendColor(tr){ return tr==='上涨' ? '#c0392b' : (tr==='下跌' ? '#27ae60' : '#7f8c8d'); }
function pf(x){ return (x==null || !isFinite(x)) ? '∞' : Number(x).toFixed(2); }

function renderStockDiag(d){
  var box = document.getElementById('sd-result');
  if(!d || !d.signal){ box.innerHTML = '<span style="color:#c0392b">诊断失败</span>'; return; }
  var tc = trendColor(d.trend), sc = sSignalColor(d.signal);
  box.innerHTML =
    '<div style="display:flex;gap:12px;align-items:baseline;flex-wrap:wrap"><span style="font-size:1.3rem;font-weight:700;color:'+tc+'">'+esc(d.trend)+'</span>'
    + '<span style="font-size:1.2rem;font-weight:700;color:'+sc+'">'+esc(d.signal)+'</span>'
    + '<span style="color:#7f8c8d">'+esc(d.code)+' · '+esc(d.date)+'</span></div>'
    + '<div style="margin-top:8px;color:#34495e">价 '+d.price.toFixed(3)+'（后复权 '+d.adj_price.toFixed(3)+'）· '+esc(d.ma_relation)+' · MA短 '+d.ma_short.toFixed(2)+' / 长 '+d.ma_long.toFixed(2)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">布林 z '+d.boll_z.toFixed(2)+'（下 '+d.boll_lower.toFixed(2)+' / 中 '+d.boll_mid.toFixed(2)+' / 上 '+d.boll_upper.toFixed(2)+'）· RSI '+d.rsi.toFixed(1)+' · MACD柱 '+d.macd_hist.toFixed(3)+'</div>'
    + '<div style="margin-top:8px;color:#5a6a7a">'+esc(d.rationale)+'</div>'
    + '<div style="margin-top:8px;padding:8px 10px;background:#f3f7ff;border-radius:6px;color:#34495e">'+esc(d.caveat)+'</div>';
}
function renderStockRun(o){
  var box = document.getElementById('sb-result');
  if(!o || !o.summary){ box.innerHTML = '<span style="color:#c0392b">回测失败</span>'; return; }
  var s = o.summary, ts = o.trade_stats || {};
  box.innerHTML = '<div class="card" style="margin-top:0">'
    + '<div style="font-size:1.1rem;font-weight:600">'+esc(o.code)+' · '+esc(o.name)+'</div>'
    + '<div style="margin-top:8px;color:#34495e">总收益 '+pct(s.total_return)+' · 年化 '+pct(s.annualized)+' · 夏普 '+s.sharpe.toFixed(2)+' · 最大回撤 '+pct(s.max_drawdown)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">投入 '+s.total_contributed.toFixed(0)+' · 期末 '+s.final_equity.toFixed(0)+' · 成交 '+s.trade_count+' 笔</div>'
    + '<div style="margin-top:6px;color:#5a6a7a">交易统计：卖出 '+(ts.round_trips||0)+' 次 · 胜率 '+pct(ts.win_rate||0)+' · 盈亏比 '+pf(ts.profit_factor)+' · 实现盈亏 '+(ts.realized_pnl||0).toFixed(0)+'</div>'
    + '</div>';
}
function sCard(r, rank){
  var d = r.diagnosis || {}, b = r.best_strategy || {}, tc = trendColor(d.trend);
  return '<div class="card" style="border-left:4px solid '+tc+'">'
    + '<div style="display:flex;align-items:baseline;gap:10px;flex-wrap:wrap"><span style="font-size:1.3rem;font-weight:700;color:#c0392b">#'+rank+'</span>'
    + '<span style="font-size:1.1rem;font-weight:600">'+esc(r.name)+'</span><span style="color:#7f8c8d">'+esc(r.code)+'</span>'
    + '<span style="margin-left:auto;color:#5a6a7a">综合评分 '+r.stock_score.toFixed(2)+'</span></div>'
    + '<div style="margin-top:8px">最优策略：<strong style="background:#fdecea;color:#c0392b;padding:2px 10px;border-radius:12px">'+esc(b.name)+'</strong></div>'
    + '<div style="margin-top:6px;color:#34495e">样本外：收益 '+pct(b.oos_return)+' · 夏普 '+b.oos_sharpe.toFixed(2)+' · 回撤 '+pct(b.oos_mdd)+'</div>'
    + '<div style="margin-top:6px;color:#34495e">技术面：<strong style="color:'+tc+'">'+esc(d.trend||'-')+'</strong> · '+esc(d.signal||'-')+'</div>'
    + '<div style="margin-top:6px;color:#5a6a7a">'+esc(r.rationale)+'</div></div>';
}
function renderStockScreen(rep){
  var box = document.getElementById('ss-result');
  if(!rep || !Array.isArray(rep.top)){ box.innerHTML = '<span style="color:#c0392b">选股失败</span>'; return; }
  if(!rep.top.length){ box.innerHTML = '<div style="color:#c0392b">暂无可分析数据（已分析 '+rep.analyzed+'/'+rep.pool_size+'）。</div>'; return; }
  var head = '<div style="color:#5a6a7a;margin-bottom:10px">已分析 '+rep.analyzed+'/'+rep.pool_size+' · 跳过 '+(rep.skipped||[]).length+' · 生成于 '+esc(rep.generated)+'</div>';
  var cards = rep.top.map(function(r,i){ return sCard(r,i+1); }).join('');
  var foot = '<div class="hint" style="margin-top:6px;color:#c0392b">'+esc(rep.disclaimer)+'</div>';
  box.innerHTML = head + cards + foot;
}

// 股回测参数组（复用 ROW_FIELDS）
var sbStrat = document.getElementById('sb-strat');
function buildSbParams(){
  var holder = document.getElementById('sb-params'); holder.innerHTML='';
  ROW_FIELDS[sbStrat.value].forEach(function(f){
    holder.insertAdjacentHTML('beforeend', '<div class="field"><label>'+f[1]+'</label><input data-k="'+f[0]+'" value="'+f[2]+'"/></div>');
  });
}
sbStrat.addEventListener('change', buildSbParams); buildSbParams();
attachStockCombobox(document.getElementById('sd-code'));
attachStockCombobox(document.getElementById('sb-code'));
attachStockCombobox(document.getElementById('s-sync-code'));

// 股票数据同步（复用 /api/stock/sync，响应结构同基金 SyncOutcome）
document.getElementById('s-sync-all').addEventListener('click', function(){
  doSyncTo('/api/stock/sync', 's-sync-result', '无可同步的股票（缓存为空）', {}, this);
});
document.getElementById('s-sync-one').addEventListener('click', function(){
  var c = document.getElementById('s-sync-code').value.trim();
  if(!c){ document.getElementById('s-sync-result').innerHTML = '<span style="color:#c0392b">请先填股票代码</span>'; return; }
  doSyncTo('/api/stock/sync', 's-sync-result', '无可同步的股票（缓存为空）', {code:c}, this);
});

document.getElementById('run-s-diagnose').addEventListener('click', function(){
  var btn = this, code = document.getElementById('sd-code').value.trim();
  if(!code){ document.getElementById('sd-result').innerHTML = '<span style="color:#c0392b">请先填股票代码</span>'; return; }
  setBtn(btn, true, '诊断'); document.getElementById('sd-result').textContent = '诊断中…';
  fetch('/api/stock/diagnose?code=' + encodeURIComponent(code))
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(renderStockDiag)
    .catch(function(e){ document.getElementById('sd-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '诊断'); });
});
document.getElementById('run-s-backtest').addEventListener('click', function(){
  var btn = this, code = document.getElementById('sb-code').value.trim();
  if(!code){ document.getElementById('sb-result').innerHTML = '<span style="color:#c0392b">请先填股票代码</span>'; return; }
  var qs = new URLSearchParams({ code: code, start: document.getElementById('sb-start').value, end: document.getElementById('sb-end').value, strategy: sbStrat.value, initial_cash: document.getElementById('sb-cash').value || '0' });
  document.querySelectorAll('#sb-params input').forEach(function(inp){ var v = inp.value.trim(); if(v!=='') qs.append(inp.getAttribute('data-k'), v); });
  setBtn(btn, true, '回测'); document.getElementById('sb-result').textContent = '回测中…';
  fetch('/api/stock/run?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(renderStockRun)
    .catch(function(e){ document.getElementById('sb-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '回测'); });
});
document.getElementById('run-s-screen').addEventListener('click', function(){
  var btn = this, topn = document.getElementById('ss-topn').value.trim();
  var qs = new URLSearchParams(); if(topn) qs.append('top_n', topn);
  setBtn(btn, true, '选股'); document.getElementById('ss-result').textContent = '分析中…（首次联网抓取，请稍候）';
  fetch('/api/stock/recommend?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(renderStockScreen)
    .catch(function(e){ document.getElementById('ss-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ setBtn(btn, false, '选股'); });
});

// ===== 推送配置 =====
function puNum(id){ var v=document.getElementById(id).value.trim(); return v===''?null:Number(v); }
function puCsv(id){ return document.getElementById(id).value.split(',').map(function(s){return s.trim();}).filter(function(s){return s;}); }
function puRowVal(d, sel){ var v=d.querySelector(sel).value.trim(); return v===''?0:Number(v); }

function puRow(container, comboFn, code, amount, profit){
  var d=document.createElement('div'); d.className='crow';
  d.innerHTML='<div class="row">'
    +'<div class="field"><label>代码</label><input class="pu-code" placeholder="如 161725 / 600519"/></div>'
    +'<div class="field"><label>持有金额</label><input type="number" class="pu-amt" placeholder="选填"/></div>'
    +'<div class="field"><label>持有收益</label><input type="number" class="pu-pft" placeholder="选填"/></div>'
    +'<button class="small del">删除</button></div>';
  container.appendChild(d);
  d.querySelector('.pu-code').value = code||'';
  if(amount) d.querySelector('.pu-amt').value = amount;
  if(profit) d.querySelector('.pu-pft').value = profit;
  d.querySelector('.del').addEventListener('click', function(){ d.remove(); });
  comboFn(d.querySelector('.pu-code'));
}
function puFundRow(code,amount,profit){ puRow(document.getElementById('pu-fund-rows'), attachCombobox, code,amount,profit); }
function puStockRow(code,amount,profit){ puRow(document.getElementById('pu-stock-rows'), attachStockCombobox, code,amount,profit); }

function collectPushConfig(){
  var holdings=[], stocks=[];
  document.querySelectorAll('#pu-fund-rows .crow').forEach(function(d){
    var c=d.querySelector('.pu-code').value.trim(); if(!c) return;
    holdings.push({code:c, amount:puRowVal(d,'.pu-amt'), profit:puRowVal(d,'.pu-pft')});
  });
  document.querySelectorAll('#pu-stock-rows .crow').forEach(function(d){
    var c=d.querySelector('.pu-code').value.trim(); if(!c) return;
    stocks.push({code:c, amount:puRowVal(d,'.pu-amt'), profit:puRowVal(d,'.pu-pft')});
  });
  var portfolio={};
  var t=puNum('pu-total'); if(t!=null) portfolio.total_amount=t;
  var pf2=puNum('pu-profit'); if(pf2!=null) portfolio.total_profit=pf2;
  var cu=puNum('pu-cum'); if(cu!=null) portfolio.cumulative_profit=cu;
  return {
    schedule:{ cron:document.getElementById('pu-cron').value.trim(), only_on_new_data: document.getElementById('pu-onlynew').value==='true' },
    channel:{ kind:document.getElementById('pu-kind').value, webhook:document.getElementById('pu-webhook').value.trim(), secret:document.getElementById('pu-secret').value.trim(), cache_dir:'.cache' },
    portfolio: portfolio,
    holdings: holdings,
    diagnose: puCsv('pu-diag-fund'),
    stocks: stocks,
    diagnose_stocks: puCsv('pu-diag-stock')
  };
}
function loadPushConfig(){
  fetch('/api/push/config').then(function(r){return r.json();}).then(function(c){
    document.getElementById('pu-kind').value = (c.channel&&c.channel.kind)||'feishu';
    document.getElementById('pu-webhook').value = (c.channel&&c.channel.webhook)||'';
    document.getElementById('pu-secret').value = (c.channel&&c.channel.secret)||'';
    document.getElementById('pu-cron').value = (c.schedule&&c.schedule.cron)||'0 30 8 * * *';
    document.getElementById('pu-onlynew').value = (c.schedule&&c.schedule.only_on_new_data===false)?'false':'true';
    var p=c.portfolio||{};
    document.getElementById('pu-total').value = p.total_amount!=null?p.total_amount:'';
    document.getElementById('pu-profit').value = p.total_profit!=null?p.total_profit:'';
    document.getElementById('pu-cum').value = p.cumulative_profit!=null?p.cumulative_profit:'';
    document.getElementById('pu-fund-rows').innerHTML='';
    (c.holdings||[]).forEach(function(h){ puFundRow(h.code,h.amount,h.profit); });
    if(!(c.holdings||[]).length) puFundRow();
    document.getElementById('pu-stock-rows').innerHTML='';
    (c.stocks||[]).forEach(function(h){ puStockRow(h.code,h.amount,h.profit); });
    if(!(c.stocks||[]).length) puStockRow();
    document.getElementById('pu-diag-fund').value = (c.diagnose||[]).join(',');
    document.getElementById('pu-diag-stock').value = (c.diagnose_stocks||[]).join(',');
  }).catch(function(){ puFundRow(); puStockRow(); });
}
function puPost(url, okMsg, btn, onOk){
  var msg=document.getElementById('pu-msg'); var t=btn.textContent; btn.disabled=true; msg.textContent='处理中…';
  fetch(url,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(collectPushConfig())})
    .then(function(res){ if(!res.ok) return res.text().then(function(x){ throw new Error(x); }); return res.json(); })
    .then(function(d){ if(onOk) onOk(d); else msg.innerHTML='<span style="color:#1a7f37">'+esc(okMsg)+'</span>'; })
    .catch(function(e){ msg.innerHTML='<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ btn.disabled=false; btn.textContent=t; });
}
document.getElementById('pu-add-fund').addEventListener('click', function(){ puFundRow(); });
document.getElementById('pu-add-stock').addEventListener('click', function(){ puStockRow(); });
document.getElementById('pu-load').addEventListener('click', loadPushConfig);
document.getElementById('pu-save').addEventListener('click', function(){ puPost('/api/push/config', '已保存到 push.toml', this); });
document.getElementById('pu-preview').addEventListener('click', function(){
  var box=document.getElementById('pu-preview-box'), msg=document.getElementById('pu-msg');
  msg.textContent='组装预览中…（首次联网抓取，请稍候）';
  puPost('/api/push/preview', '', this, function(d){
    box.style.display='block'; box.textContent=d.markdown||'(空)';
    msg.innerHTML = d.has_new ? '<span style="color:#1a7f37">检测到新数据</span>' : '<span style="color:#7f8c8d">无新数据（定时任务默认会跳过本次）</span>';
  });
});
document.getElementById('pu-test').addEventListener('click', function(){
  var msg=document.getElementById('pu-msg');
  msg.textContent='推送中…';
  puPost('/api/push/test', '', this, function(d){
    msg.innerHTML = d.ok ? '<span style="color:#1a7f37">推送成功</span>' : '<span style="color:#c0392b">推送失败：'+esc(d.error||'')+'</span>';
  });
});
loadPushConfig();
</script>
</body>
</html>
"##;

pub async fn login_html_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(LOGIN_HTML)
}

pub const LOGIN_HTML: &str = r##"<!doctype html>
<html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>xlh · 登录</title>
<style>
 body{font-family:system-ui,sans-serif;background:#0f172a;color:#e2e8f0;display:flex;min-height:100vh;align-items:center;justify-content:center}
 .card{background:#1e293b;padding:32px;border-radius:12px;width:320px;box-shadow:0 8px 30px rgba(0,0,0,.4)}
 h1{font-size:20px;margin:0 0 16px} input{width:100%;box-sizing:border-box;margin:6px 0;padding:10px;border-radius:8px;border:1px solid #334155;background:#0f172a;color:#e2e8f0}
 button{width:100%;padding:10px;margin-top:10px;border:0;border-radius:8px;background:#3b82f6;color:#fff;font-weight:600;cursor:pointer}
 .tab{display:flex;gap:8px;margin-bottom:12px} .tab button{background:#334155} .tab button.on{background:#3b82f6}
 .msg{min-height:18px;font-size:13px;color:#f87171;margin-top:8px}
</style></head><body>
<div class="card">
  <div class="tab"><button id="tlogin" class="on" onclick="mode('login')">登录</button><button id="treg" onclick="mode('register')">注册</button></div>
  <h1 id="title">登录</h1>
  <input id="u" placeholder="用户名（3-32 位字母数字）" autocomplete="username">
  <input id="p" type="password" placeholder="密码（≥6 位）" autocomplete="current-password">
  <button onclick="submit()">提交</button>
  <div class="msg" id="msg"></div>
</div>
<script>
let M='login';
function mode(m){M=m;document.getElementById('tlogin').className=m=='login'?'on':'';document.getElementById('treg').className=m=='register'?'on':'';document.getElementById('title').textContent=m=='login'?'登录':'注册';document.getElementById('msg').textContent='';}
async function submit(){
  const u=document.getElementById('u').value.trim(),p=document.getElementById('p').value;
  const url=M=='login'?'/api/auth/login':'/api/auth/register';
  const r=await fetch(url,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({username:u,password:p})});
  const j=await r.json().catch(()=>({}));
  if(r.ok){ if(M=='register'){mode('login');document.getElementById('msg').style.color='#4ade80';document.getElementById('msg').textContent='注册成功，请登录';} else {location.href='/';} }
  else { document.getElementById('msg').style.color='#f87171';document.getElementById('msg').textContent=({invalid_login:'用户名或密码错误',username_taken:'用户名已被占用',invalid_credentials:'用户名或密码格式不符',registration_closed:'当前未开放注册'})[j.error]||('失败: '+(j.error||r.status)); }
}
</script></body></html>"##;

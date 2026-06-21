# 行情形态诊断 + 策略建议 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 对选定基金识别行情形态（上涨/下跌/震荡）并推荐适配策略类型，Web 新增「诊断」Tab；明确启发式 + 免责声明，不预测涨跌。

**Architecture:** 新增 `analyze::detect_regime`（纯函数，基于 acc_nav 的区间收益 + MA20/60 + 年化波动判三态并映射建议）；`GET /api/regime` 路由（spawn_blocking 内 load_or_fetch + detect）；page.rs 加第 4 Tab。引擎/策略/回测不动。

**Tech Stack:** Rust（serde/anyhow/chrono/axum）+ 原生 JS + Playwright。

## Global Constraints

- 纯新增：`analyze` 模块、只读路由 `GET /api/regime`、前端第 4 Tab；不改引擎/策略/回测/既有 Tab。
- 不破坏既有 104 测试与 clippy 干净。
- 形态基于 **acc_nav**（累计净值）：区间收益 + MA20 vs MA60 + 年化波动率(×√252)。
- 三态：`收益>+0.10 且 MA20>MA60`→上涨；`收益<-0.10 且 MA20<MA60`→下跌；其余→震荡。默认 window=120、ma_short=20、ma_long=60、up=0.10、down=-0.10。
- 建议映射：上涨→smart_dca(智能定投)、下跌→trend(均线择时)、震荡→rsi(RSI超买超卖)。
- 数据不足（`len < max(window, ma_long+1)`）→ 报错「数据不足」。
- fund_code 校验复用 web `validate_fund_code`（非空/≤12/alnum）；handler 网络 IO 在 spawn_blocking 内。
- 颜色：上涨红(#c0392b)、下跌绿(#27ae60)、震荡灰(#7f8c8d)（红涨绿跌，与报告一致）。
- 免责声明常驻显眼：「基于历史净值的统计描述与启发式规则，不预测未来走势，不构成任何投资建议」。
- edition 2021；提交含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `data::NavPoint { date: NaiveDate, nav: f64, acc_nav: f64 }`（`Copy`）。
- `data::cache::load_or_fetch(code:&str, &Path, start:NaiveDate, end:NaiveDate) -> Result<Vec<NavPoint>>`。
- chrono：`chrono::Local::now().date_naive()`、`chrono::Duration::days`（chrono 默认 features 含 clock，可用）。
- web/mod.rs：`validate_fund_code(&str)->Result<()>`、`AppError`、`router()`（`use axum::routing::{get,post}`、`axum::extract::Query`、`axum::Json`、`axum::response::Html` 已在用）；page.rs `esc()`/`attachCombobox()`、`.tabs`/`.tab`/`.panel`/`.card`/`.field`/`.combo`/`.hint` 样式、`data-tab`/`panel-<id>` 模式（现有 single/compare/optimize）。
- `src/lib.rs` 有 `pub mod ...` 模块声明区。

---

## Task 1: analyze::detect_regime

**Files:**
- Create: `src/analyze.rs`
- Modify: `src/lib.rs`（加 `pub mod analyze;`）
- Test: `src/analyze.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces: `Regime` 枚举；`RegimeReport`（Serialize）；`RegimeParams`(+Default)；`pub fn detect_regime(points: &[NavPoint], p: &RegimeParams) -> anyhow::Result<RegimeReport>`。

- [ ] **Step 1: 注册模块**

`src/lib.rs` 模块声明区加：

```rust
pub mod analyze;
```

- [ ] **Step 2: 写测试 + 实现（同文件给出，TDD 顺序：先放测试见编译失败，再补实现）**

创建 `src/analyze.rs`：

```rust
use crate::data::NavPoint;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Regime { Uptrend, Downtrend, Range }

#[derive(Debug, serde::Serialize)]
pub struct RegimeReport {
    pub regime: String,
    pub window: usize,
    pub window_return: f64,
    pub annualized_vol: f64,
    pub ma_short: f64,
    pub ma_long: f64,
    pub ma_relation: String,
    pub rec_strategy: String,
    pub rec_name: String,
    pub rationale: String,
}

pub struct RegimeParams {
    pub window: usize,
    pub up_threshold: f64,
    pub down_threshold: f64,
    pub ma_short: usize,
    pub ma_long: usize,
}

impl Default for RegimeParams {
    fn default() -> Self {
        Self { window: 120, up_threshold: 0.10, down_threshold: -0.10, ma_short: 20, ma_long: 60 }
    }
}

fn mean_tail(points: &[NavPoint], n: usize) -> f64 {
    let s = &points[points.len() - n..];
    s.iter().map(|p| p.acc_nav).sum::<f64>() / n as f64
}

fn stdev(xs: &[f64]) -> f64 {
    if xs.len() < 2 { return 0.0; }
    let n = xs.len() as f64;
    let mean = xs.iter().sum::<f64>() / n;
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// 基于 acc_nav 判定近 window 个交易日的行情形态并给策略建议。
pub fn detect_regime(points: &[NavPoint], p: &RegimeParams) -> anyhow::Result<RegimeReport> {
    let need = p.window.max(p.ma_long + 1);
    if points.len() < need {
        return Err(anyhow::anyhow!("数据不足: 需要至少 {} 个净值点，当前 {}", need, points.len()));
    }
    let w = &points[points.len() - p.window..];
    let window_return = w.last().unwrap().acc_nav / w.first().unwrap().acc_nav - 1.0;
    let ma_short = mean_tail(points, p.ma_short);
    let ma_long = mean_tail(points, p.ma_long);
    let eps = 0.005;
    let ma_relation = if ma_short > ma_long * (1.0 + eps) { "多头排列" }
        else if ma_short < ma_long * (1.0 - eps) { "空头排列" }
        else { "纠缠" };
    let mut rets = Vec::new();
    for win in w.windows(2) {
        if win[0].acc_nav > 0.0 { rets.push(win[1].acc_nav / win[0].acc_nav - 1.0); }
    }
    let annualized_vol = stdev(&rets) * (252_f64).sqrt();
    let regime = if window_return > p.up_threshold && ma_short > ma_long {
        Regime::Uptrend
    } else if window_return < p.down_threshold && ma_short < ma_long {
        Regime::Downtrend
    } else {
        Regime::Range
    };
    let (regime_cn, rec_strategy, rec_name, rationale) = match regime {
        Regime::Uptrend => ("上涨趋势", "smart_dca", "智能定投", "上涨趋势：顺势持有，频繁进出易踏空"),
        Regime::Downtrend => ("下跌趋势", "trend", "均线择时", "下跌趋势：趋势走坏空仓离场，避开长跌"),
        Regime::Range => ("震荡", "rsi", "RSI超买超卖", "震荡：区间内高抛低吸吃波动"),
    };
    Ok(RegimeReport {
        regime: regime_cn.to_string(),
        window: p.window,
        window_return,
        annualized_vol,
        ma_short,
        ma_long,
        ma_relation: ma_relation.to_string(),
        rec_strategy: rec_strategy.to_string(),
        rec_name: rec_name.to_string(),
        rationale: rationale.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn series(vals: &[f64]) -> Vec<NavPoint> {
        vals.iter().enumerate().map(|(i, v)| NavPoint {
            date: NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(i as i64),
            nav: *v, acc_nav: *v,
        }).collect()
    }

    #[test]
    fn detects_uptrend() {
        // 130 点从 1.0 线性涨到 2.0
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "上涨趋势");
        assert_eq!(r.rec_strategy, "smart_dca");
        assert!(r.window_return > 0.10);
    }

    #[test]
    fn detects_downtrend() {
        let vals: Vec<f64> = (0..130).map(|i| 2.0 - i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "下跌趋势");
        assert_eq!(r.rec_strategy, "trend");
        assert!(r.window_return < -0.10);
    }

    #[test]
    fn detects_range() {
        // 在 1.00/1.01 间小幅交替 → 收益≈0、均线纠缠
        let vals: Vec<f64> = (0..130).map(|i| if i % 2 == 0 { 1.00 } else { 1.01 }).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert_eq!(r.regime, "震荡");
        assert_eq!(r.rec_strategy, "rsi");
    }

    #[test]
    fn insufficient_data_errors() {
        let vals: Vec<f64> = (0..30).map(|_| 1.0).collect();
        let err = detect_regime(&series(&vals), &RegimeParams::default()).unwrap_err();
        assert!(err.to_string().contains("数据不足"), "应提示数据不足: {err}");
    }

    #[test]
    fn volatility_positive_for_moving_series() {
        let vals: Vec<f64> = (0..130).map(|i| 1.0 + i as f64 / 129.0).collect();
        let r = detect_regime(&series(&vals), &RegimeParams::default()).unwrap();
        assert!(r.annualized_vol > 0.0, "上涨序列应有正波动率");
    }
}
```

- [ ] **Step 3: 运行测试**

Run: `cargo test --lib analyze`
Expected: 5 测全过。若不过按报错修正实现。

- [ ] **Step 4: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/lib.rs src/analyze.rs
git commit -m "feat: analyze::detect_regime 行情形态识别 + 策略建议"
```

---

## Task 2: GET /api/regime 路由

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `analyze::{detect_regime, RegimeParams, RegimeReport}`（Task 1）、`cache::load_or_fetch`、`validate_fund_code`、`AppError`。
- Produces: `RegimeQuery{fund_code:String, window:Option<usize>}`、`regime_handler`；`router()` 注册 `GET /api/regime`。

- [ ] **Step 1: 写失败的 hermetic 路由测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增（非法 code 在 load 前短路，不联网）：

```rust
    #[tokio::test]
    async fn regime_route_bad_code_is_400() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/api/regime?fund_code=bad!!code").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 400);
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::regime_route_bad_code_is_400`
Expected: 失败（路由不存在 → 404 ≠ 400，或 handler 未定义）。

- [ ] **Step 3: 实现 + 注册路由**

在 `router()` 加（与现有 get 路由并列）：

```rust
        .route("/api/regime", get(regime_handler))
```

在文件合适处（其它 handler 附近）加：

```rust
#[derive(Debug, Deserialize)]
pub struct RegimeQuery {
    pub fund_code: String,
    #[serde(default)]
    pub window: Option<usize>,
}

async fn regime_handler(
    axum::extract::Query(q): axum::extract::Query<RegimeQuery>,
) -> std::result::Result<axum::Json<crate::analyze::RegimeReport>, AppError> {
    let report = tokio::task::spawn_blocking(move || regime_blocking(q))
        .await
        .map_err(|e| AppError(anyhow!("任务执行失败: {e}")))??;
    Ok(axum::Json(report))
}

fn regime_blocking(q: RegimeQuery) -> Result<crate::analyze::RegimeReport> {
    validate_fund_code(&q.fund_code)?;
    let window = q.window.unwrap_or(120);
    let end = chrono::Local::now().date_naive();
    let start = end - chrono::Duration::days((window as i64) * 2 + 120);
    let points = crate::data::cache::load_or_fetch(
        &q.fund_code, std::path::Path::new(".cache"), start, end)
        .map_err(|e| anyhow!("加载净值失败: {e}"))?;
    let params = crate::analyze::RegimeParams { window, ..Default::default() };
    crate::analyze::detect_regime(&points, &params)
}
```

（`anyhow::{anyhow, Result}`、`Deserialize`、`get` 均已在文件 import；若 `get` 未直接 in scope 用 `axum::routing::get`。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 新测 + 既有全过。

- [ ] **Step 5: clippy + 全量 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add src/web/mod.rs
git commit -m "feat: GET /api/regime 行情形态诊断接口"
```

---

## Task 3: 前端「诊断」Tab

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/mod.rs`（GET / 含诊断 tab 断言）

**Interfaces:**
- Consumes: `GET /api/regime`。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内 `index_has_three_tabs` 附近加：

```rust
    #[tokio::test]
    async fn index_has_diagnose_tab() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("data-tab=\"diagnose\""), "应有诊断 tab");
        assert!(body.contains("/api/regime"), "应调用诊断接口");
        assert!(body.contains("id=\"diag-result\""), "应有诊断结果区");
        assert!(body.contains("不构成"), "应有免责声明");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::index_has_diagnose_tab`
Expected: 失败（无诊断 tab）。

- [ ] **Step 3: page.rs 加 tab 按钮 + panel**

(a) 在 `.tabs` 的 `<button class="tab" data-tab="optimize">寻优</button>` 之后加：

```html
    <button class="tab" data-tab="diagnose">诊断</button>
```

(b) 在最后一个 panel（`id="panel-optimize"` 的 `</div>` 收尾）之后、`<iframe id="result">` 之前，加诊断 panel：

```html
  <div class="panel" id="panel-diagnose">
    <div class="card">
      <div class="row">
        <div class="field combo"><label>基金代码</label><input id="diag-fund" value="161725"/></div>
        <div class="field"><label>窗口(交易日)</label><input type="number" id="diag-window" value="120"/></div>
        <button class="run" id="run-diagnose">诊断</button>
      </div>
      <div id="diag-result" style="margin-top:14px"></div>
      <div class="hint" style="margin-top:10px">说明：基于历史净值的统计描述与启发式规则，不预测未来走势，不构成任何投资建议。</div>
    </div>
  </div>
```

- [ ] **Step 4: page.rs 加诊断 JS**

在 INDEX_HTML 脚本里（其它 attachCombobox 调用处附近）加：

```javascript
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
    + '<div style="color:#5a6a7a;margin-top:4px">'+esc(r.rationale)+'</div>';
}
document.getElementById('run-diagnose').addEventListener('click', function(){
  var btn = this;
  var fund = document.getElementById('diag-fund').value.trim();
  var win = document.getElementById('diag-window').value.trim();
  if(!fund){ document.getElementById('diag-result').innerHTML = '<span style="color:#c0392b">请先填基金代码</span>'; return; }
  var qs = new URLSearchParams({fund_code: fund});
  if(win) qs.append('window', win);
  var t = btn.textContent; btn.disabled = true; btn.textContent = '诊断中…';
  document.getElementById('diag-result').textContent = '诊断中…';
  fetch('/api/regime?' + qs.toString())
    .then(function(res){ if(!res.ok) return res.text().then(function(t){ throw new Error(t); }); return res.json(); })
    .then(renderDiag)
    .catch(function(e){ document.getElementById('diag-result').innerHTML = '<span style="color:#c0392b">'+esc(String(e.message||e))+'</span>'; })
    .finally(function(){ btn.disabled = false; btn.textContent = t; });
});
attachCombobox(document.getElementById('diag-fund'));
```

（`esc`/`attachCombobox` 为脚本顶部全局函数；Tab 切换逻辑已通用处理 `data-tab`/`panel-<id>`，诊断 tab 自动纳入。）

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: `index_has_diagnose_tab` + 既有（含 index_serves_form/index_has_three_tabs/index_has_fund_combobox/index_has_rsi_option/index_has_sync_card）全过。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat: Web 诊断 Tab（行情形态 + 策略建议 + 免责）"
```

---

## Task 4: Playwright 端到端自测

**Files:**
- Modify: `scripts/verify_web.py`

- [ ] **Step 1: 扩展 verify_web.py 覆盖诊断**

在 `scripts/verify_web.py` 既有校验之后、`browser.close()` 之前加：

```python
        # ---- 诊断 tab ----
        page.click('.tab[data-tab="diagnose"]')
        page.fill("#diag-fund", "161725")
        page.click("#run-diagnose")
        page.wait_for_function("document.querySelector('#diag-result').innerText.indexOf('建议策略') >= 0", timeout=60000)
        diag_text = page.locator("#diag-result").inner_text(timeout=10000)
        assert "建议策略" in diag_text, "诊断应给出建议策略"
        body_text = page.locator("#panel-diagnose").inner_text(timeout=10000)
        assert "不构成" in body_text, "应有免责声明"
        page.screenshot(path=str(Path("output/web_diagnose.png").resolve()), full_page=True)
```

（保留脚本顶部清除 `*_PROXY` 逻辑与既有校验；最终 PASS 文案补充含「诊断」。）

- [ ] **Step 2: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 PASS；生成 `output/web_diagnose.png`。

- [ ] **Step 3: 肉眼核对截图**

Read `output/web_diagnose.png`：诊断 tab 出现形态标签（带颜色）、区间收益/波动/均线、建议策略、免责声明。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 4: 全量测试 + 提交**

Run: `cargo test`（全绿）。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 校验诊断 tab"
```

交付报告：161725 当前形态判定 + 推荐策略、截图、各断言结果。

---

## Self-Review

- **Spec 覆盖**：§3 detect_regime/RegimeReport/RegimeParams→T1；§4.1 路由→T2；§4.2 前端 tab→T3；§5 错误→T1(数据不足 Err)/T2(AppError 400)/T3(catch)；§6 测试→T1 纯函数、T2 hermetic 路由、T3 GET/、T4 e2e；§9 免责→T3 panel 文案；§7/§8 边界与影响→各 Task 纯新增。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码（含 RegimeParams::Default）。
- **类型一致**：`detect_regime(&[NavPoint], &RegimeParams)->Result<RegimeReport>`(T1) = T2 调用；`RegimeReport` 字段（regime/window_return/annualized_vol/ma_relation/rec_name/rationale）(T1) = T3 前端读；`RegimeQuery{fund_code,window:Option}`(T2) = 前端 query(T3)。
- **Hermetic 测试**：T1 纯函数；T2 非法 code 在 load 前短路（400，不联网）；live 由 T4 Playwright。
- **不破坏既有**：仅新增模块/路由/tab；引擎/策略/回测/既有 tab 不动；既有测试断言不受影响。
- **YAGNI**：不做真预测、ML、点位预测、多基金排名、自动回测推荐策略。

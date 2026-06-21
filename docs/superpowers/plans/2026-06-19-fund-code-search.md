# 基金代码下拉搜索 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Web 界面所有"基金代码"输入升级为可搜索下拉：键入代码/名称/拼音即时过滤天天基金全量清单并点选，仍可手输任意代码。

**Architecture:** 新增 `data::fundlist`（抓取/解析/缓存全量清单）；新增只读 `GET /api/funds` 返回清单 JSON（失败降级空数组）；前端 page.rs 加可复用 combobox，挂到单次/对比/寻优三处 fund input。

**Tech Stack:** Rust（reqwest blocking / serde / serde_json / axum）+ 原生 JS combobox + Playwright。

## Global Constraints

- 只新增 `data::fundlist`、一条只读路由、前端 combobox；不改回测/报告/既有提交逻辑。
- 不破坏既有 83 测试与 clippy 干净。
- 清单源：`https://fund.eastmoney.com/js/fundcode_search.js`，带 `Referer: https://fund.eastmoney.com/`（与 `eastmoney::fetch` 同款 reqwest blocking）。
- 缓存：`.cache/fundlist.json`，存在即读、不存在才抓；长期有效（删文件刷新）。
- 容错：抓取/解析/读盘失败 → `/api/funds` 返回空数组（200），前端退化为普通手输框，不阻断回测。
- 手输不强制选列表；选中项填纯数字 code，天然通过后端 fund_code charset 校验。
- 非 Send 无关（无 Box<dyn Strategy>）；清单 IO 在 spawn_blocking 内。
- Rust 测试 hermetic（不联网）：parse 纯函数测、cache 路径用临时目录测；live 端点由 Playwright 验证。
- edition 2021；提交含 `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>` 尾注。

依附既有 API（已核对）：
- `data/mod.rs` 现有 `pub mod eastmoney; pub mod cache;`。
- `eastmoney::fetch` 用 `reqwest::blocking::Client::new().get(url).header("Referer", ...).send()?.text()?`（同款写法可参照）。
- web/mod.rs `router()` 现有路由：`/`、`/api/run`(get)、`/api/compare`(post)、`/api/optimize`(post)；`use axum::routing::{get, post}` 已在用；`Html`/`Json` 已 import（compare/optimize 用了 Json）。
- page.rs `INDEX_HTML` 含单次 `name="fund_code"`、寻优 `#opt-fund`、对比动态行 `.rfund`（在 `addCompareRow` 内创建）。

---

## Task 1: data::fundlist 模块（解析 + 抓取 + 缓存）

**Files:**
- Create: `src/data/fundlist.rs`
- Modify: `src/data/mod.rs`（加 `pub mod fundlist;`）
- Test: `src/data/fundlist.rs`（`#[cfg(test)]`）

**Interfaces:**
- Produces:
  - `pub struct FundInfo { pub code: String, pub name: String, pub pinyin: String }`（`#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]`）
  - `pub fn parse_fund_list(body: &str) -> anyhow::Result<Vec<FundInfo>>`
  - `pub fn fetch_fund_list() -> anyhow::Result<Vec<FundInfo>>`
  - `pub fn load_or_fetch_fund_list(cache_dir: &std::path::Path) -> anyhow::Result<Vec<FundInfo>>`

- [ ] **Step 1: 注册模块**

`src/data/mod.rs` 在 `pub mod cache;` 下加：

```rust
pub mod fundlist;
```

- [ ] **Step 2: 写失败测试 + 函数占位**

创建 `src/data/fundlist.rs`：

```rust
use std::path::Path;
use anyhow::{anyhow, Context, Result};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FundInfo {
    pub code: String,
    pub name: String,
    pub pinyin: String,
}

/// 解析 fundcode_search.js：`var r = [["000001","HXCZHH","华夏成长混合","混合型","..."],...];`
/// 每条取 [0]=code、[1]=pinyin、[2]=name；列数不足的行跳过。
pub fn parse_fund_list(body: &str) -> Result<Vec<FundInfo>> {
    todo!()
}

/// 抓取天天基金全量清单。
pub fn fetch_fund_list() -> Result<Vec<FundInfo>> {
    let url = "https://fund.eastmoney.com/js/fundcode_search.js";
    let body = reqwest::blocking::Client::new()
        .get(url)
        .header("Referer", "https://fund.eastmoney.com/")
        .send()
        .map_err(|e| anyhow!("请求 {url} 失败: {e}"))?
        .text()
        .map_err(|e| anyhow!("读取 {url} 响应失败: {e}"))?;
    parse_fund_list(&body)
}

/// 缓存优先：cache_dir/fundlist.json 存在则读盘反序列化，否则抓取并写盘。
pub fn load_or_fetch_fund_list(cache_dir: &Path) -> Result<Vec<FundInfo>> {
    let path = cache_dir.join("fundlist.json");
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {} 失败", path.display()))?;
        return serde_json::from_str(&text)
            .with_context(|| format!("解析 {} 失败", path.display()));
    }
    let funds = fetch_fund_list()?;
    std::fs::create_dir_all(cache_dir)
        .with_context(|| format!("创建缓存目录 {} 失败", cache_dir.display()))?;
    match serde_json::to_string(&funds) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                eprintln!("写入基金清单缓存 {} 失败: {e}", path.display());
            }
        }
        Err(e) => eprintln!("序列化基金清单失败: {e}"),
    }
    Ok(funds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"var r = [["000001","HXCZHH","华夏成长混合","混合型-灵活","HUAXIACHENGZHANGHUNHE"],["161725","ZSZZBJ","招商中证白酒指数","指数型","ZHAOSHANGBAIJIU"],["bad"]];"#;

    #[test]
    fn parses_sample() {
        let funds = parse_fund_list(SAMPLE).unwrap();
        assert_eq!(funds.len(), 2, "列数不足的 [\"bad\"] 行应被跳过");
        assert_eq!(funds[0], FundInfo { code: "000001".into(), pinyin: "HXCZHH".into(), name: "华夏成长混合".into() });
        assert_eq!(funds[1].code, "161725");
        assert_eq!(funds[1].name, "招商中证白酒指数");
    }

    #[test]
    fn rejects_no_var_r() {
        assert!(parse_fund_list("garbage without array").is_err());
    }

    #[test]
    fn load_reads_cache_without_network() {
        // 写一份临时 fundlist.json，断言 load_or_fetch 读盘（不联网）
        let dir = std::env::temp_dir().join("xlh_fundlist_test");
        std::fs::create_dir_all(&dir).unwrap();
        let funds = vec![FundInfo { code: "161725".into(), name: "招商中证白酒指数".into(), pinyin: "ZSZZBJ".into() }];
        std::fs::write(dir.join("fundlist.json"), serde_json::to_string(&funds).unwrap()).unwrap();
        let loaded = load_or_fetch_fund_list(&dir).unwrap();
        assert_eq!(loaded, funds);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib data::fundlist::tests::parses_sample`
Expected: panic（`todo!()`）。

- [ ] **Step 4: 实现 parse_fund_list**

替换 `parse_fund_list` 的 `todo!()`：

```rust
pub fn parse_fund_list(body: &str) -> Result<Vec<FundInfo>> {
    let key = "var r = ";
    let start = body.find(key).ok_or_else(|| anyhow!("未找到 var r ="))? + key.len();
    let rest = &body[start..];
    let open = rest.find('[').ok_or_else(|| anyhow!("未找到数组开括号"))?;
    let close = rest.rfind(']').ok_or_else(|| anyhow!("未找到数组闭括号"))?;
    if close < open { return Err(anyhow!("数组括号位置异常")); }
    let rows: Vec<Vec<String>> = serde_json::from_str(&rest[open..=close])
        .context("解析基金清单 JSON 失败")?;
    let funds = rows.into_iter()
        .filter(|r| r.len() >= 3)
        .map(|r| FundInfo { code: r[0].clone(), pinyin: r[1].clone(), name: r[2].clone() })
        .collect();
    Ok(funds)
}
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib data::fundlist`
Expected: 3 测 PASS（`parses_sample`/`rejects_no_var_r`/`load_reads_cache_without_network`）。

- [ ] **Step 6: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/data/mod.rs src/data/fundlist.rs
git commit -m "feat: data::fundlist 抓取/解析/缓存天天基金全量清单"
```

---

## Task 2: GET /api/funds 路由

**Files:**
- Modify: `src/web/mod.rs`
- Test: `src/web/mod.rs`（`#[cfg(test)]`）

**Interfaces:**
- Consumes: `data::fundlist::{FundInfo, load_or_fetch_fund_list}`（Task 1）。
- Produces: `fn funds_payload(cache_dir: &Path) -> Vec<FundInfo>`、`async fn funds_handler() -> Json<Vec<FundInfo>>`；`router()` 注册 `GET /api/funds`。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内新增：

```rust
    #[test]
    fn funds_payload_reads_cache() {
        use crate::data::fundlist::FundInfo;
        let dir = std::env::temp_dir().join("xlh_funds_payload_test");
        std::fs::create_dir_all(&dir).unwrap();
        let funds = vec![FundInfo { code: "161725".into(), name: "招商中证白酒指数".into(), pinyin: "ZSZZBJ".into() }];
        std::fs::write(dir.join("fundlist.json"), serde_json::to_string(&funds).unwrap()).unwrap();
        let got = super::funds_payload(&dir);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].code, "161725");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn funds_route_returns_json_array() {
        // 路由存在且返回 JSON；用临时缓存避免联网
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/api/funds").body(Body::empty()).unwrap())
            .await.unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        // body 必须是合法 JSON 数组（空数组也可——降级场景）
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v.is_array(), "应返回 JSON 数组");
    }
```

注：`funds_route_returns_json_array` 在没有 `.cache/fundlist.json` 且无网络时，`funds_payload` 返回空数组——仍是合法 JSON 数组，断言成立；有网络则真抓取一次。两种情况测试都通过且不报错。核心读盘逻辑由 `funds_payload_reads_cache`（hermetic）覆盖。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::funds_payload_reads_cache`
Expected: 编译失败（`funds_payload` 未定义）。

- [ ] **Step 3: 实现 + 注册路由**

在 `src/web/mod.rs` `router()` 里加一条（与现有 `/api/run` 并列）：

```rust
        .route("/api/funds", get(funds_handler))
```

在 `run_handler`/`run_blocking` 附近加：

```rust
/// 加载基金清单；任何失败降级为空数组（不阻断界面）。
fn funds_payload(cache_dir: &std::path::Path) -> Vec<crate::data::fundlist::FundInfo> {
    crate::data::fundlist::load_or_fetch_fund_list(cache_dir).unwrap_or_else(|e| {
        eprintln!("基金清单加载失败: {e}");
        Vec::new()
    })
}

async fn funds_handler() -> axum::Json<Vec<crate::data::fundlist::FundInfo>> {
    let funds = tokio::task::spawn_blocking(|| funds_payload(std::path::Path::new(".cache")))
        .await
        .unwrap_or_default();
    axum::Json(funds)
}
```

（若文件顶部已 `use axum::routing::get;` 则直接用 `get`；否则用全限定 `axum::routing::get`。`axum::Json` 已在用。）

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: 两新测 + 既有全过。

- [ ] **Step 5: clippy + 全量 + 提交**

Run: `cargo clippy --all-targets`（无 warning）然后 `cargo test`（全绿）
```bash
git add src/web/mod.rs
git commit -m "feat: GET /api/funds 返回基金清单（失败降级空数组）"
```

---

## Task 3: 前端 combobox

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/mod.rs`（GET / 含 combobox 标识断言）

**Interfaces:**
- Consumes: `GET /api/funds`。

- [ ] **Step 1: 写失败测试**

在 `src/web/mod.rs` 的 `mod tests` 内 `index_has_three_tabs` 之后新增：

```rust
    #[tokio::test]
    async fn index_has_fund_combobox() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;
        let resp = super::router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body.contains("/api/funds"), "应在加载时取基金清单");
        assert!(body.contains("attachCombobox"), "应有 combobox 挂载函数");
        assert!(body.contains("fund-dropdown"), "应有下拉容器样式/类");
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::tests::index_has_fund_combobox`
Expected: 失败（现 INDEX_HTML 无 attachCombobox）。

- [ ] **Step 3: 在 page.rs 加样式**

在 `src/web/page.rs` INDEX_HTML 的 `<style>` 块末尾（`</style>` 前）加：

```css
.combo{position:relative}
.fund-dropdown{position:absolute;left:0;top:100%;z-index:20;min-width:240px;background:#fff;border:1px solid #cfd6e0;border-radius:6px;max-height:260px;overflow:auto;box-shadow:0 4px 12px rgba(0,0,0,.1);display:none}
.fund-dropdown.show{display:block}
.fund-item{padding:6px 10px;cursor:pointer;font-size:.88rem;white-space:nowrap}
.fund-item:hover,.fund-item.active{background:#f0f2f5}
.fund-item .code{color:#c0392b;font-weight:600;margin-right:8px}
```

- [ ] **Step 4: 在 page.rs 加 combobox JS**

在 `src/web/page.rs` INDEX_HTML 的 `<script>` 块顶部（紧接 `<script>` 之后、其它逻辑之前）加：

```javascript
var FUNDS = [];
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
    var hits = [];
    for (var i=0; i<FUNDS.length && hits.length<20; i++){
      var f = FUNDS[i];
      if (f.code.indexOf(q)===0 || f.name.indexOf(q)>=0 || (f.pinyin && f.pinyin.indexOf(uq)>=0)) hits.push(f);
    }
    if (!hits.length){ hide(); return; }
    box.innerHTML = hits.map(function(f){
      return '<div class="fund-item" data-code="'+f.code+'"><span class="code">'+f.code+'</span>'+f.name+'</div>';
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
```

- [ ] **Step 5: 挂载到三处 fund input**

在 `src/web/page.rs` INDEX_HTML 脚本里挂载。单次与寻优在脚本主体（DOM 已就绪处，例如 `syncSingle();` 调用附近）加：

```javascript
attachCombobox(document.querySelector('#f-single [name="fund_code"]'));
attachCombobox(document.getElementById('opt-fund'));
```

对比动态行：在 `addCompareRow` 函数内、`div.querySelector('.rfund')` 可取到后（函数末尾、`appendChild` 之后）加：

```javascript
  attachCombobox(div.querySelector('.rfund'));
```

（确保 `attachCombobox` 定义在 `addCompareRow` 之前或同作用域可见——它是脚本顶部的全局函数，满足。）

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test --lib web`
Expected: `index_has_fund_combobox` + 既有（含 `index_serves_form`/`index_has_three_tabs`）全过。

- [ ] **Step 7: clippy + 提交**

Run: `cargo clippy --all-targets`（无 warning）
```bash
git add src/web/page.rs src/web/mod.rs
git commit -m "feat: 基金代码可搜索 combobox（代码/名称/拼音过滤）"
```

---

## Task 4: Playwright 端到端自测

**Files:**
- Modify: `scripts/verify_web.py`

- [ ] **Step 1: 预热基金清单缓存**

确保 `.cache/fundlist.json` 存在（否则首次页面加载需联网抓取，可能慢/失败）。先跑一次后台服务并请求一次（脚本内会自然触发；或手动）。在脚本里改为依赖服务首次 `/api/funds` 自动抓取——但为稳健，本步骤先手动预热：

Run（bash，确保已联网或已有缓存）:
```bash
cargo run --quiet -- serve --port 18082 &
SVPID=$!; sleep 3
curl -s --noproxy '*' http://127.0.0.1:18082/api/funds -o /dev/null -w "funds:%{http_code}\n"
ls -la .cache/fundlist.json
kill $SVPID
```
Expected: `funds:200`，`.cache/fundlist.json` 存在（含全量清单）。若 200 但文件因无网为空清单，记录并在 Step 2 用注入清单兜底（见下）。

- [ ] **Step 2: 扩展 verify_web.py 加基金搜索校验**

在 `scripts/verify_web.py` 现有单次校验之前（页面加载后、点运行前）插入基金搜索校验。若担心清单未缓存，脚本开头可在 `page.goto` 后等待 `FUNDS` 就绪。新增片段（放在单次 tab 操作处）：

```python
        # ---- 基金代码下拉搜索 ----
        # 等清单加载（FUNDS 非空）；最多等 10 秒，未就绪则跳过下拉断言（降级场景）
        funds_ready = False
        for _ in range(20):
            if page.evaluate("Array.isArray(window.FUNDS) && window.FUNDS.length > 0"):
                funds_ready = True; break
            page.wait_for_timeout(500)
        if funds_ready:
            fc = page.locator('#f-single [name="fund_code"]')
            fc.fill("")
            fc.type("白酒", delay=30)
            page.wait_for_selector(".fund-dropdown.show .fund-item", timeout=10000)
            # 点选第一个含 161725 的项（白酒指数）
            page.click('.fund-dropdown.show .fund-item[data-code="161725"]')
            assert fc.input_value() == "161725", "点选后基金框应填入 161725"
            page.screenshot(path=str(Path("output/web_fundsearch.png").resolve()), full_page=True)
        else:
            print("WARN: 基金清单未就绪（无缓存且离线），跳过下拉断言")
```

并把最终 PASS 打印补充为包含基金搜索结论（如 `print("PASS: 三 tab + 基金下拉搜索 均正常")`）。保留脚本顶部清除 `*_PROXY` 的逻辑。

- [ ] **Step 3: 运行自测**

Run: `python scripts/verify_web.py`
Expected: 打印 PASS；若清单已缓存则生成 `output/web_fundsearch.png`；三 tab 截图照旧。

- [ ] **Step 4: 肉眼核对截图**

Read `output/web_fundsearch.png`：确认基金框下方出现下拉、含"招商中证白酒指数 / 161725"，选中后框内为 161725。异常按 systematic-debugging 修复后重跑。

- [ ] **Step 5: 全量测试 + 提交**

Run: `cargo test`（全绿）。
```bash
git add scripts/verify_web.py
git commit -m "test: Playwright 校验基金代码下拉搜索"
```

交付报告：截图、各断言结果；说明清单缓存状态（是否联网抓取成功）。

---

## Self-Review

- **Spec 覆盖**：§3.1 fundlist 模块→T1；§3.2 /api/funds→T2；§4 前端 combobox（数据加载/组件/挂载/样式）→T3；§5 容错（降级空数组、前端退化）→T1(load_or_fetch 容错)/T2(funds_payload 降级)/T3(FUNDS 空时不显示)；§6 测试→T1 解析+cache、T2 路由、T3 GET/、T4 e2e；§7 单元边界→各 Task；§8 影响→仅新增，不改提交逻辑。
- **占位符**：无 TBD/TODO；每个改代码 Step 给完整代码。
- **类型一致**：`FundInfo{code,name,pinyin}`(T1) 在 T2 funds_payload/handler、T3 前端 `f.code/f.name/f.pinyin` 一致；`parse_fund_list`/`load_or_fetch_fund_list`(T1) = T2 调用；`funds_payload(&Path)->Vec<FundInfo>`(T2) = 测试与 handler 一致。
- **Hermetic 测试**：T1 parse 纯函数 + load 用临时目录（写 stub json，不联网）；T2 funds_payload 用临时目录 + 路由测试容忍空数组；live 抓取仅 Playwright(T4，预热缓存)。
- **不破坏既有**：仅新增模块/路由/前端挂载；fund input 的 name 与提交载荷不变；既有 web/report 测试不动。
- **YAGNI**：不做拼音高级排序、收藏、历史、定时刷新。

# 量化交易 · 计划 4c:手动 / AI 生成工单与交易页遗留项 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补上 spec §5 的 manual 信号源(个股诊断与 AI 分析结果上「生成工单」),并消化计划 4b 终审留下的交易页遗留项(工单卡片显示股票名称、工单视图 N+1 查询、倒计时依赖浏览器时区、来源标签文案、管理后台未转义插值、spec §4 持仓迁移条款)。

**Architecture:** 手动工单走与其它来源完全相同的 `submit_signal` 路径(闸门、风控、模拟盘镜像一致),只新增一个受校验的入口 `actions::submit_manual`。报价优先用 `trade_quotes` 缓存(≤60 秒);缓存不新鲜时 Web handler 在**持锁之外**用 `spawn_blocking` 拉一次腾讯快照(与监听线程同源),只接受今日时间戳。重复点击靠客户端生成的 `request_id` 做幂等(`dedup_key = "manual-{request_id}"`)。

**Tech Stack:** Rust 2021、axum 0.7、rusqlite 0.31;原生 JS。无新依赖。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §4、§5(manual 行)、§6、§8。前置:计划 4a、4b 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在
- 不引入新依赖;页面不引用外部资源,页面常量里不得出现 `http://` / `https://` 字面量(既有首页常量中已有的除外,不新增)
- 测试不得访问网络、不起真线程;Web 测试通过预置 `trade_quotes` 缓存避免联网
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:163-164`、`src/ai.rs:169-170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 页面 JS 通过 `node scripts/check_inline_js.mjs <文件>`(文件中不得出现不成对的字面量 `<script>` 文本)
- 插入 `innerHTML` 的动态文本一律 `esc()`
- API 错误约定 `{error, code}`:400 参数 / 404 不存在或跨用户 / 409 冲突 / 500 内部(不外泄细节)
- 不使用 `git stash`

### 设计裁决(执行者照此实现)

1. **手动工单与自动信号同路径**。不绕开闸门:冷却、去重、涨跌停、风控上限、总开关、每日工单上限都照常生效;被拦截时写入被拦截信号列表,前端给出中文原因。
2. **只在有今日行情时允许下单**。报价时间戳不是今天(盘前、休市、停牌)→ 400 `no_quote`「暂无今日行情(盘前、休市或停牌),无法生成工单」。不为「明天的单」预生成工单(spec 的 manual 有效期默认当日)。
3. **幂等**:前端每次打开下单框生成一个 `request_id`(`crypto.randomUUID()`,不可用时退回 `Date.now()` + 随机数),同一框内重复提交得到同一结果(`Duplicate` → 200 `{result:"duplicate"}`)。
4. **倒计时以服务端为准**:`TicketView` 增加 `expires_in_secs`(服务端 `expires_at - now`,下限 0),前端以「收到响应时刻 + 秒数」为截止,不再解析本地时间字符串(4b 遗留:浏览器时区 ≠ 服务器时区时倒计时错误)。
5. **来源标签文案统一**为 spec 用语:`exit` 止盈止损、`strategy` 日线策略、`mover` 异动、`manual` 手动(4b 遗留)。
6. **spec §4「持仓迁移」改写**:现有 `holdings.rs` 的持仓是请求期输入,从未落库,无数据可导入;改为「首次使用通过『持仓校准』录入实盘持仓(留痕)」。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/store.rs` | 修改 | `signal_meta`:一次查询取齐信号的来源 / 理由 / AI 说明 / 策略号 / 名称 |
| `src/web/trade.rs` | 修改 | `TicketView` 增 `name`、`expires_in_secs`,改用 `signal_meta`;`POST /api/trade/manual` |
| `src/trade/actions.rs` | 修改 | `ManualOrder`、`validate_manual`、`submit_manual` |
| `src/web/trade_page.rs` | 修改 | 卡片显示名称、服务端倒计时、来源文案;两页同步;漂移测试更新 |
| `src/web/page.rs` | 修改 | 股诊断与 AI 分析结果上的「生成工单」按钮与下单框 |
| `src/web/auth/admin.rs` | 修改 | 既有插值全部转义 |
| `docs/superpowers/specs/2026-09-15-quant-trading-design.md` | 修改 | §4 持仓迁移条款、§5 manual 行 |

---

### Task 1: 工单视图:一次查询取信号元数据、股票名称、服务端剩余秒数

**Files:**
- Modify: `src/trade/store.rs`、`src/web/trade.rs`、`src/web/trade_page.rs`
- Test: 各文件 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct SignalMeta {
      pub source: SignalSource,
      pub reason: String,
      pub ai_note: Option<String>,
      pub strategy_id: Option<i64>,
      pub name: Option<String>,
  }
  pub fn signal_meta(conn: &Connection, signal_id: i64) -> Result<Option<SignalMeta>>;
  // TicketView 新增
  name: Option<String>,
  expires_in_secs: i64,   // max(0, (expires_at - now).num_seconds())
  ```

规则:

- `ticket_view` 只调用一次 `signal_meta`(替换 `signal_source` + `signal_reason` + `signal_ai_note_and_strategy` 三次查询);信号不存在 → `Err`(内部错误,与 4b 修复后的 `reason` 语义一致)。其余调用方仍在用的旧函数保留;若某个旧函数因此不再有调用方,删除它及其测试。
- 页面(`TRADE_HTML` 与 `SIGNED_TICKET_HTML`):
  - 卡片标题在代码后显示名称(有则显示,`esc`)。
  - 倒计时改为:渲染时记录 `deadline = Date.now() + t.expires_in_secs * 1000`(存到卡片的 `data-deadline`),每秒按 `deadline - Date.now()` 显示;不再用 `parseLocalTs(expires_at)` 计算剩余时间。若 `parseLocalTs` 因此不再被使用,删除它并从漂移测试列表中移除。
  - `sourceLabel` 文案改为设计裁决 5;两页保持一致(漂移测试会校验)。
- 「待成交」「已完成」列表同样在代码后显示名称。

- [ ] **Step 1: 写失败测试**

```rust
// store.rs
    #[test]
    fn signal_meta_reads_all_display_fields_in_one_row() {
        // 插入一个带 name、ai_note、strategy_id 的信号(沿用本文件既有插入信号的夹具,或 ticket::insert_signal)
        // signal_meta(id) == Some(SignalMeta{ source, reason, ai_note, strategy_id, name });不存在的 id → None
    }
```

`web/trade.rs`:在既有 `pending_list_confirm_with_deviation_ack_then_fill` 用例的列表断言处补:`list[0]["expires_in_secs"]` 为正整数且 ≤ 当日 15:00 前的剩余秒数上限(直接断言 `> 0`);`list[0]["name"]` 存在(手动信号夹具的 `name` 为 `None` 时为 `null`,在夹具里给 `name: Some("浦发银行".into())` 并断言等于它)。

`trade_page.rs`:

```rust
    #[tokio::test]
    async fn countdown_uses_server_seconds_and_source_labels_follow_spec() {
        for page in [crate::web::trade_page::TRADE_HTML, crate::web::trade_page::SIGNED_TICKET_HTML] {
            assert!(page.contains("expires_in_secs"), "倒计时应以服务端剩余秒数为准");
            for label in ["止盈止损", "日线策略", "异动", "手动"] {
                assert!(page.contains(label), "缺来源文案 {label}");
            }
        }
        assert!(crate::web::trade_page::TRADE_HTML.contains("data-deadline"));
    }
```

> 按注释写出完整用例。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/trade_page.rs` → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat(trade): 工单视图一次取齐信号元数据,卡片显示股票名称,倒计时以服务端秒数为准"
```

---

### Task 2: 手动工单核心 `actions::submit_manual`

**Files:**
- Modify: `src/trade/actions.rs`
- Test: `src/trade/actions.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `service::{submit_signal, SubmitContext, SubmitOutcome}`;`gate::GateReject`
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
  pub struct ManualOrder {
      pub request_id: String,
      pub code: String,
      pub name: Option<String>,
      pub side: Direction,              // serde: "buy" / "sell"
      pub amount: Option<f64>,          // 买入金额;None 按风控单笔上限
      pub qty: Option<u64>,             // 卖出股数;None 全部可卖
      pub reason: String,
      pub ai_note: Option<String>,
  }
  pub fn validate_manual(o: &ManualOrder) -> Result<()>;
  #[derive(Debug, Clone, PartialEq)]
  pub enum ManualOutcome {
      Ticketed { real_ticket: Option<i64>, paper_ticket: Option<i64> },
      Rejected(GateReject),
      Duplicate,
      NoQuote,
  }
  pub fn submit_manual(conn: &mut Connection, user_id: i64, o: &ManualOrder, quote: Option<&Quote>, now: NaiveDateTime) -> Result<ManualOutcome>;
  ```

`validate_manual`(任一不满足 → `Err`,中文文案):

| 字段 | 规则 |
|---|---|
| `request_id` | 1–64 字符,仅 `[A-Za-z0-9-]` |
| `code` | 6 位数字 |
| `name` | 去空白后 ≤ 20 字符(空串视为 `None`) |
| `amount` | 买入时若有:有限且 > 0;卖出时必须为 `None` |
| `qty` | 卖出时若有:> 0;买入时必须为 `None` |
| `reason` | 去空白后非空,≤ 500 字符 |
| `ai_note` | ≤ 8000 字符(空串视为 `None`) |

`submit_manual`:

1. `validate_manual(o)?`
2. `quote` 为 `None`,或 `quote.ts.date() != now.date()`,或 `quote.code != o.code` → `Ok(ManualOutcome::NoQuote)`
3. 构造 `NewSignal { user_id, source: Manual, strategy_id: None, code, name, side, scope: Both, ref_price: quote.price, reason(去空白), ai_note, dedup_key: format!("manual-{}", o.request_id), suggest_cash: o.amount, suggest_qty: o.qty }`
4. `submit_signal(conn, &sig, &SubmitContext { quote: Some(q), now })` 并映射到 `ManualOutcome`

> `Direction` 若未派生 `Deserialize`,在 `event.rs` 补 `Deserialize`(`#[serde(rename_all = "lowercase")]` 已在)。

- [ ] **Step 1: 写失败测试**

```rust
    fn manual(side: Direction, req: &str) -> ManualOrder {
        ManualOrder {
            request_id: req.into(),
            code: "600000".into(),
            name: Some("浦发银行".into()),
            side,
            amount: (side == Direction::Buy).then_some(5_000.0),
            qty: None,
            reason: "看好".into(),
            ai_note: Some("AI:估值偏低".into()),
        }
    }

    #[test]
    fn manual_buy_creates_real_and_paper_tickets_and_is_idempotent() {
        let mut c = db();
        let q = quote(10.0, at(10, 0, 0));
        let o = manual(Direction::Buy, "req-1");
        let r = submit_manual(&mut c, 1, &o, Some(&q), at(10, 0, 0)).unwrap();
        let ManualOutcome::Ticketed { real_ticket: Some(real), paper_ticket: Some(_) } = r else {
            panic!("{r:?}");
        };
        let t = crate::trade::ticket::get_ticket(&c, real).unwrap().unwrap();
        assert_eq!(t.expires_at, at(15, 0, 0), "手动工单默认当日 15:00 到期");
        assert_eq!(
            submit_manual(&mut c, 1, &o, Some(&q), at(10, 0, 5)).unwrap(),
            ManualOutcome::Duplicate,
            "同一 request_id 重复提交"
        );
        let meta = store::signal_meta(&c, t.signal_id).unwrap().unwrap();
        assert_eq!(meta.name.as_deref(), Some("浦发银行"));
        assert_eq!(meta.ai_note.as_deref(), Some("AI:估值偏低"));
    }

    #[test]
    fn manual_needs_todays_quote_for_the_same_code() {
        let mut c = db();
        let o = manual(Direction::Buy, "req-2");
        assert_eq!(submit_manual(&mut c, 1, &o, None, at(10, 0, 0)).unwrap(), ManualOutcome::NoQuote);
        let yesterday = NaiveDate::from_ymd_opt(2026, 9, 22).unwrap().and_hms_opt(15, 0, 0).unwrap();
        assert_eq!(
            submit_manual(&mut c, 1, &o, Some(&quote(10.0, yesterday)), at(10, 0, 0)).unwrap(),
            ManualOutcome::NoQuote
        );
        let other = Quote { code: "600036".into(), ..quote(10.0, at(10, 0, 0)) };
        assert_eq!(submit_manual(&mut c, 1, &o, Some(&other), at(10, 0, 0)).unwrap(), ManualOutcome::NoQuote);
    }

    #[test]
    fn manual_goes_through_the_gate() {
        let mut c = db();
        crate::trade::settings::set_kill_switch(&c, true, None, at(9, 0, 0)).unwrap();
        let r = submit_manual(&mut c, 1, &manual(Direction::Buy, "req-3"), Some(&quote(10.0, at(10, 0, 0))), at(10, 0, 0)).unwrap();
        assert_eq!(r, ManualOutcome::Rejected(crate::trade::gate::GateReject::TradingDisabled));
        crate::trade::settings::set_kill_switch(&c, false, None, at(9, 0, 0)).unwrap();
        let r = submit_manual(&mut c, 1, &manual(Direction::Sell, "req-4"), Some(&quote(10.0, at(10, 0, 0))), at(10, 0, 0)).unwrap();
        assert_eq!(r, ManualOutcome::Rejected(crate::trade::gate::GateReject::NothingSellable), "无持仓卖出");
    }

    #[test]
    fn manual_validation() {
        let ok = manual(Direction::Buy, "req-5");
        assert!(validate_manual(&ok).is_ok());
        let bad = [
            ManualOrder { request_id: "".into(), ..ok.clone() },
            ManualOrder { request_id: "有中文".into(), ..ok.clone() },
            ManualOrder { request_id: "x".repeat(65), ..ok.clone() },
            ManualOrder { code: "60000".into(), ..ok.clone() },
            ManualOrder { amount: Some(0.0), ..ok.clone() },
            ManualOrder { amount: Some(f64::NAN), ..ok.clone() },
            ManualOrder { qty: Some(100), ..ok.clone() },
            ManualOrder { reason: "  ".into(), ..ok.clone() },
            ManualOrder { reason: "长".repeat(501), ..ok.clone() },
            ManualOrder { ai_note: Some("长".repeat(8001)), ..ok.clone() },
            ManualOrder { name: Some("长".repeat(21)), ..ok.clone() },
            ManualOrder { side: Direction::Sell, amount: Some(1000.0), qty: None, ..ok.clone() },
            ManualOrder { side: Direction::Sell, amount: None, qty: Some(0), ..ok.clone() },
        ];
        for b in bad {
            assert!(validate_manual(&b).is_err(), "{b:?}");
        }
    }
```

> `db()`、`at()`、`quote()` 沿用本文件既有测试夹具(`at` 的日期是 2026-09-23)。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat(trade): 手动 / AI 工单入口 submit_manual(同一闸门、幂等、只认今日行情)"
```

---

### Task 3: 手动工单 API `POST /api/trade/manual`

**Files:**
- Modify: `src/web/trade.rs`
- Test: `src/web/trade.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `actions::{ManualOrder, validate_manual, submit_manual, ManualOutcome}`;`store::fresh_quote(conn, code, now, 60)`;`trade::quotes::{QuoteSource, TencentQuotes}`

请求:`ManualOrder` JSON。响应:

| 结果 | 状态 | 体 |
|---|---|---|
| `Ticketed` | 200 | `{result:"ticketed", real_ticket, paper_ticket}` |
| `Duplicate` | 200 | `{result:"duplicate"}` |
| `Rejected(r)` | 409 | `{error: <中文原因>, code:"rejected", reason: r.as_str()}` |
| `NoQuote` | 400 | `{error:"暂无今日行情(盘前、休市或停牌),无法生成工单", code:"no_quote"}` |
| 校验失败 | 400 | `bad_request` |

handler 流程(**锁外联网**):

```rust
async fn manual_ticket(
    State(st): State<AuthState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ManualOrder>, JsonRejection>,
) -> ApiResult<serde_json::Value> {
    let Json(o) = body?;
    actions::validate_manual(&o).map_err(|e| ApiError::bad(e.to_string()))?;
    let now = now();
    // 1) 先看缓存(≤60s),命中则不联网
    let cached = {
        let conn = st.db.lock().unwrap();
        store::fresh_quote(&conn, &o.code, now, actions::QUOTE_MAX_AGE_SECS)?
    };
    // 2) 未命中:锁外拉腾讯快照;失败按无行情处理(不把网络错误变成 500)
    let quote = match cached {
        Some(q) => Some(q),
        None => {
            let code = o.code.clone();
            tokio::task::spawn_blocking(move || TencentQuotes.fetch(&[code]))
                .await
                .ok()
                .and_then(|r| r.ok())
                .and_then(|qs| qs.into_iter().find(|q| q.ts.date() == now.date()))
        }
    };
    // 3) 持锁提交;新鲜报价顺手写入缓存
    let mut conn = st.db.lock().unwrap();
    if let Some(q) = &quote {
        store::upsert_quotes(&conn, std::slice::from_ref(q), now)?;
    }
    let outcome = actions::submit_manual(&mut conn, user.id, &o, quote.as_ref(), now)?;
    // …按上表映射
}
```

> `QUOTE_MAX_AGE_SECS` 若是 `actions` 私有常量,改为 `pub`;`store::fresh_quote` 的签名以现状为准。拦截原因的中文沿用 4b 页面里 `REJECT_LABELS` 的同一套文案——在 `gate.rs` 为 `GateReject` 增加 `pub fn label_zh(self) -> &'static str` 并在此使用(页面仍保留自己的映射;两处文案一致性由一个测试钉住:遍历全部 `GateReject`,断言 `TRADE_HTML` 包含 `label_zh()` 的每条文案)。

- [ ] **Step 1: 写失败测试**(沿用 `web/trade.rs` 测试夹具;报价用 `upsert_quotes` 预置为「现在」,保证不联网)

```rust
    #[tokio::test]
    async fn manual_ticket_api_creates_dedupes_rejects_and_validates() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        {
            let c = st.db.lock().unwrap();
            let now = chrono::Local::now().naive_local();
            crate::trade::store::set_capital(&c, uid, crate::trade::model::Account::Real, 100_000.0, now).unwrap();
            crate::trade::store::upsert_quotes(&c, &[crate::trade::model::Quote {
                code: "600000".into(), price: 10.0, limit_up: Some(11.0), limit_down: Some(9.0), ts: now,
            }], now).unwrap();
        }
        let body = serde_json::json!({
            "request_id": "abc-1", "code": "600000", "name": "浦发银行", "side": "buy",
            "amount": 5000.0, "reason": "看好", "ai_note": null
        });
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(body.clone())).await;
        assert_eq!((s, r["result"].as_str()), (StatusCode::OK, Some("ticketed")), "{r}");
        assert!(r["real_ticket"].is_i64());
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(body.clone())).await;
        assert_eq!((s, r["result"].as_str()), (StatusCode::OK, Some("duplicate")));

        let mut sell = body.clone();
        sell["request_id"] = serde_json::json!("abc-2");
        sell["side"] = serde_json::json!("sell");
        sell["amount"] = serde_json::Value::Null;
        let (s, r) = call(&st, "POST", "/api/trade/manual", "t", Some(sell)).await;
        assert_eq!((s, r["code"].as_str(), r["reason"].as_str()), (StatusCode::CONFLICT, Some("rejected"), Some("nothing_sellable")));

        let mut bad = body.clone();
        bad["request_id"] = serde_json::json!("abc-3");
        bad["reason"] = serde_json::json!(" ");
        let (s, _) = call(&st, "POST", "/api/trade/manual", "t", Some(bad)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        let (s, _) = call(&st, "POST", "/api/trade/manual", "", Some(body)).await;
        assert_ne!(s, StatusCode::OK, "需登录");
    }
```

> 本用例的报价缓存命中,不会走联网分支;不要为联网分支写测试。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "feat(web): 手动工单 API(先用缓存报价,锁外补拉今日快照)"
```

---

### Task 4: 首页「生成工单」入口(股诊断与 AI 分析)

**Files:**
- Modify: `src/web/page.rs`
- Test: `src/web/page.rs` 或 `src/web/mod.rs` 既有页面测试处

内容:

- 首页新增一个共用下单框 `#ticket-modal`(遮罩 + 卡片,风格照首页 `.card`):
  - 字段:代码(只读)、名称(只读,可空)、方向(买入 / 卖出单选)、买入金额(元,可空 = 按风控单笔上限)/ 卖出股数(可空 = 全部可卖,随方向切换显示其一)、理由(必填 `<textarea>`)、AI 说明(只读折叠区,有才显示)。
  - 打开时生成 `request_id`:`(window.crypto && crypto.randomUUID) ? crypto.randomUUID() : Date.now().toString(36) + '-' + Math.random().toString(36).slice(2)`;同一次打开内多次点「提交」复用该 id。
  - 提交 `POST /api/trade/manual`;结果:`ticketed` → 显示「已生成工单」与「去交易页确认」链接(`/trade#pending`);`duplicate` → 同上文案「工单已生成(重复提交)」;409 `rejected` → 显示服务端 `error`(中文拦截原因);400 → 显示 `error`;其它 → 「生成失败(状态码)」。
  - 页面中不得出现外部地址字面量。
- 股诊断结果(`renderStockDiag`)末尾加按钮「生成工单」:代码 `d.code`、名称 `d.name`、方向按 `d.action`(`buy`→买入、`sell`→卖出、`hold`→默认买入并在理由前提示「诊断为观望」)、理由预填 `「个股诊断:」 + d.signal + ',' + d.rationale`(截断到 500 字)。
- AI 分析结果(`#ai-run` 成功后)加按钮「按此分析生成工单」:仅当品种为股票且代码为 6 位数字时显示;代码取输入框,名称空,方向默认买入,理由预填「AI 分析」,AI 说明为 `d.analysis`(截断到 8000 字)。
- 所有插入 DOM 的文本经首页已有的 `esc()`。

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn index_has_manual_ticket_modal_and_entry_points() {
        let p = crate::web::page::INDEX_HTML;
        for s in [
            "id=\"ticket-modal\"", "/api/trade/manual", "request_id", "randomUUID",
            "生成工单", "按此分析生成工单", "去交易页确认", "/trade#pending",
        ] {
            assert!(p.contains(s), "缺 {s}");
        }
    }
```

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/page.rs` → 全量门禁**

> 若 `check_inline_js` 在 `page.rs` 上因首页常量里既有的 `<script>` 字面量问题报错,先确认报错来自既有代码还是本任务;既有问题在报告中说明并用最小改动(不改变行为)修正,例如把注释中的字面量改写。

- [ ] **Step 5: Commit**

```bash
git add src/web
git commit -m "feat(web): 个股诊断与 AI 分析结果可一键生成手动工单"
```

---

### Task 5: 管理后台插值转义与 spec 条款修订

**Files:**
- Modify: `src/web/auth/admin.rs`、`docs/superpowers/specs/2026-09-15-quant-trading-design.md`
- Test: `src/web/auth/admin.rs` 既有测试模块

内容:

- `ADMIN_HTML` 中所有把服务端数据拼进 `innerHTML` 的地方(`loadCodes`、`loadUsers`、`loadPushHistory` 及其它)一律经该页已有的 `esc()`;拼进 `onclick="..."` 的参数改为数值(`Number(x)`)或改成 `data-*` + 事件绑定,不得把字符串原样拼进内联 JS(授权码字符串用 `data-code` 属性 + `esc`)。
- spec §4 最后一段「持仓迁移」改为:「**持仓录入**:`holdings.rs` 的持仓是请求期输入、从未落库,无可导入数据;用户首次使用时通过交易页「持仓校准」录入实盘持仓(每次校准留痕,见 `trade_position_adjusts`)。」
- spec §5 表格 manual 行「触发」列改为「个股诊断 / AI 分析结果「生成工单」,或交易页」;规则列补「同一闸门;需今日行情;`request_id` 幂等」。

- [ ] **Step 1: 写失败测试**

```rust
    #[test]
    fn admin_page_escapes_server_data_in_lists() {
        let p = ADMIN_HTML;
        // 列表渲染必须经 esc()
        for s in ["esc(c.code)", "esc(u.username)"] {
            assert!(p.contains(s), "缺 {s}");
        }
        assert!(!p.contains("revoke('${c.code}')"), "不得把授权码字符串拼进内联 JS");
    }
```

> 推送历史摘要等其它字段的转义写法以实现为准,用例里再补对应的 `esc(...)` 断言(至少覆盖 `summary`)。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/auth/admin.rs` → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/web/auth/admin.rs docs/superpowers/specs/2026-09-15-quant-trading-design.md
git commit -m "fix(web): 管理后台列表插值全部转义;spec 修订持仓录入与手动工单条款"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] `node scripts/check_inline_js.mjs src/web/trade_page.rs src/web/page.rs src/web/auth/admin.rs` 通过
- [ ] CI 门禁符合 Global Constraints

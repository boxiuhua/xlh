# 量化交易 · 计划 4b:`/trade` 交易页面 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用计划 4a 的 JSON API 做出 spec §8 的 `/trade` 交易页面(七个标签、偏离二次确认、行情延迟禁用、心跳状态),签名链接落地页 `/trade/t/:id`,以及管理后台的总开关按钮;顺带消化 4a 终审留下的后端小项。

**Architecture:** 与既有页面一致:整页 HTML 作为 Rust 字符串常量(`src/web/trade_page.rs`),无构建工具、无外部依赖、原生 JS。页面只调 `/api/trade/*`,不含业务规则(偏离、行情新鲜度等判定以服务端返回为准,前端只做展示与二次确认交互)。所有插入 DOM 的数据一律经 `esc()` 转义(理由、名称、AI 说明可能含任意文本)。

**Tech Stack:** Rust 2021、axum 0.7;原生 HTML/CSS/JS(ES2017,`async/await`);校验用本机 Node(`node` ≥ 18,仅开发期语法检查,不进依赖)。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §8、§10.6、§11。API 契约见 `docs/superpowers/plans/2026-09-23-quant-trading-p4a-trade-api.md`(Task 6–9 的路由表)。前置:计划 4a 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在
- 不引入新依赖(Rust crate 与前端库都不引入;页面不得引用任何外部 CDN / 字体 / 脚本)
- 测试不得访问网络、不起真线程
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`
- 页面 JS 必须通过 `node scripts/check_inline_js.mjs <文件>` 语法检查(Task 2 建立该脚本)
- 所有动态文本经 `esc()` 转义后再拼进 `innerHTML`;不得用 `innerHTML` 插入未转义的服务端数据
- API 错误约定(4a):`{error, code}`;409 的 `code` 取值 `already_handled` / `deviation` / `stale_quote` / `kill_switch` / `paper_ticket`
- 界面文案中文;金额保留 2 位小数,比例显示为百分比 1 位小数

### 设计裁决(执行者照此实现)

1. **独立页面,不塞进首页的标签组**。首页已有基金 / 股票 / AI 三组十余个标签;交易是实盘操作,单独一页、单独地址(spec §8 就叫 `/trade`),首页顶栏加入口链接。
2. **登录判定照首页**:`GET /trade` 挂在 `public` 组,handler 自己查会话,未登录 302 到 `/login`(与 `index` 同一写法)。
3. **15 秒刷新只刷「待确认」与顶部状态**(spec §8:现价 15s 刷新)。其它标签切换进入时拉一次,操作后重拉。
4. **偏离二次确认的状态机在前端**:第一次点击确认若服务端返回 409 `deviation`,按钮文字变为「价格已偏离 X.X%,仍要确认」并变色;第二次点击带 `ack_deviation: true`。列表里 `deviation > deviation_th` 时按钮初始就显示该文字(但仍需点击这一次才带 ack——即页面预判偏离时一次点击即带 ack,未预判到而服务端判出时需第二次)。`quote.stale` 为真或无报价时按钮禁用并显示「行情延迟」。
5. **签名链接落地页只读一张工单 + 确认按钮**,不加载其它任何数据;响应头 `Referrer-Policy: no-referrer`,页面 `<meta name="referrer" content="no-referrer">`(4a 终审 M8:签名不得经 Referer 外泄)。
6. **成绩单四栏**(spec §10.6):样本内 / 样本外 / 模拟盘 / 实盘。样本内只有夏普(`oos.is_sharpe`);样本外取 `oos` 的年化、超额(`oos.oos_return − oos.buy_hold_return`)、夏普、最大回撤、胜率(`trade_baseline.win_rate`)、笔数;模拟盘 / 实盘取 `StageMetrics` 的胜率、盈亏比、笔数、最大回撤、平均每笔收益、已实现盈亏。没有的格子显示「—」。附加一行「执行损耗(中位数)」。
7. **「已完成」只显示终态工单**(4a 终审 M5):`filled / expired / rejected / cancelled`,实盘与模拟盘都含,SQL 内 `ORDER BY id DESC LIMIT 100`(4a 遗留:不再全表读进内存)。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/web/trade.rs` | 修改 | `TicketView` 增加预估金额 / 费用 / AI 说明 / 策略号;`done` 视图改终态 + SQL 分页 |
| `src/trade/ticket.rs` | 修改 | `list_done_tickets(conn, user_id, limit)` |
| `src/trade/actions.rs` | 修改 | `manual_fill` 预校验账户资金行(4a 终审遗留) |
| `src/trade/settings.rs`、`src/web/auth/admin.rs` | 修改 | 总开关记录操作人 |
| `scripts/check_inline_js.mjs` | 新建 | 从 Rust 源文件中抽出 `<script>` 块做语法检查 |
| `src/web/trade_page.rs` | 新建 | `TRADE_HTML`、`SIGNED_TICKET_HTML` 与两个 handler |
| `src/web/mod.rs` | 修改 | 路由接线;首页顶栏加「交易」入口 |
| `src/web/page.rs` | 修改 | 首页顶栏入口链接 |
| `src/web/auth/admin.rs` | 修改 | 管理后台「交易总开关」区块 |

---

### Task 1: 后端小项:工单视图字段、已完成分页、回填预校验、总开关留痕

**Files:**
- Modify: `src/web/trade.rs`、`src/trade/ticket.rs`、`src/trade/actions.rs`、`src/trade/settings.rs`、`src/web/auth/admin.rs`
- Test: 各文件内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // TicketView 新增(JSON 字段名即 Rust 字段名)
  est_amount: f64,          // suggest_price × qty
  est_fee: f64,             // StockFee::a_share():买入 buy_fee(amount),卖出 sell_fee(qty, price, 0)
  ai_note: Option<String>,  // 来自信号
  strategy_id: Option<i64>, // 来自信号
  // ticket.rs
  pub fn list_done_tickets(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<Ticket>>;
  // settings.rs
  pub fn set_kill_switch(conn: &Connection, on: bool, by: Option<i64>, now: NaiveDateTime) -> Result<()>;
  pub struct KillSwitchState { pub on: bool, pub by: Option<i64>, pub updated_at: Option<NaiveDateTime> }  // Serialize
  pub fn kill_switch_state(conn: &Connection) -> Result<KillSwitchState>;
  ```

规则:

- `list_done_tickets`:`WHERE user_id = ?1 AND status IN ('filled','expired','rejected','cancelled') ORDER BY id DESC LIMIT ?2`。`GET /api/trade/tickets?view=done` 改用它,删除原来的全量读取 + 内存截断。
- `signal_source` / `signal_reason` 旁新增一个读取信号 `ai_note`、`strategy_id` 的函数(放 `store.rs`,名称自定),一次查询取齐;信号不存在时两者为 `None`。`reason` 的读取错误不再 `unwrap_or_default` 吞掉,改为 `?` 上抛(4a 遗留)。
- `manual_fill` 预校验:`store::get_account(tx, user_id, t.account)?` 为 `None` → `Validation("未设置账户资金,请先在「风控设置」里设置总资金")`(4a 终审遗留:此前会落到 `add_cash` 报错而变 500)。
- 总开关:`set_kill_switch` 增加 `by` 参数,另写 `kill_switch_by` 键(值为用户 id 字符串,`None` 写空串);`kill_switch_state` 读出开关、操作人与该键的 `updated_at`。管理员 `GET /api/admin/trade/kill-switch` 返回 `KillSwitchState`;`POST` 传当前管理员 id(`CurrentUser` 通过 `Extension` 取,照 `admin.rs` 其它需要当前用户的 handler 写法;若该文件无先例,则在路由层已有 `require_login` 注入的 `Extension<CurrentUser>` 下直接取)。更新所有 `set_kill_switch` 调用点(含测试)。

- [ ] **Step 1: 写失败测试**

```rust
// ticket.rs
    #[test]
    fn done_list_is_terminal_only_newest_first_and_limited() {
        // 造 5 张工单:pending、confirmed、filled、expired、cancelled(沿用本文件既有造单夹具)
        // list_done_tickets(user, 10) → 3 张,id 倒序;limit = 2 → 2 张;其他用户 → 空
    }
// actions.rs
    #[test]
    fn manual_fill_without_account_row_is_validation_error() {
        // 用户没有 trade_accounts 行,但有实盘持仓(calibrate_position 造)与一张已确认的实盘卖出工单
        // (直接用 ticket::create_ticket 写入 status = Confirmed)→ manual_fill 返回 Ok(Err(FillError::Validation(_)))
    }
// settings.rs
    #[test]
    fn kill_switch_records_who_and_when() {
        let c = db();
        assert_eq!(kill_switch_state(&c).unwrap().on, false);
        set_kill_switch(&c, true, Some(9), now()).unwrap();
        let s = kill_switch_state(&c).unwrap();
        assert_eq!((s.on, s.by, s.updated_at), (true, Some(9), Some(now())));
    }
```

`web/trade.rs`:在既有 `pending_list_confirm_with_deviation_ack_then_fill` 用例的列表断言后补:`est_amount ≈ 10.0 × qty`、`est_fee > 0`、`strategy_id` 为 `null`;`done` 视图在回填后包含该工单,且不含任何 `pending`/`confirmed` 工单。

> 按注释写出完整用例。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src
git commit -m "feat(trade): 工单视图补预估金额与费用、已完成改终态分页、回填预校验资金行、总开关留痕"
```

---

### Task 2: 页面骨架、路由、语法检查脚本与首页入口

**Files:**
- Create: `src/web/trade_page.rs`、`scripts/check_inline_js.mjs`
- Modify: `src/web/mod.rs`、`src/web/page.rs`
- Test: `src/web/trade_page.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  pub const TRADE_HTML: &str;
  pub async fn trade_page(State(st): State<AuthState>, headers: HeaderMap) -> Response; // 未登录 302 /login
  ```
- 页面内全局 JS(后续任务在同一 `<script>` 里追加,须保持这些名字):
  ```js
  function esc(s)                       // HTML 转义 & < > " '
  async function api(path, method, body) // 返回 {ok, status, data};401 → location.href='/login'
  function toast(msg, kind)             // kind: 'ok' | 'err',3 秒消失
  function fmtMoney(x), fmtPct(x), fmtTime(ts), sideLabel(side), sourceLabel(src), statusLabel(st)
  function showTab(name)                // 切换标签并调用该标签的 load 函数
  const LOADERS = {}                    // 各标签把自己的 load 函数挂在这里:LOADERS.pending = loadPending ...
  ```

`scripts/check_inline_js.mjs`:

```js
// 用法:node scripts/check_inline_js.mjs <Rust 源文件>...
// 抽出源文件中每个 <script>…</script> 块,用 vm.Script 做语法检查(不执行)。
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

let failed = false;
for (const file of process.argv.slice(2)) {
  const src = readFileSync(file, 'utf8');
  const re = /<script>([\s\S]*?)<\/script>/g;
  let m, n = 0;
  while ((m = re.exec(src)) !== null) {
    n += 1;
    try {
      new vm.Script(m[1], { filename: `${file}#script${n}` });
    } catch (e) {
      failed = true;
      console.error(`${file} 第 ${n} 个 <script> 语法错误: ${e.message}`);
    }
  }
  if (n === 0) {
    failed = true;
    console.error(`${file} 中没有找到 <script> 块`);
  }
  console.log(`${file}: 检查了 ${n} 个 <script> 块`);
}
process.exit(failed ? 1 : 0);
```

页面骨架要求:

- 顶部状态条 `#trade-bar`:用户名、`#hb-monitor`(「监听 ●正常 / ●中断」)、`#hb-eval`(「评估 ●正常 / ●中断」)、`#kill-banner`(总开关打开时显示红色横幅「管理员已暂停交易:不生成新工单、不能确认」)、`#trading-off`(用户自己关了交易时显示「你已关闭交易」)、实盘 / 模拟盘可用资金 `#acc-real`、`#acc-paper`、「返回主页」链接。数据来自 `GET /api/trade/overview`。
- 标签按钮 `.tab[data-tab=…]`,七个:`pending` 待确认、`working` 待成交、`done` 已完成、`rejected` 被拦截的信号、`strategies` 策略、`risk` 风控设置、`positions` 持仓校准;面板 `#panel-<name>`。
- 首次进入显示 `pending`;URL hash(`#strategies` 等)可直达并在切换时更新。
- 每 15 秒:若当前标签是 `pending` 调 `LOADERS.pending()`;无论哪个标签都刷新顶部状态条。页面不可见(`document.hidden`)时跳过。
- 样式照首页(`src/web/page.rs` 的配色与 `.card`、`.tab` 风格),自带,不引外部资源;窄屏(< 640px)卡片单列。
- 本任务各面板只放一个空容器与「加载中…」,具体内容在 Task 3–5。

路由:`src/web/mod.rs` 声明 `pub mod trade_page;`,`public` 组加 `.route("/trade", get(trade_page::trade_page))`。handler 的会话判断照 `index`(读 cookie → `lookup_session_user`),已登录返回 `Html(TRADE_HTML)`,未登录 `Redirect::to("/login")`。

首页入口:`src/web/page.rs` 顶栏 `#xlh-bar` 里「管理后台」链接之前加 `<a href="/trade" style="color:#fca5a5">交易</a>`。

- [ ] **Step 1: 写失败测试**

```rust
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

    async fn get(st: &crate::web::auth::AuthState, uri: &str, cookie: Option<&str>) -> (StatusCode, String, axum::http::HeaderMap) {
        let mut req = Request::builder().uri(uri);
        if let Some(c) = cookie {
            req = req.header("cookie", format!("xlh_session={c}"));
        }
        let resp = crate::web::router(st.clone()).oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).to_string(), headers)
    }

    fn login(st: &crate::web::auth::AuthState) -> &'static str {
        let c = st.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, "u", "h", false).unwrap();
        let today = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, today + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, "tok", uid, today + chrono::Duration::days(1)).unwrap();
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
        for tab in ["pending", "working", "done", "rejected", "strategies", "risk", "positions"] {
            assert!(body.contains(&format!("data-tab=\"{tab}\"")), "缺标签 {tab}");
            assert!(body.contains(&format!("id=\"panel-{tab}\"")), "缺面板 {tab}");
        }
        for id in ["hb-monitor", "hb-eval", "kill-banner", "acc-real", "acc-paper"] {
            assert!(body.contains(&format!("id=\"{id}\"")), "缺 {id}");
        }
        assert!(body.contains("function esc("));
        assert!(!body.contains("http://") && !body.contains("https://"), "不得引用外部资源");
    }

    #[tokio::test]
    async fn index_links_to_trade() {
        assert!(crate::web::page::INDEX_HTML.contains("href=\"/trade\""));
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib web::trade_page`
Expected: 编译失败

- [ ] **Step 3: 实现** 骨架、脚本、路由、入口。

- [ ] **Step 4: 通过 + JS 语法检查 + 全量门禁**

```bash
cargo test --lib web::trade_page
node scripts/check_inline_js.mjs src/web/trade_page.rs
```

Expected: 测试通过;脚本输出「检查了 1 个 <script> 块」且退出码 0

- [ ] **Step 5: Commit**

```bash
git add src/web scripts/check_inline_js.mjs
git commit -m "feat(web): /trade 交易页骨架、状态条与首页入口"
```

---

### Task 3: 工单标签:待确认、待成交、已完成、被拦截的信号

**Files:**
- Modify: `src/web/trade_page.rs`
- Test: `src/web/trade_page.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 1 的 `TicketView` 字段;4a 路由 `GET /api/trade/tickets?view=`、`POST .../confirm|ignore|fill`、`GET /api/trade/signals/rejected`
- Produces: `LOADERS.pending/working/done/rejected`

**待确认卡片**(每张工单一张 `.card.ticket`,`data-id` 为工单号)显示:

| 项 | 来源 / 规则 |
|---|---|
| 标题 | `code` + `sideLabel(side)`(买入红、卖出绿)+ 来源标签 `sourceLabel(source)`(止盈止损 / 日线策略 / 异动 / 手动)+ 重发时「第 N 次提醒」(`urgency > 0`) |
| 剩余有效期 | 由 `expires_at` 与当前时间算「剩 mm:ss」,每秒更新;到期后卡片置灰、按钮禁用 |
| 价格 | 建议价 `suggest_price`;现价 `quote.price`(无报价显示「—」);偏离 `fmtPct(deviation)`,超过 `deviation_th` 标红 |
| 数量与金额 | `qty` 股;预估金额 `est_amount`;预估费用 `est_fee` |
| 理由 | `reason`;有 `ai_note` 时折叠显示「AI 说明」 |
| 策略 | 有 `strategy_id` 时显示「查看策略成绩单」链接 → 切到策略标签并展开该策略 |
| 按钮 | 确认(见设计裁决 4)、忽略(弹 `prompt` 要理由,空则不提交) |

确认按钮状态机:

```js
// ticket: TicketView;btn: HTMLButtonElement
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
```

> 以上两段是必须照写的逻辑;DOM 拼装由执行者按表格完成(全部经 `esc()`)。

**待成交**:实盘 `confirmed / partial` 工单列表;每行显示代码、方向、工单数量、已成交 `filled_qty`、确认时间,和一个回填表单(成交价、数量,数量默认 `qty − filled_qty`)。提交 `POST /fill {price, qty}`;成功 toast「已回填」并重拉;409 `paper_ticket` / 400 显示服务端 `error` 文案。页首提示「当日未回填的工单将在次日 9:00 自动撤销」。

**已完成**:表格:时间、账户(实盘 / 模拟盘)、代码、方向、数量 / 已成交、状态 `statusLabel`、忽略理由。

**被拦截的信号**:表格:时间、来源、代码、方向、拦截原因(`reject_reason` 映射为中文:`trading_disabled` 交易已关闭、`duplicate_open_ticket` 已有未完结工单、`cooldown` 冷却中、`not_admitted` 策略未准入、`no_quote` 无行情、`limit_up` 涨停不买、`limit_down` 跌停不卖、`daily_ticket_cap` 超过每日工单上限、`daily_loss_halt` 当日亏损已达上限、`no_capital` 未设置资金、`below_one_lot` 不足一手、`nothing_sellable` 无可卖数量;未知值原样显示)、理由。

各列表为空时显示一句说明(如「没有待确认的工单」)。

- [ ] **Step 1: 写失败测试**(页面内容断言,沿用 Task 2 夹具)

```rust
    #[tokio::test]
    async fn ticket_tabs_have_confirm_flow_and_reject_reason_labels() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "function confirmState(", "function onConfirm(", "ack_deviation", "仍要确认", "行情延迟",
            "LOADERS.pending", "LOADERS.working", "LOADERS.done", "LOADERS.rejected",
            "/api/trade/tickets?view=pending", "/api/trade/signals/rejected", "次日 9:00 自动撤销",
            "nothing_sellable", "daily_loss_halt", "paper_ticket",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }
```

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/trade_page.rs` → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/web/trade_page.rs
git commit -m "feat(web): 交易页工单标签 —— 待确认(偏离二次确认、行情延迟禁用)、待成交回填、已完成、被拦截"
```

---

### Task 4: 策略标签:策略列表、新建与修改、提交评估、成绩单、评估任务

**Files:**
- Modify: `src/web/trade_page.rs`
- Test: 同上

**Interfaces:**
- Consumes: 4a 路由 `/api/trade/strategies*`、`/api/trade/jobs*`
- Produces: `LOADERS.strategies`,`openStrategy(id)`(供 Task 3 的「查看策略成绩单」调用)

内容:

- **策略列表**:名称、类型、股票池(前 5 个 + 「等 N 只」)、状态(`draft` 草稿 / `backtesting` 回测中 / `failed` 未通过 / `paper` 观察期 / `admitted` 已准入 / `suspended` 已暂停,配色区分)、状态原因 `status_reason`、操作:「提交评估」(仅草稿 / 未通过 / 已暂停可点;结果 `queued` → toast「已排队前推回测」,`paper` → toast「异动类策略直接进入观察期」,409 → toast「当前状态不能提交」)、「编辑」、「成绩单」。
- **新建 / 编辑表单** `#strategy-form`:名称、类型下拉(`trend` 均线择时、`rsi` RSI、`smart_dca` 智能定投、`dca` 定投、`adaptive` 自适应、`mover` 盘中异动)、参数网格(`<textarea>`,TOML,占位示例 `short_window = [5, 10]\nlong_window = [20, 60]\namount = [10000.0]`)、股票池(逗号或空白分隔的 6 位代码)。编辑保存结果 `reversioned` 时提示「定义已变更,策略回到草稿,需要重新提交评估」。400 显示服务端 `error`。
- **成绩单** `#scorecard`(设计裁决 6):四栏表格,行:年化收益、超额、夏普、最大回撤、胜率、盈亏比、交易笔数、平均每笔收益、已实现盈亏;底部「执行损耗(中位数,实盘相对模拟盘多付)」。空格显示「—」。上方显示最近状态事件(`/events`,时间、从 → 到、原因)。
- **评估任务** `#jobs`:最近任务表格:时间、策略、类型(`walk_forward` 前推回测 / `paper_check` 观察期检查 / `watchdog` 实盘监控)、状态(排队 / 运行中 / 完成 / 失败)、进度 `progress`、错误;排队或运行中的前推回测显示「取消」(`requested` → toast「已请求取消,将在处理下一只股票前停止」)。

成绩单取值(必须照写):

```js
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
```

> `PoolMetrics` 池级字段名以 `src/trade/admission/walk_forward.rs` 为准(若池级没有 `oos_annualized`,该格显示「—」,不要自行推算);执行者先读该结构确认 `oos_return`、`oos_sharpe`、`oos_max_drawdown`、`oos_trades`、`is_sharpe`、`buy_hold_return`、`trade_baseline.{win_rate, avg_return}` 的实际名称并对齐。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn strategy_tab_has_form_scorecard_and_jobs() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "id=\"strategy-form\"", "id=\"scorecard\"", "id=\"jobs\"", "function scorecardRows(",
            "function openStrategy(", "LOADERS.strategies", "/api/trade/strategies", "/api/trade/jobs",
            "样本内", "样本外", "模拟盘", "实盘", "执行损耗", "reversioned", "已请求取消",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }
```

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → JS 语法检查 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/web/trade_page.rs
git commit -m "feat(web): 交易页策略标签 —— 策略管理、四栏成绩单、评估任务与取消"
```

---

### Task 5: 风控设置与持仓校准标签

**Files:**
- Modify: `src/web/trade_page.rs`
- Test: 同上

**Interfaces:**
- Consumes: 4a 路由 `/api/trade/risk`、`/api/trade/capital`、`/api/trade/positions*`
- Produces: `LOADERS.risk`、`LOADERS.positions`

**风控设置** `#risk-form`:

| 字段 | 标签 | 输入 | 提交时换算 |
|---|---|---|---|
| `enabled` | 允许生成工单 | 复选框 | — |
| `max_order_amount` | 单笔金额上限(元) | 数字 | 原值 |
| `max_position_pct` | 单票仓位上限(%) | 数字 | ÷ 100 |
| `max_daily_tickets` | 每日工单上限 | 整数 | 原值 |
| `daily_loss_halt_pct` | 当日亏损停止买入(%) | 数字 | ÷ 100 |
| `cooldown_min` | 同码冷却(分钟) | 整数 | 原值 |
| `deviation_th` | 偏离提醒阈值(%) | 数字 | ÷ 100 |
| `default_stop_loss_pct` | 默认止损(%) | 数字 | ÷ 100 |
| `default_take_profit_pct` | 默认止盈(%) | 数字 | ÷ 100 |
| `slippage` | 模拟盘滑点(%) | 数字 | ÷ 100 |

加载时比例字段 × 100 显示;保存 `POST /api/trade/risk` 发完整对象(未在表中的字段按 GET 返回原样带回);400 显示服务端 `error`。下方「账户资金」:实盘总资金、模拟盘总资金两个输入与各自「保存」按钮,`POST /api/trade/capital {account, total}`。

**持仓校准**:

- 两张表(实盘 / 模拟盘):代码、数量、可卖 `sellable`、成本 `avg_cost`、现价(`quote.price`,`stale` 时灰色并标「延迟」)、市值、盈亏 `pnl_pct`、止损 / 止盈 / 移动止盈(`trailing_pct`,显示为 %)、操作「改止盈止损」(行内表单,`POST /positions/exit-levels`,留空即清除,移动止盈输入为 %,提交 ÷ 100)。
- 实盘表上方「校准持仓」表单 `#calibrate-form`:代码、数量、成本价、原因(必填),说明文字「以券商账户为准修正系统记录;数量填 0 表示已清仓」;提交 `POST /positions/calibrate`,成功后重拉持仓与记录。模拟盘不提供校准(设计裁决 5 of 4a)。
- 「校准记录」表:时间、代码、改前数量 / 成本、改后数量 / 成本、原因(`before` / `after` 为 `null` 时显示「无」)。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn risk_and_positions_tabs_have_forms() {
        let body = crate::web::trade_page::TRADE_HTML;
        for s in [
            "id=\"risk-form\"", "id=\"calibrate-form\"", "LOADERS.risk", "LOADERS.positions",
            "/api/trade/risk", "/api/trade/capital", "/api/trade/positions/calibrate",
            "/api/trade/positions/exit-levels", "/api/trade/positions/adjusts",
            "max_position_pct", "default_stop_loss_pct", "数量填 0 表示已清仓",
        ] {
            assert!(body.contains(s), "缺 {s}");
        }
    }
```

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → JS 语法检查 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/web/trade_page.rs
git commit -m "feat(web): 交易页风控设置、账户资金与持仓校准标签"
```

---

### Task 6: 签名链接落地页与管理后台总开关

**Files:**
- Modify: `src/web/trade_page.rs`、`src/web/mod.rs`、`src/web/auth/admin.rs`
- Test: `src/web/trade_page.rs`、`src/web/auth/admin.rs`(或其既有测试位置)

**Interfaces:**
- Produces:
  ```rust
  pub const SIGNED_TICKET_HTML: &str;
  pub async fn signed_ticket_page() -> Response; // 带 Referrer-Policy: no-referrer
  ```

落地页 `GET /trade/t/:id`(`public` 组,无需登录;页面本身不校验签名,由 JS 调 API,API 已统一 404):

- `<meta name="referrer" content="no-referrer">`;响应头 `Referrer-Policy: no-referrer`、`Cache-Control: no-store`。
- JS 从 `location.pathname` 取工单号、从 `location.search` 取 `sig`,调 `GET /api/trade/t/:id?sig=`;404 显示「链接无效或已过期」,不显示其它任何信息。
- 成功显示一张与待确认卡片相同要素的卡片(代码、方向、来源、剩余有效期倒计时、建议价 / 现价 / 偏离、数量、预估金额与费用、理由、AI 说明),确认按钮复用 Task 3 的 `confirmState` 规则,提交到 `POST /api/trade/t/:id/confirm?sig=`,偏离二次确认同样处理;成功显示「已确认。请在券商 App 下单,完成后登录交易页回填成交」与 `/trade` 链接。15 秒刷新一次行情(重新 GET)。
- 本页独立,不复用 `TRADE_HTML` 的脚本;`esc`、`confirmState` 在本页脚本里再写一份(两页互不依赖,允许这两个小函数重复;不得复制其它逻辑)。

管理后台(`ADMIN_HTML`)在「用户」之前加区块:

```html
<h2>交易总开关</h2>
<div id="ks">加载中…</div>
```

JS:`GET /api/admin/trade/kill-switch` 显示「当前:运行中 / 已暂停(由用户 #N 于 时间 操作)」与按钮「暂停全部交易」/「恢复交易」;暂停前 `confirm('暂停后所有用户都不会生成新工单、不能确认工单(回填成交不受影响)。确定?')`;`POST {on}` 后重拉。数据插入同样转义(该文件其它部分的历史写法不在本任务范围)。

- [ ] **Step 1: 写失败测试**

```rust
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
```

`admin.rs` 的页面断言(放在该文件已有测试模块,或新建):`ADMIN_HTML` 包含 `交易总开关`、`/api/admin/trade/kill-switch`、`暂停全部交易`。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/trade_page.rs src/web/auth/admin.rs` → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/web
git commit -m "feat(web): 工单签名链接落地页(禁止 Referer)与管理后台交易总开关"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] `node scripts/check_inline_js.mjs src/web/trade_page.rs src/web/auth/admin.rs` 通过
- [ ] CI 门禁符合 Global Constraints
- [ ] 手动冒烟(执行者在报告中说明是否做了、结果如何;做不了就写明做不了):`cargo run -- serve` 后登录,打开 `/trade` 七个标签各点一次,浏览器控制台无报错

## 后续

- 手动 / AI「生成工单」入口(spec §5 manual 来源)在个股页与 AI 分析结果上加按钮,调用一个新的「手动信号」API —— 独立小计划
- 持仓迁移(spec §4:从 `holdings.rs` 导入):现有 `holdings.rs` 是请求期输入,不落库,无可导入数据;以持仓校准代替,待确认后在 spec 中改写该条

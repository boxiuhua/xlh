# 量化交易 · 计划 5:一期收尾(日报、全链路集成测试、文档、遗留小项) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成 spec §14 任务 10 余下部分——交易日报推送、spec §13 的全链路集成测试 `tests/trade_pipeline.rs`、README / DEPLOY 的交易模块文档——并清掉计划 4c 留下的小项。

**Architecture:** 日报是纯函数「汇总 → 渲染」(`src/trade/report.rs`),由 `trade-monitor` 守护在收盘后的窗口内每个交易日为每个用户推送一次,「今天已发」标记落库(复用心跳表,同 `trade-eval` 的做法),重启不重发。集成测试只用公开 API、内存库与报价桩,把一张信号从生成一路走到止损卖出工单。

**Tech Stack:** Rust 2021、rusqlite 0.31、chrono。无新依赖。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §11、§13、§14(任务 10)。前置:计划 1–4c 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 开关与时刻一律来自配置并带范围校验
- 测试不得访问网络、不起真线程,不依赖真实时钟
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:163-164`、`src/ai.rs:169-170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 页面 JS 改动须通过 `node scripts/check_inline_js.mjs <文件>`
- 不使用 `git stash`

### 设计裁决(执行者照此实现)

1. **日报只在交易日发**(`calendar::is_trading_day`),默认 15:35–18:00 窗口内发一次;每个用户**没有任何当日活动且没有持仓**时不发(不刷屏)。
2. **日报只统计、不建议**:不出现任何「建议买入 / 卖出」措辞,末尾固定一句「以上为系统记录的统计,不构成投资建议」(spec §15 合规)。
3. **「今天已发」标记持久化**:心跳表写 `trade-daily-report`,守护启动时读回;推送失败的用户只记日志,不重试(与其它推送一致),标记按「本轮已执行」写入。
4. **手动工单截止到 15:00 之前**(4c 遗留):手动创建另加 `now.time() < 15:00:00`;休市日(交易日历证实休市)返回与非交易时段同一提示。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/actions.rs`、`src/web/trade.rs` | 修改 | 手动工单截止 15:00、休市提示 |
| `src/web/trade.rs` | 修改 | `TicketView.source` 去掉 `Option` |
| `src/trade/gate.rs` | 修改 | `GateReject::ALL` 穷举性测试 |
| `src/web/trade_page.rs` | 修改 | 名称片段两页共用并纳入漂移测试 |
| `src/trade/report.rs` | 新建 | 日报汇总与渲染 |
| `src/trade/config.rs`、`config.toml` | 修改 | `[trade] daily_report`、时刻 |
| `src/trade/daemon.rs` | 修改 | 收盘后推送日报 |
| `src/trade/mod.rs` | 修改 | 模块声明 |
| `tests/trade_pipeline.rs` | 新建 | spec §13 全链路集成测试 |
| `README.md`、`DEPLOY.md` | 修改 | 交易模块说明 |

---

### Task 1: 4c 遗留小项

**Files:**
- Modify: `src/trade/actions.rs`、`src/web/trade.rs`、`src/trade/gate.rs`、`src/web/trade_page.rs`
- Test: 各文件 `mod tests`

内容:

1. **手动工单截止**:新增 `pub fn manual_session(conn: &Connection, now: NaiveDateTime) -> Result<bool>`(放 `actions.rs`):`monitor::is_session(now) && now.time() < 15:00:00 && calendar::day_status(conn, now.date())? != Some(false)`。`submit_manual` 与 Web handler 的时段判断都改用它(handler 在锁内读日历,锁外才抓行情,顺序同现状)。测试:15:00:00 与 15:00:59 → 非交易时段;14:59:59 且报价新鲜 → 可生成;日历标记当日休市 → 非交易时段(web 与 actions 各一例,时钟用注入的固定时刻)。
2. **`TicketView.source`** 由 `Option<SignalSource>` 改为 `SignalSource`(`signal_meta` 缺失已是 500,`Option` 已无意义);JSON 字段不变(仍是小写字符串)。
3. **`GateReject::ALL` 穷举性**:在 `gate.rs` 测试里加

```rust
    #[test]
    fn all_lists_every_variant_once() {
        // 新增变体时这里的 match 会编译失败,提醒同步 ALL
        fn index(r: GateReject) -> usize {
            match r {
                GateReject::TradingDisabled => 0,
                GateReject::DuplicateOpenTicket => 1,
                GateReject::Cooldown => 2,
                GateReject::NotAdmitted => 3,
                GateReject::NoQuote => 4,
                GateReject::LimitUp => 5,
                GateReject::LimitDown => 6,
                GateReject::DailyTicketCap => 7,
                GateReject::DailyLossHalt => 8,
                GateReject::NoCapital => 9,
                GateReject::BelowOneLot => 10,
                GateReject::NothingSellable => 11,
            }
        }
        let mut seen = [false; 12];
        for r in GateReject::ALL {
            let i = index(r);
            assert!(!seen[i], "{r:?} 重复");
            seen[i] = true;
        }
        assert!(seen.iter().all(|s| *s), "ALL 缺变体");
    }
```

   > 变体名与数量以 `gate.rs` 现状为准;若与上面不同,照现状写。
4. **名称片段共用**:签名页里内联的名称拼接改为调用与交易页同名的 `nameSuffixHtml`(两页各一份,内容一致),并把 `nameSuffixHtml` 加入 `shared_helpers_are_identical_in_both_pages` 的列表。

- [ ] **Step 1–4: 写失败测试 → 确认失败 → 实现 → 通过 → `node scripts/check_inline_js.mjs src/web/trade_page.rs` → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src
git commit -m "fix(trade): 手动工单截止 15:00 并识别休市日,工单来源非空,拒绝原因列表穷举校验,名称片段两页共用"
```

---

### Task 2: 交易日报 `src/trade/report.rs`

**Files:**
- Create: `src/trade/report.rs`
- Modify: `src/trade/mod.rs`
- Test: `src/trade/report.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Default, PartialEq)]
  pub struct TicketCounts { pub created: usize, pub confirmed: usize, pub filled: usize, pub expired: usize, pub ignored: usize, pub cancelled: usize, pub open: usize }
  #[derive(Debug, Clone, PartialEq)]
  pub struct FillLine { pub account: Account, pub code: String, pub name: Option<String>, pub side: Direction, pub qty: u64, pub price: f64, pub fee: f64, pub realized_pnl: Option<f64> }
  #[derive(Debug, Clone, Default, PartialEq)]
  pub struct HoldingSummary { pub positions: usize, pub market_value: f64, pub cost: f64, pub unpriced: usize }
  #[derive(Debug, Clone, PartialEq)]
  pub struct StrategyChange { pub strategy_id: i64, pub name: String, pub from: StrategyStatus, pub to: StrategyStatus, pub reason: String }
  #[derive(Debug, Clone, PartialEq)]
  pub struct DailyReport {
      pub user_id: i64,
      pub date: NaiveDate,
      pub real_tickets: TicketCounts,          // 当日创建的实盘工单按「当前状态」计数
      pub fills: Vec<FillLine>,                // 当日成交(实盘在前,各自按时间)
      pub realized_real: f64,
      pub realized_paper: f64,
      pub rejected: Vec<(String, usize)>,      // 当日被拦截信号:原因中文(GateReject::label_zh,未知原样) × 次数,按次数降序
      pub real_holdings: HoldingSummary,
      pub paper_holdings: HoldingSummary,
      pub strategy_changes: Vec<StrategyChange>,
  }
  pub fn build_daily_report(conn: &Connection, user_id: i64, date: NaiveDate) -> Result<Option<DailyReport>>;
  pub fn render_daily_report(r: &DailyReport) -> (String, String);
  ```

规则:

- 「当日」= `created_at` / `filled_at` / 事件 `at` 的日期等于 `date`。
- `TicketCounts`:只统计**实盘**当日创建的工单;`confirmed` = 已确认未成交(confirmed + partial),`filled`、`expired`、`ignored`(= rejected)、`cancelled`、`open` = pending。
- `HoldingSummary`:当前持仓(非当日快照);`market_value` 用 `trade_quotes` 最新价(无报价的持仓计入 `unpriced`,市值按成本计);`cost` = Σ qty × avg_cost。
- 返回 `None` 的条件:当日无工单、无成交、无被拦截信号、无策略状态变化,且实盘与模拟盘都无持仓。
- 渲染:标题 `交易日报 {date}`;Markdown 分节「今日工单」「今日成交」「已实现盈亏」「被拦截的信号」「持仓」「策略状态变化」,没有内容的节省略;金额 2 位小数,盈亏带正负号;成交行 `- 实盘 买入 600000 浦发银行 1000 股 @ 10.00(费 5.00)`;末尾固定一行 `> 以上为系统记录的统计,不构成投资建议。`。标题与正文里不得出现「建议买入」「建议卖出」。

- [ ] **Step 1: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // 夹具:内存库 + migrate + set_capital;用 service::submit_signal 造工单,
    // ticket::confirm / ticket::record_fill 造成交,router 或直接 record_fill 造模拟盘成交,
    // store::upsert_quotes 造报价,state::update_status 造策略状态变化。

    #[test]
    fn quiet_user_without_positions_gets_no_report() {
        // 空库用户 → build_daily_report == None
    }

    #[test]
    fn report_counts_tickets_fills_pnl_rejections_holdings_and_strategy_changes() {
        // 当日:3 张实盘工单(1 张确认并回填买入、1 张忽略、1 张仍 pending);
        //       1 笔实盘卖出成交(已实现盈亏 +200);1 个被拦截信号(cooldown);
        //       1 个策略 Paper → Admitted;持仓 1 只有报价、1 只无报价
        // 前一日:1 张工单与 1 笔成交(不应计入)
        // 断言各字段精确值;rejected == [("冷却中"(以 label_zh 为准), 1)];
        //   real_holdings.unpriced == 1
    }

    #[test]
    fn rendering_is_factual_and_omits_empty_sections() {
        // 只有持仓、无当日活动 → 渲染只含「持仓」节与免责声明;
        // 不含「建议买入」「建议卖出」;标题为「交易日报 2026-09-23」;盈亏带 +/- 号
    }

    #[test]
    fn report_is_scoped_to_the_user() {
        // 用户 2 的活动不出现在用户 1 的日报里
    }
}
```

> 按注释写出完整用例(日期用 2026-09-23 等固定值)。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/trade
git commit -m "feat(trade): 交易日报汇总与渲染(只统计、不建议)"
```

---

### Task 3: 日报调度与配置

**Files:**
- Modify: `src/trade/config.rs`、`config.toml`、`src/trade/daemon.rs`
- Test: `config.rs`、`daemon.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `report::{build_daily_report, render_daily_report}`;`calendar::is_trading_day`;`store::{beat, last_beat, users_with_real_account, users_with_strategies, users_with_positions}`;`notify::Notifier`
- Produces:
  ```rust
  // TradeCfg 新增
  pub daily_report: bool,               // 默认 true
  pub daily_report_hour: u32,           // 默认 15
  pub daily_report_minute: u32,         // 默认 35
  // daemon.rs
  pub fn send_daily_reports(conn: &Connection, notifier: &dyn Notifier, date: NaiveDate) -> Result<usize>; // 返回推送用户数
  ```

规则:

- 配置校验:`daily_report_hour` ∈ [15, 17](必须收盘后、窗口结束 18:00 前),`daily_report_minute` < 60;报错前缀 `[trade]`。`config.toml` 的 `[trade]` 样例补三行注释。
- `send_daily_reports`:用户集合 = 有实盘账户 ∪ 有策略 ∪ 有持仓(去重、升序);逐用户 `build_daily_report`,`Some` 则渲染并 `notifier.notify`;单个用户失败只记日志并继续。
- 守护接线(`run_loop`):循环外 `let mut last_report = store::last_beat(&conn, "trade-daily-report").ok().flatten().map(|t| t.date());`(读失败记日志按 `None`);循环内在回填提醒之后:

```rust
            if cfg.daily_report
                && due_daily_window(now, (cfg.daily_report_hour, cfg.daily_report_minute), (18, 0), last_report)
            {
                match crate::trade::calendar::is_trading_day(&conn, now.date()) {
                    Ok(false) => last_report = Some(now.date()), // 休市日不发,当日不再判断
                    Ok(true) => match send_daily_reports(&conn, notifier.as_ref(), now.date()) {
                        Ok(n) => {
                            println!("[trade] 已推送交易日报 {n} 份");
                            last_report = Some(now.date());
                            if let Err(e) = store::beat(&conn, "trade-daily-report", now) {
                                eprintln!("[trade] 日报标记写入失败: {e:#}");
                            }
                        }
                        Err(e) => eprintln!("[trade] 交易日报失败: {e:#}"),
                    },
                    Err(e) => eprintln!("[trade] 交易日历读取失败: {e:#}"),
                }
            }
```

  > 放在止损 `run_tick` 之后(计划 3e 终审约定:任何非止损工作排在止损之后)。

- [ ] **Step 1: 写失败测试**

```rust
// config.rs
    #[test]
    fn daily_report_defaults_and_validation() {
        let c = from_toml_str("").unwrap();
        assert!(c.daily_report);
        assert_eq!((c.daily_report_hour, c.daily_report_minute), (15, 35));
        assert!(from_toml_str("[trade]\ndaily_report_hour = 14").is_err());
        assert!(from_toml_str("[trade]\ndaily_report_hour = 18").is_err());
        assert!(from_toml_str("[trade]\ndaily_report_minute = 60").is_err());
        assert!(!from_toml_str("[trade]\ndaily_report = false").unwrap().daily_report);
    }
// daemon.rs
    #[test]
    fn daily_reports_go_to_active_users_only_and_survive_one_failure() {
        // 用户 1 有当日成交,用户 2 有持仓无活动,用户 3 只有空账户;
        // 用 Recorder 形式的 Notifier(本文件或 tests/trade_runtime.rs 里已有同类桩,照写一个),
        // 让用户 1 的发送返回 Err → send_daily_reports 返回 1(用户 2 成功)且不 panic;
        // 用户 3 未被推送
    }
```

> 按注释写出完整用例。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁**

- [ ] **Step 5: Commit**

```bash
git add src/trade config.toml
git commit -m "feat(trade): 收盘后按交易日推送交易日报,重启不重发"
```

---

### Task 4: 全链路集成测试 `tests/trade_pipeline.rs`

**Files:**
- Create: `tests/trade_pipeline.rs`

spec §13:「信号 → 闸门 → 工单 → 确认 → 回填 → 持仓 → 止损触发 → 卖出工单」。只用 `xlh::` 公开 API、内存库、`QuoteSource` 桩,时间全部固定。

用例 `signal_to_stop_loss_sell_ticket`(按以下步骤写全,每步都有断言):

1. 内存库 `migrate`;`set_capital(1, Real, 100_000)`;`save_risk_rules` 默认规则
2. 2026-09-23(周三)10:00 以手动信号(`actions::submit_manual`,报价 10.00、今日时间戳)买入 600000 → `Ticketed { real_ticket: Some(id) }`;模拟盘工单同时成交(断言模拟盘持仓存在)
3. `actions::confirm_ticket(.., ack_deviation=false, ack_max=None, now=10:00:30)` → `Ok(())`(行情新鲜、无偏离)
4. `actions::manual_fill(.., price=10.00, qty=工单数量, now=10:05)` → 成交;实盘持仓数量 = 工单数量、`avg_cost` ≈ 10.00 + 费用摊薄、默认止损价 = 10.00 × (1 − 0.08) 按价位取整(9.20)
5. 次日 2026-09-24 10:00 起跑 `monitor::run_tick`,报价桩依次给 9.50(不触发)、9.10(触发止损)
6. 断言:第二次 `run_tick` 的报告 `exit_signals == 1` 且 `new_real_tickets.len() == 1`;该工单方向为卖出、数量 = 可卖数量(T+1 已过)、来源为 `exit`
7. 同一日再跑一次 9.05 → 不产生新信号(当日同一规则只触发一次)

> 函数签名以当前代码为准(`confirm_ticket` 在 4b 增加了 `ack_max_deviation` 参数;`manual_fill`、`submit_manual`、`run_tick` 的参数以源码为准)。若 `submit_manual` 的交易时段判断依赖日历表,内存库无休市记录即视为开市。

- [ ] **Step 1: 写测试并运行**

Run: `cargo test --test trade_pipeline`
Expected: PASS(这是对既有行为的集成测试;若失败,先定位是测试写错还是真实缺陷——真实缺陷在报告中写明并修复)

- [ ] **Step 2: 全量门禁 + Commit**

```bash
git add tests/trade_pipeline.rs
git commit -m "test(trade): 信号到止损卖出工单的全链路集成测试"
```

---

### Task 5: README 与 DEPLOY 文档

**Files:**
- Modify: `README.md`、`DEPLOY.md`

README 新增一节「量化交易(一期)」,放在「数据存储」之前,内容(照实写,不夸大):

- 定位:工单闭环 + 模拟盘 + 策略准入;**不接券商接口,所有实盘操作由用户在券商 App 手动完成后回填**;不构成投资建议
- 运行方式:交易监听线程 `trade-monitor` 与评估线程 `trade-eval` 随 `xlh push` 守护启动(与实时异动同进程);`xlh serve` 提供 `/trade` 页面与 API
- 配置:`config.toml` 的 `[trade]`、`[trade.admission]`、`[trade.walk_forward]`、`[trade.eval]`、`[trade.signals]` 各段用途一句话 + 常用项(`enabled`、`link_base_url`、`daily_report`…),指向 `config.toml` 样例
- 页面:`/trade` 七个标签各一句;首页「生成工单」入口;签名链接的作用与有效期
- 四类信号源与准入流程(草稿 → 回测中 → 观察期 → 已准入 / 暂停)一段话
- 管理员总开关位置与效果
- 常见问题:工单确认不了(行情延迟 / 偏离 / 总开关)、日线信号没发出(不在观察期或已准入、缺实盘参数、K 线未更新)、签名链接打不开(`link_base_url` 未配置或工单已过期)

`Web 接口` 表格补 `/api/trade/*`(按功能分组列出,不必逐个)与 `/trade`、`/trade/t/:id`。

DEPLOY 新增一节:生产启用交易模块的检查清单——`xlh push` 必须常驻(否则止损监听、日线信号、日报都不运行)、`link_base_url` 设为对外 HTTPS 地址、反向代理保留 `Referrer-Policy` 头、`data/xlh.db` 备份含交易表、时区须为 Asia/Shanghai(守护按本地时间判断交易时段)。

- [ ] **Step 1: 写文档**(先读现有 README / DEPLOY 的结构与语气,照同样风格;命令与配置键名以代码为准,逐一核对 `src/trade/config.rs`)

- [ ] **Step 2: Commit**

```bash
git add README.md DEPLOY.md
git commit -m "docs: 量化交易一期使用与部署说明"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] `tests/trade_pipeline.rs` 通过
- [ ] CI 门禁符合 Global Constraints

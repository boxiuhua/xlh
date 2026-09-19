# 量化交易 · 计划 2b/4:运行时与信号(监听线程、止盈止损、异动信号、推送)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让计划 2a 的交易核心库真正运转:推送守护进程内新增 `trade-monitor` 线程,交易时段每 15 秒拉取「持仓 ∪ 未完结工单」报价并缓存,生成止盈 / 止损 / 移动止盈信号、过期重发、模拟盘撮合;实时异动转为(观察期)交易信号;新工单、待回填、监听中断推送给用户。

**Architecture:** 纯函数(`exits`、`Backoff`、`due_daily`、渲染)与可注入 IO 的编排(`monitor::run_tick(conn, &dyn QuoteSource, now)`、`Notifier` trait)分离,全部可离线测试;`daemon` 只负责线程、休眠、退避与调度。异动由推送主循环经 `mpsc` 交给监听线程,监听线程独占自己的 SQLite 连接(WAL + IMMEDIATE 事务)。

**Tech Stack:** Rust 2021、std::thread + std::sync::mpsc(不引入 tokio)、rusqlite 0.31、chrono、serde/toml、既有 `stock::realtime::snapshot`(腾讯快照)与 `push::channels`。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md`(§5 信号源、§11 运行时、§12 异常处理)。前置计划:`docs/superpowers/plans/2026-09-16-quant-trading-p2a-trade-core.md`(已合并入 main),本计划吸收其「后续计划 · 计划 2b」所列遗留项。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有交易查询按 `user_id` 隔离;监听线程的全局批处理(报价、全体持仓、过期)不得跨用户写错数据
- 时间:`NaiveDateTime` 本地时间,存储 `%Y-%m-%d %H:%M:%S`
- 推送守护进程不引入 tokio;新线程 `trade-monitor` 使用 `std::thread`,**任何错误只记日志,绝不 panic 退出,也不得拖垮既有推送循环**
- 交易时段:工作日 09:30–11:30、13:00–15:00(含端点);报价时间戳日期 ≠ 今日视为陈旧,整轮跳过
- 监听间隔默认 15 秒;失败退避 15 → 30 → 60 秒;连续失败 ≥ 180 秒推送「止损监听中断」给有持仓的用户(每次中断只推一次)
- 调度:每个工作日 09:00 撤销前一日及更早未回填的实盘工单;15:05 推送待回填提醒(每日一次)
- 止盈止损工单有效期 30 分钟;过期时仍触发 → 同一信号新建实盘工单,`urgency + 1`;用户主动忽略(rejected)的不重发
- 实时异动信号:仅对设置了实盘资金、且 `realtime_watch_stocks` 非空的用户,只处理名单内代码;`TradeAction::Buy` → 买入信号,`TradeAction::Sell` → 仅在该用户持有该代码时生成卖出信号,`Hold` 忽略;准入一律 `Admission::Probation`(仅模拟盘)直到计划 3
- 测试不得访问网络(腾讯报价经 `QuoteSource` trait 注入桩)
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 相对 spec 的实现细化(执行者照此实现)

1. **日线策略信号**(spec §5 strategy)推迟到计划 3:需要 `trade_strategies` 表与准入状态。
2. **手动 / AI 信号**入口在计划 4(网页);库函数 `service::submit_signal` 已可用。
3. **现有持仓导入**推迟到计划 4 的「持仓校准」页:`holdings.rs::Holding` 只有金额与收益,没有股数与成本,无法自动换算。
4. **异动信号未准入前只进模拟盘**(`Admission::Probation`),与 spec §10.2「mover 类策略直接进入观察期」一致。
5. **报价缓存**只存监听线程拉到的「持仓 ∪ 未完结工单」代码;网页端展示(计划 4)读 `trade_quotes`。
6. **心跳**写 `trade_heartbeat(name='trade-monitor')`,网页展示在计划 4。
7. **挂单占用资金**:未完结买入工单按 `建议价 × 未成交数量` 预留,闸门从可用资金中扣除(实盘与模拟盘分别计算)。
8. **北交所**代码(`8`/`4`/`92` 开头)暂不拉报价(腾讯前缀映射仅沪深),其持仓不会触发止盈止损;在报告中列为已知限制。
9. **快照有效性**:腾讯返回非 2xx 或内容不含快照行视为抓取失败(触发退避与中断告警),全部停牌的有效响应仍视为无报价。
10. **过期重发**遵守风控总开关与跌停:关闭交易或处于跌停时不重发。
11. **异动信号**先为异动代码拉取实时报价(含涨跌停价);无今日报价的异动不生成信号。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/ticket.rs` | 修改 | `NewTicket.urgency`;`expire_due` 覆盖模拟盘;`reserved_cash`;`reissue_expired_exit`;`list_unfilled_real`;默认止损价按最小价位取整 |
| `src/trade/gate.rs` | 修改 | `GateInput` 增加实盘 / 模拟盘预留资金 |
| `src/trade/service.rs` | 修改 | 计算预留资金;`NewTicket.urgency` |
| `src/trade/store.rs` | 修改 | `trade_quotes`、`trade_heartbeat` 表;报价与心跳读写;全体持仓、移动止盈最高价、用户列表 |
| `src/trade/router.rs` | 修改 | `fill_pending_paper` 单工单错误隔离、滑点缓存,返回 `PaperBatch` |
| `src/trade/quotes.rs` | 新建 | `QuoteSource` trait、腾讯实现、代码 → 腾讯符号 |
| `src/trade/exits.rs` | 新建 | 止盈 / 止损 / 移动止盈判定与信号(纯函数) |
| `src/trade/monitor.rs` | 新建 | 单轮监听编排 `run_tick` |
| `src/trade/movers.rs` | 新建 | 实时异动 → 交易信号 |
| `src/trade/config.rs` | 新建 | `[trade]` 配置段 |
| `src/trade/notify.rs` | 新建 | 推送渲染、`Notifier` trait、`PushNotifier` |
| `src/trade/daemon.rs` | 新建 | 退避、每日调度、通知分发、监听线程与 `MoverSink` |
| `src/trade/mod.rs` | 修改 | 模块声明 |
| `src/push/schedule.rs`、`src/push/mod.rs`、`src/main.rs` | 修改 | `user_allowed` 可见性;把异动交给监听线程;启动线程 |
| `tests/trade_runtime.rs` | 新建 | 通知分发集成测试 |

---

### Task 1: 工单紧急度、模拟盘过期、挂单占用资金、过期重发

**Files:**
- Modify: `src/trade/ticket.rs`、`src/trade/gate.rs`、`src/trade/service.rs`、`src/trade/router.rs`(仅测试辅助中的 `NewTicket` 字面量)
- Test: `src/trade/ticket.rs`、`src/trade/gate.rs` 内 `mod tests`

**Interfaces:**
- Consumes: 计划 2a 全部 `trade::*` 接口;`crate::stock::ashare::price_decimals`
- Produces:

```rust
// ticket.rs
pub struct NewTicket { /* 原字段 */, pub urgency: i64 }   // 位于 status 之后
pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize>;             // 实盘 pending + 模拟盘 confirmed/partial
pub fn reserved_cash(conn: &Connection, user_id: i64, account: Account) -> Result<f64>; // Σ suggest_price × (qty − filled_qty),未完结买入工单
pub fn list_unfilled_real(conn: &Connection) -> Result<Vec<Ticket>>;                   // 全体用户实盘 confirmed/partial
pub fn reissue_expired_exit(conn: &Connection, user_id: i64, dedup_key: &str, qty: u64, price: f64, now: NaiveDateTime) -> Result<Option<i64>>;
// gate.rs
pub struct GateInput<'a> { /* 原字段 */, pub real_reserved_cash: f64, pub paper_reserved_cash: f64 } // 位于 paper_position 之后
```

- [ ] **Step 1: 写失败测试**

`src/trade/ticket.rs` 的 `mod tests` 末尾追加:

```rust
    fn new_ticket(
        signal_id: i64,
        account: Account,
        code: &str,
        side: Direction,
        qty: u64,
        status: TicketStatus,
        now: NaiveDateTime,
    ) -> NewTicket {
        NewTicket {
            user_id: 1,
            signal_id,
            account,
            code: code.into(),
            side,
            suggest_price: 10.0,
            qty,
            expires_at: now + chrono::Duration::minutes(30),
            deviation_th: 0.015,
            status,
            urgency: 0,
            created_at: now,
        }
    }

    #[test]
    fn expire_due_covers_open_paper_tickets_but_not_confirmed_real() {
        let c = db();
        let sid = insert_signal(&c, &signal("exp-paper", Direction::Buy), at(15, 10, 0))
            .unwrap()
            .unwrap();
        let paper = create_ticket(
            &c,
            &new_ticket(sid, Account::Paper, "600000", Direction::Buy, 100, TicketStatus::Confirmed, at(15, 10, 0)),
        )
        .unwrap();
        let real = ticket(&c, Direction::Buy, 100, TicketStatus::Confirmed, at(15, 10, 1));
        assert_eq!(expire_due(&c, at(15, 10, 40)).unwrap(), 1);
        assert_eq!(get_ticket(&c, paper).unwrap().unwrap().status, TicketStatus::Expired);
        assert_eq!(get_ticket(&c, real).unwrap().unwrap().status, TicketStatus::Confirmed);
        let unfilled = list_unfilled_real(&c).unwrap();
        assert_eq!(unfilled.iter().map(|t| t.id).collect::<Vec<_>>(), vec![real]);
    }

    #[test]
    fn reserved_cash_counts_unfilled_part_of_open_buy_tickets() {
        let mut c = db();
        let b = ticket(&c, Direction::Buy, 1000, TicketStatus::Pending, at(15, 10, 0));
        ticket(&c, Direction::Sell, 500, TicketStatus::Pending, at(15, 10, 1));
        assert!((reserved_cash(&c, 1, Account::Real).unwrap() - 10_000.0).abs() < 1e-9);
        assert_eq!(confirm(&c, 1, b, at(15, 10, 2)).unwrap(), Transition::Applied);
        record_fill(&mut c, 1, b, 10.0, 400, "manual", at(15, 10, 3)).unwrap();
        assert!((reserved_cash(&c, 1, Account::Real).unwrap() - 6_000.0).abs() < 1e-9);
        assert!(reserved_cash(&c, 1, Account::Paper).unwrap().abs() < 1e-12);
        assert!(reserved_cash(&c, 2, Account::Real).unwrap().abs() < 1e-12);
    }

    #[test]
    fn reissue_expired_exit_bumps_urgency_once_per_expiry() {
        let c = db();
        let key = "exit-real-600000-stop-2026-09-15";
        let mut s = signal(key, Direction::Sell);
        s.source = SignalSource::Exit;
        let sid = insert_signal(&c, &s, at(15, 10, 0)).unwrap().unwrap();
        mark_signal(&c, sid, "ticketed", None).unwrap();
        let first = create_ticket(
            &c,
            &new_ticket(sid, Account::Real, "600000", Direction::Sell, 1000, TicketStatus::Pending, at(15, 10, 0)),
        )
        .unwrap();
        assert!(reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 20)).unwrap().is_none(), "未过期不重发");
        assert_eq!(expire_due(&c, at(15, 10, 30)).unwrap(), 1);
        let second = reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 31)).unwrap().unwrap();
        assert_ne!(second, first);
        let t = get_ticket(&c, second).unwrap().unwrap();
        assert_eq!((t.signal_id, t.urgency, t.qty, t.status), (sid, 1, 1000, TicketStatus::Pending));
        assert_eq!(t.expires_at, at(15, 11, 1));
        assert!((t.suggest_price - 9.1).abs() < 1e-9);
        assert!(reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 10, 32)).unwrap().is_none(), "已有待确认工单");
        assert!(reissue_expired_exit(&c, 2, key, 1000, 9.1, at(15, 11, 40)).unwrap().is_none(), "他人信号");

        assert_eq!(ignore(&c, 1, second, "不卖").unwrap(), Transition::Applied);
        assert!(reissue_expired_exit(&c, 1, key, 1000, 9.1, at(15, 11, 40)).unwrap().is_none(), "用户忽略不重发");
    }

    #[test]
    fn default_exit_levels_round_to_price_tick() {
        let mut c = db();
        let sid = insert_signal(&c, &signal("etf-buy", Direction::Buy), at(15, 10, 0))
            .unwrap()
            .unwrap();
        let mut nt = new_ticket(sid, Account::Real, "510300", Direction::Buy, 1000, TicketStatus::Confirmed, at(15, 10, 0));
        nt.suggest_price = 3.456;
        let id = create_ticket(&c, &nt).unwrap();
        record_fill(&mut c, 1, id, 3.456, 1000, "manual", at(15, 10, 1)).unwrap();
        let p = store::get_position(&c, 1, Account::Real, "510300").unwrap().unwrap();
        // 3.456 × 0.92 = 3.17952 → 3.180;3.456 × 1.2 = 4.1472 → 4.147
        assert_eq!((p.stop_loss, p.take_profit), (Some(3.18), Some(4.147)));
    }
```

`src/trade/gate.rs` 的测试夹具 `struct Fx` 增加字段 `real_reserved: f64` 与 `paper_reserved: f64`(`Fx::buy` 中都初始化为 `0.0`),`Fx::run` 的 `GateInput` 中传 `real_reserved_cash: self.real_reserved, paper_reserved_cash: self.paper_reserved`,并追加测试:

```rust
    #[test]
    fn open_buy_tickets_reserve_cash_per_account() {
        // 实盘预留 90000 → 预算 min(50000, 50000, 10000, 20000) = 10000 → 执行价 10.01 → 900 股
        let mut f = Fx::buy(SignalSource::Manual);
        f.real_reserved = 90_000.0;
        assert_eq!(
            plans(f.run()),
            vec![plan(Account::Real, 900), plan(Account::Paper, 1900)]
        );
    }
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::ticket::tests trade::gate::tests`
Expected: 编译失败(`urgency` 字段、`reserved_cash`、`reissue_expired_exit`、`list_unfilled_real`、`real_reserved_cash` 未定义)

- [ ] **Step 3: 实现 ticket.rs**

1. `NewTicket` 在 `pub status: TicketStatus,` 之后加 `pub urgency: i64,`。
2. `create_ticket` 的 SQL 值列表改为 `VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9, ?10, ?11, ?12, ?13)`,`params!` 在 `t.status.as_str(),` 之后插入 `t.urgency,`(其后依次为 `fmt_ts(t.created_at)` 与 confirmed_at)。
3. 把 `expire_due` 替换为:

```rust
/// 过期:实盘待确认、模拟盘待成交(模拟盘同样受有效期约束)。
pub fn expire_due(conn: &Connection, now: NaiveDateTime) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE trade_tickets SET status = 'expired'
         WHERE expires_at <= ?1
           AND ((account = 'real' AND status = 'pending')
             OR (account = 'paper' AND status IN ('confirmed', 'partial')))",
        [fmt_ts(now)],
    )?)
}
```

4. 在 `realized_pnl_on` 之后加入:

```rust
/// 未完结买入工单占用的资金:Σ 建议价 × 未成交数量。
pub fn reserved_cash(conn: &Connection, user_id: i64, account: Account) -> Result<f64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(suggest_price * (qty - filled_qty)), 0.0) FROM trade_tickets
         WHERE user_id = ?1 AND account = ?2 AND side = 'buy'
           AND status IN ('pending', 'confirmed', 'partial')",
        params![user_id, account.as_str()],
        |r| r.get(0),
    )?)
}
```

5. 在 `list_open_paper` 之后加入:

```rust
/// 全体用户已确认但未完全回填的实盘工单(15:05 提醒用)。
pub fn list_unfilled_real(conn: &Connection) -> Result<Vec<Ticket>> {
    query_tickets(
        conn,
        "account = 'real' AND status IN ('confirmed', 'partial')",
        params![],
    )
}

/// 止盈止损工单过期而条件仍成立:基于同一信号新建实盘工单,urgency + 1。
/// 仅当该信号最近一张实盘工单为 expired 时重发(用户忽略、已成交、仍待确认均不重发)。
pub fn reissue_expired_exit(
    conn: &Connection,
    user_id: i64,
    dedup_key: &str,
    qty: u64,
    price: f64,
    now: NaiveDateTime,
) -> Result<Option<i64>> {
    if qty == 0 {
        return Ok(None);
    }
    let signal_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM trade_signals
             WHERE user_id = ?1 AND dedup_key = ?2 AND source = 'exit' AND status = 'ticketed'",
            params![user_id, dedup_key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(signal_id) = signal_id else {
        return Ok(None);
    };
    let last_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM trade_tickets WHERE signal_id = ?1 AND account = 'real'
             ORDER BY id DESC LIMIT 1",
            [signal_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(last) = last_id.map(|id| get_ticket(conn, id)).transpose()?.flatten() else {
        return Ok(None);
    };
    if last.status != TicketStatus::Expired {
        return Ok(None);
    }
    let id = create_ticket(
        conn,
        &NewTicket {
            user_id,
            signal_id,
            account: Account::Real,
            code: last.code.clone(),
            side: last.side,
            suggest_price: price,
            qty,
            expires_at: default_expiry(SignalSource::Exit, now),
            deviation_th: last.deviation_th,
            status: TicketStatus::Pending,
            urgency: last.urgency + 1,
            created_at: now,
        },
    )?;
    Ok(Some(id))
}
```

6. 把 `fn round2` 替换为:

```rust
fn round_dec(x: f64, decimals: i32) -> f64 {
    let m = 10f64.powi(decimals);
    (x * m).round() / m
}
```

`record_fill` 中两处 `round2(...)` 改为 `round_dec(..., price_decimals(&t.code))`,并在顶部 use 中加入 `use crate::stock::ashare::price_decimals;`。

7. 测试辅助 `fn ticket(...)` 中的 `NewTicket` 字面量加 `urgency: 0,`;`src/trade/router.rs` 测试辅助 `paper_ticket` 与 `batch` 相关的 `NewTicket` 字面量同样加 `urgency: 0,`。

- [ ] **Step 4: 实现 gate.rs 与 service.rs**

1. `GateInput` 在 `pub paper_position: Option<&'a Position>,` 之后加:

```rust
    /// 实盘未完结买入工单占用资金
    pub real_reserved_cash: f64,
    /// 模拟盘未完结买入工单占用资金
    pub paper_reserved_cash: f64,
```

2. `evaluate` 逐账户循环中:

```rust
        let (acc, pos, reserved) = match account {
            Account::Real => (inp.real_account, inp.real_position, inp.real_reserved_cash),
            Account::Paper => (inp.paper_account, inp.paper_position, inp.paper_reserved_cash),
        };
        let sized = match s.side {
            Direction::Buy => size_buy_for(s, q, rules, acc, pos, reserved),
            Direction::Sell => size_sell_for(s, pos, today),
        };
```

3. `size_buy_for` 增加参数 `reserved: f64`,预算数组中的 `acc.available_cash` 改为 `acc.available_cash - reserved`。
4. `src/trade/service.rs`:在 `realized_pnl_today` 之后计算

```rust
    let real_reserved_cash = ticket::reserved_cash(&tx, sig.user_id, Account::Real)?;
    let paper_reserved_cash = ticket::reserved_cash(&tx, sig.user_id, Account::Paper)?;
```

并传入 `GateInput`;`NewTicket` 字面量加 `urgency: 0,`。

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::`
Expected: 全部 PASS(ticket 新增 4 个、gate 新增 1 个)

Run: `cargo test --test trade_core`
Expected: 3 PASS

- [ ] **Step 6: Commit**

```bash
git add src/trade
git commit -m "feat(trade): 工单紧急度、模拟盘过期、挂单占用资金、止盈止损过期重发"
```

---

### Task 2: 报价 / 心跳表、全体持仓查询、模拟盘批量撮合错误隔离

**Files:**
- Modify: `src/trade/store.rs`、`src/trade/router.rs`
- Test: 同文件 `mod tests`

**Interfaces:**
- Consumes: Task 1
- Produces:

```rust
// store.rs
pub fn list_all_positions(conn: &Connection) -> Result<Vec<Position>>;
pub fn set_trailing_high(conn: &Connection, user_id: i64, account: Account, code: &str, high: f64, now: NaiveDateTime) -> Result<()>;
pub fn users_with_positions(conn: &Connection) -> Result<Vec<i64>>;
pub fn users_with_real_account(conn: &Connection) -> Result<Vec<i64>>;
pub fn upsert_quotes(conn: &Connection, quotes: &[Quote], now: NaiveDateTime) -> Result<usize>;
pub fn get_quote(conn: &Connection, code: &str) -> Result<Option<Quote>>;
pub fn fresh_quote(conn: &Connection, code: &str, now: NaiveDateTime, max_age_secs: i64) -> Result<Option<Quote>>;
pub fn beat(conn: &Connection, name: &str, now: NaiveDateTime) -> Result<()>;
pub fn last_beat(conn: &Connection, name: &str) -> Result<Option<NaiveDateTime>>;
// router.rs
pub struct PaperBatch { pub filled: usize, pub errors: Vec<(i64, String)> } // Debug, Clone, Default, PartialEq
pub fn fill_pending_paper(conn: &mut Connection, quotes: &HashMap<String, Quote>, now: NaiveDateTime) -> Result<PaperBatch>;
```

- [ ] **Step 1: 写失败测试**

`src/trade/store.rs` 的 `mod tests`:把 `migrate_is_idempotent_and_creates_tables` 中的 `assert_eq!(n, 6);` 改为 `assert_eq!(n, 8);`,并追加:

```rust
    #[test]
    fn quotes_upsert_and_freshness() {
        let c = db();
        let q = Quote {
            code: "600000".into(),
            price: 10.0,
            limit_up: Some(11.0),
            limit_down: None,
            ts: at(16, 10, 0),
        };
        assert_eq!(upsert_quotes(&c, &[q.clone()], at(16, 10, 0)).unwrap(), 1);
        assert_eq!(get_quote(&c, "600000").unwrap(), Some(q.clone()));
        let newer = Quote { price: 10.2, ts: at(16, 10, 1), ..q.clone() };
        upsert_quotes(&c, &[newer.clone()], at(16, 10, 1)).unwrap();
        assert_eq!(get_quote(&c, "600000").unwrap(), Some(newer.clone()));
        assert_eq!(fresh_quote(&c, "600000", at(16, 10, 2), 60).unwrap(), Some(newer));
        assert!(fresh_quote(&c, "600000", at(16, 10, 5), 60).unwrap().is_none(), "超过 60 秒视为陈旧");
        assert!(get_quote(&c, "000001").unwrap().is_none());
    }

    #[test]
    fn heartbeat_round_trip() {
        let c = db();
        assert!(last_beat(&c, "trade-monitor").unwrap().is_none());
        beat(&c, "trade-monitor", at(16, 10, 0)).unwrap();
        beat(&c, "trade-monitor", at(16, 10, 1)).unwrap();
        assert_eq!(last_beat(&c, "trade-monitor").unwrap(), Some(at(16, 10, 1)));
    }

    #[test]
    fn all_positions_trailing_high_and_user_lists() {
        let c = db();
        for (uid, account, code) in [(2, Account::Paper, "600036"), (1, Account::Real, "600000")] {
            let mut p = Position::empty(uid, account, code);
            p.qty = 100;
            p.avg_cost = 10.0;
            p.trailing_pct = Some(0.05);
            upsert_position(&c, &p, at(16, 9, 0)).unwrap();
        }
        let all = list_all_positions(&c).unwrap();
        assert_eq!(
            all.iter().map(|p| (p.user_id, p.account, p.code.as_str())).collect::<Vec<_>>(),
            vec![(1, Account::Real, "600000"), (2, Account::Paper, "600036")]
        );
        set_trailing_high(&c, 1, Account::Real, "600000", 12.5, at(16, 10, 0)).unwrap();
        assert_eq!(
            get_position(&c, 1, Account::Real, "600000").unwrap().unwrap().trailing_high,
            Some(12.5)
        );
        assert_eq!(users_with_positions(&c).unwrap(), vec![1, 2]);
        set_capital(&c, 3, Account::Real, 1000.0, at(16, 9, 0)).unwrap();
        set_capital(&c, 4, Account::Paper, 1000.0, at(16, 9, 0)).unwrap();
        assert_eq!(users_with_real_account(&c).unwrap(), vec![3]);
    }
```

`src/trade/router.rs` 的 `mod tests`:把 `sell_waits_t_plus_one_expires_stale_and_fills_fresh_ticket` 中每个 `fill_pending_paper(...).unwrap()` 的比较改为比较 `.filled`(例如 `assert_eq!(fill_pending_paper(&mut c, &quotes, at(15, 11, 10)).unwrap().filled, 0);`),并追加:

```rust
    #[test]
    fn batch_isolates_per_ticket_errors() {
        let mut c = db();
        let good = paper_ticket(&c, Direction::Buy, 100, at(15, 10, 0));
        // 用户 2 没有模拟盘账户 → 回填时 add_cash 报错
        let sid = insert_signal(
            &c,
            &NewSignal {
                user_id: 2,
                source: crate::trade::model::SignalSource::Manual,
                strategy_id: None,
                code: "600000".into(),
                name: None,
                side: Direction::Buy,
                scope: crate::trade::model::AccountScope::Both,
                ref_price: 10.0,
                reason: "r".into(),
                ai_note: None,
                dedup_key: "u2".into(),
                suggest_cash: None,
                suggest_qty: None,
            },
            at(15, 10, 0),
        )
        .unwrap()
        .unwrap();
        let bad = create_ticket(
            &c,
            &NewTicket {
                user_id: 2,
                signal_id: sid,
                account: Account::Paper,
                code: "600000".into(),
                side: Direction::Buy,
                suggest_price: 10.0,
                qty: 100,
                expires_at: at(15, 10, 30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                urgency: 0,
                created_at: at(15, 10, 0),
            },
        )
        .unwrap();
        let mut quotes = HashMap::new();
        quotes.insert("600000".to_string(), quote(10.0, None, None, at(15, 10, 1)));
        let batch = fill_pending_paper(&mut c, &quotes, at(15, 10, 1)).unwrap();
        assert_eq!(batch.filled, 1);
        assert_eq!(batch.errors.len(), 1);
        assert_eq!(batch.errors[0].0, bad);
        assert_eq!(ticket::get_ticket(&c, good.id).unwrap().unwrap().status, TicketStatus::Filled);
    }
```

> 若 `paper_ticket` 辅助函数创建的 `NewSignal` 字面量尚未含 `scope` 以外的新字段,保持原样即可;本测试的 `NewSignal` 字面量必须与当前 `model.rs` 字段一致(含 `scope`)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::store::tests trade::router::tests`
Expected: 编译失败(`upsert_quotes` 等未定义;`fill_pending_paper` 返回值无 `.filled`)

- [ ] **Step 3: 实现 store.rs**

1. `SCHEMA` 末尾(`trade_positions` 之后)追加:

```sql
CREATE TABLE IF NOT EXISTS trade_quotes (
  code       TEXT PRIMARY KEY,
  price      REAL NOT NULL,
  limit_up   REAL,
  limit_down REAL,
  ts         TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trade_heartbeat (
  name    TEXT PRIMARY KEY,
  beat_at TEXT NOT NULL
);
```

2. 顶部 use 改为包含 `parse_ts` 与 `Quote`:`use crate::trade::model::{fmt_ts, parse_ts, Account, AccountState, Position, Quote, RiskRules, DATE_FMT};`
3. 把 `fn read_raw_position(r: &Row)` 改为带偏移的 `fn read_raw_position_at(r: &Row, o: usize)`(每个 `r.get(i)` 改为 `r.get(o + i)`),原两处调用改为闭包 `|r| read_raw_position_at(r, 0)`。
4. 追加:

```rust
/// 全体用户全部持仓(监听线程用)。
pub fn list_all_positions(conn: &Connection) -> Result<Vec<Position>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT user_id, account, {POSITION_COLS} FROM trade_positions
         ORDER BY user_id, account, code"
    ))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                read_raw_position_at(r, 2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(uid, account, raw)| raw.into_position(uid, Account::parse(&account)?))
        .collect()
}

pub fn set_trailing_high(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
    high: f64,
    now: NaiveDateTime,
) -> Result<()> {
    conn.execute(
        "UPDATE trade_positions SET trailing_high = ?1, updated_at = ?2
         WHERE user_id = ?3 AND account = ?4 AND code = ?5",
        params![high, fmt_ts(now), user_id, account.as_str(), code],
    )?;
    Ok(())
}

fn user_ids(conn: &Connection, sql: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

pub fn users_with_positions(conn: &Connection) -> Result<Vec<i64>> {
    user_ids(conn, "SELECT DISTINCT user_id FROM trade_positions ORDER BY user_id")
}

pub fn users_with_real_account(conn: &Connection) -> Result<Vec<i64>> {
    user_ids(
        conn,
        "SELECT user_id FROM trade_accounts WHERE account = 'real' ORDER BY user_id",
    )
}

pub fn upsert_quotes(conn: &Connection, quotes: &[Quote], now: NaiveDateTime) -> Result<usize> {
    let mut stmt = conn.prepare(
        "INSERT INTO trade_quotes (code, price, limit_up, limit_down, ts, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(code) DO UPDATE SET price = excluded.price, limit_up = excluded.limit_up,
           limit_down = excluded.limit_down, ts = excluded.ts, updated_at = excluded.updated_at",
    )?;
    for q in quotes {
        stmt.execute(params![
            q.code,
            q.price,
            q.limit_up,
            q.limit_down,
            fmt_ts(q.ts),
            fmt_ts(now)
        ])?;
    }
    Ok(quotes.len())
}

pub fn get_quote(conn: &Connection, code: &str) -> Result<Option<Quote>> {
    let raw: Option<(String, f64, Option<f64>, Option<f64>, String)> = conn
        .query_row(
            "SELECT code, price, limit_up, limit_down, ts FROM trade_quotes WHERE code = ?1",
            [code],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    raw.map(|(code, price, limit_up, limit_down, ts)| {
        Ok(Quote {
            code,
            price,
            limit_up,
            limit_down,
            ts: parse_ts(&ts)?,
        })
    })
    .transpose()
}

/// 行情时间距 now 不超过 max_age_secs 的缓存报价。
pub fn fresh_quote(
    conn: &Connection,
    code: &str,
    now: NaiveDateTime,
    max_age_secs: i64,
) -> Result<Option<Quote>> {
    Ok(get_quote(conn, code)?.filter(|q| (now - q.ts).num_seconds() <= max_age_secs))
}

pub fn beat(conn: &Connection, name: &str, now: NaiveDateTime) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_heartbeat (name, beat_at) VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET beat_at = excluded.beat_at",
        params![name, fmt_ts(now)],
    )?;
    Ok(())
}

pub fn last_beat(conn: &Connection, name: &str) -> Result<Option<NaiveDateTime>> {
    let s: Option<String> = conn
        .query_row(
            "SELECT beat_at FROM trade_heartbeat WHERE name = ?1",
            [name],
            |r| r.get(0),
        )
        .optional()?;
    s.as_deref().map(parse_ts).transpose()
}
```

> `get_quote` 的元组类型若触发 clippy `type_complexity`,改为一个私有 `struct RawQuote`,逻辑不变。

- [ ] **Step 4: 实现 router.rs**

把 `fill_pending_paper` 替换为:

```rust
/// 一轮批量撮合的结果。单张工单出错不影响其余工单。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PaperBatch {
    pub filled: usize,
    /// (工单 id, 错误信息)
    pub errors: Vec<(i64, String)>,
}

/// 用最新一批报价撮合所有待成交模拟盘工单。
pub fn fill_pending_paper(
    conn: &mut Connection,
    quotes: &HashMap<String, Quote>,
    now: NaiveDateTime,
) -> Result<PaperBatch> {
    let mut batch = PaperBatch::default();
    let mut slippage_by_user: HashMap<i64, f64> = HashMap::new();
    for t in ticket::list_open_paper(conn)? {
        let Some(q) = quotes.get(&t.code) else {
            continue;
        };
        let slippage = match slippage_by_user.get(&t.user_id) {
            Some(s) => *s,
            None => match store::get_risk_rules(conn, t.user_id) {
                Ok(r) => {
                    slippage_by_user.insert(t.user_id, r.slippage);
                    r.slippage
                }
                Err(e) => {
                    batch.errors.push((t.id, format!("{e:#}")));
                    continue;
                }
            },
        };
        match fill_paper_ticket(conn, &t, q, slippage, now) {
            Ok(RouteOutcome::Filled { .. }) => batch.filled += 1,
            Ok(_) => {}
            Err(e) => batch.errors.push((t.id, format!("{e:#}"))),
        }
    }
    Ok(batch)
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::`
Expected: 全部 PASS

- [ ] **Step 6: Commit**

```bash
git add src/trade/store.rs src/trade/router.rs
git commit -m "feat(trade): 报价与心跳缓存、全体持仓查询、模拟盘批量撮合错误隔离"
```

---

### Task 3: 报价源与止盈止损判定(纯函数)

**Files:**
- Create: `src/trade/quotes.rs`、`src/trade/exits.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod exits;`、`pub mod quotes;`)
- Test: 两个新文件内 `mod tests`

**Interfaces:**
- Consumes: `crate::stock::realtime::snapshot::{fetch, symbol, Tick}`;`trade::model::{Account, AccountScope, NewSignal, Position, Quote, SignalSource, DATE_FMT}`
- Produces:

```rust
// quotes.rs
pub trait QuoteSource { fn fetch(&self, codes: &[String]) -> anyhow::Result<Vec<Quote>>; }
pub struct TencentQuotes;               // impl QuoteSource
pub fn tencent_symbol(code: &str) -> Option<String>;
pub fn tick_to_quote(t: &Tick) -> Quote;
// exits.rs
pub enum ExitRule { StopLoss, Trailing, TakeProfit }   // Copy, Eq; as_str "stop"/"trailing"/"take"; label "止损"/"移动止盈"/"止盈"
pub fn trigger(p: &Position, price: f64) -> Option<ExitRule>;
pub fn next_trailing_high(p: &Position, price: f64) -> Option<f64>;
pub fn scope_for(account: Account) -> AccountScope;
pub fn exit_dedup_key(p: &Position, rule: ExitRule, day: NaiveDate) -> String; // "exit-{account}-{code}-{rule}-{YYYY-MM-DD}"
pub fn exit_signal(p: &Position, q: &Quote, now: NaiveDateTime) -> Option<NewSignal>;
```

判定优先级:止损(`price ≤ stop_loss`)→ 移动止盈(`trailing_pct` 与 `trailing_high` 均有值且 `price ≤ high × (1 − pct)`)→ 止盈(`price ≥ take_profit`)。`qty == 0` 或价格非正 → 不触发。

- [ ] **Step 1: 写 quotes.rs(含测试)**

```rust
//! 报价源。监听线程经 trait 注入,测试用桩,生产用腾讯快照。

use crate::stock::realtime::snapshot::{self, Tick};
use crate::trade::model::Quote;
use anyhow::Result;

pub trait QuoteSource {
    fn fetch(&self, codes: &[String]) -> Result<Vec<Quote>>;
}

/// 6 位沪深代码 → 腾讯符号;北交所及非法代码返回 None。
pub fn tencent_symbol(code: &str) -> Option<String> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match code.as_bytes()[0] {
        b'6' | b'5' | b'9' => snapshot::symbol(1, code),
        b'0' | b'1' | b'2' | b'3' => snapshot::symbol(0, code),
        _ => None,
    }
}

pub fn tick_to_quote(t: &Tick) -> Quote {
    Quote {
        code: t.code.clone(),
        price: t.price,
        limit_up: t.limit_up,
        limit_down: t.limit_down,
        ts: t.ts,
    }
}

/// 腾讯实时快照。停牌股(成交量 0)被快照解析跳过,因而没有报价。
pub struct TencentQuotes;

impl QuoteSource for TencentQuotes {
    fn fetch(&self, codes: &[String]) -> Result<Vec<Quote>> {
        let symbols: Vec<String> = codes.iter().filter_map(|c| tencent_symbol(c)).collect();
        Ok(snapshot::fetch(&symbols)?.iter().map(tick_to_quote).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn symbols_for_sh_sz_only() {
        assert_eq!(tencent_symbol("600519").as_deref(), Some("sh600519"));
        assert_eq!(tencent_symbol("510300").as_deref(), Some("sh510300"));
        assert_eq!(tencent_symbol("000001").as_deref(), Some("sz000001"));
        assert_eq!(tencent_symbol("300750").as_deref(), Some("sz300750"));
        assert_eq!(tencent_symbol("159915").as_deref(), Some("sz159915"));
        assert_eq!(tencent_symbol("830799"), None, "北交所暂不支持");
        assert_eq!(tencent_symbol("60051"), None);
        assert_eq!(tencent_symbol("AAPL00"), None);
    }

    #[test]
    fn tick_maps_to_quote() {
        let ts = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap().and_hms_opt(10, 0, 3).unwrap();
        let t = Tick {
            code: "600519".into(),
            ts,
            price: 1500.0,
            change_pct: 1.0,
            volume: 1.0,
            amount: 1.0,
            turnover: 0.1,
            vol_ratio: 1.0,
            limit_up: Some(1650.0),
            limit_down: Some(1350.0),
        };
        assert_eq!(
            tick_to_quote(&t),
            Quote { code: "600519".into(), price: 1500.0, limit_up: Some(1650.0), limit_down: Some(1350.0), ts }
        );
    }
}
```

- [ ] **Step 2: 写 exits.rs 测试(先失败)**

创建 `src/trade/exits.rs`,先只写模块注释与测试:

```rust
//! 止盈 / 止损 / 移动止盈判定。纯函数:输入持仓与报价,输出是否触发及信号。

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap().and_hms_opt(10, 0, 0).unwrap()
    }

    fn pos(account: Account) -> Position {
        let mut p = Position::empty(1, account, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0;
        p.stop_loss = Some(9.2);
        p.take_profit = Some(12.0);
        p
    }

    fn quote(price: f64) -> Quote {
        Quote { code: "600000".into(), price, limit_up: None, limit_down: None, ts: now() }
    }

    #[test]
    fn trigger_priority_and_bounds() {
        let p = pos(Account::Real);
        assert_eq!(trigger(&p, 9.2), Some(ExitRule::StopLoss), "等于止损价即触发");
        assert_eq!(trigger(&p, 10.0), None);
        assert_eq!(trigger(&p, 12.0), Some(ExitRule::TakeProfit));
        let mut t = p.clone();
        t.trailing_pct = Some(0.05);
        t.trailing_high = Some(11.9);
        // 11.9 × 0.95 = 11.305
        assert_eq!(trigger(&t, 11.3), Some(ExitRule::Trailing));
        assert_eq!(trigger(&t, 11.31), None);
        t.stop_loss = Some(11.5);
        assert_eq!(trigger(&t, 11.3), Some(ExitRule::StopLoss), "止损优先于移动止盈");
        let mut empty = p.clone();
        empty.qty = 0;
        assert_eq!(trigger(&empty, 1.0), None);
        assert_eq!(trigger(&p, 0.0), None);
    }

    #[test]
    fn trailing_high_tracks_new_highs_only_when_enabled() {
        let mut p = pos(Account::Real);
        assert_eq!(next_trailing_high(&p, 13.0), None, "未启用移动止盈");
        p.trailing_pct = Some(0.05);
        assert_eq!(next_trailing_high(&p, 10.5), Some(10.5), "首次记录");
        p.trailing_high = Some(12.0);
        assert_eq!(next_trailing_high(&p, 12.5), Some(12.5));
        assert_eq!(next_trailing_high(&p, 12.0), None);
        assert_eq!(next_trailing_high(&p, 11.0), None);
    }

    #[test]
    fn exit_signal_is_scoped_and_keyed_per_account_rule_day() {
        let real = exit_signal(&pos(Account::Real), &quote(9.1), now()).unwrap();
        assert_eq!(real.dedup_key, "exit-real-600000-stop-2026-09-16");
        assert_eq!(real.scope, AccountScope::RealOnly);
        assert_eq!((real.source, real.side), (SignalSource::Exit, Direction::Sell));
        assert!(real.suggest_qty.is_none(), "全部可卖");
        assert!(real.reason.contains("止损"), "{}", real.reason);
        assert!((real.ref_price - 9.1).abs() < 1e-12);

        let paper = exit_signal(&pos(Account::Paper), &quote(12.3), now()).unwrap();
        assert_eq!(paper.dedup_key, "exit-paper-600000-take-2026-09-16");
        assert_eq!(paper.scope, AccountScope::PaperOnly);

        assert!(exit_signal(&pos(Account::Real), &quote(10.0), now()).is_none());
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod exits;` 与 `pub mod quotes;`。

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib trade::exits::tests trade::quotes::tests`
Expected: 编译失败(exits 函数未定义);quotes 在 exits 修好前一同无法编译

- [ ] **Step 4: 实现 exits.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::event::Direction;
use crate::trade::model::{
    Account, AccountScope, NewSignal, Position, Quote, SignalSource, DATE_FMT,
};
use chrono::{NaiveDate, NaiveDateTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitRule {
    StopLoss,
    Trailing,
    TakeProfit,
}

impl ExitRule {
    pub fn as_str(self) -> &'static str {
        match self {
            ExitRule::StopLoss => "stop",
            ExitRule::Trailing => "trailing",
            ExitRule::TakeProfit => "take",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExitRule::StopLoss => "止损",
            ExitRule::Trailing => "移动止盈",
            ExitRule::TakeProfit => "止盈",
        }
    }
}

pub fn trigger(p: &Position, price: f64) -> Option<ExitRule> {
    if p.qty == 0 || !(price.is_finite() && price > 0.0) {
        return None;
    }
    if p.stop_loss.is_some_and(|s| price <= s) {
        return Some(ExitRule::StopLoss);
    }
    if let (Some(pct), Some(high)) = (p.trailing_pct, p.trailing_high) {
        if pct > 0.0 && price <= high * (1.0 - pct) {
            return Some(ExitRule::Trailing);
        }
    }
    if p.take_profit.is_some_and(|t| price >= t) {
        return Some(ExitRule::TakeProfit);
    }
    None
}

/// 启用移动止盈时,价格创出新高则返回新的最高价。
pub fn next_trailing_high(p: &Position, price: f64) -> Option<f64> {
    p.trailing_pct?;
    match p.trailing_high {
        Some(high) if price <= high => None,
        _ => Some(price),
    }
}

pub fn scope_for(account: Account) -> AccountScope {
    match account {
        Account::Real => AccountScope::RealOnly,
        Account::Paper => AccountScope::PaperOnly,
    }
}

pub fn exit_dedup_key(p: &Position, rule: ExitRule, day: NaiveDate) -> String {
    format!(
        "exit-{}-{}-{}-{}",
        p.account.as_str(),
        p.code,
        rule.as_str(),
        day.format(DATE_FMT)
    )
}

pub fn exit_signal(p: &Position, q: &Quote, now: NaiveDateTime) -> Option<NewSignal> {
    let rule = trigger(p, q.price)?;
    let (cmp, level) = match rule {
        ExitRule::StopLoss => ("≤", p.stop_loss),
        ExitRule::TakeProfit => ("≥", p.take_profit),
        ExitRule::Trailing => (
            "≤",
            p.trailing_high.zip(p.trailing_pct).map(|(h, pct)| h * (1.0 - pct)),
        ),
    };
    let reason = format!(
        "触发{}:现价 {:.3} {} {:.3}(成本 {:.3})",
        rule.label(),
        q.price,
        cmp,
        level.unwrap_or(0.0),
        p.avg_cost
    );
    Some(NewSignal {
        user_id: p.user_id,
        source: SignalSource::Exit,
        strategy_id: None,
        code: p.code.clone(),
        name: None,
        side: Direction::Sell,
        scope: scope_for(p.account),
        ref_price: q.price,
        reason,
        ai_note: None,
        dedup_key: exit_dedup_key(p, rule, now.date()),
        suggest_cash: None,
        suggest_qty: None,
    })
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::exits::tests trade::quotes::tests`
Expected: 5 PASS

- [ ] **Step 6: Commit**

```bash
git add src/trade/quotes.rs src/trade/exits.rs src/trade/mod.rs
git commit -m "feat(trade): 腾讯报价源与止盈止损判定"
```

---

### Task 4: 单轮监听编排与异动信号

**Files:**
- Create: `src/trade/monitor.rs`、`src/trade/movers.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod monitor;`、`pub mod movers;`)
- Test: 两个新文件内 `mod tests`

**Interfaces:**
- Consumes: Task 1–3;`service::{submit_signal, SubmitContext, SubmitOutcome}`;`gate::Admission`;`crate::stock::realtime::calendar::is_weekend`;`crate::stock::realtime::movers::{trade_action, Mover, TradeAction}`;`crate::push::store::get`
- Produces:

```rust
// monitor.rs
pub enum Skip { OutOfSession, NothingWatched, StaleQuotes }                 // Copy, Eq
pub struct TickReport { pub skipped: Option<Skip>, pub expired: usize, pub quotes: usize, pub exit_signals: usize, pub new_real_tickets: Vec<i64>, pub paper: PaperBatch, pub errors: Vec<String> } // Debug, Clone, Default, PartialEq
pub fn is_session(now: NaiveDateTime) -> bool;
pub fn watched_codes(conn: &Connection) -> Result<Vec<String>>;
pub fn run_tick(conn: &mut Connection, source: &dyn QuoteSource, now: NaiveDateTime) -> Result<TickReport>;
// movers.rs
pub struct MoverReport { pub signals: usize, pub ticketed: usize, pub errors: Vec<String> } // Debug, Clone, Default, PartialEq
pub fn mover_signal(user_id: i64, m: &Mover, side: Direction) -> NewSignal;
pub fn submit_mover_signals(conn: &mut Connection, movers: &[Mover], now: NaiveDateTime) -> Result<MoverReport>;
```

`run_tick` 顺序:① `expire_due`(无论是否交易时段)→ ② 非交易时段跳过 → ③ 无关注代码跳过 → ④ 拉报价(错误向上返回,由线程退避)→ ⑤ 只保留日期为今日的报价,为空则跳过 → ⑥ 写缓存 → ⑦ 逐持仓:更新移动止盈最高价、判定、提交信号(`Admission::NotRequired`);实盘信号返回 `Duplicate` 时尝试 `reissue_expired_exit`;单持仓错误记入 `errors` 继续 → ⑧ 模拟盘批量撮合。

- [ ] **Step 1: 写 monitor.rs 测试(先失败)**

创建 `src/trade/monitor.rs`:

```rust
//! 交易监听单轮:过期 → 报价 → 缓存 → 止盈止损 → 过期重发 → 模拟盘撮合。
//! 报价源经参数注入,整轮可离线测试;线程、休眠与退避在 `daemon`。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Account, Position, TicketStatus};
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    struct Stub(std::result::Result<Vec<Quote>, String>);

    impl QuoteSource for Stub {
        fn fetch(&self, _codes: &[String]) -> Result<Vec<Quote>> {
            self.0.clone().map_err(|e| anyhow::anyhow!(e))
        }
    }

    fn quote(price: f64, ts: NaiveDateTime) -> Stub {
        Stub(Ok(vec![Quote {
            code: "600000".into(),
            price,
            limit_up: None,
            limit_down: Some(8.19),
            ts,
        }]))
    }

    fn db_with_position(configure: impl FnOnce(&mut Position)) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        configure(&mut p);
        store::upsert_position(&c, &p, at(15, 15, 0)).unwrap();
        c
    }

    #[test]
    fn session_windows() {
        assert!(is_session(at(16, 9, 30)));
        assert!(is_session(at(16, 11, 30)));
        assert!(!is_session(at(16, 12, 0)));
        assert!(is_session(at(16, 13, 0)));
        assert!(is_session(at(16, 15, 0)));
        assert!(!is_session(at(16, 15, 1)));
        assert!(!is_session(at(19, 10, 0)), "2026-09-19 是周六");
    }

    #[test]
    fn stop_loss_creates_ticket_then_reissues_after_expiry() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r1 = run_tick(&mut c, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!((r1.skipped, r1.quotes, r1.exit_signals), (None, 1, 1));
        assert_eq!(r1.new_real_tickets.len(), 1);
        let t1 = ticket::get_ticket(&c, r1.new_real_tickets[0]).unwrap().unwrap();
        assert_eq!((t1.qty, t1.urgency, t1.status), (1000, 0, TicketStatus::Pending));
        assert!(store::get_quote(&c, "600000").unwrap().is_some(), "报价已缓存");

        let r2 = run_tick(&mut c, &quote(9.0, at(16, 10, 15)), at(16, 10, 15)).unwrap();
        assert!(r2.new_real_tickets.is_empty(), "已有待确认工单,不重复");

        let r3 = run_tick(&mut c, &quote(9.0, at(16, 10, 31)), at(16, 10, 31)).unwrap();
        assert_eq!(r3.expired, 1);
        assert_eq!(r3.new_real_tickets.len(), 1);
        let t2 = ticket::get_ticket(&c, r3.new_real_tickets[0]).unwrap().unwrap();
        assert_eq!((t2.signal_id, t2.urgency), (t1.signal_id, 1));
        assert!(r3.errors.is_empty(), "{:?}", r3.errors);
    }

    #[test]
    fn trailing_high_is_recorded_then_triggers() {
        let mut c = db_with_position(|p| p.trailing_pct = Some(0.05));
        let r1 = run_tick(&mut c, &quote(12.0, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r1.exit_signals, 0);
        let p = store::get_position(&c, 1, Account::Real, "600000").unwrap().unwrap();
        assert_eq!(p.trailing_high, Some(12.0));
        let r2 = run_tick(&mut c, &quote(11.3, at(16, 10, 1)), at(16, 10, 1)).unwrap();
        assert_eq!((r2.exit_signals, r2.new_real_tickets.len()), (1, 1));
    }

    #[test]
    fn skips_and_errors() {
        let mut c = db_with_position(|p| p.stop_loss = Some(9.2));
        let r = run_tick(&mut c, &quote(9.1, at(16, 12, 0)), at(16, 12, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::OutOfSession));
        let r = run_tick(&mut c, &quote(9.1, at(15, 15, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::StaleQuotes), "昨日报价");
        assert!(run_tick(&mut c, &Stub(Err("网络错误".into())), at(16, 10, 0)).is_err());

        let mut empty = Connection::open_in_memory().unwrap();
        store::migrate(&empty).unwrap();
        let r = run_tick(&mut empty, &quote(9.1, at(16, 10, 0)), at(16, 10, 0)).unwrap();
        assert_eq!(r.skipped, Some(Skip::NothingWatched));
        assert_eq!(watched_codes(&c).unwrap(), vec!["600000".to_string()]);
    }
}
```

- [ ] **Step 2: 写 movers.rs 测试(先失败)**

创建 `src/trade/movers.rs`:

```rust
//! 实时异动 → 交易信号。策略准入(计划 3)上线前一律按观察期处理:只进模拟盘。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stock::realtime::movers::{Baseline, Divergence, Horizon};
    use crate::trade::model::{Account, Position};
    use crate::trade::ticket;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn mover(code: &str, divergence: Divergence) -> Mover {
        Mover {
            code: code.into(),
            name: "测试股".into(),
            ts: at(10, 30),
            price: 10.0,
            jump_pct: 0.03,
            vol_surge_x: 4.0,
            main_net: Some(1.0e7),
            main_net_pct: Some(0.08),
            divergence,
            horizon: Horizon::Short,
            baseline: Baseline::History,
        }
    }

    fn db(watch: &[&str]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::trade::store::migrate(&c).unwrap();
        crate::push::store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0)).unwrap();
        let mut cfg = crate::push::config::default_config();
        cfg.realtime_watch_stocks = watch.iter().map(|s| s.to_string()).collect();
        crate::push::store::upsert(&c, 1, &cfg).unwrap();
        c
    }

    #[test]
    fn buy_mover_in_watchlist_becomes_paper_only_ticket() {
        let mut c = db(&["600000"]);
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::MainAccumulating), mover("600036", Divergence::MainAccumulating)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!((r.signals, r.ticketed), (1, 1), "{:?}", r.errors);
        let tickets = ticket::list_tickets(&c, 1, &[]).unwrap();
        assert_eq!(tickets.len(), 1);
        assert_eq!(tickets[0].account, Account::Paper, "观察期只进模拟盘");
        let sig = mover_signal(1, &mover("600000", Divergence::MainAccumulating), Direction::Buy);
        assert_eq!(sig.dedup_key, "mover-buy-600000-202609161030");
        assert_eq!(sig.source, SignalSource::Mover);
    }

    #[test]
    fn hold_and_unheld_sell_movers_are_ignored() {
        let mut c = db(&["600000"]);
        let r = submit_mover_signals(
            &mut c,
            &[mover("600000", Divergence::None), mover("600000", Divergence::RetailChasing)],
            at(10, 31),
        )
        .unwrap();
        assert_eq!(r.signals, 0);

        let mut p = Position::empty(1, Account::Paper, "600000");
        p.qty = 1000;
        p.avg_cost = 9.0;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        store::upsert_position(&c, &p, at(9, 0)).unwrap();
        let r = submit_mover_signals(&mut c, &[mover("600000", Divergence::RetailChasing)], at(10, 31)).unwrap();
        assert_eq!((r.signals, r.ticketed), (1, 1), "{:?}", r.errors);
    }

    #[test]
    fn users_without_watchlist_or_real_account_are_skipped() {
        let mut c = db(&[]);
        let r = submit_mover_signals(&mut c, &[mover("600000", Divergence::MainAccumulating)], at(10, 31)).unwrap();
        assert_eq!(r.signals, 0, "空名单不生成交易信号");
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod monitor;` 与 `pub mod movers;`。

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib trade::monitor::tests trade::movers::tests`
Expected: 编译失败(`run_tick`、`submit_mover_signals` 等未定义)

- [ ] **Step 4: 实现 monitor.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::stock::realtime::calendar::is_weekend;
use crate::trade::exits::{exit_signal, next_trailing_high};
use crate::trade::gate::Admission;
use crate::trade::model::{Account, Position, Quote};
use crate::trade::quotes::QuoteSource;
use crate::trade::router::{self, PaperBatch};
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use crate::trade::{store, ticket};
use anyhow::Result;
use chrono::{NaiveDateTime, Timelike};
use rusqlite::Connection;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    OutOfSession,
    NothingWatched,
    StaleQuotes,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickReport {
    pub skipped: Option<Skip>,
    pub expired: usize,
    pub quotes: usize,
    pub exit_signals: usize,
    /// 本轮新建的实盘工单(含过期重发),供推送
    pub new_real_tickets: Vec<i64>,
    pub paper: PaperBatch,
    pub errors: Vec<String>,
}

/// 工作日 09:30–11:30、13:00–15:00(含端点)。
pub fn is_session(now: NaiveDateTime) -> bool {
    if is_weekend(now.date()) {
        return false;
    }
    let m = now.hour() * 60 + now.minute();
    (570..=690).contains(&m) || (780..=900).contains(&m)
}

/// 需要报价的代码:全体持仓 ∪ 未完结工单。
pub fn watched_codes(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT code FROM trade_positions
         UNION
         SELECT code FROM trade_tickets WHERE status IN ('pending', 'confirmed', 'partial')
         ORDER BY code",
    )?;
    let codes = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(codes)
}

pub fn run_tick(
    conn: &mut Connection,
    source: &dyn QuoteSource,
    now: NaiveDateTime,
) -> Result<TickReport> {
    let mut report = TickReport {
        expired: ticket::expire_due(conn, now)?,
        ..TickReport::default()
    };
    if !is_session(now) {
        report.skipped = Some(Skip::OutOfSession);
        return Ok(report);
    }
    let codes = watched_codes(conn)?;
    if codes.is_empty() {
        report.skipped = Some(Skip::NothingWatched);
        return Ok(report);
    }
    let fresh: Vec<Quote> = source
        .fetch(&codes)?
        .into_iter()
        .filter(|q| q.ts.date() == now.date())
        .collect();
    if fresh.is_empty() {
        report.skipped = Some(Skip::StaleQuotes);
        return Ok(report);
    }
    store::upsert_quotes(conn, &fresh, now)?;
    report.quotes = fresh.len();
    let quotes: HashMap<String, Quote> = fresh.into_iter().map(|q| (q.code.clone(), q)).collect();

    for p in store::list_all_positions(conn)? {
        let Some(q) = quotes.get(&p.code) else {
            continue;
        };
        let label = format!("用户 {} {} {}", p.user_id, p.account.as_str(), p.code);
        if let Err(e) = process_position(conn, p, q, now, &mut report) {
            report.errors.push(format!("{label}: {e:#}"));
        }
    }
    report.paper = router::fill_pending_paper(conn, &quotes, now)?;
    Ok(report)
}

fn process_position(
    conn: &mut Connection,
    mut p: Position,
    q: &Quote,
    now: NaiveDateTime,
    report: &mut TickReport,
) -> Result<()> {
    if let Some(high) = next_trailing_high(&p, q.price) {
        store::set_trailing_high(conn, p.user_id, p.account, &p.code, high, now)?;
        p.trailing_high = Some(high);
    }
    let Some(sig) = exit_signal(&p, q, now) else {
        return Ok(());
    };
    report.exit_signals += 1;
    let ctx = SubmitContext {
        quote: Some(q),
        admission: Admission::NotRequired,
        now,
    };
    match submit_signal(conn, &sig, &ctx)? {
        SubmitOutcome::Ticketed {
            real_ticket: Some(id),
            ..
        } => report.new_real_tickets.push(id),
        SubmitOutcome::Duplicate if p.account == Account::Real => {
            if let Some(id) = ticket::reissue_expired_exit(
                conn,
                p.user_id,
                &sig.dedup_key,
                p.sellable(now.date()),
                q.price,
                now,
            )? {
                report.new_real_tickets.push(id);
            }
        }
        _ => {}
    }
    Ok(())
}
```

- [ ] **Step 5: 实现 movers.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::event::Direction;
use crate::stock::realtime::movers::{trade_action, Mover, TradeAction};
use crate::trade::gate::Admission;
use crate::trade::model::{
    side_str, Account, AccountScope, NewSignal, Quote, SignalSource,
};
use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use crate::trade::store;
use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::Connection;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MoverReport {
    pub signals: usize,
    pub ticketed: usize,
    pub errors: Vec<String>,
}

pub fn mover_signal(user_id: i64, m: &Mover, side: Direction) -> NewSignal {
    let hint = match side {
        Direction::Buy => "主力疑似吸筹",
        Direction::Sell => "疑似散户抬轿、主力流出",
    };
    NewSignal {
        user_id,
        source: SignalSource::Mover,
        strategy_id: None,
        code: m.code.clone(),
        name: Some(m.name.clone()),
        side,
        scope: AccountScope::Both,
        ref_price: m.price,
        reason: format!(
            "盘中异动:10 分钟 {:+.1}%,量能 {:.1} 倍,{hint}(未经前瞻检验,观察期仅模拟盘)",
            m.jump_pct * 100.0,
            m.vol_surge_x
        ),
        ai_note: None,
        dedup_key: format!(
            "mover-{}-{}-{}",
            side_str(side),
            m.code,
            m.ts.format("%Y%m%d%H%M")
        ),
        suggest_cash: None,
        suggest_qty: None,
    }
}

pub fn submit_mover_signals(
    conn: &mut Connection,
    movers: &[Mover],
    now: NaiveDateTime,
) -> Result<MoverReport> {
    let mut report = MoverReport::default();
    if movers.is_empty() {
        return Ok(report);
    }
    for uid in store::users_with_real_account(conn)? {
        let watch = match crate::push::store::get(conn, uid)? {
            Some(cfg) => cfg.realtime_watch_stocks,
            None => continue,
        };
        if watch.is_empty() {
            continue;
        }
        for m in movers.iter().filter(|m| watch.iter().any(|c| c == &m.code)) {
            let side = match trade_action(m.divergence) {
                TradeAction::Buy => Direction::Buy,
                TradeAction::Sell => {
                    let held = store::get_position(conn, uid, Account::Real, &m.code)?.is_some()
                        || store::get_position(conn, uid, Account::Paper, &m.code)?.is_some();
                    if !held {
                        continue;
                    }
                    Direction::Sell
                }
                TradeAction::Hold => continue,
            };
            let quote = store::fresh_quote(conn, &m.code, now, 60)?.unwrap_or(Quote {
                code: m.code.clone(),
                price: m.price,
                limit_up: None,
                limit_down: None,
                ts: m.ts,
            });
            let sig = mover_signal(uid, m, side);
            report.signals += 1;
            let ctx = SubmitContext {
                quote: Some(&quote),
                admission: Admission::Probation,
                now,
            };
            match submit_signal(conn, &sig, &ctx) {
                Ok(SubmitOutcome::Ticketed { .. }) => report.ticketed += 1,
                Ok(_) => {}
                Err(e) => report.errors.push(format!("用户 {uid} {}: {e:#}", m.code)),
            }
        }
    }
    Ok(report)
}
```

- [ ] **Step 6: 运行确认通过**

Run: `cargo test --lib trade::monitor::tests trade::movers::tests`
Expected: 7 PASS

若 `hold_and_unheld_sell_movers_are_ignored` 第二段(模拟盘持仓 + 卖出异动)失败,核对:观察期准入 → 仅模拟盘账户;模拟盘账户由 `submit_signal` 按实盘资金自动创建;持仓 `last_buy_date` 为 9-15 → 可卖 1000 → 应生成模拟盘卖出工单。

- [ ] **Step 7: Commit**

```bash
git add src/trade/monitor.rs src/trade/movers.rs src/trade/mod.rs
git commit -m "feat(trade): 单轮监听编排(止盈止损、过期重发、模拟盘撮合)与异动信号"
```

---

### Task 5: `[trade]` 配置与推送渲染

**Files:**
- Create: `src/trade/config.rs`、`src/trade/notify.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod config;`、`pub mod notify;`)、`src/push/schedule.rs:39`(`fn user_allowed` → `pub(crate) fn user_allowed`)
- Test: 两个新文件内 `mod tests`

**Interfaces:**
- Consumes: `trade::model::Ticket`;`crate::push::{store, channels, schedule::user_allowed}`
- Produces:

```rust
// config.rs
pub struct TradeCfg { pub enabled: bool, pub monitor_interval_secs: u64, pub alert_after_secs: i64, pub mover_signals: bool } // Default: true, 15, 180, true
pub fn from_toml_str(text: &str) -> Result<TradeCfg>;
pub fn init(path: &Path) -> Result<&'static TradeCfg>;
pub fn get() -> &'static TradeCfg;
// notify.rs
pub trait Notifier { fn notify(&self, conn: &Connection, user_id: i64, title: &str, md: &str) -> Result<()>; }
pub struct PushNotifier { pub warn_days: i64, pub grace_days: i64 }
pub fn side_label(side: Direction) -> &'static str;          // "买入" | "卖出"
pub fn render_new_ticket(t: &Ticket, reason: &str) -> (String, String);
pub fn render_fill_reminder(tickets: &[Ticket]) -> (String, String);
pub fn render_monitor_down(minutes: i64) -> (String, String);
pub fn signal_reason(conn: &Connection, signal_id: i64) -> Result<String>;
```

- [ ] **Step 1: 写 config.rs(含测试)**

```rust
//! `config.toml` 的 `[trade]` 段。段缺失 → 默认;段非法 → 报错(调用方决定禁用监听)。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeCfg {
    /// 是否启动交易监听线程
    pub enabled: bool,
    /// 交易时段报价轮询间隔(秒)
    pub monitor_interval_secs: u64,
    /// 连续失败多久后推送「止损监听中断」(秒)
    pub alert_after_secs: i64,
    /// 是否把实时异动转为(观察期)交易信号
    pub mover_signals: bool,
}

impl Default for TradeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            monitor_interval_secs: 15,
            alert_after_secs: 180,
            mover_signals: true,
        }
    }
}

static CFG: std::sync::OnceLock<TradeCfg> = std::sync::OnceLock::new();

pub fn from_toml_str(text: &str) -> Result<TradeCfg> {
    #[derive(Deserialize)]
    struct Root {
        trade: Option<TradeCfg>,
    }
    let root: Root = toml::from_str(text).map_err(|e| anyhow!("[trade] 段解析失败: {e}"))?;
    let cfg = root.trade.unwrap_or_default();
    if cfg.monitor_interval_secs < 5 {
        return Err(anyhow!(
            "[trade] monitor_interval_secs 须 ≥ 5,当前 {}",
            cfg.monitor_interval_secs
        ));
    }
    if cfg.alert_after_secs < 60 {
        return Err(anyhow!(
            "[trade] alert_after_secs 须 ≥ 60,当前 {}",
            cfg.alert_after_secs
        ));
    }
    Ok(cfg)
}

pub fn init(path: &Path) -> Result<&'static TradeCfg> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("读取配置 {} 失败: {e}", path.display()))?;
    let cfg = from_toml_str(&text)?;
    Ok(CFG.get_or_init(|| cfg))
}

pub fn get() -> &'static TradeCfg {
    CFG.get_or_init(TradeCfg::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_uses_defaults() {
        assert_eq!(from_toml_str("[realtime]\nretain_days = 10\n").unwrap(), TradeCfg::default());
    }

    #[test]
    fn partial_section_overrides_fields() {
        let c = from_toml_str("[trade]\nmonitor_interval_secs = 30\nmover_signals = false\n").unwrap();
        assert_eq!(c.monitor_interval_secs, 30);
        assert!(!c.mover_signals);
        assert!(c.enabled);
    }

    #[test]
    fn invalid_values_are_errors() {
        assert!(from_toml_str("[trade]\nmonitor_interval_secs = 1\n").is_err());
        assert!(from_toml_str("[trade]\nalert_after_secs = 10\n").is_err());
        assert!(from_toml_str("[trade]\nenabled = \"yes\"\n").is_err());
    }
}
```

- [ ] **Step 2: 写 notify.rs 测试(先失败)**

创建 `src/trade/notify.rs`:

```rust
//! 交易推送:渲染文案 + 经用户推送渠道发送。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::{Account, TicketStatus};
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn ticket(urgency: i64) -> Ticket {
        Ticket {
            id: 7,
            user_id: 1,
            signal_id: 3,
            account: Account::Real,
            code: "600000".into(),
            side: Direction::Sell,
            suggest_price: 9.1,
            qty: 1000,
            filled_qty: 0,
            expires_at: at(10, 30),
            deviation_th: 0.015,
            status: TicketStatus::Pending,
            urgency,
            created_at: at(10, 0),
            confirmed_at: None,
            ignore_reason: None,
        }
    }

    #[test]
    fn new_ticket_message() {
        let (title, md) = render_new_ticket(&ticket(0), "触发止损:现价 9.100 ≤ 9.200");
        assert_eq!(title, "交易工单:卖出 600000");
        for s in ["1000 股", "9.10", "10:30", "1.5%", "触发止损", "回填"] {
            assert!(md.contains(s), "缺少 {s}: {md}");
        }
        assert!(!md.contains("次提醒"));
        let (title, md) = render_new_ticket(&ticket(1), "r");
        assert_eq!(title, "交易工单:卖出 600000(重发)");
        assert!(md.contains("第 2 次提醒"), "{md}");
    }

    #[test]
    fn reminder_and_alert_messages() {
        let mut partial = ticket(0);
        partial.filled_qty = 400;
        partial.side = Direction::Buy;
        let (title, md) = render_fill_reminder(&[ticket(0), partial]);
        assert_eq!(title, "待回填成交提醒");
        assert!(md.contains("卖出 600000:已回填 0 / 1000 股"), "{md}");
        assert!(md.contains("买入 600000:已回填 400 / 1000 股"), "{md}");
        let (title, md) = render_monitor_down(4);
        assert_eq!(title, "止损监听中断");
        assert!(md.contains("4 分钟"), "{md}");
    }

    #[test]
    fn reason_lookup() {
        let c = Connection::open_in_memory().unwrap();
        crate::trade::store::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO trade_signals (id, user_id, source, code, side, ref_price, reason, dedup_key, created_at)
             VALUES (3, 1, 'exit', '600000', 'sell', 9.1, '触发止损', 'k', '2026-09-16 10:00:00')",
            [],
        )
        .unwrap();
        assert_eq!(signal_reason(&c, 3).unwrap(), "触发止损");
        assert!(signal_reason(&c, 4).is_err());
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod config;` 与 `pub mod notify;`;把 `src/push/schedule.rs` 中 `fn user_allowed(` 改为 `pub(crate) fn user_allowed(`。

- [ ] **Step 3: 运行确认失败**

Run: `cargo test --lib trade::config::tests trade::notify::tests`
Expected: 编译失败(notify 函数未定义)

- [ ] **Step 4: 实现 notify.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::event::Direction;
use crate::trade::model::Ticket;
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::{Connection, OptionalExtension};

pub trait Notifier {
    fn notify(&self, conn: &Connection, user_id: i64, title: &str, md: &str) -> Result<()>;
}

/// 经用户在推送设置里配置的渠道发送;未授权 / 未配置渠道的用户静默跳过。
pub struct PushNotifier {
    pub warn_days: i64,
    pub grace_days: i64,
}

impl Notifier for PushNotifier {
    fn notify(&self, conn: &Connection, user_id: i64, title: &str, md: &str) -> Result<()> {
        let today = chrono::Local::now().date_naive();
        if !crate::push::schedule::user_allowed(conn, user_id, today, self.warn_days, self.grace_days) {
            return Ok(());
        }
        let Some(cfg) = crate::push::store::get(conn, user_id)? else {
            return Ok(());
        };
        if cfg.channel.webhook.trim().is_empty() {
            return Ok(());
        }
        crate::push::channels::send(&cfg.channel, title, md)
    }
}

pub fn side_label(side: Direction) -> &'static str {
    match side {
        Direction::Buy => "买入",
        Direction::Sell => "卖出",
    }
}

pub fn render_new_ticket(t: &Ticket, reason: &str) -> (String, String) {
    let title = format!(
        "交易工单:{} {}{}",
        side_label(t.side),
        t.code,
        if t.urgency > 0 { "(重发)" } else { "" }
    );
    let mut md = format!(
        "### {title}\n\n- 数量:{} 股\n- 建议价:{:.2}(现价偏离超过 {:.1}% 需二次确认)\n- 有效期至:{}\n- 理由:{reason}\n",
        t.qty,
        t.suggest_price,
        t.deviation_th * 100.0,
        t.expires_at.format("%H:%M"),
    );
    if t.urgency > 0 {
        md.push_str(&format!(
            "- ⚠ 第 {} 次提醒:上一张工单已过期,触发条件仍然成立\n",
            t.urgency + 1
        ));
    }
    md.push_str("\n请在「交易」页确认,在券商 App 下单后回填成交。");
    (title, md)
}

pub fn render_fill_reminder(tickets: &[Ticket]) -> (String, String) {
    let title = "待回填成交提醒".to_string();
    let mut md = format!("### {title}\n\n以下工单已确认但尚未回填,次日 09:00 将自动撤销:\n\n");
    for t in tickets {
        md.push_str(&format!(
            "- {} {}:已回填 {} / {} 股\n",
            side_label(t.side),
            t.code,
            t.filled_qty,
            t.qty
        ));
    }
    (title, md)
}

pub fn render_monitor_down(minutes: i64) -> (String, String) {
    let title = "止损监听中断".to_string();
    let md = format!(
        "### {title}\n\n行情获取已连续失败 {minutes} 分钟,止盈止损暂未生效,请自行关注持仓。恢复后将自动继续监听。"
    );
    (title, md)
}

pub fn signal_reason(conn: &Connection, signal_id: i64) -> Result<String> {
    conn.query_row(
        "SELECT reason FROM trade_signals WHERE id = ?1",
        [signal_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| anyhow!("信号 {signal_id} 不存在"))
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::config::tests trade::notify::tests`
Expected: 6 PASS

- [ ] **Step 6: Commit**

```bash
git add src/trade/config.rs src/trade/notify.rs src/trade/mod.rs src/push/schedule.rs
git commit -m "feat(trade): [trade] 配置段与交易推送渲染"
```

---

### Task 6: 监听线程、调度、推送分发与启动接线

**Files:**
- Create: `src/trade/daemon.rs`、`tests/trade_runtime.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod daemon;`)、`src/push/schedule.rs`(`run_multi`、`realtime_tick`)、`src/push/mod.rs`(`run_multi_daemon`)、`src/main.rs`(Push 分支)
- Test: `src/trade/daemon.rs` 内 `mod tests`、`tests/trade_runtime.rs`

**Interfaces:**
- Consumes: Task 1–5 全部
- Produces:

```rust
pub struct Backoff { .. }  // Default;on_failure(now)、on_success() -> bool、delay_secs(base) -> u64、should_alert(now, alert_after_secs) -> bool、mark_alerted()、failing_minutes(now) -> i64
pub fn due_daily(now: NaiveDateTime, hour: u32, minute: u32, last_run: Option<NaiveDate>) -> bool;
pub fn notify_new_tickets(conn: &Connection, notifier: &dyn Notifier, ticket_ids: &[i64]) -> usize;
pub fn send_fill_reminders(conn: &Connection, notifier: &dyn Notifier) -> Result<usize>;
pub fn alert_holders(conn: &Connection, notifier: &dyn Notifier, minutes: i64) -> Result<usize>;
pub struct MoverSink(..);   // Clone;send(&self, movers: Vec<Mover>)
pub fn spawn(db_path: PathBuf, cfg: TradeCfg, warn_days: i64, grace_days: i64) -> std::io::Result<(std::thread::JoinHandle<()>, MoverSink)>;
// push
pub fn run_multi_daemon(conn: &Connection, warn_days: i64, grace_days: i64, trade: Option<crate::trade::daemon::MoverSink>) -> anyhow::Result<()>;
```

退避:失败 1 次后间隔 = base × 2,≥ 2 次 = base × 4(15 → 30 → 60);成功即清零。`due_daily`:工作日、`now.time() ≥ hour:minute`、且 `last_run != 今日`。

- [ ] **Step 1: 写 daemon.rs 纯函数测试与集成测试(先失败)**

创建 `src/trade/daemon.rs`:

```rust
//! 交易监听线程:每轮 run_tick → 推送新工单;失败退避与中断告警;09:00 撤销、15:05 提醒;
//! 接收推送主循环转来的实时异动。任何错误只记日志,线程不退出。

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn backoff_doubles_to_cap_alerts_once_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.delay_secs(15), 15);
        b.on_failure(at(16, 10, 0));
        assert_eq!(b.delay_secs(15), 30);
        b.on_failure(at(16, 10, 1));
        assert_eq!(b.delay_secs(15), 60);
        b.on_failure(at(16, 10, 2));
        assert_eq!(b.delay_secs(15), 60, "封顶 60 秒");
        assert!(!b.should_alert(at(16, 10, 2), 180));
        assert!(b.should_alert(at(16, 10, 3), 180), "从首次失败起满 3 分钟");
        assert_eq!(b.failing_minutes(at(16, 10, 3)), 3);
        b.mark_alerted();
        assert!(!b.should_alert(at(16, 10, 9), 180), "同一次中断只告警一次");
        assert!(b.on_success(), "从失败中恢复");
        assert!(!b.on_success());
        assert_eq!(b.delay_secs(15), 15);
    }

    #[test]
    fn daily_jobs_run_once_on_weekdays_after_time() {
        assert!(!due_daily(at(16, 8, 59), 9, 0, None));
        assert!(due_daily(at(16, 9, 0), 9, 0, None));
        assert!(due_daily(at(16, 14, 0), 9, 0, Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap())));
        assert!(!due_daily(at(16, 9, 5), 9, 0, Some(NaiveDate::from_ymd_opt(2026, 9, 16).unwrap())));
        assert!(!due_daily(at(19, 9, 5), 9, 0, None), "周六不跑");
    }
}
```

创建 `tests/trade_runtime.rs`:

```rust
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::Connection;
use std::cell::RefCell;
use xlh::trade::daemon::{alert_holders, notify_new_tickets, send_fill_reminders};
use xlh::trade::gate::Admission;
use xlh::trade::model::{Account, AccountScope, NewSignal, Position, Quote, SignalSource};
use xlh::trade::monitor::run_tick;
use xlh::trade::notify::Notifier;
use xlh::trade::quotes::QuoteSource;
use xlh::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
use xlh::trade::{store, ticket};

fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
}

#[derive(Default)]
struct Recorder(RefCell<Vec<(i64, String, String)>>);

impl Notifier for Recorder {
    fn notify(&self, _conn: &Connection, user_id: i64, title: &str, md: &str) -> anyhow::Result<()> {
        self.0.borrow_mut().push((user_id, title.to_string(), md.to_string()));
        Ok(())
    }
}

struct Fixed(Vec<Quote>);

impl QuoteSource for Fixed {
    fn fetch(&self, _codes: &[String]) -> anyhow::Result<Vec<Quote>> {
        Ok(self.0.clone())
    }
}

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    store::migrate(&c).unwrap();
    for uid in [1, 2] {
        store::set_capital(&c, uid, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
    }
    c
}

fn hold(c: &Connection, uid: i64) {
    let mut p = Position::empty(uid, Account::Real, "600000");
    p.qty = 1000;
    p.avg_cost = 10.0;
    p.stop_loss = Some(9.2);
    p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
    store::upsert_position(c, &p, at(15, 15, 0)).unwrap();
}

#[test]
fn stop_loss_tick_notifies_owner_with_reason() {
    let mut c = db();
    hold(&c, 1);
    let quotes = Fixed(vec![Quote { code: "600000".into(), price: 9.1, limit_up: None, limit_down: Some(8.19), ts: at(16, 10, 0) }]);
    let report = run_tick(&mut c, &quotes, at(16, 10, 0)).unwrap();
    let rec = Recorder::default();
    assert_eq!(notify_new_tickets(&c, &rec, &report.new_real_tickets), 1);
    let sent = rec.0.borrow();
    assert_eq!(sent[0].0, 1);
    assert_eq!(sent[0].1, "交易工单:卖出 600000");
    assert!(sent[0].2.contains("触发止损"), "{}", sent[0].2);
}

#[test]
fn fill_reminders_are_grouped_per_user() {
    let mut c = db();
    let q = Quote { code: "600000".into(), price: 10.0, limit_up: Some(11.0), limit_down: Some(9.0), ts: at(16, 10, 0) };
    for uid in [1, 2] {
        let sig = NewSignal {
            user_id: uid,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side: xlh::event::Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "手动".into(),
            ai_note: None,
            dedup_key: format!("manual-{uid}"),
            suggest_cash: None,
            suggest_qty: None,
        };
        let ctx = SubmitContext { quote: Some(&q), admission: Admission::NotRequired, now: at(16, 10, 0) };
        let SubmitOutcome::Ticketed { real_ticket: Some(id), .. } = submit_signal(&mut c, &sig, &ctx).unwrap() else {
            panic!("应生成实盘工单");
        };
        ticket::confirm(&c, uid, id, at(16, 10, 1)).unwrap();
    }
    let rec = Recorder::default();
    assert_eq!(send_fill_reminders(&c, &rec).unwrap(), 2);
    let sent = rec.0.borrow();
    assert_eq!(sent.iter().map(|s| s.0).collect::<Vec<_>>(), vec![1, 2]);
    assert!(sent.iter().all(|s| s.1 == "待回填成交提醒"));
}

#[test]
fn monitor_down_alert_goes_to_position_holders_only() {
    let c = db();
    hold(&c, 2);
    let rec = Recorder::default();
    assert_eq!(alert_holders(&c, &rec, 3).unwrap(), 1);
    assert_eq!(rec.0.borrow()[0].0, 2);
}
```

在 `src/trade/mod.rs` 加 `pub mod daemon;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::daemon::tests` 与 `cargo test --test trade_runtime`
Expected: 编译失败(`Backoff`、`due_daily`、`notify_new_tickets` 等未定义)

- [ ] **Step 3: 实现 daemon.rs**

在模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::stock::realtime::calendar::is_weekend;
use crate::stock::realtime::movers::Mover;
use crate::trade::config::TradeCfg;
use crate::trade::notify::{
    render_fill_reminder, render_monitor_down, render_new_ticket, signal_reason, Notifier,
    PushNotifier,
};
use crate::trade::quotes::TencentQuotes;
use crate::trade::{monitor, movers, store, ticket};
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Backoff {
    failures: u32,
    first_failure: Option<NaiveDateTime>,
    alerted: bool,
}

impl Backoff {
    pub fn on_failure(&mut self, now: NaiveDateTime) {
        self.failures = self.failures.saturating_add(1);
        self.first_failure.get_or_insert(now);
    }

    /// 成功一轮:清零;返回此前是否处于失败状态。
    pub fn on_success(&mut self) -> bool {
        let was_failing = self.failures > 0;
        *self = Self::default();
        was_failing
    }

    pub fn delay_secs(&self, base: u64) -> u64 {
        base * (1u64 << self.failures.min(2))
    }

    pub fn should_alert(&self, now: NaiveDateTime, alert_after_secs: i64) -> bool {
        !self.alerted
            && self
                .first_failure
                .is_some_and(|f| (now - f).num_seconds() >= alert_after_secs)
    }

    pub fn mark_alerted(&mut self) {
        self.alerted = true;
    }

    pub fn failing_minutes(&self, now: NaiveDateTime) -> i64 {
        self.first_failure.map_or(0, |f| (now - f).num_minutes())
    }
}

pub fn due_daily(now: NaiveDateTime, hour: u32, minute: u32, last_run: Option<NaiveDate>) -> bool {
    let Some(at) = NaiveTime::from_hms_opt(hour, minute, 0) else {
        return false;
    };
    !is_weekend(now.date()) && now.time() >= at && last_run != Some(now.date())
}

/// 推送本轮新建的实盘工单,返回成功推送条数;单条失败只记日志。
pub fn notify_new_tickets(conn: &Connection, notifier: &dyn Notifier, ticket_ids: &[i64]) -> usize {
    let mut sent = 0;
    for &id in ticket_ids {
        let result = (|| -> Result<()> {
            let t = ticket::get_ticket(conn, id)?
                .ok_or_else(|| anyhow::anyhow!("工单 {id} 不存在"))?;
            let reason = signal_reason(conn, t.signal_id)?;
            let (title, md) = render_new_ticket(&t, &reason);
            notifier.notify(conn, t.user_id, &title, &md)
        })();
        match result {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 工单 {id} 推送失败: {e:#}"),
        }
    }
    sent
}

/// 按用户汇总已确认未回填的实盘工单并提醒,返回推送用户数。
pub fn send_fill_reminders(conn: &Connection, notifier: &dyn Notifier) -> Result<usize> {
    let mut by_user: BTreeMap<i64, Vec<_>> = BTreeMap::new();
    for t in ticket::list_unfilled_real(conn)? {
        by_user.entry(t.user_id).or_default().push(t);
    }
    let mut sent = 0;
    for (uid, tickets) in by_user {
        let (title, md) = render_fill_reminder(&tickets);
        match notifier.notify(conn, uid, &title, &md) {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 用户 {uid} 回填提醒失败: {e:#}"),
        }
    }
    Ok(sent)
}

/// 监听中断告警:推送给所有有持仓的用户,返回推送用户数。
pub fn alert_holders(conn: &Connection, notifier: &dyn Notifier, minutes: i64) -> Result<usize> {
    let (title, md) = render_monitor_down(minutes);
    let mut sent = 0;
    for uid in store::users_with_positions(conn)? {
        match notifier.notify(conn, uid, &title, &md) {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 用户 {uid} 中断告警失败: {e:#}"),
        }
    }
    Ok(sent)
}

/// 推送主循环把每轮实时异动交给监听线程。发送失败(线程已退出)静默忽略。
#[derive(Clone)]
pub struct MoverSink(Sender<Vec<Mover>>);

impl MoverSink {
    pub fn send(&self, movers: Vec<Mover>) {
        if !movers.is_empty() {
            let _ = self.0.send(movers);
        }
    }
}

pub fn spawn(
    db_path: PathBuf,
    cfg: TradeCfg,
    warn_days: i64,
    grace_days: i64,
) -> std::io::Result<(std::thread::JoinHandle<()>, MoverSink)> {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("trade-monitor".into())
        .spawn(move || run_loop(db_path, cfg, rx, warn_days, grace_days))?;
    Ok((handle, MoverSink(tx)))
}

fn run_loop(
    db_path: PathBuf,
    cfg: TradeCfg,
    rx: Receiver<Vec<Mover>>,
    warn_days: i64,
    grace_days: i64,
) {
    let mut conn = match crate::web::auth::store::open(&db_path)
        .and_then(|c| store::migrate(&c).map(|_| c))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[trade] 打开数据库失败,交易监听未启动: {e:#}");
            return;
        }
    };
    println!(
        "交易监听已启动(每 {} 秒,库 {})",
        cfg.monitor_interval_secs,
        db_path.display()
    );
    let notifier = PushNotifier {
        warn_days,
        grace_days,
    };
    let source = TencentQuotes;
    let mut backoff = Backoff::default();
    let mut last_cancel: Option<NaiveDate> = None;
    let mut last_remind: Option<NaiveDate> = None;

    loop {
        let now = chrono::Local::now().naive_local();
        if let Err(e) = store::beat(&conn, "trade-monitor", now) {
            eprintln!("[trade] 写心跳失败: {e:#}");
        }

        while let Ok(batch) = rx.try_recv() {
            if !cfg.mover_signals {
                continue;
            }
            match movers::submit_mover_signals(&mut conn, &batch, now) {
                Ok(r) => r
                    .errors
                    .iter()
                    .for_each(|e| eprintln!("[trade] 异动信号: {e}")),
                Err(e) => eprintln!("[trade] 异动信号处理失败: {e:#}"),
            }
        }

        if due_daily(now, 9, 0, last_cancel) {
            let midnight = now.date().and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("合法时刻"));
            match ticket::cancel_unfilled(&conn, midnight) {
                Ok(n) if n > 0 => println!("[trade] 撤销未回填工单 {n} 张"),
                Ok(_) => {}
                Err(e) => eprintln!("[trade] 撤销未回填工单失败: {e:#}"),
            }
            last_cancel = Some(now.date());
        }
        if due_daily(now, 15, 5, last_remind) {
            if let Err(e) = send_fill_reminders(&conn, &notifier) {
                eprintln!("[trade] 回填提醒失败: {e:#}");
            }
            last_remind = Some(now.date());
        }

        let delay = match monitor::run_tick(&mut conn, &source, now) {
            Ok(report) => {
                if backoff.on_success() {
                    println!("[trade] 行情恢复,监听继续");
                }
                notify_new_tickets(&conn, &notifier, &report.new_real_tickets);
                for e in report
                    .errors
                    .iter()
                    .chain(report.paper.errors.iter().map(|(_, e)| e))
                {
                    eprintln!("[trade] {e}");
                }
                cfg.monitor_interval_secs
            }
            Err(e) => {
                eprintln!("[trade] 本轮监听失败: {e:#}");
                backoff.on_failure(now);
                if backoff.should_alert(now, cfg.alert_after_secs) {
                    let minutes = backoff.failing_minutes(now);
                    if let Err(e) = alert_holders(&conn, &notifier, minutes) {
                        eprintln!("[trade] 中断告警失败: {e:#}");
                    }
                    backoff.mark_alerted();
                }
                backoff.delay_secs(cfg.monitor_interval_secs)
            }
        };
        std::thread::sleep(std::time::Duration::from_secs(delay));
    }
}
```

> 若 `crate::web::auth::store::open(...).and_then(...)` 因错误类型不一致无法编译,拆成两步 `match`,语义不变。

- [ ] **Step 4: 接线推送守护**

1. `src/push/mod.rs`:

```rust
/// 多用户 cron 守护。`trade` 为交易监听线程的异动接收端(未启用交易监听时为 None)。
pub fn run_multi_daemon(
    conn: &Connection,
    warn_days: i64,
    grace_days: i64,
    trade: Option<crate::trade::daemon::MoverSink>,
) -> anyhow::Result<()> {
    schedule::run_multi(conn, warn_days, grace_days, trade)
}
```

2. `src/push/schedule.rs`:
   - `pub fn run_multi(conn: &Connection, warn: i64, grace: i64) -> Result<()>` 增加末参数 `trade: Option<crate::trade::daemon::MoverSink>`;
   - 调用处改为 `realtime_tick(d, rc, conn, now, warn, grace, trade.as_ref())`;
   - `fn realtime_tick(...)` 增加末参数 `trade: Option<&crate::trade::daemon::MoverSink>`(若 clippy 报 `too_many_arguments`,在该函数上加 `#[allow(clippy::too_many_arguments)]`);
   - 在 `let Some(out) = d.tick(rc, naive)? else { return Ok(()); };` 之后立即加入:

```rust
    if let Some(sink) = trade {
        sink.send(out.movers.clone());
    }
```

3. `src/main.rs` Push 分支:在 `xlh::push::store::migrate_legacy_push(...)` 之后、`if once {` 之前加入:

```rust
            let trade_enabled = match xlh::trade::config::init(&cli.config) {
                Ok(c) => c.enabled,
                Err(e) => {
                    eprintln!("⚠ [trade] 配置无效,交易监听未启用:{e}");
                    false
                }
            };
```

并把 `else` 分支改为:

```rust
            } else {
                let sink = if trade_enabled {
                    match xlh::trade::daemon::spawn(
                        auth_cfg.db_path.clone(),
                        xlh::trade::config::get().clone(),
                        auth_cfg.warn_days,
                        auth_cfg.grace_days,
                    ) {
                        Ok((_handle, sink)) => Some(sink),
                        Err(e) => {
                            eprintln!("⚠ [trade] 交易监听线程启动失败:{e}");
                            None
                        }
                    }
                } else {
                    None
                };
                xlh::push::run_multi_daemon(&conn, auth_cfg.warn_days, auth_cfg.grace_days, sink)
            }
```

4. `config.toml` 末尾追加注释示例(不改变默认行为):

```toml
# [trade]
# enabled = true                # 推送守护内启动交易监听线程
# monitor_interval_secs = 15    # 交易时段报价轮询间隔(秒)
# alert_after_secs = 180        # 连续失败多久推送「止损监听中断」
# mover_signals = true          # 实时异动转为观察期(仅模拟盘)交易信号
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::daemon::tests`
Expected: 2 PASS

Run: `cargo test --test trade_runtime`
Expected: 3 PASS

Run: `cargo build`
Expected: 成功

- [ ] **Step 6: 全量门禁**

Run: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets --no-fail-fast`
Expected: fmt 干净;clippy 仅 3 个既有问题;测试除既有 `realtime_pipeline::full_day_flow_from_detection_to_summary` 外全部通过(lib、`trade_core`、`trade_runtime` 均需出现在输出中)

- [ ] **Step 7: Commit**

```bash
git add src/trade src/push/schedule.rs src/push/mod.rs src/main.rs config.toml tests/trade_runtime.rs
git commit -m "feat(trade): 交易监听线程、每日调度、推送分发并接入推送守护"
```

---

## 完成标准

- [ ] 基金测试期望值未改动
- [ ] `trade` 单元测试、`tests/trade_core.rs`、`tests/trade_runtime.rs` 全部通过;测试不访问网络
- [ ] CI 门禁符合 Global Constraints
- [ ] `xlh push` 启动时打印「交易监听已启动」;监听线程任何错误不影响原推送循环

## 后续计划(不在本计划范围)

- **计划 3 策略准入**:`trade_strategies` 等表、walk-forward、准入状态机、watchdog、成绩单;日线策略信号(收盘后计算、09:25 发出);`Admission` 真实来源(含异动策略);股票推荐迁移到 A 股口径;观察期模拟盘在实盘风控限额下的偏离;probation 同码叠加
- **计划 4 网页**:`/trade` 确认页、签名链接、持仓校准(含现有持仓导入)、风控与资金设置、监听心跳展示、按用户隔离的工单读取、SQL 端状态过滤、部分成交重复最低佣金、输入校验
- 已知限制:北交所代码暂无报价(不触发止盈止损);T+0 ETF 仍按 T+1 处理

## 执行后遗留项(来自任务审查与最终审查,须被后续计划吸收)

**计划 3:**
- 交易日历:每日调度(09:00 撤销、15:05 提醒)未识别工作日法定节假日
- 被拒止盈止损信号每 15 秒重新判定时覆盖 `created_at` / `reject_reason`,丢失首次拒绝时间;规则切换(移动止盈 → 止损)时 urgency 从 0 重新计
- 闸门拒绝原因优先级:同时满足冷却与未完结实盘工单时返回 Cooldown

**计划 4:**
- 推送发送线程串行:单个失效 webhook(约 55 秒/条)会延迟其他用户推送,考虑减少重试或按渠道熔断
- 推送 Markdown 未转义理由文本(手动 / AI 理由接入前必须处理)
- `fresh_quote` 把未来时间戳视为新鲜;网页「行情延迟」判断应取绝对值
- 监听中断告警文案在数据库错误时同样提示「行情获取失败」
- 停用 / 注销 / 授权过期用户仍会生成止盈止损与异动信号行(仅推送被过滤)
- `ensure_column` 与 `web/auth/store.rs` 重复,可抽公共迁移辅助
- 缺少测试:`realtime_tick` 转发异动、`run_loop` 接线、数据库重连循环

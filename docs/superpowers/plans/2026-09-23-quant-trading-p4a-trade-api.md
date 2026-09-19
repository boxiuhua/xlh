# 量化交易 · 计划 4a:交易 Web API、签名链接、风控与持仓校准 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把已有的交易核心(工单确认 / 忽略 / 回填、策略准入、成绩单、风控)通过 JSON API 暴露给网页,加上签名工单链接、管理员总开关、持仓校准与评估取消,并消化计划 3c/3e 的遗留项。页面本身在计划 4b。

**Architecture:** 新增 `src/web/trade.rs`,全部为 JSON 接口,挂在「需登录 + 授权」路由组(与 `holdings_history_routes` 同一方式,依赖 `AuthState` + `CurrentUser`);业务规则一律放在 `src/trade/` 下的纯逻辑模块,Web 层只做参数解析、加锁、调用、序列化。签名链接的密钥与管理员总开关存于新表 `trade_settings`(Web 进程与 `xlh push` 守护进程共用同一个 `data/xlh.db`,因此两边读到同一把密钥)。

**Tech Stack:** Rust 2021、axum 0.7、rusqlite 0.31、hmac 0.12 + sha2 0.10(已有依赖)、rand 0.8、serde_json。无新依赖。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §4、§6、§7、§8、§10.6、§12、§14(任务 9)。前置:计划 1–3e 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有查询按 `user_id` 隔离;跨用户视为不存在(API 返回 404,不泄露存在性)
- 时间 `NaiveDateTime` 本地(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换只走 `admission::state`(策略)与 `trade::ticket`(工单)的条件更新
- 阈值与开关一律来自配置并带范围校验
- 测试不得访问网络、不起真线程;Web 测试用内存库 + `tower::ServiceExt::oneshot`
- 不引入新依赖
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`
- API 错误统一为 JSON:`{"error": "<中文说明>", "code": "<机器可读码>"}`,状态码 400(参数)/ 404(不存在或跨用户)/ 409(状态冲突:已处理、行情延迟、价格偏离需确认、总开关关闭)

### 设计裁决(执行者照此实现)

1. **Web 层不含业务规则**。偏离保护、行情延迟、总开关、签名校验、持仓校准留痕都在 `src/trade/` 的函数里实现并单测;handler 只调用。这样 4b 的页面、签名链接页、以及将来的三期 agent 走同一条路径。
2. **偏离保护与行情延迟在服务端强制**(spec §8)。前端的「二次确认」只是把 `ack_deviation: true` 传上来;没有它、现价偏离超过工单的 `deviation_th` 就返回 409 `deviation`。行情(`trade_quotes`)时间戳距今超过 60 秒或不存在 → 409 `stale_quote`,不允许确认。
3. **签名链接**:`sig = hex(HMAC-SHA256(secret, "t:{ticket_id}:{user_id}:{expires_at}"))` 取前 32 个十六进制字符;密钥 32 字节随机数,首次使用时生成并存入 `trade_settings`(`INSERT OR IGNORE` 后再读,两进程并发首用也只会有一把)。链接只能**查看与确认**该工单(spec §8),有效期至工单过期;回填成交必须登录。`[trade] link_base_url` 为空时推送文案不带链接(保持现状)。
4. **管理员总开关**(`trade_settings.kill_switch`):打开后 `submit_signal` 对所有来源返回 `TradingDisabled`,确认接口返回 409 `kill_switch`;**回填成交不受限**(那是已经发生的事实,必须能记)。用户自己的 `RiskRules.enabled` 语义不变。
5. **持仓校准只作用于实盘账户**(模拟盘由系统撮合,不允许人工改)。每次校准在同一事务里写 `trade_position_adjusts`(改前 / 改后 JSON + 原因);数量改为 0 即删除该持仓行。
6. **策略卖出只卖策略自己买的**(计划 3e 遗留 F6)。绑定策略(`strategy_id` 为 `Some`)的卖出信号,每个账户的可卖量再与「该策略在该账户该代码上的净买入股数」(成交表按信号的 `strategy_id` 汇总:买入 − 卖出,下限 0)取小;非策略信号不受影响。
7. **评估取消**:排队中的任务直接结束为失败(`error = "用户取消"`);运行中的任务打上 `cancel_requested`,前推回测在处理下一只股票前检查并中止(计划 3c 遗留:`on_code` 恒返 true)。被取消的首次回测把策略 `Backtesting → Failed`(原因「用户取消前推回测」),不让它滞留。
8. **成绩单**:执行损耗保留**中位数**(更稳健),同步修改 spec §10.6 的措辞;`StageMetrics` 增加盈亏比 `profit_factor`(平均盈利 / 平均亏损绝对值,无亏损或无盈利时为 `None`)。样本内一栏直接用 `oos.is_sharpe`(已在 `PoolMetrics` 中),不另算。平均持仓天数需要逐笔配对建仓与平仓,当前成交模型没有批次概念,本计划不做(记入后续)。
9. **watchdog 的 `version_hash` 过滤不做**(计划 3c 遗留):定义变更会把策略打回草稿,重新准入必经「回测中 → 观察期 → 已准入」,而 watchdog 的统计窗口本就从**最近一次进入已准入**起算,旧版本的成交天然在窗口外。唯一例外是跨版本仍未完结的工单在重新准入后才成交,量级可忽略。
10. **`judge_watchdog` 列出全部触发项**(计划 3c 遗留),以「;」连接,暂停原因里用户能看到破得有多厉害。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/store.rs` | 修改 | 新表 `trade_settings`、`trade_position_adjusts`;`trade_eval_jobs.cancel_requested` 列;设置读写;持仓删除 |
| `src/trade/settings.rs` | 新建 | 总开关、签名密钥的读写 |
| `src/trade/link.rs` | 新建 | 工单签名与校验、链接拼装 |
| `src/trade/actions.rs` | 新建 | 面向用户的动作:带偏离 / 行情 / 总开关保护的确认,持仓校准,策略提交与评估取消 |
| `src/trade/model.rs` | 修改 | `RiskRules::validate` |
| `src/trade/service.rs` | 修改 | 总开关;策略卖出上限的输入 |
| `src/trade/gate.rs` | 修改 | `GateInput` 增加策略持股上限,`size_sell_for` 取小 |
| `src/trade/admission/stats.rs` | 修改 | `strategy_net_qty` |
| `src/trade/admission/judge.rs` | 修改 | `judge_watchdog` 返回全部触发项 |
| `src/trade/admission/scorecard.rs` | 修改 | `profit_factor` |
| `src/trade/admission/worker.rs` | 修改 | 前推回测检查取消标记;取消时收尾策略状态 |
| `src/trade/config.rs` | 修改 | `TradeCfg.link_base_url` |
| `src/trade/notify.rs`、`src/trade/daemon.rs` | 修改 | 新工单推送带签名链接 |
| `src/web/trade.rs` | 新建 | 交易 JSON API 与签名链接 API |
| `src/web/mod.rs` | 修改 | 路由接线 |
| `src/web/auth/routes.rs`、`src/web/auth/admin.rs` | 修改 | 管理员总开关接口 |
| `docs/superpowers/specs/2026-09-15-quant-trading-design.md` | 修改 | §10.6 执行损耗改为中位数 |
| `config.toml` | 修改 | `link_base_url` 样例 |

---

### Task 1: 判定层遗留:策略卖出上限、watchdog 全部原因、成绩单盈亏比

**Files:**
- Modify: `src/trade/admission/stats.rs`、`src/trade/gate.rs`、`src/trade/service.rs`、`src/trade/admission/judge.rs`、`src/trade/admission/scorecard.rs`、`docs/superpowers/specs/2026-09-15-quant-trading-design.md`
- Test: 各文件内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // stats.rs
  pub fn strategy_net_qty(conn: &Connection, user_id: i64, strategy_id: i64, account: Account, code: &str) -> Result<u64>;
  // gate.rs — GateInput 新增两个字段
  pub real_strategy_cap: Option<u64>,   // None = 不设上限(非策略信号或买入)
  pub paper_strategy_cap: Option<u64>,
  // judge.rs — 签名不变,返回值改为全部触发项以「;」连接
  pub fn judge_watchdog(...) -> Option<String>;
  // scorecard.rs — StageMetrics 新增
  pub profit_factor: Option<f64>,
  ```

- [ ] **Step 1: 写失败测试**

`stats.rs`(沿用该文件测试里构造成交的既有辅助函数;下为语义,须写成完整代码):

```rust
    #[test]
    fn strategy_net_qty_counts_only_this_strategys_fills_in_this_account() {
        // 策略 A 在实盘买 1000、卖 300;策略 B 在实盘买 500;手动信号在实盘买 200;
        // 策略 A 在模拟盘买 400。
        // 断言:strategy_net_qty(A, Real, code) == 700;(A, Paper) == 400;(B, Real) == 500;
        // 其它代码为 0;卖出超过买入(数据异常)时下限为 0。
    }
```

`gate.rs`(用该文件的 `Fx` 夹具):

```rust
    #[test]
    fn strategy_sell_is_capped_by_what_the_strategy_bought() {
        let mut f = Fx::sell(1000, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        f.sig.source = SignalSource::Strategy;
        f.real_strategy_cap = Some(300);
        f.paper_strategy_cap = Some(0);
        // 实盘只卖策略自己的 300 股;模拟盘策略没有持股 → 该账户无计划
        let plans = plans(f.run());
        assert_eq!(plans.len(), 1);
        assert_eq!((plans[0].account, plans[0].qty), (Account::Real, 300));

        let mut f = Fx::sell(1000, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        f.real_strategy_cap = Some(0);
        f.paper_strategy_cap = Some(0);
        assert_eq!(f.run(), GateDecision::Reject(GateReject::NothingSellable));

        let f = Fx::sell(1000, NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
        // 无上限(止盈止损 / 手动)行为不变
        assert_eq!(plans(f.run())[0].qty, 1000);
    }
```

> `Fx` 需要新增两个字段并在 `run()` 里传给 `GateInput`;`plans(..)` 是该测试模块已有的取出 `Pass` 计划的辅助函数(若名称不同照现状)。「实盘 Reject、模拟盘 Pass」时 `evaluate` 的既有语义是「第一个账户失败即整体拒绝」(`Err(reason) if i == 0`),所以第一例把上限 0 放在**模拟盘**(第二个账户),实盘有 300 股可卖。

`judge.rs`:

```rust
    #[test]
    fn watchdog_lists_every_breach() {
        // 构造同时突破回撤、胜率、连亏三项的 WatchdogStats(沿用本文件既有 watchdog 用例的构造方式)
        // 断言返回 Some(s) 且 s 同时包含「回撤」「胜率」「连亏」,以「;」分隔
    }
```

`scorecard.rs`:

```rust
    #[test]
    fn profit_factor_is_avg_win_over_avg_loss() {
        // 卖出盈亏 +300、+100、−100、−100 → 平均盈利 200 / 平均亏损 100 = 2.0
        // 全部盈利或全部亏损 → None
    }
```

> 按注释写出完整用例(沿用各文件既有夹具)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::`
Expected: 编译失败 / 新用例 FAIL

- [ ] **Step 3: 实现**

`stats.rs`:

```rust
/// 某策略在某账户某代码上的净买入股数(买入 − 卖出,下限 0)。策略卖出信号以此为上限,
/// 不能把用户手动买入、或其它策略买入的同一只股票一并卖掉(计划 4a 设计裁决 6)。
pub fn strategy_net_qty(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    account: Account,
    code: &str,
) -> Result<u64> {
    let net: i64 = conn.query_row(
        "SELECT COALESCE(SUM(CASE t.side WHEN 'buy' THEN f.qty ELSE -f.qty END), 0)
         FROM trade_fills f
         JOIN trade_tickets t ON t.id = f.ticket_id
         JOIN trade_signals s ON s.id = t.signal_id
         WHERE t.user_id = ?1 AND s.strategy_id = ?2 AND t.account = ?3 AND t.code = ?4",
        params![user_id, strategy_id, account.as_str(), code],
        |r| r.get(0),
    )?;
    Ok(net.max(0) as u64)
}
```

> 列名以 `store.rs` 的 `SCHEMA` 为准(`trade_fills` 的数量列、`trade_tickets.side` 的取值 `buy`/`sell`);若 `trade_fills` 自带 `account` 列也可直接用它。

`gate.rs`:`GateInput` 加两个字段(带中文注释);`evaluate` 的循环里按账户取对应上限传入 `size_sell_for(s, pos, today, cap)`:

```rust
fn size_sell_for(
    s: &NewSignal,
    pos: Option<&Position>,
    today: chrono::NaiveDate,
    cap: Option<u64>,
) -> Result<u64, GateReject> {
    let sellable = pos.map_or(0, |p| p.sellable(today)).min(cap.unwrap_or(u64::MAX));
    ...(其余不变)
}
```

修好 `gate.rs` 测试夹具与所有 `GateInput { .. }` 构造点(`grep -rn "GateInput {" src tests`),非策略场景传 `None`。

`service.rs::submit_signal`:在收集闸门输入处加

```rust
    // 绑定策略的卖出:每个账户只能卖该策略自己买入的部分(计划 4a 设计裁决 6)
    let cap = |account| -> Result<Option<u64>> {
        match (sig.side, sig.strategy_id) {
            (Direction::Sell, Some(sid)) => Ok(Some(crate::trade::admission::stats::strategy_net_qty(
                &tx, sig.user_id, sid, account, &sig.code,
            )?)),
            _ => Ok(None),
        }
    };
    let real_strategy_cap = cap(Account::Real)?;
    let paper_strategy_cap = cap(Account::Paper)?;
```

并传入 `GateInput`。补一个 `service.rs` 用例:绑定策略的卖出信号在「持仓 1000 股、其中该策略只买了 200 股」时,实盘工单数量为 200。

`judge.rs::judge_watchdog`:三个 `return Some(format!(..))` 改为 `reasons.push(format!(..))`,末尾 `(!reasons.is_empty()).then(|| reasons.join(";"))`。既有断言若比对完整字符串且只触发一项,不受影响。

`scorecard.rs`:`StageMetrics` 加 `pub profit_factor: Option<f64>`(`#[serde(default)]` 不需要——该结构只序列化不反序列化),`stage_metrics` 里:

```rust
    let wins: Vec<f64> = pnls.iter().copied().filter(|p| *p > 0.0).collect();
    let losses: Vec<f64> = pnls.iter().copied().filter(|p| *p < 0.0).collect();
    let profit_factor = (!wins.is_empty() && !losses.is_empty()).then(|| {
        (wins.iter().sum::<f64>() / wins.len() as f64)
            / (losses.iter().sum::<f64>().abs() / losses.len() as f64)
    });
```

spec §10.6「附加 **执行损耗** = … 的平均偏差」改为「… 的**中位数**偏差(中位数对个别极端成交更稳健)」。

- [ ] **Step 4: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade docs/superpowers/specs/2026-09-15-quant-trading-design.md
git commit -m "fix(trade): 策略卖出只卖策略自己的持股,watchdog 列出全部触发项,成绩单加盈亏比"
```

---

### Task 2: 设置表、管理员总开关与风控规则校验

**Files:**
- Create: `src/trade/settings.rs`
- Modify: `src/trade/store.rs`(`SCHEMA`)、`src/trade/model.rs`、`src/trade/service.rs`、`src/trade/mod.rs`
- Test: `settings.rs`、`model.rs`、`service.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // settings.rs
  pub fn kill_switch(conn: &Connection) -> Result<bool>;
  pub fn set_kill_switch(conn: &Connection, on: bool, now: NaiveDateTime) -> Result<()>;
  pub fn link_secret(conn: &Connection) -> Result<Vec<u8>>;   // 首次调用生成并持久化
  // model.rs
  impl RiskRules { pub fn validate(&self) -> Result<()>; }
  ```

- [ ] **Step 1: 建表**

`SCHEMA` 追加:

```sql
CREATE TABLE IF NOT EXISTS trade_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

(`store.rs` 里统计 `trade_%` 表数量的迁移测试相应 +1。)

- [ ] **Step 2: 写失败测试**

```rust
// settings.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::store;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }
    fn now() -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 23).unwrap().and_hms_opt(10, 0, 0).unwrap()
    }

    #[test]
    fn kill_switch_defaults_off_and_toggles() {
        let c = db();
        assert!(!kill_switch(&c).unwrap());
        set_kill_switch(&c, true, now()).unwrap();
        assert!(kill_switch(&c).unwrap());
        set_kill_switch(&c, false, now()).unwrap();
        assert!(!kill_switch(&c).unwrap());
    }

    #[test]
    fn link_secret_is_generated_once_and_stable() {
        let c = db();
        let a = link_secret(&c).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(link_secret(&c).unwrap(), a, "第二次读到同一把");
    }
}
```

`model.rs`:

```rust
    #[test]
    fn risk_rules_validation_rejects_out_of_range_values() {
        assert!(RiskRules::default().validate().is_ok());
        let bad = [
            RiskRules { max_order_amount: 0.0, ..RiskRules::default() },
            RiskRules { max_position_pct: 1.5, ..RiskRules::default() },
            RiskRules { max_position_pct: 0.0, ..RiskRules::default() },
            RiskRules { max_daily_tickets: 0, ..RiskRules::default() },
            RiskRules { daily_loss_halt_pct: 0.0, ..RiskRules::default() },
            RiskRules { daily_loss_halt_pct: 0.6, ..RiskRules::default() },
            RiskRules { cooldown_min: -1, ..RiskRules::default() },
            RiskRules { deviation_th: 0.0, ..RiskRules::default() },
            RiskRules { deviation_th: 0.2, ..RiskRules::default() },
            RiskRules { default_stop_loss_pct: 0.0, ..RiskRules::default() },
            RiskRules { default_stop_loss_pct: 0.6, ..RiskRules::default() },
            RiskRules { default_take_profit_pct: 0.0, ..RiskRules::default() },
            RiskRules { default_take_profit_pct: 5.1, ..RiskRules::default() },
            RiskRules { slippage: -0.01, ..RiskRules::default() },
            RiskRules { slippage: 0.06, ..RiskRules::default() },
            RiskRules { max_order_amount: f64::NAN, ..RiskRules::default() },
        ];
        for r in bad {
            assert!(r.validate().is_err(), "{r:?}");
        }
    }
```

`service.rs`:

```rust
    #[test]
    fn kill_switch_rejects_every_source() {
        let mut c = db();
        crate::trade::settings::set_kill_switch(&c, true, now()).unwrap();
        let q = quote();
        let ctx = SubmitContext { quote: Some(&q), now: now() };
        for (i, src) in [SignalSource::Exit, SignalSource::Manual, SignalSource::Mover].into_iter().enumerate() {
            let s = sig(src, None, &format!("k{i}"));
            assert!(matches!(
                submit_signal(&mut c, &s, &ctx).unwrap(),
                SubmitOutcome::Rejected { reason: GateReject::TradingDisabled, .. }
            ));
        }
    }
```

- [ ] **Step 3: 实现**

`settings.rs`:

```rust
//! 交易全局设置(存于 `trade_settings`):管理员总开关、工单签名密钥。
//! Web 进程与推送守护进程共用同一个库,因此两边读到的是同一份设置。

use crate::trade::model::fmt_ts;
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};

const KILL_SWITCH: &str = "kill_switch";
const LINK_SECRET: &str = "link_secret";

fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM trade_settings WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

fn put(conn: &Connection, key: &str, value: &str, now: NaiveDateTime) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_settings (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        params![key, value, fmt_ts(now)],
    )?;
    Ok(())
}

/// 管理员总开关:打开后不再生成任何工单、不允许确认(回填成交不受限)。
pub fn kill_switch(conn: &Connection) -> Result<bool> {
    Ok(get(conn, KILL_SWITCH)?.as_deref() == Some("1"))
}

pub fn set_kill_switch(conn: &Connection, on: bool, now: NaiveDateTime) -> Result<()> {
    put(conn, KILL_SWITCH, if on { "1" } else { "0" }, now)
}

/// 工单签名密钥(32 字节)。首次调用生成;`INSERT OR IGNORE` 后再读,
/// 两个进程同时首次调用也只会留下一把。
pub fn link_secret(conn: &Connection) -> Result<Vec<u8>> {
    if get(conn, LINK_SECRET)?.is_none() {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        conn.execute(
            "INSERT OR IGNORE INTO trade_settings (key, value, updated_at) VALUES (?1, ?2, ?3)",
            params![LINK_SECRET, hex(&buf), fmt_ts(chrono::Local::now().naive_local())],
        )?;
    }
    let s = get(conn, LINK_SECRET)?.ok_or_else(|| anyhow!("签名密钥写入失败"))?;
    unhex(&s)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 {
        return Err(anyhow!("签名密钥格式错误"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| anyhow!("签名密钥格式错误: {e}")))
        .collect()
}
```

`RiskRules::validate`(范围与测试一致;每条错误中文、带当前值):

| 字段 | 合法范围 |
|---|---|
| `max_order_amount` | 有限且 > 0 |
| `max_position_pct` | (0, 1] |
| `max_daily_tickets` | ≥ 1 |
| `daily_loss_halt_pct` | (0, 0.5] |
| `cooldown_min` | ≥ 0 |
| `deviation_th` | (0, 0.1] |
| `default_stop_loss_pct` | (0, 0.5] |
| `default_take_profit_pct` | (0, 5] |
| `slippage` | [0, 0.05] |

所有浮点字段先判 `is_finite()`。`store::save_risk_rules` 开头调用 `rules.validate()?`(拒绝落库非法规则)。

`service.rs::submit_signal`:在 `resolve_admission` 之后读 `settings::kill_switch(&tx)?`;为真时把传入 `gate::evaluate` 的 `rules` 换成 `RiskRules { enabled: false, ..rules.clone() }`(复用闸门既有的 `TradingDisabled` 分支,不新增拒绝码)。

- [ ] **Step 4: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade
git commit -m "feat(trade): 交易设置表、管理员总开关与风控规则范围校验"
```

---

### Task 3: 工单签名链接与推送带链接

**Files:**
- Create: `src/trade/link.rs`
- Modify: `src/trade/config.rs`、`config.toml`、`src/trade/notify.rs`、`src/trade/daemon.rs`、`src/trade/mod.rs`
- Test: `link.rs`、`notify.rs`、`config.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `settings::{link_secret, hex}`
- Produces:
  ```rust
  pub fn sign(secret: &[u8], t: &Ticket) -> String;               // 32 个十六进制字符
  pub fn verify(secret: &[u8], t: &Ticket, sig: &str, now: NaiveDateTime) -> bool;
  pub fn ticket_url(base: &str, secret: &[u8], t: &Ticket) -> String; // "{base}/trade/t/{id}?sig={sig}"
  // config.rs
  pub link_base_url: String   // TradeCfg,默认 ""
  // notify.rs
  pub fn render_new_ticket(t: &Ticket, reason: &str, link: Option<&str>) -> (String, String);
  ```

- [ ] **Step 1: 写失败测试**

```rust
// link.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::{Account, TicketStatus};
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap().and_hms_opt(h, m, 0).unwrap()
    }
    fn ticket() -> Ticket {
        Ticket {
            id: 42,
            user_id: 7,
            signal_id: 1,
            account: Account::Real,
            code: "600000".into(),
            side: Direction::Buy,
            suggest_price: 10.0,
            qty: 100,
            filled_qty: 0,
            expires_at: at(10, 30),
            deviation_th: 0.015,
            status: TicketStatus::Pending,
            urgency: 0,
            created_at: at(9, 26),
            confirmed_at: None,
            ignore_reason: None,
        }
    }
    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn signature_verifies_until_expiry_and_binds_ticket_user_and_expiry() {
        let t = ticket();
        let sig = sign(SECRET, &t);
        assert_eq!(sig.len(), 32);
        assert!(verify(SECRET, &t, &sig, at(10, 0)));
        assert!(!verify(SECRET, &t, &sig, at(10, 30)), "到期即失效");
        assert!(!verify(b"another-secret-another-secret-xx", &t, &sig, at(10, 0)));
        assert!(!verify(SECRET, &Ticket { id: 43, ..t.clone() }, &sig, at(10, 0)));
        assert!(!verify(SECRET, &Ticket { user_id: 8, ..t.clone() }, &sig, at(10, 0)));
        assert!(!verify(SECRET, &Ticket { expires_at: at(11, 0), ..t.clone() }, &sig, at(10, 0)));
        assert!(!verify(SECRET, &t, "", at(10, 0)));
        assert!(!verify(SECRET, &t, "zz", at(10, 0)));
    }

    #[test]
    fn url_joins_base_without_double_slash() {
        let t = ticket();
        let u = ticket_url("https://x.example.com/", SECRET, &t);
        assert!(u.starts_with("https://x.example.com/trade/t/42?sig="));
        assert!(u.ends_with(&sign(SECRET, &t)));
    }
}
```

`notify.rs`:

```rust
    #[test]
    fn new_ticket_message_carries_the_link_when_given() {
        // 用该文件既有用例里的 Ticket 构造方式
        // render_new_ticket(&t, "r", Some("https://x/trade/t/1?sig=ab")) 的 md 包含该链接;
        // render_new_ticket(&t, "r", None) 的 md 与改动前一致(含「请在「交易」页确认」)
    }
```

`config.rs`:`from_toml_str("[trade]\nlink_base_url = \"https://x\"")` 读到该值;`"ftp://x"` 与 `"x"` 被拒绝(只允许空、`http://` 或 `https://` 开头)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::link trade::notify trade::config`
Expected: 编译失败

- [ ] **Step 3: 实现**

```rust
//! 工单签名链接(spec §8):推送里的 `/trade/t/{id}?sig=` 只允许查看与确认该工单,
//! 有效期至工单过期。签名绑定工单号、用户与过期时刻,任何一项被改都失效。

use crate::trade::model::{fmt_ts, Ticket};
use chrono::NaiveDateTime;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 截取的十六进制长度:128 位,足以抵御在线猜测。
const SIG_HEX_LEN: usize = 32;

fn message(t: &Ticket) -> String {
    format!("t:{}:{}:{}", t.id, t.user_id, fmt_ts(t.expires_at))
}

fn mac(secret: &[u8], t: &Ticket) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC 接受任意长度密钥");
    m.update(message(t).as_bytes());
    m
}

pub fn sign(secret: &[u8], t: &Ticket) -> String {
    let full = crate::trade::settings::hex(&mac(secret, t).finalize().into_bytes());
    full[..SIG_HEX_LEN].to_string()
}

pub fn verify(secret: &[u8], t: &Ticket, sig: &str, now: NaiveDateTime) -> bool {
    if now >= t.expires_at || sig.len() != SIG_HEX_LEN {
        return false;
    }
    let Some(bytes) = (0..sig.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(sig.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()
    else {
        return false;
    };
    // 常量时间比较前缀,不给计时侧信道
    mac(secret, t).verify_truncated_left(&bytes).is_ok()
}

pub fn ticket_url(base: &str, secret: &[u8], t: &Ticket) -> String {
    format!("{}/trade/t/{}?sig={}", base.trim_end_matches('/'), t.id, sign(secret, t))
}
```

`config.rs`:`TradeCfg` 加

```rust
    /// 推送里工单链接的站点根地址(如 https://xlh.example.com);为空则不带链接
    pub link_base_url: String,
```

默认 `String::new()`;`from_toml_str` 校验:非空时必须以 `http://` 或 `https://` 开头。`config.toml` 的 `[trade]` 样例加一行注释:

```toml
# link_base_url = ""            # 推送工单链接的站点根地址(如 https://xlh.example.com),空则不带链接
```

`notify.rs::render_new_ticket` 增加 `link: Option<&str>` 参数:有链接时最后一行改为 `[查看并确认]({link}),在券商 App 下单后登录回填成交。`;无链接时保持原文。更新所有调用点。

`daemon.rs::notify_new_tickets` 增加参数 `link_base: &str`:非空时 `settings::link_secret(conn)` 取密钥(失败只记日志、降级为无链接),为每张工单生成 `ticket_url`。`run_loop` 里的两处调用传 `&cfg.link_base_url`。

- [ ] **Step 4: 运行确认通过 + 全量门禁 + Commit**

```bash
git add src/trade config.toml
git commit -m "feat(trade): 工单签名链接,新工单推送附带查看与确认链接"
```

---

### Task 4: 用户动作:受保护的确认、持仓校准、资金设置

**Files:**
- Create: `src/trade/actions.rs`
- Modify: `src/trade/store.rs`(`SCHEMA`、`delete_position`、`list_adjusts`)、`src/trade/mod.rs`
- Test: `actions.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `settings::kill_switch`;`ticket::{confirm, get_ticket}`;`store::{get_quote, get_position, upsert_position}`
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub enum ConfirmError { NotFound, AlreadyHandled, KillSwitch, StaleQuote, Deviation { price: f64, deviation: f64 } }
  pub const QUOTE_MAX_AGE_SECS: i64 = 60;
  pub fn confirm_ticket(conn: &Connection, user_id: i64, ticket_id: i64, ack_deviation: bool, now: NaiveDateTime)
      -> Result<std::result::Result<(), ConfirmError>>;
  pub struct Calibration { pub code: String, pub qty: u64, pub avg_cost: f64, pub reason: String }
  pub fn calibrate_position(conn: &mut Connection, user_id: i64, c: &Calibration, now: NaiveDateTime) -> Result<()>;
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct PositionAdjust { pub id: i64, pub code: String, pub before: Option<serde_json::Value>, pub after: Option<serde_json::Value>, pub reason: String, pub at: NaiveDateTime }
  // store.rs
  pub fn delete_position(conn: &Connection, user_id: i64, account: Account, code: &str) -> Result<bool>;
  pub fn list_adjusts(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<PositionAdjust>>;
  ```

`confirm_ticket` 规则(按序):

1. 工单不存在或不属于该用户 → `NotFound`
2. 总开关打开 → `KillSwitch`
3. 工单不是 `Pending`,或已到 `expires_at` → `AlreadyHandled`
4. 模拟盘工单 → `AlreadyHandled`(模拟盘无需人工确认)
5. `trade_quotes` 无该代码或 `now - ts > 60s` → `StaleQuote`
6. `|现价 / 建议价 − 1| > deviation_th` 且 `!ack_deviation` → `Deviation { price, deviation }`
7. 调 `ticket::confirm`;`AlreadyHandled` → `AlreadyHandled`(并发重复点击)

持仓校准规则:代码须为 6 位数字;`qty` 为 0 时删除实盘持仓;否则 `avg_cost` 须有限且 > 0;写 `trade_position_adjusts`(改前、改后以 `serde_json::to_value(&Position)` 存,没有则 NULL);新建持仓用 `Position::empty(user, Real, code)` 后覆盖数量与成本,保留原有止盈止损;`today_bought_qty = min(原值, 新数量)`;`reason` 去掉首尾空白后不得为空。整个过程一个 `IMMEDIATE` 事务。

- [ ] **Step 1: 建表**

```sql
CREATE TABLE IF NOT EXISTS trade_position_adjusts (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     INTEGER NOT NULL,
  account     TEXT NOT NULL,
  code        TEXT NOT NULL,
  before_json TEXT,
  after_json  TEXT,
  reason      TEXT NOT NULL,
  at          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_position_adjusts_user ON trade_position_adjusts(user_id, id);
```

(迁移测试的表数量 +1。`Position` 若未派生 `Serialize`,在 `model.rs` 补上。)

- [ ] **Step 2: 写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::{Account, AccountScope, NewSignal, Quote, SignalSource, TicketStatus};
    use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23).unwrap().and_hms_opt(h, m, s).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        store::set_capital(&c, 1, Account::Real, 100_000.0, at(9, 0, 0)).unwrap();
        c
    }
    fn quote(price: f64, ts: NaiveDateTime) -> Quote {
        Quote { code: "600000".into(), price, limit_up: Some(11.0), limit_down: Some(9.0), ts }
    }
    /// 手动买入信号 → 一张待确认实盘工单,返回工单号。
    fn pending_ticket(c: &mut Connection) -> i64 {
        let q = quote(10.0, at(10, 0, 0));
        store::upsert_quotes(c, &[q.clone()], at(10, 0, 0)).unwrap();
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600000".into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "t".into(),
            ai_note: None,
            dedup_key: "m1".into(),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        match submit_signal(c, &sig, &SubmitContext { quote: Some(&q), now: at(10, 0, 0) }).unwrap() {
            SubmitOutcome::Ticketed { real_ticket: Some(id), .. } => id,
            other => panic!("{other:?}"),
        }
    }
    fn status(c: &Connection, id: i64) -> TicketStatus {
        crate::trade::ticket::get_ticket(c, id).unwrap().unwrap().status
    }

    #[test]
    fn confirm_requires_fresh_quote_and_ack_for_deviation() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        // 行情 61 秒前 → 延迟
        assert_eq!(confirm_ticket(&c, 1, id, false, at(10, 1, 1)).unwrap(), Err(ConfirmError::StaleQuote));
        // 新鲜但偏离 3%(阈值 1.5%)→ 需确认
        store::upsert_quotes(&c, &[quote(10.3, at(10, 1, 0))], at(10, 1, 0)).unwrap();
        assert!(matches!(
            confirm_ticket(&c, 1, id, false, at(10, 1, 5)).unwrap(),
            Err(ConfirmError::Deviation { .. })
        ));
        assert_eq!(status(&c, id), TicketStatus::Pending);
        // 带 ack 通过
        assert_eq!(confirm_ticket(&c, 1, id, true, at(10, 1, 5)).unwrap(), Ok(()));
        assert_eq!(status(&c, id), TicketStatus::Confirmed);
        // 重复点击
        assert_eq!(confirm_ticket(&c, 1, id, true, at(10, 1, 6)).unwrap(), Err(ConfirmError::AlreadyHandled));
    }

    #[test]
    fn confirm_is_scoped_to_user_and_blocked_by_kill_switch() {
        let mut c = db();
        let id = pending_ticket(&mut c);
        assert_eq!(confirm_ticket(&c, 2, id, true, at(10, 0, 5)).unwrap(), Err(ConfirmError::NotFound));
        crate::trade::settings::set_kill_switch(&c, true, at(10, 0, 0)).unwrap();
        assert_eq!(confirm_ticket(&c, 1, id, true, at(10, 0, 5)).unwrap(), Err(ConfirmError::KillSwitch));
    }

    #[test]
    fn calibration_logs_before_and_after_and_zero_deletes() {
        let mut c = db();
        calibrate_position(
            &mut c,
            1,
            &Calibration { code: "600000".into(), qty: 1000, avg_cost: 10.5, reason: "券商对账".into() },
            at(15, 0, 0),
        )
        .unwrap();
        let p = store::get_position(&c, 1, Account::Real, "600000").unwrap().unwrap();
        assert_eq!((p.qty, p.avg_cost), (1000, 10.5));
        calibrate_position(
            &mut c,
            1,
            &Calibration { code: "600000".into(), qty: 0, avg_cost: 0.0, reason: "已清仓".into() },
            at(15, 1, 0),
        )
        .unwrap();
        assert!(store::get_position(&c, 1, Account::Real, "600000").unwrap().is_none());
        let log = store::list_adjusts(&c, 1, 10).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().any(|a| a.before.is_none() && a.after.is_some()));
        assert!(log.iter().any(|a| a.before.is_some() && a.after.is_none()));
        assert!(store::list_adjusts(&c, 2, 10).unwrap().is_empty(), "按用户隔离");
    }

    #[test]
    fn calibration_rejects_bad_input() {
        let mut c = db();
        for bad in [
            Calibration { code: "60000".into(), qty: 100, avg_cost: 10.0, reason: "x".into() },
            Calibration { code: "600000".into(), qty: 100, avg_cost: 0.0, reason: "x".into() },
            Calibration { code: "600000".into(), qty: 100, avg_cost: 10.0, reason: "  ".into() },
        ] {
            assert!(calibrate_position(&mut c, 1, &bad, at(15, 0, 0)).is_err());
        }
        assert!(store::list_adjusts(&c, 1, 10).unwrap().is_empty(), "失败不留痕");
    }
}
```

> `list_adjusts` 放在 `store.rs` 还是 `actions.rs` 由执行者按「表的读写都在 `store.rs`」的既有惯例决定;接口签名见上,`PositionAdjust` 定义在 `model.rs` 或 `store.rs` 均可,但须 `pub` 且派生 `Serialize`。

- [ ] **Step 3: 运行确认失败 → 实现 → 通过**

Run: `cargo test --lib trade::actions`

`confirm_ticket` 的偏离计算:`let dev = (q.price / t.suggest_price - 1.0).abs();`,`dev > t.deviation_th + 1e-12` 视为偏离。

- [ ] **Step 4: 全量门禁 + Commit**

```bash
git add src/trade
git commit -m "feat(trade): 带偏离与行情保护的工单确认、实盘持仓校准留痕"
```

---

### Task 5: 策略提交与评估取消

**Files:**
- Modify: `src/trade/actions.rs`、`src/trade/store.rs`、`src/trade/admission/worker.rs`、`src/trade/model.rs`(如需)
- Test: `actions.rs`、`worker.rs` 内 `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // actions.rs
  #[derive(Debug, Clone, PartialEq)]
  pub enum SubmitStrategyOutcome { Queued { job_id: i64 }, Paper, AlreadyHandled, NotFound }
  pub fn submit_strategy(conn: &Connection, user_id: i64, id: i64, now: NaiveDateTime) -> Result<SubmitStrategyOutcome>;
  #[derive(Debug, Clone, PartialEq)]
  pub enum CancelOutcome { Cancelled, Requested, NotCancellable, NotFound }
  pub fn cancel_job(conn: &Connection, user_id: i64, job_id: i64, now: NaiveDateTime) -> Result<CancelOutcome>;
  // store.rs
  pub fn job_cancel_requested(conn: &Connection, job_id: i64) -> Result<bool>;
  ```

规则:

- `submit_strategy`:策略不存在 → `NotFound`;`state::submit_for_backtest` 为 `AlreadyHandled` → `AlreadyHandled`;异动类(直接进观察期)→ `Paper`;其余 → `store::enqueue_eval(.., EvalKind::WalkForward, now)`,返回 `Queued { job_id }`(`enqueue_eval` 返回 `None` 即已有同类任务,取已有任务号,查询 `store::list_jobs` 中同策略、`WalkForward`、queued/running 的那条)。
- `cancel_job`:任务不存在或不属于用户 → `NotFound`;`queued` → 条件更新为 `failed`、`error = '用户取消'`、`finished_at = now`,若其策略仍是 `Backtesting` 则 `state::update_status(Backtesting → Failed, "用户取消前推回测")`,返回 `Cancelled`;`running` → 置 `cancel_requested = 1`,返回 `Requested`;其余 → `NotCancellable`。
- `trade_eval_jobs` 增加列 `cancel_requested INTEGER NOT NULL DEFAULT 0`(用既有的 `ensure_column`)。
- `worker.rs` 的 WalkForward 分支:`on_code` 闭包里读 `store::job_cancel_requested(conn, job_id)`(读失败按未取消处理并记日志),为真返回 `false`;`outcome.cancelled` 为真时,若策略是 `Backtesting` 则 `state::update_status(Backtesting → Failed, "用户取消前推回测")`,返回 `Ok("已取消")`。

> `on_code` 闭包已经可变借用 `conn` 写进度——读取取消标记与写进度在同一个闭包里用同一个 `conn`,不需要额外借用。

- [ ] **Step 1: 写失败测试**

```rust
// actions.rs 追加
    fn trend_strategy(c: &Connection) -> i64 {
        store::create_strategy(
            c,
            &crate::trade::model::NewStrategy {
                user_id: 1,
                name: "趋势".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]\nlong_window = [20]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 0, 0),
        )
        .unwrap()
    }

    #[test]
    fn submit_queues_walk_forward_once_and_cancel_fails_the_strategy() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } = submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap() else {
            panic!("应入队前推回测");
        };
        assert_eq!(submit_strategy(&c, 1, id, at(9, 2, 0)).unwrap(), SubmitStrategyOutcome::AlreadyHandled);
        assert_eq!(submit_strategy(&c, 2, id, at(9, 2, 0)).unwrap(), SubmitStrategyOutcome::NotFound);
        assert_eq!(cancel_job(&c, 2, job_id, at(9, 3, 0)).unwrap(), CancelOutcome::NotFound);
        assert_eq!(cancel_job(&c, 1, job_id, at(9, 3, 0)).unwrap(), CancelOutcome::Cancelled);
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(s.status, crate::trade::model::StrategyStatus::Failed, "不滞留回测中");
        assert_eq!(cancel_job(&c, 1, job_id, at(9, 4, 0)).unwrap(), CancelOutcome::NotCancellable);
    }

    #[test]
    fn cancelling_a_running_job_only_sets_the_flag() {
        let c = db();
        let id = trend_strategy(&c);
        let SubmitStrategyOutcome::Queued { job_id } = submit_strategy(&c, 1, id, at(9, 1, 0)).unwrap() else {
            panic!()
        };
        store::claim_next_job(&c, at(9, 1, 30)).unwrap().unwrap();
        assert_eq!(cancel_job(&c, 1, job_id, at(9, 2, 0)).unwrap(), CancelOutcome::Requested);
        assert!(store::job_cancel_requested(&c, job_id).unwrap());
    }
```

`worker.rs`(沿用该文件既有 WalkForward 用例的夹具):

```rust
    #[test]
    fn walk_forward_stops_at_the_next_code_when_cancel_is_requested() {
        // 两只股票的池;领取任务后先置 cancel_requested;run_job 的加载闭包计数
        // 断言:返回 Ok 且文本含「已取消」;加载闭包只被调用 1 次(处理完第一只后即中止);
        // 策略状态从 Backtesting 变为 Failed,原因含「用户取消」;未写入 oos 评估
    }
```

> 按注释写出完整用例。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src/trade
git commit -m "feat(trade): 策略提交入队与评估取消,取消的首次回测不滞留"
```

---

### Task 6: 交易 API(一):工单、信号与概览

**Files:**
- Create: `src/web/trade.rs`
- Modify: `src/web/mod.rs`(`pub mod trade;`,`licensed` 组 `.merge(trade::routes())`)
- Test: `src/web/trade.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `settings::kill_switch`;Task 4 `actions::{confirm_ticket, ConfirmError}`;`ticket::{list_tickets, ignore, record_fill}`;`store::{get_account, get_quote, get_risk_rules, last_beat}`
- Produces: `pub fn routes() -> Router<AuthState>`;`ApiError`(本模块私有即可,Task 7/8 在同一文件复用)

路由(全部 JSON,需登录 + 授权):

| 方法 路径 | 请求 | 响应 |
|---|---|---|
| GET `/api/trade/overview` | — | `{ accounts: { real: AccountState?, paper: AccountState? }, monitor_alive, eval_alive, kill_switch, trading_enabled }` |
| GET `/api/trade/tickets?view=pending\|working\|done` | — | `[TicketView]` |
| POST `/api/trade/tickets/:id/confirm` | `{ ack_deviation: bool }`(缺省 false) | `{ ok: true }` |
| POST `/api/trade/tickets/:id/ignore` | `{ reason: string }` | `{ ok: true }` |
| POST `/api/trade/tickets/:id/fill` | `{ price: f64, qty: u64 }` | `{ ok, status, fee, realized_pnl }` |
| GET `/api/trade/signals/rejected?limit=N` | — | `[{ id, source, code, side, reason, reject_reason, created_at }]`(默认 50,上限 200) |

- `view`:`pending` = 实盘 `Pending`;`working` = 实盘 `Confirmed | Partial`;`done` = 其余(实盘 + 模拟盘,按 id 倒序取最近 100)。缺省 `pending`;其它值 400。
- `TicketView`:工单全部字段 + `source`(来自信号)+ `reason`(`notify::signal_reason`)+ `quote: { price, ts, stale }?`(`stale` = 距今 > `actions::QUOTE_MAX_AGE_SECS`)+ `deviation: f64?`。
- 心跳存活:`last_beat(name)` 距今 ≤ 120 秒(`trade-monitor`)/ ≤ 300 秒(`trade-eval`,空闲轮询 30 秒 + 慢任务)。
- 确认错误映射:`NotFound` 404 `not_found`;`AlreadyHandled` 409 `already_handled`;`KillSwitch` 409 `kill_switch`;`StaleQuote` 409 `stale_quote`;`Deviation` 409 `deviation`,响应里附 `price`、`deviation`。
- 忽略:`reason` 去空白后为空 → 400;`Transition::AlreadyHandled` → 409。
- 回填:`record_fill(.., source = "manual", now)` 的 `Err` → 400(其文案已是中文校验信息);工单不存在 / 跨用户同样来自 `record_fill` 的「工单不存在」→ 这里先用 `get_ticket` 判归属,跨用户 404。
- `rejected` 信号查询写在 `store.rs`:`list_rejected_signals(conn, user_id, limit) -> Result<Vec<RejectedSignal>>`(`RejectedSignal` 派生 `Serialize`)。

`ApiError`:

```rust
pub(crate) struct ApiError {
    status: StatusCode,
    code: &'static str,
    msg: String,
    extra: Option<serde_json::Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, msg: impl Into<String>) -> Self {
        Self { status, code, msg: msg.into(), extra: None }
    }
    fn bad(msg: impl Into<String>) -> Self { Self::new(StatusCode::BAD_REQUEST, "bad_request", msg) }
    fn not_found() -> Self { Self::new(StatusCode::NOT_FOUND, "not_found", "不存在") }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.msg, "code": self.code });
        if let (Some(extra), Some(obj)) = (self.extra, body.as_object_mut()) {
            if let Some(e) = extra.as_object() {
                obj.extend(e.clone());
            }
        }
        (self.status, axum::Json(body)).into_response()
    }
}

/// 内部错误(数据库等):500,不把细节暴露给前端,只记日志。
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        eprintln!("[trade-api] {e:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "服务内部错误")
    }
}
```

Handler 写法(与既有 `push_config_get` 一致):`State(st): State<AuthState>`、`Extension(user): Extension<CurrentUser>`,在 `st.db.lock()` 的作用域内完成读写后释放锁;需要 `&mut Connection` 的(`record_fill`)用 `let mut conn = st.db.lock().unwrap();` 后 `&mut conn`。时刻统一 `chrono::Local::now().naive_local()`。

- [ ] **Step 1: 写失败测试**

在 `src/web/trade.rs` 的 `mod tests` 里自建测试夹具(复制 `web/mod.rs` 测试里 `push_state` / `seed_licensed` 的做法,不能跨模块引用其私有函数):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state() -> AuthState {
        let conn = crate::web::auth::store::open_in_memory().unwrap();
        crate::history::migrate(&conn).unwrap();
        crate::push::store::migrate(&conn).unwrap();
        crate::trade::store::migrate(&conn).unwrap();
        AuthState::new(conn, Default::default())
    }

    fn seed_user(st: &AuthState, name: &str, token: &str) -> i64 {
        let c = st.db.lock().unwrap();
        let uid = crate::web::auth::store::create_user(&c, name, "h", false).unwrap();
        let today = chrono::Local::now().date_naive();
        crate::web::auth::store::set_expiry(&c, uid, today + chrono::Duration::days(30)).unwrap();
        crate::web::auth::store::create_session(&c, token, uid, today + chrono::Duration::days(1)).unwrap();
        uid
    }

    async fn call(st: &AuthState, method: &str, uri: &str, token: &str, body: Option<serde_json::Value>)
        -> (StatusCode, serde_json::Value)
    {
        let mut req = Request::builder().method(method).uri(uri).header("cookie", format!("xlh_session={token}"));
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = crate::web::router(st.clone()).oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    /// 给用户造一张待确认的实盘手动买入工单(建议价 10.0),再把行情改成 `price_now`。
    /// handler 用真实时钟,所以这里的时间也取「现在」,行情才算新鲜。
    fn pending_ticket(st: &AuthState, uid: i64, code: &str, price_now: f64) -> i64 {
        use crate::event::Direction;
        use crate::trade::model::{Account, AccountScope, NewSignal, Quote, SignalSource};
        use crate::trade::service::{submit_signal, SubmitContext, SubmitOutcome};
        let now = chrono::Local::now().naive_local();
        let mut c = st.db.lock().unwrap();
        if crate::trade::store::get_account(&c, uid, Account::Real).unwrap().is_none() {
            crate::trade::store::set_capital(&c, uid, Account::Real, 100_000.0, now).unwrap();
        }
        let q = |price: f64| Quote {
            code: code.into(),
            price,
            limit_up: Some(11.0),
            limit_down: Some(9.0),
            ts: now,
        };
        let sig = NewSignal {
            user_id: uid,
            source: SignalSource::Manual,
            strategy_id: None,
            code: code.into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::RealOnly,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: format!("web-test-{uid}-{code}"),
            suggest_cash: Some(5_000.0),
            suggest_qty: None,
        };
        let id = match submit_signal(&mut c, &sig, &SubmitContext { quote: Some(&q(10.0)), now }).unwrap() {
            SubmitOutcome::Ticketed { real_ticket: Some(id), .. } => id,
            other => panic!("{other:?}"),
        };
        crate::trade::store::upsert_quotes(&c, &[q(price_now)], now).unwrap();
        id
    }
}
```

> 若 `AuthState` 未实现 `Clone`,按 `web/mod.rs` 既有测试的做法改为每次构造路由时传值(例如让 `call` 接收 `AuthState` 并由调用方 `st.clone()`;`AuthState` 内部是 `Arc`,派生 `Clone` 即可)。

用例:

```rust
    #[tokio::test]
    async fn pending_list_confirm_with_deviation_ack_then_fill() {
        let st = state();
        let uid = seed_user(&st, "u1", "t1");
        let id = pending_ticket(&st, uid, "600000", 10.3); // 偏离 3%
        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=pending", "t1", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert!(list[0]["deviation"].as_f64().unwrap() > 0.029);
        assert_eq!(list[0]["quote"]["stale"], false);

        let url = format!("/api/trade/tickets/{id}/confirm");
        let (s, e) = call(&st, "POST", &url, "t1", Some(serde_json::json!({}))).await;
        assert_eq!((s, e["code"].as_str()), (StatusCode::CONFLICT, Some("deviation")));
        let (s, _) = call(&st, "POST", &url, "t1", Some(serde_json::json!({"ack_deviation": true}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, e) = call(&st, "POST", &url, "t1", Some(serde_json::json!({"ack_deviation": true}))).await;
        assert_eq!((s, e["code"].as_str()), (StatusCode::CONFLICT, Some("already_handled")));

        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=working", "t1", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 1));
        let qty = list[0]["qty"].as_u64().unwrap();
        let (s, r) = call(&st, "POST", &format!("/api/trade/tickets/{id}/fill"), "t1",
            Some(serde_json::json!({"price": 10.3, "qty": qty}))).await;
        assert_eq!(s, StatusCode::OK, "{r}");
        assert_eq!(r["status"], "filled");
        let (s, _) = call(&st, "POST", &format!("/api/trade/tickets/{id}/fill"), "t1",
            Some(serde_json::json!({"price": 10.3, "qty": 1}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "已成交不可再回填");
    }

    #[tokio::test]
    async fn other_users_ticket_is_not_found_and_anonymous_is_rejected() {
        let st = state();
        let a = seed_user(&st, "a", "ta");
        seed_user(&st, "b", "tb");
        let id = pending_ticket(&st, a, "600000", 10.0);
        let (s, _) = call(&st, "POST", &format!("/api/trade/tickets/{id}/confirm"), "tb",
            Some(serde_json::json!({"ack_deviation": true}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(&st, "POST", &format!("/api/trade/tickets/{id}/ignore"), "tb",
            Some(serde_json::json!({"reason": "x"}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, list) = call(&st, "GET", "/api/trade/tickets?view=pending", "tb", None).await;
        assert_eq!((s, list.as_array().unwrap().len()), (StatusCode::OK, 0));
        let (s, _) = call(&st, "GET", "/api/trade/tickets", "no-such-token", None).await;
        assert_ne!(s, StatusCode::OK, "未登录不可访问");
    }

    #[tokio::test]
    async fn ignore_requires_a_reason_and_bad_view_is_400() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        let url = format!("/api/trade/tickets/{id}/ignore");
        let (s, _) = call(&st, "POST", &url, "t", Some(serde_json::json!({"reason": "  "}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(&st, "POST", &url, "t", Some(serde_json::json!({"reason": "不看好"}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&st, "GET", "/api/trade/tickets?view=nope", "t", None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn overview_reports_kill_switch_and_heartbeats() {
        let st = state();
        seed_user(&st, "u", "t");
        {
            let c = st.db.lock().unwrap();
            crate::trade::settings::set_kill_switch(&c, true, chrono::Local::now().naive_local()).unwrap();
            crate::trade::store::beat(&c, "trade-monitor", chrono::Local::now().naive_local()).unwrap();
        }
        let (s, o) = call(&st, "GET", "/api/trade/overview", "t", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(o["kill_switch"], true);
        assert_eq!(o["monitor_alive"], true);
        assert_eq!(o["eval_alive"], false);
    }

    #[tokio::test]
    async fn rejected_signals_are_listed_per_user() {
        // 总开关打开后提交一个手动信号(被拒 trading_disabled),
        // GET /api/trade/signals/rejected 返回 1 条且 reject_reason == "trading_disabled";另一用户为空
    }
```

> 最后一个用例按注释写全。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src/web src/trade
git commit -m "feat(web): 交易 API —— 工单列表、确认(偏离与行情保护)、忽略、回填、被拦信号与概览"
```

---

### Task 7: 交易 API(二):资金、风控、持仓与校准

**Files:**
- Modify: `src/web/trade.rs`
- Test: `src/web/trade.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 2 `RiskRules::validate`;Task 4 `actions::{calibrate_position, Calibration}`、`store::list_adjusts`;`store::{set_capital, get_risk_rules, save_risk_rules, list_positions, set_exit_levels, get_quote}`

| 方法 路径 | 请求 | 响应 |
|---|---|---|
| GET `/api/trade/risk` | — | `RiskRules` |
| POST `/api/trade/risk` | `RiskRules`(完整对象) | `{ ok: true }`;校验失败 400 |
| POST `/api/trade/capital` | `{ account: "real"\|"paper", total: f64 }` | `{ ok: true }` |
| GET `/api/trade/positions` | — | `{ real: [PositionView], paper: [PositionView] }` |
| POST `/api/trade/positions/exit-levels` | `{ account, code, stop_loss?, take_profit?, trailing_pct? }` | `{ ok: true }`;无此持仓 404 |
| POST `/api/trade/positions/calibrate` | `{ code, qty, avg_cost, reason }` | `{ ok: true }` |
| GET `/api/trade/positions/adjusts` | — | `[PositionAdjust]`(最近 100 条) |

- `PositionView` = `Position` 全部字段 + `sellable`(`p.sellable(today)`)+ `quote: { price, ts, stale }?` + `market_value?` + `pnl_pct?`(有报价时)。
- 止盈止损校验(写在 `actions.rs`,`pub fn validate_exit_levels(stop_loss, take_profit, trailing_pct) -> Result<()>` 并单测):价格须有限且 > 0;`stop_loss < take_profit`(两者都有时);`trailing_pct` ∈ (0, 0.5]。
- `RiskRules` 需派生 `Deserialize`(若尚未)。

- [ ] **Step 1: 写失败测试**(沿用 Task 6 的夹具)

```rust
    #[tokio::test]
    async fn risk_rules_roundtrip_and_validation() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, mut r) = call(&st, "GET", "/api/trade/risk", "t", None).await;
        assert_eq!(s, StatusCode::OK);
        r["max_order_amount"] = serde_json::json!(20000.0);
        let (s, _) = call(&st, "POST", "/api/trade/risk", "t", Some(r.clone())).await;
        assert_eq!(s, StatusCode::OK);
        let (_, back) = call(&st, "GET", "/api/trade/risk", "t", None).await;
        assert_eq!(back["max_order_amount"], 20000.0);
        r["max_position_pct"] = serde_json::json!(2.0);
        let (s, e) = call(&st, "POST", "/api/trade/risk", "t", Some(r)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    }

    #[tokio::test]
    async fn capital_positions_calibration_and_exit_levels() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, _) = call(&st, "POST", "/api/trade/capital", "t",
            Some(serde_json::json!({"account": "real", "total": 200000.0}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&st, "POST", "/api/trade/positions/calibrate", "t",
            Some(serde_json::json!({"code": "600000", "qty": 1000, "avg_cost": 10.0, "reason": "对账"}))).await;
        assert_eq!(s, StatusCode::OK);
        let (_, p) = call(&st, "GET", "/api/trade/positions", "t", None).await;
        assert_eq!(p["real"].as_array().unwrap().len(), 1);
        assert_eq!(p["real"][0]["qty"], 1000);
        let (s, _) = call(&st, "POST", "/api/trade/positions/exit-levels", "t",
            Some(serde_json::json!({"account": "real", "code": "600000", "stop_loss": 9.0, "take_profit": 12.0}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(&st, "POST", "/api/trade/positions/exit-levels", "t",
            Some(serde_json::json!({"account": "real", "code": "600000", "stop_loss": 12.0, "take_profit": 9.0}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "止损须低于止盈");
        let (s, _) = call(&st, "POST", "/api/trade/positions/exit-levels", "t",
            Some(serde_json::json!({"account": "real", "code": "000001", "stop_loss": 9.0}))).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, log) = call(&st, "GET", "/api/trade/positions/adjusts", "t", None).await;
        assert_eq!(log.as_array().unwrap().len(), 1);
        let (s, _) = call(&st, "POST", "/api/trade/capital", "t",
            Some(serde_json::json!({"account": "real", "total": -1.0}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src/web src/trade
git commit -m "feat(web): 交易 API —— 资金、风控设置、持仓、止盈止损与持仓校准"
```

---

### Task 8: 交易 API(三):策略、成绩单、评估任务与管理员总开关

**Files:**
- Modify: `src/web/trade.rs`、`src/web/auth/routes.rs`、`src/web/auth/admin.rs`
- Test: `src/web/trade.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 5 `actions::{submit_strategy, cancel_job}`;`store::{create_strategy, list_strategies, get_strategy, update_definition, list_status_events, list_jobs}`;`admission::scorecard::scorecard`;Task 2 `settings::{kill_switch, set_kill_switch}`

| 方法 路径 | 请求 | 响应 |
|---|---|---|
| GET `/api/trade/strategies` | — | `[StrategyDef]` |
| POST `/api/trade/strategies` | `{ name, kind, grid_toml, pool: [code] }` | `{ id }`;校验失败 400 |
| POST `/api/trade/strategies/:id` | 同上 | `{ result: "unchanged"\|"renamed"\|"reversioned" }`;不存在 404 |
| POST `/api/trade/strategies/:id/submit` | — | `{ result: "queued", job_id }` / `{ result: "paper" }`;`AlreadyHandled` 409 |
| GET `/api/trade/strategies/:id/scorecard` | — | `Scorecard`;不存在 404 |
| GET `/api/trade/strategies/:id/events` | — | 状态事件列表 |
| GET `/api/trade/jobs` | — | 最近 50 个 `EvalJob` |
| POST `/api/trade/jobs/:id/cancel` | — | `{ result: "cancelled"\|"requested" }`;`NotCancellable` 409;`NotFound` 404 |
| GET `/api/admin/trade/kill-switch` | — | `{ on: bool }`(管理员) |
| POST `/api/admin/trade/kill-switch` | `{ on: bool }` | `{ ok: true }`(管理员) |

- `StrategyDef`、`EvalJob`、状态事件类型若未派生 `Serialize`,补上。
- 管理员路由加到 `auth::routes::admin_router()`,handler 写在 `admin.rs`,与该文件其它 handler 同样拿 `AuthState`。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn strategy_lifecycle_create_submit_cancel_and_scorecard() {
        let st = state();
        seed_user(&st, "u", "t");
        seed_user(&st, "v", "tv");
        let body = serde_json::json!({
            "name": "趋势", "kind": "trend",
            "grid_toml": "short_window = [5]\nlong_window = [20]", "pool": ["600000"]
        });
        let (s, r) = call(&st, "POST", "/api/trade/strategies", "t", Some(body.clone())).await;
        assert_eq!(s, StatusCode::OK, "{r}");
        let id = r["id"].as_i64().unwrap();
        let (s, _) = call(&st, "POST", "/api/trade/strategies", "t",
            Some(serde_json::json!({"name": "x", "kind": "nope", "grid_toml": "a = [1]", "pool": ["600000"]}))).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);

        let (s, r) = call(&st, "POST", &format!("/api/trade/strategies/{id}/submit"), "t", None).await;
        assert_eq!((s, r["result"].as_str()), (StatusCode::OK, Some("queued")));
        let job = r["job_id"].as_i64().unwrap();
        let (s, _) = call(&st, "POST", &format!("/api/trade/strategies/{id}/submit"), "t", None).await;
        assert_eq!(s, StatusCode::CONFLICT);

        let (_, jobs) = call(&st, "GET", "/api/trade/jobs", "t", None).await;
        assert_eq!(jobs.as_array().unwrap().len(), 1);
        let (_, jobs_v) = call(&st, "GET", "/api/trade/jobs", "tv", None).await;
        assert!(jobs_v.as_array().unwrap().is_empty(), "按用户隔离");
        let (s, _) = call(&st, "POST", &format!("/api/trade/jobs/{job}/cancel"), "tv", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, r) = call(&st, "POST", &format!("/api/trade/jobs/{job}/cancel"), "t", None).await;
        assert_eq!((s, r["result"].as_str()), (StatusCode::OK, Some("cancelled")));

        let (s, sc) = call(&st, "GET", &format!("/api/trade/strategies/{id}/scorecard"), "t", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(sc["status"], "failed");
        let (s, _) = call(&st, "GET", &format!("/api/trade/strategies/{id}/scorecard"), "tv", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (_, ev) = call(&st, "GET", &format!("/api/trade/strategies/{id}/events"), "t", None).await;
        assert!(ev.as_array().unwrap().len() >= 2);

        let mut renamed = body.clone();
        renamed["name"] = serde_json::json!("趋势2");
        let (_, r) = call(&st, "POST", &format!("/api/trade/strategies/{id}"), "t", Some(renamed)).await;
        assert_eq!(r["result"], "renamed");
    }

    #[tokio::test]
    async fn kill_switch_admin_only() {
        let st = state();
        seed_user(&st, "u", "t");
        let (s, _) = call(&st, "POST", "/api/admin/trade/kill-switch", "t",
            Some(serde_json::json!({"on": true}))).await;
        assert_ne!(s, StatusCode::OK, "普通用户不可操作");
        // 造管理员:create_user(.., is_admin = true) + 授权 + 会话(照 seed_user 写一个 seed_admin)
        // 管理员 POST {"on": true} → 200;GET → {"on": true};普通用户 overview 的 kill_switch 为 true
    }
```

> 第二个用例按注释写全。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src/web src/trade
git commit -m "feat(web): 交易 API —— 策略管理、成绩单、评估任务取消与管理员总开关"
```

---

### Task 9: 签名链接 API

**Files:**
- Modify: `src/web/trade.rs`、`src/web/mod.rs`(`public` 组 `.merge(trade::public_routes())`)
- Test: `src/web/trade.rs` 内 `mod tests`

**Interfaces:**
- Consumes: Task 3 `link::{verify, sign}`、`settings::link_secret`;Task 4 `actions::confirm_ticket`
- Produces: `pub fn public_routes() -> Router<AuthState>`

| 方法 路径 | 请求 | 响应 |
|---|---|---|
| GET `/api/trade/t/:id?sig=` | — | `TicketView`(同 Task 6) |
| POST `/api/trade/t/:id/confirm?sig=` | `{ ack_deviation }` | 同 Task 6 的确认 |

- 不需要登录;以工单自身的 `user_id` 作为操作用户。
- 工单不存在、签名缺失 / 错误 / 过期 → 一律 404 `not_found`(不区分原因,不泄露工单是否存在)。
- 页面(`GET /trade/t/:id`)在计划 4b 实现。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn signed_link_views_and_confirms_only_its_own_ticket() {
        let st = state();
        let uid = seed_user(&st, "u", "t");
        let id = pending_ticket(&st, uid, "600000", 10.0);
        let other = pending_ticket(&st, uid, "600036", 10.0); // 同用户另一张
        let (sig, other_sig) = {
            let c = st.db.lock().unwrap();
            let secret = crate::trade::settings::link_secret(&c).unwrap();
            let t = crate::trade::ticket::get_ticket(&c, id).unwrap().unwrap();
            let o = crate::trade::ticket::get_ticket(&c, other).unwrap().unwrap();
            (crate::trade::link::sign(&secret, &t), crate::trade::link::sign(&secret, &o))
        };
        // 不带 cookie
        let (s, v) = call(&st, "GET", &format!("/api/trade/t/{id}?sig={sig}"), "", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["id"], id);
        let (s, _) = call(&st, "GET", &format!("/api/trade/t/{id}?sig={other_sig}"), "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "别的工单的签名不能用");
        let (s, _) = call(&st, "GET", &format!("/api/trade/t/{id}"), "", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _) = call(&st, "POST", &format!("/api/trade/t/{id}/confirm?sig={sig}"), "",
            Some(serde_json::json!({"ack_deviation": false}))).await;
        assert_eq!(s, StatusCode::OK);
        let (s, e) = call(&st, "POST", &format!("/api/trade/t/{id}/confirm?sig={sig}"), "",
            Some(serde_json::json!({"ack_deviation": false}))).await;
        assert_eq!((s, e["code"].as_str()), (StatusCode::CONFLICT, Some("already_handled")), "链接重放");
    }
```

> `call` 在 `token` 为空时不要设置 cookie 头(Task 6 的 `call` 需要相应调整)。

- [ ] **Step 2–4: 运行确认失败 → 实现 → 通过 → 全量门禁 → Commit**

```bash
git add src/web
git commit -m "feat(web): 工单签名链接 API(免登录查看与确认,签名绑定工单、用户与有效期)"
```

---

## 完成标准

- [ ] 基金与既有测试期望值未改动
- [ ] 新增单元测试与 Web 测试通过;测试不访问网络、不起真线程
- [ ] CI 门禁符合 Global Constraints

## 遗留项对照

| 遗留项 | 处理 |
|---|---|
| 3e F6:策略卖出可能卖掉手动持仓 | Task 1:按策略净买入股数封顶 |
| 3c:`judge_watchdog` 只记首条原因 | Task 1 |
| 3c:成绩单四栏与缺行、执行损耗均值 / 中位数 | Task 1:加盈亏比、定案中位数并改 spec;四栏展示在计划 4b;平均持仓天数不做(设计裁决 8) |
| 3c:`on_code` 恒返 true(取消是死代码) | Task 5 |
| 3c:watchdog 未按 `version_hash` 过滤 | 不做(设计裁决 9) |

## 后续计划

- **计划 4b**:`/trade` 页面(待确认 / 待成交 / 已完成 / 被拦截的信号 / 策略 / 风控设置 / 持仓校准 七个标签,15 秒刷新行情与心跳,偏离二次确认、行情延迟禁用)、签名链接落地页 `/trade/t/:id`、管理后台总开关按钮
- 平均持仓天数与逐批配对(需要成交批次模型)

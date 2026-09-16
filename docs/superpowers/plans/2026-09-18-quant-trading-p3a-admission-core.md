# 量化交易 · 计划 3a:策略准入内核(策略定义、前推回测、准入判定)Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让「策略」成为系统里的一等对象:可定义(类型 + 参数网格 + 股票池)、可用滚动前推回测评估(训练窗选参、检验窗实测、拼接样本外)、可按阈值判定准入,并以状态机记录 草稿 → 回测中 → 观察期 → 已准入 / 未通过 / 已暂停。

**Architecture:** 纯函数与 IO 分离:`admission::walk_forward` 只接收 K 线切片与参数网格,不碰网络与数据库;`admission::judge` 是阈值比较的纯函数;`store` 负责策略定义与评估结果落库;状态机用条件 UPDATE + 事件表记录。后台评估线程、watchdog、成绩单、日线策略信号与股票推荐迁移在计划 3b。

**Tech Stack:** Rust 2021、rusqlite 0.31、toml(参数网格)、serde_json(评估指标)、sha2(版本哈希)、既有回测引擎与 `AShareExecution`。无新依赖。

**Spec:** `docs/superpowers/specs/2026-09-15-quant-trading-design.md` §10(策略准入:10.1 定义与版本、10.2 状态机、10.3 前推回测、10.4 阈值)。前置:计划 1、2a、2b 已合并入 main。

## Global Constraints

- 基金回测结果逐位不变;不得修改基金测试期望值
- 所有策略数据按 `user_id` 隔离;跨用户读写视为不存在
- 时间:`NaiveDateTime` 本地时间(`%Y-%m-%d %H:%M:%S`),日期 `%Y-%m-%d`
- 状态转换一律条件 UPDATE(`WHERE id = ? AND user_id = ? AND status = ?`),影响 0 行返回 `Transition::AlreadyHandled`,并写 `trade_strategy_events`
- 策略参数或股票池变更 → 新 `version_hash` → 状态重置为 `Draft`(spec §10.1)
- 前推回测用 A 股成交口径(`AShareExecution`,滑点取 `[trade] slippage` 默认 0.1%),训练窗只用于选参数,检验窗只运行不参与选择
- 准入默认阈值(spec §10.4):样本外夏普 ≥ 0.8、最大回撤 ≤ 25%、交易 ≥ 30 笔、夏普衰减 ≤ 50%、池内正收益占比 ≥ 55%、数据 ≥ 3 年、样本外收益 > 0 且 > 同期买入持有;观察期 ≥ 20 交易日且 ≥ 10 笔(异动类 40 日 / 30 笔)
- 测试不得访问网络:K 线经闭包 / 切片注入(沿用 `stock::recommend::build_report` 的注入约定)
- 不引入新依赖;不引入 tokio
- CI:`cargo fmt --check` 干净;`cargo clippy --all-targets -- -D warnings` 除 3 个既有问题(`src/stock/diagnose.rs:16`、`src/ai.rs:164`、`src/ai.rs:170`)外无新增;`cargo test --all-targets --no-fail-fast` 除既有失败 `tests/realtime_pipeline.rs::full_day_flow_from_detection_to_summary` 外全部通过
- 不使用 `git stash`

### 相对 spec 的实现细化(执行者照此实现)

1. **样本外聚合**:各检验窗独立回测(每窗重新建仓、初始资金相同),汇总方式为:收益按窗口连乘、年化按总检验天数折算、夏普按检验天数加权平均、最大回撤取各窗最大值、交易笔数求和。不做跨窗资金连续的单条权益曲线(现引擎每次回测独立建仓,连接曲线会混入重复建仓成本)。该近似在指标文档注释中写明。
2. **策略定义**含参数网格(`grid_toml`),每个训练窗各自选参;`trade_strategy_evals.metrics_json` 保存逐窗选中的参数与指标,供计划 3b 的成绩单展示。
3. **买入持有基准**按每只股票检验区间首末复权收盘价计算,池内取中位数。
4. **观察期与实盘监控**(spec §10.5)在计划 3b:本计划只提供 `judge_paper` 纯函数与状态机接口。
5. **异动类策略**没有历史分时数据,不能回测:`Strategy::kind == "mover"` 的策略提交后直接进入观察期(`Paper`),回测关跳过。
6. **资金口径**:前推回测默认 `initial_cash = 0`,组合按需注入资金,因此收益与回撤都相对「实际投入」度量,不受策略单笔金额与初始现金比例影响。
7. **数据年限**与样本外年限分开:`data_years` 为 K 线总跨度(准入按它判定 ≥3 年),`years` 为检验窗合计跨度(仅用于年化)。
8. **池内覆盖率**:`PoolMetrics` 记录 requested / skipped,低于 `min_evaluated_ratio` 判不通过;交易笔数按每只中位数判定。

---

## File Structure

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/trade/model.rs` | 修改 | `StrategyStatus`、`StrategyDef`、`NewStrategy` |
| `src/trade/store.rs` | 修改 | 3 张策略表;策略 CRUD、评估落库、状态事件 |
| `src/trade/admission/mod.rs` | 新建 | 模块声明与 `Admission` 映射 |
| `src/trade/admission/walk_forward.rs` | 新建 | 窗口切分、逐只前推回测、池内聚合 |
| `src/trade/admission/judge.rs` | 新建 | 阈值判定(回测关、观察期关)与 `AdmissionCfg` |
| `src/trade/admission/state.rs` | 新建 | 状态机转换 + 提交回测结论 |
| `src/trade/config.rs` | 修改 | `[trade.admission]` 段与 `slippage` |
| `src/trade/mod.rs` | 修改 | `pub mod admission;` |

---

### Task 1: 策略定义、版本哈希与三张表

**Files:**
- Modify: `src/trade/model.rs`、`src/trade/store.rs`
- Test: 同文件 `mod tests`

**Interfaces:**
- Consumes: 既有 `model::{fmt_ts, parse_ts, DATE_FMT}`、`store::{migrate, ensure_column}` 约定
- Produces:

```rust
// model.rs
pub enum StrategyStatus { Draft, Backtesting, Failed, Paper, Admitted, Suspended } // Copy/Eq/Serialize; as_str "draft"/"backtesting"/"failed"/"paper"/"admitted"/"suspended"; parse
pub struct NewStrategy { pub user_id: i64, pub name: String, pub kind: String, pub grid_toml: String, pub pool: Vec<String> }
pub struct StrategyDef { pub id: i64, pub user_id: i64, pub name: String, pub kind: String, pub grid_toml: String, pub pool: Vec<String>, pub version_hash: String, pub status: StrategyStatus, pub status_reason: Option<String>, pub updated_at: NaiveDateTime }
pub fn strategy_version_hash(kind: &str, grid_toml: &str, pool: &[String]) -> String; // sha256 前 16 hex,池内代码排序后参与
// store.rs
pub fn create_strategy(conn: &Connection, s: &NewStrategy, now: NaiveDateTime) -> Result<i64>;
pub fn get_strategy(conn: &Connection, user_id: i64, id: i64) -> Result<Option<StrategyDef>>;
pub fn list_strategies(conn: &Connection, user_id: i64) -> Result<Vec<StrategyDef>>;
pub fn update_definition(conn: &Connection, user_id: i64, id: i64, s: &NewStrategy, now: NaiveDateTime) -> Result<bool>; // 版本变化 → status 重置 Draft、status_reason 清空
pub fn save_eval(conn: &Connection, strategy_id: i64, version_hash: &str, stage: &str, metrics_json: &str, data_from: NaiveDate, data_to: NaiveDate, now: NaiveDateTime) -> Result<i64>;
pub fn latest_eval(conn: &Connection, strategy_id: i64, stage: &str) -> Result<Option<(String, NaiveDateTime)>>; // (metrics_json, run_at)
pub fn log_status_event(conn: &Connection, strategy_id: i64, from: StrategyStatus, to: StrategyStatus, reason: &str, now: NaiveDateTime) -> Result<()>;
pub fn list_status_events(conn: &Connection, strategy_id: i64) -> Result<Vec<(StrategyStatus, StrategyStatus, String, NaiveDateTime)>>;
```

- [ ] **Step 1: 写失败测试**

`src/trade/model.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn strategy_status_round_trip_and_hash_is_order_insensitive() {
        for s in [
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            StrategyStatus::Failed,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            StrategyStatus::Suspended,
        ] {
            assert_eq!(StrategyStatus::parse(s.as_str()).unwrap(), s);
        }
        assert!(StrategyStatus::parse("x").is_err());

        let a = strategy_version_hash("rsi", "rsi_window = [14]", &["600000".into(), "000001".into()]);
        let b = strategy_version_hash("rsi", "rsi_window = [14]", &["000001".into(), "600000".into()]);
        assert_eq!(a, b, "池内顺序不影响版本");
        assert_eq!(a.len(), 16);
        assert_ne!(a, strategy_version_hash("rsi", "rsi_window = [20]", &["600000".into()]));
        assert_ne!(a, strategy_version_hash("trend", "rsi_window = [14]", &["600000".into(), "000001".into()]));
    }
```

`src/trade/store.rs` 的 `mod tests`:把 `migrate_is_idempotent_and_creates_tables` 中的 `assert_eq!(n, 8);` 改为 `assert_eq!(n, 11);`,并追加:

```rust
    fn new_strategy() -> NewStrategy {
        NewStrategy {
            user_id: 1,
            name: "RSI 低吸".into(),
            kind: "rsi".into(),
            grid_toml: "rsi_window = [14]\noversold = [30.0]\noverbought = [70.0]\namount = [10000.0]".into(),
            pool: vec!["600000".into(), "000001".into()],
        }
    }

    #[test]
    fn strategy_crud_versioning_and_user_isolation() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let got = get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!((got.status, got.kind.as_str(), got.pool.len()), (StrategyStatus::Draft, "rsi", 2));
        assert!(get_strategy(&c, 2, id).unwrap().is_none(), "用户隔离");
        assert_eq!(list_strategies(&c, 1).unwrap().len(), 1);
        assert!(list_strategies(&c, 2).unwrap().is_empty());

        // 未改定义 → 版本不变;改网格 → 版本变化且状态回到草稿
        update_status(&c, 1, id, StrategyStatus::Draft, StrategyStatus::Backtesting, "提交", at(16, 9, 1)).unwrap();
        assert!(!update_definition(&c, 1, id, &new_strategy(), at(16, 9, 2)).unwrap(), "同定义不产生新版本");
        assert_eq!(get_strategy(&c, 1, id).unwrap().unwrap().status, StrategyStatus::Backtesting);
        let mut changed = new_strategy();
        changed.grid_toml = "rsi_window = [14, 20]\noversold = [30.0]\noverbought = [70.0]\namount = [10000.0]".into();
        assert!(update_definition(&c, 1, id, &changed, at(16, 9, 3)).unwrap());
        let got = get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Draft);
        assert_ne!(got.version_hash, new_strategy_hash());
        assert!(!update_definition(&c, 2, id, &changed, at(16, 9, 4)).unwrap(), "他人不可改");
    }

    fn new_strategy_hash() -> String {
        let s = new_strategy();
        strategy_version_hash(&s.kind, &s.grid_toml, &s.pool)
    }

    #[test]
    fn evals_and_status_events_are_recorded() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let v = new_strategy_hash();
        save_eval(&c, id, &v, "oos", r#"{"sharpe":1.2}"#, day(15), day(16), at(16, 9, 5)).unwrap();
        save_eval(&c, id, &v, "oos", r#"{"sharpe":1.3}"#, day(15), day(16), at(16, 9, 6)).unwrap();
        let (json, run_at) = latest_eval(&c, id, "oos").unwrap().unwrap();
        assert!(json.contains("1.3"), "取最新一条");
        assert_eq!(run_at, at(16, 9, 6));
        assert!(latest_eval(&c, id, "paper").unwrap().is_none());

        log_status_event(&c, id, StrategyStatus::Draft, StrategyStatus::Backtesting, "提交", at(16, 9, 7)).unwrap();
        let events = list_status_events(&c, id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].0, events[0].1, events[0].2.as_str()), (StrategyStatus::Draft, StrategyStatus::Backtesting, "提交"));
    }
```

在 store 测试模块顶部补一个日期助手(若已存在同名则复用):

```rust
    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }
```

> `update_status` 由 Task 3 实现;本任务的测试先用它,Task 1 结束时该测试会因缺函数而无法编译 —— 因此 Task 1 只需在 Step 1 里写出除 `strategy_crud_versioning_and_user_isolation` 中那一行 `update_status(...)` 外的内容:实现时把该行替换为直接 SQL `c.execute("UPDATE trade_strategies SET status='backtesting' WHERE id=?1", [id]).unwrap();`,Task 3 完成后再换回 `update_status`(Task 3 的 Step 会明确要求换回)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::model::tests trade::store::tests`
Expected: 编译失败(`StrategyStatus`、`create_strategy` 等未定义)

- [ ] **Step 3: 实现 model.rs**

在 `Ticket` 定义之后追加:

```rust
/// 策略生命周期(spec §10.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyStatus {
    /// 草稿:定义已保存,未提交评估
    Draft,
    /// 回测中:前推回测排队 / 运行中
    Backtesting,
    /// 未通过:回测或观察期不达标
    Failed,
    /// 观察期:仅模拟盘
    Paper,
    /// 已准入:实盘 + 模拟盘
    Admitted,
    /// 已暂停:实盘表现异常,退回观察期前需重新提交
    Suspended,
}

impl StrategyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyStatus::Draft => "draft",
            StrategyStatus::Backtesting => "backtesting",
            StrategyStatus::Failed => "failed",
            StrategyStatus::Paper => "paper",
            StrategyStatus::Admitted => "admitted",
            StrategyStatus::Suspended => "suspended",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "draft" => StrategyStatus::Draft,
            "backtesting" => StrategyStatus::Backtesting,
            "failed" => StrategyStatus::Failed,
            "paper" => StrategyStatus::Paper,
            "admitted" => StrategyStatus::Admitted,
            "suspended" => StrategyStatus::Suspended,
            _ => return Err(anyhow!("未知策略状态: {s}")),
        })
    }
}

/// 待创建 / 待更新的策略定义。策略 = 类型 + 参数网格 + 股票池。
#[derive(Debug, Clone, PartialEq)]
pub struct NewStrategy {
    pub user_id: i64,
    pub name: String,
    /// 策略类型,见 `crate::config::build_strategy_from`
    pub kind: String,
    /// 参数网格(TOML 表文本),每个训练窗在其中选参
    pub grid_toml: String,
    pub pool: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StrategyDef {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub kind: String,
    pub grid_toml: String,
    pub pool: Vec<String>,
    pub version_hash: String,
    pub status: StrategyStatus,
    pub status_reason: Option<String>,
    pub updated_at: NaiveDateTime,
}

/// 定义指纹:类型 + 网格 + 排序后的股票池。定义一变即换版本,状态回到草稿。
pub fn strategy_version_hash(kind: &str, grid_toml: &str, pool: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&str> = pool.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"\n");
    h.update(grid_toml.as_bytes());
    h.update(b"\n");
    h.update(sorted.join(",").as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}
```

- [ ] **Step 4: 实现 store.rs**

1. `SCHEMA` 末尾追加:

```sql
CREATE TABLE IF NOT EXISTS trade_strategies (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id       INTEGER NOT NULL,
  name          TEXT NOT NULL,
  kind          TEXT NOT NULL,
  grid_toml     TEXT NOT NULL,
  pool_json     TEXT NOT NULL,
  version_hash  TEXT NOT NULL,
  status        TEXT NOT NULL,
  status_reason TEXT,
  created_at    TEXT NOT NULL,
  updated_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_strategies_user ON trade_strategies(user_id, status);
CREATE TABLE IF NOT EXISTS trade_strategy_evals (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  strategy_id  INTEGER NOT NULL,
  version_hash TEXT NOT NULL,
  stage        TEXT NOT NULL,
  metrics_json TEXT NOT NULL,
  data_from    TEXT NOT NULL,
  data_to      TEXT NOT NULL,
  run_at       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_strategy_evals ON trade_strategy_evals(strategy_id, stage, id);
CREATE TABLE IF NOT EXISTS trade_strategy_events (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  strategy_id INTEGER NOT NULL,
  from_status TEXT NOT NULL,
  to_status   TEXT NOT NULL,
  reason      TEXT NOT NULL,
  at          TEXT NOT NULL
);
```

2. 顶部 use 增加 `NewStrategy, StrategyDef, StrategyStatus, strategy_version_hash`。
3. 追加实现:

```rust
fn read_strategy(r: &Row) -> rusqlite::Result<(i64, i64, String, String, String, String, String, String, Option<String>, String)> {
    Ok((
        r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?,
    ))
}

const STRATEGY_COLS: &str = "id, user_id, name, kind, grid_toml, pool_json, version_hash, status, status_reason, updated_at";

#[allow(clippy::type_complexity)]
fn to_strategy(
    raw: (i64, i64, String, String, String, String, String, String, Option<String>, String),
) -> Result<StrategyDef> {
    let (id, user_id, name, kind, grid_toml, pool_json, version_hash, status, status_reason, updated_at) = raw;
    Ok(StrategyDef {
        id,
        user_id,
        name,
        kind,
        grid_toml,
        pool: serde_json::from_str(&pool_json).context("策略股票池格式错误")?,
        version_hash,
        status: StrategyStatus::parse(&status)?,
        status_reason,
        updated_at: parse_ts(&updated_at)?,
    })
}

pub fn create_strategy(conn: &Connection, s: &NewStrategy, now: NaiveDateTime) -> Result<i64> {
    let hash = strategy_version_hash(&s.kind, &s.grid_toml, &s.pool);
    conn.execute(
        "INSERT INTO trade_strategies (user_id, name, kind, grid_toml, pool_json, version_hash,
           status, status_reason, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?8)",
        params![
            s.user_id,
            s.name,
            s.kind,
            s.grid_toml,
            serde_json::to_string(&s.pool)?,
            hash,
            StrategyStatus::Draft.as_str(),
            fmt_ts(now),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_strategy(conn: &Connection, user_id: i64, id: i64) -> Result<Option<StrategyDef>> {
    conn.query_row(
        &format!("SELECT {STRATEGY_COLS} FROM trade_strategies WHERE id = ?1 AND user_id = ?2"),
        params![id, user_id],
        read_strategy,
    )
    .optional()?
    .map(to_strategy)
    .transpose()
}

pub fn list_strategies(conn: &Connection, user_id: i64) -> Result<Vec<StrategyDef>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {STRATEGY_COLS} FROM trade_strategies WHERE user_id = ?1 ORDER BY id"
    ))?;
    let raws = stmt
        .query_map([user_id], read_strategy)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(to_strategy).collect()
}

/// 定义变更:版本哈希不同才写入,并把状态重置为草稿(spec §10.1)。返回是否发生变更。
pub fn update_definition(
    conn: &Connection,
    user_id: i64,
    id: i64,
    s: &NewStrategy,
    now: NaiveDateTime,
) -> Result<bool> {
    let Some(cur) = get_strategy(conn, user_id, id)? else {
        return Ok(false);
    };
    let hash = strategy_version_hash(&s.kind, &s.grid_toml, &s.pool);
    if hash == cur.version_hash {
        return Ok(false);
    }
    conn.execute(
        "UPDATE trade_strategies SET name = ?1, kind = ?2, grid_toml = ?3, pool_json = ?4,
           version_hash = ?5, status = ?6, status_reason = NULL, updated_at = ?7
         WHERE id = ?8 AND user_id = ?9",
        params![
            s.name,
            s.kind,
            s.grid_toml,
            serde_json::to_string(&s.pool)?,
            hash,
            StrategyStatus::Draft.as_str(),
            fmt_ts(now),
            id,
            user_id,
        ],
    )?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
pub fn save_eval(
    conn: &Connection,
    strategy_id: i64,
    version_hash: &str,
    stage: &str,
    metrics_json: &str,
    data_from: NaiveDate,
    data_to: NaiveDate,
    now: NaiveDateTime,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO trade_strategy_evals (strategy_id, version_hash, stage, metrics_json, data_from, data_to, run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            strategy_id,
            version_hash,
            stage,
            metrics_json,
            data_from.format(DATE_FMT).to_string(),
            data_to.format(DATE_FMT).to_string(),
            fmt_ts(now),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn latest_eval(
    conn: &Connection,
    strategy_id: i64,
    stage: &str,
) -> Result<Option<(String, NaiveDateTime)>> {
    let raw: Option<(String, String)> = conn
        .query_row(
            "SELECT metrics_json, run_at FROM trade_strategy_evals
             WHERE strategy_id = ?1 AND stage = ?2 ORDER BY id DESC LIMIT 1",
            params![strategy_id, stage],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    raw.map(|(j, at)| Ok((j, parse_ts(&at)?))).transpose()
}

pub fn log_status_event(
    conn: &Connection,
    strategy_id: i64,
    from: StrategyStatus,
    to: StrategyStatus,
    reason: &str,
    now: NaiveDateTime,
) -> Result<()> {
    conn.execute(
        "INSERT INTO trade_strategy_events (strategy_id, from_status, to_status, reason, at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![strategy_id, from.as_str(), to.as_str(), reason, fmt_ts(now)],
    )?;
    Ok(())
}

#[allow(clippy::type_complexity)]
pub fn list_status_events(
    conn: &Connection,
    strategy_id: i64,
) -> Result<Vec<(StrategyStatus, StrategyStatus, String, NaiveDateTime)>> {
    let mut stmt = conn.prepare(
        "SELECT from_status, to_status, reason, at FROM trade_strategy_events
         WHERE strategy_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([strategy_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(f, t, reason, at)| {
            Ok((
                StrategyStatus::parse(&f)?,
                StrategyStatus::parse(&t)?,
                reason,
                parse_ts(&at)?,
            ))
        })
        .collect()
}
```

- [ ] **Step 5: 运行确认通过**

Run: `cargo test --lib trade::model::tests trade::store::tests`
Expected: 全部 PASS(model +1、store +3)

- [ ] **Step 6: Commit**

```bash
git add src/trade/model.rs src/trade/store.rs
git commit -m "feat(trade): 策略定义、版本哈希与策略/评估/事件三张表"
```

---

### Task 2: 滚动前推回测

**Files:**
- Create: `src/trade/admission/mod.rs`、`src/trade/admission/walk_forward.rs`
- Modify: `src/trade/mod.rs`(加 `pub mod admission;`)
- Test: `src/trade/admission/walk_forward.rs` 内 `mod tests`

**Interfaces:**
- Consumes: `crate::config::build_strategy_from`、`crate::optimize::expand_grid`、`crate::stock::backtest::run_one`、`crate::stock::data::{StockBar, StockData}`、`crate::stock::fee::StockFee`、`crate::stock::ashare::AShareExecution`、`crate::metrics::Summary`
- Produces:

```rust
pub struct WalkForwardCfg { pub train_days: i64, pub test_days: i64, pub step_days: i64, pub metric: String, pub initial_cash: f64, pub slippage: f64 } // Default: 730/182/182/"sharpe"/100_000.0/0.001
pub struct Window { pub train_from: NaiveDate, pub train_to: NaiveDate, pub test_to: NaiveDate } // 检验段 = [train_to, test_to]
pub fn windows(first: NaiveDate, last: NaiveDate, cfg: &WalkForwardCfg) -> Vec<Window>;
pub struct WindowResult { pub window: Window, pub params: toml::Value, pub is_sharpe: f64, pub oos: Summary, pub oos_days: i64 }
pub struct CodeResult { pub code: String, pub windows: Vec<WindowResult>, pub buy_hold_return: f64 }
pub struct CodeMetrics { pub code: String, pub windows: usize, pub oos_return: f64, pub oos_annualized: f64, pub oos_sharpe: f64, pub oos_max_drawdown: f64, pub oos_trades: usize, pub is_sharpe: f64, pub years: f64, pub buy_hold_return: f64 } // Serialize
pub struct PoolMetrics { pub codes: Vec<CodeMetrics>, pub oos_return: f64, pub oos_sharpe: f64, pub oos_max_drawdown: f64, pub oos_trades: usize, pub is_sharpe: f64, pub positive_ratio: f64, pub years: f64, pub buy_hold_return: f64 } // Serialize;逐项中位数/求和见文档注释
pub fn buy_and_hold_return(bars: &[StockBar], from: NaiveDate, to: NaiveDate) -> f64;
pub fn run_code(kind: &str, code: &str, bars: &[StockBar], grid: &toml::Table, cfg: &WalkForwardCfg) -> Result<CodeResult>;
impl CodeResult { pub fn metrics(&self) -> CodeMetrics }
pub fn aggregate(codes: Vec<CodeMetrics>) -> PoolMetrics;
pub fn run_pool<F>(kind: &str, pool: &[String], grid: &toml::Table, cfg: &WalkForwardCfg, load: F) -> Result<(PoolMetrics, Vec<String>)> where F: FnMut(&str) -> Result<Vec<StockBar>>; // 第二个返回值:加载失败被跳过的代码
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/mod.rs`:

```rust
//! 策略准入:前推回测、阈值判定、状态机。
//! 设计见 docs/superpowers/specs/2026-09-15-quant-trading-design.md §10

pub mod walk_forward;
```

创建 `src/trade/admission/walk_forward.rs`,先写模块注释与测试:

```rust
//! 滚动前推回测:训练窗选参数、紧接的检验窗只运行,拼接所有检验窗得样本外表现。
//!
//! 与 `optimize.rs` 的单次 70/30 切分不同:每段检验数据都没参与过任何挑选,
//! 因此样本外指标不含 winner's curse。

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    /// 连续交易日(跳过周末),价格按给定序列。
    fn bars(start: NaiveDate, prices: &[f64]) -> Vec<StockBar> {
        let mut out = Vec::new();
        let mut date = start;
        for p in prices {
            while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                date += chrono::Duration::days(1);
            }
            out.push(StockBar {
                date,
                open: *p,
                high: *p,
                low: *p,
                close: *p,
                volume: 1.0,
                adj_close: *p,
            });
            date += chrono::Duration::days(1);
        }
        out
    }

    fn cfg() -> WalkForwardCfg {
        WalkForwardCfg {
            train_days: 60,
            test_days: 30,
            step_days: 30,
            ..WalkForwardCfg::default()
        }
    }

    fn grid() -> toml::Table {
        "short_window = [3, 5]\nlong_window = [10]\namount = [20000.0]"
            .parse::<toml::Table>()
            .unwrap()
    }

    #[test]
    fn windows_roll_forward_and_stop_at_data_end() {
        // 训练 60 天 + 检验 30 天,步长 30 天:起点 1/1、1/31、3/1、3/31 共 4 窗
        let ws = windows(d(2024, 1, 1), d(2024, 6, 30), &cfg());
        assert_eq!(ws.len(), 4, "{ws:?}");
        assert_eq!(ws[0].train_from, d(2024, 1, 1));
        assert_eq!(ws[0].train_to, d(2024, 3, 1));
        assert_eq!(ws[0].test_to, d(2024, 3, 31));
        assert_eq!(ws[1].train_from, d(2024, 1, 31), "步长 30 天");
        assert!(ws.iter().all(|w| w.test_to <= d(2024, 6, 30)));
        assert!(windows(d(2024, 1, 1), d(2024, 2, 1), &cfg()).is_empty(), "数据不足一窗");
    }

    #[test]
    fn buy_and_hold_uses_adjusted_span() {
        let b = bars(d(2024, 1, 1), &[10.0, 11.0, 12.0, 13.0]);
        let r = buy_and_hold_return(&b, b[1].date, b[3].date);
        assert!((r - (13.0 / 11.0 - 1.0)).abs() < 1e-9, "{r}");
        assert_eq!(buy_and_hold_return(&b, d(2030, 1, 1), d(2030, 2, 1)), 0.0, "区间无数据");
    }

    #[test]
    fn run_code_picks_params_in_train_and_measures_in_test() {
        // 190 个交易日的上涨序列,3 个窗口
        let prices: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let b = bars(d(2024, 1, 1), &prices);
        let out = run_code("trend", "600000", &b, &grid(), &cfg()).unwrap();
        assert!(!out.windows.is_empty(), "应至少产出一个窗口");
        for w in &out.windows {
            assert!(w.params.get("short_window").is_some(), "参数来自网格");
            assert!(w.oos_days > 0);
        }
        let m = out.metrics();
        assert_eq!(m.code, "600000");
        assert_eq!(m.windows, out.windows.len());
        assert!(m.years > 0.0);
        assert!(m.buy_hold_return > 0.0, "上涨行情买入持有为正");
    }

    #[test]
    fn aggregate_takes_medians_sums_and_positive_ratio() {
        let mk = |code: &str, ret: f64, sharpe: f64, mdd: f64, trades: usize| CodeMetrics {
            code: code.into(),
            windows: 2,
            oos_return: ret,
            oos_annualized: ret,
            oos_sharpe: sharpe,
            oos_max_drawdown: mdd,
            oos_trades: trades,
            is_sharpe: sharpe * 2.0,
            years: 1.0,
            buy_hold_return: 0.05,
        };
        let p = aggregate(vec![
            mk("a", 0.10, 1.0, 0.10, 20),
            mk("b", -0.05, 0.4, 0.30, 10),
            mk("c", 0.20, 1.6, 0.20, 12),
        ]);
        assert!((p.oos_return - 0.10).abs() < 1e-9, "中位数");
        assert!((p.oos_sharpe - 1.0).abs() < 1e-9);
        assert!((p.oos_max_drawdown - 0.20).abs() < 1e-9);
        assert_eq!(p.oos_trades, 42, "求和");
        assert!((p.positive_ratio - 2.0 / 3.0).abs() < 1e-9);
        assert!((p.buy_hold_return - 0.05).abs() < 1e-9);
        assert!((p.is_sharpe - 2.0).abs() < 1e-9);
        assert_eq!(aggregate(Vec::new()).positive_ratio, 0.0);
    }

    #[test]
    fn run_pool_skips_unloadable_codes() {
        let prices: Vec<f64> = (0..190).map(|i| 10.0 + i as f64 * 0.05).collect();
        let good = bars(d(2024, 1, 1), &prices);
        let (metrics, skipped) = run_pool(
            "trend",
            &["600000".to_string(), "000001".to_string()],
            &grid(),
            &cfg(),
            |code| match code {
                "600000" => Ok(good.clone()),
                _ => Err(anyhow::anyhow!("无数据")),
            },
        )
        .unwrap();
        assert_eq!(metrics.codes.len(), 1);
        assert_eq!(skipped, vec!["000001".to_string()]);
    }
}
```

在 `src/trade/mod.rs` 加 `pub mod admission;`(字母序置于 `config` 之前)。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission::walk_forward::tests`
Expected: 编译失败(`windows`、`run_code` 等未定义)

- [ ] **Step 3: 实现**

在 `walk_forward.rs` 模块注释之后、`#[cfg(test)]` 之前插入:

```rust
use crate::config::build_strategy_from;
use crate::metrics::Summary;
use crate::optimize::expand_grid;
use crate::stock::ashare::AShareExecution;
use crate::stock::backtest;
use crate::stock::data::{StockBar, StockData};
use crate::stock::fee::StockFee;
use anyhow::{anyhow, Result};
use chrono::{Datelike, Duration, NaiveDate};
use serde::Serialize;

/// 一个窗口内至少要有多少根 K 线才算数(约 2 个月 / 1 个月)。
const MIN_TRAIN_BARS: usize = 40;
const MIN_TEST_BARS: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct WalkForwardCfg {
    pub train_days: i64,
    pub test_days: i64,
    pub step_days: i64,
    /// 训练窗选参依据:sharpe | total_return | annualized | max_drawdown
    pub metric: String,
    pub initial_cash: f64,
    pub slippage: f64,
}

impl Default for WalkForwardCfg {
    fn default() -> Self {
        Self {
            train_days: 730,
            test_days: 182,
            step_days: 182,
            metric: "sharpe".into(),
            initial_cash: 100_000.0,
            slippage: AShareExecution::DEFAULT_SLIPPAGE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Window {
    pub train_from: NaiveDate,
    /// 训练段结束(不含),同时是检验段起点(含)
    pub train_to: NaiveDate,
    pub test_to: NaiveDate,
}

pub fn windows(first: NaiveDate, last: NaiveDate, cfg: &WalkForwardCfg) -> Vec<Window> {
    let mut out = Vec::new();
    if cfg.train_days <= 0 || cfg.test_days <= 0 || cfg.step_days <= 0 {
        return out;
    }
    let mut train_from = first;
    loop {
        let train_to = train_from + Duration::days(cfg.train_days);
        let test_to = train_to + Duration::days(cfg.test_days);
        if test_to > last {
            break;
        }
        out.push(Window {
            train_from,
            train_to,
            test_to,
        });
        train_from += Duration::days(cfg.step_days);
    }
    out
}

/// 区间内买入持有收益(复权)。区间无数据返回 0。
pub fn buy_and_hold_return(bars: &[StockBar], from: NaiveDate, to: NaiveDate) -> f64 {
    let span: Vec<&StockBar> = bars.iter().filter(|b| b.date >= from && b.date <= to).collect();
    match (span.first(), span.last()) {
        (Some(a), Some(b)) if a.adj_close > 0.0 => b.adj_close / a.adj_close - 1.0,
        _ => 0.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct WindowResult {
    pub window: Window,
    pub params: toml::Value,
    pub is_sharpe: f64,
    pub oos: Summary,
    pub oos_days: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeResult {
    pub code: String,
    pub windows: Vec<WindowResult>,
    pub buy_hold_return: f64,
}

/// 单只股票的样本外汇总。
///
/// 聚合口径(见计划「相对 spec 的实现细化」1):收益连乘、年化按总检验天数折算、
/// 夏普按检验天数加权平均、最大回撤取各窗最大、交易笔数求和。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeMetrics {
    pub code: String,
    pub windows: usize,
    pub oos_return: f64,
    pub oos_annualized: f64,
    pub oos_sharpe: f64,
    pub oos_max_drawdown: f64,
    pub oos_trades: usize,
    pub is_sharpe: f64,
    pub years: f64,
    pub buy_hold_return: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PoolMetrics {
    pub codes: Vec<CodeMetrics>,
    /// 池内中位数
    pub oos_return: f64,
    pub oos_sharpe: f64,
    pub oos_max_drawdown: f64,
    /// 池内求和
    pub oos_trades: usize,
    pub is_sharpe: f64,
    /// 样本外收益为正的股票占比
    pub positive_ratio: f64,
    pub years: f64,
    pub buy_hold_return: f64,
}

fn metric_of(s: &Summary, metric: &str) -> f64 {
    match metric {
        "total_return" => s.total_return,
        "annualized" => s.annualized,
        // 回撤越小越好
        "max_drawdown" => -s.max_drawdown,
        _ => s.sharpe,
    }
}

fn median(mut xs: Vec<f64>) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = xs.len();
    if n % 2 == 1 {
        xs[n / 2]
    } else {
        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
    }
}

fn run_once(
    kind: &str,
    code: &str,
    data: StockData,
    params: &toml::Value,
    cfg: &WalkForwardCfg,
) -> Result<backtest::StockRunOutcome> {
    let strategy = build_strategy_from(kind, &Some(params.clone()), &[])?;
    Ok(backtest::run_one(
        kind.to_string(),
        code.to_string(),
        data,
        strategy,
        StockFee::a_share(),
        cfg.initial_cash,
        Box::new(AShareExecution::new(code, None, cfg.slippage)),
    ))
}

/// 对一只股票做完整前推回测:每个训练窗按 `cfg.metric` 选参,紧接的检验窗只运行。
pub fn run_code(
    kind: &str,
    code: &str,
    bars: &[StockBar],
    grid: &toml::Table,
    cfg: &WalkForwardCfg,
) -> Result<CodeResult> {
    let combos = expand_grid(grid)?;
    let (Some(first), Some(last)) = (bars.first(), bars.last()) else {
        return Err(anyhow!("{code} 无 K 线数据"));
    };
    let mut out = Vec::new();
    for w in windows(first.date, last.date, cfg) {
        let train: Vec<StockBar> = bars
            .iter()
            .copied()
            .filter(|b| b.date >= w.train_from && b.date < w.train_to)
            .collect();
        let test: Vec<StockBar> = bars
            .iter()
            .copied()
            .filter(|b| b.date >= w.train_to && b.date <= w.test_to)
            .collect();
        if train.len() < MIN_TRAIN_BARS || test.len() < MIN_TEST_BARS {
            continue;
        }
        let prev = bars.iter().copied().filter(|b| b.date < w.train_to).next_back();

        let mut best: Option<(toml::Value, f64, f64)> = None;
        for params in &combos {
            let run = run_once(kind, code, StockData::new(train.clone()), params, cfg)?;
            let score = metric_of(&run.summary, &cfg.metric);
            if best.as_ref().is_none_or(|(_, s, _)| score > *s) {
                best = Some((params.clone(), score, run.summary.sharpe));
            }
        }
        let Some((params, _, is_sharpe)) = best else {
            continue;
        };
        let oos = run_once(
            kind,
            code,
            StockData::with_prev_bar(test.clone(), prev),
            &params,
            cfg,
        )?;
        out.push(WindowResult {
            window: w,
            params,
            is_sharpe,
            oos: oos.summary,
            oos_days: (w.test_to - w.train_to).num_days().max(1),
        });
    }
    let (from, to) = match (out.first(), out.last()) {
        (Some(a), Some(b)) => (a.window.train_to, b.window.test_to),
        _ => (first.date, last.date),
    };
    Ok(CodeResult {
        code: code.to_string(),
        windows: out,
        buy_hold_return: buy_and_hold_return(bars, from, to),
    })
}

impl CodeResult {
    pub fn metrics(&self) -> CodeMetrics {
        let days: i64 = self.windows.iter().map(|w| w.oos_days).sum();
        let years = (days as f64 / 365.0).max(1e-9);
        let oos_return = self
            .windows
            .iter()
            .fold(1.0, |acc, w| acc * (1.0 + w.oos.total_return))
            - 1.0;
        let weight = |f: fn(&WindowResult) -> f64| -> f64 {
            if days == 0 {
                return 0.0;
            }
            self.windows
                .iter()
                .map(|w| f(w) * w.oos_days as f64)
                .sum::<f64>()
                / days as f64
        };
        CodeMetrics {
            code: self.code.clone(),
            windows: self.windows.len(),
            oos_return,
            oos_annualized: if self.windows.is_empty() {
                0.0
            } else {
                (1.0 + oos_return).powf(1.0 / years) - 1.0
            },
            oos_sharpe: weight(|w| w.oos.sharpe),
            oos_max_drawdown: self
                .windows
                .iter()
                .map(|w| w.oos.max_drawdown)
                .fold(0.0, f64::max),
            oos_trades: self.windows.iter().map(|w| w.oos.trade_count).sum(),
            is_sharpe: weight(|w| w.is_sharpe),
            years: days as f64 / 365.0,
            buy_hold_return: self.buy_hold_return,
        }
    }
}

pub fn aggregate(codes: Vec<CodeMetrics>) -> PoolMetrics {
    let positive = codes.iter().filter(|c| c.oos_return > 0.0).count();
    let ratio = if codes.is_empty() {
        0.0
    } else {
        positive as f64 / codes.len() as f64
    };
    PoolMetrics {
        oos_return: median(codes.iter().map(|c| c.oos_return).collect()),
        oos_sharpe: median(codes.iter().map(|c| c.oos_sharpe).collect()),
        oos_max_drawdown: median(codes.iter().map(|c| c.oos_max_drawdown).collect()),
        oos_trades: codes.iter().map(|c| c.oos_trades).sum(),
        is_sharpe: median(codes.iter().map(|c| c.is_sharpe).collect()),
        positive_ratio: ratio,
        years: median(codes.iter().map(|c| c.years).collect()),
        buy_hold_return: median(codes.iter().map(|c| c.buy_hold_return).collect()),
        codes,
    }
}

/// 对整个股票池跑前推回测。K 线由调用方注入(测试用切片,生产用缓存加载)。
/// 返回池内汇总与加载失败被跳过的代码。
pub fn run_pool<F>(
    kind: &str,
    pool: &[String],
    grid: &toml::Table,
    cfg: &WalkForwardCfg,
    mut load: F,
) -> Result<(PoolMetrics, Vec<String>)>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let mut metrics = Vec::new();
    let mut skipped = Vec::new();
    for code in pool {
        match load(code) {
            Ok(bars) => match run_code(kind, code, &bars, grid, cfg) {
                Ok(r) if !r.windows.is_empty() => metrics.push(r.metrics()),
                _ => skipped.push(code.clone()),
            },
            Err(_) => skipped.push(code.clone()),
        }
    }
    Ok((aggregate(metrics), skipped))
}
```

> 若 `Datelike` 未被用到,删除该 import;若 clippy 提示 `type_complexity`,把 `best` 的元组换成局部 struct。

- [ ] **Step 4: 运行确认通过**

Run: `cargo test --lib trade::admission::walk_forward::tests`
Expected: 5 PASS(若 `run_code_picks_params_in_train_and_measures_in_test` 因合成数据未产出窗口而失败,先打印 `windows(...)` 结果核对边界,再按实际交易日数调整测试里的价格序列长度,并在报告中说明)

- [ ] **Step 5: Commit**

```bash
git add src/trade/admission src/trade/mod.rs
git commit -m "feat(trade): 滚动前推回测(训练窗选参、检验窗实测、池内聚合)"
```

---

### Task 3: 准入阈值、判定与状态机

**Files:**
- Create: `src/trade/admission/judge.rs`、`src/trade/admission/state.rs`
- Modify: `src/trade/admission/mod.rs`、`src/trade/config.rs`、`src/trade/store.rs`(测试改回 `update_status`)
- Test: 新文件内 `mod tests`;`src/trade/config.rs` 测试补 `[trade.admission]`

**Interfaces:**
- Consumes: Task 1 store 与 model;Task 2 `PoolMetrics`;`gate::Admission`
- Produces:

```rust
// config.rs
pub struct AdmissionCfg { pub min_oos_sharpe: f64, pub max_oos_drawdown: f64, pub min_oos_trades: usize, pub max_sharpe_decay: f64, pub min_positive_ratio: f64, pub min_years: f64, pub paper_days: i64, pub paper_trades: usize, pub mover_paper_days: i64, pub mover_paper_trades: usize }
// Default: 0.8 / 0.25 / 30 / 0.5 / 0.55 / 3.0 / 20 / 10 / 40 / 30;TradeCfg 增加字段 `pub admission: AdmissionCfg`(serde default)
// judge.rs
pub struct Verdict { pub passed: bool, pub reasons: Vec<String> } // reasons 为不达标项的中文说明
pub fn judge_backtest(m: &PoolMetrics, cfg: &AdmissionCfg) -> Verdict;
pub struct PaperStats { pub days: i64, pub trades: usize, pub avg_trade_return: f64, pub max_drawdown: f64 }
pub fn judge_paper(stats: &PaperStats, backtest_avg_trade_return: f64, backtest_trade_return_sd: f64, backtest_max_drawdown: f64, is_mover: bool, cfg: &AdmissionCfg) -> Verdict;
// state.rs
pub fn update_status(conn: &Connection, user_id: i64, id: i64, expect: StrategyStatus, to: StrategyStatus, reason: &str, now: NaiveDateTime) -> Result<Transition>; // 条件 UPDATE + 事件
pub fn submit_for_backtest(conn: &Connection, user_id: i64, id: i64, now: NaiveDateTime) -> Result<Transition>; // Draft|Failed|Suspended → Backtesting;kind == "mover" 直接 → Paper
pub fn apply_backtest_verdict(conn: &Connection, user_id: i64, id: i64, metrics: &PoolMetrics, verdict: &Verdict, from: NaiveDate, to: NaiveDate, now: NaiveDateTime) -> Result<Transition>; // 保存 eval(stage "oos") + Backtesting → Paper / Failed
pub fn admission_for(conn: &Connection, user_id: i64, strategy_id: Option<i64>) -> Result<Admission>; // None → NotRequired;Admitted → Admitted;Paper → Probation;其余/不存在 → Blocked
```

- [ ] **Step 1: 写失败测试**

创建 `src/trade/admission/judge.rs`:

```rust
//! 准入阈值判定(spec §10.4)。纯函数:比较指标与阈值,列出不达标原因。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::walk_forward::{CodeMetrics, PoolMetrics};

    fn pool(sharpe: f64, mdd: f64, trades: usize, ratio: f64, years: f64, ret: f64, is_sharpe: f64) -> PoolMetrics {
        PoolMetrics {
            codes: vec![CodeMetrics {
                code: "600000".into(),
                windows: 4,
                oos_return: ret,
                oos_annualized: ret,
                oos_sharpe: sharpe,
                oos_max_drawdown: mdd,
                oos_trades: trades,
                is_sharpe,
                years,
                buy_hold_return: 0.05,
            }],
            oos_return: ret,
            oos_sharpe: sharpe,
            oos_max_drawdown: mdd,
            oos_trades: trades,
            is_sharpe,
            positive_ratio: ratio,
            years,
            buy_hold_return: 0.05,
        }
    }

    #[test]
    fn backtest_passes_when_every_threshold_is_met() {
        let v = judge_backtest(&pool(1.0, 0.20, 40, 0.60, 4.0, 0.30, 1.4), &AdmissionCfg::default());
        assert!(v.passed, "{:?}", v.reasons);
        assert!(v.reasons.is_empty());
    }

    #[test]
    fn backtest_lists_every_failed_threshold() {
        let v = judge_backtest(&pool(0.5, 0.40, 10, 0.30, 2.0, 0.02, 2.0), &AdmissionCfg::default());
        assert!(!v.passed);
        // 夏普、回撤、笔数、正收益占比、数据年限、跑输买入持有、夏普衰减
        assert_eq!(v.reasons.len(), 7, "{:?}", v.reasons);
        assert!(v.reasons.iter().any(|r| r.contains("夏普")));
        assert!(v.reasons.iter().any(|r| r.contains("买入持有")));
        assert!(v.reasons.iter().any(|r| r.contains("衰减")));
    }

    #[test]
    fn negative_return_fails_even_with_good_sharpe() {
        let v = judge_backtest(&pool(1.5, 0.10, 50, 0.80, 5.0, -0.10, 1.6), &AdmissionCfg::default());
        assert!(!v.passed);
        assert!(v.reasons.iter().any(|r| r.contains("样本外收益")), "{:?}", v.reasons);
    }

    #[test]
    fn paper_stage_uses_stricter_bar_for_movers() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats { days: 25, trades: 12, avg_trade_return: 0.02, max_drawdown: 0.15 };
        assert!(judge_paper(&stats, 0.03, 0.02, 0.20, false, &cfg).passed);
        let v = judge_paper(&stats, 0.03, 0.02, 0.20, true, &cfg);
        assert!(!v.passed, "异动类要求 40 日 / 30 笔");
        assert_eq!(v.reasons.len(), 2, "{:?}", v.reasons);
    }

    #[test]
    fn paper_stage_flags_underperformance_and_deeper_drawdown() {
        let cfg = AdmissionCfg::default();
        let stats = PaperStats { days: 30, trades: 20, avg_trade_return: 0.001, max_drawdown: 0.35 };
        let v = judge_paper(&stats, 0.03, 0.01, 0.20, false, &cfg);
        assert!(!v.passed);
        assert!(v.reasons.iter().any(|r| r.contains("每笔收益")), "{:?}", v.reasons);
        assert!(v.reasons.iter().any(|r| r.contains("回撤")), "{:?}", v.reasons);
    }
}
```

创建 `src/trade/admission/state.rs`:

```rust
//! 策略状态机(spec §10.2)。所有转换都是条件 UPDATE,并写事件表。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::walk_forward::PoolMetrics;
    use crate::trade::model::NewStrategy;
    use crate::trade::store;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, m, 0).unwrap()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    fn strategy(c: &Connection, kind: &str) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: kind.into(),
                grid_toml: "rsi_window = [14]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap()
    }

    fn empty_metrics() -> PoolMetrics {
        crate::trade::admission::walk_forward::aggregate(Vec::new())
    }

    #[test]
    fn transitions_are_conditional_and_logged() {
        let c = db();
        let id = strategy(&c, "rsi");
        assert_eq!(
            update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Admitted, "x", at(16, 9, 1)).unwrap(),
            Transition::AlreadyHandled,
            "当前不是观察期"
        );
        assert_eq!(
            update_status(&c, 2, id, StrategyStatus::Draft, StrategyStatus::Backtesting, "x", at(16, 9, 1)).unwrap(),
            Transition::AlreadyHandled,
            "他人策略"
        );
        assert_eq!(
            update_status(&c, 1, id, StrategyStatus::Draft, StrategyStatus::Backtesting, "提交评估", at(16, 9, 2)).unwrap(),
            Transition::Applied
        );
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!((got.status, got.status_reason.as_deref()), (StrategyStatus::Backtesting, Some("提交评估")));
        assert_eq!(store::list_status_events(&c, id).unwrap().len(), 1);
    }

    #[test]
    fn mover_strategies_skip_backtest_stage() {
        let c = db();
        let rsi = strategy(&c, "rsi");
        let mover = strategy(&c, "mover");
        assert_eq!(submit_for_backtest(&c, 1, rsi, at(16, 9, 1)).unwrap(), Transition::Applied);
        assert_eq!(store::get_strategy(&c, 1, rsi).unwrap().unwrap().status, StrategyStatus::Backtesting);
        assert_eq!(submit_for_backtest(&c, 1, mover, at(16, 9, 1)).unwrap(), Transition::Applied);
        assert_eq!(store::get_strategy(&c, 1, mover).unwrap().unwrap().status, StrategyStatus::Paper, "异动类无历史分时,直接进观察期");
    }

    #[test]
    fn verdict_moves_to_paper_or_failed_and_saves_eval() {
        let c = db();
        let id = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, id, at(16, 9, 1)).unwrap();
        let ok = Verdict { passed: true, reasons: Vec::new() };
        assert_eq!(
            apply_backtest_verdict(&c, 1, id, &empty_metrics(), &ok, day(15), day(16), at(16, 9, 2)).unwrap(),
            Transition::Applied
        );
        assert_eq!(store::get_strategy(&c, 1, id).unwrap().unwrap().status, StrategyStatus::Paper);
        assert!(store::latest_eval(&c, id, "oos").unwrap().is_some());

        let id2 = strategy(&c, "rsi");
        submit_for_backtest(&c, 1, id2, at(16, 9, 1)).unwrap();
        let bad = Verdict { passed: false, reasons: vec!["样本外夏普 0.50 < 0.80".into()] };
        apply_backtest_verdict(&c, 1, id2, &empty_metrics(), &bad, day(15), day(16), at(16, 9, 3)).unwrap();
        let got = store::get_strategy(&c, 1, id2).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Failed);
        assert!(got.status_reason.unwrap().contains("夏普"));
    }

    #[test]
    fn admission_maps_status_to_gate_admission() {
        let c = db();
        let id = strategy(&c, "rsi");
        assert_eq!(admission_for(&c, 1, None).unwrap(), Admission::NotRequired);
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Blocked, "草稿不得交易");
        update_status(&c, 1, id, StrategyStatus::Draft, StrategyStatus::Paper, "观察", at(16, 9, 2)).unwrap();
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Probation);
        update_status(&c, 1, id, StrategyStatus::Paper, StrategyStatus::Admitted, "准入", at(16, 9, 3)).unwrap();
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Admitted);
        update_status(&c, 1, id, StrategyStatus::Admitted, StrategyStatus::Suspended, "异常", at(16, 9, 4)).unwrap();
        assert_eq!(admission_for(&c, 1, Some(id)).unwrap(), Admission::Blocked);
        assert_eq!(admission_for(&c, 2, Some(id)).unwrap(), Admission::Blocked, "他人策略");
        assert_eq!(admission_for(&c, 1, Some(999)).unwrap(), Admission::Blocked);
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }
}
```

`src/trade/config.rs` 测试追加:

```rust
    #[test]
    fn admission_section_defaults_and_overrides() {
        let c = from_toml_str("[trade]\n[trade.admission]\nmin_oos_sharpe = 1.2\n").unwrap();
        assert!((c.admission.min_oos_sharpe - 1.2).abs() < 1e-9);
        assert_eq!(c.admission.min_oos_trades, 30, "未覆盖项取默认");
        assert_eq!(from_toml_str("[trade]\n").unwrap().admission, AdmissionCfg::default());
    }
```

在 `src/trade/admission/mod.rs` 加 `pub mod judge;` 与 `pub mod state;`。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test --lib trade::admission trade::config::tests`
Expected: 编译失败(`judge_backtest`、`update_status`、`AdmissionCfg` 等未定义)

- [ ] **Step 3: 实现 config.rs 的 AdmissionCfg**

在 `TradeCfg` 之前插入:

```rust
/// 策略准入阈值(spec §10.4)。管理员可在 `[trade.admission]` 调整;用户侧只可调严。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AdmissionCfg {
    pub min_oos_sharpe: f64,
    pub max_oos_drawdown: f64,
    pub min_oos_trades: usize,
    /// 1 − 样本外夏普 / 样本内夏普 的上限
    pub max_sharpe_decay: f64,
    pub min_positive_ratio: f64,
    pub min_years: f64,
    pub paper_days: i64,
    pub paper_trades: usize,
    pub mover_paper_days: i64,
    pub mover_paper_trades: usize,
}

impl Default for AdmissionCfg {
    fn default() -> Self {
        Self {
            min_oos_sharpe: 0.8,
            max_oos_drawdown: 0.25,
            min_oos_trades: 30,
            max_sharpe_decay: 0.5,
            min_positive_ratio: 0.55,
            min_years: 3.0,
            paper_days: 20,
            paper_trades: 10,
            mover_paper_days: 40,
            mover_paper_trades: 30,
        }
    }
}
```

`TradeCfg` 增加字段 `pub admission: AdmissionCfg,`(放在末尾),`Default for TradeCfg` 中加 `admission: AdmissionCfg::default(),`。

- [ ] **Step 4: 实现 judge.rs**

```rust
use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::config::AdmissionCfg;

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub passed: bool,
    /// 不达标项说明;通过时为空
    pub reasons: Vec<String>,
}

impl Verdict {
    fn from(reasons: Vec<String>) -> Self {
        Self {
            passed: reasons.is_empty(),
            reasons,
        }
    }
}

/// 回测关(spec §10.4)。
pub fn judge_backtest(m: &PoolMetrics, cfg: &AdmissionCfg) -> Verdict {
    let mut r = Vec::new();
    if m.years < cfg.min_years {
        r.push(format!("数据不足:{:.1} 年 < {:.1} 年", m.years, cfg.min_years));
    }
    if m.oos_return <= 0.0 {
        r.push(format!("样本外收益 {:.1}% ≤ 0", m.oos_return * 100.0));
    } else if m.oos_return <= m.buy_hold_return {
        r.push(format!(
            "样本外收益 {:.1}% 未跑赢买入持有 {:.1}%",
            m.oos_return * 100.0,
            m.buy_hold_return * 100.0
        ));
    }
    if m.oos_sharpe < cfg.min_oos_sharpe {
        r.push(format!("样本外夏普 {:.2} < {:.2}", m.oos_sharpe, cfg.min_oos_sharpe));
    }
    if m.oos_max_drawdown > cfg.max_oos_drawdown {
        r.push(format!(
            "样本外最大回撤 {:.1}% > {:.1}%",
            m.oos_max_drawdown * 100.0,
            cfg.max_oos_drawdown * 100.0
        ));
    }
    if m.oos_trades < cfg.min_oos_trades {
        r.push(format!("样本外交易 {} 笔 < {} 笔", m.oos_trades, cfg.min_oos_trades));
    }
    if m.is_sharpe > 0.0 {
        let decay = 1.0 - m.oos_sharpe / m.is_sharpe;
        if decay > cfg.max_sharpe_decay {
            r.push(format!(
                "夏普衰减 {:.0}% > {:.0}%(疑似过拟合)",
                decay * 100.0,
                cfg.max_sharpe_decay * 100.0
            ));
        }
    }
    if m.positive_ratio < cfg.min_positive_ratio {
        r.push(format!(
            "池内正收益占比 {:.0}% < {:.0}%",
            m.positive_ratio * 100.0,
            cfg.min_positive_ratio * 100.0
        ));
    }
    Verdict::from(r)
}

/// 观察期实际表现。
#[derive(Debug, Clone, PartialEq)]
pub struct PaperStats {
    pub days: i64,
    pub trades: usize,
    pub avg_trade_return: f64,
    pub max_drawdown: f64,
}

/// 观察期关(spec §10.5):时长、笔数、平均每笔收益不低于回测均值 − 1σ、回撤不超过回测。
#[allow(clippy::too_many_arguments)]
pub fn judge_paper(
    stats: &PaperStats,
    backtest_avg_trade_return: f64,
    backtest_trade_return_sd: f64,
    backtest_max_drawdown: f64,
    is_mover: bool,
    cfg: &AdmissionCfg,
) -> Verdict {
    let (min_days, min_trades) = if is_mover {
        (cfg.mover_paper_days, cfg.mover_paper_trades)
    } else {
        (cfg.paper_days, cfg.paper_trades)
    };
    let mut r = Vec::new();
    if stats.days < min_days {
        r.push(format!("观察期 {} 个交易日 < {} 日", stats.days, min_days));
    }
    if stats.trades < min_trades {
        r.push(format!("观察期 {} 笔 < {} 笔", stats.trades, min_trades));
    }
    let floor = backtest_avg_trade_return - backtest_trade_return_sd;
    if stats.avg_trade_return < floor {
        r.push(format!(
            "模拟盘平均每笔收益 {:.2}% 低于回测均值 − 1σ({:.2}%)",
            stats.avg_trade_return * 100.0,
            floor * 100.0
        ));
    }
    if stats.max_drawdown > backtest_max_drawdown {
        r.push(format!(
            "模拟盘最大回撤 {:.1}% 超过回测 {:.1}%",
            stats.max_drawdown * 100.0,
            backtest_max_drawdown * 100.0
        ));
    }
    Verdict::from(r)
}
```

- [ ] **Step 5: 实现 state.rs**

```rust
use crate::trade::admission::judge::Verdict;
use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::gate::Admission;
use crate::trade::model::{fmt_ts, StrategyStatus};
use crate::trade::store;
use crate::trade::ticket::Transition;
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection};

/// 条件转换:状态必须等于 `expect`,且策略属于该用户。成功则写事件。
pub fn update_status(
    conn: &Connection,
    user_id: i64,
    id: i64,
    expect: StrategyStatus,
    to: StrategyStatus,
    reason: &str,
    now: NaiveDateTime,
) -> Result<Transition> {
    let n = conn.execute(
        "UPDATE trade_strategies SET status = ?1, status_reason = ?2, updated_at = ?3
         WHERE id = ?4 AND user_id = ?5 AND status = ?6",
        params![
            to.as_str(),
            reason,
            fmt_ts(now),
            id,
            user_id,
            expect.as_str()
        ],
    )?;
    if n == 0 {
        return Ok(Transition::AlreadyHandled);
    }
    store::log_status_event(conn, id, expect, to, reason, now)?;
    Ok(Transition::Applied)
}

/// 提交评估:草稿 / 未通过 / 已暂停 → 回测中;异动类没有历史分时,直接进观察期。
pub fn submit_for_backtest(
    conn: &Connection,
    user_id: i64,
    id: i64,
    now: NaiveDateTime,
) -> Result<Transition> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(Transition::AlreadyHandled);
    };
    let to = if s.kind == "mover" {
        StrategyStatus::Paper
    } else {
        StrategyStatus::Backtesting
    };
    let reason = if s.kind == "mover" {
        "异动类无历史分时,直接进入观察期"
    } else {
        "提交前推回测"
    };
    for expect in [
        StrategyStatus::Draft,
        StrategyStatus::Failed,
        StrategyStatus::Suspended,
    ] {
        if s.status == expect {
            return update_status(conn, user_id, id, expect, to, reason, now);
        }
    }
    Ok(Transition::AlreadyHandled)
}

/// 落库回测结论并推进状态:通过 → 观察期,不通过 → 未通过。
#[allow(clippy::too_many_arguments)]
pub fn apply_backtest_verdict(
    conn: &Connection,
    user_id: i64,
    id: i64,
    metrics: &PoolMetrics,
    verdict: &Verdict,
    from: NaiveDate,
    to: NaiveDate,
    now: NaiveDateTime,
) -> Result<Transition> {
    let Some(s) = store::get_strategy(conn, user_id, id)? else {
        return Ok(Transition::AlreadyHandled);
    };
    store::save_eval(
        conn,
        id,
        &s.version_hash,
        "oos",
        &serde_json::to_string(metrics)?,
        from,
        to,
        now,
    )?;
    let (next, reason) = if verdict.passed {
        (StrategyStatus::Paper, "回测达标,进入观察期".to_string())
    } else {
        (StrategyStatus::Failed, verdict.reasons.join(";"))
    };
    update_status(
        conn,
        user_id,
        id,
        StrategyStatus::Backtesting,
        next,
        &reason,
        now,
    )
}

/// 策略状态 → 闸门准入。无策略(止盈止损 / 手动)为 NotRequired。
pub fn admission_for(
    conn: &Connection,
    user_id: i64,
    strategy_id: Option<i64>,
) -> Result<Admission> {
    let Some(id) = strategy_id else {
        return Ok(Admission::NotRequired);
    };
    Ok(match store::get_strategy(conn, user_id, id)? {
        Some(s) => match s.status {
            StrategyStatus::Admitted => Admission::Admitted,
            StrategyStatus::Paper => Admission::Probation,
            _ => Admission::Blocked,
        },
        None => Admission::Blocked,
    })
}
```

- [ ] **Step 6: 把 Task 1 的临时 SQL 换回 `update_status`**

`src/trade/store.rs` 测试 `strategy_crud_versioning_and_user_isolation` 中那行临时的 `c.execute("UPDATE trade_strategies SET status='backtesting' ...")` 换回:

```rust
        crate::trade::admission::state::update_status(&c, 1, id, StrategyStatus::Draft, StrategyStatus::Backtesting, "提交", at(16, 9, 1)).unwrap();
```

- [ ] **Step 7: 运行确认通过 + 全量门禁**

Run: `cargo test --lib trade::`
Expected: 全部 PASS(judge 5、state 4、config +1)

Run: `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets --no-fail-fast`
Expected: fmt 干净;clippy 仅 3 个既有问题;测试除既有 `realtime_pipeline::full_day_flow_from_detection_to_summary` 外全部通过

- [ ] **Step 8: Commit**

```bash
git add src/trade
git commit -m "feat(trade): 准入阈值配置、回测/观察期判定与策略状态机"
```

---

## 完成标准

- [ ] 基金与既有股票测试期望值未改动
- [ ] `trade::admission` 全部单元测试通过;测试不访问网络
- [ ] CI 门禁符合 Global Constraints

## 后续计划(不在本计划范围)

- **计划 3b**:评估任务队列与 `trade-eval` 线程(前推回测在后台跑、进度可见)、观察期统计与 watchdog(滚动回撤 / 胜率 / 连亏,自动暂停、每月重跑)、成绩单(样本内 / 样本外 / 模拟盘 / 实盘 + 执行损耗)、日线策略信号(15:30 计算、次日 09:25 发出,`admission_for` 接入 `submit_signal`)、`stock/recommend.rs` 迁移到 A 股口径
- **计划 4**:网页策略管理与成绩单展示、`/trade` 确认页、持仓校准、风控设置
- 已知遗留(计划 2b 记录):交易日历节假日、推送串行、Markdown 转义、报价新鲜度绝对值等

## 执行后遗留项(来自任务审查与最终审查,须被后续计划吸收)

**计划 3b(评估线程与准入流水线):**
- `run_pool` 无进度回调与取消:50 只 × 100 组合 × 6 窗 = 3 万次回测在一次同步调用里,线程无法上报进度 / 停止 / 写心跳(spec §11 要求「进度可见」)
- 夏普衰减用两个独立中位数相除(`median(oos)/median(is)`),应改为逐只算衰减再取中位数
- `metric_of` 对未知 metric 名静默回退到 sharpe;`WalkForwardCfg.metric` 应校验
- `expand_grid` 每只股票重复展开;训练窗每个组合 clone 整段 K 线
- `NewStrategy` 无校验:空股票池、重复代码、未知 kind、无法解析的 grid 都要等回测失败才暴露
- `aggregate()` 默认把 `requested` 设为成功只数,绕过覆盖率闸门;非 `run_pool` 调用方需自行填写
- 测试缺口:`metric_of` 真正影响选参、不同窗口选出不同参数、端到端(提交 → 回测 → 判定 → 状态)用例、`admission_for` 的 Backtesting/Failed 分支
- 判定夹具里 `positive_ratio` 与 `codes` 不自洽(真实 `aggregate` 不会产生)

**计划 4 之前必须完成(在 3b 内):**
- `update_status` 没有合法转换表,可从 `Draft` 直跳 `Admitted`;网页一旦允许传入 from/to,就等于绕过回测直接开实盘
- `oos_annualized` 计算但未参与判定(spec §10.4 文字为「年化收益」),口径需统一

**已知口径说明(已写入本计划细化 6–8):** 资金口径按实际投入度量;`data_years` 与样本外 `years` 分离;池内覆盖率与每只中位交易笔数参与判定。

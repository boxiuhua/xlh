//! 交易表结构与账户 / 风控 / 持仓读写。所有查询按 user_id 隔离。

use crate::trade::model::{
    fmt_ts, parse_ts, strategy_version_hash, Account, AccountState, EvalJob, EvalKind, JobStatus,
    NewStrategy, Position, Quote, RiskRules, StrategyDef, StrategyStatus, DATE_FMT,
};
use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS trade_accounts (
  user_id        INTEGER NOT NULL,
  account        TEXT NOT NULL,
  total_capital  REAL NOT NULL,
  available_cash REAL NOT NULL,
  updated_at     TEXT NOT NULL,
  PRIMARY KEY (user_id, account)
);
CREATE TABLE IF NOT EXISTS trade_risk_rules (
  user_id    INTEGER PRIMARY KEY,
  rules_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS trade_signals (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id       INTEGER NOT NULL,
  source        TEXT NOT NULL,
  strategy_id   INTEGER,
  code          TEXT NOT NULL,
  name          TEXT,
  side          TEXT NOT NULL,
  scope         TEXT NOT NULL DEFAULT 'both',
  ref_price     REAL NOT NULL,
  reason        TEXT NOT NULL,
  ai_note       TEXT,
  dedup_key     TEXT NOT NULL,
  suggest_cash  REAL,
  suggest_qty   INTEGER,
  status        TEXT NOT NULL DEFAULT 'new',
  reject_reason TEXT,
  created_at    TEXT NOT NULL,
  UNIQUE (user_id, dedup_key)
);
CREATE TABLE IF NOT EXISTS trade_tickets (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id       INTEGER NOT NULL,
  signal_id     INTEGER NOT NULL,
  account       TEXT NOT NULL,
  code          TEXT NOT NULL,
  side          TEXT NOT NULL,
  suggest_price REAL NOT NULL,
  qty           INTEGER NOT NULL,
  filled_qty    INTEGER NOT NULL DEFAULT 0,
  expires_at    TEXT NOT NULL,
  deviation_th  REAL NOT NULL,
  status        TEXT NOT NULL,
  urgency       INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  confirmed_at  TEXT,
  ignore_reason TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_tickets_user_status ON trade_tickets(user_id, status);
CREATE TABLE IF NOT EXISTS trade_fills (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  ticket_id    INTEGER NOT NULL,
  user_id      INTEGER NOT NULL,
  account      TEXT NOT NULL,
  code         TEXT NOT NULL,
  side         TEXT NOT NULL,
  price        REAL NOT NULL,
  qty          INTEGER NOT NULL,
  fee          REAL NOT NULL,
  realized_pnl REAL,
  source       TEXT NOT NULL,
  filled_at    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_trade_fills_user_time ON trade_fills(user_id, filled_at);
CREATE TABLE IF NOT EXISTS trade_positions (
  user_id          INTEGER NOT NULL,
  account          TEXT NOT NULL,
  code             TEXT NOT NULL,
  qty              INTEGER NOT NULL,
  avg_cost         REAL NOT NULL,
  today_bought_qty INTEGER NOT NULL DEFAULT 0,
  last_buy_date    TEXT,
  stop_loss        REAL,
  take_profit      REAL,
  trailing_pct     REAL,
  trailing_high    REAL,
  updated_at       TEXT NOT NULL,
  PRIMARY KEY (user_id, account, code)
);
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
CREATE INDEX IF NOT EXISTS idx_trade_strategy_events ON trade_strategy_events(strategy_id, id);
CREATE TABLE IF NOT EXISTS trade_eval_jobs (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     INTEGER NOT NULL,
  strategy_id INTEGER NOT NULL,
  kind        TEXT NOT NULL,
  status      TEXT NOT NULL,
  progress    TEXT,
  error       TEXT,
  created_at  TEXT NOT NULL,
  started_at  TEXT,
  finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_trade_eval_jobs_status ON trade_eval_jobs(status, id);
-- F5:去重是数据库不变量,而不只是 enqueue_eval 里的一次 SELECT——并发入队时
-- SELECT 判重会有竞态,真正兜底的是这条唯一索引;上面的 SELECT 只是快路径。
CREATE UNIQUE INDEX IF NOT EXISTS idx_trade_eval_jobs_pending
  ON trade_eval_jobs(user_id, strategy_id, kind) WHERE status IN ('queued', 'running');

CREATE TABLE IF NOT EXISTS trade_calendar (
  day        TEXT PRIMARY KEY,
  is_open    INTEGER NOT NULL,
  checked_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trade_settings (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS trade_strategy_plans (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id      INTEGER NOT NULL,
  strategy_id  INTEGER NOT NULL,
  version_hash TEXT NOT NULL,
  code         TEXT NOT NULL,
  side         TEXT,
  cash         REAL,
  basis_date   TEXT NOT NULL,
  reason       TEXT NOT NULL,
  status       TEXT NOT NULL,
  note         TEXT,
  created_at   TEXT NOT NULL,
  settled_at   TEXT
);
-- 每个策略每只股票每个基准日只算一次:重启、重试都靠它幂等
CREATE UNIQUE INDEX IF NOT EXISTS idx_trade_strategy_plans_key
  ON trade_strategy_plans(strategy_id, code, basis_date);
CREATE INDEX IF NOT EXISTS idx_trade_strategy_plans_status ON trade_strategy_plans(status, basis_date);

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
"#;

/// 表已存在但缺列时补建:`CREATE TABLE IF NOT EXISTS` 对已存在的旧表是空操作,
/// 新增列(如 `trade_signals.scope`)不会自动出现,后续显式列清单的 INSERT 会报错。
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let has_column = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|n| n == column);
    if !has_column {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
    }
    Ok(())
}

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建交易表失败")?;
    ensure_column(
        conn,
        "trade_signals",
        "scope",
        "TEXT NOT NULL DEFAULT 'both'",
    )
    .context("补建 trade_signals.scope 失败")?;
    ensure_column(
        conn,
        "trade_eval_jobs",
        "cancel_requested",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .context("补建 trade_eval_jobs.cancel_requested 失败")?;
    Ok(())
}

pub fn set_capital(
    conn: &Connection,
    user_id: i64,
    account: Account,
    total: f64,
    now: NaiveDateTime,
) -> Result<()> {
    if !(total.is_finite() && total > 0.0) {
        return Err(anyhow!("账户总资金必须为正数"));
    }
    // 已存在时按总资金变化量调整可用资金(已占用资金不变)
    conn.execute(
        "INSERT INTO trade_accounts (user_id, account, total_capital, available_cash, updated_at)
         VALUES (?1, ?2, ?3, ?3, ?4)
         ON CONFLICT(user_id, account) DO UPDATE SET
           available_cash = available_cash + (excluded.total_capital - total_capital),
           total_capital  = excluded.total_capital,
           updated_at     = excluded.updated_at",
        params![user_id, account.as_str(), total, fmt_ts(now)],
    )
    .context("设置账户资金失败")?;
    Ok(())
}

pub fn get_account(
    conn: &Connection,
    user_id: i64,
    account: Account,
) -> Result<Option<AccountState>> {
    conn.query_row(
        "SELECT total_capital, available_cash FROM trade_accounts
         WHERE user_id = ?1 AND account = ?2",
        params![user_id, account.as_str()],
        |r| {
            Ok(AccountState {
                user_id,
                account,
                total_capital: r.get(0)?,
                available_cash: r.get(1)?,
            })
        },
    )
    .optional()
    .context("读取账户失败")
}

pub fn add_cash(
    conn: &Connection,
    user_id: i64,
    account: Account,
    delta: f64,
    now: NaiveDateTime,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE trade_accounts SET available_cash = available_cash + ?1, updated_at = ?2
         WHERE user_id = ?3 AND account = ?4",
        params![delta, fmt_ts(now), user_id, account.as_str()],
    )?;
    if n == 0 {
        return Err(anyhow!("未设置账户资金"));
    }
    Ok(())
}

pub fn get_risk_rules(conn: &Connection, user_id: i64) -> Result<RiskRules> {
    let json: Option<String> = conn
        .query_row(
            "SELECT rules_json FROM trade_risk_rules WHERE user_id = ?1",
            [user_id],
            |r| r.get(0),
        )
        .optional()?;
    match json {
        Some(j) => serde_json::from_str(&j).context("风控规则格式错误"),
        None => Ok(RiskRules::default()),
    }
}

pub fn save_risk_rules(
    conn: &Connection,
    user_id: i64,
    rules: &RiskRules,
    now: NaiveDateTime,
) -> Result<()> {
    rules.validate()?;
    conn.execute(
        "INSERT INTO trade_risk_rules (user_id, rules_json, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(user_id) DO UPDATE SET rules_json = excluded.rules_json, updated_at = excluded.updated_at",
        params![user_id, serde_json::to_string(rules)?, fmt_ts(now)],
    )?;
    Ok(())
}

const POSITION_COLS: &str = "code, qty, avg_cost, today_bought_qty, last_buy_date, \
                             stop_loss, take_profit, trailing_pct, trailing_high";

struct RawPosition {
    code: String,
    qty: i64,
    avg_cost: f64,
    today_bought_qty: i64,
    last_buy_date: Option<String>,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
    trailing_high: Option<f64>,
}

fn read_raw_position_at(r: &Row, o: usize) -> rusqlite::Result<RawPosition> {
    Ok(RawPosition {
        code: r.get(o)?,
        qty: r.get(o + 1)?,
        avg_cost: r.get(o + 2)?,
        today_bought_qty: r.get(o + 3)?,
        last_buy_date: r.get(o + 4)?,
        stop_loss: r.get(o + 5)?,
        take_profit: r.get(o + 6)?,
        trailing_pct: r.get(o + 7)?,
        trailing_high: r.get(o + 8)?,
    })
}

impl RawPosition {
    fn into_position(self, user_id: i64, account: Account) -> Result<Position> {
        let last_buy_date = self
            .last_buy_date
            .map(|s| NaiveDate::parse_from_str(&s, DATE_FMT))
            .transpose()
            .context("持仓买入日期格式错误")?;
        Ok(Position {
            user_id,
            account,
            code: self.code,
            qty: self.qty.max(0) as u64,
            avg_cost: self.avg_cost,
            today_bought_qty: self.today_bought_qty.max(0) as u64,
            last_buy_date,
            stop_loss: self.stop_loss,
            take_profit: self.take_profit,
            trailing_pct: self.trailing_pct,
            trailing_high: self.trailing_high,
        })
    }
}

pub fn get_position(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
) -> Result<Option<Position>> {
    let raw = conn
        .query_row(
            &format!(
                "SELECT {POSITION_COLS} FROM trade_positions
                 WHERE user_id = ?1 AND account = ?2 AND code = ?3"
            ),
            params![user_id, account.as_str(), code],
            |r| read_raw_position_at(r, 0),
        )
        .optional()?;
    raw.map(|r| r.into_position(user_id, account)).transpose()
}

pub fn list_positions(conn: &Connection, user_id: i64, account: Account) -> Result<Vec<Position>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {POSITION_COLS} FROM trade_positions
         WHERE user_id = ?1 AND account = ?2 ORDER BY code"
    ))?;
    let raws = stmt
        .query_map(params![user_id, account.as_str()], |r| {
            read_raw_position_at(r, 0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter()
        .map(|r| r.into_position(user_id, account))
        .collect()
}

pub fn upsert_position(conn: &Connection, p: &Position, now: NaiveDateTime) -> Result<()> {
    if p.qty == 0 {
        conn.execute(
            "DELETE FROM trade_positions WHERE user_id = ?1 AND account = ?2 AND code = ?3",
            params![p.user_id, p.account.as_str(), p.code],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO trade_positions (user_id, account, code, qty, avg_cost, today_bought_qty,
           last_buy_date, stop_loss, take_profit, trailing_pct, trailing_high, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT(user_id, account, code) DO UPDATE SET
           qty = excluded.qty, avg_cost = excluded.avg_cost,
           today_bought_qty = excluded.today_bought_qty, last_buy_date = excluded.last_buy_date,
           stop_loss = excluded.stop_loss, take_profit = excluded.take_profit,
           trailing_pct = excluded.trailing_pct, trailing_high = excluded.trailing_high,
           updated_at = excluded.updated_at",
        params![
            p.user_id,
            p.account.as_str(),
            p.code,
            p.qty as i64,
            p.avg_cost,
            p.today_bought_qty as i64,
            p.last_buy_date.map(|d| d.format(DATE_FMT).to_string()),
            p.stop_loss,
            p.take_profit,
            p.trailing_pct,
            p.trailing_high,
            fmt_ts(now),
        ],
    )?;
    Ok(())
}

/// 删除一行持仓(实盘持仓校准归零用)。返回是否确有该行。
pub fn delete_position(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
) -> Result<bool> {
    let n = conn.execute(
        "DELETE FROM trade_positions WHERE user_id = ?1 AND account = ?2 AND code = ?3",
        params![user_id, account.as_str(), code],
    )?;
    Ok(n > 0)
}

/// 持仓校准留痕(计划 4a):改前 / 改后各以 `Position` 的 JSON 快照存,没有则 NULL。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PositionAdjust {
    pub id: i64,
    pub code: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
    pub reason: String,
    pub at: NaiveDateTime,
}

#[allow(clippy::too_many_arguments)]
pub fn record_position_adjust(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
    reason: &str,
    now: NaiveDateTime,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO trade_position_adjusts (user_id, account, code, before_json, after_json, reason, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            user_id,
            account.as_str(),
            code,
            before.map(|v| v.to_string()),
            after.map(|v| v.to_string()),
            reason,
            fmt_ts(now),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_adjusts(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<PositionAdjust>> {
    let mut stmt = conn.prepare(
        "SELECT id, code, before_json, after_json, reason, at FROM trade_position_adjusts
         WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let raws = stmt
        .query_map(params![user_id, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter()
        .map(|(id, code, before, after, reason, at)| {
            Ok(PositionAdjust {
                id,
                code,
                before: before
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .context("持仓校准前值格式错误")?,
                after: after
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .context("持仓校准后值格式错误")?,
                reason,
                at: parse_ts(&at)?,
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub fn set_exit_levels(
    conn: &Connection,
    user_id: i64,
    account: Account,
    code: &str,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    trailing_pct: Option<f64>,
    now: NaiveDateTime,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE trade_positions SET stop_loss = ?1, take_profit = ?2, trailing_pct = ?3, updated_at = ?4
         WHERE user_id = ?5 AND account = ?6 AND code = ?7",
        params![stop_loss, take_profit, trailing_pct, fmt_ts(now), user_id, account.as_str(), code],
    )?;
    Ok(n > 0)
}

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
    user_ids(
        conn,
        "SELECT DISTINCT user_id FROM trade_positions ORDER BY user_id",
    )
}

/// 持有实盘仓位的用户(监听中断告警只发给这些用户,模拟盘持仓无需线下操作)。
pub fn users_with_real_positions(conn: &Connection) -> Result<Vec<i64>> {
    user_ids(
        conn,
        "SELECT DISTINCT user_id FROM trade_positions WHERE account = 'real' ORDER BY user_id",
    )
}

/// 有策略定义的用户 id,升序。
pub fn users_with_strategies(conn: &Connection) -> Result<Vec<i64>> {
    user_ids(
        conn,
        "SELECT DISTINCT user_id FROM trade_strategies ORDER BY user_id",
    )
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

struct RawQuote {
    code: String,
    price: f64,
    limit_up: Option<f64>,
    limit_down: Option<f64>,
    ts: String,
}

pub fn get_quote(conn: &Connection, code: &str) -> Result<Option<Quote>> {
    let raw: Option<RawQuote> = conn
        .query_row(
            "SELECT code, price, limit_up, limit_down, ts FROM trade_quotes WHERE code = ?1",
            [code],
            |r| {
                Ok(RawQuote {
                    code: r.get(0)?,
                    price: r.get(1)?,
                    limit_up: r.get(2)?,
                    limit_down: r.get(3)?,
                    ts: r.get(4)?,
                })
            },
        )
        .optional()?;
    raw.map(|r| {
        Ok(Quote {
            code: r.code,
            price: r.price,
            limit_up: r.limit_up,
            limit_down: r.limit_down,
            ts: parse_ts(&r.ts)?,
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

#[allow(clippy::type_complexity)]
fn read_strategy(
    r: &Row,
) -> rusqlite::Result<(
    i64,
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

const STRATEGY_COLS: &str =
    "id, user_id, name, kind, grid_toml, pool_json, version_hash, status, status_reason, updated_at";

#[allow(clippy::type_complexity)]
fn to_strategy(
    raw: (
        i64,
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
    ),
) -> Result<StrategyDef> {
    let (
        id,
        user_id,
        name,
        kind,
        grid_toml,
        pool_json,
        version_hash,
        status,
        status_reason,
        updated_at,
    ) = raw;
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

/// 策略定义校验:名称、股票池、类型与网格都要在入库前拦住,
/// 否则错误要等到跑完整轮回测才以「未通过」的形式暴露。
pub fn validate_new_strategy(s: &NewStrategy) -> Result<()> {
    if s.name.trim().is_empty() {
        return Err(anyhow!("策略名称不能为空"));
    }
    if !matches!(
        s.kind.as_str(),
        "dca" | "smart_dca" | "trend" | "rsi" | "adaptive" | "mover"
    ) {
        return Err(anyhow!("未知策略类型: {}", s.kind));
    }
    if s.pool.is_empty() {
        return Err(anyhow!("股票池不能为空"));
    }
    let mut seen = std::collections::HashSet::new();
    for code in &s.pool {
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(anyhow!("股票代码须为 6 位数字: {code}"));
        }
        if !seen.insert(code.as_str()) {
            return Err(anyhow!("股票池含重复代码: {code}"));
        }
    }
    let grid: toml::Table = s
        .grid_toml
        .parse()
        .map_err(|e| anyhow!("参数网格解析失败: {e}"))?;
    if grid.is_empty() {
        return Err(anyhow!("参数网格不能为空"));
    }
    Ok(())
}

pub fn create_strategy(conn: &Connection, s: &NewStrategy, now: NaiveDateTime) -> Result<i64> {
    validate_new_strategy(s)?;
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

/// 该用户观察期 / 已准入、且股票池含 `code` 的异动策略;已准入优先,同状态取 id 最小。
pub fn active_mover_strategy(
    conn: &Connection,
    user_id: i64,
    code: &str,
) -> Result<Option<StrategyDef>> {
    let mut best: Option<StrategyDef> = None;
    for s in list_strategies(conn, user_id)? {
        if s.kind != "mover" || !s.pool.iter().any(|c| c == code) {
            continue;
        }
        let rank = match s.status {
            StrategyStatus::Admitted => 0,
            StrategyStatus::Paper => 1,
            _ => continue,
        };
        let better = match &best {
            None => true,
            Some(b) => {
                let b_rank = if b.status == StrategyStatus::Admitted {
                    0
                } else {
                    1
                };
                (rank, s.id) < (b_rank, b.id)
            }
        };
        if better {
            best = Some(s);
        }
    }
    Ok(best)
}

/// 该用户观察期 / 已准入的异动策略股票池并集(去重、排序)。
pub fn active_mover_pools(conn: &Connection, user_id: i64) -> Result<Vec<String>> {
    let mut codes: Vec<String> = list_strategies(conn, user_id)?
        .into_iter()
        .filter(|s| {
            s.kind == "mover"
                && matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted)
        })
        .flat_map(|s| s.pool)
        .collect();
    codes.sort();
    codes.dedup();
    Ok(codes)
}

/// `update_definition` 的结果(spec §10.1 + F11):区分「无变化」「仅改名」
/// 「换版本」「策略不存在 / 不属于该用户」,调用方据此决定是否需要重新提交回测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionUpdate {
    NotFound,
    Unchanged,
    /// 仅名称变化:类型 / 网格 / 股票池未变,版本与状态都保留
    Renamed,
    /// 类型 / 网格 / 股票池发生变化:换版本哈希,状态重置为草稿
    Reversioned,
}

/// 定义变更(spec §10.1)。版本哈希(类型 + 网格 + 股票池)不变时只是改名,
/// 不影响已有的回测结论与当前状态;版本哈希改变才重置为草稿,因为旧的回测/
/// 观察期结论不再适用于新定义。
pub fn update_definition(
    conn: &Connection,
    user_id: i64,
    id: i64,
    s: &NewStrategy,
    now: NaiveDateTime,
) -> Result<DefinitionUpdate> {
    validate_new_strategy(s)?;
    let Some(cur) = get_strategy(conn, user_id, id)? else {
        return Ok(DefinitionUpdate::NotFound);
    };
    let hash = strategy_version_hash(&s.kind, &s.grid_toml, &s.pool);
    if hash == cur.version_hash {
        if s.name == cur.name {
            return Ok(DefinitionUpdate::Unchanged);
        }
        conn.execute(
            "UPDATE trade_strategies SET name = ?1, updated_at = ?2 WHERE id = ?3 AND user_id = ?4",
            params![s.name, fmt_ts(now), id, user_id],
        )?;
        return Ok(DefinitionUpdate::Renamed);
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
    Ok(DefinitionUpdate::Reversioned)
}

/// 子表(评估 / 事件)按父行 `trade_strategies` 的 user_id 隔离的公共谓词:
/// 策略必须存在且属于该用户,否则该 strategy_id 下所有子行都视为不可见。
const OWNED_BY_USER: &str =
    "EXISTS (SELECT 1 FROM trade_strategies s WHERE s.id = ?1 AND s.user_id = ?2)";

#[allow(clippy::too_many_arguments)]
pub fn save_eval(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
    version_hash: &str,
    stage: &str,
    metrics_json: &str,
    data_from: NaiveDate,
    data_to: NaiveDate,
    now: NaiveDateTime,
) -> Result<i64> {
    let n = conn.execute(
        &format!(
            "INSERT INTO trade_strategy_evals (strategy_id, version_hash, stage, metrics_json, data_from, data_to, run_at)
             SELECT ?1, ?3, ?4, ?5, ?6, ?7, ?8 WHERE {OWNED_BY_USER}"
        ),
        params![
            strategy_id,
            user_id,
            version_hash,
            stage,
            metrics_json,
            data_from.format(DATE_FMT).to_string(),
            data_to.format(DATE_FMT).to_string(),
            fmt_ts(now),
        ],
    )?;
    if n == 0 {
        return Err(anyhow!("策略不存在或不属于该用户"));
    }
    Ok(conn.last_insert_rowid())
}

pub fn latest_eval(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
    stage: &str,
) -> Result<Option<(String, NaiveDateTime)>> {
    let raw: Option<(String, String)> = conn
        .query_row(
            &format!(
                "SELECT metrics_json, run_at FROM trade_strategy_evals
                 WHERE strategy_id = ?1 AND stage = ?3 AND {OWNED_BY_USER} ORDER BY id DESC LIMIT 1"
            ),
            params![strategy_id, user_id, stage],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    raw.map(|(j, at)| Ok((j, parse_ts(&at)?))).transpose()
}

pub fn log_status_event(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
    from: StrategyStatus,
    to: StrategyStatus,
    reason: &str,
    now: NaiveDateTime,
) -> Result<()> {
    let n = conn.execute(
        &format!(
            "INSERT INTO trade_strategy_events (strategy_id, from_status, to_status, reason, at)
             SELECT ?1, ?3, ?4, ?5, ?6 WHERE {OWNED_BY_USER}"
        ),
        params![
            strategy_id,
            user_id,
            from.as_str(),
            to.as_str(),
            reason,
            fmt_ts(now)
        ],
    )?;
    if n == 0 {
        return Err(anyhow!("策略不存在或不属于该用户"));
    }
    Ok(())
}

/// 该策略最近一次「转入指定状态」的时间(观察期起点等用)。
pub fn last_transition_at(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
    to: StrategyStatus,
) -> Result<Option<NaiveDateTime>> {
    let s: Option<String> = conn.query_row(
        &format!(
            "SELECT MAX(at) FROM trade_strategy_events
             WHERE strategy_id = ?1 AND to_status = ?3 AND {OWNED_BY_USER}"
        ),
        params![strategy_id, user_id, to.as_str()],
        |r| r.get(0),
    )?;
    s.as_deref().map(parse_ts).transpose()
}

#[allow(clippy::type_complexity)]
pub fn list_status_events(
    conn: &Connection,
    strategy_id: i64,
    user_id: i64,
) -> Result<Vec<(StrategyStatus, StrategyStatus, String, NaiveDateTime)>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT from_status, to_status, reason, at FROM trade_strategy_events
         WHERE strategy_id = ?1 AND {OWNED_BY_USER} ORDER BY id"
    ))?;
    let rows = stmt
        .query_map(params![strategy_id, user_id], |r| {
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

const JOB_COLS: &str =
    "id, user_id, strategy_id, kind, status, progress, error, created_at, started_at, finished_at";

#[allow(clippy::type_complexity)]
fn read_job(
    r: &Row,
) -> rusqlite::Result<(
    i64,
    i64,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
    ))
}

#[allow(clippy::type_complexity)]
fn to_job(
    raw: (
        i64,
        i64,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    ),
) -> Result<EvalJob> {
    let (
        id,
        user_id,
        strategy_id,
        kind,
        status,
        progress,
        error,
        created_at,
        started_at,
        finished_at,
    ) = raw;
    Ok(EvalJob {
        id,
        user_id,
        strategy_id,
        kind: EvalKind::parse(&kind)?,
        status: JobStatus::parse(&status)?,
        progress,
        error,
        created_at: parse_ts(&created_at)?,
        started_at: started_at.as_deref().map(parse_ts).transpose()?,
        finished_at: finished_at.as_deref().map(parse_ts).transpose()?,
    })
}

/// 入队一个评估任务;同策略同类型已排队 / 运行中则返回 None(幂等)。
///
/// F5:真正的不变量是 `idx_trade_eval_jobs_pending` 唯一索引——这里的 SELECT
/// 判重只是快路径(避免大多数重复请求触发一次注定失败的 INSERT),两次并发
/// 入队之间仍可能都通过 SELECT 检查再同时 INSERT,此时唯一索引会让其中一次
/// INSERT 失败,下面把这种违反约束映射成 `Ok(None)`,与快路径命中语义一致。
pub fn enqueue_eval(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
    kind: EvalKind,
    now: NaiveDateTime,
) -> Result<Option<i64>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM trade_eval_jobs
           WHERE user_id = ?1 AND strategy_id = ?2 AND kind = ?3 AND status IN ('queued', 'running'))",
        params![user_id, strategy_id, kind.as_str()],
        |r| r.get(0),
    )?;
    if exists {
        return Ok(None);
    }
    match conn.execute(
        "INSERT INTO trade_eval_jobs (user_id, strategy_id, kind, status, created_at)
         VALUES (?1, ?2, ?3, 'queued', ?4)",
        params![user_id, strategy_id, kind.as_str(), fmt_ts(now)],
    ) {
        Ok(_) => Ok(Some(conn.last_insert_rowid())),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// 进程重启时回收上次中断的任务:`running` 没有租约,不回收会让同策略同类型
/// 永远无法再次入队(enqueue_eval 按 queued/running 去重)。
pub fn reclaim_stale_jobs(conn: &Connection, now: NaiveDateTime) -> Result<usize> {
    let n = conn.execute(
        "UPDATE trade_eval_jobs SET status = 'failed', error = '进程重启,任务中断', finished_at = ?1
         WHERE status = 'running'",
        params![fmt_ts(now)],
    )?;
    Ok(n)
}

/// 领取最早的排队任务并置为运行中。无任务返回 None。
pub fn claim_next_job(conn: &Connection, now: NaiveDateTime) -> Result<Option<EvalJob>> {
    conn.query_row(
        &format!(
            "UPDATE trade_eval_jobs SET status = 'running', started_at = ?1
             WHERE id = (SELECT id FROM trade_eval_jobs WHERE status = 'queued' ORDER BY id LIMIT 1)
             RETURNING {JOB_COLS}"
        ),
        [fmt_ts(now)],
        read_job,
    )
    .optional()?
    .map(to_job)
    .transpose()
}

/// 仅供评估线程调用:不按 user_id 隔离;面向用户的取消 / 重试必须先用 EvalJob.user_id 校验归属。
pub fn set_job_progress(
    conn: &Connection,
    job_id: i64,
    progress: &str,
    _now: NaiveDateTime,
) -> Result<()> {
    conn.execute(
        "UPDATE trade_eval_jobs SET progress = ?1 WHERE id = ?2",
        params![progress, job_id],
    )?;
    Ok(())
}

/// 结束任务:`error` 为 None 记 done,否则记 failed。
/// 仅供评估线程调用:不按 user_id 隔离;面向用户的取消 / 重试必须先用 EvalJob.user_id 校验归属。
pub fn finish_job(
    conn: &Connection,
    job_id: i64,
    error: Option<&str>,
    now: NaiveDateTime,
) -> Result<()> {
    let status = if error.is_some() {
        JobStatus::Failed
    } else {
        JobStatus::Done
    };
    conn.execute(
        "UPDATE trade_eval_jobs SET status = ?1, error = ?2, finished_at = ?3 WHERE id = ?4",
        params![status.as_str(), error, fmt_ts(now), job_id],
    )?;
    Ok(())
}

/// 按 id 查单个任务,按 user_id 隔离(评估线程测试与手动排障用)。
pub fn get_job(conn: &Connection, user_id: i64, job_id: i64) -> Result<Option<EvalJob>> {
    conn.query_row(
        &format!("SELECT {JOB_COLS} FROM trade_eval_jobs WHERE id = ?1 AND user_id = ?2"),
        params![job_id, user_id],
        read_job,
    )
    .optional()?
    .map(to_job)
    .transpose()
}

pub fn list_jobs(conn: &Connection, user_id: i64, limit: usize) -> Result<Vec<EvalJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLS} FROM trade_eval_jobs WHERE user_id = ?1 ORDER BY id DESC LIMIT ?2"
    ))?;
    let raws = stmt
        .query_map(params![user_id, limit as i64], read_job)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(to_job).collect()
}

/// 任务是否被标记为待取消(评估取消,design decision 7)。不按 user_id 隔离:
/// 只供 worker 的 `on_code` 闭包高频轮询,归属校验已在 `cancel_job` 写入时做过。
pub fn job_cancel_requested(conn: &Connection, job_id: i64) -> Result<bool> {
    let flag: i64 = conn.query_row(
        "SELECT cancel_requested FROM trade_eval_jobs WHERE id = ?1",
        params![job_id],
        |r| r.get(0),
    )?;
    Ok(flag != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::model::*;
    use chrono::{NaiveDate, NaiveDateTime};

    pub(crate) fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
    }

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, d).unwrap()
    }

    #[test]
    fn capital_set_then_adjust_keeps_cash_delta() {
        let c = db();
        assert!(get_account(&c, 1, Account::Real).unwrap().is_none());
        set_capital(&c, 1, Account::Real, 100_000.0, at(15, 9, 0)).unwrap();
        add_cash(&c, 1, Account::Real, -30_000.0, at(15, 10, 0)).unwrap();
        set_capital(&c, 1, Account::Real, 120_000.0, at(15, 11, 0)).unwrap();
        let a = get_account(&c, 1, Account::Real).unwrap().unwrap();
        assert!((a.total_capital - 120_000.0).abs() < 1e-9);
        assert!((a.available_cash - 90_000.0).abs() < 1e-9, "70000 + 20000");
        assert!(
            get_account(&c, 2, Account::Real).unwrap().is_none(),
            "用户隔离"
        );
        assert!(set_capital(&c, 1, Account::Real, 0.0, at(15, 11, 0)).is_err());
        assert!(add_cash(&c, 2, Account::Real, 1.0, at(15, 11, 0)).is_err());
    }

    #[test]
    fn risk_rules_default_then_saved() {
        let c = db();
        assert_eq!(get_risk_rules(&c, 1).unwrap(), RiskRules::default());
        let r = RiskRules {
            cooldown_min: 15,
            ..RiskRules::default()
        };
        save_risk_rules(&c, 1, &r, at(15, 9, 0)).unwrap();
        assert_eq!(get_risk_rules(&c, 1).unwrap().cooldown_min, 15);
        assert_eq!(get_risk_rules(&c, 2).unwrap(), RiskRules::default());
    }

    #[test]
    fn position_upsert_list_and_delete_on_zero() {
        let c = db();
        let mut p = Position::empty(1, Account::Real, "600000");
        p.qty = 1000;
        p.avg_cost = 10.0051;
        p.today_bought_qty = 1000;
        p.last_buy_date = Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
        p.stop_loss = Some(9.2);
        upsert_position(&c, &p, at(15, 10, 0)).unwrap();
        assert_eq!(
            get_position(&c, 1, Account::Real, "600000").unwrap(),
            Some(p.clone())
        );
        assert!(get_position(&c, 1, Account::Paper, "600000")
            .unwrap()
            .is_none());
        assert_eq!(list_positions(&c, 1, Account::Real).unwrap().len(), 1);

        assert!(set_exit_levels(
            &c,
            1,
            Account::Real,
            "600000",
            Some(9.5),
            Some(12.0),
            Some(0.05),
            at(15, 11, 0)
        )
        .unwrap());
        let got = get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .unwrap();
        assert_eq!(
            (got.stop_loss, got.take_profit, got.trailing_pct),
            (Some(9.5), Some(12.0), Some(0.05))
        );
        assert!(!set_exit_levels(
            &c,
            2,
            Account::Real,
            "600000",
            None,
            None,
            None,
            at(15, 11, 0)
        )
        .unwrap());

        p.qty = 0;
        upsert_position(&c, &p, at(15, 12, 0)).unwrap();
        assert!(get_position(&c, 1, Account::Real, "600000")
            .unwrap()
            .is_none());
    }

    #[test]
    fn migrate_adds_scope_to_legacy_trade_signals() {
        let c = Connection::open_in_memory().unwrap();
        // 旧版 trade_signals(无 scope 列),模拟历史库升级前的状态。
        c.execute_batch(
            "CREATE TABLE trade_signals (
              id            INTEGER PRIMARY KEY AUTOINCREMENT,
              user_id       INTEGER NOT NULL,
              source        TEXT NOT NULL,
              strategy_id   INTEGER,
              code          TEXT NOT NULL,
              name          TEXT,
              side          TEXT NOT NULL,
              ref_price     REAL NOT NULL,
              reason        TEXT NOT NULL,
              ai_note       TEXT,
              dedup_key     TEXT NOT NULL,
              suggest_cash  REAL,
              suggest_qty   INTEGER,
              status        TEXT NOT NULL DEFAULT 'new',
              reject_reason TEXT,
              created_at    TEXT NOT NULL,
              UNIQUE (user_id, dedup_key)
            );",
        )
        .unwrap();

        migrate(&c).unwrap();

        let cols: Vec<String> = {
            let mut stmt = c.prepare("PRAGMA table_info(trade_signals)").unwrap();
            stmt.query_map([], |r| r.get(1))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert!(
            cols.iter().any(|n| n == "scope"),
            "迁移后应补上 scope 列: {:?}",
            cols
        );

        migrate(&c).unwrap();
    }

    #[test]
    fn migrate_is_idempotent_and_creates_tables() {
        let c = db();
        migrate(&c).unwrap();
        let n: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'trade_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 16);
    }

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
        assert_eq!(
            upsert_quotes(&c, std::slice::from_ref(&q), at(16, 10, 0)).unwrap(),
            1
        );
        assert_eq!(get_quote(&c, "600000").unwrap(), Some(q.clone()));
        let newer = Quote {
            price: 10.2,
            ts: at(16, 10, 1),
            ..q.clone()
        };
        upsert_quotes(&c, std::slice::from_ref(&newer), at(16, 10, 1)).unwrap();
        assert_eq!(get_quote(&c, "600000").unwrap(), Some(newer.clone()));
        assert_eq!(
            fresh_quote(&c, "600000", at(16, 10, 2), 60).unwrap(),
            Some(newer)
        );
        assert!(
            fresh_quote(&c, "600000", at(16, 10, 5), 60)
                .unwrap()
                .is_none(),
            "超过 60 秒视为陈旧"
        );
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
            all.iter()
                .map(|p| (p.user_id, p.account, p.code.as_str()))
                .collect::<Vec<_>>(),
            vec![(1, Account::Real, "600000"), (2, Account::Paper, "600036")]
        );
        set_trailing_high(&c, 1, Account::Real, "600000", 12.5, at(16, 10, 0)).unwrap();
        assert_eq!(
            get_position(&c, 1, Account::Real, "600000")
                .unwrap()
                .unwrap()
                .trailing_high,
            Some(12.5)
        );
        assert_eq!(users_with_positions(&c).unwrap(), vec![1, 2]);
        assert_eq!(
            users_with_real_positions(&c).unwrap(),
            vec![1],
            "用户 2 只有模拟盘持仓,不应计入"
        );
        set_capital(&c, 3, Account::Real, 1000.0, at(16, 9, 0)).unwrap();
        set_capital(&c, 4, Account::Paper, 1000.0, at(16, 9, 0)).unwrap();
        assert_eq!(users_with_real_account(&c).unwrap(), vec![3]);
    }

    #[test]
    fn signal_dedup_key_is_unique_per_user() {
        let c = db();
        let ins = |uid: i64| {
            c.execute(
                "INSERT OR IGNORE INTO trade_signals (user_id, source, code, side, ref_price, reason, dedup_key, created_at)
                 VALUES (?1, 'manual', '600000', 'buy', 10.0, 'r', 'k1', '2026-09-15 10:00:00')",
                [uid],
            )
            .unwrap()
        };
        assert_eq!(ins(1), 1);
        assert_eq!(ins(1), 0, "同用户同 key 被忽略");
        assert_eq!(ins(2), 1, "不同用户可用相同 key");
    }

    fn new_strategy() -> NewStrategy {
        NewStrategy {
            user_id: 1,
            name: "RSI 低吸".into(),
            kind: "rsi".into(),
            grid_toml:
                "rsi_window = [14]\noversold = [30.0]\noverbought = [70.0]\namount = [10000.0]"
                    .into(),
            pool: vec!["600000".into(), "000001".into()],
        }
    }

    #[test]
    fn new_strategy_is_validated() {
        let c = db();
        let mut s = new_strategy();
        s.pool = vec!["600000".into(), "600000".into()];
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "重复代码");
        let mut s = new_strategy();
        s.pool = Vec::new();
        assert!(create_strategy(&c, &s, at(16, 9, 0)).is_err(), "空池");
        let mut s = new_strategy();
        s.kind = "nope".into();
        assert!(
            create_strategy(&c, &s, at(16, 9, 0)).is_err(),
            "未知策略类型"
        );
        let mut s = new_strategy();
        s.grid_toml = "rsi_window = ".into();
        assert!(
            create_strategy(&c, &s, at(16, 9, 0)).is_err(),
            "网格无法解析"
        );
        assert!(create_strategy(&c, &new_strategy(), at(16, 9, 0)).is_ok());
    }

    #[test]
    fn strategy_crud_versioning_and_user_isolation() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let got = get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(
            (got.status, got.kind.as_str(), got.pool.len()),
            (StrategyStatus::Draft, "rsi", 2)
        );
        assert!(get_strategy(&c, 2, id).unwrap().is_none(), "用户隔离");
        assert_eq!(list_strategies(&c, 1).unwrap().len(), 1);
        assert!(list_strategies(&c, 2).unwrap().is_empty());

        // 未改定义 → 版本不变;改网格 → 版本变化且状态回到草稿
        crate::trade::admission::state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            "提交",
            at(16, 9, 1),
        )
        .unwrap();
        assert_eq!(
            update_definition(&c, 1, id, &new_strategy(), at(16, 9, 2)).unwrap(),
            DefinitionUpdate::Unchanged,
            "同定义不产生新版本"
        );
        assert_eq!(
            get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Backtesting
        );
        let mut changed = new_strategy();
        changed.grid_toml =
            "rsi_window = [14, 20]\noversold = [30.0]\noverbought = [70.0]\namount = [10000.0]"
                .into();
        assert_eq!(
            update_definition(&c, 1, id, &changed, at(16, 9, 3)).unwrap(),
            DefinitionUpdate::Reversioned
        );
        let got = get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Draft);
        assert_ne!(got.version_hash, new_strategy_hash());
        assert_eq!(
            update_definition(&c, 2, id, &changed, at(16, 9, 4)).unwrap(),
            DefinitionUpdate::NotFound,
            "他人不可改"
        );

        // F11:仅改名不应影响版本或状态(即便策略已在回测中)
        crate::trade::admission::state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            "重新提交",
            at(16, 9, 5),
        )
        .unwrap();
        let mut renamed = changed.clone();
        renamed.name = "RSI 低吸 v2".into();
        assert_eq!(
            update_definition(&c, 1, id, &renamed, at(16, 9, 6)).unwrap(),
            DefinitionUpdate::Renamed
        );
        let got = get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.name, "RSI 低吸 v2");
        assert_eq!(got.status, StrategyStatus::Backtesting, "改名不应重置状态");
        assert_eq!(
            got.version_hash,
            strategy_version_hash(&changed.kind, &changed.grid_toml, &changed.pool),
            "改名不应换版本"
        );
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
        save_eval(
            &c,
            id,
            1,
            &v,
            "oos",
            r#"{"sharpe":1.2}"#,
            day(15),
            day(16),
            at(16, 9, 5),
        )
        .unwrap();
        save_eval(
            &c,
            id,
            1,
            &v,
            "oos",
            r#"{"sharpe":1.3}"#,
            day(15),
            day(16),
            at(16, 9, 6),
        )
        .unwrap();
        let (json, run_at) = latest_eval(&c, id, 1, "oos").unwrap().unwrap();
        assert!(json.contains("1.3"), "取最新一条");
        assert_eq!(run_at, at(16, 9, 6));
        assert!(latest_eval(&c, id, 1, "paper").unwrap().is_none());

        log_status_event(
            &c,
            id,
            1,
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            "提交",
            at(16, 9, 7),
        )
        .unwrap();
        let events = list_status_events(&c, id, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            (events[0].0, events[0].1, events[0].2.as_str()),
            (StrategyStatus::Draft, StrategyStatus::Backtesting, "提交")
        );
    }

    /// F3:评估 / 事件子表按父行 `trade_strategies.user_id` 隔离,不能靠 strategy_id 越权读取。
    #[test]
    fn child_rows_are_user_scoped() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let v = new_strategy_hash();
        save_eval(
            &c,
            id,
            1,
            &v,
            "oos",
            r#"{"sharpe":1.2}"#,
            day(15),
            day(16),
            at(16, 9, 5),
        )
        .unwrap();
        log_status_event(
            &c,
            id,
            1,
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            "提交",
            at(16, 9, 6),
        )
        .unwrap();

        assert!(
            latest_eval(&c, id, 2, "oos").unwrap().is_none(),
            "他人不可读评估"
        );
        assert!(
            list_status_events(&c, id, 2).unwrap().is_empty(),
            "他人不可读事件"
        );
        // 拥有者本人仍可读
        assert!(latest_eval(&c, id, 1, "oos").unwrap().is_some());
        assert_eq!(list_status_events(&c, id, 1).unwrap().len(), 1);

        // 写入侧同样按 user_id 拒绝越权
        assert!(save_eval(
            &c,
            id,
            2,
            &v,
            "oos",
            r#"{"sharpe":0.1}"#,
            day(15),
            day(16),
            at(16, 9, 7),
        )
        .is_err());
        assert!(log_status_event(
            &c,
            id,
            2,
            StrategyStatus::Draft,
            StrategyStatus::Backtesting,
            "越权",
            at(16, 9, 7),
        )
        .is_err());
    }

    #[test]
    fn last_transition_at_finds_the_latest_entry_per_status() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        assert!(last_transition_at(&c, id, 1, StrategyStatus::Paper)
            .unwrap()
            .is_none());
        log_status_event(
            &c,
            id,
            1,
            StrategyStatus::Backtesting,
            StrategyStatus::Paper,
            "首次",
            at(16, 9, 1),
        )
        .unwrap();
        log_status_event(
            &c,
            id,
            1,
            StrategyStatus::Paper,
            StrategyStatus::Suspended,
            "暂停",
            at(16, 9, 2),
        )
        .unwrap();
        log_status_event(
            &c,
            id,
            1,
            StrategyStatus::Suspended,
            StrategyStatus::Paper,
            "再次",
            at(16, 9, 3),
        )
        .unwrap();
        assert_eq!(
            last_transition_at(&c, id, 1, StrategyStatus::Paper).unwrap(),
            Some(at(16, 9, 3))
        );
        assert!(
            last_transition_at(&c, id, 2, StrategyStatus::Paper)
                .unwrap()
                .is_none(),
            "用户隔离"
        );
    }

    #[test]
    fn eval_queue_dedups_claims_and_finishes() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let job = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 1))
            .unwrap()
            .unwrap();
        assert!(
            enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 2))
                .unwrap()
                .is_none(),
            "同类型已排队"
        );
        assert!(
            enqueue_eval(&c, 1, id, EvalKind::Watchdog, at(16, 9, 2))
                .unwrap()
                .is_some(),
            "不同类型可并存"
        );

        let claimed = claim_next_job(&c, at(16, 9, 3)).unwrap().unwrap();
        assert_eq!((claimed.id, claimed.status), (job, JobStatus::Running));
        assert_eq!(
            claimed.started_at,
            Some(at(16, 9, 3)),
            "F7:领取后记录开始时间"
        );
        assert_eq!(claimed.finished_at, None, "尚未结束");
        set_job_progress(&c, job, "3/10", at(16, 9, 4)).unwrap();
        finish_job(&c, job, None, at(16, 9, 5)).unwrap();
        let jobs = list_jobs(&c, 1, 10).unwrap();
        let done = jobs.iter().find(|j| j.id == job).unwrap();
        assert_eq!(
            (done.status, done.progress.as_deref()),
            (JobStatus::Done, Some("3/10"))
        );
        assert_eq!(done.started_at, Some(at(16, 9, 3)));
        assert_eq!(
            done.finished_at,
            Some(at(16, 9, 5)),
            "F7:结束后记录结束时间"
        );
        assert!(list_jobs(&c, 2, 10).unwrap().is_empty(), "用户隔离");

        // 完成后可再次入队;失败记录原因
        let again = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 6))
            .unwrap()
            .unwrap();
        let claimed = claim_next_job(&c, at(16, 9, 7)).unwrap().unwrap();
        assert_ne!(claimed.id, job);
        finish_job(&c, again, Some("加载失败"), at(16, 9, 8)).unwrap();
        let jobs = list_jobs(&c, 1, 10).unwrap();
        let failed = jobs.iter().find(|j| j.id == again).unwrap();
        assert_eq!(failed.status, JobStatus::Failed);
        assert_eq!(failed.error.as_deref(), Some("加载失败"));
    }

    #[test]
    fn get_job_reads_by_id_and_is_user_scoped() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let job = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 1))
            .unwrap()
            .unwrap();
        let got = get_job(&c, 1, job).unwrap().unwrap();
        assert_eq!(
            (got.id, got.strategy_id, got.kind),
            (job, id, EvalKind::WalkForward)
        );
        assert!(get_job(&c, 2, job).unwrap().is_none(), "用户隔离");
        assert!(get_job(&c, 1, job + 1).unwrap().is_none(), "不存在的 id");
    }

    /// F4:进程重启后 `running` 任务没有租约,永远不会自然结束;不回收的话
    /// `enqueue_eval` 会一直因为「同类型已在运行」而拒绝重新入队。
    /// 这个测试顺带覆盖了此前未测过的「入队去重命中 running 状态」路径。
    #[test]
    fn reclaim_stale_jobs_unblocks_enqueue() {
        let c = db();
        let id = create_strategy(&c, &new_strategy(), at(16, 9, 0)).unwrap();
        let job = enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 1))
            .unwrap()
            .unwrap();
        claim_next_job(&c, at(16, 9, 2)).unwrap().unwrap();
        assert!(
            enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 3))
                .unwrap()
                .is_none(),
            "同类型已在运行中,应去重"
        );

        let reclaimed = reclaim_stale_jobs(&c, at(16, 9, 4)).unwrap();
        assert_eq!(reclaimed, 1);

        let jobs = list_jobs(&c, 1, 10).unwrap();
        let stale = jobs.iter().find(|j| j.id == job).unwrap();
        assert_eq!(stale.status, JobStatus::Failed);
        assert_eq!(stale.error.as_deref(), Some("进程重启,任务中断"));

        assert!(
            enqueue_eval(&c, 1, id, EvalKind::WalkForward, at(16, 9, 5))
                .unwrap()
                .is_some(),
            "回收后应能重新入队"
        );
    }
}

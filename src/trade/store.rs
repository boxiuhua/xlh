//! 交易表结构与账户 / 风控 / 持仓读写。所有查询按 user_id 隔离。

use crate::trade::model::{fmt_ts, Account, AccountState, Position, RiskRules, DATE_FMT};
use anyhow::{anyhow, Context, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension, Row};

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
"#;

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA).context("建交易表失败")?;
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

fn read_raw_position(r: &Row) -> rusqlite::Result<RawPosition> {
    Ok(RawPosition {
        code: r.get(0)?,
        qty: r.get(1)?,
        avg_cost: r.get(2)?,
        today_bought_qty: r.get(3)?,
        last_buy_date: r.get(4)?,
        stop_loss: r.get(5)?,
        take_profit: r.get(6)?,
        trailing_pct: r.get(7)?,
        trailing_high: r.get(8)?,
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
            read_raw_position,
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
        .query_map(params![user_id, account.as_str()], read_raw_position)?
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
        assert_eq!(n, 6);
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
}

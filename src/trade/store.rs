//! 交易表结构与账户 / 风控 / 持仓读写。所有查询按 user_id 隔离。

use anyhow::{Context, Result};
use rusqlite::Connection;

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

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        c
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

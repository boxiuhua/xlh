//! 日线策略信号计划表(计划 3e):收盘后写入、次日开盘发出。
//! `(strategy_id, code, basis_date)` 唯一,是「今天算过没有」的幂等边界(设计裁决 4)。

use crate::event::Direction;
use crate::trade::model::{fmt_ts, parse_side, side_str, DATE_FMT};
use anyhow::{anyhow, Result};
use chrono::{NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension, Row};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// 有动作,等次日发出
    Planned,
    /// 算过了,无动作(或无实盘参数、决策失败),不发
    Idle,
    /// 已发出(工单已生成或被闸门拒绝,都算处理过)
    Submitted,
    /// 发出窗口内未能发出,作废
    Dropped,
}

impl PlanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PlanStatus::Planned => "planned",
            PlanStatus::Idle => "idle",
            PlanStatus::Submitted => "submitted",
            PlanStatus::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "planned" => Ok(PlanStatus::Planned),
            "idle" => Ok(PlanStatus::Idle),
            "submitted" => Ok(PlanStatus::Submitted),
            "dropped" => Ok(PlanStatus::Dropped),
            _ => Err(anyhow!("未知计划状态: {s}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewPlan {
    pub user_id: i64,
    pub strategy_id: i64,
    pub version_hash: String,
    pub code: String,
    pub side: Option<Direction>,
    pub cash: Option<f64>,
    /// 计算所依据的最后一根 K 线日期(收盘日);次日及以后才发出
    pub basis_date: NaiveDate,
    pub reason: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalPlan {
    pub id: i64,
    pub user_id: i64,
    pub strategy_id: i64,
    pub version_hash: String,
    pub code: String,
    pub side: Option<Direction>,
    pub cash: Option<f64>,
    pub basis_date: NaiveDate,
    pub reason: String,
    pub status: PlanStatus,
    pub note: Option<String>,
}

/// 写入一行计划;同一 `(strategy_id, code, basis_date)` 已存在时不覆盖,返回 false。
pub fn insert_plan(conn: &Connection, p: &NewPlan, now: NaiveDateTime) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO trade_strategy_plans
           (user_id, strategy_id, version_hash, code, side, cash, basis_date, reason, status, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            p.user_id,
            p.strategy_id,
            p.version_hash,
            p.code,
            p.side.map(side_str),
            p.cash,
            p.basis_date.format(DATE_FMT).to_string(),
            p.reason,
            p.status.as_str(),
            fmt_ts(now),
        ],
    )?;
    Ok(n == 1)
}

pub fn has_plan(conn: &Connection, strategy_id: i64, code: &str, basis: NaiveDate) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM trade_strategy_plans
             WHERE strategy_id = ?1 AND code = ?2 AND basis_date = ?3",
            params![strategy_id, code, basis.format(DATE_FMT).to_string()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn plan_from_row(r: &Row) -> rusqlite::Result<SignalPlanRaw> {
    Ok(SignalPlanRaw {
        id: r.get(0)?,
        user_id: r.get(1)?,
        strategy_id: r.get(2)?,
        version_hash: r.get(3)?,
        code: r.get(4)?,
        side: r.get(5)?,
        cash: r.get(6)?,
        basis_date: r.get(7)?,
        reason: r.get(8)?,
        status: r.get(9)?,
        note: r.get(10)?,
    })
}

/// 数据库原样读出的一行,字符串字段在 `into_plan` 里再解析(解析错误不是 rusqlite 错误)。
struct SignalPlanRaw {
    id: i64,
    user_id: i64,
    strategy_id: i64,
    version_hash: String,
    code: String,
    side: Option<String>,
    cash: Option<f64>,
    basis_date: String,
    reason: String,
    status: String,
    note: Option<String>,
}

impl SignalPlanRaw {
    fn into_plan(self) -> Result<SignalPlan> {
        let id = self.id;
        Ok(SignalPlan {
            id,
            user_id: self.user_id,
            strategy_id: self.strategy_id,
            version_hash: self.version_hash,
            code: self.code,
            side: self.side.as_deref().map(parse_side).transpose()?,
            cash: self.cash,
            basis_date: NaiveDate::parse_from_str(&self.basis_date, DATE_FMT)
                .map_err(|e| anyhow!("计划 {id} 基准日格式错误 {}: {e}", self.basis_date))?,
            reason: self.reason,
            status: PlanStatus::parse(&self.status)?,
            note: self.note,
        })
    }
}

/// 待发出的计划:状态为 planned 且基准日早于 `today`(今天算的明天才发)。
pub fn due_plans(conn: &Connection, today: NaiveDate) -> Result<Vec<SignalPlan>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, strategy_id, version_hash, code, side, cash, basis_date, reason, status, note
         FROM trade_strategy_plans
         WHERE status = 'planned' AND basis_date < ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![today.format(DATE_FMT).to_string()], plan_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(SignalPlanRaw::into_plan).collect()
}

/// 结算一条 planned 计划;已结算过(或不存在)返回 false——只结一次。
pub fn settle_plan(
    conn: &Connection,
    id: i64,
    status: PlanStatus,
    note: &str,
    now: NaiveDateTime,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE trade_strategy_plans SET status = ?1, note = ?2, settled_at = ?3
         WHERE id = ?4 AND status = 'planned'",
        params![status.as_str(), note, fmt_ts(now), id],
    )?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::store;

    fn d(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, day).unwrap()
    }
    fn at(day: u32, h: u32) -> NaiveDateTime {
        d(day).and_hms_opt(h, 0, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }
    fn plan(code: &str, basis: NaiveDate, status: PlanStatus) -> NewPlan {
        NewPlan {
            user_id: 1,
            strategy_id: 7,
            version_hash: "h".into(),
            code: code.into(),
            side: (status == PlanStatus::Planned).then_some(Direction::Buy),
            cash: Some(5000.0),
            basis_date: basis,
            reason: "r".into(),
            status,
        }
    }

    #[test]
    fn insert_is_idempotent_per_strategy_code_basis() {
        let c = db();
        assert!(insert_plan(&c, &plan("600000", d(16), PlanStatus::Planned), at(16, 15)).unwrap());
        assert!(!insert_plan(&c, &plan("600000", d(16), PlanStatus::Idle), at(16, 16)).unwrap());
        assert!(has_plan(&c, 7, "600000", d(16)).unwrap());
        assert!(!has_plan(&c, 7, "600000", d(17)).unwrap());
    }

    #[test]
    fn due_plans_are_planned_rows_with_an_earlier_basis_and_settle_once() {
        let c = db();
        insert_plan(&c, &plan("600000", d(16), PlanStatus::Planned), at(16, 15)).unwrap();
        insert_plan(&c, &plan("600036", d(16), PlanStatus::Idle), at(16, 15)).unwrap();
        insert_plan(&c, &plan("000001", d(17), PlanStatus::Planned), at(17, 15)).unwrap();
        let due = due_plans(&c, d(17)).unwrap();
        assert_eq!(due.len(), 1, "idle 不发、今天算的明天才发");
        assert_eq!(due[0].code, "600000");
        assert_eq!(due[0].side, Some(Direction::Buy));
        assert!(settle_plan(&c, due[0].id, PlanStatus::Submitted, "ok", at(17, 9)).unwrap());
        assert!(
            !settle_plan(&c, due[0].id, PlanStatus::Dropped, "x", at(17, 10)).unwrap(),
            "只结一次"
        );
        assert!(due_plans(&c, d(17)).unwrap().is_empty());
    }
}

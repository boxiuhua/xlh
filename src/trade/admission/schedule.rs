//! 评估任务的自动入队。只负责写队列,执行在 `thread.rs`:
//! 入队与执行分离,重启、崩溃与手动触发三条路径才能共用同一套执行逻辑。

use crate::trade::admission::walk_forward::PoolMetrics;
use crate::trade::model::{EvalKind, StrategyDef, StrategyStatus};
use crate::trade::store;
use anyhow::{Context, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use rusqlite::Connection;

/// 一轮入队的结果,仅用于日志与测试。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Enqueued {
    pub paper: usize,
    pub watchdog: usize,
    pub walk_forward: usize,
    /// 单个策略入队失败不该中断整轮
    pub errors: Vec<String>,
}

/// 逐用户遍历策略,按状态映射出要入队的任务类型。
fn enqueue_by_status(
    conn: &Connection,
    now: NaiveDateTime,
    pick: impl Fn(&StrategyDef) -> Result<Option<EvalKind>>,
) -> Result<Enqueued> {
    let mut out = Enqueued::default();
    for user_id in store::users_with_strategies(conn)? {
        for s in store::list_strategies(conn, user_id)? {
            let kind = match pick(&s) {
                Ok(Some(k)) => k,
                Ok(None) => continue,
                Err(e) => {
                    out.errors
                        .push(format!("策略 {} 入队判定失败: {e:#}", s.id));
                    continue;
                }
            };
            match store::enqueue_eval(conn, user_id, s.id, kind, now) {
                Ok(Some(_)) => match kind {
                    EvalKind::PaperCheck => out.paper += 1,
                    EvalKind::Watchdog => out.watchdog += 1,
                    EvalKind::WalkForward => out.walk_forward += 1,
                },
                // None = 同类型任务已排队或运行中,不是错误
                Ok(None) => {}
                Err(e) => out.errors.push(format!("策略 {} 入队失败: {e:#}", s.id)),
            }
        }
    }
    Ok(out)
}

/// 每日:观察期策略查是否达标,已准入策略查实盘是否失控。
pub fn enqueue_daily(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |s| {
        Ok(match s.status {
            StrategyStatus::Paper => Some(EvalKind::PaperCheck),
            StrategyStatus::Admitted => Some(EvalKind::Watchdog),
            _ => None,
        })
    })
}

/// 每月:对还在用的策略重跑前推回测(计划 3c 的 `apply_monthly_verdict` 负责裁决)。
/// 异动类没有历史分时、不可回测(spec §10.2),跳过——否则每月都会失败一次并留下噪音。
pub fn enqueue_monthly(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |s| {
        Ok(daily_strategy_running(s).then_some(EvalKind::WalkForward))
    })
}

/// 观察期 / 已准入的非异动策略:走前推回测与日线信号的那一类。
fn daily_strategy_running(s: &StrategyDef) -> bool {
    s.kind != "mover" && matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted)
}

/// 进程启动时补跑一次:观察期 / 已准入的日线策略,若最近一次样本外评估里没有任何
/// 股票带实盘参数(计划 3e 之前的评估记录没有这一项),日线信号计算只能逐只记
/// 「无实盘参数」,要等下一次月度重跑才恢复——最长近一个月没有信号。这里补排一次
/// 前推回测;已排队 / 运行中的由 `enqueue_eval` 去重。评估记录读不出来(库错误或
/// JSON 损坏)只记错误、不入队,不猜。
pub fn enqueue_missing_live_params(conn: &Connection, now: NaiveDateTime) -> Result<Enqueued> {
    enqueue_by_status(conn, now, |s| {
        if !daily_strategy_running(s) {
            return Ok(None);
        }
        let has_live = match store::latest_eval(conn, s.id, s.user_id, "oos")? {
            Some((json, _)) => serde_json::from_str::<PoolMetrics>(&json)
                .with_context(|| format!("策略 {} 的 oos 评估无法反序列化", s.id))?
                .codes
                .iter()
                .any(|c| c.live_params.is_some()),
            None => false,
        };
        Ok((!has_live).then_some(EvalKind::WalkForward))
    })
}

/// 是否该跑本月的重跑:「已过触发点」且「本自然月还没跑过」。
///
/// 这是桌面进程而非常驻服务:原先按 `now.day() == day` 精确匹配,一旦触发日当天
/// 进程没在跑(关机、待机、次日才启动),`day()` 再也不会等于配置的日子,
/// 整月的前推回测重跑就被彻底跳过——已准入策略可能连续几个月得不到样本外复核。
/// 改成「超过触发点」按月去重后,晚到的那一轮会在当月内补跑一次,而 `last_run`
/// 按年月比较(不是按具体日期)则保证同一自然月只跑一次,即便触发点当天多次
/// 重启也不会在当月内重复裁决。
pub fn due_monthly(now: NaiveDateTime, day: u32, hour: u32, last_run: Option<NaiveDate>) -> bool {
    // 补跑也必须等到触发时刻:整池 12 年前推回测既吃 CPU 又要全池联网,
    // 若只看日期,错过的那一轮会在重启后的第一轮立刻开跑——很可能正是盘中,
    // 与 15 秒一轮的止盈止损监听抢资源。宁可当天晚些补,也不在开盘时段动手。
    let past_trigger = now.day() >= day && now.hour() >= hour;
    past_trigger && last_run.is_none_or(|d| (d.year(), d.month()) != (now.year(), now.month()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::model::{EvalKind, NewStrategy, StrategyStatus};
    use crate::trade::store;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    /// 建一个指定状态的策略。`mover` 提交后直接进观察期,便于造 Paper;
    /// Admitted 再从 Paper 转一次。
    fn strategy_at(c: &Connection, user_id: i64, status: StrategyStatus) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "S".into(),
                kind: "mover".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        if status == StrategyStatus::Draft {
            return id;
        }
        state::submit_for_backtest(c, user_id, id, at(16, 9, 1)).unwrap(); // → Paper
        if status == StrategyStatus::Admitted {
            state::update_status(
                c,
                user_id,
                id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "准入",
                at(16, 9, 2),
            )
            .unwrap();
        }
        id
    }

    /// 建一个指定状态的非 mover(trend)策略:观察期要走一次通过的前推回测裁决,
    /// Admitted 再从 Paper 转一次。
    fn trend_strategy_at(c: &Connection, user_id: i64, status: StrategyStatus) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id,
                name: "T".into(),
                kind: "trend".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        if status == StrategyStatus::Draft {
            return id;
        }
        state::submit_for_backtest(c, user_id, id, at(16, 9, 1)).unwrap(); // → Backtesting
        state::apply_backtest_verdict(
            c,
            user_id,
            id,
            &crate::trade::admission::walk_forward::aggregate(Vec::new()),
            &crate::trade::admission::judge::Verdict {
                passed: true,
                reasons: vec![],
            },
            NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            at(16, 9, 1),
        )
        .unwrap();
        if status == StrategyStatus::Admitted {
            state::update_status(
                c,
                user_id,
                id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "准入",
                at(16, 9, 2),
            )
            .unwrap();
        }
        id
    }

    fn queued_kinds(c: &Connection) -> Vec<(i64, EvalKind)> {
        let mut out = Vec::new();
        while let Some(j) = store::claim_next_job(c, at(16, 17, 0)).unwrap() {
            out.push((j.strategy_id, j.kind));
        }
        out
    }

    #[test]
    fn daily_enqueues_paper_check_and_watchdog_per_status() {
        let c = db();
        let paper = strategy_at(&c, 1, StrategyStatus::Paper);
        let admitted = strategy_at(&c, 2, StrategyStatus::Admitted);
        let draft = strategy_at(&c, 1, StrategyStatus::Draft);

        let r = enqueue_daily(&c, at(16, 16, 30)).unwrap();
        assert_eq!((r.paper, r.watchdog), (1, 1));
        assert!(r.errors.is_empty());
        let mut got = queued_kinds(&c);
        got.sort();
        assert_eq!(
            got,
            vec![
                (paper, EvalKind::PaperCheck),
                (admitted, EvalKind::Watchdog)
            ]
        );
        assert!(!got.iter().any(|(id, _)| *id == draft), "草稿不入队");
    }

    #[test]
    fn daily_does_not_double_enqueue() {
        let c = db();
        strategy_at(&c, 1, StrategyStatus::Paper);
        assert_eq!(enqueue_daily(&c, at(16, 16, 30)).unwrap().paper, 1);
        assert_eq!(
            enqueue_daily(&c, at(16, 16, 31)).unwrap().paper,
            0,
            "已排队的不重复入队"
        );
    }

    #[test]
    fn monthly_enqueues_walk_forward_for_paper_and_admitted() {
        // 异动(mover)策略每月重跑跳过(见 monthly_rerun_skips_mover_strategies),
        // 这里用非 mover(trend)策略来测「Paper/Admitted 都入队、Draft 不入队」。
        let c = db();
        trend_strategy_at(&c, 1, StrategyStatus::Paper);
        trend_strategy_at(&c, 1, StrategyStatus::Admitted);
        trend_strategy_at(&c, 1, StrategyStatus::Draft);
        let r = enqueue_monthly(&c, at(16, 17, 0)).unwrap();
        assert_eq!(r.walk_forward, 2);
        assert!(queued_kinds(&c)
            .iter()
            .all(|(_, k)| *k == EvalKind::WalkForward));
    }

    /// 计划 3c 遗留项:异动类没有历史分时、不可回测(spec §10.2),每月重跑不该把它排进去,
    /// 否则每月都会失败一次并留下噪音。
    #[test]
    fn monthly_rerun_skips_mover_strategies() {
        let c = db();
        strategy_at(&c, 1, StrategyStatus::Paper); // kind=mover 的观察期策略
        trend_strategy_at(&c, 1, StrategyStatus::Paper); // kind=trend 的观察期策略

        let r = enqueue_monthly(&c, at(16, 17, 0)).unwrap();
        assert_eq!(r.walk_forward, 1, "异动策略跳过,只有 trend 入队");
    }

    /// 观察期 trend 策略,样本外评估里 600000 一只股票,`live` 决定它带不带实盘参数。
    fn trend_paper_with_live_params(c: &Connection, live: bool) -> i64 {
        use crate::trade::admission::walk_forward::{aggregate, CodeMetrics};
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "T".into(),
                kind: "trend".into(),
                grid_toml: "x = [1]".into(),
                pool: vec!["600000".into()],
            },
            at(16, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(16, 9, 1)).unwrap();
        let code = CodeMetrics {
            code: "600000".into(),
            windows: 1,
            oos_return: 0.1,
            oos_annualized: 0.1,
            oos_sharpe: 1.0,
            oos_max_drawdown: 0.1,
            oos_trades: 10,
            is_sharpe: 1.0,
            years: 1.0,
            data_years: 4.0,
            buy_hold_return: 0.0,
            trade_baseline: Default::default(),
            window_details: Vec::new(),
            data_from: None,
            data_to: None,
            live_params: live.then(|| toml::Value::Table("x = 1".parse().unwrap())),
        };
        state::apply_backtest_verdict(
            c,
            1,
            id,
            &aggregate(vec![code]),
            &crate::trade::admission::judge::Verdict {
                passed: true,
                reasons: vec![],
            },
            NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 16).unwrap(),
            at(16, 9, 1),
        )
        .unwrap();
        id
    }

    /// 计划 3e 上线前就在观察期 / 已准入的策略,样本外评估里没有实盘参数:
    /// 不补跑的话要等下一次月度重跑(最长近一个月)才会有日线信号。
    #[test]
    fn startup_enqueues_walk_forward_for_running_strategies_without_live_params() {
        let c = db();
        let stale_paper = trend_paper_with_live_params(&c, false);
        let stale_admitted = trend_strategy_at(&c, 1, StrategyStatus::Admitted); // 空评估
        trend_paper_with_live_params(&c, true); // 已有实盘参数:不动
        strategy_at(&c, 1, StrategyStatus::Paper); // 异动策略不可回测
        trend_strategy_at(&c, 1, StrategyStatus::Draft);

        let r = enqueue_missing_live_params(&c, at(16, 9, 30)).unwrap();
        assert_eq!(r.walk_forward, 2);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let mut got = queued_kinds(&c);
        got.sort();
        assert_eq!(
            got,
            vec![
                (stale_paper, EvalKind::WalkForward),
                (stale_admitted, EvalKind::WalkForward)
            ]
        );
    }

    #[test]
    fn startup_live_params_backfill_does_not_double_enqueue() {
        let c = db();
        trend_paper_with_live_params(&c, false);
        assert_eq!(
            enqueue_missing_live_params(&c, at(16, 9, 30))
                .unwrap()
                .walk_forward,
            1
        );
        assert_eq!(
            enqueue_missing_live_params(&c, at(16, 9, 31))
                .unwrap()
                .walk_forward,
            0,
            "已排队的不重复入队"
        );
    }

    #[test]
    fn due_monthly_fires_once_per_month_and_catches_up_after_a_missed_trigger_day() {
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 9, d).unwrap();
        assert!(due_monthly(at(1, 17, 0), 1, 17, None), "到点当天触发");
        assert!(!due_monthly(at(1, 16, 59), 1, 17, None), "未到点");
        assert!(
            due_monthly(at(2, 17, 0), 1, 17, None),
            "F13:桌面进程触发日当天没跑,次日要补跑,不能整月错过"
        );
        assert!(
            !due_monthly(at(1, 18, 0), 1, 17, Some(day(1))),
            "本月已跑,不再重复"
        );
        assert!(
            !due_monthly(at(5, 9, 0), 1, 17, Some(day(1))),
            "F13:本月已跑,即便重启且日期已过触发点也不再重复"
        );
        assert!(
            due_monthly(
                at(1, 17, 0),
                1,
                17,
                Some(NaiveDate::from_ymd_opt(2026, 8, 1).unwrap())
            ),
            "跨月后重新触发"
        );
    }
}

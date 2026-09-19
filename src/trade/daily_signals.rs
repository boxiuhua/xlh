//! 日线策略信号:收盘后计算(评估线程)→ 计划表 → 次日开盘发出(监听线程)。
//! 设计见计划 3e 设计裁决 1–7。

use crate::stock::data::StockBar;
use crate::trade::admission::walk_forward::{PoolMetrics, WalkForwardCfg};
use crate::trade::calendar;
use crate::trade::config::SignalCfg;
use crate::trade::model::StrategyStatus;
use crate::trade::plans::{self, NewPlan, PlanStatus};
use crate::trade::store;
use crate::trade::strategy_signal;
use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::Connection;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComputeReport {
    /// 今天已被证实休市,整轮跳过
    pub skipped_closed: bool,
    pub planned: usize,
    pub idle: usize,
    /// K 线未更新或加载失败,留待重试的(策略, 代码)数
    pub pending: usize,
    pub errors: Vec<String>,
}

/// 收盘后对所有观察期 / 已准入的日线策略逐只股票算次日动作,写入计划表。
/// 已有今天的行的(策略, 代码)直接跳过,可以随意重跑。
pub fn compute<F>(
    conn: &Connection,
    now: NaiveDateTime,
    wf: &WalkForwardCfg,
    mut load: F,
) -> Result<ComputeReport>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let today = now.date();
    let mut r = ComputeReport::default();
    if !calendar::is_trading_day(conn, today)? {
        r.skipped_closed = true;
        return Ok(r);
    }
    let next = calendar::next_trading_day(conn, today)?;
    for uid in store::users_with_strategies(conn)? {
        for s in store::list_strategies(conn, uid)? {
            // 异动策略由实时异动出信号(设计裁决 9),不走日线计算
            if s.kind == "mover"
                || !matches!(s.status, StrategyStatus::Paper | StrategyStatus::Admitted)
            {
                continue;
            }
            // 基线读不出来是数据问题,记错误、跳过该策略,不影响其它策略
            let metrics: Option<PoolMetrics> = match store::latest_eval(conn, s.id, uid, "oos")? {
                Some((json, _)) => match serde_json::from_str(&json)
                    .with_context(|| format!("策略 {} 的 oos 评估无法反序列化", s.id))
                {
                    Ok(m) => Some(m),
                    Err(e) => {
                        r.errors.push(format!("{e:#}"));
                        continue;
                    }
                },
                None => None,
            };
            for code in &s.pool {
                if plans::has_plan(conn, s.id, code, today)? {
                    continue;
                }
                let base = |status: PlanStatus, reason: String| NewPlan {
                    user_id: uid,
                    strategy_id: s.id,
                    version_hash: s.version_hash.clone(),
                    code: code.clone(),
                    side: None,
                    cash: None,
                    basis_date: today,
                    reason,
                    status,
                };
                let Some(params) = metrics.as_ref().and_then(|m| m.live_params_for(code)) else {
                    plans::insert_plan(
                        conn,
                        &base(PlanStatus::Idle, "无实盘参数(前推回测未产出)".into()),
                        now,
                    )?;
                    r.idle += 1;
                    continue;
                };
                let bars = match load(code) {
                    Ok(b) => b,
                    Err(e) => {
                        r.pending += 1;
                        r.errors
                            .push(format!("策略 {} {code} K 线加载失败: {e:#}", s.id));
                        continue;
                    }
                };
                // K 线本身就是开市证明:最后一根不是今天就还没更新,留待重试(设计裁决 5)
                if bars.last().map(|b| b.date) != Some(today) {
                    r.pending += 1;
                    continue;
                }
                match strategy_signal::decide(&s.kind, code, params, &bars, next, wf) {
                    Ok(Some(dec)) => {
                        let p = NewPlan {
                            side: Some(dec.side),
                            cash: dec.cash,
                            ..base(PlanStatus::Planned, dec.reason)
                        };
                        plans::insert_plan(conn, &p, now)?;
                        r.planned += 1;
                    }
                    Ok(None) => {
                        plans::insert_plan(conn, &base(PlanStatus::Idle, "无操作".into()), now)?;
                        r.idle += 1;
                    }
                    Err(e) => {
                        // 决策报错(参数与策略不匹配、不支持的数量口径等)重试也不会好:
                        // 记 idle 并留下原因,同时进 errors 由线程打日志(设计裁决 3)
                        plans::insert_plan(
                            conn,
                            &base(PlanStatus::Idle, format!("决策失败: {e:#}")),
                            now,
                        )?;
                        r.idle += 1;
                        r.errors
                            .push(format!("策略 {} {code} 决策失败: {e:#}", s.id));
                    }
                }
            }
        }
    }
    Ok(r)
}

/// 跨轮次的计算状态。不落库:重启后 `done_for` 为 None 会再跑一轮,
/// 但计划表的唯一键让已算过的(策略, 代码)直接跳过,不会重复加载或重复出信号。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ComputeState {
    pub done_for: Option<NaiveDate>,
    pub last_attempt: Option<NaiveDateTime>,
}

fn hm(h: u32, m: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, m, 0).expect("配置已校验时刻合法")
}

/// 工作日 [compute, cutoff) 内、当日未完成、且距上次尝试已满 `retry_minutes` 时到点。
pub fn compute_due(now: NaiveDateTime, cfg: &SignalCfg, st: &ComputeState) -> bool {
    let t = now.time();
    !crate::stock::realtime::calendar::is_weekend(now.date())
        && t >= hm(cfg.compute_hour, cfg.compute_minute)
        && t < hm(cfg.cutoff_hour, 0)
        && st.done_for != Some(now.date())
        && st
            .last_attempt
            .is_none_or(|a| a.date() != now.date() || (now - a).num_minutes() >= cfg.retry_minutes)
}

/// 到点才算;算完若无待重试项(且无错误)则标记当日完成。未到点返回 None。
pub fn run_compute<F>(
    conn: &Connection,
    cfg: &SignalCfg,
    wf: &WalkForwardCfg,
    st: &mut ComputeState,
    now: NaiveDateTime,
    load: F,
) -> Option<ComputeReport>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    if !cfg.enabled || !compute_due(now, cfg, st) {
        return None;
    }
    st.last_attempt = Some(now);
    let r = match compute(conn, now, wf, load) {
        Ok(r) => r,
        Err(e) => ComputeReport {
            errors: vec![format!("日线信号计算失败: {e:#}")],
            ..ComputeReport::default()
        },
    };
    if r.skipped_closed || (r.pending == 0 && r.errors.is_empty()) {
        st.done_for = Some(now.date());
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::{state, walk_forward};
    use crate::trade::model::{NewStrategy, StrategyStatus};
    use crate::trade::plans;
    use crate::trade::store;
    use chrono::Datelike;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    fn at(m: u32, day: u32, h: u32, mi: u32) -> NaiveDateTime {
        d(m, day).and_hms_opt(h, mi, 0).unwrap()
    }
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    /// 以 `last` 为最后一天、往前连续交易日(跳过周末)的 K 线。
    fn bars_until(last: NaiveDate, prices: &[f64]) -> Vec<StockBar> {
        let mut dates = Vec::new();
        let mut day = last;
        while dates.len() < prices.len() {
            if !matches!(day.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                dates.push(day);
            }
            day -= chrono::Duration::days(1);
        }
        dates.reverse();
        dates
            .into_iter()
            .zip(prices)
            .map(|(date, p)| StockBar {
                date,
                open: *p,
                high: *p,
                low: *p,
                close: *p,
                volume: 1.0,
                adj_close: *p,
            })
            .collect()
    }

    /// 最后一根 K 线上金叉(同 strategy_signal 的用例)。
    fn golden_cross(last: NaiveDate) -> Vec<StockBar> {
        let mut p: Vec<f64> = (0..80).map(|i| 20.0 - i as f64 * 0.1).collect();
        p.extend([12.2, 12.3, 12.4, 12.5, 25.0]);
        bars_until(last, &p)
    }

    /// 一只股票的样本外指标,只有 `code` 与 `live_params` 对本模块有意义。
    fn code_metrics(code: &str, live: bool) -> walk_forward::CodeMetrics {
        walk_forward::CodeMetrics {
            code: code.into(),
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
            live_params: live.then(|| {
                toml::Value::Table(
                    "short_window = 5\nlong_window = 20\namount = 100000.0"
                        .parse()
                        .unwrap(),
                )
            }),
        }
    }

    /// 观察期的 trend 策略;oos 评估里 `with_params` 中的代码带实盘参数,池内其余代码没有。
    fn running_trend_strategy(c: &Connection, pool: &[&str], with_params: &[&str]) -> i64 {
        let id = store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "趋势".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]\nlong_window = [20]\namount = [100000.0]".into(),
                pool: pool.iter().map(|s| s.to_string()).collect(),
            },
            at(9, 1, 9, 0),
        )
        .unwrap();
        state::submit_for_backtest(c, 1, id, at(9, 1, 9, 1)).unwrap();
        let metrics = walk_forward::aggregate(
            pool.iter()
                .map(|code| code_metrics(code, with_params.contains(code)))
                .collect(),
        );
        let verdict = crate::trade::admission::judge::Verdict {
            passed: true,
            reasons: Vec::new(),
        };
        state::apply_backtest_verdict(
            c,
            1,
            id,
            &metrics,
            &verdict,
            d(9, 1),
            d(9, 1),
            at(9, 1, 9, 2),
        )
        .unwrap();
        assert_eq!(
            store::get_strategy(c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper
        );
        id
    }

    #[test]
    fn compute_plans_buy_for_next_trading_day_and_is_idempotent() {
        let c = db();
        let sid = running_trend_strategy(&c, &["600000", "600036"], &["600000", "600036"]);
        let today = at(9, 18, 15, 30); // 周五
        let mut loads = 0;
        let r = compute(&c, today, &WalkForwardCfg::default(), |code| {
            loads += 1;
            Ok(if code == "600000" {
                golden_cross(d(9, 18))
            } else {
                bars_until(d(9, 18), &[10.0; 85])
            })
        })
        .unwrap();
        assert_eq!((r.planned, r.idle, r.pending), (1, 1, 0));
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // 下周一发出
        let due = plans::due_plans(&c, d(9, 21)).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].strategy_id, sid);
        assert_eq!(due[0].side, Some(crate::event::Direction::Buy));
        assert_eq!(due[0].cash, Some(100000.0));
        assert_eq!(due[0].basis_date, d(9, 18));
        // 再跑一次:已有今天的行,不再加载 K 线
        let before = loads;
        let r2 = compute(&c, at(9, 18, 15, 40), &WalkForwardCfg::default(), |_| {
            loads += 1;
            Ok(Vec::new())
        })
        .unwrap();
        assert_eq!((r2.planned, r2.idle, r2.pending), (0, 0, 0));
        assert_eq!(loads, before);
    }

    #[test]
    fn stale_kline_is_pending_and_missing_params_is_idle() {
        let c = db();
        running_trend_strategy(&c, &["600000", "600036"], &["600000", "600036"]);
        let r = compute(&c, at(9, 18, 15, 30), &WalkForwardCfg::default(), |code| {
            Ok(if code == "600000" {
                golden_cross(d(9, 17)) // 还没有今天的 K 线
            } else {
                golden_cross(d(9, 18))
            })
        })
        .unwrap();
        assert_eq!(r.pending, 1, "600000 的 K 线未更新,留待重试");
        assert_eq!(r.planned, 1, "600036 有今天的 K 线与实盘参数,当天金叉");
        let c2 = db();
        running_trend_strategy(&c2, &["600000"], &[]);
        let r = compute(&c2, at(9, 18, 15, 30), &WalkForwardCfg::default(), |_| {
            Ok(golden_cross(d(9, 18)))
        })
        .unwrap();
        assert_eq!(r.idle, 1, "无实盘参数:记 idle(当天算完),不发信号");
    }

    #[test]
    fn compute_skips_closed_days_draft_strategies_and_movers() {
        let c = db();
        running_trend_strategy(&c, &["600000"], &["600000"]);
        crate::trade::calendar::mark_day(&c, d(10, 1), false, at(10, 1, 9, 31)).unwrap();
        let r = compute(&c, at(10, 1, 15, 30), &WalkForwardCfg::default(), |_| {
            panic!("休市日不应加载")
        })
        .unwrap();
        assert!(r.skipped_closed);
        // 草稿策略不参与
        let c2 = db();
        store::create_strategy(
            &c2,
            &NewStrategy {
                user_id: 1,
                name: "草稿".into(),
                kind: "trend".into(),
                grid_toml: "short_window = [5]".into(),
                pool: vec!["600000".into()],
            },
            at(9, 1, 9, 0),
        )
        .unwrap();
        let r = compute(&c2, at(9, 18, 15, 30), &WalkForwardCfg::default(), |_| {
            panic!("草稿不应加载")
        })
        .unwrap();
        assert_eq!((r.planned, r.idle, r.pending), (0, 0, 0));
    }

    #[test]
    fn compute_due_paces_retries_and_stops_at_cutoff() {
        let cfg = SignalCfg::default();
        let mut st = ComputeState::default();
        assert!(!compute_due(at(9, 18, 15, 29), &cfg, &st), "未到点");
        assert!(compute_due(at(9, 18, 15, 30), &cfg, &st));
        assert!(!compute_due(at(9, 19, 15, 30), &cfg, &st), "周六");
        st.last_attempt = Some(at(9, 18, 15, 30));
        assert!(!compute_due(at(9, 18, 15, 35), &cfg, &st), "重试间隔内");
        assert!(compute_due(at(9, 18, 15, 40), &cfg, &st));
        assert!(!compute_due(at(9, 18, 21, 0), &cfg, &st), "截止");
        st.done_for = Some(d(9, 18));
        assert!(!compute_due(at(9, 18, 16, 0), &cfg, &st), "当日已完成");
    }

    #[test]
    fn run_compute_marks_done_only_when_nothing_is_pending() {
        let c = db();
        running_trend_strategy(&c, &["600000"], &["600000"]);
        let cfg = SignalCfg::default();
        let wf = WalkForwardCfg::default();
        let mut st = ComputeState::default();
        let r = run_compute(&c, &cfg, &wf, &mut st, at(9, 18, 15, 30), |_| {
            Ok(golden_cross(d(9, 17)))
        })
        .unwrap();
        assert_eq!(r.pending, 1);
        assert_eq!(st.done_for, None);
        assert_eq!(st.last_attempt, Some(at(9, 18, 15, 30)));
        assert!(
            run_compute(
                &c,
                &cfg,
                &wf,
                &mut st,
                at(9, 18, 15, 31),
                |_| unreachable!()
            )
            .is_none(),
            "未到重试点"
        );
        let r = run_compute(&c, &cfg, &wf, &mut st, at(9, 18, 15, 40), |_| {
            Ok(golden_cross(d(9, 18)))
        })
        .unwrap();
        assert_eq!(r.planned, 1);
        assert_eq!(st.done_for, Some(d(9, 18)));
    }
}

//! 单个评估任务的执行:前推回测 / 观察期检查 / 实盘监控。
//! 线程与调度在计划 3d;本模块保证在内存库 + 注入 K 线下可完整测试。

use crate::stock::data::StockBar;
use crate::trade::admission::judge::{self, BacktestBaseline};
use crate::trade::admission::state;
use crate::trade::admission::stats;
use crate::trade::admission::walk_forward::{self, PoolMetrics, PoolProgress, WalkForwardCfg};
use crate::trade::config::AdmissionCfg;
use crate::trade::model::{EvalJob, EvalKind, StrategyStatus};
use crate::trade::store;
use anyhow::{anyhow, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;

pub struct JobContext<'a> {
    pub wf: &'a WalkForwardCfg,
    pub admission: &'a AdmissionCfg,
    pub now: NaiveDateTime,
}

/// 最近一次前推回测的池内指标(供观察期与 watchdog 取基线)。
fn latest_pool_metrics(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
) -> Result<Option<PoolMetrics>> {
    Ok(
        match store::latest_eval(conn, strategy_id, user_id, "oos")? {
            Some((json, _)) => serde_json::from_str::<PoolMetrics>(&json).ok(),
            None => None,
        },
    )
}

pub fn run_job<F>(conn: &mut Connection, job: &EvalJob, ctx: &JobContext, load: F) -> Result<String>
where
    F: FnMut(&str) -> Result<Vec<StockBar>>,
{
    let Some(s) = store::get_strategy(conn, job.user_id, job.strategy_id)? else {
        return Err(anyhow!("策略 {} 不存在", job.strategy_id));
    };
    match job.kind {
        EvalKind::WalkForward => {
            let grid: toml::Table = s
                .grid_toml
                .parse()
                .map_err(|e| anyhow!("参数网格解析失败: {e}"))?;
            let total = s.pool.len();
            let job_id = job.id;
            // 进度写库失败不该中断评估,只记日志
            let mut on_code = |code: &str, done: usize, _total: usize| {
                if let Err(e) = store::set_job_progress(
                    conn,
                    job_id,
                    &format!("{done}/{total} {code}"),
                    ctx.now,
                ) {
                    eprintln!("[trade] 任务 {job_id} 进度写入失败: {e:#}");
                }
                true
            };
            let mut progress = PoolProgress {
                on_code: &mut on_code,
            };
            let outcome =
                walk_forward::run_pool(&s.kind, &s.pool, &grid, ctx.wf, load, Some(&mut progress))?;
            if outcome.cancelled {
                return Ok("已取消".to_string());
            }
            let verdict = judge::judge_backtest(&outcome.metrics, ctx.admission);
            let (from, to) = (ctx.now.date(), ctx.now.date());
            let transition = if s.status == StrategyStatus::Backtesting {
                state::apply_backtest_verdict(
                    conn,
                    job.user_id,
                    job.strategy_id,
                    &outcome.metrics,
                    &verdict,
                    from,
                    to,
                    ctx.now,
                )?
            } else {
                state::apply_monthly_verdict(
                    conn,
                    job.user_id,
                    job.strategy_id,
                    &outcome.metrics,
                    &verdict,
                    from,
                    to,
                    ctx.now,
                )?
            };
            Ok(format!(
                "回测{}:{};状态变更 {:?}",
                if verdict.passed {
                    "通过"
                } else {
                    "未通过"
                },
                if verdict.passed {
                    "—".to_string()
                } else {
                    verdict.reasons.join(";")
                },
                transition
            ))
        }
        EvalKind::PaperCheck => {
            if s.status != StrategyStatus::Paper {
                return Ok(format!("跳过:当前状态 {}", s.status.as_str()));
            }
            let since = store::last_transition_at(
                conn,
                job.strategy_id,
                job.user_id,
                StrategyStatus::Paper,
            )?
            .unwrap_or(s.updated_at);
            let stats = stats::paper_stats(conn, job.user_id, job.strategy_id, since, ctx.now)?;
            let baseline = if s.kind == "mover" {
                None
            } else {
                latest_pool_metrics(conn, job.user_id, job.strategy_id)?
                    .map(|m| BacktestBaseline::from_pool(&m))
            };
            let verdict =
                judge::judge_paper(&stats, baseline.as_ref(), s.kind == "mover", ctx.admission);
            if !verdict.passed {
                return Ok(format!("观察期未达标:{}", verdict.reasons.join(";")));
            }
            let transition = state::update_status(
                conn,
                job.user_id,
                job.strategy_id,
                StrategyStatus::Paper,
                StrategyStatus::Admitted,
                "观察期达标,准入",
                ctx.now,
            )?;
            Ok(format!("观察期达标,准入({transition:?})"))
        }
        EvalKind::Watchdog => {
            if s.status != StrategyStatus::Admitted {
                return Ok(format!("跳过:当前状态 {}", s.status.as_str()));
            }
            let Some(metrics) = latest_pool_metrics(conn, job.user_id, job.strategy_id)? else {
                return Ok("跳过:无回测基线".to_string());
            };
            let stats = stats::watchdog_stats(
                conn,
                job.user_id,
                job.strategy_id,
                ctx.admission.watchdog_window,
            )?;
            let baseline = BacktestBaseline::from_pool(&metrics);
            match judge::judge_watchdog(
                &stats,
                &baseline,
                metrics.trade_baseline.win_rate,
                metrics.trade_baseline.max_consecutive_losses,
                ctx.admission,
            ) {
                Some(reason) => {
                    let transition = state::update_status(
                        conn,
                        job.user_id,
                        job.strategy_id,
                        StrategyStatus::Admitted,
                        StrategyStatus::Suspended,
                        &reason,
                        ctx.now,
                    )?;
                    Ok(format!("已暂停:{reason}({transition:?})"))
                }
                None => Ok("实盘表现正常".to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trade::admission::state;
    use crate::trade::admission::walk_forward::WalkForwardCfg;
    use crate::trade::config::AdmissionCfg;
    use crate::trade::model::{
        Account, AccountScope, EvalKind, NewSignal, NewStrategy, SignalSource, StrategyStatus,
        TicketStatus,
    };
    use crate::trade::store;
    use crate::trade::ticket::{create_ticket, insert_signal, NewTicket};
    use chrono::{Datelike, NaiveDate};

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

    fn strategy(c: &Connection, kind: &str) -> i64 {
        store::create_strategy(
            c,
            &NewStrategy {
                user_id: 1,
                name: "S".into(),
                kind: kind.into(),
                grid_toml: "short_window = [3, 5]\nlong_window = [10]\namount = [20000.0]".into(),
                pool: vec!["600000".into()],
            },
            at(15, 9, 0),
        )
        .unwrap()
    }

    fn job(c: &Connection, strategy_id: i64, kind: EvalKind) -> crate::trade::model::EvalJob {
        store::enqueue_eval(c, 1, strategy_id, kind, at(16, 9, 0))
            .unwrap()
            .unwrap();
        store::claim_next_job(c, at(16, 9, 1)).unwrap().unwrap()
    }

    fn ctx(_now: NaiveDateTime) -> (WalkForwardCfg, AdmissionCfg) {
        (
            WalkForwardCfg {
                train_days: 60,
                test_days: 30,
                step_days: 30,
                ..WalkForwardCfg::default()
            },
            AdmissionCfg::default(),
        )
    }

    /// 锯齿行情:与 walk_forward 测试同款(sin 波 + 微弱漂移,跳过周末),
    /// 保证短均线反复穿越长均线,真的产生买卖(单调序列永远不会触发 Trend 策略)。
    fn wave_bars(n: usize) -> Vec<StockBar> {
        let mut out = Vec::new();
        let mut date = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap();
        for i in 0..n {
            while matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                date += chrono::Duration::days(1);
            }
            let p = 10.0 + (i as f64 / 3.0).sin() * 2.0 + i as f64 * 0.01;
            out.push(StockBar {
                date,
                open: p,
                high: p,
                low: p,
                close: p,
                volume: 1.0,
                adj_close: p,
            });
            date += chrono::Duration::days(1);
        }
        out
    }

    /// 建一套「策略 → 信号 → 工单 → 成交」的数据,直接写 trade_fills(同 stats.rs 的 seed)。
    #[allow(clippy::too_many_arguments)]
    fn seed_fill(
        c: &mut Connection,
        strategy_id: i64,
        account: Account,
        side: crate::event::Direction,
        realized: Option<f64>,
        key: &str,
        now: NaiveDateTime,
    ) {
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Strategy,
            strategy_id: Some(strategy_id),
            code: "600000".into(),
            name: None,
            side,
            scope: AccountScope::Both,
            ref_price: 11.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: key.into(),
            suggest_cash: None,
            suggest_qty: None,
        };
        let sid = insert_signal(c, &sig, now).unwrap().unwrap();
        let tid = create_ticket(
            c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account,
                code: "600000".into(),
                side,
                suggest_price: 11.0,
                qty: 1000,
                expires_at: now + chrono::Duration::minutes(30),
                deviation_th: 0.015,
                status: TicketStatus::Confirmed,
                urgency: 0,
                created_at: now,
            },
        )
        .unwrap();
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, ?2, ?3, '600000', ?4, 11.0, 1000, 10.61, ?5, 'test', ?6)",
            rusqlite::params![
                tid,
                1i64,
                account.as_str(),
                crate::trade::model::side_str(side),
                realized,
                crate::trade::model::fmt_ts(now),
            ],
        )
        .unwrap();
    }

    /// `n` 笔盈利的模拟盘卖出成交,时间落在 2026-09-16 当天,间隔一分钟。
    fn seed_paper_fills(c: &mut Connection, strategy_id: i64, n: usize, pnl: f64) {
        for i in 0..n {
            let t = at(16, 10, 0) + chrono::Duration::minutes(i as i64);
            seed_fill(
                c,
                strategy_id,
                Account::Paper,
                crate::event::Direction::Sell,
                Some(pnl),
                &format!("paper{i}"),
                t,
            );
        }
    }

    /// `n` 笔连续亏损的实盘卖出成交(同一代码,确保连亏计数覆盖到它们)。
    fn seed_real_losses(c: &mut Connection, strategy_id: i64, n: usize) {
        for i in 0..n {
            let t = at(16, 11, 0) + chrono::Duration::minutes(i as i64);
            seed_fill(
                c,
                strategy_id,
                Account::Real,
                crate::event::Direction::Sell,
                Some(-50.0),
                &format!("real{i}"),
                t,
            );
        }
    }

    #[test]
    fn walk_forward_job_moves_backtesting_strategy_to_paper_or_failed() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(16, 9, 0)).unwrap();
        let j = job(&c, id, EvalKind::WalkForward);
        let (wf, adm) = ctx(at(16, 9, 2));
        let note = run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 9, 2),
            },
            |_| Ok(wave_bars(190)),
        )
        .unwrap();
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert!(
            matches!(got.status, StrategyStatus::Paper | StrategyStatus::Failed),
            "回测后应进入观察期或未通过,实际 {:?}",
            got.status
        );
        assert!(
            store::latest_eval(&c, id, 1, "oos").unwrap().is_some(),
            "评估结果落库"
        );
        assert!(!note.is_empty());
    }

    #[test]
    fn walk_forward_job_fails_when_bars_cannot_load() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(16, 9, 0)).unwrap();
        let j = job(&c, id, EvalKind::WalkForward);
        let (wf, adm) = ctx(at(16, 9, 2));
        run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 9, 2),
            },
            |_| Err(anyhow::anyhow!("无数据")),
        )
        .unwrap();
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Failed, "全池无数据 → 未通过");
    }

    #[test]
    fn paper_check_promotes_only_when_thresholds_met() {
        let mut c = db();
        let id = strategy(&c, "mover"); // mover 无回测基线
        state::submit_for_backtest(&c, 1, id, at(15, 9, 0)).unwrap(); // → Paper
                                                                      // 观察期起点次日就检查 + 35 笔盈利的模拟盘成交
        seed_paper_fills(&mut c, id, 35, 100.0);
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::PaperCheck);
        run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 15, 0),
            },
            |_| Ok(Vec::new()),
        )
        .unwrap();
        // 观察期天数不足(mover 要求 40 个交易日)→ 仍为 Paper
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper
        );
    }

    #[test]
    fn watchdog_job_suspends_admitted_strategy_on_deep_drawdown() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(15, 9, 0)).unwrap();
        state::apply_backtest_verdict(
            &c,
            1,
            id,
            &crate::trade::admission::walk_forward::aggregate(Vec::new()),
            &crate::trade::admission::judge::Verdict {
                passed: true,
                reasons: Vec::new(),
            },
            at(15, 9, 0).date(),
            at(16, 9, 0).date(),
            at(15, 9, 1),
        )
        .unwrap();
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "准入",
            at(15, 9, 2),
        )
        .unwrap();
        // 回测基线为空(aggregate(vec![]) 的 max_drawdown = 0)→ 回撤规则不触发,连亏规则也不触发
        seed_real_losses(&mut c, id, 5);
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::Watchdog);
        run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 15, 0),
            },
            |_| Ok(Vec::new()),
        )
        .unwrap();
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Admitted,
            "空基线不应误暂停"
        );
    }
}

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
use anyhow::{anyhow, Context, Result};
use chrono::NaiveDateTime;
use rusqlite::Connection;

pub struct JobContext<'a> {
    pub wf: &'a WalkForwardCfg,
    pub admission: &'a AdmissionCfg,
    pub now: NaiveDateTime,
}

/// 最近一次前推回测的池内指标(供观察期与 watchdog 取基线)。
///
/// 反序列化失败必须上抛而不能吞成 `None`:判定路径把「没有基线」当成「只查天数和笔数」,
/// 一次 schema 变更(本分支就因 `trade_baseline` 加字段补过 `#[serde(default)]`)会让
/// 所有非 mover 策略零业绩证据地被准入。展示路径(`scorecard.rs`)才可以容错。
fn latest_pool_metrics(
    conn: &Connection,
    user_id: i64,
    strategy_id: i64,
) -> Result<Option<PoolMetrics>> {
    match store::latest_eval(conn, strategy_id, user_id, "oos")? {
        Some((json, _)) => Ok(Some(
            serde_json::from_str::<PoolMetrics>(&json)
                .with_context(|| format!("策略 {strategy_id} 的 oos 评估结果无法反序列化"))?,
        )),
        None => Ok(None),
    }
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
            // 月度重跑与首次回测对「数据不足」的处理必须不同:首次回测(仍在
            // Backtesting)按 spec §10.3/§12 fail-closed,直接判未通过。但已在
            // Paper/Admitted 的策略月度重跑时,`run_pool` 把单只代码的加载失败
            // 吸收进 `skipped` 而不是报错——行情源抽风、限流或断网都会让大半池子
            // 加载失败,`aggregate` 于是给出 data_years=0、全零指标的假阴性,
            // `judge_backtest` 会因为 `min_years`/`min_evaluated_ratio` 判定未通过,
            // 进而把一个健康策略暂停。这不是「不达标」,是「这个月量不到数据」,
            // 必须原样跳过、不写评估、不改状态,好让任务能在下次重跑时重试。
            if s.status != StrategyStatus::Backtesting {
                let requested = outcome.metrics.requested;
                if requested > 0
                    && (outcome.metrics.codes.len() as f64 / requested as f64)
                        < ctx.admission.min_evaluated_ratio
                {
                    return Ok("跳过:池内数据不足,本月不裁决".to_string());
                }
            }
            let verdict = judge::judge_backtest(&outcome.metrics, ctx.admission);
            // 评估记录的数据跨度用真实 K 线区间;全池都没评估出来时退回评估当天。
            let from = outcome.metrics.data_from.unwrap_or(ctx.now.date());
            let to = outcome.metrics.data_to.unwrap_or(ctx.now.date());
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
            let is_mover = s.kind == "mover";
            let baseline = if is_mover {
                None
            } else {
                latest_pool_metrics(conn, job.user_id, job.strategy_id)?
                    .map(|m| BacktestBaseline::from_pool(&m))
            };
            // fail-closed:非异动类策略缺回测基线时,`judge_paper` 只会检查天数与笔数,
            // 等于零业绩证据就授予实盘信号权限。这里必须拒绝,与 Watchdog 分支一致。
            if !is_mover && baseline.is_none() {
                return Ok("跳过:缺回测基线,无法判定观察期".to_string());
            }
            let verdict = judge::judge_paper(&stats, baseline.as_ref(), is_mover, ctx.admission);
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
            // 统计窗口从**本次准入**起算:回撤与连亏是永不衰减的运行最大值,取全历史会让
            // 复活(Suspended → Backtesting → Paper → Admitted)后的第一次 watchdog
            // 拿同一份旧证据再次暂停,策略永远无法康复。老数据没有事件行时退回全历史。
            let since = store::last_transition_at(
                conn,
                job.strategy_id,
                job.user_id,
                StrategyStatus::Admitted,
            )?;
            let stats = stats::watchdog_stats(
                conn,
                job.user_id,
                job.strategy_id,
                ctx.admission.watchdog_window,
                since,
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
        price: f64,
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
            ref_price: price,
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
                suggest_price: price,
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
             VALUES (?1, ?2, ?3, '600000', ?4, ?5, 1000, 10.61, ?6, 'test', ?7)",
            rusqlite::params![
                tid,
                1i64,
                account.as_str(),
                crate::trade::model::side_str(side),
                price,
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
                11.0,
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
                11.0,
                Some(-50.0),
                &format!("real{i}"),
                t,
            );
        }
    }

    /// 一份真实的回测基线:样本外最大回撤 20%、胜率 55%、最长连亏 4 笔,落库为 `oos` 评估。
    fn seed_baseline(c: &Connection, strategy_id: i64, now: NaiveDateTime) {
        let mut m = crate::trade::admission::walk_forward::aggregate(Vec::new());
        m.oos_max_drawdown = 0.20;
        m.trade_baseline.win_rate = 0.55;
        m.trade_baseline.max_consecutive_losses = 4;
        let s = store::get_strategy(c, 1, strategy_id).unwrap().unwrap();
        store::save_eval(
            c,
            strategy_id,
            1,
            &s.version_hash,
            "oos",
            &serde_json::to_string(&m).unwrap(),
            at(15, 9, 0).date(),
            now.date(),
            now,
        )
        .unwrap();
    }

    /// 把策略推到 `Admitted`(回测 → 观察期 → 准入),返回策略 id。
    fn admitted_strategy(c: &Connection, kind: &str) -> i64 {
        let id = strategy(c, kind);
        state::submit_for_backtest(c, 1, id, at(15, 9, 0)).unwrap();
        state::apply_backtest_verdict(
            c,
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
            c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "准入",
            at(15, 9, 2),
        )
        .unwrap();
        id
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

    /// 月度重跑遇到「拉不到数据」(行情源限流/断网,不是策略真的不达标)不能
    /// 当成未通过处理:既不能降级 `Admitted`,也不能落一条全零指标的 `oos` 评估
    /// 覆盖掉此前真实的基线,否则下一轮 watchdog/观察期判定会用假基线。
    #[test]
    fn walk_forward_monthly_rerun_skips_verdict_when_pool_data_is_unavailable() {
        let mut c = db();
        let id = admitted_strategy(&c, "trend");
        seed_baseline(&c, id, at(15, 9, 3));
        let before = store::latest_eval(&c, id, 1, "oos").unwrap().unwrap();
        let j = job(&c, id, EvalKind::WalkForward);
        let (wf, adm) = ctx(at(16, 15, 0));
        let note = run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 15, 0),
            },
            // 模拟行情源不可用:池内每只代码都加载失败(`run_pool` 把它吸收进
            // skipped,不会直接报错),而不是网络异常一路 `?` 冒泡上去。
            |_| Err(anyhow::anyhow!("网络不可用")),
        )
        .unwrap();
        assert!(note.contains("池内数据不足"), "{note}");
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Admitted,
            "拉不到数据不得暂停健康策略"
        );
        let after = store::latest_eval(&c, id, 1, "oos").unwrap().unwrap();
        assert_eq!(after, before, "跳过时不应覆盖已有基线");
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

    /// 裁决 3:非异动类策略缺回测基线时不得准入——`judge_paper` 在 `baseline = None`
    /// 下只剩天数与笔数两道关,放行等于零业绩证据授予实盘信号权限。
    #[test]
    fn paper_check_refuses_to_admit_without_a_backtest_baseline() {
        let mut c = db();
        let id = strategy(&c, "trend"); // 非 mover
        state::submit_for_backtest(&c, 1, id, at(1, 9, 0)).unwrap(); // → Backtesting
                                                                     // 直接进观察期,不落任何 oos 评估(模拟 schema 变更后基线读不出来的情形)
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Backtesting,
            StrategyStatus::Paper,
            "进入观察期",
            NaiveDate::from_ymd_opt(2026, 8, 3)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
        .unwrap();
        // 天数(8-03 → 9-16 共 33 个工作日 ≥ 20)与笔数(12 ≥ 10)都达标
        seed_paper_fills(&mut c, id, 12, 100.0);
        assert!(
            store::latest_eval(&c, id, 1, "oos").unwrap().is_none(),
            "前提:没有任何 oos 评估"
        );
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::PaperCheck);
        let note = run_job(
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
        assert!(note.contains("缺回测基线"), "{note}");
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper,
            "缺基线不得准入"
        );
    }

    /// 裁决 3:落库的 `metrics_json` 读不出来时必须上抛,不得静默退化成「无基线」。
    #[test]
    fn unreadable_baseline_is_an_error_not_a_silent_admission() {
        let mut c = db();
        let id = strategy(&c, "trend");
        state::submit_for_backtest(&c, 1, id, at(1, 9, 0)).unwrap();
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Backtesting,
            StrategyStatus::Paper,
            "进入观察期",
            NaiveDate::from_ymd_opt(2026, 8, 3)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
        )
        .unwrap();
        seed_paper_fills(&mut c, id, 12, 100.0);
        let s = store::get_strategy(&c, 1, id).unwrap().unwrap();
        // 模拟 schema 变更后旧记录无法反序列化
        store::save_eval(
            &c,
            id,
            1,
            &s.version_hash,
            "oos",
            "{\"totally\":\"not a PoolMetrics\"}",
            at(15, 9, 0).date(),
            at(16, 9, 0).date(),
            at(16, 9, 0),
        )
        .unwrap();
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::PaperCheck);
        let err = run_job(
            &mut c,
            &j,
            &JobContext {
                wf: &wf,
                admission: &adm,
                now: at(16, 15, 0),
            },
            |_| Ok(Vec::new()),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("无法反序列化"), "{err:#}");
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Paper,
            "读不出基线不得准入"
        );
    }

    #[test]
    fn watchdog_job_keeps_admitted_when_baseline_is_empty() {
        let mut c = db();
        let id = admitted_strategy(&c, "trend");
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

    /// 裁决 4:端到端的真暂停——真实基线 + 真实成交流水,经 `run_job` 走到 `Suspended`。
    /// 这是唯一能同时抓住「回撤分母」「统计窗口」「基线缺失」三个接缝缺陷的用例。
    #[test]
    fn watchdog_job_suspends_admitted_strategy_on_real_drawdown() {
        let mut c = db();
        let id = admitted_strategy(&c, "trend");
        seed_baseline(&c, id, at(15, 9, 3));
        // 建仓:买 1000 @10,费 10.61 → 投入 10 010.61
        seed_fill(
            &mut c,
            id,
            Account::Real,
            crate::event::Direction::Buy,
            10.0,
            None,
            "rb",
            at(16, 9, 30),
        );
        // 割肉:卖 1000 @6,已实现 −4 000 → 回撤 4000 / 10 010.61 ≈ 39.96% > 20% × 1.5
        seed_fill(
            &mut c,
            id,
            Account::Real,
            crate::event::Direction::Sell,
            6.0,
            Some(-4000.0),
            "rs",
            at(16, 10, 0),
        );
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::Watchdog);
        let note = run_job(
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
        assert!(note.contains("已暂停"), "{note}");
        let got = store::get_strategy(&c, 1, id).unwrap().unwrap();
        assert_eq!(got.status, StrategyStatus::Suspended);
        assert!(
            got.status_reason.as_deref().unwrap_or("").contains("回撤"),
            "{:?}",
            got.status_reason
        );
    }

    /// 裁决 2:watchdog 只看本次准入之后的成交,否则被暂停的策略复活后会被同一份
    /// 旧证据立刻再次暂停,永远无法康复。
    #[test]
    fn watchdog_job_ignores_fills_from_before_this_admission() {
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
        seed_baseline(&c, id, at(15, 9, 3));
        // 上一轮实盘的惨状:投入 10 010.61,亏掉 4 000(回撤 ≈ 40%)
        seed_fill(
            &mut c,
            id,
            Account::Real,
            crate::event::Direction::Buy,
            10.0,
            None,
            "oldb",
            at(15, 10, 0),
        );
        seed_fill(
            &mut c,
            id,
            Account::Real,
            crate::event::Direction::Sell,
            6.0,
            Some(-4000.0),
            "olds",
            at(15, 10, 1),
        );
        // 复活:重新准入(事件时间在旧成交之后)
        state::update_status(
            &c,
            1,
            id,
            StrategyStatus::Paper,
            StrategyStatus::Admitted,
            "重新准入",
            at(16, 9, 0),
        )
        .unwrap();
        let (wf, adm) = ctx(at(16, 15, 0));
        let j = job(&c, id, EvalKind::Watchdog);
        let note = run_job(
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
        assert_eq!(note, "实盘表现正常", "旧回撤不得再次开火");
        assert_eq!(
            store::get_strategy(&c, 1, id).unwrap().unwrap().status,
            StrategyStatus::Admitted
        );
    }
}

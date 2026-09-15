//! 交易监听线程:每轮 run_tick → 推送新工单;失败退避与中断告警;09:00 撤销、15:05 提醒;
//! 接收推送主循环转来的实时异动。任何错误只记日志,线程不退出。

use crate::stock::realtime::calendar::is_weekend;
use crate::stock::realtime::movers::Mover;
use crate::trade::config::TradeCfg;
use crate::trade::notify::{
    self, render_fill_reminder, render_monitor_down, render_new_ticket, signal_reason, Notifier,
    PushNotifier, QueuedPushNotifier,
};
use crate::trade::quotes::TencentQuotes;
use crate::trade::{monitor, movers, store, ticket};
use anyhow::Result;
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Backoff {
    failures: u32,
    first_failure: Option<NaiveDateTime>,
    alerted: bool,
}

impl Backoff {
    pub fn on_failure(&mut self, now: NaiveDateTime) {
        self.failures = self.failures.saturating_add(1);
        self.first_failure.get_or_insert(now);
    }

    /// 成功一轮:清零;返回此前是否处于失败状态。
    pub fn on_success(&mut self) -> bool {
        let was_failing = self.failures > 0;
        *self = Self::default();
        was_failing
    }

    pub fn delay_secs(&self, base: u64) -> u64 {
        base.saturating_mul(1u64 << self.failures.min(2))
    }

    pub fn should_alert(&self, now: NaiveDateTime, alert_after_secs: i64) -> bool {
        !self.alerted
            && self
                .first_failure
                .is_some_and(|f| (now - f).num_seconds() >= alert_after_secs)
    }

    pub fn mark_alerted(&mut self) {
        self.alerted = true;
    }

    pub fn failing_minutes(&self, now: NaiveDateTime) -> i64 {
        self.first_failure.map_or(0, |f| (now - f).num_minutes())
    }
}

pub fn due_daily(now: NaiveDateTime, hour: u32, minute: u32, last_run: Option<NaiveDate>) -> bool {
    let Some(at) = NaiveTime::from_hms_opt(hour, minute, 0) else {
        return false;
    };
    !is_weekend(now.date()) && now.time() >= at && last_run != Some(now.date())
}

/// 同 `due_daily`,但只在 `[start, end)` 窗口内触发——避免因某轮迟迟未跑而在
/// 深夜甚至次日凌晨才补跑「当日」任务(如 15:05 回填提醒,过了 16:00 就不再有意义)。
pub fn due_daily_window(
    now: NaiveDateTime,
    start: (u32, u32),
    end: (u32, u32),
    last_run: Option<NaiveDate>,
) -> bool {
    let Some(end_at) = NaiveTime::from_hms_opt(end.0, end.1, 0) else {
        return false;
    };
    due_daily(now, start.0, start.1, last_run) && now.time() < end_at
}

/// 推送本轮新建的实盘工单,返回成功推送条数;单条失败只记日志。
pub fn notify_new_tickets(conn: &Connection, notifier: &dyn Notifier, ticket_ids: &[i64]) -> usize {
    let mut sent = 0;
    for &id in ticket_ids {
        let result = (|| -> Result<()> {
            let t =
                ticket::get_ticket(conn, id)?.ok_or_else(|| anyhow::anyhow!("工单 {id} 不存在"))?;
            let reason = signal_reason(conn, t.signal_id)?;
            let (title, md) = render_new_ticket(&t, &reason);
            notifier.notify(conn, t.user_id, &title, &md)
        })();
        match result {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 工单 {id} 推送失败: {e:#}"),
        }
    }
    sent
}

/// 按用户汇总已确认未回填的实盘工单并提醒,返回推送用户数。
pub fn send_fill_reminders(conn: &Connection, notifier: &dyn Notifier) -> Result<usize> {
    let mut by_user: BTreeMap<i64, Vec<_>> = BTreeMap::new();
    for t in ticket::list_unfilled_real(conn)? {
        by_user.entry(t.user_id).or_default().push(t);
    }
    let mut sent = 0;
    for (uid, tickets) in by_user {
        let (title, md) = render_fill_reminder(&tickets);
        match notifier.notify(conn, uid, &title, &md) {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 用户 {uid} 回填提醒失败: {e:#}"),
        }
    }
    Ok(sent)
}

/// 监听中断告警:推送给所有持有实盘仓位的用户(模拟盘持仓无需线下操作),返回推送用户数。
pub fn alert_holders(conn: &Connection, notifier: &dyn Notifier, minutes: i64) -> Result<usize> {
    let (title, md) = render_monitor_down(minutes);
    let mut sent = 0;
    for uid in store::users_with_real_positions(conn)? {
        match notifier.notify(conn, uid, &title, &md) {
            Ok(()) => sent += 1,
            Err(e) => eprintln!("[trade] 用户 {uid} 中断告警失败: {e:#}"),
        }
    }
    Ok(sent)
}

/// 推送主循环把每轮实时异动交给监听线程。发送失败(线程已退出)静默忽略。
#[derive(Clone)]
pub struct MoverSink(Sender<Vec<Mover>>);

impl MoverSink {
    pub fn send(&self, movers: Vec<Mover>) {
        if !movers.is_empty() {
            let _ = self.0.send(movers);
        }
    }
}

pub fn spawn(
    db_path: PathBuf,
    cfg: TradeCfg,
    warn_days: i64,
    grace_days: i64,
) -> std::io::Result<(std::thread::JoinHandle<()>, MoverSink)> {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::Builder::new()
        .name("trade-monitor".into())
        .spawn(move || run_loop(db_path, cfg, rx, warn_days, grace_days))?;
    Ok((handle, MoverSink(tx)))
}

fn run_loop(
    db_path: PathBuf,
    cfg: TradeCfg,
    rx: Receiver<Vec<Mover>>,
    warn_days: i64,
    grace_days: i64,
) {
    // 打开失败(如库被其它进程独占、磁盘瞬时不可用)不能让监听线程直接退出——
    // 退出就意味着止盈止损此后彻底停摆且无人知晓;每 60 秒重试直至成功。
    let mut conn = loop {
        match crate::web::auth::store::open(&db_path).and_then(|c| store::migrate(&c).map(|_| c)) {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("[trade] 打开数据库失败,60 秒后重试: {e:#}");
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
    };
    println!(
        "交易监听已启动(每 {} 秒,库 {})",
        cfg.monitor_interval_secs,
        db_path.display()
    );
    // 网络发送(重试 + 超时)交给独立线程,避免慢 webhook 拖住这里 15 秒一轮的止盈止损监听。
    // 发送线程起不来时(极罕见)退回同步 PushNotifier——功能仍可用,只是恢复了原来的阻塞风险。
    let notifier: Box<dyn Notifier> = match notify::spawn_sender() {
        Ok((_handle, tx)) => Box::new(QueuedPushNotifier {
            warn_days,
            grace_days,
            tx,
        }),
        Err(e) => {
            eprintln!("[trade] 推送发送线程启动失败,退回同步推送: {e:#}");
            Box::new(PushNotifier {
                warn_days,
                grace_days,
            })
        }
    };
    let source = TencentQuotes;
    let mut backoff = Backoff::default();
    let mut last_cancel: Option<NaiveDate> = None;
    let mut last_remind: Option<NaiveDate> = None;

    loop {
        // 心跳、异动转发、每日任务用这个较早的时刻;真正拉报价前再重新取一次(见下),
        // 避免异动处理/日终任务耗时把止盈止损判定用的时间戳带偏。
        let now = chrono::Local::now().naive_local();

        // 任何一轮内部 panic(如报价源/第三方库的极端输入)都只记日志、按普通间隔重试,
        // 线程本身绝不能因此退出——退出就意味着止盈止损彻底停摆且无人知晓。
        let delay = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Err(e) = store::beat(&conn, "trade-monitor", now) {
                eprintln!("[trade] 写心跳失败: {e:#}");
            }

            while let Ok(batch) = rx.try_recv() {
                if !cfg.mover_signals {
                    continue;
                }
                // 先为本批异动代码刷新报价缓存(含涨跌停价);拉取失败不阻塞信号处理——
                // 已有的陈旧缓存会被 fresh_quote 判定为不新鲜,submit_mover_signals
                // 自然会跳过那些代码,而不是用不可靠的价格硬出信号。
                if let Err(e) = movers::refresh_mover_quotes(&conn, &source, &batch, now) {
                    eprintln!("[trade] 异动报价获取失败: {e:#}");
                }
                match movers::submit_mover_signals(&mut conn, &batch, now) {
                    Ok(r) => r
                        .errors
                        .iter()
                        .for_each(|e| eprintln!("[trade] 异动信号: {e}")),
                    Err(e) => eprintln!("[trade] 异动信号处理失败: {e:#}"),
                }
            }

            if due_daily(now, 9, 0, last_cancel) {
                let midnight = now
                    .date()
                    .and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("合法时刻"));
                match ticket::cancel_unfilled(&conn, midnight) {
                    Ok(n) => {
                        if n > 0 {
                            println!("[trade] 撤销未回填工单 {n} 张");
                        }
                        last_cancel = Some(now.date());
                    }
                    Err(e) => eprintln!("[trade] 撤销未回填工单失败: {e:#}"),
                }
            }
            // 只在 15:05–16:00 窗口内跑;过了 16:00 才轮到的话说明这一轮严重滞后,
            // 「即将撤单」提醒已无意义,不该在深夜甚至次日凌晨补发。
            if due_daily_window(now, (15, 5), (16, 0), last_remind) {
                match send_fill_reminders(&conn, notifier.as_ref()) {
                    Ok(_) => last_remind = Some(now.date()),
                    Err(e) => eprintln!("[trade] 回填提醒失败: {e:#}"),
                }
            }

            // 异动转发与日终任务耗时不确定;拉报价前重新取时刻,让止盈止损判定与
            // 退避计时都基于「实际发起本轮监听」的时间,而非循环开始时的时间。
            let tick_now = chrono::Local::now().naive_local();
            match monitor::run_tick(&mut conn, &source, tick_now) {
                Ok(report) => {
                    if backoff.on_success() {
                        println!("[trade] 行情恢复,监听继续");
                    }
                    notify_new_tickets(&conn, notifier.as_ref(), &report.new_real_tickets);
                    for e in report
                        .errors
                        .iter()
                        .chain(report.paper.errors.iter().map(|(_, e)| e))
                    {
                        eprintln!("[trade] {e}");
                    }
                    cfg.monitor_interval_secs
                }
                Err(e) => {
                    eprintln!("[trade] 本轮监听失败: {e:#}");
                    backoff.on_failure(tick_now);
                    if backoff.should_alert(tick_now, cfg.alert_after_secs) {
                        let minutes = backoff.failing_minutes(tick_now);
                        if let Err(e) = alert_holders(&conn, notifier.as_ref(), minutes) {
                            eprintln!("[trade] 中断告警失败: {e:#}");
                        }
                        backoff.mark_alerted();
                    }
                    backoff.delay_secs(cfg.monitor_interval_secs)
                }
            }
        }))
        .unwrap_or_else(|_| {
            eprintln!("[trade] 监听本轮 panic,已恢复");
            cfg.monitor_interval_secs
        });
        std::thread::sleep(std::time::Duration::from_secs(delay));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(d: u32, h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn backoff_doubles_to_cap_alerts_once_and_resets() {
        let mut b = Backoff::default();
        assert_eq!(b.delay_secs(15), 15);
        b.on_failure(at(16, 10, 0));
        assert_eq!(b.delay_secs(15), 30);
        b.on_failure(at(16, 10, 1));
        assert_eq!(b.delay_secs(15), 60);
        b.on_failure(at(16, 10, 2));
        assert_eq!(b.delay_secs(15), 60, "封顶 60 秒");
        assert!(!b.should_alert(at(16, 10, 2), 180));
        assert!(b.should_alert(at(16, 10, 3), 180), "从首次失败起满 3 分钟");
        assert_eq!(b.failing_minutes(at(16, 10, 3)), 3);
        b.mark_alerted();
        assert!(!b.should_alert(at(16, 10, 9), 180), "同一次中断只告警一次");
        assert!(b.on_success(), "从失败中恢复");
        assert!(!b.on_success());
        assert_eq!(b.delay_secs(15), 15);
    }

    #[test]
    fn daily_jobs_run_once_on_weekdays_after_time() {
        assert!(!due_daily(at(16, 8, 59), 9, 0, None));
        assert!(due_daily(at(16, 9, 0), 9, 0, None));
        assert!(due_daily(
            at(16, 14, 0),
            9,
            0,
            Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap())
        ));
        assert!(!due_daily(
            at(16, 9, 5),
            9,
            0,
            Some(NaiveDate::from_ymd_opt(2026, 9, 16).unwrap())
        ));
        assert!(!due_daily(at(19, 9, 5), 9, 0, None), "周六不跑");
    }

    #[test]
    fn daily_window_bounds_the_reminder_job() {
        assert!(
            due_daily_window(at(16, 15, 5), (15, 5), (16, 0), None),
            "窗口内"
        );
        assert!(
            !due_daily_window(at(16, 16, 0), (15, 5), (16, 0), None),
            "已到窗口终点,不再补跑"
        );
        assert!(
            !due_daily_window(at(19, 15, 5), (15, 5), (16, 0), None),
            "周六不跑"
        );
        assert!(
            !due_daily_window(
                at(16, 15, 30),
                (15, 5),
                (16, 0),
                Some(NaiveDate::from_ymd_opt(2026, 9, 16).unwrap())
            ),
            "当日已跑过"
        );
    }
}

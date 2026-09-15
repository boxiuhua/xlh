//! 交易监听线程:每轮 run_tick → 推送新工单;失败退避与中断告警;09:00 撤销、15:05 提醒;
//! 接收推送主循环转来的实时异动。任何错误只记日志,线程不退出。

use crate::stock::realtime::calendar::is_weekend;
use crate::stock::realtime::movers::Mover;
use crate::trade::config::TradeCfg;
use crate::trade::notify::{
    render_fill_reminder, render_monitor_down, render_new_ticket, signal_reason, Notifier,
    PushNotifier,
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
        base * (1u64 << self.failures.min(2))
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

/// 监听中断告警:推送给所有有持仓的用户,返回推送用户数。
pub fn alert_holders(conn: &Connection, notifier: &dyn Notifier, minutes: i64) -> Result<usize> {
    let (title, md) = render_monitor_down(minutes);
    let mut sent = 0;
    for uid in store::users_with_positions(conn)? {
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
    let mut conn =
        match crate::web::auth::store::open(&db_path).and_then(|c| store::migrate(&c).map(|_| c)) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[trade] 打开数据库失败,交易监听未启动: {e:#}");
                return;
            }
        };
    println!(
        "交易监听已启动(每 {} 秒,库 {})",
        cfg.monitor_interval_secs,
        db_path.display()
    );
    let notifier = PushNotifier {
        warn_days,
        grace_days,
    };
    let source = TencentQuotes;
    let mut backoff = Backoff::default();
    let mut last_cancel: Option<NaiveDate> = None;
    let mut last_remind: Option<NaiveDate> = None;

    loop {
        let now = chrono::Local::now().naive_local();
        if let Err(e) = store::beat(&conn, "trade-monitor", now) {
            eprintln!("[trade] 写心跳失败: {e:#}");
        }

        while let Ok(batch) = rx.try_recv() {
            if !cfg.mover_signals {
                continue;
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
                Ok(n) if n > 0 => println!("[trade] 撤销未回填工单 {n} 张"),
                Ok(_) => {}
                Err(e) => eprintln!("[trade] 撤销未回填工单失败: {e:#}"),
            }
            last_cancel = Some(now.date());
        }
        if due_daily(now, 15, 5, last_remind) {
            if let Err(e) = send_fill_reminders(&conn, &notifier) {
                eprintln!("[trade] 回填提醒失败: {e:#}");
            }
            last_remind = Some(now.date());
        }

        let delay = match monitor::run_tick(&mut conn, &source, now) {
            Ok(report) => {
                if backoff.on_success() {
                    println!("[trade] 行情恢复,监听继续");
                }
                notify_new_tickets(&conn, &notifier, &report.new_real_tickets);
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
                backoff.on_failure(now);
                if backoff.should_alert(now, cfg.alert_after_secs) {
                    let minutes = backoff.failing_minutes(now);
                    if let Err(e) = alert_holders(&conn, &notifier, minutes) {
                        eprintln!("[trade] 中断告警失败: {e:#}");
                    }
                    backoff.mark_alerted();
                }
                backoff.delay_secs(cfg.monitor_interval_secs)
            }
        };
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
}

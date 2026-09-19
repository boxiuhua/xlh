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
use crate::trade::report::{build_daily_report, render_daily_report};
use crate::trade::{link, monitor, movers, settings, store, ticket};
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

/// 交易日历自证探针出错后的重试间隔(分钟)。
const PROBE_RETRY_MINUTES: i64 = 5;

/// 交易日历自证是否到点:工作日 09:31 起、当日尚无结论;上次出错后
/// `PROBE_RETRY_MINUTES` 分钟内不再试——网络不好时每轮都搭上一次最长 20 秒的
/// 快照抓取,只会拖慢同一线程里的止盈止损。
pub fn probe_due(
    now: NaiveDateTime,
    last_probe: Option<NaiveDate>,
    last_failure: Option<NaiveDateTime>,
) -> bool {
    due_daily(now, 9, 31, last_probe)
        && last_failure.is_none_or(|f| (now - f).num_minutes() >= PROBE_RETRY_MINUTES)
}

/// 推送本轮新建的实盘工单,返回成功推送条数;单条失败只记日志。
/// `link_base` 非空时,每张工单附带签名链接;取密钥失败只记日志、降级为无链接。
pub fn notify_new_tickets(
    conn: &Connection,
    notifier: &dyn Notifier,
    ticket_ids: &[i64],
    link_base: &str,
) -> usize {
    let secret = if link_base.is_empty() {
        None
    } else {
        match settings::link_secret(conn) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[trade] 取工单链接密钥失败,本轮推送不带链接: {e:#}");
                None
            }
        }
    };
    let mut sent = 0;
    for &id in ticket_ids {
        let result = (|| -> Result<()> {
            let t =
                ticket::get_ticket(conn, id)?.ok_or_else(|| anyhow::anyhow!("工单 {id} 不存在"))?;
            let reason = signal_reason(conn, t.signal_id)?;
            let url = secret
                .as_ref()
                .map(|secret| link::ticket_url(link_base, secret, &t));
            let (title, md) = render_new_ticket(&t, &reason, url.as_deref());
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

/// 收盘后按交易日推送交易日报,返回推送用户数(设计裁决 1、3)。
/// 用户集合 = 有实盘账户 ∪ 有策略 ∪ 有持仓(去重、升序);当日无任何活动且无持仓的用户
/// `build_daily_report` 返回 `None`,不推送(不刷屏)。单个用户推送失败只记日志、继续下一个。
pub fn send_daily_reports(
    conn: &Connection,
    notifier: &dyn Notifier,
    date: NaiveDate,
) -> Result<usize> {
    let mut users: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    users.extend(store::users_with_real_account(conn)?);
    users.extend(store::users_with_strategies(conn)?);
    users.extend(store::users_with_positions(conn)?);

    let mut sent = 0;
    for uid in users {
        match build_daily_report(conn, uid, date) {
            Ok(Some(report)) => {
                let (title, md) = render_daily_report(&report);
                match notifier.notify(conn, uid, &title, &md) {
                    Ok(()) => sent += 1,
                    Err(e) => eprintln!("[trade] 用户 {uid} 日报推送失败: {e:#}"),
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("[trade] 用户 {uid} 日报生成失败: {e:#}"),
        }
    }
    Ok(sent)
}

/// 日报调度(设计裁决 1、3):开启时在 [配置时刻, 18:00) 窗口内每个交易日执行一次。
/// 休市日只把 `last` 记为当日(当日不再判断),不写持久标记;交易日执行后 `last`
/// 记为当日并写心跳 `trade-daily-report`(守护重启时从它读回 `last`,同日不重发)。
/// 任何错误只记日志。
pub fn run_daily_report(
    conn: &Connection,
    notifier: &dyn Notifier,
    cfg: &TradeCfg,
    now: NaiveDateTime,
    last: &mut Option<NaiveDate>,
) {
    if !cfg.daily_report
        || !due_daily_window(
            now,
            (cfg.daily_report_hour, cfg.daily_report_minute),
            (18, 0),
            *last,
        )
    {
        return;
    }
    match crate::trade::calendar::is_trading_day(conn, now.date()) {
        Ok(false) => *last = Some(now.date()), // 休市日不发,当日不再判断
        Ok(true) => match send_daily_reports(conn, notifier, now.date()) {
            Ok(n) => {
                // n 只是 notify 返回成功的用户数:未配置推送渠道或授权失效的用户,
                // Notifier 会静默跳过并同样返回成功,故这里说「已处理」而非「已推送」。
                println!("[trade] 已处理交易日报 {n} 份(未配置推送渠道或授权失效的用户会被跳过)");
                *last = Some(now.date());
                if let Err(e) = store::beat(conn, "trade-daily-report", now) {
                    eprintln!("[trade] 日报标记写入失败: {e:#}");
                }
            }
            Err(e) => eprintln!("[trade] 交易日报失败: {e:#}"),
        },
        Err(e) => eprintln!("[trade] 交易日历读取失败: {e:#}"),
    }
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
    let mut last_probe: Option<NaiveDate> = None;
    let mut last_probe_failure: Option<NaiveDateTime> = None;
    let mut last_report = match store::last_beat(&conn, "trade-daily-report") {
        Ok(t) => t.map(|t| t.date()),
        Err(e) => {
            eprintln!("[trade] 日报心跳读取失败,按未发过处理: {e:#}");
            None
        }
    };

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
            let delay = match monitor::run_tick(&mut conn, &source, tick_now) {
                Ok(report) => {
                    if backoff.on_success() {
                        println!("[trade] 行情恢复,监听继续");
                    }
                    notify_new_tickets(
                        &conn,
                        notifier.as_ref(),
                        &report.new_real_tickets,
                        &cfg.link_base_url,
                    );
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
            };

            // 日历自证与日线信号发出都要联网拉快照(各自最长 20 秒超时),放在止盈止损
            // 之后:网络不好时它们只会推迟下一轮,而不会挡在本轮止损前面。
            // 止盈止损可能耗时,这里重新取时刻。
            let now = chrono::Local::now().naive_local();
            // 交易日历自证:每个工作日开盘后探一次,直到得出结论(见 trade::calendar)。
            if probe_due(now, last_probe, last_probe_failure) {
                match crate::trade::calendar::probe(&conn, &source, now) {
                    Ok(Some(open)) => {
                        last_probe = Some(now.date());
                        if !open {
                            println!("[trade] {} 休市(开盘后行情仍非今日)", now.date());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        last_probe_failure = Some(now);
                        eprintln!("[trade] 交易日历自证失败: {e:#}");
                    }
                }
            }

            // 日线策略信号:窗口内每轮都尝试(无可发计划时只是一次本地查询)。
            match crate::trade::daily_signals::emit_due(&mut conn, &source, &cfg.signals, now) {
                Ok(r) => {
                    notify_new_tickets(
                        &conn,
                        notifier.as_ref(),
                        &r.new_real_tickets,
                        &cfg.link_base_url,
                    );
                    for e in &r.errors {
                        eprintln!("[trade] 日线信号发出: {e}");
                    }
                    if r.submitted + r.dropped > 0 {
                        println!(
                            "[trade] 日线信号:发出 {} 条、作废 {} 条",
                            r.submitted, r.dropped
                        );
                    }
                }
                Err(e) => eprintln!("[trade] 日线信号发出失败: {e:#}"),
            }
            if let Err(e) = crate::trade::daily_signals::drop_unsent(&conn, &cfg.signals, now) {
                eprintln!("[trade] 作废过期信号计划失败: {e:#}");
            }

            run_daily_report(&conn, notifier.as_ref(), &cfg, now, &mut last_report);
            delay
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
    use crate::event::Direction;
    use crate::trade::model::{
        Account, AccountScope, NewSignal, Position, SignalSource, TicketStatus,
    };
    use crate::trade::ticket::NewTicket;
    use chrono::NaiveDate;
    use std::cell::RefCell;

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
    fn calendar_probe_runs_after_0931_until_concluded_and_backs_off_after_errors() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        assert!(!probe_due(at(16, 9, 30), None, None), "开盘自证从 09:31 起");
        assert!(probe_due(at(16, 9, 31), None, None));
        assert!(!probe_due(at(16, 10, 0), Some(day), None), "当日已有结论");
        assert!(!probe_due(at(19, 9, 31), None, None), "周六不探");
        // 出错后 5 分钟内不重试,免得每 15 秒一轮都搭上一次 20 秒超时的抓取
        let failed = Some(at(16, 9, 31));
        assert!(!probe_due(at(16, 9, 35), None, failed));
        assert!(probe_due(at(16, 9, 36), None, failed));
        assert!(
            probe_due(at(17, 9, 31), None, Some(at(16, 14, 58))),
            "昨天的失败不拦今天"
        );
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

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        store::migrate(&c).unwrap();
        c
    }

    /// Recorder 形式的 Notifier 桩(仿 tests/trade_runtime.rs 的 Recorder):记录成功推送的
    /// 用户,可指定某个用户 id 让 `notify` 返回 Err,验证单个用户失败不影响其它用户。
    #[derive(Default)]
    struct FailingRecorder {
        calls: RefCell<Vec<i64>>,
        fail_for: Option<i64>,
    }

    impl Notifier for FailingRecorder {
        fn notify(&self, _conn: &Connection, user_id: i64, _title: &str, _md: &str) -> Result<()> {
            if Some(user_id) == self.fail_for {
                anyhow::bail!("推送失败(用户 {user_id})");
            }
            self.calls.borrow_mut().push(user_id);
            Ok(())
        }
    }

    #[test]
    fn daily_reports_go_to_active_users_only_and_survive_one_failure() {
        let c = db();
        let today = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();

        // 三个用户都开了实盘账户(空账户、当日无活动、无持仓的用户不该被推送)。
        for uid in [1, 2, 3] {
            store::set_capital(&c, uid, Account::Real, 100_000.0, at(20, 9, 0)).unwrap();
        }

        // 用户 1:当日一笔实盘成交。
        let sig = NewSignal {
            user_id: 1,
            source: SignalSource::Manual,
            strategy_id: None,
            code: "600001".into(),
            name: None,
            side: Direction::Buy,
            scope: AccountScope::Both,
            ref_price: 10.0,
            reason: "测试".into(),
            ai_note: None,
            dedup_key: "u1".into(),
            suggest_cash: None,
            suggest_qty: None,
        };
        let sid = ticket::insert_signal(&c, &sig, at(23, 9, 30))
            .unwrap()
            .unwrap();
        ticket::mark_signal(&c, sid, "ticketed", None).unwrap();
        let tid = ticket::create_ticket(
            &c,
            &NewTicket {
                user_id: 1,
                signal_id: sid,
                account: Account::Real,
                code: "600001".into(),
                side: Direction::Buy,
                suggest_price: 10.0,
                qty: 100,
                expires_at: at(23, 10, 0),
                deviation_th: 0.015,
                status: TicketStatus::Filled,
                urgency: 0,
                created_at: at(23, 9, 30),
            },
        )
        .unwrap();
        c.execute(
            "INSERT INTO trade_fills (ticket_id, user_id, account, code, side, price, qty, fee, realized_pnl, source, filled_at)
             VALUES (?1, 1, 'real', '600001', 'buy', 10.0, 100, 5.0, NULL, 'manual', ?2)",
            rusqlite::params![tid, crate::trade::model::fmt_ts(at(23, 9, 35))],
        )
        .unwrap();

        // 用户 2:只有持仓,当日无其它活动。
        let mut pos = Position::empty(2, Account::Real, "600002");
        pos.qty = 100;
        pos.avg_cost = 10.0;
        store::upsert_position(&c, &pos, at(20, 9, 0)).unwrap();

        // 用户 3:只有空账户,当日无活动、无持仓 —— 不应被推送。

        let notifier = FailingRecorder {
            fail_for: Some(1),
            ..Default::default()
        };
        let sent = send_daily_reports(&c, &notifier, today).unwrap();
        assert_eq!(sent, 1, "用户 1 推送失败;用户 2 成功;用户 3 无活动不发");
        assert_eq!(*notifier.calls.borrow(), vec![2]);
    }

    /// 用户 1 有一只实盘持仓 —— 任一交易日都有日报可发。
    fn db_with_holder() -> Connection {
        let c = db();
        let mut pos = Position::empty(1, Account::Real, "600002");
        pos.qty = 100;
        pos.avg_cost = 10.0;
        store::upsert_position(&c, &pos, at(20, 9, 0)).unwrap();
        c
    }

    fn report_marker(c: &Connection) -> Option<NaiveDateTime> {
        store::last_beat(c, "trade-daily-report").unwrap()
    }

    #[test]
    fn daily_report_skips_holidays_without_persisting_marker() {
        let c = db_with_holder();
        let d23 = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        crate::trade::calendar::mark_day(&c, d23, false, at(23, 9, 31)).unwrap();
        let notifier = FailingRecorder::default();
        let mut last = None;
        run_daily_report(
            &c,
            &notifier,
            &TradeCfg::default(),
            at(23, 15, 40),
            &mut last,
        );
        assert!(notifier.calls.borrow().is_empty(), "休市日不发");
        assert_eq!(last, Some(d23), "当日不再判断");
        assert_eq!(report_marker(&c), None, "休市日不写持久标记");
    }

    #[test]
    fn daily_report_sends_once_per_trading_day_and_survives_restart() {
        let c = db_with_holder();
        let cfg = TradeCfg::default();
        let d23 = NaiveDate::from_ymd_opt(2026, 9, 23).unwrap();
        let notifier = FailingRecorder::default();
        let mut last = None;

        run_daily_report(&c, &notifier, &cfg, at(23, 15, 40), &mut last);
        assert_eq!(*notifier.calls.borrow(), vec![1]);
        assert_eq!(last, Some(d23));
        assert_eq!(report_marker(&c), Some(at(23, 15, 40)));

        // 同日再调用:不重发,标记不变
        run_daily_report(&c, &notifier, &cfg, at(23, 16, 0), &mut last);
        assert_eq!(*notifier.calls.borrow(), vec![1]);
        assert_eq!(report_marker(&c), Some(at(23, 15, 40)));

        // 模拟重启:从持久标记读回 last,同日不重发
        let mut restored = report_marker(&c).map(|t| t.date());
        let fresh = FailingRecorder::default();
        run_daily_report(&c, &fresh, &cfg, at(23, 17, 0), &mut restored);
        assert!(fresh.calls.borrow().is_empty(), "重启后同日不重发");
    }

    #[test]
    fn daily_report_only_runs_inside_the_window_and_when_enabled() {
        let c = db_with_holder();
        let cfg = TradeCfg::default();
        let notifier = FailingRecorder::default();
        let mut last = None;
        run_daily_report(&c, &notifier, &cfg, at(23, 15, 34), &mut last);
        run_daily_report(&c, &notifier, &cfg, at(23, 18, 0), &mut last);
        assert!(notifier.calls.borrow().is_empty(), "窗口外不发");
        assert_eq!(last, None);
        assert_eq!(report_marker(&c), None);

        let off = TradeCfg {
            daily_report: false,
            ..TradeCfg::default()
        };
        run_daily_report(&c, &notifier, &off, at(23, 15, 40), &mut last);
        assert!(notifier.calls.borrow().is_empty(), "关闭日报不发");
        assert_eq!(last, None);
    }
}

//! 交易推送:渲染文案 + 经用户推送渠道发送。

use crate::event::Direction;
use crate::trade::model::Ticket;
use anyhow::{anyhow, Result};
use rusqlite::{Connection, OptionalExtension};

pub trait Notifier {
    fn notify(&self, conn: &Connection, user_id: i64, title: &str, md: &str) -> Result<()>;
}

/// 经用户在推送设置里配置的渠道发送;未授权 / 未配置渠道的用户静默跳过。
pub struct PushNotifier {
    pub warn_days: i64,
    pub grace_days: i64,
}

impl Notifier for PushNotifier {
    fn notify(&self, conn: &Connection, user_id: i64, title: &str, md: &str) -> Result<()> {
        let today = chrono::Local::now().date_naive();
        if !crate::push::schedule::user_allowed(
            conn,
            user_id,
            today,
            self.warn_days,
            self.grace_days,
        ) {
            return Ok(());
        }
        let Some(cfg) = crate::push::store::get(conn, user_id)? else {
            return Ok(());
        };
        if cfg.channel.webhook.trim().is_empty() {
            return Ok(());
        }
        crate::push::channels::send(&cfg.channel, title, md)
    }
}

pub fn side_label(side: Direction) -> &'static str {
    match side {
        Direction::Buy => "买入",
        Direction::Sell => "卖出",
    }
}

pub fn render_new_ticket(t: &Ticket, reason: &str) -> (String, String) {
    let title = format!(
        "交易工单:{} {}{}",
        side_label(t.side),
        t.code,
        if t.urgency > 0 { "(重发)" } else { "" }
    );
    let mut md = format!(
        "### {title}\n\n- 数量:{} 股\n- 建议价:{:.2}(现价偏离超过 {:.1}% 需二次确认)\n- 有效期至:{}\n- 理由:{reason}\n",
        t.qty,
        t.suggest_price,
        t.deviation_th * 100.0,
        t.expires_at.format("%H:%M"),
    );
    if t.urgency > 0 {
        md.push_str(&format!(
            "- ⚠ 第 {} 次提醒:上一张工单已过期,触发条件仍然成立\n",
            t.urgency + 1
        ));
    }
    md.push_str("\n请在「交易」页确认,在券商 App 下单后回填成交。");
    (title, md)
}

pub fn render_fill_reminder(tickets: &[Ticket]) -> (String, String) {
    let title = "待回填成交提醒".to_string();
    let mut md = format!("### {title}\n\n以下工单已确认但尚未回填,次日 09:00 将自动撤销:\n\n");
    for t in tickets {
        md.push_str(&format!(
            "- {} {}:已回填 {} / {} 股\n",
            side_label(t.side),
            t.code,
            t.filled_qty,
            t.qty
        ));
    }
    (title, md)
}

pub fn render_monitor_down(minutes: i64) -> (String, String) {
    let title = "止损监听中断".to_string();
    let md = format!(
        "### {title}\n\n行情获取已连续失败 {minutes} 分钟,止盈止损暂未生效,请自行关注持仓。恢复后将自动继续监听。"
    );
    (title, md)
}

pub fn signal_reason(conn: &Connection, signal_id: i64) -> Result<String> {
    conn.query_row(
        "SELECT reason FROM trade_signals WHERE id = ?1",
        [signal_id],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| anyhow!("信号 {signal_id} 不存在"))
}

#[cfg(test)]
mod tests {
    use crate::trade::model::{Account, TicketStatus};
    use chrono::NaiveDate;

    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 16)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn ticket(urgency: i64) -> crate::trade::model::Ticket {
        crate::trade::model::Ticket {
            id: 7,
            user_id: 1,
            signal_id: 3,
            account: Account::Real,
            code: "600000".into(),
            side: crate::event::Direction::Sell,
            suggest_price: 9.1,
            qty: 1000,
            filled_qty: 0,
            expires_at: at(10, 30),
            deviation_th: 0.015,
            status: TicketStatus::Pending,
            urgency,
            created_at: at(10, 0),
            confirmed_at: None,
            ignore_reason: None,
        }
    }

    #[test]
    fn new_ticket_message() {
        let (title, md) = super::render_new_ticket(&ticket(0), "触发止损:现价 9.100 ≤ 9.200");
        assert_eq!(title, "交易工单:卖出 600000");
        for s in ["1000 股", "9.10", "10:30", "1.5%", "触发止损", "回填"] {
            assert!(md.contains(s), "缺少 {s}: {md}");
        }
        assert!(!md.contains("次提醒"));
        let (title, md) = super::render_new_ticket(&ticket(1), "r");
        assert_eq!(title, "交易工单:卖出 600000(重发)");
        assert!(md.contains("第 2 次提醒"), "{md}");
    }

    #[test]
    fn reminder_and_alert_messages() {
        let mut partial = ticket(0);
        partial.filled_qty = 400;
        partial.side = crate::event::Direction::Buy;
        let (title, md) = super::render_fill_reminder(&[ticket(0), partial]);
        assert_eq!(title, "待回填成交提醒");
        assert!(md.contains("卖出 600000:已回填 0 / 1000 股"), "{md}");
        assert!(md.contains("买入 600000:已回填 400 / 1000 股"), "{md}");
        let (title, md) = super::render_monitor_down(4);
        assert_eq!(title, "止损监听中断");
        assert!(md.contains("4 分钟"), "{md}");
    }

    #[test]
    fn reason_lookup() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        crate::trade::store::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO trade_signals (id, user_id, source, code, side, ref_price, reason, dedup_key, created_at)
             VALUES (3, 1, 'exit', '600000', 'sell', 9.1, '触发止损', 'k', '2026-09-16 10:00:00')",
            [],
        )
        .unwrap();
        assert_eq!(super::signal_reason(&c, 3).unwrap(), "触发止损");
        assert!(super::signal_reason(&c, 4).is_err());
    }
}

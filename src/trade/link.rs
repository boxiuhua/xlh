//! 工单签名链接(spec §8):推送里的 `/trade/t/{id}?sig=` 只允许查看与确认该工单,
//! 有效期至工单过期。签名绑定工单号、用户与过期时刻,任何一项被改都失效。

use crate::trade::model::{fmt_ts, Ticket};
use chrono::NaiveDateTime;
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 截取的十六进制长度:128 位,足以抵御在线猜测。
const SIG_HEX_LEN: usize = 32;

fn message(t: &Ticket) -> String {
    format!("t:{}:{}:{}", t.id, t.user_id, fmt_ts(t.expires_at))
}

fn mac(secret: &[u8], t: &Ticket) -> HmacSha256 {
    let mut m = HmacSha256::new_from_slice(secret).expect("HMAC 接受任意长度密钥");
    m.update(message(t).as_bytes());
    m
}

pub fn sign(secret: &[u8], t: &Ticket) -> String {
    let full = crate::trade::settings::hex(&mac(secret, t).finalize().into_bytes());
    full[..SIG_HEX_LEN].to_string()
}

pub fn verify(secret: &[u8], t: &Ticket, sig: &str, now: NaiveDateTime) -> bool {
    if now >= t.expires_at || sig.len() != SIG_HEX_LEN {
        return false;
    }
    let Some(bytes) = (0..sig.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(sig.get(i..i + 2)?, 16).ok())
        .collect::<Option<Vec<u8>>>()
    else {
        return false;
    };
    // 常量时间比较前缀,不给计时侧信道
    mac(secret, t).verify_truncated_left(&bytes).is_ok()
}

pub fn ticket_url(base: &str, secret: &[u8], t: &Ticket) -> String {
    format!(
        "{}/trade/t/{}?sig={}",
        base.trim_end_matches('/'),
        t.id,
        sign(secret, t)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Direction;
    use crate::trade::model::{Account, Ticket, TicketStatus};
    use chrono::{NaiveDate, NaiveDateTime};

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 23)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }
    fn ticket() -> Ticket {
        Ticket {
            id: 42,
            user_id: 7,
            signal_id: 1,
            account: Account::Real,
            code: "600000".into(),
            side: Direction::Buy,
            suggest_price: 10.0,
            qty: 100,
            filled_qty: 0,
            expires_at: at(10, 30),
            deviation_th: 0.015,
            status: TicketStatus::Pending,
            urgency: 0,
            created_at: at(9, 26),
            confirmed_at: None,
            ignore_reason: None,
        }
    }
    const SECRET: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn signature_verifies_until_expiry_and_binds_ticket_user_and_expiry() {
        let t = ticket();
        let sig = sign(SECRET, &t);
        assert_eq!(sig.len(), 32);
        assert!(verify(SECRET, &t, &sig, at(10, 0)));
        assert!(!verify(SECRET, &t, &sig, at(10, 30)), "到期即失效");
        assert!(!verify(
            b"another-secret-another-secret-xx",
            &t,
            &sig,
            at(10, 0)
        ));
        assert!(!verify(
            SECRET,
            &Ticket {
                id: 43,
                ..t.clone()
            },
            &sig,
            at(10, 0)
        ));
        assert!(!verify(
            SECRET,
            &Ticket {
                user_id: 8,
                ..t.clone()
            },
            &sig,
            at(10, 0)
        ));
        assert!(!verify(
            SECRET,
            &Ticket {
                expires_at: at(11, 0),
                ..t.clone()
            },
            &sig,
            at(10, 0)
        ));
        assert!(!verify(SECRET, &t, "", at(10, 0)));
        assert!(!verify(SECRET, &t, "zz", at(10, 0)));
    }

    #[test]
    fn url_joins_base_without_double_slash() {
        let t = ticket();
        let u = ticket_url("https://x.example.com/", SECRET, &t);
        assert!(u.starts_with("https://x.example.com/trade/t/42?sig="));
        assert!(u.ends_with(&sign(SECRET, &t)));
    }
}

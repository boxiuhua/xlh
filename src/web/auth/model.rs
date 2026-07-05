use chrono::{Duration, NaiveDate};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseStatus {
    Inactive,
    Active,
    Warning,
    Grace,
    Expired,
}

impl LicenseStatus {
    pub fn of(expires_at: Option<NaiveDate>, now: NaiveDate, warn_days: i64, grace_days: i64) -> Self {
        match expires_at {
            None => LicenseStatus::Inactive,
            Some(exp) => {
                if now <= exp - Duration::days(warn_days) {
                    LicenseStatus::Active
                } else if now <= exp {
                    LicenseStatus::Warning
                } else if now <= exp + Duration::days(grace_days) {
                    LicenseStatus::Grace
                } else {
                    LicenseStatus::Expired
                }
            }
        }
    }

    pub fn allows_access(self) -> bool {
        matches!(self, LicenseStatus::Active | LicenseStatus::Warning | LicenseStatus::Grace)
    }
}

/// 激活续期：从「now 与原到期日的较大者」叠加 days 天。
pub fn renew_expiry(current: Option<NaiveDate>, now: NaiveDate, days: i64) -> NaiveDate {
    let base = current.map(|c| c.max(now)).unwrap_or(now);
    base + Duration::days(days)
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub expires_at: Option<NaiveDate>,
    pub is_admin: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeRow {
    pub code: String,
    pub days: i64,
    pub used_by: Option<i64>,
    pub used_at: Option<String>,
    pub revoked: bool,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn d(s: &str) -> NaiveDate { s.parse().unwrap() }

    #[test]
    fn inactive_when_no_expiry() {
        assert_eq!(LicenseStatus::of(None, d("2026-07-05"), 7, 3), LicenseStatus::Inactive);
    }

    #[test]
    fn status_boundaries() {
        let exp = d("2026-07-10");
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-03"), 7, 3), LicenseStatus::Active);  // == exp-7
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-04"), 7, 3), LicenseStatus::Warning); // 进入临期
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-10"), 7, 3), LicenseStatus::Warning); // == exp
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-11"), 7, 3), LicenseStatus::Grace);   // exp+1
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-13"), 7, 3), LicenseStatus::Grace);   // == exp+3
        assert_eq!(LicenseStatus::of(Some(exp), d("2026-07-14"), 7, 3), LicenseStatus::Expired); // exp+4
    }

    #[test]
    fn allows_access_set() {
        assert!(LicenseStatus::Active.allows_access());
        assert!(LicenseStatus::Warning.allows_access());
        assert!(LicenseStatus::Grace.allows_access());
        assert!(!LicenseStatus::Inactive.allows_access());
        assert!(!LicenseStatus::Expired.allows_access());
    }

    #[test]
    fn renew_from_now_when_inactive_or_expired() {
        let now = d("2026-07-05");
        assert_eq!(renew_expiry(None, now, 30), d("2026-08-04"));
        assert_eq!(renew_expiry(Some(d("2026-06-01")), now, 30), d("2026-08-04")); // 过期→从今天
    }

    #[test]
    fn renew_stacks_when_still_valid() {
        let now = d("2026-07-05");
        assert_eq!(renew_expiry(Some(d("2026-08-01")), now, 30), d("2026-08-31")); // 未过期→叠加
    }
}

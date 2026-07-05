use std::path::{Path, PathBuf};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AuthCfg {
    #[serde(default = "default_db_path")]
    pub db_path: PathBuf,
    #[serde(default = "default_true")]
    pub open_registration: bool,
    #[serde(default = "default_warn")]
    pub warn_days: i64,
    #[serde(default = "default_grace")]
    pub grace_days: i64,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_days: i64,
}

fn default_db_path() -> PathBuf { PathBuf::from("data/xlh.db") }
fn default_true() -> bool { true }
fn default_warn() -> i64 { 7 }
fn default_grace() -> i64 { 3 }
fn default_session_ttl() -> i64 { 30 }

impl Default for AuthCfg {
    fn default() -> Self {
        AuthCfg {
            db_path: default_db_path(),
            open_registration: default_true(),
            warn_days: default_warn(),
            grace_days: default_grace(),
            session_ttl_days: default_session_ttl(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct AuthFile {
    #[serde(default)]
    auth: AuthCfg,
}

/// 从 config.toml 宽松读取 `[auth]` 段；文件不存在或段缺失时返回默认值。
pub fn load_auth(path: &Path) -> AuthCfg {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str::<AuthFile>(&s).ok())
        .map(|f| f.auth)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = load_auth(Path::new("does/not/exist.toml"));
        assert_eq!(cfg.warn_days, 7);
        assert_eq!(cfg.grace_days, 3);
        assert!(cfg.open_registration);
        assert_eq!(cfg.db_path, PathBuf::from("data/xlh.db"));
    }

    #[test]
    fn parses_partial_section() {
        let toml = "[auth]\nwarn_days = 14\n";
        let f: AuthFile = toml::from_str(toml).unwrap();
        assert_eq!(f.auth.warn_days, 14);
        assert_eq!(f.auth.grace_days, 3); // 未写字段回落默认
    }
}

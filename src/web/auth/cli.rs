use std::path::Path;
use anyhow::{anyhow, Result};

use super::store::{self, CodeFilter};
use super::{admin, config, password};

fn open_db(config_path: &Path) -> Result<rusqlite::Connection> {
    let cfg = config::load_auth(config_path);
    store::open(&cfg.db_path)
}

/// 创建首个/追加管理员；从环境变量 XLH_ADMIN_PASSWORD 读密码（避免交互 TTY 依赖）。
pub fn admin_create(config_path: &Path, username: &str) -> Result<()> {
    let pw = std::env::var("XLH_ADMIN_PASSWORD")
        .map_err(|_| anyhow!("请用环境变量 XLH_ADMIN_PASSWORD 提供管理员密码"))?;
    if pw.chars().count() < 6 { return Err(anyhow!("密码至少 6 位")); }
    let conn = open_db(config_path)?;
    let hash = password::hash(&pw)?;
    let id = store::create_user(&conn, username, &hash, true)
        .map_err(|_| anyhow!("创建失败：用户名 {username} 可能已存在"))?;
    println!("✓ 管理员已创建：{username} (id={id})");
    Ok(())
}

pub fn license_issue(config_path: &Path, days: i64, count: u32) -> Result<()> {
    let conn = open_db(config_path)?;
    for _ in 0..count {
        let code = admin::gen_code();
        store::issue_code(&conn, &code, days)?;
        println!("{code}  (+{days}天)");
    }
    Ok(())
}

pub fn license_list(config_path: &Path, filter: &str) -> Result<()> {
    let conn = open_db(config_path)?;
    let f = match filter { "used" => CodeFilter::Used, "all" => CodeFilter::All, _ => CodeFilter::Unused };
    for c in store::list_codes(&conn, f)? {
        let st = if c.revoked { "作废" } else if c.used_by.is_some() { "已用" } else { "未用" };
        println!("{}  {:>4}天  {}  用户{}", c.code, c.days, st, c.used_by.map(|u| u.to_string()).unwrap_or_else(|| "-".into()));
    }
    Ok(())
}

pub fn user_list(config_path: &Path) -> Result<()> {
    let conn = open_db(config_path)?;
    for u in store::list_users(&conn)? {
        println!("{:>3}  {:<20} 到期 {}  {}{}",
            u.id, u.username,
            u.expires_at.map(|e| e.to_string()).unwrap_or_else(|| "未激活".into()),
            if u.is_admin { "[管理员]" } else { "" },
            if u.disabled { "[封禁]" } else { "" });
    }
    Ok(())
}

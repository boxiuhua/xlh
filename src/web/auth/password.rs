use anyhow::{anyhow, Result};
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// 生成 argon2 PHC 串（含随机盐）。
pub fn hash(plain: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow!("hash 失败: {e}"))?
        .to_string();
    Ok(phc)
}

/// 校验明文口令与 PHC 串是否匹配；任何解析/校验错误都视为不匹配。
pub fn verify(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default().verify_password(plain.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrip() {
        let h = hash("s3cret").unwrap();
        assert!(verify("s3cret", &h));
        assert!(!verify("wrong", &h));
    }

    #[test]
    fn salt_makes_hashes_differ() {
        assert_ne!(hash("same").unwrap(), hash("same").unwrap());
    }

    #[test]
    fn garbage_phc_is_not_verified() {
        assert!(!verify("x", "not-a-phc-string"));
    }
}

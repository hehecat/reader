//! SHA-256 工具（OPDS 独立账号密码存储）
//!
//! 存储格式：`{salt}${sha256_hex(salt || password)}`（salt 为 16 字节随机 hex）。
//! 与系统用户（legacy 兼容的 md5 双哈希）分离：OPDS 账号独立于 users 表，
//! 配置后仅用于 OPDS Basic 认证。

use rand::RngCore;

/// sha256 hex（小写）
pub fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 随机盐（16 字节 → 32 位 hex）
pub fn random_salt() -> String {
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// 密码哈希：sha256(salt || password)
pub fn hash_password(password: &str, salt: &str) -> String {
    sha256_hex(&format!("{salt}{password}"))
}

/// 生成存储串 `{salt}${hash}`
pub fn store_password(password: &str) -> String {
    let salt = random_salt();
    format!("{salt}${}", hash_password(password, &salt))
}

/// 校验存储串 `{salt}${hash}`（格式不符返回 false）
pub fn verify_password(password: &str, stored: &str) -> bool {
    match stored.split_once('$') {
        Some((salt, hash)) => {
            !salt.is_empty()
                && crate::util::constant_time::ct_eq(&hash_password(password, salt), hash)
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_known_vector() {
        // sha256("abc") 标准向量
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_store_and_verify_roundtrip() {
        let stored = store_password("opds-secret");
        assert!(stored.contains('$'));
        assert!(verify_password("opds-secret", &stored));
        assert!(!verify_password("wrong", &stored));
        assert!(!verify_password("opds-secret", "no-dollar-format"));
        assert!(!verify_password("opds-secret", "$deadbeef"));
    }

    #[test]
    fn test_salt_randomness() {
        let a = random_salt();
        let b = random_salt();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
    }
}

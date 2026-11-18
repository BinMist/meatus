#![forbid(unsafe_code)]
//! 令牌派生与 HMAC 挑战应答。
//!
//! 服务端只保存 `sha256(token)`，不保存明文 token；客户端用同一个哈希作为
//! HMAC-SHA256 的密钥，对每次连接的新 nonce 计算证明。这样既保留哈希存储，
//! 又绑定了连接随机数。

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// 由 token 派生 HMAC 密钥：`sha256(token)`。
#[must_use]
pub fn token_key(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// 客户端侧：用 token 对 nonce 计算证明。
#[must_use]
pub fn proof(token: &str, nonce: &[u8]) -> Vec<u8> {
    proof_with_key(&token_key(token), nonce)
}

/// 用已派生的密钥计算证明。
#[must_use]
pub fn proof_with_key(key: &[u8], nonce: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(nonce);
    mac.finalize().into_bytes().to_vec()
}

/// 常量时间校验证明。
#[must_use]
pub fn verify_proof_with_key(key: &[u8], nonce: &[u8], proof: &[u8]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(nonce);
    mac.verify_slice(proof).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_key_matches_known_hash() {
        assert_eq!(
            hex::encode(token_key("secret")),
            "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
        );
    }

    #[test]
    fn proof_roundtrips() {
        let nonce = [7u8; 32];
        let value = proof("secret", &nonce);
        assert!(verify_proof_with_key(&token_key("secret"), &nonce, &value));
    }

    #[test]
    fn proof_is_bound_to_nonce_and_key() {
        let nonce = [1u8; 32];
        let other_nonce = [2u8; 32];
        let value = proof("secret", &nonce);
        assert!(!verify_proof_with_key(
            &token_key("secret"),
            &other_nonce,
            &value
        ));
        assert!(!verify_proof_with_key(&token_key("other"), &nonce, &value));
    }
}

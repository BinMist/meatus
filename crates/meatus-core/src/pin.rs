//! SPKI（Subject Public Key Info）证书公钥 pin 解析。
//!
//! 接受 `sha256:<64 hex>` 或裸 64 位 hex；只支持 SHA-256，避免配置歧义。

/// 解析 SPKI SHA-256 pin，返回 32 字节摘要。
pub fn parse_spki_pin(raw: &str) -> Result<[u8; 32], String> {
    let value = raw.trim();
    let hex_part = value.strip_prefix("sha256:").unwrap_or(value);
    if hex_part.len() != 64 {
        return Err("SPKI pin must be 64 hex characters (sha256: prefix optional)".to_string());
    }
    let bytes = hex::decode(hex_part).map_err(|err| format!("SPKI pin is not hex: {err}"))?;
    bytes
        .try_into()
        .map_err(|_| "SPKI pin must decode to 32 bytes".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_prefixed_and_bare_hex() {
        let hex = "aa".repeat(32);
        assert!(parse_spki_pin(&hex).is_ok());
        assert!(parse_spki_pin(&format!("sha256:{hex}")).is_ok());
    }

    #[test]
    fn rejects_bad_length_and_non_hex() {
        assert!(parse_spki_pin("abc").is_err());
        assert!(parse_spki_pin(&"zz".repeat(32)).is_err());
    }
}

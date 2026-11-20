//! 可观测性 ID：生成与校验。
//!
//! `trace_id` 标识一次控制会话，`connection_id` 标识一条数据流；
//! 两者都限制为 ASCII 可见字符，避免日志注入与超长字段。

use std::fmt;

pub const MAX_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    Empty,
    TooLong,
    InvalidChar,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "id is empty"),
            Self::TooLong => write!(f, "id exceeds {MAX_ID_LEN} bytes"),
            Self::InvalidChar => write!(f, "id contains characters outside [A-Za-z0-9._:-]"),
        }
    }
}

impl std::error::Error for IdError {}

/// 返回 16 字节随机数的 32 位小写十六进制字符串。
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // 随机源不可用时退回时间戳，仍满足格式与唯一性下限。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        return format!("{nanos:032x}");
    }
    hex::encode(bytes)
}

/// 校验 ID：非空、<= 64 字节、仅 `[A-Za-z0-9._:-]`。
pub fn validate_id(value: &str) -> Result<(), IdError> {
    if value.is_empty() {
        return Err(IdError::Empty);
    }
    if value.len() > MAX_ID_LEN {
        return Err(IdError::TooLong);
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    {
        return Err(IdError::InvalidChar);
    }
    Ok(())
}

/// 缺失或非法时生成新 ID；重复由调用方注册表负责。
pub fn ensure_id(value: &str) -> String {
    if validate_id(value).is_ok() {
        value.to_string()
    } else {
        new_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_missing_duplicate_and_overlong() {
        assert_eq!(validate_id(""), Err(IdError::Empty));
        assert!(validate_id("abc-123_x:y.z").is_ok());
        assert_eq!(
            validate_id(&"a".repeat(MAX_ID_LEN + 1)),
            Err(IdError::TooLong)
        );
        assert_eq!(validate_id("bad\nvalue"), Err(IdError::InvalidChar));
    }

    #[test]
    fn generated_ids_are_unique_and_valid() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..10_000 {
            let id = new_id();
            assert!(validate_id(&id).is_ok());
            assert!(seen.insert(id), "duplicate generated id");
        }
    }
}

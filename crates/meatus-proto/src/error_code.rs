//! 协议错误码，与开发规划 3.5 节一致。

use crate::v1::{ControlMsg, Error as WireError, control_msg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ErrorCode {
    AuthFailed = 1001,
    UnsupportedVersion = 1002,
    RateLimited = 1003,
    PortInUse = 2001,
    PortPoolExhausted = 2002,
    DomainConflict = 2003,
    LocalConnectFailed = 3001,
    StreamLimit = 3002,
    Internal = 4001,
}

impl ErrorCode {
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    #[must_use]
    pub const fn is_fatal(self) -> bool {
        matches!(
            self,
            Self::AuthFailed | Self::UnsupportedVersion | Self::RateLimited
        )
    }

    #[must_use]
    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            1001 => Some(Self::AuthFailed),
            1002 => Some(Self::UnsupportedVersion),
            1003 => Some(Self::RateLimited),
            2001 => Some(Self::PortInUse),
            2002 => Some(Self::PortPoolExhausted),
            2003 => Some(Self::DomainConflict),
            3001 => Some(Self::LocalConnectFailed),
            3002 => Some(Self::StreamLimit),
            4001 => Some(Self::Internal),
            _ => None,
        }
    }

    /// 生成一条可直接发送的 Error 控制帧。
    #[must_use]
    pub fn to_wire(self, message: impl Into<String>) -> ControlMsg {
        ControlMsg {
            kind: Some(control_msg::Kind::Error(WireError {
                code: self.as_u32(),
                message: message.into(),
                fatal: self.is_fatal(),
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_classification() {
        assert!(ErrorCode::AuthFailed.is_fatal());
        assert!(!ErrorCode::PortInUse.is_fatal());
    }

    #[test]
    fn roundtrip_codes() {
        for code in [
            ErrorCode::AuthFailed,
            ErrorCode::UnsupportedVersion,
            ErrorCode::RateLimited,
            ErrorCode::PortInUse,
            ErrorCode::PortPoolExhausted,
            ErrorCode::DomainConflict,
            ErrorCode::LocalConnectFailed,
            ErrorCode::StreamLimit,
            ErrorCode::Internal,
        ] {
            assert_eq!(ErrorCode::from_u32(code.as_u32()), Some(code));
        }
    }

    #[test]
    fn wire_error_carries_fatal_flag() {
        let msg = ErrorCode::AuthFailed.to_wire("bad token");
        match msg.kind {
            Some(control_msg::Kind::Error(e)) => {
                assert_eq!(e.code, 1001);
                assert!(e.fatal);
                assert_eq!(e.message, "bad token");
            }
            other => panic!("unexpected kind: {other:?}"),
        }
    }
}

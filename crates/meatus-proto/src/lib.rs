#![forbid(unsafe_code)]
//! MEATUS 线协议：protobuf 消息定义、长度前缀帧编解码、版本与能力位协商、错误码。
//!
//! 生成代码来自 `proto/meatus.proto`，不要手改；改协议先改 proto 并重新构建。

pub mod error_code;
pub mod frame;
pub mod negotiation;

#[cfg(test)]
mod wire_compat;

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/meatus.v1.rs"));
}

pub use negotiation::{
    CAP_PROBE, CAPABILITIES, DEFAULT_CLIENT_CAPS, MIN_PROTOCOL_VERSION, Negotiated,
    PROTOCOL_VERSION, UnsupportedProtocolVersion, default_client_caps, negotiate, negotiate_with,
};

/// 控制帧上限，超限直接判错（防止内存被打爆）。
pub const MAX_FRAME_LEN: usize = 64 * 1024;

pub use error_code::ErrorCode;
pub use frame::{FrameError, decode, decode_msg, encode, encode_msg};

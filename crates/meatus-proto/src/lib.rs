#![forbid(unsafe_code)]
//! MEATUS/1 线协议：protobuf 消息定义、长度前缀帧编解码、错误码。
//!
//! 生成代码来自 `proto/meatus.proto`，不要手改；改协议先改 proto 并重新构建。

pub mod error_code;
pub mod frame;

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/meatus.v1.rs"));
}

/// 当前协议大版本，与 `Hello.protocol_version` 对应。
pub const PROTOCOL_VERSION: u32 = 1;

/// 控制帧上限，超限直接判错（防止内存被打爆）。
pub const MAX_FRAME_LEN: usize = 64 * 1024;

pub use error_code::ErrorCode;
pub use frame::{FrameError, decode, decode_msg, encode, encode_msg};

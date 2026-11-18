//! 长度前缀帧编解码：`4 字节大端长度 + protobuf 载荷`。

use bytes::{BufMut, Bytes, BytesMut};
use prost::Message;

use crate::MAX_FRAME_LEN;
use crate::v1::ControlMsg;

const PREFIX_LEN: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame is too large: {0} bytes (limit {MAX_FRAME_LEN})")]
    TooLarge(usize),
    #[error("frame is empty")]
    Empty,
    #[error("frame is truncated")]
    Truncated,
    #[error("frame has trailing bytes")]
    Trailing,
    #[error("protobuf decode failed: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("protobuf encode failed: {0}")]
    Encode(#[from] prost::EncodeError),
}

/// 把任意 protobuf 消息编码成一帧。
pub fn encode_msg<M: Message>(msg: &M) -> Result<Bytes, FrameError> {
    let len = msg.encoded_len();
    check_len(len)?;
    let mut buf = BytesMut::with_capacity(PREFIX_LEN + len);
    buf.put_u32(len as u32);
    msg.encode(&mut buf)?;
    Ok(buf.freeze())
}

/// 把控制消息编码成一帧。
pub fn encode(msg: &ControlMsg) -> Result<Bytes, FrameError> {
    encode_msg(msg)
}

/// 解码恰好一帧为任意 protobuf 消息；带多余字节视为帧边界错误。
pub fn decode_msg<M: Message + Default>(frame: &[u8]) -> Result<M, FrameError> {
    if frame.len() < PREFIX_LEN {
        return Err(FrameError::Truncated);
    }
    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    check_len(len)?;
    if frame.len() < PREFIX_LEN + len {
        return Err(FrameError::Truncated);
    }
    if frame.len() > PREFIX_LEN + len {
        return Err(FrameError::Trailing);
    }
    M::decode(&frame[PREFIX_LEN..]).map_err(FrameError::Decode)
}

/// 解码恰好一帧控制消息。
pub fn decode(frame: &[u8]) -> Result<ControlMsg, FrameError> {
    decode_msg(frame)
}

fn check_len(len: usize) -> Result<(), FrameError> {
    if len == 0 {
        return Err(FrameError::Empty);
    }
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{ControlMsg, Ping, control_msg};

    fn ping(seq: u64) -> ControlMsg {
        ControlMsg {
            kind: Some(control_msg::Kind::Ping(Ping { seq, ts_ms: 1 })),
        }
    }

    #[test]
    fn roundtrip_control_message() {
        let bytes = encode(&ping(42)).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        match decoded.kind {
            Some(control_msg::Kind::Ping(p)) => assert_eq!(p.seq, 42),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[test]
    fn reject_oversize_frame() {
        let big = ControlMsg {
            kind: Some(control_msg::Kind::Hello(crate::v1::Hello {
                protocol_version: 1,
                caps: vec!["x".repeat(MAX_FRAME_LEN + 1)],
                ..Default::default()
            })),
        };
        assert!(matches!(encode(&big), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn reject_declared_oversize_frame() {
        let mut frame = BytesMut::new();
        frame.put_u32((MAX_FRAME_LEN + 1) as u32);
        frame.extend_from_slice(&[0u8; 8]);
        assert!(matches!(decode(&frame), Err(FrameError::TooLarge(_))));
    }

    #[test]
    fn reject_empty_and_truncated() {
        assert!(matches!(decode(&[0, 0, 0, 0]), Err(FrameError::Empty)));
        assert!(matches!(decode(&[0, 0, 0]), Err(FrameError::Truncated)));
        assert!(matches!(
            decode(&[0, 0, 0, 5, 0x08]),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn reject_trailing_bytes() {
        let bytes = encode(&ping(1)).expect("encode");
        let mut with_extra = BytesMut::from(bytes.as_ref());
        with_extra.extend_from_slice(&[0xff]);
        assert!(matches!(decode(&with_extra), Err(FrameError::Trailing)));
    }

    #[test]
    fn ignore_unknown_fields() {
        let mut payload = BytesMut::new();
        ping(7).encode(&mut payload).expect("encode payload");
        // 未知字段 99，wire type 0，值 1
        payload.extend_from_slice(&[0x98, 0x06, 0x01]);

        let mut frame = BytesMut::new();
        frame.put_u32(payload.len() as u32);
        frame.extend_from_slice(&payload);

        assert!(decode(&frame).is_ok());
    }
}

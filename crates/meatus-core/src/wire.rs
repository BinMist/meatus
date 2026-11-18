#![forbid(unsafe_code)]
//! 控制流上的异步帧读写：长度前缀 + protobuf。

use meatus_proto::{FrameError, MAX_FRAME_LEN};
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("peer closed the connection")]
    Closed,
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
}

pub async fn write_frame<W, M>(writer: &mut W, msg: &M) -> Result<(), WireError>
where
    W: AsyncWrite + Unpin,
    M: Message,
{
    let bytes = meatus_proto::encode_msg(msg)?;
    writer.write_all(&bytes).await?;
    Ok(())
}

pub async fn read_frame<R, M>(reader: &mut R) -> Result<M, WireError>
where
    R: AsyncRead + Unpin,
    M: Message + Default,
{
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(WireError::Closed);
        }
        Err(err) => return Err(WireError::Io(err)),
    }

    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 {
        return Err(FrameError::Empty.into());
    }
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len).into());
    }

    let mut payload = vec![0u8; len];
    match reader.read_exact(&mut payload).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(WireError::Closed);
        }
        Err(err) => return Err(WireError::Io(err)),
    }

    M::decode(payload.as_slice()).map_err(|err| FrameError::Decode(err).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use meatus_proto::v1::{ControlMsg, Ping, control_msg};

    fn ping(seq: u64) -> ControlMsg {
        ControlMsg {
            kind: Some(control_msg::Kind::Ping(Ping { seq, ts_ms: 1 })),
        }
    }

    #[tokio::test]
    async fn roundtrip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(128);
        write_frame(&mut a, &ping(9)).await.expect("write");
        let msg: ControlMsg = read_frame(&mut b).await.expect("read");
        match msg.kind {
            Some(control_msg::Kind::Ping(p)) => assert_eq!(p.seq, 9),
            other => panic!("unexpected kind: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_maps_to_closed() {
        let (a, mut b) = tokio::io::duplex(16);
        drop(a);
        let result: Result<ControlMsg, _> = read_frame(&mut b).await;
        assert!(matches!(result, Err(WireError::Closed)));
    }
}

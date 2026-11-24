//! protobuf 加法兼容测试：v2 `Welcome` 必须能被 v1 解码器读取，v1 `Welcome`
//! 也必须能被 v2 解码器读取且新字段取默认值。

use prost::Message;

use crate::frame::{decode_msg, encode_msg};
use crate::v1::Welcome;

/// V1 时代 `Welcome` 的字段视图，只声明 tag 1..3。
#[derive(Clone, PartialEq, Message)]
struct LegacyWelcome {
    #[prost(string, tag = "1")]
    run_id: String,
    #[prost(uint32, tag = "2")]
    heartbeat_secs: u32,
    #[prost(int64, tag = "3")]
    server_time_ms: i64,
}

fn v2_welcome() -> Welcome {
    Welcome {
        run_id: "run-v2".to_string(),
        heartbeat_secs: 10,
        server_time_ms: 1_760_000_000_000,
        protocol_version: 2,
        caps: vec!["tcp".to_string(), "probe".to_string()],
        trace_id: "trace-v2".to_string(),
        relay_group: "prod".to_string(),
        relay_id: "relay-a".to_string(),
        relay_role: "primary".to_string(),
        group_epoch: 3,
    }
}

#[test]
fn v1_decoder_ignores_v2_welcome_fields() {
    let encoded = v2_welcome().encode_to_vec();
    let legacy =
        LegacyWelcome::decode(encoded.as_slice()).expect("v1 decoder must accept v2 Welcome");

    assert_eq!(legacy.run_id, "run-v2");
    assert_eq!(legacy.heartbeat_secs, 10);
    assert_eq!(legacy.server_time_ms, 1_760_000_000_000);
}

#[test]
fn v2_decoder_accepts_v1_welcome_and_defaults_new_fields() {
    let legacy = LegacyWelcome {
        run_id: "run-v1".to_string(),
        heartbeat_secs: 10,
        server_time_ms: 42,
    };
    let encoded = legacy.encode_to_vec();
    let decoded = Welcome::decode(encoded.as_slice()).expect("v2 decoder must accept v1 Welcome");

    assert_eq!(decoded.run_id, "run-v1");
    assert_eq!(decoded.heartbeat_secs, 10);
    assert_eq!(decoded.server_time_ms, 42);
    assert_eq!(decoded.protocol_version, 0, "absent v2 field defaults to 0");
    assert!(decoded.caps.is_empty());
    assert!(
        decoded.trace_id.is_empty(),
        "absent trace_id defaults to empty"
    );
    assert!(decoded.relay_group.is_empty());
    assert!(decoded.relay_id.is_empty());
    assert!(decoded.relay_role.is_empty());
    assert_eq!(decoded.group_epoch, 0);
}

#[test]
fn welcome_frame_roundtrip_keeps_v2_fields() {
    let welcome = v2_welcome();
    let frame = encode_msg(&welcome).expect("encode Welcome frame");
    let decoded: Welcome = decode_msg(&frame).expect("decode Welcome frame");

    assert_eq!(decoded, welcome);
}

#[test]
fn stream_header_optional_ids_roundtrip() {
    use crate::v1::{StreamHeader, StreamKind};

    let missing = StreamHeader {
        proxy_name: "echo".to_string(),
        kind: StreamKind::UserConn as i32,
        src_addr: "127.0.0.1:1".to_string(),
        dst_hint: String::new(),
        created_ms: 1,
        trace_id: String::new(),
        udp_session_id: 0,
        connection_id: String::new(),
    };
    let decoded: StreamHeader =
        decode_msg(&encode_msg(&missing).expect("encode header")).expect("decode header");
    assert!(decoded.trace_id.is_empty());
    assert!(decoded.connection_id.is_empty());

    let long = StreamHeader {
        trace_id: "a".repeat(64 + 1),
        connection_id: "b".repeat(64 + 1),
        ..missing
    };
    let decoded: StreamHeader =
        decode_msg(&encode_msg(&long).expect("encode header")).expect("decode header");
    assert_eq!(decoded.trace_id.len(), 64 + 1);
    assert!(decoded.trace_id.len() > 64);
}

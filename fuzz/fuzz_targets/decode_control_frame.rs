#![no_main]

use libfuzzer_sys::fuzz_target;

// 线协议帧解码必须是 panic-free：任意 4B 长度前缀 + 任意 protobuf 字节都只能返回 Ok/Err。
fuzz_target!(|data: &[u8]| {
    let _ = meatus_proto::decode(data);
});

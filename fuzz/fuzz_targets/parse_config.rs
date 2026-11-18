#![no_main]

use libfuzzer_sys::fuzz_target;

// TOML 配置解析 / 校验必须 panic-free；非法输入统一走 ConfigError。
fuzz_target!(|data: &[u8]| {
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let _ = meatus_core::parse_server_config(raw);
    let _ = meatus_core::parse_client_config(raw);
});

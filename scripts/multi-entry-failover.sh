#!/usr/bin/env bash
# D2-04 多入口故障注入：启动真实服务端，验证首个入口不可达时切换、
# 当前入口停止后重新注册，以及 QUIC 控制面重启后的重连。
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

echo "== D2-04 multi-entry fault injection =="
cargo test -p meatus-server --test tunnel --offline client_fails_over_to_second_server_addr -- --exact
cargo test -p meatus-server --test tunnel --offline client_switches_when_active_entry_stops -- --exact
cargo test -p meatus-server --test tunnel --offline quic_client_reconnects_after_server_restart -- --exact

echo "PASS multi-entry failover: dead entry skipped, active entry switch re-registers proxies, restart reconnect works"

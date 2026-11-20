#!/usr/bin/env bash
# D3-03 IPv6 回归：Host/SNI 解析、IPv6-only 隧道、双栈 v4/v6 回环。
set -euo pipefail
cd "$(cd "$(dirname "$0")/.." && pwd)"

echo "== IPv6 regression =="
cargo test -p meatus-core --offline config::tests::host_port_brackets_ipv6_literals -- --exact
cargo test -p meatus-server --lib --offline http::tests -- --nocapture
cargo test -p meatus-server --lib --offline routes::tests::normalizes_domains_and_hosts -- --exact
cargo test -p meatus-server --test tunnel --offline ipv6_only_control_and_proxy_roundtrip -- --exact
cargo test -p meatus-server --test tunnel --offline dual_stack_bind_accepts_ipv4_and_ipv6 -- --exact
echo "PASS IPv6: host/SNI parsing, IPv6-only tunnel, dual-stack v4+v6"

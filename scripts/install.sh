#!/usr/bin/env bash
# Meatus V1 Linux 安装脚本：二进制 + 自签证书 + token + systemd。
# 只支持 Linux；不会覆盖已存在的 server.toml / client.toml。
#
# 用法：
#   sudo ./scripts/install.sh --binary target/release/meatus
#   sudo ./scripts/install.sh --binary ./meatus --domain tunnel.example.com --server-addr tunnel.example.com:7000
#   sudo ./scripts/install.sh --binary ./meatus --with-client
set -euo pipefail

cd "$(dirname "$0")/.."

BIN=""
DOMAIN="${MEATUS_DOMAIN:-localhost}"
SERVER_ADDR="${MEATUS_SERVER_ADDR:-127.0.0.1:7000}"
WITH_CLIENT=0
START=1
PREFIX="/usr/local/bin"
CONF_DIR="/etc/meatus"
STATE_DIR="/var/lib/meatus"
SERVICE_DIR="/etc/systemd/system"

usage() {
    sed -n '2,12p' "$0"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary) BIN="${2:?}"; shift 2 ;;
        --domain) DOMAIN="${2:?}"; shift 2 ;;
        --server-addr) SERVER_ADDR="${2:?}"; shift 2 ;;
        --with-client) WITH_CLIENT=1; shift ;;
        --no-start) START=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage; exit 2 ;;
    esac
done

[[ "$(uname -s)" == "Linux" ]] || { echo "install.sh 只支持 Linux（当前 $(uname -s)）" >&2; exit 1; }
[[ "$(id -u)" -eq 0 ]] || { echo "请用 root 运行（sudo）" >&2; exit 1; }
[[ -n "$BIN" && -x "$BIN" ]] || { echo "--binary 必须是可执行的 meatus 文件" >&2; exit 2; }
command -v systemctl >/dev/null || { echo "未找到 systemd（systemctl）" >&2; exit 1; }

echo "== meatus V1 install =="
echo "binary: $BIN"
echo "domain: $DOMAIN"
echo "server: $SERVER_ADDR"

install -d -m 0755 "$CONF_DIR" "$STATE_DIR" "$STATE_DIR/certs" "$STATE_DIR/acme"
if ! id meatus >/dev/null 2>&1; then
    useradd --system --home "$STATE_DIR" --shell /usr/sbin/nologin meatus
fi
chown -R meatus:meatus "$STATE_DIR"
chmod 0750 "$STATE_DIR"

install -m 0755 "$BIN" "$PREFIX/meatus"
echo "installed $PREFIX/meatus"

# YAML 示例分发到文档目录；实际 YAML 配置需手工复制并改路径。
DOC_DIR="/usr/share/doc/meatus/config"
install -d -m 0755 "$DOC_DIR"
install -m 0644 config/server.example.yaml config/client.example.yaml "$DOC_DIR/"

# 许可证、第三方声明与发布包 SBOM 一并安装，便于离线审计。
DOC_ROOT="/usr/share/doc/meatus"
install -d -m 0755 "$DOC_ROOT"
install -m 0644 LICENSE THIRD_PARTY_NOTICES.md "$DOC_ROOT/"
for f in THIRD_PARTY_LICENSES.md THIRD_PARTY_LICENSE_TEXTS.txt; do
    if [[ -f "$f" ]]; then
        install -m 0644 "$f" "$DOC_ROOT/"
    else
        install -m 0644 "docs/$f" "$DOC_ROOT/"
    fi
done
[[ -f SBOM.spdx.json ]] && install -m 0644 SBOM.spdx.json "$DOC_ROOT/"

SERVER_CONF="$CONF_DIR/server.toml"
if [[ -f "$SERVER_CONF" ]]; then
    echo "keep existing $SERVER_CONF"
else
    install -m 0600 config/server.example.toml "$SERVER_CONF"
    sed -i "s|tls_cert = \"certs/server.crt\"|tls_cert = \"$STATE_DIR/certs/server.crt\"|" "$SERVER_CONF"
    sed -i "s|tls_key = \"certs/server.key\"|tls_key = \"$STATE_DIR/certs/server.key\"|" "$SERVER_CONF"

    if [[ ! -f "$STATE_DIR/certs/server.crt" ]]; then
        "$PREFIX/meatus" gen-cert --domain "$DOMAIN" --domain 127.0.0.1 --out "$STATE_DIR/certs"
    fi
    chown -R meatus:meatus "$STATE_DIR/certs"
    chmod 0600 "$STATE_DIR/certs/server.key"

    TOKEN_OUT="$("$PREFIX/meatus" gen-token)"
    CLIENT_TOKEN="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "token" { print $2 }')"
    TOKEN_HASH="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "sha256" { print $2 }')"
    [[ -n "$CLIENT_TOKEN" && -n "$TOKEN_HASH" ]] || { echo "gen-token 输出无法解析" >&2; exit 1; }

    sed -i "s|0000000000000000000000000000000000000000000000000000000000000000|$TOKEN_HASH|" "$SERVER_CONF"
    printf '%s\n' "$CLIENT_TOKEN" > "$CONF_DIR/client.token"
    chmod 0600 "$CONF_DIR/client.token"

    ADMIN_TOKEN="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    printf '%s\n' "$ADMIN_TOKEN" > "$CONF_DIR/admin.token"
    chmod 0600 "$CONF_DIR/admin.token"
    sed -i "s|# admin_token = \"REPLACE_WITH_ADMIN_TOKEN\"|admin_token = \"$ADMIN_TOKEN\"|" "$SERVER_CONF"
    echo "generated self-signed certificate and tokens"
fi

# 服务以 meatus 用户运行：配置与 token 必须可被该用户读取，且仅该用户可读。
chown meatus:meatus "$SERVER_CONF" "$CONF_DIR/client.token" "$CONF_DIR/admin.token" 2>/dev/null || true
chmod 0600 "$SERVER_CONF" "$CONF_DIR/client.token" "$CONF_DIR/admin.token" 2>/dev/null || true

install -m 0644 deploy/systemd/meatus-server.service "$SERVICE_DIR/meatus-server.service"
install -m 0644 deploy/systemd/meatus-client.service "$SERVICE_DIR/meatus-client.service"

CLIENT_CONF="$CONF_DIR/client.toml"
if [[ ! -f "$CLIENT_CONF" ]]; then
    install -m 0600 config/client.example.toml "$CLIENT_CONF"
    sed -i "s|server_addr = \"tunnel.example.com:7000\"|server_addr = \"$SERVER_ADDR\"|" "$CLIENT_CONF"
    sed -i "s|server_name = \"tunnel.example.com\"|server_name = \"$DOMAIN\"|" "$CLIENT_CONF"
    sed -i "s|\${MEATUS_CLIENT_TOKEN}|$(cat "$CONF_DIR/client.token")|" "$CLIENT_CONF"
    # 把服务端自签证书复制成客户端 CA；远程客户端需要单独拷贝这个文件。
    install -m 0644 "$STATE_DIR/certs/server.crt" "$CONF_DIR/server.crt"
    sed -i "s|# server_ca_cert = \"certs/server.crt\"|server_ca_cert = \"$CONF_DIR/server.crt\"|" "$CLIENT_CONF"
    chown meatus:meatus "$CLIENT_CONF"
fi

systemctl daemon-reload
systemctl enable meatus-server >/dev/null
if [[ "$START" -eq 1 ]]; then
    systemctl restart meatus-server
fi
if [[ "$WITH_CLIENT" -eq 1 ]]; then
    systemctl enable meatus-client >/dev/null
    if [[ "$START" -eq 1 ]]; then
        systemctl restart meatus-client
    fi
fi

echo
echo "server: systemctl status meatus-server"
echo "config: $SERVER_CONF"
echo "admin token: $CONF_DIR/admin.token (面板默认 127.0.0.1:7400，请用 SSH 隧道访问)"
echo "client token: $CONF_DIR/client.token"
echo "client config: $CLIENT_CONF"
[[ "$START" -eq 1 ]] || echo "已安装但未启动（--no-start）"
echo "完成。"

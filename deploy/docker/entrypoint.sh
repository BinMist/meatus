#!/bin/sh
# V1 容器入口：首次启动生成自签证书、admin token、client token，并写入 /data/server.toml。
# 已存在 /data/server.toml 时完全不改动，直接执行传入的子命令。
set -eu

DATA_DIR="${MEATUS_DATA_DIR:-/data}"
CONFIG="${MEATUS_CONFIG:-$DATA_DIR/server.toml}"
COMMAND="${1:-server}"

# 容器默认以 root 启动：先把 /data 归一到 meatus 用户，再降权执行。
# bind mount 由宿主机创建时通常是 root/其它 UID，不修正会导致首启写配置失败。
if [ "$(id -u)" = "0" ]; then
    mkdir -p "$DATA_DIR/certs" "$DATA_DIR/acme" 2>/dev/null || true
    chown -R meatus:meatus "$DATA_DIR" 2>/dev/null || true
    exec su-exec meatus "$0" "$@"
fi

if [ "$COMMAND" = "server" ] && [ ! -f "$CONFIG" ]; then
    DOMAIN="${MEATUS_DOMAIN:-localhost}"
    mkdir -p "$DATA_DIR/certs" "$DATA_DIR/acme"
    cp /etc/meatus/server.example.toml "$CONFIG"
    chmod 0600 "$CONFIG"

    if [ ! -f "$DATA_DIR/certs/server.crt" ]; then
        meatus gen-cert --domain "$DOMAIN" --domain 127.0.0.1 --out "$DATA_DIR/certs"
    fi

    # admin token：64 位 hex，写入 0600 文件；面板默认只允许本机访问，
    # 容器内 127.0.0.1 无法从宿主机访问，因此改为 0.0.0.0 并要求 token。
    ADMIN_TOKEN="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    printf '%s\n' "$ADMIN_TOKEN" > "$DATA_DIR/admin.token"
    chmod 0600 "$DATA_DIR/admin.token"
    sed -i 's|bind = "127.0.0.1:7400"|bind = "0.0.0.0:7400"|' "$CONFIG"
    sed -i "s|# admin_token = \"REPLACE_WITH_ADMIN_TOKEN\"|admin_token = \"$ADMIN_TOKEN\"|" "$CONFIG"

    # client token：服务端只保存 sha256，明文单独保存供客户端使用。
    TOKEN_OUT="$(meatus gen-token)"
    CLIENT_TOKEN="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "token" { print $2 }')"
    TOKEN_HASH="$(printf '%s\n' "$TOKEN_OUT" | awk '$1 == "sha256" { print $2 }')"
    if [ -z "$CLIENT_TOKEN" ] || [ -z "$TOKEN_HASH" ]; then
        echo "meatus: gen-token 输出无法解析" >&2
        exit 1
    fi
    printf '%s\n' "$CLIENT_TOKEN" > "$DATA_DIR/client.token"
    chmod 0600 "$DATA_DIR/client.token"
    sed -i "s|0000000000000000000000000000000000000000000000000000000000000000|$TOKEN_HASH|" "$CONFIG"

    echo "meatus: 首次启动生成 $CONFIG"
    echo "meatus: admin token  -> $DATA_DIR/admin.token"
    echo "meatus: client token -> $DATA_DIR/client.token"
    echo "meatus: 客户端 server_ca_cert 需指向 $DATA_DIR/certs/server.crt"
fi

exec meatus "$@"

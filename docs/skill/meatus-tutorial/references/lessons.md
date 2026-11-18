# Meatus 教程课程脚本

## 1. 先认识四个值

```text
本地服务 127.0.0.1:22
        │
        │  client 主动连 server
        ▼
公网 server <公网IP>:7000
        │
        │  公网用户访问
        ▼
公网入口 <公网IP>:20022
```

| 值 | 写在哪 | 含义 |
|---|---|---|
| `server_addr` | 客户端 | 服务端控制面地址，`公网IP:7000` 或 `域名:7000` |
| `server_name` | 客户端 | 证书 SAN，域名或 IP；必须和证书一致 |
| `local_addr` | 客户端 | 运行 client 的这台机器上的目标服务，如 `127.0.0.1:22` |
| `remote_port` | 客户端 | 公网映射端口，如 `20022` |

服务端只写 `bind_addr` 和 `control_port`；`7000` 是控制面，`7400` 是管理面板，`remote_port` 是业务入口。

## 2. 本机准备：下载、证书、token

在将要运行 Meatus 的机器上执行。

```bash
VERSION=1.1.0
# 按平台选择包名：
# macOS Apple Silicon: aarch64-apple-darwin
# macOS Intel:         x86_64-apple-darwin
# Linux x86_64:        x86_64-unknown-linux-musl
TARGET=aarch64-apple-darwin

curl -LO "https://gitee.com/binsail/meatus/releases/download/v${VERSION}/meatus-${VERSION}-${TARGET}.tar.gz"
tar xzf "meatus-${VERSION}-${TARGET}.tar.gz"
cd "meatus-${VERSION}-${TARGET}"
./meatus --version
```

预期：输出 `meatus 1.1.0` 或同版本信息。

```bash
./meatus gen-cert --domain localhost --domain 127.0.0.1 --out certs
./meatus gen-token
```

预期：

```text
token   X-soKlfDMY1UswzI9JaKKQH9Fa4KWcvB9E5LRdCrP18
sha256  ed35c7e7bfefdcaade04956001ac051f4509b6b74cd2cfb03f3008b00d900eff
```

- `token` 后面的明文给客户端 `token`。
- `sha256` 后面的值给服务端 `token_sha256`。
- 证书目录应有 `certs/server.crt`、`certs/server.key`。

检查点：`ls certs` 能看到两个文件；token 已保存。

## 3. 本机写 server.toml

创建 `server.toml`，把 `<sha256>` 换成上一步的值：

```toml
[server]
bind_addr = "127.0.0.1"
control_port = 7000
tls_cert = "certs/server.crt"
tls_key = "certs/server.key"

[tenant]
default = "default"

[auth]
mode = "hmac"
provider = "local"
clients = [
  { id = "demo", tenant = "default", token_sha256 = "<sha256>" },
]

[ports]
range = [20000, 30000]

[admin]
bind = "127.0.0.1:7400"
panel = true

[log]
level = "info"
format = "text"
```

校验：

```bash
./meatus check -c server.toml
```

预期：`OK server.toml (server, toml)`，并列出 `control 127.0.0.1:7000`、`admin 127.0.0.1:7400`。

启动：

```bash
./meatus server -c server.toml
```

预期：进程保持运行，日志出现控制面监听 / admin 监听，没有 `error`。

检查点：另开终端 `curl -fsS http://127.0.0.1:7400/healthz` 返回健康状态。

## 4. 本机写 client.toml

创建 `client.toml`，把 `<token>` 换成第 2 步的明文 token：

```toml
[client]
server_addr = "127.0.0.1:7000"
server_name = "localhost"
client_id = "demo"
tenant = "default"
token = "<token>"
server_ca_cert = "certs/server.crt"

[[proxy]]
name = "demo-web"
type = "tcp"
local_addr = "127.0.0.1:8080"
remote_port = 20080

[log]
level = "info"
format = "text"
```

校验并启动：

```bash
./meatus check -c client.toml
./meatus client -c client.toml
```

预期：`OK client.toml (client, toml)`，客户端日志出现连接成功 / 注册 proxy 成功。

检查点：client 日志没有 `authentication failed`、`certificate verify failed`、`connection refused`。

## 5. 第一条隧道：访问本地网页

本机另开一个终端，起演示网页：

```bash
python3 -m http.server 8080 --bind 127.0.0.1
```

浏览器打开：

```text
http://127.0.0.1:20080
```

预期：看到 `Directory listing for /` 或当前目录文件列表。

检查点：网页能打开，说明 `client -> server -> remote_port` 整条链路通了。

如果 `8080` 被占用，换一个本地端口（例如 `18080`），同时修改 `client.toml` 的 `local_addr`，并把 `remote_port` 换成端口池内未占用的端口。

## 6. 公网路线：服务器安装

以下命令在公网 Linux 服务器执行。

```bash
VERSION=1.1.0
TARGET=x86_64-unknown-linux-musl
curl -LO "https://gitee.com/binsail/meatus/releases/download/v${VERSION}/meatus-${VERSION}-${TARGET}.tar.gz"
tar xzf "meatus-${VERSION}-${TARGET}.tar.gz"
cd "meatus-${VERSION}-${TARGET}"

sudo ./scripts/install.sh \
  --binary ./meatus \
  --domain 203.0.113.10
```

`--domain` 用服务器的公网 IP 或域名；用 IP 时会生成 IP SAN 证书。安装脚本只支持 Linux + systemd。

检查：

```bash
sudo systemctl status meatus-server --no-pager
curl -fsS http://127.0.0.1:7400/healthz
sudo cat /etc/meatus/client.token
sudo ls -l /etc/meatus/certs
```

预期：

- `meatus-server` 为 `active (running)`；
- `/healthz` 返回健康状态；
- 能读到客户端明文 token；
- `server.crt`、`server.key` 存在。

检查点：三条命令都成功，记下 token 和 `server.crt`。

## 7. 公网路线：本机客户端

在运行 client 的本机执行。

1. 把服务器上的 `/etc/meatus/certs/server.crt` 复制到本机 `certs/server.crt`。
2. 把 `/etc/meatus/client.token` 的内容作为 `token`。
3. 写 `client.toml`：

```toml
[client]
server_addr = "203.0.113.10:7000"
server_name = "203.0.113.10"
client_id = "home-nas"
tenant = "default"
token = "<服务器 client.token 的内容>"
server_ca_cert = "certs/server.crt"

[[proxy]]
name = "ssh"
type = "tcp"
local_addr = "127.0.0.1:22"
remote_port = 20022

[log]
level = "info"
format = "text"
```

校验并启动：

```bash
meatus check -c client.toml
meatus client -c client.toml
```

预期：client 日志出现连接成功，proxy `ssh` 注册成功。

检查点：client 没有认证失败或证书校验失败。

## 8. 公网验证：SSH 回家

先确认运行 client 的本机 22 端口有 SSH 服务：

```bash
nc -vz 127.0.0.1 22
```

macOS 需要在“系统设置 → 通用 → 共享 → 远程登录”打开；Linux 需要 `sshd` 运行。

然后在任意一台能上网的机器执行：

```bash
ssh -p 20022 <本机用户名>@203.0.113.10
```

预期：登录到运行 client 的那台机器。

检查点：SSH 能登录；如果超时，先查服务器安全组 / 防火墙是否放行 `7000/tcp` 和 `20022/tcp`。

## 9. 管理面板

面板默认只监听服务器回环 `127.0.0.1:7400`，不要直接对公网开放。

在本机执行 SSH 隧道：

```bash
ssh -L 7400:127.0.0.1:7400 <服务器用户>@203.0.113.10
```

浏览器打开：

```text
http://127.0.0.1:7400
```

登录 token 在服务器：

```bash
sudo cat /etc/meatus/admin.token
```

检查点：能登录面板，能看到 server 状态和 client / proxy 列表。

## 10. Docker 路线

在仓库 `deploy/docker` 目录：

```bash
MEATUS_DOMAIN=203.0.113.10 docker compose up -d --build
docker compose ps
docker compose logs -f meatus
```

取 token 和证书：

```bash
docker compose exec meatus cat /data/admin.token
docker compose cp meatus:/data/client.token ./client.token
docker compose cp meatus:/data/certs/server.crt ./server.crt
```

检查点：

- `docker compose ps` 中 meatus 为 running；
- 宿主机 `curl 127.0.0.1:7400/healthz` 成功；
- 已拿到 `client.token` 和 `server.crt`。

容器内面板需要监听 `0.0.0.0:7400`；宿主机端口仍建议发布为 `127.0.0.1:7400:7400`。

## 11. HTTP 域名隧道（可选）

服务端 `server.toml` 增加：

```toml
[http]
enabled = true
bind = "0.0.0.0:80"
https_bind = "0.0.0.0:443"
domain = "*.tunnel.example.com"
```

客户端 `client.toml` 增加：

```toml
[[proxy]]
name = "demo-web"
type = "http"
local_addr = "127.0.0.1:8080"
remote_port = 0
custom_domains = ["demo.tunnel.example.com"]
```

前置条件：

- DNS 把 `demo.tunnel.example.com` 解析到服务器公网 IP；
- 安全组 / 防火墙放行 `80/tcp`、`443/tcp`；
- 服务端和客户端都重启或 SIGHUP。

访问：

```text
http://demo.tunnel.example.com
```

检查点：域名返回本地网页；404 时先查 `custom_domains` 与 `Host` 是否一致、client 是否在线。

## 12. 收尾：开机自启、防火墙、备份

服务器：

```bash
sudo systemctl enable --now meatus-server
sudo systemctl status meatus-server --no-pager
sudo tar czf "meatus-backup-$(date +%F).tgz" -C / etc/meatus var/lib/meatus
```

防火墙只放行必要端口：

```bash
# ufw 示例
sudo ufw allow 7000/tcp
sudo ufw allow 20022/tcp
# 开 HTTP 时再放行
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
```

不要放行 `7400` 到公网；管理面板走 SSH 隧道。

毕业检查：

```bash
meatus --version
meatus check -c client.toml
curl -fsS http://127.0.0.1:7400/healthz
ssh -p 20022 <用户>@203.0.113.10
```

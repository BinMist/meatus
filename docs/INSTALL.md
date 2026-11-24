# Meatus 1.4 安装与长期运行

V1 交付**完全开源（Apache-2.0）自托管版**：一台公网 server + 若干内网 client，单二进制、零数据库、零 Redis。本文覆盖 Linux systemd、Docker Compose、纯 `docker run` 和手工安装四种方式。

> `1.4.0` 已于 2026-11-22 发布，当前线上正式版本为 `1.4.0`；四平台包、SBOM、ACR 多架构镜像与 Gitee Release 均已提供。`1.4.0` 以全量测试、兼容矩阵、20% 弱网、ZAP baseline、10 client 并发与性能基线为发布门禁；24h soak 不作为发布前门禁，1.4.0 构建已于 2026-11-22 发布后补跑 PASS（1437/1437、fail=0、max_restarts=0）。Windows 暂不提供。

## 1. 产物与平台

| 产物 | 平台 | 说明 |
|---|---|---|
| `meatus-<version>-x86_64-unknown-linux-musl.tar.gz` | Linux x86_64 | 静态 musl，解压即用 |
| `meatus-<version>-aarch64-unknown-linux-musl.tar.gz` | Linux arm64 | NAS / 树莓派 / ARM VPS |
| `meatus-<version>-aarch64-apple-darwin.tar.gz` | macOS Apple Silicon | 本地调试 / 内网 client |
| `meatus-<version>-x86_64-apple-darwin.tar.gz` | macOS Intel | 本地调试 / 内网 client |
| Docker 镜像 | linux/amd64、linux/arm64 | Compose 或 `docker run` 首次启动自动生成证书与 token |

每个包附带 `.sha256`。**server 与 client 是同一个二进制**，只用子命令区分。

## 2. 从 release 包安装（推荐）

```bash
VERSION=1.4.0
TARGET=x86_64-unknown-linux-musl
curl -LO "https://your-release-host/meatus-${VERSION}-${TARGET}.tar.gz"
sha256sum -c "meatus-${VERSION}-${TARGET}.tar.gz.sha256"
tar xzf "meatus-${VERSION}-${TARGET}.tar.gz"
cd "meatus-${VERSION}-${TARGET}"

sudo ./scripts/install.sh \
  --binary ./meatus \
  --domain tunnel.example.com \
  --server-addr tunnel.example.com:7000
```

`install.sh` 会：

- 安装 `/usr/local/bin/meatus`；
- 创建系统用户 `meatus`、`/etc/meatus`、`/var/lib/meatus`；
- 首次安装时生成自签证书（`/var/lib/meatus/certs`）、client token、admin token；
- 写入 `/etc/meatus/server.toml` 与 `/etc/meatus/client.toml`，**已存在则不覆盖**；
- 同时安装 `config/*.example.yaml` 到 `/usr/share/doc/meatus/config/`，需要 YAML 时手工复制。
- 安装并启动 `meatus-server.service`。

装完后：

```bash
systemctl status meatus-server
journalctl -u meatus-server -f
cat /etc/meatus/client.token      # 交给内网 client
cat /etc/meatus/admin.token       # 面板登录
```

面板默认只绑 `127.0.0.1:7400`，从本机通过 SSH 隧道访问：

```bash
ssh -L 7400:127.0.0.1:7400 user@server
# 浏览器打开 http://127.0.0.1:7400，token 用 admin.token
```

### 内网 client

把 release 包和 `/etc/meatus/client.toml`（或 token）带到内网机器：

```bash
sudo ./scripts/install.sh --binary ./meatus --with-client \
  --domain tunnel.example.com --server-addr tunnel.example.com:7000
```

如果 client 与 server 不在同一台机器，需要把 server 的 `/etc/meatus/server.crt` 复制到 client 的 `/etc/meatus/server.crt`；client 配置里的 `server_ca_cert` 指向它。不想用自签证书时，换成公网 CA 证书并删除 `server_ca_cert`。

## 3. 手工安装（systemd）

```bash
sudo useradd --system --home /var/lib/meatus --shell /usr/sbin/nologin meatus
sudo install -d -m 0755 /etc/meatus /var/lib/meatus /var/lib/meatus/certs /var/lib/meatus/acme
sudo install -m 0755 meatus /usr/local/bin/meatus
sudo install -m 0600 config/server.example.toml /etc/meatus/server.toml
sudo install -m 0600 config/client.example.toml /etc/meatus/client.toml
# 需要 YAML 时改用：
# sudo install -m 0600 config/server.example.yaml /etc/meatus/server.yaml

# 证书
sudo /usr/local/bin/meatus gen-cert --domain tunnel.example.com --domain 127.0.0.1 \
  --out /var/lib/meatus/certs

# token：服务端只存 sha256，明文给 client
/usr/local/bin/meatus gen-token
```

编辑 `/etc/meatus/server.toml`：

```toml
[server]
bind_addr = "0.0.0.0"
control_port = 7000
tls_cert = "/var/lib/meatus/certs/server.crt"
tls_key = "/var/lib/meatus/certs/server.key"

[auth]
clients = [
  { id = "home-nas", token_sha256 = "<gen-token 输出的 sha256>" },
]

[admin]
bind = "127.0.0.1:7400"
admin_token = "<随机 64 hex>"
panel = true
```

安装 unit：

```bash
sudo install -m 0644 deploy/systemd/meatus-server.service /etc/systemd/system/
sudo install -m 0644 deploy/systemd/meatus-client.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now meatus-server
```

`systemd` unit 已包含 `Restart=on-failure`、`LimitNOFILE=1048576`、`NoNewPrivileges`、`ProtectSystem=strict`，并允许非 root 绑定 80/443（`CAP_NET_BIND_SERVICE`）。

## 4. Docker 部署

镜像同时包含 `server`、`client` 与 Web 面板。Compose 与纯 `docker run` 两种方式等价，按环境选一种：4.1–4.5 为 Compose，4.6 为纯 `docker run`。

### 4.1 启动 server

```bash
cd deploy/docker
# 公网部署务必替换成真实域名；证书只在首次启动生成
MEATUS_DOMAIN=tunnel.example.com docker compose up -d --build
docker compose ps
docker compose logs -f meatus
```

首次启动自动生成到命名卷 `meatus-data`（容器内 `/data`）：

- `/data/server.toml`
- `/data/certs/server.crt`、`server.key`
- `/data/admin.token`
- `/data/client.token`

查看 token：

```bash
docker compose exec meatus cat /data/admin.token
docker compose cp meatus:/data/client.token ./client.token
docker compose cp meatus:/data/certs/server.crt ./server.crt
```

面板默认只发布到宿主机 `127.0.0.1:7400`，登录需要 admin token；不要把 7400 直接暴露公网。若确需非 loopback 访问，在 `server.toml` 中配置 `[admin.tls]` 并只发布 TLS 端口，同时保持 `admin_token`。

修改配置：

```bash
docker compose cp meatus:/data/server.toml ./server.toml
# 编辑 server.toml
docker compose cp ./server.toml meatus:/data/server.toml
docker compose restart meatus
```

YAML 变体（管理面可写；如需面板写回不要使用 `:ro` 只读挂载）：

```bash
docker login registry.cn-hangzhou.aliyuncs.com

docker run -d --name meatus \
  -v "$PWD/server.yaml:/data/server.yaml" \
  -v meatus-data:/data \
  registry.cn-hangzhou.aliyuncs.com/mist/meatus:1.4.0 \
  server --config /data/server.yaml
```

YAML 配置下管理面板同样可以写回系统设置和 client proxy；写回会规范化重写，注释和键顺序不保证保留。若挂载为 `:ro`，面板保存会失败。

默认配置只发布 7000（控制面）与 7400（面板）。client 申请的 `remote_port` 必须逐个映射；开启 `[http]` 后再映射 80/443。在 `docker-compose.yml` 的 `ports` 中追加：

```yaml
      - "20022:20022"   # 某个 TCP proxy
      - "80:80"         # 开启 [http]
      - "443:443"
```

> 容器内面板必须绑 `0.0.0.0` 才能被宿主机访问，因此入口会改写 `bind = "0.0.0.0:7400"` 并启用 admin token。宿主机侧仍只发布到 `127.0.0.1`。

### 4.2 容器内权限

镜像默认以 root 启动入口脚本，先 `chown -R meatus:meatus /data`，再 `su-exec meatus` 降权运行二进制。这样：

- 命名卷首次创建可直接写；
- bind mount 使用宿主机目录时，不会因为宿主机 UID 与容器内 UID 不同而首启失败；
- 容器真正运行 meatus 时是非 root。

### 4.3 运行 client

客户端镜像使用同一 `meatus:local`，不需要映射端口（由 server 监听），只需要能访问 server 控制面与内网目标。

```bash
cp client.example.toml ./client.toml
# 修改 server_addr / server_name / client_id / token / local_addr
docker run -d --name meatus-client --restart unless-stopped \
  -v "$PWD/client.toml:/data/client.toml" \
  -v "$PWD/server.crt:/data/server.crt:ro" \
  --add-host host.docker.internal:host-gateway \
  meatus:local client --config /data/client.toml
docker logs -f meatus-client
```

- `token` 填服务端 `/data/client.token` 的内容，`client_id` 必须与服务端 `clients[].id` 一致。
- Docker Desktop / OrbStack 用 `host.docker.internal` 访问宿主机；Linux 用 `--add-host host.docker.internal:host-gateway`，或直接把 `server_addr` / `local_addr` 写成可达地址。
- 客户端容器内进程同样降权为 `meatus`；挂载的 `client.toml` 若为 0600 且属于宿主机其他 UID，入口的 chown 会把它改成容器内 UID 100。若不想改属主，可 `chmod 0644 client.toml` 后以只读方式挂载。
- 如果目标服务就在宿主机上，`local_addr` 用 `host.docker.internal:8080`；在 Linux 也可用 `--network host` 并改回 `127.0.0.1:8080`。

### 4.4 构建与镜像源

以下命令在仓库根目录执行（先 `cd ../..`）：

```bash
docker build -t meatus:local .
# 国内网络：基础镜像与 crates 镜像
docker build \
  --build-arg BASE_REGISTRY=docker.m.daocloud.io/library/ \
  --build-arg CARGO_REGISTRY=https://rsproxy.cn/index/ \
  -t meatus:local .
```

Compose 也支持同名环境变量（回到 `deploy/docker` 执行）：

```bash
cd deploy/docker
BASE_REGISTRY=docker.m.daocloud.io/library/ \
CARGO_REGISTRY=https://rsproxy.cn/index/ \
docker compose up -d --build
```

多架构构建用 Buildx：

```bash
docker buildx build --platform linux/amd64,linux/arm64 -t meatus:1.4.0 .
```

构建后做一次部署冒烟（首启、面板、Bearer API、SIGTERM）：

```bash
IMAGE=meatus:local ./scripts/docker-smoke.sh
```

生成发布包时会自动生成 SPDX SBOM 并放入包内 `SBOM.spdx.json`：

```bash
brew install syft                 # 或把 syft 放到 PATH
./scripts/build-release.sh
# 仅本地快速打包、不需要 SBOM 时：
./scripts/build-release.sh --no-sbom
```

> 离线环境无法拉取 node / rust / alpine 基础镜像时，请使用 `scripts/build-release.sh` 产出的静态二进制配合 systemd，或提前 `docker load` 离线镜像。

### 4.5 备份 / 恢复

在 `deploy/docker` 目录执行：

```bash
# 备份 /data（配置、证书、token、ACME 缓存）
docker compose exec -T meatus tar czf - -C /data . > meatus-data.tgz

# 停止写入后恢复（用临时容器，避免对已停止容器 exec）
docker compose stop meatus
docker compose run --rm --entrypoint sh meatus \
  -c 'tar xzf - -C /data' < meatus-data.tgz
docker compose start meatus
```

### 4.6 纯 docker run（不依赖 Compose）

只装了 Docker、没有 Compose 插件时，用下面的命令直接运行。镜像先按 4.4 构建为 `meatus:local`。

#### server

```bash
docker volume create meatus-data

docker run -d --name meatus \
  --restart unless-stopped \
  --stop-timeout 20 \
  -p 7000:7000 \
  -p 127.0.0.1:7400:7400 \
  -v meatus-data:/data \
  -e MEATUS_DOMAIN=tunnel.example.com \
  --health-cmd "wget -qO- http://127.0.0.1:7400/healthz >/dev/null || exit 1" \
  --health-interval 30s \
  --health-timeout 5s \
  --health-start-period 10s \
  --health-retries 3 \
  meatus:local server --config /data/server.toml
```

`--restart unless-stopped` 会在进程崩溃后自动拉起；手动 `docker stop` 后不会自动启动，用 `docker start meatus` 恢复。

查看状态、日志与 token：

```bash
docker ps --filter name=meatus
docker inspect -f '{{.State.Health.Status}}' meatus
docker logs -f meatus
docker exec meatus cat /data/admin.token
docker cp meatus:/data/client.token ./client.token
docker cp meatus:/data/certs/server.crt ./server.crt
```

按需增加端口映射；容器不会自动映射 client 申请的端口：

```bash
-p 20022:20022    # 某个 TCP proxy
-p 80:80          # 开启 [http]
-p 443:443
```

修改配置：

```bash
docker cp meatus:/data/server.toml ./server.toml
# 编辑 server.toml
docker cp ./server.toml meatus:/data/server.toml
docker restart meatus
```

如不用命名卷，可改成宿主机目录（入口会 chown 后降权，Linux 下宿主机目录属主会变成 UID 100）：

```bash
mkdir -p ./meatus-data
# 把上面的 -v meatus-data:/data 换成：
-v "$PWD/meatus-data:/data"
```

停止 / 删除容器（**不要加 `-v`**，否则会删除配置与证书）：

```bash
docker stop meatus
docker rm meatus
# 数据卷保留，下次同名挂载可继续使用
```

#### client

```bash
cp deploy/docker/client.example.toml ./client.toml
# 填 server_addr / server_name / client_id / token / local_addr

docker run -d --name meatus-client \
  --restart unless-stopped \
  -v "$PWD/client.toml:/data/client.toml" \
  -v "$PWD/server.crt:/data/server.crt:ro" \
  --add-host host.docker.internal:host-gateway \
  meatus:local client --config /data/client.toml

docker logs -f meatus-client
```

client 容器不需要发布端口；`token` 用 server 的 `/data/client.token`，`client_id` 要匹配服务端配置。

#### 直接备份命名卷

```bash
docker stop meatus
docker run --rm --entrypoint sh \
  -v meatus-data:/data -v "$PWD:/backup" meatus:local \
  -c 'tar czf /backup/meatus-data.tgz -C /data .'
docker start meatus
```

### 4.7 使用预构建镜像

当前发布到阿里云 ACR；GHCR 与 GitHub Release 暂缓：

```bash
# ACR 当前为私有仓库，先登录
docker login registry.cn-hangzhou.aliyuncs.com

# 固定版本 / 次版本 / 最新
docker pull registry.cn-hangzhou.aliyuncs.com/mist/meatus:1.4.0
docker pull registry.cn-hangzhou.aliyuncs.com/mist/meatus:1.3
docker pull registry.cn-hangzhou.aliyuncs.com/mist/meatus:latest
```

server / client 使用同一镜像，启动参数与 4.6 相同，把镜像名替换为上面的仓库地址即可。镜像覆盖 linux/amd64 与 linux/arm64；ACR 推送关闭 buildx attestation，镜像 SBOM 由 syft 单独扫描生成。GHCR 路径保留在发布脚本中，待恢复 GitHub 发布时启用。

## 5. 升级与回滚

1. `systemctl stop meatus-server`（或 `docker compose stop`）。
2. 备份 `/etc/meatus`、`/var/lib/meatus`（证书与 ACME 缓存）。
3. 替换 `/usr/local/bin/meatus`。
4. `meatus check --config /etc/meatus/server.toml` 校验配置。
5. `systemctl start meatus-server`，观察 `/healthz` 与面板。

配置不兼容时回滚旧二进制与旧配置；token / 证书不随二进制变化。

## 6. 端口与防火墙

| 端口 | 用途 | 暴露范围 |
|---|---|---|
| 7000/tcp | 控制面 TLS + yamux | 公网（client 连接） |
| 80/tcp、443/tcp | HTTP / HTTPS / ACME | 公网（开启 `[http]` 时） |
| 20000–30000/tcp+udp | 动态 proxy 端口池 | 公网（按需） |
| 7400/tcp | 面板 / metrics | 仅本机、内网或 `[admin.tls]` 保护下 |
| 2080/tcp | SOCKS5（默认关闭） | 仅显式开启时 |

## 7. 验收清单

```bash
meatus --version
meatus check --config /etc/meatus/server.toml
curl -fsS http://127.0.0.1:7400/healthz
curl -fsS http://127.0.0.1:7400/metrics | head
systemctl is-active meatus-server
```

内网 client 启动后，面板 Proxy 页应看到对应 proxy；从公网访问 `remote_port` 或域名应返回内网服务。

## 8. 安全基线

- 面板默认只绑 loopback；非 loopback 必须设置 `admin_token`，并建议开启 `[admin.tls]`。
- 管理面 HTTPS 下 Cookie 带 `Secure`，并发送 HSTS；反向代理场景默认不信任 `X-Forwarded-Proto`。
- `server.key`、`client.token`、`admin.token` 权限 0600。
- 不要把 `server.toml`、token、证书私钥提交到仓库。
- 公网只开放 7000 与业务端口；7400 不要暴露。
- V1 不接 PostgreSQL / Redis；发现配置里出现 `[ctrl]` 或数据库地址，说明误用了 Post-V1 设计。

## 9. Windows 客户端服务（1.5.1 预览）

> 状态（2026-11-24）：服务命令与安装/卸载/日志路径代码已完成，Windows GNU target 交叉编译通过；**尚未在 Windows 11 真机做 SCM 验收**，不能把本节当作已验收能力。

管理员 PowerShell 中执行：

```powershell
# 安装：复制配置到 %ProgramData%\Meatus\client.toml，注册为自动启动服务并立即启动
meatus.exe service install --config .\client.toml

# 只安装、不启动
meatus.exe service install --config .\client.toml --no-start

meatus.exe service status
meatus.exe service stop
meatus.exe service start

# 卸载；默认保留配置与日志
meatus.exe service uninstall

# 卸载并清理 %ProgramData%\Meatus
meatus.exe service uninstall --purge
```

固定路径：

| 内容 | 路径 |
|---|---|
| 配置 | `%ProgramData%\Meatus\client.toml` |
| 日志 | `%ProgramData%\Meatus\logs\client.log.YYYY-MM-DD` |
| 启动失败兜底 | `%ProgramData%\Meatus\logs\service-error.log` |

服务名 `meatus-client`，显示名 `Meatus Client`，以 `LocalSystem` 运行；服务启动后工作目录切到 `%ProgramData%\Meatus`，配置里的相对证书路径按该目录解析。安装后收紧目录 ACL：

```powershell
icacls "$env:ProgramData\Meatus" /inheritance:r `
  /grant:r "SYSTEM:(OI)(CI)F" "Administrators:(OI)(CI)F"
```

排障时可用 `meatus.exe service run --console --config <path>` 前台运行；服务由 SCM 调用时使用 `meatus service run`，不要手工常驻该命令。

Windows 11 真机验收清单（当前待执行）：

1. `service install` 后 `sc query meatus-client` 为 `RUNNING`，`binPath` 指向当前 `meatus.exe service run`；
2. 重启主机后服务自动为 `RUNNING`，`%ProgramData%\Meatus\logs` 有新日志；
3. `service stop` 后进程退出、日志停止；`service start` 后客户端重新连上 relay；
4. `service uninstall --purge` 后 `sc query` 返回 1060，`%ProgramData%\Meatus` 不存在；
5. 非管理员执行 `service install` 明确失败，不留下半装服务。

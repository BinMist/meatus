<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/meatus-logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/meatus-logo-light.png">
    <img src="docs/assets/meatus-logo-light.png" alt="Meatus" width="360">
  </picture>
</p>

# Meatus

Rust 实现的内网穿透工具：把本机端口安全地暴露到公网。**完全开源（Apache-2.0）、面向个人 / 小团队自托管**。核心代码、协议、面板和部署脚本都在本仓库中。

- **传输**：TCP + TLS 1.3 + yamux（QUIC 留 v2）
- **配置**：单二进制 + TOML；服务端只存 token 的 SHA-256
- **HTTP**：`:80` 单入口按 `Host` 精确匹配 → 泛域名匹配 → 404，原始字节流式透传（支持 chunked / gzip / WebSocket）
- **HTTPS**：两种模式可共存。`type = "https"` 默认 **SNI 透传**（server 不持私钥）；`acme.enabled = true` 时，HTTP proxy 的精确域名由 server 用 Let's Encrypt HTTP-01 自动签发、TLS 终止并缓存/续期
- **权限**：端口池、域名、限流按 `tenant_id` 命名空间分区，V1 固定 `tenant_id = "default"`；多租户接缝保留为开源扩展点
- **状态**：V1 已完成并发布，完全开源自托管版 `1.0.0`；macOS aarch64、Linux x86_64 musl 静态包、linux/arm64 与 linux/amd64 Docker 镜像均已实跑；公开镜像仓库地址在发布时替换为实际地址（文档暂用 `registry.example.com` 占位）。V1 验收记录已归档到内部资料目录（不随开源仓库发布）：fuzz 5 分钟 × 2 target、netem 20% 丢包、ZAP baseline 0 FAIL、cargo audit 无漏洞均通过；24h fuzz 作为持续巡检项。安装见 `docs/INSTALL.md`，Docker 部署见第 4 节。

## 目录结构

```
crates/
  meatus-proto/    # protobuf 定义、帧编解码、错误码
  meatus-core/     # 配置模型、租户命名、五个开源扩展接缝
  meatus-server/   # relay：TLS 终结、控制面会话、端口映射
  meatus-client/   # 客户端：拨号、握手、重连、数据面转发
  meatus-cli/      # `meatus` 二进制入口
proto/meatus.proto # MEATUS/1 线协议
config/            # server / client 示例配置
docs/              # 开发规划、ADR
scripts/           # 本地开发自检
```

## 环境要求

- Rust 1.96.0（`rust-toolchain.toml` 已固定）
- 不需要系统安装 `protoc`：构建时用 `protoc-bin-vendored`
- 构建管理面板需要 Node ≥ 20.19 / npm ≥ 10；运行 `meatus server` 不需要 Node
- 本地 Postgres / Redis 仅历史 ctrl 扩展设计使用；V1 完全开源版零数据库、零 Redis。

## Docker 快速开始（server）

```bash
cd deploy/docker
MEATUS_DOMAIN=tunnel.example.com docker compose up -d --build
docker compose exec meatus cat /data/admin.token
# 面板：http://127.0.0.1:7400 ；client 配置与 token 见 docs/INSTALL.md 第 4 节
# 不用 Compose 的纯 docker run 命令见 docs/INSTALL.md 4.6
# 预构建镜像（registry.example.com 为占位，请替换为实际公开镜像仓库）：
# docker pull registry.example.com/meatus:1.0.0
```

## 快速开始（TCP / UDP / HTTP / HTTPS 隧道）

```bash
cargo build --release
cargo test --workspace

# 1. 生成客户端 token：token 填 client.toml，sha256 填 server.toml
cargo run -p meatus-cli -- gen-token

# 2. 自签证书（多域名 / IP 写多个 --domain；key 自动 0600）
mkdir -p certs
cargo run -p meatus-cli -- gen-cert \
  --domain tunnel.example.com --domain 127.0.0.1 --out certs

# 3. 配置
cp config/server.example.toml server.toml
cp config/client.example.toml client.toml
# server.toml：tls_cert / tls_key、clients[].token_sha256
# client.toml：server_addr、server_name、client_id、tenant、token；
#              自签场景把 server_ca_cert 指向 certs/server.crt

# 4. 启动（两个终端）
cargo run -p meatus-cli -- server -c server.toml
cargo run -p meatus-cli -- client -c client.toml

# 5. 验证：连接服务端 remote_port 应命中客户端 local_addr
nc 127.0.0.1 20022

# 6. HTTP：server.toml 打开 [http] enabled = true，client.toml 增加
#    type = "http"、local_addr、custom_domains 的 proxy 后：
curl -H "Host: nas.tunnel.example.com" http://127.0.0.1:80/

# 7. UDP：client.toml 增加 type = "udp" 的 proxy 后
dig @127.0.0.1 -p <remote_port> example.com

# 8. SOCKS5：server.toml 打开 [socks5] enabled = true，
#    client.toml 增加 type = "socks5" 的 proxy 后：
curl --socks5 127.0.0.1:2080 http://10.0.0.12:8080/
# 只支持 IP 目标与 CONNECT；目标默认限定内网 CIDR，可用 options.allow_cidrs 覆盖
# 当前仅登记一个全局 SOCKS5 路由，多 client 路由策略留待后续

# 9. 指标 / 健康检查 / 状态（默认只绑本机）
curl -s http://127.0.0.1:7400/metrics
curl -s http://127.0.0.1:7400/healthz
meatus status --admin http://127.0.0.1:7400                # 无 token
meatus status --admin http://127.0.0.1:7400 --token "$MEATUS_ADMIN_TOKEN"
# 设置 admin_token 后，/api/v1/status 与 /metrics 需要 Authorization: Bearer

# 10. 热重载：改 client.toml 的 [[proxy]] 后
kill -HUP <client_pid>
```

当前行为边界：

- 控制面：客户端先连 control 端口，TLS 1.3 握手后由客户端打开 control stream 并发 `Hello`；服务端回 `Challenge`，客户端用 `HMAC-SHA256(key=sha256(token), nonce)` 应答。
- 数据面：TCP 每个公网连接一条 yamux stream，先写 `StreamHeader` 再双向拷贝；UDP 每个 proxy 一条专用 stream，帧为 `UdpPacket{session_id,payload}`，空闲 60s 回收会话，单包 ≤1500B，默认上限 2048 会话/proxy。
- HTTP：server `[http] enabled = true` 时绑定 `http.bind` / `https_bind`；`Host` 精确优先、再最长后缀泛域名，未知 404，非法请求 400，超过 route `max_conns` 返回 503。代理类型写 `type = "http"`，`custom_domains` 必填，`remote_port` 必须为 0。
- HTTPS：同一 listener 读取 TLS ClientHello 的 SNI。默认走 `type = "https"` 的 **SNI 透传**，客户端 `local_addr` 指向自己的 TLS 服务；`acme.enabled = true` 且域名由 `type = "http"` 注册时，server 用 ACME HTTP-01 签发证书并终止 TLS，再把明文 HTTP 转发给本地 HTTP 服务。无 SNI / 无路由直接断开。
- SOCKS5：server `[socks5] enabled = true` 才监听；只接受 `CONNECT` 与 IPv4/IPv6 目标，域名目标暂不支持。目标先过服务端 `allow_cidrs`，再写入 `StreamHeader.dst_hint`，由 client 从内网侧发起连接并做第二次白名单校验；默认拒绝公网目标。当前只登记一个全局路由，`remote_port` 必须为 0。
- ACME：仅支持精确域名 + HTTP-01；泛域名需要 DNS-01，当前不自动签发，请继续用 `type = "https"` 自带证书。证书与账户缓存在 `acme.cert_dir`，文件 0600、目录 0700；`staging = true` 指向 Let's Encrypt staging。TLS-ALPN-01 尚未接入。
- 心跳：客户端 10s Ping、30s 无 Pong 重连；服务端 15s Ping、45s 无消息关闭。
- 重连：`1→2→4→8→16→30s`，±20% 抖动；重连后全量重注册 proxy。`kill -9 client` 需要 systemd / Docker 等 supervisor 拉起。
- 证书：默认系统根；自签 / 私有 CA 用 `client.server_ca_cert`。`server_public_key_pin` 支持 `sha256:<64hex>` 或裸 64 hex：先校验证书链，再比对叶子 SPKI SHA-256。
- 限速 / 统计：每 proxy 一个令牌桶（`rate_limit_bps`，默认不限），超限只延迟不丢包；client 每 10s 上报 `Stats`，server 输出 Prometheus 文本到 `/metrics`，`/healthz` 返回 uptime。
- 热重载：client 收到 `SIGHUP` 后重新解析配置并做 proxy 差量（新增 / 删除 / 修改先删后加），失败保留旧配置；`server_addr` / `token` 等连接级字段变更需重启。
- 管理面：`/metrics`、`/healthz`、`/api/v1/status`、`/api/v1/settings`、`/api/v1/certificates`、`/api/v1/logs`；配置 `admin_token` 后除 `/healthz` 外均要求 Cookie 会话或 `Authorization: Bearer`。`meatus status` 直接请求 `/api/v1/status` 并输出 clients/proxies 表格。
- 管理面板：`meatus server` 启动后访问 `http://127.0.0.1:7400/`；登录用 `admin_token` 换 HttpOnly、SameSite=Strict 会话 Cookie，登出校验 `X-Meatus-CSRF`；连续失败 5 次锁 10 分钟。详见下方面板章节。


## Web 管理面板

`web/` 是 Vue 3 + TypeScript + Vite 8 工程，UI 全部来自 `@sailonix/ui`，按子路径按需导入，主题走 `var(--sl-*)` 语义 token。构建产物 `web/dist` 由 `rust-embed` 编进 `meatus-server`，因此运行期不依赖 Node、不需要额外部署前端。

```bash
# 1. 先构建前端，再构建 Rust（build.rs 会在 web/dist 缺失时直接编译失败）
cd web && npm ci && npm run build
cd .. && cargo build --release

# 2. 启动；默认只绑 127.0.0.1:7400
./target/release/meatus server -c server.toml
# 浏览器打开 http://127.0.0.1:7400/ ，用 [admin] admin_token 登录
```

配置：

```toml
[admin]
bind = "127.0.0.1:7400"
panel = true
admin_token = "REPLACE_WITH_ADMIN_TOKEN" # 非 loopback 必填；为空时配置校验会拒绝
```

六个页面：登录、总览、Proxy 管理、证书状态、实时日志、系统设置。

- 总览：5 秒轮询 `/api/v1/status`，展示客户端 / proxy / 活跃连接 / uptime 与累计流量曲线。
- Proxy 管理：通过 admin API 新增、编辑、删除运行中的 proxy；server 下发管理帧到指定 `run_id` 的 client，等待 ack 后生效，失败回滚上一份 runtime。client 启动时带 `--config` 会把变更写回 `client.toml`，重启后仍保留。写操作走 CSRF 校验，表单支持类型、域名、本地地址、端口、连接数 / 限速 / 空闲 / CIDR 高级项。
- 证书状态：读取 `acme.cert_dir` 中的 PEM，解析叶证书 SAN、到期时间、剩余天数、序列号、签发者与链长度；状态区分 managed / expiring / expired / pending / not-configured。
- 实时日志：2 秒拉取 `/api/v1/logs?limit=300`，页脚显示总行数与当前展示行数；内容来自进程内 2000 行环形缓冲，重启即清空，不落盘、不包含 admin token。
- 系统设置：在线修改 server / 租户 / 端口池 / HTTP / ACME / SOCKS5 / 日志并写回 `server.toml`；保存前校验配置和端口预检，成功后自动重启生效。TLS、admin 绑定、认证 token 与会话策略保持只读。

安全边界：

- 面板静态资源与 API 同一 admin listener，默认只绑 loopback；非 loopback 的 HTTPS / `Secure` Cookie 尚未实现，当前只建议本机或 SSH 隧道访问。
- 所有响应带 `X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer` 与 CSP（`script-src 'self'`）；`admin.panel = false` 时静态资源返回 404。
- `admin_token` 不写浏览器存储，只用于换会话；Cookie HttpOnly、SameSite=Strict、12 小时过期；登录失败按 IP 限流。
- 未授权 `/api/v1/*` 统一 401 JSON；`/healthz` 保持公开供探活。

## 本地开发自检

```bash
cp .env.example .env   # 填入本地开发凭据；.env 已被 gitignore
chmod 600 .env
./scripts/dev-check.sh
```

自检覆盖：Rust 工具链、`.env`、`local-postgres`、`local-redis`、宿主端口连通性，并提示容器绑定 `0.0.0.0` 的风险。

## 许可证

Apache License 2.0，见 `LICENSE`。面板依赖的第三方组件声明见 `THIRD_PARTY_NOTICES.md`，完整依赖许可证清单见 `docs/THIRD_PARTY_LICENSES.md`，上游许可证原文见 `docs/THIRD_PARTY_LICENSE_TEXTS.txt`。

## 文档

- 完整使用教程：`docs/USAGE.md`
- 开发规划与验收记录：已归档到内部资料目录，不随开源仓库发布
- ADR：`docs/adr/0001-transport-tcp-tls-yamux.md`、`docs/adr/0002-dual-mode-self-host-first.md`
- 贡献指南：`CONTRIBUTING.md`
- 行为准则：`CODE_OF_CONDUCT.md`
- 安全策略：`SECURITY.md`
- 版本变更：`CHANGELOG.md`

## 协作者

- BinSail

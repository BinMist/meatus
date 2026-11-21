<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/meatus-logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/meatus-logo-light.png">
    <img src="docs/assets/meatus-logo-light.png" alt="Meatus" width="360">
  </picture>
</p>

# Meatus

Rust 实现的内网穿透工具：把本机端口安全地暴露到公网。**完全开源（Apache-2.0）、面向个人 / 小团队自托管**。核心代码、协议、面板和部署脚本都在本仓库中。

- **传输**：默认 TCP + TLS 1.3 + yamux，控制连接与 4 条数据连接分离；可选 QUIC（`server.quic = true` + `client.transport = "quic"` / `"auto"`，`auto` 5 秒失败回落 TCP）
- **配置**：单二进制 + TOML（默认）/ YAML（V1.1 支持，管理面可写）；服务端只存 token 的 SHA-256
- **HTTP**：`:80` 单入口按 `Host` 精确匹配 → 泛域名匹配 → 404，原始字节流式透传（支持 chunked / gzip / WebSocket）
- **HTTPS**：两种模式可共存。`type = "https"` 默认 **SNI 透传**（server 不持私钥）；`acme.enabled = true` 时，精确域名由 server 用 HTTP-01 自动签发、TLS 终止并缓存/续期，泛域名走 DNS-01（支持 Cloudflare / DNSPod）
- **权限**：端口池、域名、限流按 `tenant_id` 命名空间分区，V1 固定 `tenant_id = "default"`；多租户接缝保留为开源扩展点
- **版本状态**：`1.4.0` 已于 2026-11-22 发布，当前线上正式版本为 `1.4.0`。四平台包、`.sha256`、源码/镜像 SBOM、ACR 多架构镜像 `1.4.0` / `1.4` / `latest` 与 Gitee Release 均已发布；生产完成 `1.3.0 → 1.4.0` 升级与回滚验证。Windows 暂不提供；GHCR 与 GitHub Release 暂缓。`1.4.0` 以全量测试、兼容矩阵、20% 弱网、ZAP baseline、10 client 并发与性能基线为发布门禁；24h soak 不作为发布前门禁，发布后补跑。安装见 `docs/INSTALL.md`，完整教程见 `docs/USAGE.md`，Docker 部署见下方“Docker 快速开始”。

## 当前版本摘要

`1.4.0`：

- **协议 v2 与旧客户端兼容**：`MEATUS/2` 加法协商，1.2.1 / 1.3.0 客户端继续以 v1 接入；1.4 客户端连接 1.3.0 服务端时自动回落 v1 + TCP。
- **传输升级**：QUIC 可选、`auto` 5 秒回落；默认 TCP 下控制连接与 4 条数据连接分离，消除高并发心跳饥饿。
- **多入口与 IPv6**：`server_addrs` 顺序切换与指数退避；`::` 双栈、IPv6-only、Host / SNI / ACL 全链路支持。
- **连接池与流元数据**：`[client.pool]` 默认关闭、可按需开启；`trace_id` / `connection_id` 贯穿日志、指标与审计。
- **优雅 drain**：`SIGTERM` 停止接收新连接，存量流继续完成。
- **性能与兼容实验室**：TCP / QUIC 基准、性能门禁、3×2 兼容矩阵与旧客户端 fixture 全部脚本化并接入 CI。

完整版本历史、升级说明与已知限制见 [CHANGELOG.md](CHANGELOG.md)。

## 目录结构

```
crates/
  meatus-proto/    # protobuf 定义、帧编解码、错误码
  meatus-core/     # 配置模型、租户命名、五个开源扩展接缝
  meatus-server/   # relay：TLS 终结、控制面会话、端口映射
  meatus-client/   # 客户端：拨号、握手、重连、数据面转发
  meatus-cli/      # `meatus` 二进制入口
proto/meatus.proto # MEATUS 线协议（v1/v2）
config/            # server / client 示例配置
docs/              # 使用教程、安装、ADR、示例
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
# 预构建镜像（ACR 当前为私有仓库，先登录）：
# docker login registry.cn-hangzhou.aliyuncs.com
# docker pull registry.cn-hangzhou.aliyuncs.com/mist/meatus:1.4.0
# docker pull registry.cn-hangzhou.aliyuncs.com/mist/meatus:1.4
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

# 3. 配置（TOML 默认；需要 YAML 时用 config/*.example.yaml，YAML 管理面可写但会规范化重写）
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
curl --socks5-hostname 127.0.0.1:2080 http://nas.internal:8080/
# 支持 IP / 域名目标与 CONNECT；目标默认限定内网 CIDR，可用 options.allow_cidrs 覆盖
# 域名目标必须显式配置 options.allow_domains（精确或 *.suffix），空表示全部拒绝
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
- 控制面防护：每个控制面连接在 TLS 握手前按来源 IP 计数，60s 滑动窗口最多 10 次；超限直接拒绝。TLS、控制流、Hello 校验、认证失败或等待认证超时统一延迟 3s 再断开；已认证成功后的正常会话断线不延迟。
- 数据面：TCP 每个公网连接一条 yamux stream，先写 `StreamHeader` 再双向拷贝；UDP 每个 proxy 一条专用 stream，帧为 `UdpPacket{session_id,payload}`，空闲 60s 回收会话，单包 ≤1500B，默认上限 2048 会话/proxy。
- HTTP：server `[http] enabled = true` 时绑定 `http.bind` / `https_bind`；`Host` 精确优先、再最长后缀泛域名，未知 404，非法请求 400，超过 route `max_conns` 返回 503。代理类型写 `type = "http"`，`custom_domains` 必填，`remote_port` 必须为 0。
- HTTPS：同一 listener 读取 TLS ClientHello 的 SNI。默认走 `type = "https"` 的 **SNI 透传**，客户端 `local_addr` 指向自己的 TLS 服务；`acme.enabled = true` 且域名由 `type = "http"` 注册时，精确域名优先用 ACME HTTP-01，泛域名用 DNS-01 签发证书并终止 TLS；DNS-01 下未启用 HTTP 时，精确域名也走 DNS-01；泛域名证书只覆盖一级子域名。签发后把明文 HTTP 转发给本地 HTTP 服务。无 SNI / 无路由直接断开。
- SOCKS5：server `[socks5] enabled = true` 才监听；只接受 `CONNECT`，支持 IPv4/IPv6 与域名目标。域名目标必须命中 client `options.allow_domains`（精确域名或 `*.suffix`），由 client 解析 DNS 并按 `allow_cidrs` 过滤后固定首个允许 IP；server 只做白名单校验，不解析内网域名。未配置 `allow_domains` 时域名目标仍拒绝，默认拒绝公网目标。当前只登记一个全局路由，`remote_port` 必须为 0。
- ACME：精确域名支持 HTTP-01；泛域名支持 Cloudflare / DNSPod DNS-01，按 `_acme-challenge.<域名>` 创建/清理 TXT，到期前 30 天续期，续期失败保留旧证书。证书与账户缓存在 `acme.cert_dir`，文件 0600、目录 0700；`staging = true` 指向 Let's Encrypt staging。TLS-ALPN-01 尚未接入。
- 心跳：客户端 10s Ping、30s 无 Pong 重连；服务端 10s Ping、25s 无消息关闭。
- 重连：`1→2→4→8→16→30s`，±20% 抖动；重连后全量重注册 proxy。`kill -9 client` 需要 systemd / Docker 等 supervisor 拉起。
- 证书：默认系统根；自签 / 私有 CA 用 `client.server_ca_cert`。`server_public_key_pin` 支持 `sha256:<64hex>` 或裸 64 hex：先校验证书链，再比对叶子 SPKI SHA-256。
- 限速 / 统计：每 proxy 一个令牌桶（`rate_limit_bps`，默认不限），超限只延迟不丢包；client 每 10s 上报 `Stats`，server 输出 Prometheus 文本到 `/metrics`（含 `meatus_probe_total{result}` 与 `meatus_pool_*` 连接池指标），`/healthz` 返回 uptime。
- 代理诊断：面板 Proxy 行的“测试”按钮经控制帧 `Probe` / `ProbeResult` 由客户端探测 `local_addr`；TCP / HTTP / HTTPS 返回可达性与延迟，UDP 只做 socket bind/connect，SOCKS5 提示“需实际请求验证”。单 proxy 限速 1 次 / 秒、超时 3 秒，不影响正常转发。
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
trust_forwarded_proto = false

[admin.tls]                              # 可选：管理面独立 HTTPS
enabled = false
# cert = "/data/certs/admin.crt"
# key = "/data/certs/admin.key"
hsts = true
```

九个页面：登录、总览、Proxy 管理、客户端与令牌、证书状态、审计日志、API Token、实时日志、系统设置。

- 总览：5 秒轮询 `/api/v1/status`，展示客户端 / proxy / 活跃连接 / uptime 与累计流量曲线。
- Proxy 管理：通过 admin API 新增、编辑、删除运行中的 proxy；server 下发管理帧到指定 `run_id` 的 client，等待 ack 后生效，失败回滚上一份 runtime。client 启动时带 `--config` 会把变更写回 `client.toml` / `client.yaml`，重启后仍保留；YAML 会规范化重写。写操作走 CSRF 校验，表单支持类型、域名、本地地址、端口、连接数 / 限速 / 空闲 / CIDR 高级项。
- 客户端与令牌：管理 `auth.clients`，展示在线状态、proxy 数量、授权摘要、配额与 token 指纹；支持创建、轮换、删除，明文 token 只在响应中出现一次。
- 证书状态：读取 `acme.cert_dir` 中的 PEM，解析叶证书 SAN、到期时间、剩余天数、序列号、签发者与链长度；状态区分 managed / expiring / expired / pending / not-configured。
- 审计日志：查询最近 90 天 JSONL，按时间、操作者、client、动作、结果过滤，并可导出当前查询结果为 NDJSON；敏感值已脱敏。
- API Token：管理 `[[admin.api_tokens]]` 的 Scope、到期与吊销状态；服务端只存 SHA-256，面板显示指纹与“最后使用”时间。
- 实时日志：2 秒拉取 `/api/v1/logs?limit=300`，页脚显示总行数与当前展示行数；内容来自进程内 2000 行环形缓冲，重启即清空，不落盘、不包含 admin token。
- 系统设置：在线修改 server / 租户 / 端口池 / HTTP / ACME / SOCKS5 / 日志并写回 `server.toml` / `server.yaml`；保存前校验配置和端口预检，成功后热加载，需重启的字段通过 `restart_required` 返回。TLS、admin 绑定、认证 token 与会话策略保持只读；YAML 会规范化重写。

安全边界：

- 面板静态资源与 API 同一 admin listener，默认只绑 loopback；非 loopback 建议开启 `[admin.tls]`，HTTPS 下 Cookie 自动带 `Secure` 并发送 HSTS。
- 所有响应带 `X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer` 与 CSP（`script-src 'self'`、`style-src 'self'`，不含 `unsafe-inline`）；`admin.panel = false` 时静态资源返回 404。
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

Apache License 2.0，见 `LICENSE`。面板依赖的第三方组件声明见 `THIRD_PARTY_NOTICES.md`，完整依赖许可证清单见 `docs/THIRD_PARTY_LICENSES.md`，上游许可证原文见 `docs/THIRD_PARTY_LICENSE_TEXTS.txt`。发布包内 `SBOM.spdx.json` 为 SPDX 源码依赖清单；镜像 SBOM 单独输出为 `dist/sbom-meatus-<version>-image.spdx.json`。当前镜像只实推阿里云 ACR；ACR 不接受 buildx 的 OCI 1.1 attestation manifest，改为推送后再用 syft 扫描生成镜像 SBOM。GHCR 发布路径保留在脚本中，待恢复 GitHub 发布时启用。

## 文档

- 完整使用教程：`docs/USAGE.md`（含 V1.3 更新、客户端令牌、SOCKS5 域名、管理面 TLS、YAML 与 Docker 示例）
- 新手教程 Skill：`docs/skill/meatus-tutorial/`（复制到 Agent 的 `.user_skills/` 后可按步骤教学）
- 安装部署：`docs/INSTALL.md`
- 版本发布：<https://gitee.com/binsail/meatus/releases/tag/v1.4.0>
- 开发规划与验收记录：已归档到内部资料目录，不随开源仓库发布
- ADR：`docs/adr/0001-transport-tcp-tls-yamux.md`、`docs/adr/0002-dual-mode-self-host-first.md`
- 贡献指南：`CONTRIBUTING.md`
- 行为准则：`CODE_OF_CONDUCT.md`
- 安全策略：`SECURITY.md`
- 版本变更：`CHANGELOG.md`

## 协作者

- BinSail

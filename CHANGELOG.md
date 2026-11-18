# Changelog

本文件记录 Meatus 的版本变更。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。

## [1.1.0] - 2026-11-18

V1.1：公网可用性加固 + ACR 镜像发布 + YAML 配置。

### Added

- 管理面独立 TLS：`[admin.tls]` 支持 `enabled` / `cert` / `key` / `hsts`，不复用隧道 `[server] tls_cert` / `tls_key`。
- `admin.trust_forwarded_proto`：默认 `false`，仅在可信反向代理后识别 `X-Forwarded-Proto: https`。
- YAML 配置：`.yaml` / `.yml` 只读解析；`meatus server|client|check --format auto|toml|yaml`。
- `config/server.example.yaml`、`config/client.example.yaml`，均可直接 `meatus check`。
- 镜像发布脚本 `scripts/docker-release.sh`：支持 GHCR / 阿里云 ACR、多架构、三标签、digest 比对；当前 `1.1.0` 实推 ACR，GHCR 暂缓。GHCR 保留镜像 SBOM attestation，ACR 因不支持 OCI 1.1 empty descriptor 改为 syft 单独扫描。
- `scripts/sbom.sh`：从 `Cargo.lock` 与 `web/package-lock.json` 生成 SPDX SBOM；`build-release.sh` 将其作为 `SBOM.spdx.json` 打入发布包。
- 发布流水线支持 GHCR / ACR 登录与双推、SPDX SBOM、随 Release 上传；当前 `1.1.0` 仅启用 ACR。
- `meatus check` 输出实际配置格式；YAML 解析错误报告文件、行、列。

### Changed

- 版本升至 `1.1.0`；发布包与镜像同时包含 TOML / YAML 示例。
- 发布包包含 `SBOM.spdx.json`；GHCR 镜像附带 SBOM attestation，ACR 镜像 SBOM 单独输出。
- 管理面 HTTPS 下登录/登出 Cookie 自动带 `Secure`，并发送 HSTS。
- 管理面 CSP 从 `style-src 'self' 'unsafe-inline'` 收紧为 `style-src 'self'`。
- 管理面板系统设置、client proxy 写回遇到 YAML 配置时返回只读错误，不尝试写回。
- 文档 `README.md`、`docs/USAGE.md`、`docs/INSTALL.md`、`SECURITY.md` 更新为 V1.1 行为。

### Security

- 管理面 TLS 与隧道 TLS 证书生命周期解耦。
- 非 loopback 仍强制 `admin_token`，开启 TLS 不放宽认证。
- 反向代理协议头默认不信任，避免伪造 `X-Forwarded-Proto` 绕过 Cookie 策略。
- YAML 配置整体只读，避免管理面破坏用户注释与格式。

### Known limitations

- YAML 不支持写回、格式互转、`!include`、远程引用或模板执行。
- 管理面 HTTPS 的可信反代场景需显式设置 `trust_forwarded_proto = true`；默认不支持任意代理链。
- GHCR 与 GitHub Release 暂缓；当前镜像发布以阿里云 ACR 为准，ACR 仓库仍为私有，匿名拉取需先 `docker login`。

## [1.0.0] - 2026-11-18

V1 首个正式版本：完全开源、Apache-2.0、面向个人 / 小团队的自托管内网穿透工具。

### Added

- `meatus-server` 与 `meatus-client` 两个运行角色，共用 `meatus` 单二进制，通过子命令区分。
- TCP 端口映射：公网 `remote_port` 通过 TLS 1.3 + yamux 多路复用转发到内网 `local_addr`。
- UDP 转发：每个 proxy 一条专用 stream，按 `session_id` 管理会话，空闲回收。
- HTTP 域名路由：`:80` 单入口，`Host` 精确匹配优先、最长后缀泛域名匹配、未知域名 404；支持 chunked / gzip / WebSocket 原始字节透传。
- HTTPS 双模式：默认 SNI 透传；`acme.enabled = true` 时由 server 通过 Let's Encrypt HTTP-01 自动签发、终止 TLS、缓存与续期。
- SOCKS5 反向代理：默认关闭，显式开启后支持 `CONNECT` 与 IPv4 / IPv6 目标；目标默认限定内网 CIDR，默认拒绝公网目标。
- token 鉴权：client token 的 SHA-256 存在 server；控制面使用 HMAC-SHA256 挑战应答。
- 心跳与重连：客户端 10s Ping、30s 无 Pong 重连；指数退避 `1→2→4→8→16→30s`，带 ±20% 抖动；重连后全量重注册 proxy。
- 多 proxy 并行、`SIGHUP` 热重载与差量应用；失败保留旧配置。
- 端口池自动分配与回收、域名路由、每 proxy 令牌桶限速、Prometheus `/metrics`、`/healthz`、`/api/v1/status`。
- 内置 Web 管理面板：登录、总览、Proxy 管理、证书状态、实时日志、系统设置六个页面。
- 面板在线编辑：server / 租户 / 端口池 / HTTP / ACME / SOCKS5 / 日志写回 `server.toml`，保存前校验配置与端口预检，成功后自动重启。
- 面板安全：`admin_token` 换 HttpOnly、SameSite=Strict、12 小时会话 Cookie；写操作 CSRF 校验；登录失败按 IP 限流，连续 5 次锁 10 分钟；CSP 与安全响应头。
- 单二进制 + TOML：`rust-embed` 将面板静态资源编入 server；V1 零 PostgreSQL、零 Redis，运行期不需要 Node。
- 发布产物：macOS aarch64、Linux x86_64 musl 静态包；Docker `linux/amd64`、`linux/arm64` 镜像；每个包附带 `.sha256`。
- 安装与部署：release 包 + `install.sh`、systemd、Docker Compose、纯 `docker run`。
- 文档：README、`docs/INSTALL.md`、`docs/USAGE.md`、内部 V1 验收记录、ADR 0001 / 0002。

### Security

- TLS 1.3 传输；server 不保存 client token 明文，只保存 SHA-256。
- 管理面默认只绑 `127.0.0.1:7400`，与业务端口物理隔离。
- 管理 API 未授权统一 401；`/healthz` 保持公开供探活。
- 面板响应带 `X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer` 与 CSP（`script-src 'self'`）。
- `admin.panel = false` 时面板静态资源返回 404。
- 发布门禁完成：
  - `cargo fmt` / `clippy` / `cargo test` 通过；workspace 72 passed / 1 ignored。
  - fuzz 双 target 共 116,926,884 次执行，无 crash。
  - 20% 丢包弱网：20/20 请求通过，重连 PASS。
  - `cargo audit`：249 crates 无漏洞；2 条 unmaintained 警告。
  - `npm audit`：0 漏洞。
  - ZAP baseline：`FAIL-NEW: 0`、`WARN-NEW: 5`、`PASS: 63`；0 High、1 Medium、4 Low。
  - 面板安全冒烟 9 项 PASS。

### Changed

- 面板 UI 依赖从本地 vendored `@sailon/*` tarball 迁移到 npm 中央仓库 `@sailonix/ui@0.1.0` / `@sailonix/icon@0.1.0`，由 `web/package-lock.json` 锁定；移除 `web/vendor` 与 `scripts/vendor-sailon.sh`。
- 初始版本，无历史版本兼容性变更。

### Known limitations

- 24h fuzz 未纳入本次发布门禁，保留为持续巡检项。
- `chrono 0.4.45`、`rustls-pemfile 2.2.0` 存在无固定升级版本的 unmaintained 警告，持续跟踪上游。
- ZAP 接受的 1 条 Medium：CSP `style-src 'unsafe-inline'`；移除需引入 nonce / hash，留待后续安全加固。
- 管理面板尚未实现非 loopback 的 HTTPS / `Secure` Cookie，当前只建议本机或 SSH 隧道访问。
- SOCKS5 当前只支持 `CONNECT` 与 IP 目标，不支持域名目标；仅登记一个全局路由。
- ACME 自动签发只支持精确域名 + HTTP-01；泛域名需自带证书或等待 DNS-01 方案。
- GitHub Release workflow 已就绪，但当前 `origin` 为内部仓库，托管侧 Release 尚未触发。

### License

- 项目主体以 [Apache License 2.0](LICENSE) 发布。
- 第三方组件声明见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

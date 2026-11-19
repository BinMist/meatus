# Changelog

本文件记录 Meatus 的版本变更。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。

## [Unreleased]

## [1.2.0] - 2026-11-19

V1.2：客户端接入与代理可用性闭环。`1.1.1` 未单独发布，其 YAML 写回与面板修正一并包含在 `1.2.0`。

### Added

- 管理面板新增「客户端」页：列出 `auth.clients`、在线状态、proxy 数量、token 指纹与端口范围。
- 新增 `GET/POST /api/v1/clients` 与 `PUT/DELETE /api/v1/clients/{id}`：创建、轮换、删除客户端 token。
- 创建/轮换只在响应中返回一次明文 token；服务端只写 `token_sha256`，并同时生成 TOML / YAML 客户端配置片段。
- 首次通过面板创建客户端时，自动把 `auth.token_sha256` 简写迁移为 `auth.clients` 数组；TOML 内联 `clients = [{...}]` 会自动规范化为 `[[auth.clients]]`。
- 客户端写回沿用备份 + 原子替换 + 1 秒后重启；删除最后一个客户端凭据会被拒绝。
- SOCKS5 域名目标：`ATYP=3` 不再直接拒绝；`options.allow_domains` 支持精确域名与 `*.example.com` 后缀。
- SOCKS5 域名解析由 client 执行：只解析一次，按 `allow_cidrs` 过滤后固定第一个允许 IP 连接，避免 DNS rebinding。
- `ProxyOptions.allow_domains` 贯穿 proto、服务端 / 客户端配置、管理 API、状态输出、配置写回与面板表单。
- 面板 Proxy 表单为 SOCKS5 增加「允许目标域名」，并在状态/API 中回显。

### Changed

- 版本升至 `1.2.0`。
- SOCKS5 域名目标默认仍拒绝；未配置 `allow_domains` 时不放行任何域名。
- 面板「客户端」页创建/轮换后提示服务将在 1 秒后重启；客户端需要更新 token 后重连。
- `auth.clients` TOML 写回从内联数组规范化为标准数组表，保留其余注释与键顺序。
- `README.md`、`docs/USAGE.md`、`docs/INSTALL.md`、配置示例与教程 Skill 更新为 V1.2 行为。

### Fixed

- 修复 YAML 配置下管理面无法保存系统设置 / proxy 的问题（随 1.2.0 发布）。
- 修复面板日志页桌面端滚动被裁切、搜索栏操作按钮缺失等 1.1.1 细节。

### Known limitations

- 客户端 CRUD 后服务端会重启约 1 秒，期间所有隧道短暂断开；尚无无中断热加载。
- SOCKS5 仍只支持单条路由；第二个 SOCKS5 proxy 会被拒绝。
- 域名目标只支持显式白名单，不做自动 DNS 发现或按租户动态策略。
- 泛域名 ACME（DNS-01）、Proxy 连通性探测、跨平台发布仍属于后续版本。
- GHCR 与 GitHub Release 暂缓；当前镜像发布以阿里云 ACR 为准。

## [1.1.1] - 2026-11-19

V1.1.1 补丁：YAML 管理面写回与管理面板细节修正。

### Added

- 管理设置响应新增 `config_format`，设置页显示当前配置为 TOML 还是 YAML。
- 管理面板 Proxy / 证书页搜索栏补充重置、查询操作按钮；日志页桌面端改为面板内滚动，避免底部被裁切。

### Changed

- YAML 配置支持管理面写回：`server.yaml` 的系统设置与 `client.yaml` 的 proxy 增删都会持久化；YAML 采用规范化重写，注释和键顺序不保证保留。TOML 仍保留注释与键顺序。
- server YAML 写回前生成 `.bak` 备份；写回失败不覆盖原文件。
- 管理面板操作列、表单栅格、页面间距与圆角细节统一；设置页提示补充 YAML 规范化重写说明。
- `README.md`、`docs/INSTALL.md`、`docs/USAGE.md`、YAML 示例与教程 Skill 更新为当前写回行为。

### Fixed

- 移除管理面与客户端对 YAML 配置的只读拒绝，修复 YAML 配置下无法在线保存设置 / proxy 的问题。
- 修复日志页在桌面端高度不足时被底部裁切的问题。

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

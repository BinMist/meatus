# Changelog

本文件记录 Meatus 的版本变更。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循语义化版本。

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
- 文档：README、`docs/INSTALL.md`、`docs/USAGE.md`、`docs/M6_ACCEPTANCE.md`、ADR 0001 / 0002。

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

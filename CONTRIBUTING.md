# 贡献指南

感谢你参与 Meatus。本文说明本地开发、代码约定、测试门禁与提交 / PR 流程。

参与本项目即表示你同意遵守 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)，并以 [Apache-2.0](LICENSE) 许可授权你的贡献。

## 1. 先读这些文档

- [README.md](README.md)：项目定位、能力边界与快速开始
- [docs/DEVELOPMENT_PLAN.md](docs/DEVELOPMENT_PLAN.md)：V1 范围、架构、协议与安全设计
- [docs/USAGE.md](docs/USAGE.md)：部署、配置、面板与故障排查
- [docs/M6_ACCEPTANCE.md](docs/M6_ACCEPTANCE.md)：V1 发布门禁与已知遗留
- [SECURITY.md](SECURITY.md)：漏洞报告渠道与安全边界

## 2. 环境要求

| 依赖 | 版本 | 说明 |
|---|---|---|
| Rust | `1.96.0` | 由 `rust-toolchain.toml` 固定 |
| Node.js | `>= 20.19` | CI 使用 Node 22；仅构建管理面板需要 |
| npm | `>= 10` | 面板依赖管理 |
| Docker | 可选 | Docker 部署、弱网与安全冒烟测试 |

`protoc` 不需要系统安装：构建使用 `protoc-bin-vendored`。

## 3. 本地开发

```bash
git clone <repo-url> meatus
cd meatus

# Rust 侧
cargo build --release

# 管理面板
cd web
npm ci
npm run build
cd ..

# 面板产物编进 server 二进制后，再构建一次
cargo build --release
```

运行 `meatus server` 不需要 Node；`web/dist` 通过 `rust-embed` 编入二进制。

### 3.1 本地自检

```bash
cp .env.example .env
chmod 600 .env
./scripts/dev-check.sh
```

`scripts/dev-check.sh` 会检查 Rust 工具链、`.env`、本地 `local-postgres` / `local-redis` 以及宿主端口。V1 运行期是**零 PostgreSQL、零 Redis**；该脚本保留的是历史 ctrl 开发环境的自检，只做核心隧道 / 面板开发时可以跳过，直接跑第 4 节的命令。

### 3.2 面板依赖

面板的 `@sailonix/ui` / `@sailonix/icon` 依赖从 npm 中央仓库安装，版本由 `web/package-lock.json` 锁定；`npm ci` 不需要私有 npm 源。修改面板依赖时：

- 同时更新 `web/package.json` 与 `web/package-lock.json`；
- 确认新版本仍带上游 `LICENSE`，并同步更新 `THIRD_PARTY_NOTICES.md`；
- 不要把依赖改回本地绝对路径或 `file:` vendor tarball；
- 不要提交 `web/node_modules`。

### 3.3 配置与证书

本地开发不要提交真实 token、私钥或生产证书。`.env`、`*.key`、`*.pem`、`*.crt` 仅放在本地或 secret 管理中。

## 4. 提交前必跑门禁

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets

cd web && npm run build && cd ..
```

涉及发布配置时再跑：

```bash
cargo test --workspace --all-targets --locked
```

### 4.1 可选集成 / 安全测试

按改动范围选择，不需要每次全跑：

```bash
# 依赖漏洞
cargo audit
npm audit --omit=dev --audit-level=high --registry=https://registry.npmjs.org

# 面板安全冒烟（需要已启动 server 与 admin token）
./scripts/security-smoke.sh http://127.0.0.1:7400 <admin-token>

# 弱网测试（需要 Docker 与 NET_ADMIN）
./scripts/weaknet-test.sh

# fuzz（需要 nightly 与 cargo-fuzz）
./scripts/fuzz.sh 300
```

## 5. 代码结构边界

| 目录 | 职责 | 改动注意 |
|---|---|---|
| `crates/meatus-proto` | protobuf 定义、帧编解码、错误码 | 协议变更必须同步 `proto/meatus.proto`，并说明兼容性 |
| `crates/meatus-core` | 配置模型、租户命名、扩展接缝 | 不要引入 V1 明确非目标 |
| `crates/meatus-server` | 公网 relay、TLS、控制面、端口映射、管理 API | 改动管理面必须检查鉴权、CSRF 与响应头 |
| `crates/meatus-client` | 拨号、握手、重连、数据面转发 | 保持断线重连与热重载语义 |
| `crates/meatus-cli` | `meatus` 二进制入口 | 新子命令需要同步文档与帮助 |
| `web/` | Vue 3 + TypeScript + Vite 8 管理面板 | 保持按需引入与语义 token；不要引入运行期 Node 依赖 |
| `deploy/`、`scripts/` | systemd、Docker、发布与测试脚本 | 脚本改动需要实际跑一次 |

协议、配置模型或安全边界发生实质变化时，请同时更新 `docs/DEVELOPMENT_PLAN.md` 和相应 ADR，不要只改代码。

## 6. 提交与 PR

提交信息使用 Conventional Commits 风格，与现有历史保持一致：

```text
feat(web): add proxy edit dialog
fix(server): reject unauthorized metrics request
docs: document mysql tcp proxy
test: cover reconnect backoff
chore: update release script
```

建议：

- 一个 PR 只做一件逻辑上的事，避免把重构、功能、格式化混在一起；
- PR 描述写清动机、行为变化、测试结果与风险；
- 修复 bug 时附最小复现步骤；
- 涉及面板的改动附截图或录屏；
- 不提交生成物、编辑器配置、内部排障笔记或设计过程文件。

### PR 检查清单

- [ ] `cargo fmt --all --check` 通过
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 通过
- [ ] `cargo test --workspace --all-targets` 通过
- [ ] `cd web && npm run build` 通过（涉及面板时）
- [ ] 新增 / 修改的配置项已更新示例配置与文档
- [ ] 新增依赖的许可证与 Apache-2.0 兼容，并更新 `THIRD_PARTY_NOTICES.md`
- [ ] 没有提交 token、私钥、`.env`、生产地址或内部截图
- [ ] 安全相关改动没有在公开 PR 中暴露可利用细节

## 7. 文档

文档和代码同等重要。以下改动必须同步文档：

- CLI 参数、配置字段、默认值；
- HTTP / HTTPS / SOCKS5 行为边界；
- 面板页面、权限与安全策略；
- 发布产物、安装方式、升级 / 回滚步骤；
- 已知限制与不兼容变更。

## 8. 安全与隐私

- 发现安全漏洞不要开公开 Issue，按 [SECURITY.md](SECURITY.md) 私下报告。
- Issue / PR 中的日志、配置、截图必须先脱敏：删除 token、Cookie、私钥、公网 IP、内网主机名。
- 不要为复现问题要求维护者提供生产环境凭据。

## 9. 发布

维护者发布 V1 版本时：

1. 确认 `Cargo.toml` 与 `web/package.json` 版本一致；
2. 跑完第 4 节门禁与 `docs/M6_ACCEPTANCE.md` 对应验收；
3. 更新 `CHANGELOG.md`；
4. 打 `vX.Y.Z` tag；
5. 由 release workflow 生成静态包、校验和与 GitHub Release。

普通贡献者不需要自行打 tag 或推送 release。

## 10. 许可

本项目以 [Apache License 2.0](LICENSE) 开源。提交贡献即表示你同意该贡献以 Apache-2.0 授权，不额外要求 CLA。第三方代码、素材和依赖必须保留其原始许可证，并在 `THIRD_PARTY_NOTICES.md` 中列明。

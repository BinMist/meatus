# M6 验收记录（V1.0.0）

日期：2026-11-18
范围：静态产物与发布流水线、fuzz 与弱网、安全扫描与 V1 收口
结论：**PASS（V1 发布门禁）**。0 个发布阻塞项；24h fuzz 作为持续巡检项保留，不阻塞本次 V1 发布。

## 1. 静态产物与发布流水线

| 产物 | 结果 |
| --- | --- |
| macOS aarch64 包 | `dist/meatus-1.0.0-aarch64-apple-darwin.tar.gz`，二进制 8,989 KiB；sha256 `1644ea5aa08046c4718785e4656a2bd62958d7620817737fd00462a1fa8b6940` |
| Linux x86_64 musl 包 | `dist/meatus-1.0.0-x86_64-unknown-linux-musl.tar.gz`，tar 4.1 MiB；静态二进制 `static-pie linked`、9.8 MiB；sha256 `b8aa099a31a7373033d29e15b64956e909eb2a377ff8251119633ef6a00ae4b1` |
| 包内容 | `meatus` 二进制、`config/server.example.toml`、`config/client.example.toml`、systemd units、`scripts/install.sh`、README、INSTALL、DEVELOPMENT_PLAN |
| 二进制自检 | `docker run --rm -v .../meatus:/meatus alpine:3.22 /meatus --version` → `meatus 1.0.0` |
| Docker 冒烟 | `IMAGE=registry.example.com/meatus:1.0.0-panel ./scripts/docker-smoke.sh` → `docker smoke PASS`（control=7800、admin=127.0.0.1:7801） |
| 发布流水线 | `.github/workflows/release.yml` 已配置 Linux x86_64 / aarch64 musl 矩阵、Web 构建、测试、`scripts/build-release.sh --no-web --target`、artifact 上传与 GitHub Release。当前 `origin` 为内部仓库，托管侧 GitHub Release 未触发；本地流水线已用同一脚本复现 macOS 与 Linux 包 |

## 2. fuzz 与弱网

### fuzz

- 环境：nightly `1.102.0-nightly (5df8c02af 2026-11-16)`，`cargo-fuzz 0.13.2`。
- 命令：`./scripts/fuzz.sh 300`。
- 结果：
  - `decode_control_frame`：`Done 113333341 runs in 301 second(s)`，无 crash。
  - `parse_config`：`Done 3593543 runs in 301 second(s)`，无 crash。
  - 合计 116,926,884 次执行，`fuzz done: no crash`，exit 0。
- 24h soak：**本轮未执行**。保留为持续巡检项；V1 发布门禁采用双 target 各 5 分钟 smoke。

### 弱网

- 环境：`alpine:3.22 --platform linux/amd64 --cap-add=NET_ADMIN`，20% 丢包 `tc netem`，使用当前静态 Linux 二进制。
- 结果：
  - `lossy requests: 20/20`
  - `reconnect: PASS`
  - `RESULT: PASS (20% loss)`
- 测试脚本已加固：基线 10 秒超时；丢包请求 `--retry 2 --retry-delay 1 --max-time 10`；重连最多轮询 30 秒。第一次 18/20 + reconnect FAIL 是脚本 3 秒等待过短，不是隧道崩溃；复跑通过。

## 3. 安全扫描

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 面板安全冒烟 | `./scripts/security-smoke.sh http://127.0.0.1:7400 <admin-token>` | 9 项 PASS：未授权 401、登录 + CSRF、4 个安全响应头、无 CSRF 403、带 CSRF 400、登出失效 |
| Rust 依赖漏洞 | `cargo audit` | 249 crates，无漏洞；2 条 allowed unmaintained 警告：`chrono 0.4.45` RUSTSEC-2026-0543、`rustls-pemfile 2.2.0` RUSTSEC-2025-0134，均无 fixed upgrade |
| Web 依赖漏洞 | `npm audit --omit=dev --audit-level=high --registry=https://registry.npmjs.org` | `found 0 vulnerabilities` |
| ZAP baseline | `ghcr.io/zaproxy/zaproxy:stable`，spider + HTML/JSON/Markdown 报告 | `FAIL-NEW: 0`、`WARN-NEW: 5`、`PASS: 63`；0 High、1 Medium、4 Low |

ZAP 报告已归档：`docs/security/zap-baseline-2026-11-18.md` 与 `.json`（扫描目标为面板的临时 7401 转发地址）。

ZAP 已接受风险：

- Medium：CSP `style-src 'unsafe-inline'`。面板存在运行时内联样式；移除需引入 nonce/hash，留到后续安全加固。
- Low：Non-Storable Content、Permissions Policy 缺失、Modern Web Application、COEP Header Missing/Invalid。均为低风险或信息性发现，不阻塞 V1。

## 4. 质量门禁

| 检查 | 结果 |
| --- | --- |
| `cargo fmt --all` | 通过 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通过（修复 Rust 1.96 的 2 处 `collapsible_if` 与 1 处 `redundant_closure`） |
| `cargo test --workspace --all-targets` | 通过；workspace 72 passed / 1 ignored，server admin 2 passed，tunnel 7 passed / 1 ignored |
| `npm run build --prefix web` | 通过 |

## 5. 遗留与后续

- 24h fuzz 未在本轮执行，保留为 nightly / 发布前 soak。
- `chrono` / `rustls-pemfile` 的 unmaintained 警告无固定升级版本，持续跟踪上游。
- ZAP CSP `style-src 'unsafe-inline'` 为 1 条 Medium，已记录为接受风险。
- GitHub Release workflow 代码已就绪，但当前 origin 为内部仓库；需要镜像到 GitHub 或在 CI 侧接入后才能产生托管 Release。

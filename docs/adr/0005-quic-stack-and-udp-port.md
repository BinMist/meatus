# ADR 0005：QUIC 选型、TLS 复用与 UDP 端口策略

- 状态：已接受（D1-01）
- 日期：2026-11-21
- 决策者：项目负责人

## 背景

V1.4 要为控制面增加 QUIC 传输。现有 TCP 控制面使用 rustls 0.23（ring provider、TLS 1.3）
与 yamux 0.14；要求 QUIC 不引入第二套 TLS 栈、不改变证书校验语义，并且可以与 TCP
在同一份配置下并存、默认关闭。

## 决策

### 1. 选型

使用 `quinn 0.11`（`runtime-tokio` + `rustls-ring`），不启用 `rustls-aws-lc-rs`。
quinn 直接依赖工作区已有的 `rustls 0.23`，证书根、SPKI pin、TLS 1.3 版本策略与
TCP 控制面一致。Spike 已验证 `quinn::crypto::rustls::QuicClientConfig::try_from`
可直接包装现有 `rustls::ClientConfig`，服务端可用
`quinn::ServerConfig::with_single_cert` 接收同一份 PEM 解析出的证书链与私钥。

TLS 复用边界：

- 证书 / 私钥解析继续使用 `rustls-pemfile`，不新增证书库；
- 客户端根证书、`server_name`、SPKI pin 继续走现有 `client::tls` 规则；
- QUIC 不启用 0-RTT，避免重放与首包放大问题。

### 2. 传输映射

QUIC 的一条双向流对应 yamux 的一条流语义：控制流固定为客户端打开的第一条双向流，
数据流按连接池需要在同一 QUIC 连接上继续 `open_bi`。业务层仍只看到
`transport::Connection`，不直接依赖 quinn 类型。

### 3. UDP 端口策略

QUIC 默认与 TCP 控制面使用同一数值端口（例如 `server_addr = 0.0.0.0:21090`，
则 TCP 21090 + UDP 21090）。TCP 与 UDP 端口空间独立，不需要额外偏移；容器与
防火墙需要同时放行 UDP。`transport = "tcp"` 时不绑定 UDP，不改变 1.3 行为。
`auto` 只影响客户端拨号顺序，不改变服务端监听集合。

### 4. 默认关闭与回落

`transport` 默认 `tcp`；`quic` 需显式开启。`auto` 先试 QUIC，5 秒内失败回落 TCP，
回落原因写日志与指标；不允许静默把 QUIC 失败当作成功。

## Spike 结果

`crates/meatus-core/src/quic.rs` 的 `quic_handshake_and_stream_roundtrip` 使用
`rcgen` 自签证书完成 quinn 服务端 / 客户端握手，并在一条双向流上完成 `ping` / `pong`。
测试 0.17s 通过，证明 quinn 0.11.12 与 rustls 0.23 在本工作区可共存，无需降级
yamux 或替换 TLS 栈。

## 后果

- 正面：QUIC 与 TCP 共享证书与校验策略；传输层可继续抽象在 `transport`。
- 负面：新增 quinn / quinn-proto 依赖，二进制体积增加；UDP 在部分网络被 QoS。
- 风险：UDP 被封锁时只能回落 TCP；QUIC 的流上限与连接上限必须在 D1-05 显式设置。

## 备选方案

| 方案 | 放弃原因 |
| --- | --- |
| 使用 quinn 自带 aws-lc-rs | 与现有 ring provider 冲突，增加第二套 crypto 栈 |
| 使用 s2n-quic / quiche | 需要额外 C / BoringSSL 依赖，跨平台构建复杂 |
| QUIC 使用独立端口 | 配置与防火墙复杂度翻倍，无必要 |

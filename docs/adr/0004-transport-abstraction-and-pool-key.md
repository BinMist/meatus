# ADR 0004：传输抽象与连接池键

- 状态：已接受（D5-01）
- 日期：2026-11-21
- 决策者：项目负责人

## 背景

V1.3 的控制面直接调用 `multiplex::spawn(io, yamux::Mode::*)`，上层同时持有
`OpenHandle` 与 `Inbound`。V1.4 要加入 QUIC、连接池和多入口，如果继续把
yamux 句柄散落在 client / server，后续每加一种传输都要改业务代码。

## 决策

### 1. 统一入口

`meatus_core::transport` 只暴露：

- `Role::{Client, Server}`；
- `attach(io, role) -> Connection`；
- `Connection::{open, open_handle, accept}`；
- `PoolKey`。

`multiplex` 模块降为私有实现；client / server 不再直接命名 `yamux` 类型。
QUIC 在 D1 接入时新增 `transport::quic` 实现，但公开签名保持不变。

### 2. 池键

`PoolKey` 格式固定为 `proxy/host`：

- `proxy` 去首尾空白，保持大小写；配置层已禁止代理名包含 `/`；
- `host` 去首尾空白并转小写；域名大小写不敏感；
- 空 proxy、空 host、任一含 `/` 均返回 `PoolKeyError`。

固定代理用 `proxy.local_addr` 作为 host；SOCKS5 动态目标用 `dst_hint`。
同一个 proxy 的不同 host 必须是不同键，避免跨目标复用错误连接。

### 3. 默认关闭

D5-01 只建立抽象与键，不改变运行时行为：仍是一条控制连接、无池。
D5-02 / D5-03 才加入 `[client.pool]` 解析与生命周期，默认 `enabled = false`。

## 后果

- 正面：QUIC 与连接池不再侵入控制面业务；键规则可用单测固定。
- 负面：`Connection` 目前仍由 yamux 驱动；池化前每条连接只服务一个会话。
- 风险：host 归一化只处理大小写与空白，不做 IDN / DNS 解析；同一目标的不同
  写法（如 `localhost` 与 `127.0.0.1`）仍视为不同键，后续如需合并要显式加规则。

## 备选方案

| 方案 | 放弃原因 |
| --- | --- |
| 继续直接传 `OpenHandle` + `Inbound` | QUIC 与池化会扩散到所有调用点 |
| 池键用 `proxy` 单键 | SOCKS5 / 多目标会错误复用连接 |
| 池键用序列化 JSON | 日志与指标可读性差，哈希成本更高 |

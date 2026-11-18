# Meatus 内网穿透工具・开发规划（Rust）



| 项    | 内容                                                                 |
| ---- | ------------------------------------------------------------------ |
| 文档版本 | v1.8                                                               |
| 日期   | 2026-11-17                                                         |
| 状态   | **V1 范围锁定：完全开源、自托管**；M0–M6 已实现并提交，发布加固已落地（fuzz 5 分钟 × 2 target、netem 20% 丢包、完整 ZAP baseline 均已通过；24h fuzz 作为持续巡检）；多租户接缝仅作开源扩展，不进入产品路线图 |
| 技术栈  | Rust 2024 / Tokio / rustls / yamux / rustls-acme；前端 Vue3 + TS + Vite8 + `@sailonix/ui`；V1 零数据库 / 零 Redis |
| 目标版本 | **v1.0（M6 完全开源自托管版，含面板与 ACME；V1 终点）**，workspace 版本已置 `1.0.0`，许可证 Apache-2.0 |
| 预计工期 | 单人 10 周（49 人日，纯自托管）                            |

> `meatus`
>
> （拉丁语「通道、管口」）。命名对应产品本质：把公网的一个 "开口" 接到内网深处的一条通道上。



***

## 一、产品定位与范围

### 1.1 一句话定位

**一个 Rust 写的、单二进制、内存占用极低、完全开源的内网穿透 / 反向代理工具**：内网机器只需主动向外建立一条长连接，即可把 SSH、数据库、Web 服务、UDP 服务暴露到公网服务器的指定端口或域名上，不需要公网 IP、不需要改路由器。**V1 交付 Apache-2.0 许可的个人 / 小团队自托管版**；协议与租户接缝保留为开源扩展能力（见 2.5）。

### 1.2 目标用户与场景



| 场景            | 典型诉求                    | 对应能力                          |
| ------------- | ----------------------- | ----------------------------- |
| 个人 / 小团队自建    | 家里 NAS、树莓派、开发机远程访问      | TCP 端口映射 + 一条命令启动             |
| 运维 / 交付       | 客户内网设备排障、临时联调           | 多 proxy 配置、热重载、断线自动重连         |
| 开发联调          | 本地 Web / 回调地址暴露给外部      | HTTP 域名路由、HTTPS SNI 透传        |
| 内网出口          | 从公网侧访问内网任意主机            | SOCKS5 反向代理（M4）               |
| 轻量私有化         | 不想用 ngrok/frp 云服务，数据自己管 | 单 server 二进制 + TLS + token 鉴权 |
| 开源二次开发 / 自建平台（非官方产品） | 想基于 Meatus 做自己的接入平台 | Apache-2.0 源码 + 扩展接缝；官方不提供独立控制面 |

### 1.3 V1 范围（做什么）

1. `meatus-server`（公网侧）与 `meatus-client`（内网侧）两个二进制，共用一套协议。
2. 传输层：**TCP + TLS 1.3 + yamux 多路复用**为默认（一条连接承载全部隧道流量）。
3. 代理类型按优先级交付：**TCP → UDP → HTTP → HTTPS → SOCKS5**（SOCKS5 默认关闭，显式开启）。
4. HTTPS：默认 **ACME 自动签发与续期**（HTTP-01 / TLS-ALPN-01；泛域名走 DNS-01），保留"自带证书 + SNI 透传"作兜底。
5. 控制面：token 鉴权（HMAC 挑战应答）、心跳、断线指数退避重连、多 proxy 并行。
6. 运维面：TOML 配置、热重载、结构化日志、Prometheus 指标、`/healthz`、`/api/v1/status`。
7. 管理面：**内置 Web 面板**（Vue3 + TS + Vite8，UI 全部取自 `@sailonix/ui` 按需导入），静态资源 `rust-embed` 打进 server 二进制，仍是单文件分发；默认只绑 `127.0.0.1`。
8. 端口资源：服务端端口池自动分配与回收；HTTP 走泛域名 + Host 路由，不占端口。
9. **产品形态**：V1 交付「完全开源自托管版」（单机 server + TOML 即可跑，Apache-2.0）；代码层保留租户、鉴权源、配额、计量等扩展接缝，**不实现独立控制面、不接数据库、不做计费**—— 详见 2.5。

### 1.4 明确非目标（这一版不做，避免范围失控）

| 非目标 | 原因 | 计划 |
|---|---|---|
| P2P 打洞（STUN 直连） | 国内对称 NAT / 企业防火墙命中率低 | v2（M8），先做 relay 版 |
| 兼容 frp 的 ini 配置与协议 | 兼容成本高，且是历史包袱 | 永不 |
| Kubernetes Operator / CRD | 用户群不匹配 | 视需求 |
| 独立控制面（自助注册、签发、计费、账单） | 完全开源版不提供 | 永久非目标；第三方可基于 Apache-2.0 自行扩展 |
| 官方多租户 / 计费前台 | 不属于自托管面板职责 | 不做 |
| 非 ACME 的商业 CA API 接入 | 需求少、各家 API 不统一 | 不做，用 ACME 或自带证书 |
| 图形化桌面客户端 | 维护成本高 | 不做，用面板 / 脚本替代 |

### 1.5 竞品对标



| 项目      | 语言      | 优势             | 短板                              | Meatus 的差异点                    |
| ------- | ------- | -------------- | ------------------------------- | ------------------------------ |
| frp     | Go      | 功能最全、生态成熟、文档多  | 配置繁琐、内存偏高（几十 MB 起）、代码结构偏老       | 更小更快、配置更少、协议更严格、内置指标           |
| rathole | Rust    | 轻量、性能好、Rust 生态 | 功能少（无 UDP 会话管理、无 HTTP 域名路由、无面板） | 补齐 HTTP/HTTPS 路由与 UDP 会话，仍保持轻量 |
| nps     | Go      | 自带 Web 面板、上手快  | 组件重、安全设计弱（历史漏洞多）                | 默认全链路 TLS、最小权限 token           |
| bore    | Rust    | 极简、仅 TCP       | 只有 TCP、无鉴权体系                    | 生产级鉴权 + 多协议                    |
| ngrok   | Go / 闭源 | 托管体验最好      | 收费、流量过第三方                       | 私有化、零外联                        |

**结论**：不打 "功能最多"，打 "**最小可运维的私有化穿透**"—— 单二进制、默认安全、指标齐全、协议可演进。



***

## 二、总体架构

### 2.1 拓扑



```
                    ┌──────────────────────── 公网 VPS ────────────────────────┐
                    │                     meatus-server                         │
   SSH 客户端 ──────┼─► :20022 (TCP proxy) ─┐                                 │
                    │                        │                                 │
   浏览器 ──────────┼─► :80/:443 (HTTP/SNI) ─┼─► 路由表 ─► 每个用户连接开一条  │
                    │                        │              yamux stream       │
   数据库客户端 ────┼─► :20033 (TCP proxy) ──┘                                 │
                    │                                                          │
                    │   :7000 控制端口 ◄────── TLS 1.3 长连接（client 主动外连）│
                    └──────────────────────────────┬───────────────────────────┘
                                                   │
                                    （NAT / 防火墙，无需映射）
                                                   │
                    ┌──────────────────────────────┴───────────────────────────┐
                    │                    meatus-client（内网）                   │
                    │  控制会话 ── 心跳 / 注册 / 热重载                          │
                    │       │                                                  │
                    │       ├─ stream ─► 127.0.0.1:22      (SSH)                │
                    │       ├─ stream ─► 127.0.0.1:8080    (内部 Web)            │
                    │       └─ stream ─► 10.0.0.12:5432    (内网数据库)           │
                    └──────────────────────────────────────────────────────────┘
```

### 2.2 三个平面



| 平面  | 承载内容                                       | 实现要点                                                          |
| --- | ------------------------------------------ | ------------------------------------------------------------- |
| 控制面 | 握手、鉴权、proxy 注册、心跳、配置下发、状态上报                | 一条 TLS 连接上的**固定 control stream**（stream id 1），长度前缀 protobuf 帧 |
| 数据面 | 用户真实字节流 / UDP 报文                           | 同一 TLS 连接上按需开 yamux stream，**流首帧带元数据，之后是裸字节（零拷贝转发）**          |
| 管理面 | `/metrics`、`/healthz`、`/api/v1/status`、热重载 | 独立监听，**默认只绑&#x20;**`127.0.0.1:7400`，与业务端口物理隔离                 |

> 关键点：
>
> **控制面与数据面共连接、分 stream**
>
> 。好处是一条 TCP 只做一次 TLS 握手，隧道数量增加不增加公网端口消耗；坏处是底层 TCP 的队头阻塞会影响同连接所有流（见 2.4）。

### 2.3 一次完整请求的时序（TCP proxy）



```
client                          server                        外部用户
  │  TLS1.3 握手(校验证书)         │                               │
  │─── Hello{ver,client_id} ─────►│                               │
  │◄── Challenge{nonce_s} ────────│                               │
  │─── Auth{hmac(sha256(token),nonce_s)} ►│ 校验 → 绑定 client_id │
  │◄── Welcome{run_id,hb=15s} ────│                               │
  │─── NewProxy{tcp,20022,...} ──►│  端口池分配 → 开始 listen      │
  │◄── ProxyOK{port=20022} ───────│                               │
  │                               │◄──── TCP SYN ─────────────────│
  │◄── OPEN(stream) + StreamHeader│  分配 stream，写入 src_addr   │
  │─── connect 127.0.0.1:22 ──────│                               │
  │◄════════ 双向字节流（copy_bidirectional）════════════════════►│
  │─── 心跳 Ping/Pong（双向）─────│                               │
```

失败分支：`Auth` 失败 → `Error{code=AUTH_FAILED}` 并**立即关闭且延迟 3s**（防爆破速率探测）；端口占用 → `Error{PORT_IN_USE}`，client 记录并按配置降级到下一候选端口。

### 2.4 关键设计决策



| 决策       | 选择                             | 理由                                          | 放弃的方案                            |
| -------- | ------------------------------ | ------------------------------------------- | -------------------------------- |
| 传输       | TCP + TLS 1.3 + yamux          | 国内 UDP 常被 QoS / 封禁，QUIC 失败率高；TCP 穿透率最高、排障简单 | 首发 QUIC：握手快但弱网 / 企业网失败多，且生态排障工具少 |
| 多路复用     | yamux                          | 成熟（Rust 原生、被 libp2p 使用）、窗口流控、支持优先级          | 自研帧复用：实现与调优成本高；HTTP/2：语义不匹配      |
| 控制帧编码    | protobuf（prost）                | 可演进、跨语言（未来 Go/Python client 可行）、体积小         | JSON：体积与解析开销大；bincode：版本兼容差      |
| 数据流      | 首帧 protobuf 元数据 + 裸字节          | 避免每包解析，转发路径可用 `copy_bidirectional`          | 全程帧化：多一次内存拷贝                     |
| 鉴权       | HMAC-SHA256 挑战应答 + TLS         | token 不落地明文、防重放；不引入 JWT 复杂度                 | 直接明文传 token；JWT：需要额外密钥轮换         |
| 端口分配     | 服务端端口池 + 位图分配                  | 避免用户手工配端口冲突                                 | 用户固定端口：冲突频发                      |
| 配置       | TOML + 支持环境变量覆盖                | Rust 生态标准、可读性好                              | YAML：缩进陷阱；命令行参数：proxy 多了不可维护     |
| 单 / 双二进制 | 单二进制子命令 `meatus server/client` | 分发简单（一个文件），符合 "轻量" 定位                       | 两个 crate 两个产物：分发与版本对齐麻烦          |

**已知代价（写进风险）**：yamux 共享底层 TCP，一条流的阻塞会拖慢同连接其他流（head-of-line）。缓解：为 HTTP 等短请求场景引入**独立传输连接池**（M3 可选，按 `proxy` 分连接），或 v2 用 QUIC 多连接。

### 2.5 完全开源自托管

**产品形态**：Meatus 是完全开源（Apache-2.0）的自托管内网穿透工具。官方只交付一种形态：**单机 server + TOML + 本地 token 哈希表**，零外部依赖；不提供闭源商业版、自助注册、计费或独立控制面。

**开源范围**：

- `meatus-server` / `meatus-client` / 协议 / CLI / Web 面板 / Docker / systemd / 安装脚本全部在公开仓库，按 Apache-2.0 分发。
- 面板由 server 自带并通过 `rust-embed` 嵌入，不依赖私有源；面板依赖的第三方组件在 `THIRD_PARTY_NOTICES.md` 中声明。
- 不建 `meatus-ctrl`、不接 PostgreSQL / Redis、不做 token 签发服务与用量计费。
- 第三方可以基于 Apache-2.0 代码自行搭建多租户平台或托管服务，但那是第三方衍生形态，不属于本项目官方路线图。

**保留的五个扩展接缝（用于开源扩展，不代表商业控制面承诺）**：

| 接缝 | V1 实现 | 可选扩展实现 |
|---|---|---|
| `AuthProvider` | 本地 token 哈希表 | 外部鉴权服务 / 企业 SSO |
| `ConfigSource` | TOML 文件 + SIGHUP | 外部配置中心 / 增量下发 |
| `UsageSink` | Prometheus 指标 | 外部计量与审计系统 |
| `NameAllocator` | 本地端口池 + 静态域名 | 多 relay 全局分配 |
| `QuotaPolicy` | 全放行（可配上限） | 按租户 / 套餐限额 |

**V1 必须遵守的 6 条低成本约束**：

1. 所有标识使用 `tenant_id / client_id / proxy_name` 三级命名空间；V1 固定 `tenant_id = "default"`，禁止把 proxy name 写成全局唯一键。
2. 端口池、域名表、限流器按 `tenant_id` 分区，即使当前只有一个租户。
3. 业务代码不得直接读 token 配置，只能经 `AuthProvider`。
4. `Stats` / 用量统一走 `UsageSink`，V1 落 metrics。
5. 管理 API 一律 `/api/v1/*`，URL 与响应保留 `tenant` 维度。
6. relay 内禁止可变的 `static GLOBAL` 单例（metrics registry 除外），所有状态挂在 `ServerState`。

**边界**：这些接缝是为可维护性和社区扩展保留的，不会在 V1 增加 ctrl、数据库、计费或多租户 UI。官方支持范围以 `README.md`、`docs/INSTALL.md` 和本规划为准。


***

## 三、协议设计（`MEATUS/1`）

### 3.1 协商与版本



* `protocol_version: u16`，当前 `1`。server 只接受 `1.x`，`major` 不匹配则回 `Error{UNSUPPORTED_VERSION}` 并附 `min_supported`。

* 握手顺序固定：`Hello → Challenge → Auth → Welcome`，任一步超时 15s 断开。

* `capabilities: repeated string`（如 `udp`、`http`、`socks5`、`compress`），用于未来灰度；server 可拒绝不支持的 proxy 类型而不是断连。

### 3.2 控制消息（`proto/meatus.proto` 摘要）



```
syntax = "proto3";
package meatus.v1;

message Hello {
  uint32 protocol_version = 1;
  string client_version   = 2;   // semver，便于灰度与问题定位
  string client_id        = 3;   // 可选，未注册为空
  repeated string caps    = 4;
  string tenant_id        = 5;   // V1 固定 "default"；保留字段供扩展实现使用
  string token_id         = 6;   // token 的公开标识（不含密钥），保留字段
}
message Challenge { bytes nonce = 1; }              // 32B 随机
message Auth      { bytes hmac  = 1; }              // HMAC-SHA256(key=sha256(token), nonce)
message Welcome   {
  string run_id    = 1;
  uint32 heartbeat_secs = 2;   // M1 为 15s
  int64  server_time_ms = 3;
}

message NewProxy {                                  // client → server
  string name        = 1;    // 全局唯一
  ProxyType type     = 2;    // TCP | UDP | HTTP | HTTPS | SOCKS5
  string local_addr  = 3;
  uint32 remote_port = 4;    // 0 = 让服务端从端口池分配
  repeated string custom_domains = 5;
  ProxyOptions options = 6;  // 限速、超时、压缩、鉴权等
}
message ProxyOK   { string name = 1; uint32 remote_port = 2; repeated string domains = 3; }
message RemoveProxy { string name = 1; }

message Ping { uint64 seq = 1; int64 ts_ms = 2; }
message Pong { uint64 seq = 1; int64 ts_ms = 2; }

message Stats {                                     // 每 10s 上报一次
  string proxy_name = 1;
  uint64 bytes_in   = 2;
  uint64 bytes_out  = 3;
  uint32 active_conns = 4;
}

message Error { uint32 code = 1; string message = 2; bool fatal = 3; }

enum ProxyType { TCP = 0; UDP = 1; HTTP = 2; HTTPS = 3; SOCKS5 = 4; }
```

**数据流首帧&#x20;**`StreamHeader`：`proxy_name`、`kind`（USER\_CONN / UDP\_SESSION / HTTP\_REQ）、`src_addr`、`dst_hint`、`created_ms`、`trace_id`。之后该 stream 内全部是裸字节。

### 3.3 帧与背压



| 参数       | 值                                       | 说明                             |
| -------- | --------------------------------------- | ------------------------------ |
| 控制帧上限    | 64 KiB                                  | 超限直接断连（防内存打爆）                  |
| 编码       | `LengthDelimitedCodec`（4B 长度 + payload） | prost 解码，未知字段忽略（向前兼容）          |
| 数据流      | 不设帧，依赖 yamux 窗口                         | yamux 默认窗口 256 KiB，可按 proxy 调大 |
| 背压       | 全链路 `poll` 驱动，不落盘不排队                    | 任一环节慢，向上游施加读压，杜绝无界缓冲           |
| 单流空闲超时   | 300s（可配）                                | 到点双向关闭，防僵尸流占内存                 |
| 单连接最大并发流 | 默认 4096                                 | 超限新用户连接直接 RST，避免雪崩             |

### 3.4 会话与流状态机



```
DISCONNECTED ──connect──► TLS_HANDSHAKE ──ok──► AUTHENTICATING ──ok──► READY
     ▲                          │fail                │fail                │
     │                     backoff                    │             heartbeat ×3 miss
     └───────────────────────◄────────────────────────┴───────────────────┘

stream: OPEN ─► (首帧写入) ─► TRANSFERRING ─► HALF_CLOSED ─► CLOSED
                                     └─ idle 300s / error ─► ABORTED
```

重连策略：`1s → 2s → 4s → 8s → 16s → 30s`（上限），每档叠加 ±20% 抖动；连续失败 10 次写入 WARN 并保持服务不退出（systemd 不需要反复拉起）。**重连成功后全量重注册 proxy**（服务端不保留旧 run 的路由）。

### 3.5 错误码



| 码    | 名称                     | 语义               | 是否 fatal      |
| ---- | ---------------------- | ---------------- | ------------- |
| 1001 | `AUTH_FAILED`          | token 或 HMAC 不匹配 | 是             |
| 1002 | `UNSUPPORTED_VERSION`  | 协议大版本不符          | 是             |
| 1003 | `RATE_LIMITED`         | 握手过于频繁           | 是（延迟重连）       |
| 2001 | `PORT_IN_USE`          | 指定端口被占           | 否，仅该 proxy 失败 |
| 2002 | `PORT_POOL_EXHAUSTED`  | 端口池耗尽            | 否             |
| 2003 | `DOMAIN_CONFLICT`      | 域名已被其他 client 占用 | 否             |
| 3001 | `LOCAL_CONNECT_FAILED` | client 连本地服务失败   | 否             |
| 3002 | `STREAM_LIMIT`         | 并发流超限            | 否             |
| 4001 | `INTERNAL`             | 服务端内部错误          | 视情况           |

错误一律带 `proxy_name`，日志与指标按 proxy 维度聚合 —— 这是排障时最常用的切面。



***

## 四、功能实现方案

### 4.1 TCP 端口映射（M1）



* server：`TcpListener::bind(0.0.0.0:port)`；accept 后向对应 client 开 stream，写 `StreamHeader`，随后 `tokio::io::copy_bidirectional`。

* 透传 `src_addr`（通过 `X-Forwarded-For` 语义只写进 header，不改字节流）。

* 连接数上限按 proxy 维度限制（默认 1024）。

* 优雅退出：收到 `SIGTERM` 停止 accept → 等在途流 ≤ 30s → 关闭。

### 4.2 UDP（M2）

UDP 无连接，不能一会话一流（否则流数量爆炸）。方案：



* 每个 UDP proxy 在 server 上绑定一个 UDP socket，**只开一条专用 yamux stream 承载该 proxy 的全部报文**。

* 报文帧化：`{ session_id: u32, payload }`（服务端维护 `src_addr → session_id` 映射，空闲 60s 回收，上限 2048 会话 /proxy）。

* client 侧按 `session_id` 建 `UdpSocket` 并与目标通信；目标返回的报文按同一 `session_id` 回写。

* 不支持 multicast /broadcast（明确写进文档）。

**M2 实现口径（2026-11-16）**：

* server 为每个 UDP proxy 绑定 `UdpSocket`，只开一条 `StreamKind::UdpSession` 的 yamux 流；首帧 `StreamHeader` 后只收发 `UdpPacket`。
* `src_addr → session_id` 与 `session_id → (addr,last_seen,bytes)` 双索引；新来源分配递增 u32，空闲 60s（`options.idle_timeout_s` 可调，夹在 15–3600s）回收，默认上限 2048 会话/proxy（`options.max_conns` 可调）。回收时经 `UsageSink` 记一条会话用量。
* client 首次见到 `session_id` 时建 `UdpSocket::bind(0.0.0.0:0)` 并 `connect(local_addr)`，每个会话一个回包读循环；目标回包按同一 `session_id` 写回隧道。
* 单包上限 1500B，超限在两端丢弃；限速令牌桶对 UDP 双向按字节延迟，不丢包。
* 集成测试覆盖本机 UDP 回显往返；公网 DNS 成功率 ≥ 99% 仍需真实链路压测。

### 4.3 HTTP 域名路由（M3）

**M3 实现口径（2026-11-16，已落地）**：

- 不引入 axum/hyper 请求对象；`:80` 单 listener 直接读取请求头到 `\r\n\r\n`，上限 8 KiB，解析请求行与 `Host`。
- 路由 key 为 `http:<domain>`：先精确域名，再最长后缀泛域名；精确与泛域名可共存且精确优先，仅两个泛域名互相覆盖判 `DOMAIN_CONFLICT`；未知 Host 返回 404。
- 命中后开 yamux stream，写 `StreamHeader{kind: HttpReq, dst_hint = Host}`，随后把**已读到的原始请求字节**加后续双向字节原样透传；请求体、chunked、gzip、WebSocket Upgrade 均不单独解析，大文件不整体缓冲。
- 每路由一个 `max_conns` 信号量（默认 1024），超限返回 503（HTTP）或直接断开（HTTPS）；metrics 的 active/bytes 与 `UsageSink` 均按 route 记账。
- `http.domain` 目前只做配置校验，不参与兜底；实际域名来自客户端 `custom_domains`。不注入 `X-Forwarded-*`，只保留客户端原样请求头。


* server 上 `:80` 由单一 HTTP 入口承接，**按&#x20;**`Host`**&#x20;精确匹配 → 泛域名匹配 → 404**。

* 走 hyper/axum 的 service 层，把请求 "降级" 成一条 yamux stream：


  * 写 `StreamHeader{kind: HTTP_REQ}` + `method/path/headers`（保留原始 Host 与 `X-Forwarded-*`）；

  * 请求体**流式**写入，不做整体缓冲（大文件上传不爆内存）；

  * 响应头先回，响应体流式回传；`Upgrade: websocket` 命中后转为纯双向透传。

* 支持 `gzip` 透传、`Transfer-Encoding: chunked`；不做内容改写（避免破坏签名类应用）。

* Host 冲突：先到先得 + 配置里显式声明优先；冲突返回 `DOMAIN_CONFLICT` 并在 server 日志告警。

### 4.4 HTTPS 与 ACME（M3）

> **M3 当前边界（2026-11-16）**：`https` listener 先读 ClientHello 的 SNI；若该域名由 `type = "http"` 注册且 `acme.enabled = true`，server 用 `rustls-acme` 按域名懒创建 ACME 状态机，HTTP-01 挑战共用 `:80`，签发后 **TLS 终止**，再把明文 HTTP 交给本地 `type = "http"` 服务；未命中 ACME 域名时仍走 `type = "https"` 的 **SNI 透传**。当前只接入 **HTTP-01 + 精确域名**；TLS-ALPN-01 与泛域名 DNS-01 未接入，泛域名继续走自带证书 / SNI。staging 目录映射、0600 缓存、失败保留旧证书已实现，但 staging 与生产各签发一次、Pebble 续期仍待验收。

三种模式，**默认 (c) 自动 ACME**，(a) 作为兜底：

| 模式 | 做法 | 适用 |
|---|---|---|
| (a) SNI 透传 | server 用 `LazyConfigAcceptor` 读出 ClientHello 的 SNI，按域名选 client，之后**原样透传 TLS 字节**（server 不持私钥） | 用户自带证书、端到端加密 |
| (b) TLS 终止 | server 持证书解密，再把明文 HTTP 交给 client | 不想自己管证书、又不走 ACME |
| (c) 自动 ACME | server 用 `rustls-acme` 申请、缓存、自动续期 | **默认**，零手工 |

**ACME 实现要点**

- 密钥分存：账户密钥 `acme/account.key`（0600）、证书 `acme/certs/<domain>.crt|key`；只落盘 + 内存缓存热加载，不进日志。
- 挑战方式按域名形态选：
  - 单域名 → **HTTP-01**：占用 80 的 `/.well-known/acme-challenge/`，与 HTTP 入口共用 listener，先匹配挑战路径再走 Host 路由；
  - 单域名备选 → **TLS-ALPN-01**：在 443 识别 `acme-tls/1`，与 SNI 透传共存；
  - 泛域名 → **必须 DNS-01**，需 DNS 厂商 API（先支持 Cloudflare / 阿里云 / DNSPod）；没有 API 就降级为"自带证书"，不静默失败。
- 续期：剩余 30 天触发，抖动 ±6h；失败重试 3 次（1h / 6h / 24h），**失败绝不清空旧证书**，面板与日志双告警。
- 首次上线给 `acme.staging = true`（Let's Encrypt staging），避免把生产配额打光。
- 速率前置拦截：Let's Encrypt 每注册域名每周 50 张、同 set 5 张；签发前查本地近 7 天记录，命中就复用现有证书。

> 80/443 被其他服务占用时 ACME 直接不可用——`meatus check` 必须在启动前探测并明确报错，而不是等用户翻日志。

### 4.5 SOCKS5 反向代理（M4）



* server 侧暴露 SOCKS5 端口，完成 `CONNECT` 协商后，把目标地址写进 `StreamHeader`。

* client 侧**从内网发起**到目标地址的连接 —— 这样 "公网进来 → 内网出网" 方向正确，可访问内网任意主机。

* 仅支持 `CONNECT`（TCP），不支持 `BIND` / `UDP ASSOCIATE`（v2 再评估）。

* **默认关闭**（D5）：`[socks5] enabled = false`，显式开启后才监听；开启后目标地址默认走白名单，可配置 `allow_cidrs`。

**M4 实现口径（2026-11-16）**

* 服务端绑定 `[socks5] bind`，握手 `05 00`，只接受 `CONNECT`；支持 IPv4 / IPv6 目标，`ATYP=3` 域名目标明确拒绝，避免 DNS rebinding 绕过 CIDR。
* `allow_cidrs` 为空时使用默认内网集合（`10/8`、`100.64/10`、`127/8`、`169.254/16`、`172.16/12`、`192.168/16`、`::1/128`、`fc00::/7`、`fe80::/10`）；显式配置则整体替换默认集合。CIDR 拒绝回 `0x02`，无路由 / 限流 / 打开隧道失败回 `0x01`。
* 目标写入 `StreamHeader.dst_hint`；client 从内网侧连接前用同一份 `allow_cidrs` 再做一次校验，服务端与 client 双重约束。
* 当前 `Socks5Registry` 只允许一个全局路由；第二个 `type = "socks5"` proxy 会被拒绝。多 client / 多租户需后续按 tenant、SOCKS5 用户名或独立端口定义选择策略，M4 单路由验收。
* SOCKS5 proxy 的 `remote_port` 必须为 0，`custom_domains` 必须为空；`local_addr` 仅作占位，实际目标来自请求。`[socks5] enabled = false` 时注册返回错误。
* 管理面：`/api/v1/status` 输出 `status`、`uptime_s`、`clients[]`、`proxies[]`、`totals`；proxy 条目含 `run_id`，避免重连时旧会话误删新会话；计数来自 `ProxyMetrics::snapshot()`。`/metrics` 暴露 `meatus_client_connected`、`meatus_proxy_active`、`meatus_stream_total`、`meatus_bytes_total{direction}` 等核心指标。
* 配置 `admin.admin_token` 后，除 `/healthz` 外 `/api/v1/status` 与 `/metrics` 均要求 `Authorization: Bearer`，缺失 / 不匹配返回 401；默认 loopback 未配 token 时开放。
* `meatus status --admin http://127.0.0.1:7400 [--token ...]` 直接 GET `/api/v1/status`，输出 clients / proxies 表格；`--token` 也读 `MEATUS_ADMIN_TOKEN`。

### 4.6 端口池与域名



* 配置 `port_range = [20000, 30000]`，位图 + 游标轮转分配；释放即回收（带 10s 冷却防抖）。

* 用户指定 `remote_port` 优先，冲突则 `PORT_IN_USE`（不静默改配，避免用户以为生效了）。

* 域名：`*.tunnel.example.com` 泛域名解析到 server；HTTP/HTTPS 共用，无需端口。

### 4.7 限速与统计（M2）



* 每 proxy 独立令牌桶（`bytes/s`，默认不限）；超限**不丢包**，只做读侧延迟（保持 TCP 语义）。

* 统计：`bytes_in/out`、`active_conns`、`conn_total`、`local_connect_fail`，经 `Stats` 每 10s 上报，服务端聚合并暴露到 `/metrics`。



### 4.8 Web 管理面板（M5）

- **形态**：`web/` 下 Vue3 构建，产物 `web/dist` 由 `rust-embed` 编译进 server 二进制；`meatus server` 起来即访问 `http://127.0.0.1:7400/`，**不依赖 Node、不额外部署**。
- **技术栈**：Vue 3.5 + TypeScript 6.x + Vite 8 + Sass 1.108 + vue-tsc 3.3（与 `sailon-frame` 的测试矩阵对齐，Node ≥ 20.19）。
- **组件体系**：**全部 UI 来自 `@sailonix/ui`，按 sailon-dev 技能口径接入**——子路径按需导入（`@sailonix/ui/button` + `@sailonix/ui/button/style`），图标用 `@sailonix/icon`，主题用 `@sailonix/ui/config-provider` + `var(--sl-*)` 语义 token；**禁止引入 Element Plus / Vant 等第三方 UI 框架，禁止自造基础组件**。

**六个页面与对应组件**（不改 sailon 组件源码，只用其公开 props/slots）：

| 页面 | 主要组件 |
|---|---|
| 登录 | `card` / `form` / `input` / `button` / `alert` |
| 总览 | `page-header` / `stat-cards` / `line-chart-card`（visual）/ `status-tag` |
| proxy 列表与增删改 | `data-table` 或 `pro-table` / `dialog` / `form` / `select` / `number-field` / `switch` / `popconfirm` |
| 证书状态 | `table` / `status-tag` / `timeline` / `alert` |
| 实时日志 | `code-block` / `card` / `switch`（自动滚动） / `empty-state` |
| 系统设置 | `descriptions` / `panel` / `typography` |

- **业务组件**：确实需要组合封装时放 `web/src/components/`，命名 `Mt<Name>`、类名 `.mt-*`、样式只引 token；**不回写 `packages/ui`**，也不在 meatus 仓内改 sailon 源码。
- **取包方式（2026-11-18 起）**：`@sailonix/ui` / `@sailonix/icon` 从 npm 中央仓库安装，版本 `^0.1.0` 由 `web/package-lock.json` 锁定；旧 `web/vendor/*.tgz` 已移除。CI / Docker 直接 `npm ci`，不依赖私有源或仓库外路径。
- **认证**：`admin_token` 登录换 HttpOnly + SameSite=Strict 会话 Cookie；写操作校验 CSRF token；连续失败 5 次锁 10 分钟。
- **边界**：面板只调本机 `/api/v1/*`，不直连数据库；V1 只有 server 自带面板，不规划独立 ctrl 面板。
- **安全**：CSP 禁内联脚本、`X-Frame-Options: DENY`、输出全转义；面板监听与业务端口物理隔离。

> **M5 实现口径（2026-11-16，已完成）**
>
> `web/` 为 Vue 3 + TS + Vite 8 六页面，UI 全部来自 `@sailonix/ui` 子路径按需导入；`web/dist` 经 `rust-embed` 编入 `meatus-server`，`admin.panel=true` 时 `GET /` 返回内嵌 index，运行期不依赖 Node。
>
> - **认证**：`POST /api/v1/session` 用 `admin_token` 换 `meatus_session` HttpOnly + SameSite=Strict Cookie（12h）；`GET /api/v1/session` 返回 `{authenticated,csrf,mode}`；`DELETE /api/v1/session` 与所有写接口校验 `X-Meatus-CSRF`。失败按 IP 5 次锁 10 分钟，第 6 次 429；token 常量时间比较。
> - **读接口**：`GET /api/v1/status`、`/settings`、`/certificates`、`/logs?limit=N`、`/metrics`；有 token 时 Cookie / Bearer 均可，未授权统一 401 JSON；`/healthz` 公开。
> - **proxy 增删改**：`POST` 新增、`PUT` 同名替换、`DELETE` 删除；body 含 `run_id/name/type/local_addr/remote_port/custom_domains/options`。server 经 `AdminBus` 按 `run_id` 路由管理帧，等待 client ack（5s），失败回滚到上一份 runtime；admin 投递超时 3s、整体 15s，错误映射 404/409/504/400。
> - **写权限**：无 token 的 loopback 直接允许；Bearer 直接允许；Cookie 会话必须带 `X-Meatus-CSRF`，否则 403；未授权 401。
> - **校验**：name `[A-Za-z0-9_.-]{1,64}`、`local_addr ≤255`、`remote_port ≤65535`、域名 ≤16 且去重小写、`max_conns ≤4096`、`idle_timeout_s ≤3600`、`allow_cidrs ≤16`；HTTP/HTTPS 必须有域名且 `remote_port=0`，SOCKS5 禁止域名且 `remote_port=0`，TCP/UDP 禁止域名。
> - **证书解析**：`/api/v1/certificates` 扫描 `acme.cert_dir/cert-*.pem`，用 `rustls_pemfile` 取叶证书、`x509-parser` 解析 SAN / `not_after` / serial / issuer / 链长度；按域名匹配 proxy，状态 `managed|expiring(≤30天)|expired|pending|not-configured`，来源 `acme|cache|manual`，challenge `http-01|none`，并输出 `cached_count`。面板展示真实到期、剩余天数、序列号、签发者与链长度。
> - **日志与设置**：日志页显示 `共 total 行 · 当前显示 lines 行`；设置页新增会话（ttl、Secure、CSRF、登录锁定）与写接口（方法、proxy CRUD）面板。
> - **安全响应头**：`X-Frame-Options: DENY`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、CSP `script-src 'self'`；index no-cache，带 hash 资产 immutable；`admin.panel=false` 时静态资源 404。
> - **验收**：`cargo test --workspace` 79 passed / 1 ignored，含 `admin_proxy_crud_requires_csrf_and_updates_client` 与证书解析单测；`cargo clippy --workspace --all-targets -- -D warnings` 通过；`npm run typecheck` / `npm run build` 通过；浏览器逐页复验登录、总览、Proxy、证书、日志、设置，Proxy 操作列 sticky 后可完整点击。
> - **吞吐验收**：`ab` 经 TCP proxy 拉取 5 MiB 静态文件（20 次 × 4 并发），`admin.panel=true` 5 轮中位 341,266 KB/s，`panel=false` 5 轮中位 338,356 KB/s，差异 +0.86%，满足「面板开关吞吐差 ≤5%」。
- **ZAP baseline**：早期环境拉取 ZAP 镜像不可达，曾用 `scripts/security-smoke.sh` 的 9 项作为替代证据；M6 已完成完整 ZAP baseline，结果 `FAIL-NEW: 0`、`WARN-NEW: 5`、`PASS: 63`，详见 `docs/M6_ACCEPTANCE.md`。
- **发布前遗留**：非 loopback 面板 HTTPS 与 Cookie `Secure` 未实现，当前只支持 loopback HTTP；Docker 镜像与 musl 静态产物已构建并实跑。

***

## 五、安全设计

### 5.1 链路



```
client ──TLS1.3(mTLS可选, SPKI pin)──► server ──► 业务端口
         └ HMAC 挑战应答（token 不裸传）
```



* TLS：默认启用，`rustls`（无 OpenSSL 依赖，静态编译友好）；**M1 仅启用 TLS 1.3，协议版本写死、不可降级**；套件走 AEAD（ring provider）。

* 证书校验：默认用系统根证书验证 server 域名；自签 / 私有 CA 场景用 `client.server_ca_cert` 指定根证书。`server_public_key_pin`（SPKI SHA-256，`sha256:<64hex>` 或裸 64 hex）M2 已实现：先正常校验证书链，再常量时间比对叶子证书 SPKI 摘要；**永不提供** `insecure_skip_verify`。

* token：`meatus gen-token` 生成 32 字节随机 base64；服务端只存 `sha256(token)`，并把这个哈希同时作为 HMAC-SHA256 密钥。token 本身是高熵随机值，SHA-256 足以抗离线猜测，无需加盐慢哈希（慢哈希只会拖慢每次握手）。

* 授权粒度：token 绑定 `client_id` + 允许的 proxy 名单 / 端口范围（V1 由服务端配置下发）。

### 5.2 十条安全铁律（实现时必须逐条对照）

1. 所有对外连接默认 TLS，明文模式需显式配置且启动告警。
2. token 绝不出现在日志、指标标签、错误信息里（统一脱敏函数）。
3. 管理端口与面板默认只绑 `127.0.0.1`；绑 `0.0.0.0` 必须同时配 `admin_token` 并强制 HTTPS。
4. 控制帧 ≤ 64 KiB；HTTP header 总量 ≤ 8 KiB；UDP 单包 ≤ 1500B；面板 JSON body ≤ 1 MiB。
5. 握手失败延迟 3s 再断，同 IP 每分钟最多 10 次握手（滑动窗口限流）。
6. 端口池只允许配置范围内分配，禁止借用系统端口（< 1024）。
7. 不在 `unsafe` 里处理协议数据；解析全部走安全 Rust，`#![forbid(unsafe_code)]`。
8. 依赖审计进 CI（`cargo audit` + `cargo deny`），有 RUSTSEC 高危即阻断合并。
9. 日志与面板展示一律脱敏：token / Authorization / Cookie / ACME 密钥路径必须打码。
10. 每个里程碑做一次威胁自检（表 5.3），新增功能必须补一行威胁与对策。

### 5.3 威胁与对策

| 威胁 | 影响 | 对策 |
|---|---|---|
| 暴力猜 token | 隧道被劫持 | 32B 随机 + 限流 + 失败延迟 + 服务端哈希存储 |
| 中间人 | 流量窃听 / 篡改 | TLS 1.3 + 证书校验 / SPKI pin |
| 重放 Auth 帧 | 绕过鉴权 | 每次连接随机 nonce，HMAC 绑定 nonce |
| 域名劫持（Host 欺骗） | 把请求打到别的 client | Host 严格白名单匹配，未知 Host 直接 404 |
| 端口扫描 / 资源耗尽 | 打挂 server | 每 IP 连接数上限、全局流上限、accept 速率限制 |
| 内网横向（client 被攻陷） | 攻击者借隧道进内网 | TCP/HTTP/HTTPS proxy 只允许连 `local_addr`；SOCKS5 默认关闭，仅 IP 目标 + 服务端/client 双重 CIDR 白名单，拒绝域名目标 |
| 面板 XSS / CSRF | 劫持管理员会话、篡改隧道配置 | HttpOnly + SameSite Cookie、CSRF token、CSP、输出转义 |
| ACME 私钥 / 账户密钥泄露 | 可签发任意域名证书 | 文件 0600、独立目录、日志不打印密钥、面板只显示状态 |
| 域名被他人绑定到共享 relay | 借你的 relay 给他人域名签证书 | ACME 挑战天然要求控制权；共享平台需要额外做"域名归属验证" |
| 日志泄露凭据 | 凭据外流 | 统一 `redact()`；日志级别默认 info |

***

## 六、工程结构

### 6.1 Cargo Workspace

```
meatus/
├── Cargo.toml                  # [workspace] resolver="3"，统一版本与 lint
├── rust-toolchain.toml         # 固定 1.96（与开发机一致，避免 CI 漂移）
├── crates/
│   ├── meatus-proto/           # protobuf 定义 + build.rs + 帧编解码 + 错误码
│   ├── meatus-core/            # 传输抽象、TLS、yamux、ACME、状态机、配置、指标
│   ├── meatus-server/          # 监听器、路由表、端口池、HTTP/SNI/ACME 分发、鉴权、管理 API
│   ├── meatus-client/          # 会话维持、本地转发、proxy 生命周期
│   └── meatus-cli/             # 唯一二进制：server / client / gen-token / check
│                               # 非目标：不新增 meatus-ctrl；V1 只留 2.5 的扩展接缝
├── migrations/                 # 历史 ctrl 设计，V1 不实现
├── web/                        # Vue3 面板：src/{pages,components,api,styles}；UI 全用 @sailonix/ui
│                               # 构建产物 dist 由 rust-embed 编进 server；业务组件前缀 mt-
├── proto/meatus.proto
├── docs/                       # 本规划 + ADR
├── tests/e2e/                  # docker compose + netns + Pebble（ACME 测试）
├── benches/                    # criterion 基准
└── scripts/                    # 打包、systemd、dockerfile
```

**依赖关系**：`cli → server/client → core → proto`；不引入 ctrl。若第三方扩展控制面，应只依赖 `core/proto`，server 不依赖控制面库。
**构建顺序**：`web`（`npm ci && npm run build`）→ `cargo build`；`meatus-server/build.rs` 校验 `web/dist` 存在，缺失即编译失败（避免"面板 404 才发现没打包"）。

### 6.2 依赖清单（已核对 crates.io 当前版本）

| crate | 版本 | 用途 |
|---|---|---|
| `tokio` | 1.54（`rt-multi-thread,net,io-util,time,sync,macros,signal`） | 异步运行时 |
| `tokio-util` | 0.7 | `LengthDelimitedCodec`、`CancellationToken` |
| `tokio-rustls` / `rustls` | 0.26 / 0.23 | TLS 1.3（`LazyConfigAcceptor` 做 SNI） |
| `rustls-pemfile` | 2.2 | 证书读取 |
| `rustls-acme` | 0.15 | ACME 客户端（HTTP-01 / TLS-ALPN-01 / DNS-01） |
| `yamux` | 0.14 | 多路复用 |
| `prost`（+`prost-build`） | 0.14 | 控制帧编解码 |
| `quinn` | 0.11（v2 才启用） | QUIC 可选传输 |
| `bytes` | 1.12 | 零拷贝缓冲 |
| `serde` + `toml` | 1.0 / 1.2 | 配置 |
| `clap` | 4.6（`derive`） | CLI |
| `tracing` + `tracing-subscriber` | 0.1（`json,env-filter`） | 结构化日志 |
| `metrics` + `metrics-exporter-prometheus` | 最新 | 指标 |
| `axum` | 0.8 | HTTP 路由 + 管理 API |
| `axum-extra` | 0.12 | Cookie / typed header（面板会话、CSRF） |
| `tower-http` | 0.7 | trace / compression / CSP header |
| `rust-embed` | 8.14 | 面板静态资源嵌入 |
| `sqlx` | 0.9（`postgres, runtime-tokio-rustls, migrate, uuid, time`） | ctrl 数据访问与迁移 |
| `redis` | 1.8（2.0 仍是 RC，不跟进） | 会话、限流、token 撤销缓存 |
| `tower-sessions` + `tower-sessions-redis-store` | 0.15 / 0.16（以 cargo 解析为准） | 历史 ctrl 扩展，V1 不引入 |
| `uuid` | 1.29 | 主键 |
| `socket2` | 0.6 | `TCP_NODELAY` / `SO_REUSEPORT` |
| `hmac` + `sha2` + `argon2` | — | 鉴权 |
| `rcgen` | 0.14 | `gen-cert` 自签工具 |
| 前端：`vue` / `vite` / `typescript` / `vue-tsc` / `sass` | 3.5 / 8.3 / 6.x / 3.3 / 1.108 | 面板，版本与 `sailon-frame` 对齐 |
| 组件库：`@sailonix/ui` / `@sailonix/icon` / `@sailonix/visual` | 0.1.0（npm 中央仓库，MIT） | 面板全部 UI 与图表；按子路径按需导入 |
| `cargo-fuzz` / `criterion` | — | 测试与基准（dev） |

### 6.3 质量门禁



* `cargo fmt --check`、`cargo clippy -- -D warnings`、`cargo test`（CI 三件套）。

* 前端：`vue-tsc --noEmit`、`vite build`、ESLint；`web/src` 内禁止十六进制色值，样式只引 `var(--sl-*)`。

* 组件边界：依赖树中出现 `element-plus` / `vant` / 其他第三方 UI 框架即 CI 失败；`@sailonix/*` 版本必须锁定（`package-lock`），不允许 `latest`。

* `#![forbid(unsafe_code)]` 全 crate 生效。

* 覆盖率目标：`core`/`proto` ≥ 80%（`cargo-llvm-cov`），`server` ≥ 60%。

* 每个 PR 必须带：修改点、影响面、回滚方式（PR 模板写死）。



***

## 七、配置模型

### 7.1 `server.toml`

```toml
[server]
bind_addr    = "0.0.0.0"
control_port = 7000

[tenant]
default = "default"            # V1 单租户；保留字段供扩展实现使用，relay 不落库

[auth]
mode     = "hmac"              # hmac | mtls
provider = "local"             # local = 本地文件；remote 为历史扩展设计，V1 不实现
# token_sha256 = "…"           # 单客户端简写；多客户端请写到每个 client 里
clients = [
  # 每个客户端各自一把 token，服务端只存 sha256（`meatus gen-token` 输出）
  { id = "home-nas", tenant = "default", token_sha256 = "…", allow_ports = [20000, 20100] },
]

[ports]
range = [20000, 30000]

[http]
enabled    = true
bind       = "0.0.0.0:80"
https_bind = "0.0.0.0:443"
domain     = "*.tunnel.example.com"

[acme]
enabled   = true
directory = "https://acme-v02.api.letsencrypt.org/directory"
email     = "ops@example.com"
challenge = "http-01"          # 单域名：http-01 | tls-alpn-01；泛域名强制 dns-01
cert_dir  = "/etc/meatus/acme"
# staging = true               # 首次联调走 staging，避免打光生产配额
# dns_provider = "cloudflare"  # 泛域名必填：cloudflare | aliyun | dnspod
# dns_api_token = "…"

[socks5]
enabled = false                # D5：默认关闭，显式开启
# bind = "0.0.0.0:2080"

[admin]
bind  = "127.0.0.1:7400"
panel = true                   # 静态资源已嵌入二进制
# admin_token = "…"            # 绑 0.0.0.0 时必填，且必须配 TLS

[ctrl]                         # 历史设计，V1 整段注释掉、不启用
# url      = "https://ctrl.internal:8443"
# relay_id = "relay-nj-01"
# mtls_cert = "/etc/meatus/relay.crt"

[log]
level  = "info"
format = "json"
```

> 自签/自带证书场景把 `[acme] enabled` 置 `false`，`type = "https"` 走 SNI 透传；开启 `[acme] enabled = true` 时需同时开启 `[http] enabled = true`（HTTP-01 使用 `:80`），且 `challenge = "http-01"`；M1 启动前强制校验 `tls_cert` / `tls_key` 同时存在。

### 7.2 `client.toml`



```
[client]
server_addr = "tunnel.example.com:7000"
server_name = "tunnel.example.com"   # TLS SNI/校验用
client_id   = "home-nas"             # 必须与服务端 clients[].id 一致
tenant      = "default"
token       = "…"
# server_ca_cert = "/etc/meatus/server.crt"  # 自签/私有 CA；公网 CA 留空用系统根
# server_public_key_pin 在 M1 未实现，配置即报错；固定信任请用 server_ca_cert
[log]
level = "info"

[[proxy]]
name       = "ssh"
type       = "tcp"
local_addr = "127.0.0.1:22"
remote_port = 20022

[[proxy]]
name        = "nas-web"
type        = "http"
local_addr  = "127.0.0.1:8080"
custom_domains = ["nas.tunnel.example.com"]

[[proxy]]
name       = "dns"
type       = "udp"
local_addr = "127.0.0.1:53"
remote_port = 20053
```

### 7.3 CLI



```
meatus server -c /etc/meatus/server.toml
meatus client -c /etc/meatus/client.toml
meatus check -c client.toml          # 校验配置 + 连通性 + 端口可用性，不启动
meatus gen-token                     # 输出 token 与哈希
meatus gen-cert --domain tunnel.example.com --out ./certs
meatus status --admin http://127.0.0.1:7400 # 打 /api/v1/status，表格输出
meatus status --admin http://127.0.0.1:7400 --token "$MEATUS_ADMIN_TOKEN"
```

热重载：`SIGHUP` → 重新解析配置 → 差量执行（新增 proxy 注册、删除 proxy 注销、改 token 需重连）；失败则保留旧配置并告警，**不允许 "半套配置" 生效**。



### 7.4 历史 ctrl 配置设计（非当前范围）

> 以下 ctrl 配置与数据模型是早期控制面预研，**不属于完全开源 V1**；保留仅作扩展参考，不实现、不排期。

```toml
[ctrl]
bind       = "127.0.0.1:8443"
public_url = "https://ctrl.example.com"

[database]
url = "${MEATUS_DATABASE_URL}"      # 缺变量启动即失败，不做空口令兜底
max_connections      = 20
min_connections      = 2
connect_timeout_s    = 5
statement_timeout_ms = 3000

[redis]
url        = "${MEATUS_REDIS_URL}"
pool_size  = 20
key_prefix = "meatus:"

[token]
hmac_secret_env = "MEATUS_TOKEN_HMAC_KEY"
access_ttl_s    = 900               # relay 侧缓存的上限，过期须回 ctrl 换发
refresh_ttl_s   = 86400

[quota]
default_max_proxies = 10
default_max_ports   = 5
default_rate_mbps   = 100

[relay]
register_ttl_s = 30
```

### 7.5 历史 ctrl 数据模型（非当前范围）

| 表 | 关键列 | 说明 |
|---|---|---|
| `tenants` | id, name, plan, status, created_at | 租户主表，`plan` 决定配额 |
| `users` | id, tenant_id, email, password_hash, role | 控制台登录；口令 `argon2id` |
| `api_tokens` | id, tenant_id, client_id, token_hash, scopes, expires_at, revoked_at | **只存哈希**，前缀留 8 位便于识别 |
| `clients` | id, tenant_id, name, relay_id, last_seen_at | 隧道客户端，`last_seen` 由 relay 心跳回写 |
| `domains` | id, tenant_id, fqdn, verify_token, verified_at | 唯一索引 `fqdn`；共享平台必须验证归属 |
| `port_allocations` | tenant_id, relay_id, port, proxy_name, allocated_at | 唯一索引 `(relay_id, port)` |
| `usage_records` | tenant_id, client_id, proxy_name, hour, bytes_in, bytes_out, conns | 按小时聚合，保留 90 天 |
| `relay_nodes` | id, addr, region, capacity, last_heartbeat | 由 relay 注册上报 |
| `audit_logs` | id, tenant_id, actor, action, target, ip, created_at | 管理面写操作全留痕 |

约束：所有业务表带 `tenant_id` 并建 `(tenant_id, …)` 复合索引；**跨租户查询在 SQL 与 API 两层都不允许**；迁移文件放 `migrations/`，用 `sqlx migrate` 管理，`meatus ctrl migrate` 单独可跑。

### 7.6 历史扩展开发环境（V1 不依赖）

> PostgreSQL / Redis 仅用于早期 ctrl 预研；完全开源 V1 运行时不依赖它们，下面的启动记录不构成 V1 安装要求。

| 依赖 | 容器 | 镜像 | 宿主地址 | 版本 | 状态 |
|---|---|---|---|---|---|
| PostgreSQL | `local-postgres` | `postgres:14.18` | `127.0.0.1:504` | 14.18 | 运行中 |
| Redis | `local-redis` | `redis:8.0.3` | `127.0.0.1:505` | 8.0.3 | 已启动（原为 Exited） |

- PG 使用本地开发账号，口令见本地 `.env`（不入库）；meatus 使用独立数据库，不与既有库混用、不跨库查表。
- Redis 本地**无密码**（`redis-cli ping` → `PONG`），仅限本机开发；生产必须 `requirepass` + 内网绑定 + 独立实例。
- 本地 `.env`（**必须 git 忽略**）：

```bash
MEATUS_DATABASE_URL=postgres://<user>:<password>@127.0.0.1:504/<database>
MEATUS_REDIS_URL=redis://127.0.0.1:505/0
MEATUS_TOKEN_HMAC_KEY=<openssl rand -hex 32 生成>
```

- `.env.example` 只放占位符；口令不进 `*.toml`、不进代码、不进日志；CI 用 secret 注入。
- 连通性自检：

```bash
docker exec local-postgres psql -U <user> -d <database> -c "select 1"
docker exec local-redis redis-cli ping
```

- 生产差异：本地 PG 14 与生产保持同大版本起步；本地 Redis 单实例无密码，**不能照搬到生产**。
- 现状提醒：两个容器的宿主端口当前绑在 `0.0.0.0`（局域网可见），建议改 `-p 127.0.0.1:504:5432` / `-p 127.0.0.1:505:6379`，仅本机可连。

***

## 八、里程碑与排期

### 8.1 里程碑总览（V1 单人 49 人日，完全开源自托管）

| 里程碑 | 交付内容 | 人日 | 验收标准（DoD） |
|---|---|---|---|
| **M0 脚手架** | workspace、proto、帧编解码、CLI、CI、错误码、五个接缝 trait 空实现 | 3 | `meatus --version` 可跑；帧编解码往返单测全绿；clippy/fmt 无告警；CI 5 分钟内通过 |
| **M1 TCP MVP** | server/client、TLS、HMAC 鉴权、单 TCP proxy、心跳、重连 | 7 | **公网 VPS + 家用机 SSH 穿透成功**；`iperf3` 单流 ≥ 裸链路 85%；kill -9 client 后 ≤ 30s 自动恢复 |
| **M2 多 proxy / UDP / 运维** | 多配置、端口池（按租户分区）、UDP 会话、限速、metrics、热重载、`UsageSink` | 8 | 10 个 proxy 在线；UDP DNS 成功率 ≥ 99%；`SIGHUP` 增删不断流；24h 压测内存无增长（< 40MB） |
| **M3 HTTP/HTTPS + ACME**（本机实现完成） | Host 路由、精确/泛域名、HTTP 原样流式、WebSocket 透传、SNI 透传、ACME HTTP-01 + TLS 终止、0600 缓存、per-route `max_conns`；待收口：TLS-ALPN-01、泛域名 DNS-01、真实 CA 验收 | 13（已用 12，剩 1） | 已过：本机泛域名/精确域名访问 echo、HTTP 404、本机 TLS 服务 SNI 透传、ACME 缓存 0600 单测、`cargo test --workspace`；待验收：WebSocket 1h、4GB 上传峰值 < 60MB、**staging 与生产各签发成功一次、Pebble 验证续期、签发失败保留旧证书** |
| **M4 SOCKS5 + 管理 API**（本机实现完成） | SOCKS5 反向（默认关）、`/api/v1/status`、`/metrics`、`meatus status` | 6 | 已过：curl 经 SOCKS5 访问 3 个内网端点、公网目标被 `0x02` 拒绝；`/api/v1/status` 无 token 401 / 带 Bearer 200；`meatus status` 与真实连接一致；`/metrics` 抓到 4 个核心指标；本机集成测试 + `cargo test --workspace` + clippy。待验收：Grafana 实际抓取面板 |
| **M5 Web 面板**（已完成） | `@sailonix/ui` 六页面、登录/CSRF、证书状态、日志 tail、资源嵌入 | 6 | 六页面可用；按需子路径导入、无第三方 UI 框架；无 Node 单文件启动；登录限流 / 未授权 401 / 安全响应头 |
| **M5.1 面板写操作收口**（已完成） | server→client 管理控制帧、proxy 增删改、证书到期解析、写操作统一 CSRF、面板吞吐回归 | 4 | 面板可增删改运行中 proxy 且失败回滚；无 CSRF 403 / 有 CSRF 成功；证书页显示 not_after / serial；面板开关吞吐差 +0.86%。ZAP baseline 已在 M6 完成（0 FAIL / 1 Medium / 4 Low），不再阻塞发布 |
| **M6 加固与发布（V1）**（已完成，2026-11-18） | `gen-cert`、SIGTERM、fuzz target、弱网脚本、安全冒烟、静态构建、Docker/systemd、发布流水线 | 6 | 已过：`gen-cert` 多 SAN / 0600 / `--force`；SIGTERM 优雅退出；SIGHUP 热重载；macOS + Linux musl 静态包且 sha256；Linux 安装 + 面板 200；`cargo test` 72 passed / clippy / fmt；安全冒烟 9 项 PASS；fuzz 双 target 5 分钟无 crash；netem 20% 丢包 20/20 + 重连 PASS；ZAP baseline 0 FAIL / 1 Medium / 4 Low；cargo audit 无漏洞；npm audit 0 漏洞。持续巡检：fuzz 24h（不阻塞 V1 发布） |
| **独立控制面（非目标，不排期）** | 不自建 ctrl、不做注册/计费/多 relay 调度；第三方如需可基于 Apache-2.0 自行扩展 | — | 不纳入 V1；官方不承诺控制面实现 |
| **M8（Post-V1，可选）** | QUIC 传输、P2P 打洞、多 client 负载均衡 | 10+ | QUIC 失败自动回落 TCP；打洞成功率可观测；同 proxy 双 client 轮询 |

> **V1 自建版可运行性验收（2026-11-16）**
>
> - `cargo build --release` 产出 **9.8MB 单二进制**；release server/client 建立 TLS + yamux 隧道，入口 `127.0.0.1:21000` → 内网 `127.0.0.1:19080` 的 HTTP 返回正常，面板 `GET /` 返回 200。
> - V1 运行零 PostgreSQL / Redis 依赖；`tenant_id` 固定 `default`，独立控制面为非目标，只保留扩展接缝，不建 ctrl、不做计费。
> - M6 已完成：`gen-cert`、SIGTERM、systemd / Docker / 安装脚本、静态发布流水线、安全冒烟、fuzz / 弱网脚本；musl 静态产物、Linux 安装与 Docker 镜像均已验证。弱网 20% 丢包 20/20、重连 PASS；ZAP baseline 0 FAIL；cargo audit 无漏洞；fuzz 5 分钟 × 2 target 无 crash。24h fuzz 作为持续巡检，不作为发布阻塞。

> **M6 实现口径（2026-11-18，验收完成）**
>
> - **退出与重载**：server/client 同时监听 Ctrl-C 与 `SIGTERM`，systemd `stop` 走优雅退出；client `SIGHUP` 重读配置做 proxy 差量，连接级字段变更提示重启。已用 release 包实跑 `SIGTERM`（两端日志 `shutdown requested`）与 `SIGHUP`（改 `remote_port` 后新端口可访问）。
> - **证书**：`meatus gen-cert` 用 `rcgen` 生成自签 ECDSA 证书，支持多个 `--domain`（DNS/IP）、`-o/--out`、`--force`；`server.key` 写后 0600。已实测 SAN 含 `DNS:localhost` + `IP:127.0.0.1`。
> - **发布**：`scripts/build-release.sh` 构建 Web + cargo release + strip + 打包 + sha256；2026-11-18 重建 macOS aarch64 包二进制 8,989KiB、sha256 `1644ea5aa08046c4718785e4656a2bd62958d7620817737fd00462a1fa8b6940`；Linux x86_64 musl 静态包二进制 9.8MiB、`static-pie linked`、tar 4.1MiB、sha256 `b8aa099a31a7373033d29e15b64956e909eb2a377ff8251119633ef6a00ae4b1`；包内含二进制、示例配置、systemd units、install 脚本、README、INSTALL、DEVELOPMENT_PLAN。`release.yml` 已配置 x86_64 / aarch64 musl 矩阵；当前 origin 为内部仓库，GitHub Release 托管侧未触发，本地流水线已复现。
> - **安装**：`scripts/install.sh` 面向 Linux/root/systemd，安装 `/usr/local/bin/meatus`、`/etc/meatus`、`/var/lib/meatus`，首启生成证书 / client token / admin token，保留已有配置；已在 Ubuntu 24.04 VM 实装并以 `meatus` 用户启动，`/healthz` 200、`/` 200、`/api/v1/status` Bearer 200、SIGTERM 优雅退出；修复了 `/etc/meatus/{server.toml,*.token}` 未 chown 给 meatus 导致服务读不到配置的问题。`deploy/systemd/*.service` 带 `NoNewPrivileges`、`ProtectSystem=strict`、`StateDirectory=meatus`；VM 非 systemd init，unit 未做运行态验证。
> - **容器**：多阶段 `Dockerfile`（node 构建面板 → rust musl 编译 → alpine 运行，非 root `meatus`）；`deploy/docker/{entrypoint.sh,docker-compose.yml}` 首启生成配置 / 证书 / token；面板容器内绑 `0.0.0.0:7400`，宿主机只发布 `127.0.0.1:7400`。`Dockerfile` 多阶段构建已实际产出 `meatus:1.0.0`（linux/arm64，30.5MB，非 root `meatus`）；修复了当时 `web/vendor` 未在 `npm ci` 前复制（2026-11-18 已改为 npm 中央仓库）、容器内 rustup 联网下载组件、crates.io 不可达三个构建问题，新增可选 `BASE_REGISTRY` / `CARGO_REGISTRY` 构建参数。容器首启生成 `/data/{server.toml,certs,admin.token,client.token}`，实测 `/healthz` 200、`/` 200、Bearer `/api/v1/status` 200，`docker stop` 日志 `shutdown requested`。Compose 命名卷与宿主机 bind mount 两种方式均实跑通过；同一镜像以 client 模式接入 server，`remote_port 21090` → 宿主 `host.docker.internal:19090` 返回 `docker-e2e`，server/client 均优雅退出。纯 `docker run` 再次实测：命名卷首启、显式 `--health-cmd` 后 `healthy`、面板 / Bearer API 200、client 容器无 healthcheck 误报、隧道返回 `docker-run`、两端优雅退出；新增 `scripts/docker-smoke.sh`。`docker compose config` 通过。镜像已推送到公开容器仓库（实际地址按发布环境替换），并在测试服务器用公网 7000 + macOS 外部 client 实测隧道返回 200；完整测试记录归档在内部资料目录，不随开源仓发布。
> - **安全与质量**：`scripts/security-smoke.sh` 对 `http://127.0.0.1:7400` 实跑 9 项全 PASS；`cargo audit` 扫描 249 个 crate，无漏洞，2 条无修复版本的 unmaintained 警告（`chrono 0.4.45` RUSTSEC-2026-0543、`rustls-pemfile 2.2.0` RUSTSEC-2025-0134）；`npm audit --omit=dev` 为 0 漏洞。ZAP baseline（`ghcr.io/zaproxy/zaproxy:stable`，spider + HTML/JSON/Markdown 报告）结果 `FAIL-NEW: 0` / `WARN-NEW: 5` / `PASS: 63`：0 High、1 Medium（CSP `style-src 'unsafe-inline'`）、4 Low（Non-Storable Content、Permissions Policy 缺失、Modern Web Application、COEP 缺失），均为已接受风险。fuzz targets `decode_control_frame` / `parse_config` 用 nightly 1.102 + cargo-fuzz 0.13.2 各跑 5 分钟：`decode_control_frame` 113,333,341 次、`parse_config` 3,593,543 次，无 crash；24h 长跑作为持续巡检，未在本轮执行。弱网在 `alpine:3.22 --cap-add=NET_ADMIN` 内用当前静态 Linux 二进制跑 20% 丢包：`lossy requests: 20/20`、`reconnect: PASS`、`RESULT: PASS`。
> - **依赖**：`@sailonix/ui` / `@sailonix/icon` 0.1.0 从 npm 中央仓库安装，`npm ci` / `typecheck` / `build` 通过（最大 JS 1.167MB / gzip 394KB）；`cargo test --workspace` 72 passed / 1 ignored，clippy `-D warnings` 与 fmt 通过；workspace 版本升 `1.0.0`。

> **M4 实现口径（2026-11-16）**
>
> - 服务端 `[socks5] enabled = true` 才 bind；只接受 `CONNECT` 与 IP 目标，域名 `ATYP=3` 拒绝。目标先过服务端 `allow_cidrs`，再写入 `StreamHeader.dst_hint`，client 连接前二次校验。
> - 默认白名单为内网集合；显式 `options.allow_cidrs` 覆盖默认。公网目标回 `0x02`，无路由 / 限流 / 隧道打开失败回 `0x01`。
> - 当前只支持一个全局 SOCKS5 路由，第二个 socks5 proxy 拒绝；多租户 / 多 client 路由策略留后续。
> - `/api/v1/status` 输出 clients/proxies/totals；`/metrics` 输出 4 个核心指标；`admin_token` 设置后除 `/healthz` 外均要求 Bearer。
> - 本机验收：3 个 loopback 内网端点经 `curl --socks5` 成功；公网目标被拒；status 401/200 与 CLI 表格一致；新增 `socks5_tunnel_roundtrip_and_status` 集成测试；`cargo test --workspace` 通过。Grafana 实际抓取仍待外部面板验收。

> **M3 实现口径（2026-11-16）**
>
> - HTTP：单 `:80` listener 读请求头（8 KiB 上限）→ `Host` 归一化 → 精确/泛域名路由 → yamux `HttpReq` → 原始字节双向透传；无路由 404，非法请求 400，超 `max_conns` 503。
> - 域名冲突：精确与泛域名允许共存，精确优先；仅两个泛域名互相覆盖返回 `DOMAIN_CONFLICT`；路由按 tenant/client/proxy 整组注册与清理。
> - HTTPS：5B TLS record → 3B handshake length → ClientHello 扩展解析 SNI。若 `acme.enabled` 且该域名为 ACME 托管域名，回放 ClientHello 后做 TLS 终止；未命中则按 `https` 路由 SNI 透传，无 SNI / 无路由直接断开。
> - ACME：`rustls-acme 0.15.4`，每个精确域名一个状态机，HTTP-01 挑战由 `:80` 的 `/.well-known/acme-challenge/<token>` 应答；账户 / 证书缓存目录 0700、文件 0600；staging 自动切 Let's Encrypt staging；旧证书在签发失败时保留。TLS-ALPN-01 与 DNS-01（泛域名）未接入，泛域名继续走 SNI。
> - 本机验收：`http_host_routing_and_404`、`https_sni_passthrough`、`acme::tests::cache_roundtrip_uses_0600_files`、`cargo test --workspace` 均通过；ACME 真实签发 / 续期、WebSocket 1h、4GB 上传仍待验收。

> **M1 实现口径（2026-11-16）**
>
> - 控制流由**客户端**打开并首帧发 `Hello`：yamux 的 SYN 随首个数据帧发出，服务端不能“只开流不写”地等待。
> - HMAC key = `sha256(token)`，服务端只存同一哈希；每次连接 32B 新 nonce，失败统一延迟 1s 后关闭。
> - TLS 仅 1.3；自签场景用 `client.server_ca_cert`；`server_public_key_pin` M1 配置即拒绝，M2 实现。
> - 本机验收：自签证书 + echo 隧道，小包与 200KB 双向通过；`kill -9 server` 后客户端按 `1→2→4→8→16→30s`（±20% 抖动）重连，并重新注册 proxy、恢复端口。
> - 公网 VPS SSH 穿透、`iperf3` 85% 指标仍待真实链路验收；`kill -9 client` 需由 systemd / Docker 等外部 supervisor 拉起（进程内无法自愈）。

> **M2 实现口径（2026-11-16）**
>
> - 多 proxy：端口池按 tenant 租约分配；用户指定 `remote_port` 冲突返回 `PORT_IN_USE`，池耗尽返回 `PORT_POOL_EXHAUSTED`；proxy 退出时 Drop 释放租约并 abort listener。
> - UDP：每个 proxy 一条专用 yamux 流 + `session_id` 映射，空闲 60s 回收；单包 ≤1500B。
> - 限速：每 proxy 一个令牌桶，TCP/UDP 双向共享，超限只延迟不丢包。
> - 统计：client 每 10s `Stats` 上报；server 聚合到 `/metrics`（默认 `127.0.0.1:7400`），同时提供 `/healthz`。
> - 热重载：client 监听 `SIGHUP`，重新解析配置后做 proxy 差量（新增 `NewProxy`、删除 `RemoveProxy`、修改先删后加）；解析/校验失败保留旧配置并告警。token / server_addr 等连接级字段变更需重启（日志提示）。
> - SPKI pin：M2 已实现叶子证书 SPKI SHA-256 校验，与 CA 链校验叠加。
> - 本机验收：`cargo test --workspace` 覆盖 10 proxy、UDP 回显、`/metrics` 抓取、SPKI pin 成功/失败、热重载差量；24h 内存 <40MB 与公网 DNS ≥99% 待真实环境跑。

### 8.2 周节奏（V1 完全开源版 10 周，控制面非目标）

| 周 | 内容 | 阶段出口 |
|---|---|---|
| W1 | M0 + M1 前半（TLS / 鉴权 / 帧） | 握手链路打通，能打印 Welcome |
| W2 | M1 收口 | **可演示：SSH 穿透** |
| W3–W4 | M2 | 多 proxy + UDP + 指标，可长期挂着跑 |
| W5–W6 | M3 HTTP/HTTPS | 域名访问 + WebSocket + 大文件可用 |
| W7 | M3 ACME 收口 | staging 与生产各签出一张证书，续期演练通过 |
| W8 | M4 | SOCKS5（默认关）+ 管理 API |
| W9 | M5 | 面板可用，仍是单文件分发 |
| W10 | M6 | **发布 v1.0 自建版**，出 Docker 镜像与安装脚本 |
| 非目标 | 独立控制面 | 不另立项目；第三方可自行扩展，官方不排期 |

> 排期原则：**每个里程碑结束都能对外演示一个真实场景**，不做"三个里程碑后才知道行不行"的长链路。

### 8.3 每个里程碑的安全门禁

| 里程碑 | 必须通过的检查 |
|---|---|
| M1 | token 不出现在日志；握手失败限流生效；TLS 不可降级 |
| M2 | 端口池越界分配被拒；UDP 单包大小限制生效；管理端口无 token 时只绑本地 |
| M3 | 已过：Host 欺骗无法串流（精确/泛域名白名单，未知 Host 404）；SNI 未知域名直接断开；超 `max_conns` 拒绝；ACME 缓存文件 0600 / 目录 0700；泛域名不自动签发、回退 SNI。待过：staging 与生产各签发一次；Pebble 续期；真实签发失败保留旧证书 |
| M4 | 已过：SOCKS5 默认关闭、开启后目标受白名单约束（公网目标 `0x02`）；服务端与 client 双重 CIDR 校验；管理 API 未授权 401、Bearer 200。待过：Grafana 实际抓取 |
| M5 | 已过：面板默认本地绑定；登出 CSRF；CSP 无内联脚本；登录失败限流；未授权 `/api/v1/*` 401；无第三方 UI 框架；proxy 写操作 CSRF；面板吞吐回归 +0.86%；ZAP 被动扫描无 High（1 Medium CSP style-src unsafe-inline）；完整 ZAP baseline 已在 M6 完成（0 FAIL / 5 WARN） |
| M6 | 已过：`cargo test` / clippy / fmt；`gen-cert` 0600；SIGTERM 优雅退出；安全冒烟 9 项；musl 静态产物可运行；Linux 安装 + 面板 200；Docker 镜像可运行；Compose server + Docker client 端到端隧道通过；fuzz 双 target 5 分钟无 crash；netem 20% 丢包 20/20 + 重连 PASS；`cargo audit` 无漏洞；`npm audit` 0 漏洞；完整 ZAP baseline 0 FAIL。持续巡检：fuzz 24h |
| 独立控制面（非目标） | 不适用；V1 只验证本地 token、租户命名空间与扩展接缝不被绕过 |

***

## 九、测试与质量

| 层级 | 工具 / 手段 | 覆盖点 |
|---|---|---|
| 单元 | `cargo test` | 帧编解码边界、端口池分配回收、Host 匹配优先级、令牌桶、配置解析、ACME 续期决策 |
| 协议一致性 | 手写 golden 用例 | 版本不匹配、未知字段、超大帧、乱序 / 截断帧 |
| 集成 | `tokio::test` + 真实 listener | 握手全流程、鉴权失败分支、10 并发 proxy、热重载差量 |
| ACME | `Pebble`（Let's Encrypt 官方测试 CA）| HTTP-01 / TLS-ALPN-01 / DNS-01 全流程、缩短续期周期、失败回滚旧证书 |
| 端到端 | Docker Compose + `netns` 模拟 NAT | SSH、HTTP、UDP、WebSocket、4GB 大文件、4 种弱网 |
| 面板 | Playwright + OWASP ZAP baseline + 截图基线 | 登录、CSRF、越权、XSS、CSP 生效、六页面主流程；`@sailonix/ui` 组件交互与视觉回归 |
| 数据库 | `sqlx migrate` + 本地 docker（`local-postgres`）/ `local-redis` | 空库迁移幂等、跨租户 SQL 被拒、连接池回收、Redis 断连降级 |
| 模糊测试 | `cargo-fuzz`（帧解码 + 配置解析 + ACME 目录 JSON） | 24h 无 panic、无越界 |
| 基准 | `criterion` + `iperf3` | 吞吐、P99 建连延迟、每流内存占用 |
| 安全 | `cargo-audit` / `cargo-deny` / `nmap` 自扫 | 依赖漏洞、暴露端口面 |

**性能目标（验收口径）**

| 指标 | 目标 |
|---|---|
| 单流吞吐（同机房 1Gbps） | ≥ 裸 TCP 的 85% |
| 单流往返延迟增量 | ≤ 1.5ms（同城） |
| 建连 P99（隧道内新用户连接） | ≤ 30ms |
| 1000 空闲 client 会话内存 | ≤ 250MB（server） |
| 空载常驻内存 | server ≤ 40MB，client ≤ 25MB |
| 冷启动到 READY | ≤ 100ms |

***

## 十、可观测性与运维发布

- **日志**：`tracing` + JSON，字段固定 `ts/level/run_id/client_id/proxy/stream_id/msg`；默认 info，`RUST_LOG` 可临时提级。
- **指标**（`/metrics`）：`meatus_client_connected`、`meatus_proxy_active`、`meatus_stream_total`、`meatus_bytes_total{direction}`、`meatus_handshake_fail_total{reason}`、`meatus_local_connect_fail_total`、`meatus_acme_cert_expiry_days`。
- **健康**：`/healthz` 返回 `{"status":"ok","uptime_s":…}`；`/api/v1/status` 返回 client / proxy / 流 的实时快照。
- **面板**：随 server 启动，`admin.panel = true` 时挂载 `/`；未授权访问仅到登录页，所有接口走 `/api/v1/*`。
- **发布**：
  - 交叉编译矩阵：`x86_64-unknown-linux-musl`、`aarch64-unknown-linux-musl`、`x86_64-pc-windows-msvc`、`aarch64-apple-darwin`。
  - CI 顺序：`npm ci && npm run build`（web，`@sailonix/*` 从 npm 中央仓库安装并由 `package-lock.json` 锁定）→ `cargo fmt/clippy/test` → `cargo build --release` → `cargo audit`。
  - 产物：静态二进制（含面板，目标 ≤ 20MB）+ `docker image`（`scratch`）+ `install.sh` + systemd unit。
  - 版本：`0.x` 阶段 minor 可破坏性变更，但**协议版本独立演进**（`MEATUS/1` 保持兼容）。
  - 升级策略：先升 server（兼容旧 client）再升 client；协议不兼容时显式返回 `UNSUPPORTED_VERSION`。
- **许可证**：`Apache-2.0`（已提交 `LICENSE`，与 frp/rathole 一致，商业友好）；第三方组件声明见 `THIRD_PARTY_NOTICES.md`。

***

## 十一、风险登记

| # | 风险 | 概率 | 影响 | 应对 |
|---|---|---|---|---|
| R1 | 共享 TCP 的队头阻塞导致多流相互拖慢 | 中 | 中 | 短请求拆独立连接；v2 上 QUIC |
| R2 | 国内 UDP/QUIC 被 QoS | 高 | 中 | 默认 TCP+TLS；QUIC 仅可选开关 |
| R3 | HTTPS SNI 透传被中间盒劫持 / 阻断 | 中 | 中 | 直连端口兜底；文档给出 TLS 终止模式 |
| R4 | 端口池与宿主机其他服务冲突 | 中 | 中 | 启动预检整段端口，冲突拒绝启动并报占用者 |
| R5 | Windows 客户端服务化体验差 | 高 | 低 | 首版只保证前台运行 + `sc create` 脚本 |
| R6 | 协议早期设计缺陷导致无法演进 | 中 | 高 | 首版带 `protocol_version` 与未知字段忽略；M1 前冻结 proto + 2 轮 ADR |
| R7 | 单人排期被排障打断 | 高 | 中 | 按"可演示"切分，任意时点中断都有可交付版本 |
| R8 | 依赖（yamux/rustls）升级破坏 API | 低 | 中 | 锁 `Cargo.lock`，升级单独 PR + 全量 e2e |
| R9 | 开源扩展时写成"单租户假设"，后续接缝返工 | 中 | 高 | 2.5 的 6 条约束进 M0；PR 模板固定检查项 |
| R10 | ctrl 成为单点 | 中 | 高 | relay 缓存已授权 token，ctrl 短时不可用不阻断在用隧道 |
| R11 | ACME 配额打光 / 签发失败导致断服 | 中 | 高 | staging 先行；续期前置查 7 天记录；失败保留旧证书并告警 |
| R12 | 泛域名必须 DNS-01，用户没有 DNS API | 高 | 中 | 明确降级路径：改用单域名 HTTP-01 或自带证书；文档给三家示例 |
| R13 | 面板成为新攻击面（XSS/CSRF/越权） | 中 | 高 | 默认本地绑定、CSRF、CSP、ZAP baseline 进 M5 门禁 |
| R14 | `@sailonix/*` 版本漂移或中央仓库包缺失 | 高 | 中 | 锁定 `^0.1.0` + `package-lock.json`；升级单独 PR；发布前确认 tarball 内含 MIT LICENSE |
| R15 | 组件库升级导致面板视觉 / 交互漂移 | 中 | 中 | 锁定版本；升级单独 PR；六页面截图基线 + 视觉回归兜底 |
| R16 | 本地 PG 14 与生产版本 / 参数不一致 | 中 | 中 | 生产同大版本起步；连接池上限按 ctrl 实例数 × 20 预算；上线前跑一次生产同规格压测 |
| R17 | 本地 Redis 无密码、单实例，误当生产配置 | 高 | 高 | 启动检测：无密码 + 非 127.0.0.1 绑定即拒绝启动；提供 `prod.toml.example`；生产独立实例 |

***

## 十二、决策记录（D1–D8 已全部关闭）

| # | 问题 | 结论 | 落地位置 / 影响 |
|---|---|---|---|
| **D1 ✅（2026-11-17 修订）** | 产品形态 | **完全开源、自托管** | Apache-2.0；V1 = M0–M6；不建 ctrl、不接 PG/Redis、不做计费；接缝仅作开源扩展 |
| **D2 ✅** | 首发是否带 Web 面板 | **带** | 新增 M5（6 人日）；Vue3 + `@sailonix/ui` 构建，`rust-embed` 嵌入 server，仍单文件分发；4.8 定义 6 个页面 |
| **D3 ✅** | 传输首发 | **TCP + TLS 1.3 + yamux** | QUIC 仅 v2（M8）可选开关；2.4 已固化 |
| **D4 ✅** | HTTPS 证书策略 | **自动 ACME 为默认** | M3 扩为 13 人日（HTTP/HTTPS 8 + ACME 5）；单域名 HTTP-01/TLS-ALPN-01，泛域名 DNS-01，自带证书兜底 |
| **D5 ✅** | SOCKS5 是否默认开 | **显式开启**（默认 `false`） | 4.5 + 7.1 `[socks5] enabled=false`；M4 门禁含"默认关、开启后受白名单约束" |
| **D6 ✅** | P2P 打洞时机 | **v2** | 排到 M8；先靠 relay 版验证真实用量 |
| **D7 ✅** | 面板前端技术 | **Vue3 + TS + Vite8 + `@sailonix/ui`（按 sailon-dev 口径）** | 4.8 按需子路径导入、`var(--sl-*)` token、禁第三方 UI 框架；依赖从 npm 中央仓库安装并锁 `package-lock` |
| **D8 ✅（范围修订）** | 数据层与本地环境 | **V1 零数据库 / 零 Redis** | PostgreSQL 14 / Redis 8 仅为历史 ctrl 预研；7.4–7.6 明确为非当前范围，不进 V1 安装与验收 |

***

## 十三、下一步（M0 可直接开工的具体任务）

1. `cargo new --lib` 建 5 个 crate + workspace 根 `Cargo.toml`（`resolver = "3"`、edition 2024、`[workspace.lints]`）。
2. 落 `proto/meatus.proto`（3.2 的完整版）→ `meatus-proto` 生成代码，补 `Frame` 编解码与错误码枚举。
3. `meatus-core`：`Transport` trait（`connect/accept/open_stream`）先实现 TCP+TLS，QUIC 留空实现。
4. `meatus-cli`：子命令骨架 + 配置加载 + `--dry-run`。
5. CI：web build → fmt/clippy/test → release build → audit，五段流水线 + `cargo-deny` 白名单。
6. ADR：`0001-transport-tcp-tls-yamux.md`、`0002-dual-mode-self-host-first.md`。
7. 五个接缝落 trait 空实现（`AuthProvider`/`ConfigSource`/`UsageSink`/`NameAllocator`/`QuotaPolicy`），`tenant_id = "default"` 贯穿所有 key——约 2 人日；仅为开源扩展保留，不排 M7。
8. **ACME spike（1 人日，建议与 M1 并行）**：起 Pebble，验证 80 的 HTTP-01 与 443 的 TLS-ALPN-01 能否与现有 listener 共存、证书热加载是否不断连。这一步能提前暴露 M3 最大的不确定性。
9. **面板骨架（1 人日）**：`web/` 起 Vue3 + TS + Vite8，接 `@sailonix/ui` 最小闭环（`config-provider` + 主题 token + `button`/`card`/`data-table`），从 npm 中央仓库安装并锁定 `package-lock.json`，先把"按需导入 → 打包 → `rust-embed` → 单二进制"整条链路跑通。
10. **开源发布自检（0.5 人日）**：校验 `LICENSE`、`THIRD_PARTY_NOTICES.md`、npm 包内 MIT 许可证、秘密扫描与公开构建；V1 不校验 PG / Redis，因为它们不属于运行依赖。

> M0 完成即可进入 M1，M1 的唯一目标就是**在真实公网 VPS 上把 SSH 穿透打通并压出吞吐数据**——这是整个项目最有说服力的第一个证据。

***

## 十四、完全开源发布清单（2026-11-17）

### 14.1 已落地

- [x] `LICENSE`：Apache License 2.0。
- [x] `README.md` 与本文档改为“完全开源、自托管”。
- [x] `Cargo.toml` workspace `license = "Apache-2.0"`；`web/package.json` 增加 `license` 字段。
- [x] `THIRD_PARTY_NOTICES.md`：声明面板使用的 `@sailonix/ui` / `@sailonix/icon` 0.1.0 及上游 MIT 许可证。
- [x] V1 运行路径确认零 PostgreSQL / 零 Redis；历史 ctrl 设计标记为非当前范围。

### 14.2 公开发布前必须完成

- [ ] **公开仓库**：把当前 Codeup 仓库镜像到 GitHub / Gitee 等公开地址，更新 `Cargo.toml` 的 `repository`、README 徽章与克隆命令。
- [ ] **npm 包许可证**：确认 `@sailonix/ui` / `@sailonix/icon` 0.1.0 的 npm tarball 内含 MIT `LICENSE`；新增第三方包在 `THIRD_PARTY_NOTICES.md` 逐项列明。
- [ ] **私有源清理**：CI、Dockerfile、文档中不得出现私有 npm registry、内网地址、token；面板依赖统一走 npm 中央仓库并由 `package-lock.json` 锁定。
- [ ] **许可证扫描**：接入 `cargo-deny` / `cargo-about`、`npm ls` + license checker；生成第三方许可证清单。
- [ ] **SBOM 与校验和**：release 产物附 `SHA256SUMS` 与 CycloneDX / SPDX SBOM；Docker 镜像加 `org.opencontainers.image.licenses=Apache-2.0`、`source` 标签。
- [ ] **社区文件**：补 `CONTRIBUTING.md`、`SECURITY.md`、`CODE_OF_CONDUCT.md`、Issue / PR 模板；明确安全漏洞私下报告渠道。
- [ ] **秘密扫描**：用 `gitleaks` / `git secrets` 扫全历史；确认 admin token、client token、服务器密码、registry 密码从未进入提交。当前测试记录中的服务器 IP / 用户名已脱敏为示例值。
- [ ] **公开构建**：用公开 commit 从零跑 `cargo test --workspace`、`npm ci && npm run build`、Docker build 与 `scripts/docker-smoke.sh`，确认不依赖本机私有文件。
- [ ] **文档一致性**：删除或归档所有“控制面排期”“ctrl 必做”表述；保留的历史设计统一标注“非当前范围”。

### 14.3 发布口径

- 对外名称：**Meatus 完全开源自托管版**。
- 许可证：Apache-2.0，允许商业使用、修改、再分发；不提供官方商业支持承诺。
- 不做闭源企业版、付费控制面；第三方托管或二次开发不属于官方产品。

***

## 十五、下一版本规划（v1.1，草案）：YAML 可选配置

> 状态：**草案，不进入 V1.0 范围**。V1 仍只承诺 `server.toml` / `client.toml`，现有部署不需要迁移。
> 目标：在不改变默认配置格式的前提下，增加 **YAML 只读解析**，方便 Docker Compose / Kubernetes 用户复用熟悉的 YAML 书写习惯；TOML 继续作为默认与推荐格式。

### 15.1 范围与边界

| 项 | v1.1 计划 |
|---|---|
| 配置格式 | 同时支持 TOML 与 YAML；TOML 默认，YAML 可选 |
| 文件识别 | `.toml` 走 TOML；`.yaml` / `.yml` 走 YAML；未知扩展名回退 TOML，并提示显式使用 `--format` |
| CLI 默认值 | 仍为 `server.toml` / `client.toml`；`--config` 可直接指向 YAML |
| `meatus check` | 按扩展名自动选择解析器；解析顶层 `server` / `client` 判断类型，不依赖 `[server]` 行扫描 |
| 写回 | 不实现 YAML / TOML 互转、不自动改写用户配置；面板也不提供格式转换 |
| Docker / systemd | 挂载路径与启动参数不变；用户可把 YAML 挂到 `/data/server.yaml` 并显式 `--config` |
| 环境变量 | 继续在解析前展开 `${VAR}`；文档要求把占位符放在引号内 |
| 支持子集 | YAML 1.2 核心映射 / 序列 / 标量；不支持自定义 tag、多文档 `---`、复杂锚点合并 |
| 安全 | 不提供 `!include`、远程引用、模板执行；YAML 解析失败直接报错，不静默回退到 TOML |

### 15.2 代码改动点

1. `crates/meatus-core/src/config.rs`：抽出 `ConfigFormat` 与 `parse_by_format`；`parse_server_config` / `parse_client_config` 保持不变作为 TOML 入口，新增 YAML 入口。
2. `ConfigError`：增加 `Yaml` 变体，保留文件路径、行号、列号；错误文案明确写出实际格式。
3. `detect_kind`：TOML 继续按顶层表判断，YAML 解析为 `Value` 后检查顶层 `server` / `client`；无法判断时报 `cannot detect config kind`，不再猜测。
4. `crates/meatus-cli`：`--config` 支持 `.yaml` / `.yml`；新增可选 `--format auto|toml|yaml`，默认 `auto`；`check` 输出中打印实际使用的格式。
5. 依赖：选择仍在维护的 YAML 解析库（`serde_yaml_ng` 或 `serde_norway`），锁定版本；不引入运行时模板引擎。
6. 示例与文档：新增 `config/server.example.yaml`、`config/client.example.yaml`；`docs/USAGE.md` 增加“YAML 写法与 TOML 对照”一节；`docs/INSTALL.md` 增加 Docker 挂载 YAML 的示例。
7. 测试：增加 TOML / YAML 语义等价测试、错误定位测试、`detect_kind` 测试、Docker smoke 的 YAML 变体；现有 TOML 测试不得改动语义。

### 15.3 兼容策略

- 现有 `server.toml` / `client.toml` 原样可用，CLI 默认值不变。
- 不重命名现有示例、Docker 卷内路径或 systemd 参数。
- 新格式通过 `--config` 显式选择；不在同一目录自动猜测“同名 `.yaml` 优先”。
- 若后续要调整默认格式，至少经过一个 minor 版本的弃用提示，不在 v1.1 做。

### 15.4 验收标准（DoD）

- `meatus check --config server.yaml` 与等价 `server.toml` 输出同一份启动计划。
- `meatus server --config /data/server.yaml`、`meatus client --config /data/client.yaml` 可正常启动并建立隧道。
- YAML 中 `remote_port`、`enabled`、`options.*` 的类型与 TOML 一致；错误类型在启动前被拒绝。
- 非法 YAML（缩进错误、重复键、未加引号的 `*.domain`）返回可定位的错误，不 panic、不静默使用默认值。
- `cargo test --workspace`、`meatus check`、Docker `docker run` YAML 实测、文档示例全部通过。
- 现有 TOML 部署与测试零回归。

### 15.5 风险与对策

| 风险 | 对策 |
|---|---|
| YAML 隐式类型把 token / port 解析错 | 文档强制引号；校验层拒绝错误类型；示例全部显式引号 |
| 通配域名 `*.example.com` 被当成别名 | 文档与示例统一加引号；`meatus check` 给出针对性错误 |
| YAML 解析库维护状态变化 | 选择维护中的分支并锁版本；解析层封装，后续可替换 |
| 两种格式行为漂移 | 用同一组语义等价 fixture 跑双格式测试；差异视为 bug |
| 用户误以为 YAML 是默认 | CLI 输出打印实际格式；文档标题明确“可选” |
| Docker 首启仍只生成 TOML | entrypoint 不变；YAML 用户自行挂载并显式 `--config`，文档给完整命令 |

### 15.6 预估与排期

- 预估 **3–5 人日**：解析抽象 1 人日、CLI / 检测 0.5 人日、测试 1 人日、示例与文档 1 人日、Docker / 回归 0.5 人日。
- 进入条件：V1.0 发布收口完成，且现有 TOML 回归稳定。
- 不在 v1.1 承诺：YAML 写回、配置格式互转、Web 面板在线编辑 YAML、JSON 配置、配置 schema 自动生成。

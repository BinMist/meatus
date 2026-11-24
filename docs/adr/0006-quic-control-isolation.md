# ADR 0006：QUIC 控制面隔离方案

- 状态：已实现（G6-02，待 G6-03 公网门禁确认）
- 日期：2026-11-24
- 决策者：项目负责人
- 关联：ADR 0004（传输抽象与池键）、ADR 0005（QUIC 选型与单连接单会话）

## 背景

E5-08 在公网 `.47` 记录到 QUIC 控制面饥饿：32 并发 × 16KB × 10s 的 7 次尝试中 2 次失败，
整轮成功率 71%。其中一次客户端报 `no Pong for 68.988979041s`，期间 32 条数据流仍陆续被
服务端受理，QUIC 连接整体未断；同窗口 TCP×4 为 3/3 成功。

现有实现按 ADR 0005 的约束，每条 QUIC 连接只承载一条 yamux 会话：

- `crates/meatus-core/src/quic.rs::server_transport` 把每条连接限制为双向流 1、单向流 0；
- `crates/meatus-server/src/quic.rs` 每连接只 `accept_bi()` 一次，随后 `transport::attach`；
- `crates/meatus-client/src/quic.rs` 只 `open_bi()` 一次；
- 控制帧与全部数据流都在这条 yamux 会话内复用。

因此控制 Ping/Pong 与数据帧共享同一条 QUIC 连接的发送队列和拥塞控制。数据面排队时，
Pong 可能被推迟到超过客户端 `PONG_TIMEOUT=60s`，客户端判定控制会话失败并重连。

## 备选方案

### A. 独立控制连接（复用现有连接池）

客户端开启 `[client.pool]` 后：

- 控制连接被标记为 `control_only`（`crates/meatus-core/src/pool.rs`）；
- `data_conns = pool.size.max(1)`（`crates/meatus-client/src/session.rs`）；
- `establish_data()` 为数据面另拨一条 QUIC 连接，服务端 `serve_data_connection`
  将其注册为数据连接。

控制与数据分别拥有独立的 QUIC 连接、拥塞控制、发送缓冲与丢包恢复，不需要改协议、
不需要改 yamux 复用路径，也不与 ADR 0005 冲突。代价是每条客户端多一次 QUIC 握手与
一份连接状态。

### B. 原生 QUIC 多流

把控制流与数据流分别映射到 quinn bidi stream，绕过 yamux 的数据复用。它需要：

- 修改 ADR 0005 的「每条 QUIC 连接只承载一条 yamux 会话」；
- 重构 `transport` 抽象、客户端/服务端流分发、`StreamHeader` 读取路径与连接池语义；
- 处理 QUIC 流关闭、半关闭与错误传播。

更关键的是，多流仍共享同一条连接的拥塞控制；quinn 0.11 没有显式流优先级 API，
控制帧不保证绝对优先。因此它降低应用层 yamux 队头阻塞，但不能把控制面从数据面
拥塞窗口中独立出来，工作量与风险都高于方案 A。

## Spike 结果

新增 `scripts/quic-isolation-spike.py`：用户态 UDP WAN 代理，双向固定延迟、丢包与
FIFO 速率限制；每个 attempt 独立临时目录、独立证书、独立服务端/客户端进程。

### 1. 计划口径：未能复现 71%

条件：32 并发、16KB、10s、300ms RTT、1% 丢包、每方向 2Mbps，各 5 轮。

| 方案 | 成功轮次 | 吞吐 | P50 | P99 | Pong 超时 |
| --- | --- | --- | --- | --- | --- |
| baseline（单连接共享） | 5/5 | 3.07–3.28 Mbps | 2.79–3.73s | 4.63–6.23s | 0 |
| isolated（池 size=1） | 5/5 | 2.49–3.28 Mbps | 2.96–4.57s | 4.58–5.84s | 0 |

吞吐与历史公网 3.2Mbps、P50 2.82s 基本一致，但没有出现 `no Pong`。结论：**当前实验室
的人造 WAN 在计划并发下无法复现 E5-08 的 71% 失败**，不能用这 5 轮证明隔离修复了
公网故障。

证据：`planning/benchmarks/g6-01/g6-01-both.json`。

### 2. 故障注入：能打出控制饥饿，但不能区分方案

- 32 并发 × pipeline 64 × 每方向 1Mbps：baseline 出现 `no Pong for 60s` 并重连；
  isolated 同样出现，说明该压力已进入客户端 CPU/运行时饱和区。
- 128 并发 × pipeline 16：baseline 2 次中 1 次控制超时，isolated 2 次均超时；
  数据面两端都大面积失败。

证据：`planning/benchmarks/g6-01/fault-128/g6-01-both.json` 与临时日志。

结论：**独立控制连接能消除“控制与数据共享发送队列”这一结构性风险，但不能解决
客户端 CPU 饱和导致的控制任务饥饿**。ADR 不把方案 A 宣称为万能修复。

## 决策

1. **采用方案 A 作为 G6-02 的实现基线**：QUIC 模式下，控制连接必须
   `control_only`，数据面至少一条独立 QUIC 连接；不把控制流放回数据连接。
2. **G6-02 同时补控制面可观测性与任务预算**：
   - 记录 Pong RTT 直方图、连续丢失次数、最近一次 Pong 时间；
   - 控制会话读循环与数据转发任务分离，避免任一数据任务长时间占用运行时；
   - 超过 20s 无 Pong 打 WARN，超过 60s 才判定失败，保留现有客户端语义。
3. **不采用方案 B 作为本期实现**：保留为后续可选优化；若未来要做，必须先修订
   ADR 0005，并接受“多流共享拥塞控制、无流优先级”的限制。
4. **不改变默认**：`[client.pool]` 仍默认关闭；G6-03 在公网做 10 轮 ≥95% 后才出
   “是否默认化”的数据与建议，不直接改默认。
5. **G6-03 必须以公网复测为主**：实验室 5 轮 100% 不能替代 E5-08 口径；若公网仍
   出现控制饥饿，按本 ADR 第 2 条继续加控制面预算，而不是回头选多流。

## G6-02 实现记录（2026-11-24）

- 数据连接选择：`crates/meatus-client/src/session.rs` 新增 `data_connection_count()`：
  - 协议 v1：`0`；
  - 协议 v2、池启用：`pool.size.max(1)`；
  - 协议 v2、池关闭、控制连接为 QUIC：`1`（新增行为，落地 ADR 第 1 条）；
  - 协议 v2、控制连接为 TCP（含 `transport=auto` 回落 TCP）：沿用
    `default_tcp_data_connections(pool_size)`。
  控制传输是否为 QUIC 由 `EstablishedSession.quic.is_some()` 判定，不靠配置猜测，
  因此 `auto` 回落 TCP 时不会误开 QUIC 数据连接。
- 控制连接保持 `control_only`：独立数据连接建立后，服务端会话池只从非控制连接分配
  `StreamHeader`；控制连接上的 dispatcher 仅作为旧服务端/数据连接失败时的回退路径保留。
- 可观测性：新增 `ControlHealth`，记录 Pong RTT 直方图（`<1s / 1–5s / 5–20s /
  20–60s / >=60s`）、连续 Pong 丢失次数、最近 Pong 时间、最大 Pong 间隔与超时次数。
  每个 Pong 打 INFO（`rtt_ms`、`seq`），每 60s 与会话退出时打聚合快照。
- WARN/失败阈值：沿用客户端 10s Ping；超过 20s 无 Pong 打 WARN（每次连续丢失只打一次），
  超过 60s 才 `no Pong` 判失败，保留原重连语义。
- QUIC 端生命周期：`crates/meatus-server/src/quic.rs` 用 `JoinSet` 持有每条已握手连接
  的任务，端点关闭时 `abort_all()` 并等待任务释放 `Connection`。否则独立数据连接会让旧
  UDP socket 被连接引用，重启/滚动升级时新端点绑定失败；回归用例
  `quic_client_reconnects_after_server_restart` 覆盖该路径。
- 任务边界：控制读循环本来就是独立 Tokio 任务，数据连接每条一个转发任务；本轮未引入
  独立 runtime 或线程优先级（Tokio 无稳定优先级 API，强行拆 runtime 会扩大 G6-02 改动面）。
  因此独立连接消除的是发送队列/拥塞控制共享，不能消除 CPU/运行时饱和；128 并发深流水线
  仍可能控制超时，G6-03 若公网复现饥饿，再按 ADR 第 2 条评估控制面预算。
- 验证：`cargo test --workspace --all-targets --locked` **291 passed / 0 failed /
  1 ignored**；`cargo clippy -p meatus-client -p meatus-server --all-targets -- -D warnings`
  通过；本地 32×16KB×10s、300ms RTT、1% 丢包、2Mbps 3/3 成功，控制 Pong 最大 2.83s、
  无 `no Pong`、无重连。证据见 `planning/benchmarks/g6-02/`。

## 后果

- 正面：控制面与数据面在 QUIC 层彻底分连接；实现复用现有池路径，改动面小，符合
  G6-02 的 3 人日预算。
- 负面：每客户端多一条 QUIC 连接与握手；服务端连接上限 256 需要按“控制+数据”成对
  计入；池 size=1 时数据面只有一条连接，单连接故障会丢全部数据流，但控制面仍可重连。
- 风险：ADR 未能用实验室数据复现公网 71%，因此 G6-03 是必过门禁；在公网门禁通过前，
  不得对外宣称 QUIC 控制面已修复。

## 验证

- Spike 脚本：`scripts/quic-isolation-spike.py`
- G6-01 计划口径数据：`planning/benchmarks/g6-01/g6-01-both.json`
- G6-01 故障注入数据：`planning/benchmarks/g6-01/fault-128/g6-01-both.json`
- G6-02 实现验收数据：`planning/benchmarks/g6-02/g6-02-baseline.json`
- 复跑 G6-01 数据需使用 G6-01 提交（`835335f`）的二进制；G6-02 之后的二进制在
  `--variant baseline` 下也会自动开独立数据连接，不能再代表单连接共享基线。
- 复跑命令：

```bash
python3 scripts/quic-isolation-spike.py \
  --variant both --attempts 5 --duration 10 --concurrency 32 \
  --pipeline 1 --rate-mbps 2 --delay-ms 150 --loss 0.01 \
  --out-dir planning/benchmarks/g6-01
```

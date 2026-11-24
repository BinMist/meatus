# G6-02 QUIC 控制面隔离实现验收数据

日期：2026-11-24
脚本：`scripts/quic-isolation-spike.py`
二进制：macOS 本地 `release`（`feat/v1.5` G6-02 工作树，版本串仍为 `meatus 1.4.0`）
配置：QUIC 控制传输，`[client.pool]` 保持默认关闭

## 代码变更对应的判据

G6-02 后，协议 v2 且控制连接为 QUIC 时，客户端即使未启用 `[client.pool]` 也会另拨
**1 条独立数据连接**，控制连接保持 `control_only`。因此本目录的 `baseline` 标签实际是
「池关闭 + 自动独立数据连接」，**不再是 G6-01 的单连接共享 baseline**。

每次 attempt 的 `client.log` 应出现：

```text
QUIC control isolation enabled: dedicated data connection data_connections=1
data connection established session_id=...
```

服务端 `server.log` 应出现 `data connection registered ... pool_size=2`（控制连接 + 数据连接）。

## 计划口径结果

`g6-02-baseline.json`：32 并发 × 16KB × 10s，300ms RTT，1% 丢包，每方向 2Mbps，3 轮。

| attempt | 成功 | 吞吐 | P50 | P99 | `no Pong` | 重连 | 控制 Pong RTT |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 是 | 2.779 Mbps | 3.999s | 5.356s | 0 | 0 | 2538ms / 303ms / 308ms |
| 2 | 是 | 3.198 Mbps | 2.753s | 4.875s | 0 | 0 | 2832ms / 2535ms / 302ms |
| 3 | 是 | 2.988 Mbps | 3.617s | 5.615s | 0 | 0 | 550ms / 303ms / 302ms |

3/3 通过，32 个 worker 全部成功，无 `no Pong`、无客户端重连。首条 Pong RTT 较高
（0.55–2.83s）出现在数据面爬坡阶段，随后稳定在约 300ms（WAN 代理配置的 300ms RTT）。
所有 Pong 均低于 20s WARN 阈值，更远低于 60s 失败阈值。

## 环境与复跑

- 执行机：macOS 本地；服务端、客户端、echo 后端与用户态 UDP WAN 代理同机。
- WAN 代理：双向 150ms 固定延迟（合计 300ms RTT）、1% 丢包、每方向 2Mbps。
- 每个 attempt 独立证书、端口、进程与临时目录；私钥已从提交目录删除。

```bash
python3 scripts/quic-isolation-spike.py \
  --variant baseline --attempts 3 --duration 10 --concurrency 32 \
  --payload 16384 --pipeline 1 --rate-mbps 2 --delay-ms 150 --loss 0.01 \
  --out-dir planning/benchmarks/g6-02
```

## 边界

- 这是**实验室同机 loopback + 用户态代理**，不能替代公网 NAT/MTU/运营商缓冲；
  不能据此宣称 E5-08 的 71% 失败已修复。
- 本目录仅验收「独立数据连接已建立 + 控制 Pong 不超时」；G6-03 仍需公网 10 轮 ≥95%。
- 本轮只有 3 轮，且仅跑「池关闭 + 自动隔离」一种配置；池开启 `size=1` 的路径在 G6-01
  已覆盖 5 轮。
- 首条 Pong 的秒级 RTT 说明独立连接消除了发送队列共享，但不能消除 CPU/运行时饱和；
  128 并发深流水线的控制超时结论仍有效。

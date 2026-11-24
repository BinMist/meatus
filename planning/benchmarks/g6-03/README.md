# G6-03 QUIC 公网 10 轮门禁

日期：2026-11-24
状态：**通过**（10/10，`success_rate=1.0`，`gate_pass=true`）
脚本：`scripts/g6-03-public-gate.py`
数据：`g6-03-public.json`

## 门禁口径

- 32 并发 × 16KB × 10s，连续 10 轮；
- 成功率 ≥95%（10 轮中至少 10 轮成功，9 轮只有 90%）；
- 每轮 32/32 worker 完成，无 `worker_errors`；
- 控制面不超时：无 `no Pong for`、无 `control session failed, reconnecting`；
- 控制 Pong 最大 RTT 低于 20s WARN 观察阈值，更低于 60s 失败阈值；
- QUIC 控制隔离日志存在：客户端 `QUIC control isolation enabled ... data_connections=1`，服务端至少 1 条 `data connection registered`。

## 环境

- 服务端：腾讯云轻量 `43.173.105.47`（`VM-0-14-ubuntu`，Ubuntu Linux 6.8，2 vCPU / 1967MB）。
- 客户端：macOS 本机，公网出口 `153.3.163.91`；未使用本机 WAN 代理，走真实公网。
- 测试端口：TCP 22000 控制面、TCP 22001 代理面、UDP 22000 QUIC。
- 线上 80/443/7000 容器未改动；测试前 80/443/7000 由生产进程监听，测试只占用 22000/22001。
- 服务端二进制：`target/x86_64-unknown-linux-musl/release/meatus`，21MB，
  SHA-256 `cd9aabb3ee7bacc7824865b9b38bfbd3560c90ca72d9adb157165801053d8110`；
  远端 `/home/ubuntu/meatus-wan/meatus-g603` 与本地一致。
- 客户端二进制：`target/release/meatus`，版本串 `meatus 1.4.0`，包含 G6-02 的
  `QUIC control isolation enabled`。
- 配置：服务端 `quic=true`，`[ports] range=[22001,22001]`，admin `127.0.0.1:12741`；
  客户端 `transport="quic"`，`[client.pool]` 未启用。自签证书 domain `localhost`，
  测试 token `g6-gate-secret`。

## 结果

| attempt | worker | 吞吐 Mbps | P50 ms | P99 ms | Pong 数 | Pong 最大 ms | 最大 gap ms |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | 32/32 | 3.041 | 3144.055 | 5977.884 | 8 | 2463 | 11766 |
| 2 | 32/32 | 3.618 | 2512.066 | 5527.839 | 8 | 1121 | 10748 |
| 3 | 32/32 | 0.839 | 32498.147 | 37403.545 | 11 | 1164 | 10794 |
| 4 | 32/32 | 3.460 | 2620.505 | 5793.492 | 8 | 1360 | 10988 |
| 5 | 32/32 | 3.408 | 2875.795 | 5775.847 | 8 | 2337 | 9963 |
| 6 | 32/32 | 3.618 | 2591.718 | 5247.101 | 6 | 1371 | 11013 |
| 7 | 32/32 | 3.434 | 2616.222 | 5758.601 | 8 | 745 | 10152 |
| 8 | 32/32 | 3.434 | 2618.119 | 5847.626 | 8 | 1193 | 10771 |
| 9 | 32/32 | 3.460 | 2817.423 | 6392.017 | 8 | 1272 | 10977 |
| 10 | 32/32 | 3.618 | 2449.791 | 5226.295 | 11 | 1249 | 10800 |

汇总：

- **10/10 成功，成功率 100%**；全部轮 32/32 worker，无 `worker_errors`。
- 10 轮全部 `no_pong_failures=0`、`reconnects=0`、`delayed_warnings=0`；
  客户端日志无 `no Pong for`、无 `control session failed, reconnecting`。
- 控制 Pong：共 95 个，中位数 **377ms**，P90 **1193ms**，最大 **2463ms**；
  最大 Pong 间隔 **11766ms**，均低于 20s WARN 阈值。
- 吞吐中位数 **3.447 Mbps**（范围 0.839–3.618）；P50 中位数 **2619ms**，
  P99 中位数 **5785ms**。
- 10 轮全部 `isolation_logged=true`、`isolation_data_connections=1`、
  `data_registered=1`；客户端日志确认自动另拨独立数据连接，服务端日志确认该连接
  注册为 `data connection registered ... pool_size=2`（控制 + 数据）。

## 判读

- **门禁通过**：E5-08 同口径 7 轮中 2 轮失败（71%）；本轮到 10 轮 100% 成功，
  控制面在整个数据面压满期间没有触发 20s WARN，更没有触发 60s `no Pong`。
- **不等于长尾消失**：attempt 3 仍是 32/32 完成、控制面正常，但吞吐仅 0.839 Mbps、
  P50 32.5s、P99 37.4s，单轮只完成 32 个请求（每 worker 1 个）。这是公网链路/TCP
  流恢复的长尾，不是控制面饥饿；门禁判据只要求 32/32 完成与控制 Pong 不超时，因此
  该轮仍计为通过。不能把 10/10 等同于“性能完全稳定”。
- **隔离生效**：池关闭时客户端自动另拨 1 条 QUIC 数据连接，控制连接不承载数据；
  这验证了 ADR 0006 方案 A 的结构性修复在真实公网成立。
- **UDP GSO 回退**：10 轮服务端日志各有 1 次
  `libc::sendmsg failed with I/O error (os error 5); halting segmentation offload`，
  随后 `quinn` 关闭 UDP GSO 并继续发送，数据全部成功。该错误是测试机/网卡不支持
  GSO 的环境噪声，不是门禁失败；但它会带来额外发送开销，后续性能优化应单独评估。
- **样本边界**：10 轮只能证明该公网链路、该时段、该配置下的成功率；不能证明任意
  运营商、MTU、丢包或 CPU 饱和条件下都不饥饿。G6-01 的 128 并发深流水线结论仍有效。

## 连接池默认化评估（只出建议，不改默认）

现状：G6-02 后，协议 v2 + QUIC 控制连接时，即使 `[client.pool].enabled=false`，
客户端也自动另拨 **1 条**独立数据连接。本轮 10/10 走的就是这个默认路径。

数据：

| 路径 | 数据连接数 | 结果 | 来源 |
| --- | --- | --- | --- |
| 池关闭 + 自动隔离（当前默认） | 1 | 公网 10/10，控制 Pong 最大 2.463s | 本目录 |
| 池开启 `size=1` | 1 | 实验室 5/5，无 `no Pong` | `planning/benchmarks/g6-01/` |
| 池开启 `size=1` + G6-02 代码 | 1 | 本地 3/3，Pong 最大 2.83s | `planning/benchmarks/g6-02/` |

结论：**建议 `[client.pool].enabled` 继续保持默认 `false`，不在 1.5.0 默认化。**

理由：

- 池 `size=1` 与当前自动隔离的数据连接数相同，都是 1 条；默认化不会增加吞吐或隔离收益，
  只会多一层配置语义。
- 当前默认路径已在公网 10/10 通过；池路径只有实验室 5/5 与本地 3/3，没有同口径公网 10 轮，
  默认化缺少公网证据。
- 池的价值在 `size>1`（多数据连接聚合/冗余），但这会改变服务端连接预算与端口行为，
  需要单独的公网门禁与容量评估，不能借 G6-03 顺带默认。

取舍：需要多数据连接的用户仍可显式 `enabled=true, size=N`；默认用户获得“无需配置即隔离”，
代价是每条客户端固定多 1 条 QUIC 连接与握手。后续若要默认化，至少补 10 轮公网池开启对照，
并评估服务端连接上限。

## 清理与生产影响

- 正式跑完后远端 `pgrep -a -x meatus-g603` 无输出，`ss -ltnup` 无 22000/22001 监听；
  临时测试进程已清理。
- 远端 `/home/ubuntu/meatus-wan/meatus-g603` 与临时证书/配置/日志保留，未替换生产二进制。
- 腾讯云 Lighthouse 防火墙两条 G6-03 临时规则已删除，恢复为原 5 条（443/80/7000/22/ICMP）；
  删除后从公网复测 TCP 22000/22001 均不可达。
- 测试未重启或替换线上容器；生产 80 仍返回 200、7000 仍可连接。测试后 `docker ps`
  因 SSH 用户无 docker socket 权限未复核容器状态，不能声称已核对容器内部状态。

## 复跑

前置：远端已有 `meatus-g603`（hash 与本地一致），防火墙临时放行 `153.3.163.91/32`
的 TCP 22000-22001 与 UDP 22000；跑完立即删除临时规则。

```bash
python3 scripts/g6-03-public-gate.py \
  --attempts 10 --skip-upload \
  --out-dir planning/benchmarks/g6-03
```

`--skip-upload` 复用远端二进制；首次上传会 gzip 后 scp（21MB 原始 ELF 跨境直传曾 180s
超时，压缩后约 7.8MB 仍需约 23 分钟）。

## 已知脚本限制

- `ok` 判据当前为“32/32 worker + 无 `no Pong` + 无重连 + 客户端存活 + 服务端 PID 非空”；
  `isolation_logged` 只记录字段，不在 `ok` 布尔条件内。本轮 10 轮已逐项核对全部为 true，
  但后续若要把它作为硬门禁，应改脚本并重跑。
- `server_alive` 是 `server_pid is not None` 的进程句柄判断，不是每轮远程 `pgrep` 探活；
  本轮所有轮 32/32 完成且有服务端日志，可作为服务端实际工作的证据，但不能替代严格探活。

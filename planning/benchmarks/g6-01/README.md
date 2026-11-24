# G6-01 QUIC 控制面隔离 spike 数据

日期：2026-11-24
脚本：`scripts/quic-isolation-spike.py`
二进制：`feat/v1.5` Linux release（版本串仍为 `meatus 1.4.0`）

## 环境

- 执行机：Linux VM，2 vCPU / 3.9GiB；服务端、客户端、UDP WAN 代理同机。
- WAN 代理：用户态 UDP 转发，双向固定延迟、1% 丢包、每方向速率限制。
- 每个 attempt 独立证书、echo 后端、服务端与客户端进程；临时目录内保留
  `server.log` / `client.log` / `client.toml` / `server.toml`。

## 计划口径 A/B

`g6-01-both.json`：32 并发 × 16KB × 10s，300ms RTT，1% 丢包，每方向 2Mbps，各 5 轮。

| 方案 | 成功 | 吞吐 | P50 | P99 | `no Pong` |
| --- | --- | --- | --- | --- | --- |
| baseline | 5/5 | 3.07–3.28 Mbps | 2.79–3.73s | 4.63–6.23s | 0 |
| isolated | 5/5 | 2.49–3.28 Mbps | 2.96–4.57s | 4.58–5.84s | 0 |

与 E5-08 的 3.2Mbps / P50 2.82s 同量级，但未复现 71% 失败。

## 故障注入

`fault-128/g6-01-both.json`：128 并发 × pipeline 16 × 每方向 2Mbps，各 2 轮。

- baseline：1/2 出现 `no Pong for 60s`；两轮数据面均大面积失败。
- isolated：2/2 出现 `no Pong`；数据面同样大面积失败。

结论：该压力已进入 CPU/运行时饱和区，独立控制连接不能消除控制任务饥饿。
另有 32 并发 × pipeline 64 × 1Mbps 的单次探针，两个方案均出现 `no Pong for 60s`
（日志在同次运行的临时目录，未单独汇总）。

## 复跑

```bash
python3 scripts/quic-isolation-spike.py \
  --variant both --attempts 5 --duration 10 --concurrency 32 \
  --pipeline 1 --rate-mbps 2 --delay-ms 150 --loss 0.01 \
  --out-dir planning/benchmarks/g6-01
```

## 边界

- 实验室为同机 loopback + 用户态代理，不等价于公网 NAT/MTU/运营商缓冲。
- `client.log` 中的 `no Pong` 是控制面饥饿的直接判据；`ok=false` 也可能仅由数据面
  worker 失败导致，两者需分开看。
- 公网 10 轮门禁属于 G6-03，本目录数据不能替代。
- 复跑注意：G6-02 起，QUIC 池关闭时会自动开 1 条独立数据连接，`--variant baseline`
  不再等于本目录的单连接共享基线。要复跑真正的 baseline，请使用 G6-01 提交
  `835335f` 构建的二进制。

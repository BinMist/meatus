# 教程检查点与常见卡点

## 1. 开课前检查

在开始前确认：

- 服务端是 Linux + systemd，还是本机 / Docker；`install.sh` 只支持 Linux。
- 服务器公网 IP 或域名已确定；用 IP 时证书 SAN 必须是 IP。
- `7000/tcp` 与计划使用的 `remote_port/tcp` 已在云安全组和系统防火墙放行。
- 运行 client 的本机，目标服务确实在监听，例如 `nc -vz 127.0.0.1 22`。
- 用户能拿到服务器文件：`/etc/meatus/client.token`、`/etc/meatus/certs/server.crt`。
- 管理面板 `7400` 不对公网开放。

端口检查：

```bash
sudo ss -ltnp | grep -E ':(7000|7400|20022)\b'
```

## 2. 每课检查点

| 课 | 通过标准 |
|---|---|
| A1 证书 / token | `certs/server.crt`、`certs/server.key` 存在；已保存 token 与 sha256 |
| A2 server | `meatus check -c server.toml` 输出 OK；`curl 127.0.0.1:7400/healthz` 成功 |
| A3 client | `meatus check -c client.toml` 输出 OK；日志出现 connected / proxy 注册成功 |
| A4 第一条隧道 | 浏览器打开 `http://127.0.0.1:20080` 能看到本地网页 |
| B1 公网安装 | `systemctl is-active meatus-server` 为 `active`；healthz 成功 |
| B2 公网 client | 本机 client 日志 connected；无认证 / 证书错误 |
| B3 SSH | `ssh -p 20022 <用户>@<公网IP>` 能登录到运行 client 的机器 |
| B4 面板 | SSH 隧道后 `http://127.0.0.1:7400` 能登录，看到 proxy 列表 |
| C Docker | `docker compose ps` 为 running；拿到 token 与 `server.crt` |
| D HTTP | `http://<custom_domain>` 返回本地网页，不是 404 |
| E 收尾 | `systemctl is-enabled meatus-server` 为 enabled；备份文件存在 |

## 3. 常见卡点

| 现象 | 先查 | 处理 |
|---|---|---|
| `authentication failed (1001)` | `client_id` 是否等于 `auth.clients[].id`；token 与 sha256 是否配对 | 重新 `gen-token`，更新两端，重启 server/client |
| `certificate verify failed` | `server_name` 是否等于证书 SAN；自签是否配 `server_ca_cert` | 用 `openssl x509 -in server.crt -noout -ext subjectAltName` 核对 |
| `connection refused` / 连不上 7000 | server `bind_addr`、`control_port`、安全组、防火墙、Docker 端口映射 | 先 `nc -vz <server> 7000`，再查 `ss -ltnp` |
| `remote_port` 被占用 / 端口池耗尽 | `[ports].range` 是否覆盖；端口是否被占；多个 client 是否抢同一端口 | 换端口或调整 range |
| HTTP 404 | `custom_domains` 与 `Host` 是否一致；`[http] enabled` 是否 true；client 是否在线 | 域名小写、无空白；重启两端 |
| 面板 401 | 是否配置了 `admin_token`；token 是否取对 | 读 `/etc/meatus/admin.token`，连续失败 5 次会锁 10 分钟 |
| 面板打不开 | 是否只绑 `127.0.0.1:7400`；SSH 隧道是否建立 | `ssh -L 7400:127.0.0.1:7400 ...`，先 curl healthz |
| YAML 只读 | 是否编辑了 `server.yaml` / `client.yaml` | 面板不写回 YAML；手工改文件后重启 |
| ACR 401 | 是否 `docker login registry.cn-hangzhou.aliyuncs.com` | ACR 是私有仓库，匿名拉取会 401 |
| `ghcr.io` 拉取失败 | GHCR 是否已发布 | 当前用 ACR，不要给 `ghcr.io` 地址 |
| Docker client 访问不到宿主机 | 目标地址是否 `127.0.0.1` | 用 `host.docker.internal` 或 `host-gateway` |
| `meatus status` HTTPS 失败 | `status` 是否只支持 http | 改用 `curl --cacert ... https://.../api/v1/status` |

## 4. 毕业验收

```bash
meatus --version
meatus check -c client.toml
curl -fsS http://127.0.0.1:7400/healthz
ssh -p 20022 <用户>@203.0.113.10
```

用户应能回答：

1. `server_addr` 写什么？答：服务端 `公网IP:7000`，不带协议。
2. `server_name` 写什么？答：证书 SAN，域名或 IP。
3. `local_addr` 和 `remote_port` 分别是什么？答：本机目标服务地址、公网映射端口。
4. 管理面板为什么不直接开公网？答：默认只绑回环，远程用 SSH 隧道，避免暴露管理面。

## 5. 教学节奏

- 用户说“好了”再进入下一课。
- 用户报错时，先让他贴出完整报错和当前步骤，不要猜。
- 一次只排一个变量：先确认 server 活着，再确认 client 连上，再确认端口，再确认目标服务。
- 不要为了省步骤跳过 `meatus check`；配置错误在这一步拦下最省时间。

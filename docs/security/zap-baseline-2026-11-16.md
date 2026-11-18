# Meatus 面板 ZAP 被动扫描记录

日期：2026-11-16
目标：`http://127.0.0.1:7410`（admin token 测试实例）
引擎：OWASP ZAP 2.17.0（Maven Central core jar）+ `callhome` 0.23.0、`network` 0.33.0、`commonlib` 1.44.0、`pscanrules` 78、`ascanrules` 84、`pscan` 0.2.0

## 执行方式

因 Docker Hub / GHCR 不可达，未使用官方镜像与 `zap-baseline.py`。以 ZAP daemon + API 对面板首页、3 个静态资源、`/api/v1/status`、`/healthz`、404 路径共 8 个 URL 调用 `core/action/accessUrl`，等待 passive scan 队列归零后导出 `core/view/alerts`。

## 结果

| 风险 | 规则 | URL | 说明 |
|---|---|---|---|
| Medium | CSP: style-src unsafe-inline | `http://127.0.0.1:7410/` | CSP 为兼容组件运行时内联样式保留 `style-src 'unsafe-inline'`；无 High 风险 |
| Informational | Modern Web Application | `http://127.0.0.1:7410/` | SPA 识别，无需处理 |

- 未授权 `/api/v1/*` 返回 401，面板 4 个安全响应头（`X-Frame-Options`、`nosniff`、`Referrer-Policy`、CSP）均存在。
- 本次为**被动扫描**，spider 与 report add-on 未取到，未生成官方 HTML baseline 报告；完整 baseline 仍列为发布前外部环境待补项。
- 同一实例另有 `scripts/security-smoke.sh` 9 项实跑全 PASS。

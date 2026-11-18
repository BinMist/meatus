#!/usr/bin/env bash
# V1 面板安全冒烟（ZAP baseline 不可用时的本机替代证据，不代替 ZAP）。
# 用法：./scripts/security-smoke.sh [admin_url] [admin_token]
set -euo pipefail

ADMIN="${1:-http://127.0.0.1:7400}"
TOKEN="${2:-${MEATUS_ADMIN_TOKEN:-}}"
[[ -n "$TOKEN" ]] || { echo "需要 admin token" >&2; exit 2; }
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
FAIL=0; pass(){ echo "PASS  $1"; }; fail(){ echo "FAIL  $1"; FAIL=1; }

code() { curl -sS -o /dev/null -w '%{http_code}' "$@"; }

# 1. 未授权 API 必须 401
C="$(code "$ADMIN/api/v1/status")"
[[ "$C" == 401 ]] && pass "unauth status 401" || fail "unauth status=$C"

# 2. 登录，拿 cookie 与 csrf
curl -sS -c "$TMP/cookies" -H 'Content-Type: application/json' \
  -d "{\"token\":\"$TOKEN\"}" "$ADMIN/api/v1/session" > "$TMP/login.json"
grep -q '"csrf"' "$TMP/login.json" && pass "login + csrf" || fail "login response missing csrf"
CSRF="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["csrf"])' "$TMP/login.json")"

# 3. 面板与安全响应头
curl -sS -D "$TMP/head" -o /dev/null "$ADMIN/"
for h in 'x-frame-options: DENY' 'x-content-type-options: nosniff' 'referrer-policy: no-referrer' 'content-security-policy:'; do
    grep -qi "$h" "$TMP/head" && pass "header $h" || fail "missing header $h"
done

# 4. 写接口无 CSRF 403、带 CSRF 不因 CSRF 被拒（空 body 返回 400）
C="$(code -b "$TMP/cookies" -H 'Content-Type: application/json' -d '{}' "$ADMIN/api/v1/proxies")"
[[ "$C" == 403 ]] && pass "write without csrf 403" || fail "write without csrf=$C"
C="$(code -b "$TMP/cookies" -H 'Content-Type: application/json' -H "X-Meatus-CSRF: $CSRF" -d '{}' "$ADMIN/api/v1/proxies")"
[[ "$C" == 400 ]] && pass "write with csrf reaches validation 400" || fail "write with csrf=$C"

# 5. 登出后 cookie 失效
code -b "$TMP/cookies" -H "X-Meatus-CSRF: $CSRF" -X DELETE "$ADMIN/api/v1/session" >/dev/null || true
C="$(code -b "$TMP/cookies" "$ADMIN/api/v1/status")"
[[ "$C" == 401 ]] && pass "logout invalidates session" || fail "post-logout status=$C"

echo
if [[ "$FAIL" -eq 0 ]]; then echo "RESULT: PASS"; else echo "RESULT: FAIL"; exit 1; fi

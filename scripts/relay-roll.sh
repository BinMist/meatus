#!/usr/bin/env bash
# 逐台滚动 drain / upgrade / rollback relay。
#
# 目标格式（6 段，用 | 分隔）：
#   name|ready_url|stop_cmd|upgrade_cmd|start_cmd|rollback_cmd
#
# - stop_cmd 必须阻塞到旧进程退出；systemd 用 `systemctl stop <unit>`。
# - upgrade_cmd 在旧进程退出后、start_cmd 前执行；空命令写 `-`。
# - rollback_cmd 在新版本起不来时执行，随后仍走 start_cmd；空命令写 `-`。
# - 远端目标把 ssh 写进各命令，例如：
#     relay-a|http://10.0.0.2:7402/healthz|ssh root@10.0.0.2 systemctl stop meatus|\
#       ssh root@10.0.0.2 /opt/meatus/install.sh --version 1.5.0|\
#       ssh root@10.0.0.2 systemctl start meatus|\
#       ssh root@10.0.0.2 /opt/meatus/install.sh --version 1.4.0
#
# readiness 约定：`/healthz` 在 preparing/draining 阶段返回 503。脚本要求
# 每个目标滚动后 status=ok，且 relay 快照存在时 rotation.state=complete。
# 因此前置 LB/DNS 必须以 `/healthz` 做主动健康检查，检查间隔小于
# `server.drain_grace_s`（默认 5s）。

set -euo pipefail

readonly SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
readonly REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGETS=()
TIMEOUT_SECS=45
SETTLE_SECS=60
POLL_SECS=1
DRY_RUN=0
LOAD_CMD=""
LOAD_DELAY_SECS=2
REPORT=""
LOG_FILE=""
WORK_DIR=""

usage() {
    cat <<'USAGE'
用法:
  scripts/relay-roll.sh --target '<spec>' [--target '<spec>'] [选项]

目标 spec:
  name|ready_url|stop_cmd|upgrade_cmd|start_cmd|rollback_cmd
  命令为空写 -；stop_cmd 必须阻塞到旧进程退出。

选项:
  --target SPEC        可重复；按给定顺序逐台滚动（建议 secondary 在前）
  --timeout SECS       stop_cmd 超时，默认 45（覆盖 drain_grace_s + 30s drain）
  --settle SECS        单台重启后等待健康的上限，默认 60
  --poll SECS          健康轮询间隔，默认 1
  --load-cmd CMD       滚动期间后台运行的 1000 请求验收命令；退出码必须为 0
  --load-delay SECS    启动 load-cmd 后等待多久再开始滚动，默认 2
  --dry-run            只打印计划
  --report FILE        写 JSON 报告
  --log FILE           写滚动日志；默认 relay-roll-<UTC>.log
  -h, --help           显示帮助

示例:
  scripts/relay-roll.sh \
    --target 'relay-b|http://10.0.0.2:7402/healthz|ssh root@10.0.0.2 systemctl stop meatus|ssh root@10.0.0.2 /opt/meatus/install.sh --version 1.5.0|ssh root@10.0.0.2 systemctl start meatus|ssh root@10.0.0.2 /opt/meatus/install.sh --version 1.4.0' \
    --target 'relay-a|http://10.0.0.1:7401/healthz|ssh root@10.0.0.1 systemctl stop meatus|ssh root@10.0.0.1 /opt/meatus/install.sh --version 1.5.0|ssh root@10.0.0.1 systemctl start meatus|ssh root@10.0.0.1 /opt/meatus/install.sh --version 1.4.0' \
    --load-cmd 'python3 scripts/relay-roll-load.py --url http://lb.example.com --count 1000'
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            [[ $# -ge 2 ]] || { echo "missing value for --target" >&2; exit 2; }
            TARGETS+=("$2")
            shift 2
            ;;
        --timeout) TIMEOUT_SECS="$2"; shift 2 ;;
        --settle) SETTLE_SECS="$2"; shift 2 ;;
        --poll) POLL_SECS="$2"; shift 2 ;;
        --load-cmd) LOAD_CMD="$2"; shift 2 ;;
        --load-delay) LOAD_DELAY_SECS="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --report) REPORT="$2"; shift 2 ;;
        --log) LOG_FILE="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ ${#TARGETS[@]} -eq 0 ]]; then
    echo "at least one --target is required" >&2
    usage >&2
    exit 2
fi

for numeric in "$TIMEOUT_SECS" "$SETTLE_SECS" "$POLL_SECS" "$LOAD_DELAY_SECS"; do
    [[ "$numeric" =~ ^[0-9]+$ ]] || { echo "numeric option expected, got $numeric" >&2; exit 2; }
done

umask 077
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/meatus-relay-roll.XXXXXX")"
LOAD_PID=""
LOAD_EXIT_FILE="$WORK_DIR/load.exit"
LOAD_LOG="$WORK_DIR/load.log"

cleanup() {
    if [[ -n "$LOAD_PID" ]] && kill -0 "$LOAD_PID" 2>/dev/null; then
        kill "$LOAD_PID" 2>/dev/null || true
        wait "$LOAD_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

if [[ -z "$LOG_FILE" ]]; then
    LOG_FILE="$REPO_ROOT/relay-roll-$(date -u +%Y%m%dT%H%M%SZ).log"
fi
mkdir -p "$(dirname "$LOG_FILE")"
exec > >(tee -a "$LOG_FILE") 2>&1

log() {
    printf '[%s] %s\n' "$(date -u +%H:%M:%SZ)" "$*"
}

die() {
    log "ERROR: $*"
    exit 1
}

run_cmd() {
    local label="$1" timeout_s="$2" command="$3"
    [[ "$command" == "-" ]] && return 0
    log "  $label: $command"
    if command -v timeout >/dev/null 2>&1; then
        timeout "$timeout_s" bash -lc "$command"
    else
        # macOS 默认没有 GNU timeout；用后台进程 + 轮询实现同等超时。
        bash -lc "$command" &
        local pid=$!
        local waited=0
        while kill -0 "$pid" 2>/dev/null; do
            if (( waited >= timeout_s )); then
                kill -TERM "$pid" 2>/dev/null || true
                sleep 1
                kill -KILL "$pid" 2>/dev/null || true
                wait "$pid" 2>/dev/null || true
                log "  $label timed out after ${timeout_s}s"
                return 124
            fi
            sleep 1
            waited=$((waited + 1))
        done
        wait "$pid"
    fi
}

# 输出: code<TAB>status<TAB>relay_state<TAB>rotation_state
health_tuple() {
    local url="$1" code body
    body="$WORK_DIR/health.json"
    code="$(curl -k -sS --max-time 3 -o "$body" -w '%{http_code}' "$url" 2>/dev/null || true)"
    [[ "$code" =~ ^[0-9]{3}$ ]] || code="000"
    python3 - "$code" "$body" <<'PY'
import json
import sys

code = sys.argv[1]
try:
    with open(sys.argv[2], "r", encoding="utf-8") as handle:
        payload = json.load(handle)
except Exception:
    print(f"{code}\t-\t-\t-")
    raise SystemExit(0)

relay = payload.get("relay") or {}
rotation = relay.get("rotation") or {}
print("\t".join([
    code,
    str(payload.get("status", "-")),
    str(relay.get("state", "-")),
    str(rotation.get("state", "-")),
]))
PY
}

is_healthy_body() {
    local code="$1" status="$2" relay_state="$3" rotation="$4"
    [[ "$code" == "200" && "$status" == "ok" ]] || return 1
    if [[ "$relay_state" != "-" ]]; then
        [[ "$relay_state" == "healthy" ]] || return 1
        [[ "$rotation" == "complete" ]] || return 1
    fi
    return 0
}

wait_healthy() {
    local name="$1" url="$2" limit="$3"
    local deadline=$((SECONDS + limit)) code status relay_state rotation
    while (( SECONDS < deadline )); do
        IFS=$'\t' read -r code status relay_state rotation < <(health_tuple "$url")
        if is_healthy_body "$code" "$status" "$relay_state" "$rotation"; then
            log "  $name healthy (relay=${relay_state}, rotation=${rotation})"
            return 0
        fi
        sleep "$POLL_SECS"
    done
    IFS=$'\t' read -r code status relay_state rotation < <(health_tuple "$url")
    log "  $name not healthy after ${limit}s (code=${code}, status=${status}, relay=${relay_state}, rotation=${rotation})"
    return 1
}

wait_unreachable() {
    local name="$1" url="$2" limit="$3"
    local deadline=$((SECONDS + limit)) code status relay_state rotation
    while (( SECONDS < deadline )); do
        IFS=$'\t' read -r code status relay_state rotation < <(health_tuple "$url")
        if [[ "$code" != "200" && "$code" != "503" ]]; then
            log "  $name stopped serving (health code=${code})"
            return 0
        fi
        sleep "$POLL_SECS"
    done
    log "  $name still serving after ${limit}s; drain did not finish"
    return 1
}

validate_target() {
    local spec="$1" name url stop_cmd upgrade_cmd start_cmd rollback_cmd
    IFS='|' read -r name url stop_cmd upgrade_cmd start_cmd rollback_cmd <<<"$spec"
    [[ -n "$name" && -n "$url" && -n "$stop_cmd" && -n "$start_cmd" ]] || {
        die "target spec must be name|ready_url|stop_cmd|upgrade_cmd|start_cmd|rollback_cmd: $spec"
    }
    [[ "$url" == http://* || "$url" == https://* ]] || die "target $name has invalid ready_url: $url"
}

parse_target() {
    local spec="$1"
    IFS='|' read -r T_NAME T_URL T_STOP T_UPGRADE T_START T_ROLLBACK <<<"$spec"
}

load_running=0
if [[ -n "$LOAD_CMD" ]]; then
    if [[ "$DRY_RUN" == "1" ]]; then
        log "dry-run load-cmd: $LOAD_CMD"
    else
        log "starting load: $LOAD_CMD"
        (
            set +e
            bash -lc "$LOAD_CMD" >"$LOAD_LOG" 2>&1
            printf '%s\n' "$?" >"$LOAD_EXIT_FILE"
        ) &
        LOAD_PID=$!
        load_running=1
        sleep "$LOAD_DELAY_SECS"
    fi
fi

# 先全量预检，避免只滚到一半才发现另一台本来就不可用。
if [[ "$DRY_RUN" != "1" ]]; then
    log "preflight all targets"
    for spec in "${TARGETS[@]}"; do
        validate_target "$spec"
        parse_target "$spec"
        wait_healthy "$T_NAME" "$T_URL" "$SETTLE_SECS" || die "preflight failed for $T_NAME"
    done
else
    for spec in "${TARGETS[@]}"; do
        validate_target "$spec"
        parse_target "$spec"
        log "plan $T_NAME: stop=[$T_STOP] upgrade=[$T_UPGRADE] start=[$T_START] rollback=[$T_ROLLBACK]"
    done
fi
[[ "$DRY_RUN" == "1" ]] && { log "dry-run complete"; exit 0; }

STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
ROLL_START=$SECONDS
RESULTS="$WORK_DIR/results.tsv"
: >"$RESULTS"
FAILED=0

for spec in "${TARGETS[@]}"; do
    parse_target "$spec"
    name="$T_NAME"
    log "rolling $name"
    target_start=$SECONDS
    rollback_used=0
    rollback_secs=0

    if ! run_cmd "stop" "$TIMEOUT_SECS" "$T_STOP"; then
        printf '%s\tstop_failed\t%s\t0\n' "$name" "$((SECONDS - target_start))" >>"$RESULTS"
        FAILED=1
        break
    fi
    if ! wait_unreachable "$name" "$T_URL" "$TIMEOUT_SECS"; then
        printf '%s\tdrain_timeout\t%s\t0\n' "$name" "$((SECONDS - target_start))" >>"$RESULTS"
        FAILED=1
        break
    fi

    if ! run_cmd "upgrade" "$TIMEOUT_SECS" "$T_UPGRADE"; then
        log "  upgrade failed for $name; rolling back"
        rollback_start=$SECONDS
        rollback_used=1
        if ! run_cmd "rollback" "$TIMEOUT_SECS" "$T_ROLLBACK"; then
            printf '%s\tupgrade_and_rollback_failed\t%s\t0\n' "$name" "$((SECONDS - target_start))" >>"$RESULTS"
            FAILED=1
            break
        fi
    fi

    if ! run_cmd "start" "$TIMEOUT_SECS" "$T_START"; then
        log "  start failed for $name"
        if [[ "$rollback_used" == "0" ]]; then
            log "  rolling back $name"
            rollback_start=$SECONDS
            rollback_used=1
            run_cmd "rollback" "$TIMEOUT_SECS" "$T_ROLLBACK" || true
            if ! run_cmd "start-old" "$TIMEOUT_SECS" "$T_START"; then
                printf '%s\tstart_and_rollback_failed\t%s\t0\n' "$name" "$((SECONDS - target_start))" >>"$RESULTS"
                FAILED=1
                break
            fi
        else
            printf '%s\trollback_start_failed\t%s\t%s\n' "$name" "$((SECONDS - target_start))" "$rollback_secs" >>"$RESULTS"
            FAILED=1
            break
        fi
    fi

    if ! wait_healthy "$name" "$T_URL" "$SETTLE_SECS"; then
        if [[ "$rollback_used" == "1" ]]; then
            printf '%s\trollback_unhealthy\t%s\t%s\n' "$name" "$((SECONDS - target_start))" "$rollback_secs" >>"$RESULTS"
            FAILED=1
            break
        fi
        log "  new version unhealthy; rolling back $name"
        rollback_start=$SECONDS
        rollback_used=1
        run_cmd "stop-bad" "$TIMEOUT_SECS" "$T_STOP" || true
        if ! run_cmd "rollback" "$TIMEOUT_SECS" "$T_ROLLBACK" \
            || ! run_cmd "start-old" "$TIMEOUT_SECS" "$T_START" \
            || ! wait_healthy "$name" "$T_URL" "$SETTLE_SECS"; then
            printf '%s\trollback_failed\t%s\t0\n' "$name" "$((SECONDS - target_start))" >>"$RESULTS"
            FAILED=1
            break
        fi
    fi

    if [[ "$rollback_used" == "1" ]]; then
        rollback_secs=$((SECONDS - rollback_start))
    fi
    printf '%s\tok\t%s\t%s\n' "$name" "$((SECONDS - target_start))" "$rollback_secs" >>"$RESULTS"
    log "  $name done in $((SECONDS - target_start))s (rollback_used=${rollback_used})"
done

load_exit=0
if [[ "$load_running" == "1" ]]; then
    log "waiting for load command"
    wait "$LOAD_PID" || true
    LOAD_PID=""
    if [[ -f "$LOAD_EXIT_FILE" ]]; then
        load_exit="$(cat "$LOAD_EXIT_FILE")"
    else
        load_exit=1
    fi
    log "load exit=$load_exit"
    sed 's/^/  load| /' "$LOAD_LOG" | tail -n 40 || true
    [[ "$load_exit" == "0" ]] || FAILED=1
fi

log "final health check"
for spec in "${TARGETS[@]}"; do
    parse_target "$spec"
    if ! wait_healthy "$T_NAME" "$T_URL" 15; then
        FAILED=1
    fi
done

FINISHED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
TOTAL_SECS=$((SECONDS - ROLL_START))

if [[ -n "$REPORT" ]]; then
    mkdir -p "$(dirname "$REPORT")"
    python3 - "$RESULTS" "$REPORT" "$STARTED_AT" "$FINISHED_AT" "$TOTAL_SECS" "$load_exit" "$FAILED" <<'PY'
import json
import sys

results_path, report_path, started, finished, total, load_exit, failed = sys.argv[1:]
targets = []
with open(results_path, "r", encoding="utf-8") as handle:
    for line in handle:
        name, status, secs, rollback = line.rstrip("\n").split("\t")
        targets.append({
            "name": name,
            "status": status,
            "seconds": int(secs),
            "rollback_seconds": int(rollback),
        })
payload = {
    "started_at": started,
    "finished_at": finished,
    "total_seconds": int(total),
    "load_exit": int(load_exit),
    "ok": int(failed) == 0,
    "targets": targets,
}
with open(report_path, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
print(json.dumps(payload, ensure_ascii=False, indent=2))
PY
fi

if [[ "$FAILED" != "0" ]]; then
    log "roll FAILED in ${TOTAL_SECS}s; log $LOG_FILE"
    exit 1
fi
log "roll OK in ${TOTAL_SECS}s; log $LOG_FILE"

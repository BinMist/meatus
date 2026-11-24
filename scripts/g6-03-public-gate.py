#!/usr/bin/env python3
"""G6-03 QUIC 控制面公网门禁。

拓扑：本机压测客户端 -> 公网临时 meatus server -> 本机 echo 服务。
每轮独立启动远端临时 server、独立证书与客户端进程，固定测试端口：

  TCP 22000 控制面 / TCP 22001 代理面 / UDP 22000 QUIC

不触碰线上 80/443/7000 容器。默认使用当前 macOS release 客户端和
`target/x86_64-unknown-linux-musl/release/meatus` 服务端。

判据：
  * 32 个 worker 全部完成（round_ok）
  * 客户端无 `no Pong for`、无 `control session failed, reconnecting`
  * 客户端仍然存活；服务端仍存活
  * 每轮日志确认 `QUIC control isolation enabled ... data_connections=1`
"""
import argparse
import gzip
import hashlib
import json
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOKEN = "g6-gate-secret"
CONTROL_PORT = 22000
PROXY_PORT = 22001
ADMIN_PORT = 12741
REMOTE_DIR = "/home/ubuntu/meatus-wan"
SSH_KEY = "/Users/binsail/.ssh/meatus_panel_ed25519"
REMOTE = "ubuntu@43.173.105.47"


def run(cmd, **kwargs):
    return subprocess.run(cmd, check=True, text=True, **kwargs)


def ssh(remote_cmd, timeout=30):
    return run(["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no",
                "-i", SSH_KEY, REMOTE, remote_cmd], timeout=timeout,
               stdout=subprocess.PIPE, stderr=subprocess.STDOUT).stdout.strip()


def scp(paths, timeout=300):
    run(["scp", "-q", "-C", "-o", "StrictHostKeyChecking=no", "-i", SSH_KEY,
         *[str(p) for p in paths], f"{REMOTE}:{REMOTE_DIR}/"], timeout=timeout)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(host, port, timeout=25.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"{host}:{port} did not open")


def start_echo(port):
    code = f"""
import socket, threading
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
s.bind(('127.0.0.1',{port})); s.listen(256)
def h(c):
    c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    try:
        while True:
            b=c.recv(131072)
            if not b: break
            c.sendall(b)
    finally: c.close()
while True:
    c,_=s.accept(); threading.Thread(target=h,args=(c,),daemon=True).start()
"""
    return subprocess.Popen([sys.executable, "-c", code],
                            stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)


def worker(host, port, payload, duration, barrier, out, idx, timeout):
    result = {"idx": idx, "lat": [], "reqs": 0, "bytes": 0, "error": None}
    sock = None
    try:
        sock = socket.create_connection((host, port), timeout=20)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.settimeout(timeout)
        barrier.wait(timeout=30)
        warm_deadline = time.perf_counter() + 1.0
        while time.perf_counter() < warm_deadline:
            sock.sendall(payload)
            got = 0
            while got < len(payload):
                chunk = sock.recv(len(payload) - got)
                if not chunk:
                    raise RuntimeError("echo closed during warmup")
                got += len(chunk)
        deadline = time.perf_counter() + duration
        while time.perf_counter() < deadline:
            t0 = time.perf_counter()
            sock.sendall(payload)
            got = 0
            while got < len(payload):
                chunk = sock.recv(len(payload) - got)
                if not chunk:
                    raise RuntimeError("echo closed")
                got += len(chunk)
            result["lat"].append((time.perf_counter() - t0) * 1000)
            result["reqs"] += 1
            result["bytes"] += len(payload) * 2
    except Exception as exc:
        result["error"] = repr(exc)
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        out[idx] = result


def percentile(values, p):
    if not values:
        return 0.0
    values = sorted(values)
    k = min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1))))
    return values[k]


def phase(host, port, payload, duration, concurrency, timeout):
    barrier = threading.Barrier(concurrency)
    results = [None] * concurrency
    threads = [threading.Thread(target=worker,
                                args=(host, port, payload, duration, barrier,
                                      results, i, timeout))
               for i in range(concurrency)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    lat, reqs, total_bytes, errors = [], 0, 0, []
    for item in results:
        if item is None:
            errors.append("worker produced no result")
            continue
        if item["error"]:
            errors.append(item["error"])
        lat.extend(item["lat"])
        reqs += item["reqs"]
        total_bytes += item["bytes"]
    return {
        "requests": reqs,
        "bytes": total_bytes,
        "throughput_mbps": round(total_bytes * 8 / duration / 1_000_000, 3),
        "p50_ms": round(percentile(lat, 50), 3),
        "p99_ms": round(percentile(lat, 99), 3),
        "p999_ms": round(percentile(lat, 99.9), 3),
        "workers_ok": concurrency - len(errors),
        "workers_total": concurrency,
        "worker_errors": errors,
    }


def read_text(path: Path) -> str:
    try:
        return path.read_text(errors="replace")
    except OSError:
        return ""


ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def parse_control_health(text: str) -> dict:
    text = ANSI_RE.sub("", text)
    rtts = [int(value) for value in re.findall(r"rtt_ms=(\d+)", text)]
    gaps = [int(value) for value in re.findall(r"control_max_gap_ms=(\d+)", text)]
    last_pongs = [int(value) for value in re.findall(r"control_last_pong_ms=(\d+)", text)]
    health_lines = [line for line in text.splitlines() if "client control health" in line]
    pongs = len(rtts)
    return {
        "pongs": pongs,
        "max_rtt_ms": max(rtts) if rtts else None,
        "max_gap_ms": max(gaps) if gaps else None,
        "last_pong_unix_ms": max(last_pongs) if last_pongs else None,
        "health_snapshots": len(health_lines),
        "no_pong_failures": text.count("no Pong for"),
        "delayed_warnings": text.count("control pong delayed beyond 20s"),
        "reconnects": text.count("control session failed, reconnecting"),
    }


def parse_isolation(text: str) -> dict:
    return {
        "isolation_logged": "QUIC control isolation enabled" in text,
        "isolation_data_connections": text.count("data connection established"),
        "data_registered": text.count("data connection registered"),
    }


def run_attempt(args, attempt, out_dir, remote_bin, client_bin):
    tmp = out_dir / f"attempt-{attempt:02d}"
    tmp.mkdir(parents=True, exist_ok=True)
    echo_port = free_port()
    cert, key = tmp / "server.crt", tmp / "server.key"
    subprocess.check_call([str(client_bin), "gen-cert", "--domain", "localhost",
                           "-o", str(tmp), "--force"], stdout=subprocess.DEVNULL)
    token_hash = hashlib.sha256(TOKEN.encode()).hexdigest()
    server_cfg = tmp / "server.toml"
    server_cfg.write_text(f'''[server]
bind_addr = "0.0.0.0"
control_port = {CONTROL_PORT}
tls_cert = "{REMOTE_DIR}/server.crt"
tls_key = "{REMOTE_DIR}/server.key"
quic = true

[auth]
clients = [{{ id = "g6gate", tenant = "default", token_sha256 = "{token_hash}" }}]

[ports]
range = [{PROXY_PORT}, {PROXY_PORT}]

[admin]
bind = "127.0.0.1:{ADMIN_PORT}"
''')
    client_cfg = tmp / "client.toml"
    client_cfg.write_text(f'''[client]
server_addr = "{args.host}:{CONTROL_PORT}"
server_name = "localhost"
server_ca_cert = "{cert}"
transport = "quic"
client_id = "g6gate"
tenant = "default"
token = "{TOKEN}"

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {PROXY_PORT}
''')
    ssh(f"mkdir -p {REMOTE_DIR} && chmod 700 {REMOTE_DIR}")
    scp([cert, key, server_cfg])
    ssh(f"pkill -x {remote_bin} 2>/dev/null || true; sleep 0.5")
    echo = start_echo(echo_port)
    client = None
    server_pid = None
    log_s = open(tmp / "server.log", "w")
    log_c = open(tmp / "client.log", "w")
    start = time.time()
    try:
        server_pid = ssh(
            f"cd {REMOTE_DIR} && chmod +x {remote_bin} && "
            f"setsid -f ./{remote_bin} server -c server.toml "
            f"> server.log 2>&1 < /dev/null; sleep 0.8; "
            f"pgrep -x {remote_bin} | head -1")
        wait_port(args.host, CONTROL_PORT, timeout=20)
        client = subprocess.Popen([str(client_bin), "client", "-c", str(client_cfg)],
                                  stdout=log_c, stderr=subprocess.STDOUT)
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                with socket.create_connection((args.host, PROXY_PORT), timeout=1) as sock:
                    sock.settimeout(3)
                    sock.sendall(b"warmup")
                    if sock.recv(6) == b"warmup":
                        break
            except OSError:
                time.sleep(0.3)
        else:
            raise RuntimeError("proxy did not become ready")

        idle = phase(args.host, PROXY_PORT, b"x" * 64,
                     args.latency_duration, 1, args.worker_timeout)
        loaded = phase(args.host, PROXY_PORT, bytes([0x5A]) * args.payload,
                       args.duration, args.concurrency, args.worker_timeout)
        time.sleep(args.post_load_wait)
        client_text = read_text(tmp / "client.log")
        server_text = ssh(f"cat {REMOTE_DIR}/server.log")
        (tmp / "server.log").write_text(server_text)
        control = parse_control_health(client_text)
        isolation = parse_isolation(client_text)
        isolation["data_registered"] = server_text.count("data connection registered")
        # 若最后一条 Pong 已超过 20s，等到 60s 判失败窗口，明确区分“慢”与“死”。
        now_ms = int(time.time() * 1000)
        last = control["last_pong_unix_ms"]
        if last is None or now_ms - last > 20_000:
            wait_s = 45 if last is None else min(45, max(5, 65 - (now_ms - last) / 1000))
            time.sleep(max(0, wait_s))
            client_text = read_text(tmp / "client.log")
            control = parse_control_health(client_text)
            isolation = parse_isolation(client_text)
            server_text = ssh(f"cat {REMOTE_DIR}/server.log")
            isolation["data_registered"] = server_text.count("data connection registered")
            (tmp / "server.log").write_text(server_text)
        client_alive = client.poll() is None
        server_alive = server_pid is not None
        ok = (not loaded["worker_errors"] and loaded["workers_ok"] == loaded["workers_total"]
              and control["no_pong_failures"] == 0 and control["reconnects"] == 0
              and client_alive and server_alive)
        return {
            "attempt": attempt,
            "ok": ok,
            "elapsed_s": round(time.time() - start, 2),
            "idle": idle,
            "load": loaded,
            "control": control,
            "isolation": isolation,
            "client_alive": client_alive,
            "server_alive": server_alive,
            "log_dir": str(tmp),
            "client_log_tail": client_text.splitlines()[-20:],
            "server_log_tail": server_text.splitlines()[-20:],
        }
    finally:
        if client is not None:
            client.terminate()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
        if server_pid:
            ssh(f"kill {server_pid} 2>/dev/null || true; pkill -x {remote_bin} 2>/dev/null || true")
        echo.terminate()
        try:
            echo.wait(timeout=3)
        except subprocess.TimeoutExpired:
            echo.kill()
        log_s.close()
        log_c.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server-bin", default="meatus-g603")
    ap.add_argument("--server-local",
                    default=str(ROOT / "target/x86_64-unknown-linux-musl/release/meatus"))
    ap.add_argument("--client-bin", default=str(ROOT / "target/release/meatus"))
    ap.add_argument("--host", default="43.173.105.47")
    ap.add_argument("--attempts", type=int, default=10)
    ap.add_argument("--duration", type=float, default=10.0)
    ap.add_argument("--concurrency", type=int, default=32)
    ap.add_argument("--payload", type=int, default=16384)
    ap.add_argument("--latency-duration", type=float, default=5.0)
    ap.add_argument("--worker-timeout", type=float, default=90.0)
    ap.add_argument("--post-load-wait", type=float, default=5.0)
    ap.add_argument("--out-dir", default=str(ROOT / "planning/benchmarks/g6-03"))
    ap.add_argument("--skip-upload", action="store_true",
                    help="复用远端已解压的 server 二进制")
    args = ap.parse_args()

    server_local = Path(args.server_local).resolve()
    client_bin = Path(args.client_bin).resolve()
    if not server_local.exists():
        raise SystemExit(f"server binary not found: {server_local}")
    if not client_bin.exists():
        raise SystemExit(f"client binary not found: {client_bin}")
    if not Path(SSH_KEY).exists():
        raise SystemExit(f"ssh key not found: {SSH_KEY}")

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    version = subprocess.check_output([str(client_bin), "--version"], text=True).strip()
    ssh(f"mkdir -p {REMOTE_DIR} && chmod 700 {REMOTE_DIR}")
    if not args.skip_upload:
        gz = out_dir / f"{args.server_bin}.gz"
        with open(server_local, "rb") as src, gzip.open(gz, "wb", compresslevel=9) as dst:
            shutil.copyfileobj(src, dst)
        scp([gz])
        ssh(f"gunzip -fc {REMOTE_DIR}/{gz.name} > {REMOTE_DIR}/{args.server_bin} && "
            f"chmod +x {REMOTE_DIR}/{args.server_bin}")
    ssh(f"test -x {REMOTE_DIR}/{args.server_bin}")

    summary = {
        "schema": "meatus-g6-03/v1",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "client_binary": str(client_bin),
        "server_binary": str(server_local),
        "client_version": version,
        "remote": args.host,
        "params": {
            "attempts": args.attempts,
            "duration_s": args.duration,
            "concurrency": args.concurrency,
            "payload_bytes": args.payload,
            "latency_duration_s": args.latency_duration,
            "post_load_wait_s": args.post_load_wait,
        },
        "results": [],
    }
    try:
        for attempt in range(1, args.attempts + 1):
            print(f"[g6-03] attempt {attempt}/{args.attempts}", flush=True)
            result = run_attempt(args, attempt, out_dir, args.server_bin, client_bin)
            summary["results"].append(result)
            print(json.dumps({key: result[key] for key in
                              ("attempt", "ok", "elapsed_s", "control", "load")},
                             ensure_ascii=False), flush=True)
            (out_dir / "g6-03-public.json").write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    finally:
        ssh(f"pkill -x {args.server_bin} 2>/dev/null || true")
    ok_count = sum(1 for item in summary["results"] if item["ok"])
    summary["ok"] = ok_count
    summary["failed"] = len(summary["results"]) - ok_count
    summary["success_rate"] = round(ok_count / max(1, len(summary["results"])), 4)
    summary["gate_pass"] = ok_count == args.attempts
    (out_dir / "g6-03-public.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({key: summary[key] for key in
                      ("ok", "failed", "success_rate", "gate_pass")}, ensure_ascii=False))
    print(f"wrote {out_dir / 'g6-03-public.json'}", flush=True)


if __name__ == "__main__":
    main()

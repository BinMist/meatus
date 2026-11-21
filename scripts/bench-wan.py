#!/usr/bin/env python3
"""公网端到端基准：本机客户端 -> 公网服务端 -> 本机 echo 服务。

与 bench.py 的 loopback 场景不同，这里服务端运行在远端主机，压测客户端从本机
经公网连接服务端代理端口。输出保持 meatus-bench/v1 的顶层字段，便于复用
bench-gate.py；额外记录 idle RTT 与公网 TCP 握手基线。

用法：
  scripts/bench-wan.py --server-bin meatus-1.4 --client-bin target/release/meatus \
      --transport tcp --out planning/benchmarks/wan/tcp-1.4.json
"""
import argparse
import hashlib
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

TOKEN = "wanbench-secret"
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


def scp(paths):
    run(["scp", "-q", "-C", "-o", "StrictHostKeyChecking=no", "-i", SSH_KEY,
         *[str(p) for p in paths], f"{REMOTE}:{REMOTE_DIR}/"], timeout=120)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(host, port, timeout=20.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((host, port), timeout=0.5):
                return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError(f"{host}:{port} did not open")


def connect_ms(host, port, samples=10):
    vals = []
    for _ in range(samples):
        t0 = time.perf_counter()
        with socket.create_connection((host, port), timeout=5):
            vals.append((time.perf_counter() - t0) * 1000)
    vals.sort()
    return round(vals[len(vals) // 2], 3), round(vals[-1], 3)


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


def worker(host, port, payload, duration, barrier, out, idx):
    try:
        sock = socket.create_connection((host, port), timeout=20)
    except Exception as exc:
        out[idx] = ("ERR", repr(exc), 0, 0)
        barrier.abort()
        return
    try:
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.settimeout(60)
        barrier.wait(timeout=30)
        # 1s 预热，避免 TLS/隧道建连计入吞吐。
        warm_deadline = time.perf_counter() + 1.0
        while time.perf_counter() < warm_deadline:
            sock.sendall(payload)
            got = 0
            while got < len(payload):
                chunk = sock.recv(len(payload) - got)
                if not chunk:
                    raise RuntimeError("echo closed during warmup")
                got += len(chunk)
        lat, reqs, bts = [], 0, 0
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
            lat.append((time.perf_counter() - t0) * 1000)
            reqs += 1
            bts += len(payload) * 2
        out[idx] = (lat, reqs, bts)
    except Exception as exc:
        out[idx] = ("ERR", repr(exc), 0, 0)
    finally:
        sock.close()

def percentile(values, p):
    values = sorted(values)
    k = min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1))))
    return values[k]


def phase(host, port, payload, duration, concurrency):
    barrier = threading.Barrier(concurrency)
    results = [None] * concurrency
    threads = [threading.Thread(target=worker,
                                args=(host, port, payload, duration, barrier, results, i))
               for i in range(concurrency)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    lat, reqs, bts = [], 0, 0
    errs = [item for item in results if item and item[0] == "ERR"]
    if errs:
        raise RuntimeError(f"{len(errs)}/{concurrency} workers failed: {errs[:3]}")
    for item in results:
        lat.extend(item[0])
        reqs += item[1]
        bts += item[2]
    return {
        "duration_s": duration,
        "concurrency": concurrency,
        "payload_bytes": len(payload),
        "requests": reqs,
        "bytes": bts,
        "throughput_mbps": round(bts * 8 / duration / 1_000_000, 3),
        "p50_ms": round(percentile(lat, 50), 3),
        "p99_ms": round(percentile(lat, 99), 3),
        "p999_ms": round(percentile(lat, 99.9), 3),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server-bin", required=True, help="远端二进制文件名，如 meatus-1.4")
    ap.add_argument("--client-bin", required=True)
    ap.add_argument("--transport", choices=["tcp", "quic"], required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--duration", type=float, default=10.0)
    ap.add_argument("--concurrency", type=int, default=64)
    ap.add_argument("--payload", type=int, default=65536)
    ap.add_argument("--latency-duration", type=float, default=5.0)
    ap.add_argument("--host", default="43.173.105.47")
    args = ap.parse_args()

    client_bin = str(Path(args.client_bin).resolve())
    version = subprocess.check_output([client_bin, "--version"], text=True).strip()
    echo_port = free_port()
    tmp = Path(tempfile.mkdtemp(prefix="meatus-wan-"))
    cert, key = tmp / "server.crt", tmp / "server.key"
    subprocess.check_call([client_bin, "gen-cert", "--domain", "localhost",
                           "-o", str(tmp), "--force"], stdout=subprocess.DEVNULL)
    token_hash = hashlib.sha256(TOKEN.encode()).hexdigest()
    quic_line = "\nquic = true" if args.transport == "quic" else ""
    server_cfg = tmp / "server.toml"
    server_cfg.write_text(f'''[server]
bind_addr = "0.0.0.0"
control_port = {CONTROL_PORT}
tls_cert = "{REMOTE_DIR}/server.crt"
tls_key = "{REMOTE_DIR}/server.key"{quic_line}

[auth]
clients = [{{ id = "wanbench", tenant = "default", token_sha256 = "{token_hash}" }}]

[ports]
range = [{PROXY_PORT}, {PROXY_PORT}]

[admin]
bind = "127.0.0.1:{ADMIN_PORT}"
''')
    transport_line = '\ntransport = "quic"' if args.transport == "quic" else ""
    client_cfg = tmp / "client.toml"
    client_cfg.write_text(f'''[client]
server_addr = "{args.host}:{CONTROL_PORT}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "wanbench"
tenant = "default"
token = "{TOKEN}"{transport_line}

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {PROXY_PORT}
''')
    ssh(f"mkdir -p {REMOTE_DIR} && chmod 700 {REMOTE_DIR}")
    scp([cert, key, server_cfg])
    ssh("pkill -x meatus-1.4 || true; pkill -x meatus-1.3 || true; sleep 0.3")
    echo = start_echo(echo_port)
    client = None
    server_pid = None
    log_s = open(tmp / "server-remote.log", "w")
    log_c = open(tmp / "client.log", "w")
    try:
        server_pid = ssh(f"cd {REMOTE_DIR} && chmod +x {args.server_bin} && "
                         f"setsid -f ./{args.server_bin} server -c server.toml "
                         f"> server.log 2>&1 < /dev/null; sleep 0.6; "
                         f"pgrep -x {args.server_bin} | head -1")
        wait_port(args.host, CONTROL_PORT, timeout=20)
        client = subprocess.Popen([client_bin, "client", "-c", str(client_cfg)],
                                  stdout=log_c, stderr=subprocess.STDOUT)
        deadline = time.time() + 25
        while time.time() < deadline:
            try:
                with socket.create_connection((args.host, PROXY_PORT), timeout=1) as s:
                    s.sendall(b"warmup")
                    if s.recv(6) == b"warmup":
                        break
            except OSError:
                time.sleep(0.3)
        else:
            raise RuntimeError("proxy did not become ready; see " + str(tmp))

        handshake_p50, handshake_max = connect_ms(args.host, PROXY_PORT, samples=10)
        idle = phase(args.host, PROXY_PORT, b"x" * 64, args.latency_duration, 1)
        loaded = phase(args.host, PROXY_PORT, bytes([0x5A]) * args.payload,
                       args.duration, args.concurrency)
        cpu = ""
        if server_pid:
            cpu = ssh(f"ps -o %cpu=,rss= -p {server_pid} 2>/dev/null || true")
        result = {
            "schema": "meatus-bench/v1",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "binary": client_bin,
            "version": version,
            "server_binary": args.server_bin,
            "transport": args.transport,
            "remote": args.host,
            "host": {"platform": sys.platform, "cpu_count": os.cpu_count()},
            "params": {"duration_s": args.duration, "concurrency": args.concurrency,
                        "payload_bytes": args.payload, "latency_duration_s": args.latency_duration},
            "wan_tcp_handshake_p50_ms": handshake_p50,
            "wan_tcp_handshake_max_ms": handshake_max,
            "idle": idle,
            "loaded": loaded,
            "requests": loaded["requests"],
            "bytes": loaded["bytes"],
            "throughput_mbps": loaded["throughput_mbps"],
            "p50_ms": loaded["p50_ms"],
            "p99_ms": loaded["p99_ms"],
            "server_cpu_percent": cpu,
            "logs": str(tmp),
        }
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False))
    finally:
        if client:
            client.terminate()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
        if server_pid:
            ssh(f"kill {server_pid} 2>/dev/null || true")
        ssh(f"pkill -x {args.server_bin} || true")
        echo.terminate()
        for fh in (log_s, log_c):
            fh.close()


if __name__ == "__main__":
    main()

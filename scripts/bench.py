#!/usr/bin/env python3
"""E5-01 隧道基准：固定 echo 场景，输出可复现 JSON。

用法：
  scripts/bench.py --binary target/release/meatus --transport tcp|quic \
      --out bench-results/tcp.json [--duration 5] [--concurrency 8] [--payload 4096]
"""
import argparse
import hashlib
import json
import os
import platform
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOKEN = "secret"


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port: int, timeout: float = 15.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} did not open")


def start_echo(port: int) -> subprocess.Popen:
    code = f"""
import socket, threading
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
s.bind(('127.0.0.1',{port})); s.listen(128)
def h(c):
    c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    try:
        while True:
            b=c.recv(65536)
            if not b: break
            c.sendall(b)
    finally: c.close()
while True:
    c,_=s.accept(); threading.Thread(target=h,args=(c,),daemon=True).start()
"""
    return subprocess.Popen([sys.executable, "-c", code])


def run_worker(port, payload, deadline, barrier, out, idx):
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    sock.settimeout(10)
    barrier.wait()
    lat = []
    reqs = 0
    bytes_total = 0
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
        bytes_total += len(payload) * 2
    sock.close()
    out[idx] = (lat, reqs, bytes_total)


def percentile(values, p):
    if not values:
        return 0.0
    values = sorted(values)
    k = min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1))))
    return values[k]


def proc_stat(pid):
    try:
        out = subprocess.check_output(["ps", "-o", "%cpu=,rss=", "-p", str(pid)], text=True).strip()
        cpu, rss = out.split()
        return float(cpu), int(rss) / 1024.0
    except Exception:
        return 0.0, 0.0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", required=True)
    ap.add_argument("--transport", choices=["tcp", "quic"], required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--duration", type=float, default=5.0)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--payload", type=int, default=4096)
    args = ap.parse_args()

    binary = str(Path(args.binary).resolve())
    version = subprocess.check_output([binary, "--version"], text=True).strip()
    echo_port, control_port, proxy_port, admin_port = (free_port() for _ in range(4))
    tmp = Path(tempfile.mkdtemp(prefix="meatus-bench-"))
    cert = tmp / "server.crt"
    key = tmp / "server.key"
    subprocess.check_call([binary, "gen-cert", "--domain", "localhost", "-o", str(tmp), "--force"],
                          stdout=subprocess.DEVNULL)
    token_hash = hashlib.sha256(TOKEN.encode()).hexdigest()
    quic = "\nquic = true" if args.transport == "quic" else ""
    server_cfg = tmp / "server.toml"
    server_cfg.write_text(f'''[server]
bind_addr = "127.0.0.1"
control_port = {control_port}
tls_cert = "{cert}"
tls_key = "{key}"{quic}

[auth]
clients = [{{ id = "bench", tenant = "default", token_sha256 = "{token_hash}" }}]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "127.0.0.1:{admin_port}"
''')
    client_extra = '\ntransport = "quic"' if args.transport == "quic" else ""
    client_cfg = tmp / "client.toml"
    client_cfg.write_text(f'''[client]
server_addr = "127.0.0.1:{control_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "bench"
tenant = "default"
token = "{TOKEN}"{client_extra}

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "127.0.0.1:{echo_port}"
remote_port = {proxy_port}
''')
    log_s = open(tmp / "server.log", "w")
    log_c = open(tmp / "client.log", "w")
    echo = start_echo(echo_port)
    time.sleep(0.2)
    server = subprocess.Popen([binary, "server", "-c", str(server_cfg)], stdout=log_s, stderr=subprocess.STDOUT)
    client = subprocess.Popen([binary, "client", "-c", str(client_cfg)], stdout=log_c, stderr=subprocess.STDOUT)
    try:
        wait_port(control_port)
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                with socket.create_connection(("127.0.0.1", proxy_port), timeout=0.3) as s:
                    s.sendall(b"warmup")
                    if s.recv(6) == b"warmup":
                        break
            except OSError:
                time.sleep(0.2)
        else:
            raise RuntimeError("proxy did not become ready; see " + str(tmp))

        payload = bytes([0x5A]) * args.payload
        barrier = threading.Barrier(args.concurrency)
        results = [None] * args.concurrency
        threads = [threading.Thread(target=run_worker,
                                    args=(proxy_port, payload, time.perf_counter() + 1.0 + args.duration,
                                          barrier, results, i))
                   for i in range(args.concurrency)]
        # 预热 1s：先跑短轮，随后正式轮以同一 deadline 机制计时。
        for t in threads:
            t.start()
        for t in threads:
            t.join()

        lat, reqs, bts = [], 0, 0
        for item in results:
            if item:
                lat.extend(item[0])
                reqs += item[1]
                bts += item[2]
        cpu, rss = proc_stat(server.pid)
        result = {
            "schema": "meatus-bench/v1",
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "binary": binary,
            "version": version,
            "transport": args.transport,
            "host": {"platform": platform.platform(), "cpu_count": os.cpu_count()},
            "params": {"duration_s": args.duration, "concurrency": args.concurrency,
                        "payload_bytes": args.payload, "warmup_s": 1.0},
            "requests": reqs,
            "bytes": bts,
            "throughput_mbps": round(bts * 8 / args.duration / 1_000_000, 3),
            "p50_ms": round(percentile(lat, 50), 3),
            "p99_ms": round(percentile(lat, 99), 3),
            "server_cpu_percent": cpu,
            "server_rss_mb": round(rss, 1),
            "logs": str(tmp),
        }
        Path(args.out).parent.mkdir(parents=True, exist_ok=True)
        Path(args.out).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(result, ensure_ascii=False))
    finally:
        for proc in (client, server, echo):
            proc.terminate()
        for proc in (client, server, echo):
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
        log_s.close()
        log_c.close()


if __name__ == "__main__":
    main()

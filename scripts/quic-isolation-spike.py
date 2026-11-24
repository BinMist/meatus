#!/usr/bin/env python3
"""G6-01 QUIC 控制面隔离 spike。

在本机用用户态 UDP 代理模拟 300ms RTT / 1% 丢包 / 4Mbps 瓶颈，复现
32×16KB 数据面压满时控制 Ping/Pong 被饿死的问题，并对比：

  baseline  单 QUIC 连接 + 单 yamux 会话承载控制与数据
  isolated  控制连接与数据连接分离（复用现有 [client.pool] 路径）

脚本不修改生产配置，不占用固定端口，所有进程与临时目录按 attempt 隔离。
"""
import argparse
import asyncio
import hashlib
import json
import os
import random
import socket
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOKEN = "g6-spike-secret"
HOST = "127.0.0.1"


def free_tcp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind((HOST, 0))
        return sock.getsockname()[1]


def free_udp_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind((HOST, 0))
        return sock.getsockname()[1]


def wait_port(port: int, timeout: float = 25.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            with socket.create_connection((HOST, port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"port {port} did not open")


def start_echo(port: int) -> subprocess.Popen:
    code = f"""
import socket, threading
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
s.bind(('{HOST}', {port})); s.listen(256)
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
                            stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT,
                            start_new_session=True)


class WanProxy:
    """单瓶颈 UDP 代理：双向固定延迟 + 丢包 + FIFO 速率限制。"""

    def __init__(self, listen_port: int, upstream_port: int, delay_ms: float,
                 loss: float, rate_mbps: float, seed: int):
        self.listen_port = listen_port
        self.upstream_port = upstream_port
        self.delay = delay_ms / 1000.0
        self.loss = loss
        self.rate_bps = rate_mbps * 1_000_000 if rate_mbps > 0 else 0
        self.rng = random.Random(seed)
        self.seq = 0
        self.queues = [asyncio.PriorityQueue(), asyncio.PriorityQueue()]
        self.upstreams = {}
        self.tasks = []
        self.sent = [0, 0]
        self.sock = None
        self.loop = None

    async def run(self, ready: threading.Event):
        self.loop = asyncio.get_running_loop()
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.bind((HOST, self.listen_port))
        sock.setblocking(False)
        self.sock = sock
        self.tasks.append(self.loop.create_task(self._client_reader()))
        for i, q in enumerate(self.queues):
            self.tasks.append(self.loop.create_task(self._sender(q, i)))
        ready.set()
        try:
            await asyncio.Future()
        finally:
            sock.close()

    async def _client_reader(self):
        while True:
            data, addr = await self.loop.sock_recvfrom(self.sock, 65535)
            if self.rng.random() < self.loss:
                continue
            upstream = self.upstreams.get(addr)
            if upstream is None:
                upstream = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                upstream.bind((HOST, 0))
                upstream.setblocking(False)
                self.upstreams[addr] = upstream
                self.tasks.append(self.loop.create_task(
                    self._upstream_reader(upstream, addr)))
            self._enqueue(0, upstream, data, (HOST, self.upstream_port))

    async def _upstream_reader(self, upstream: socket.socket, client_addr):
        while True:
            data, _ = await self.loop.sock_recvfrom(upstream, 65535)
            if self.rng.random() < self.loss:
                continue
            self._enqueue(1, self.sock, data, client_addr)

    def _enqueue(self, direction: int, sock: socket.socket, data: bytes, dest):
        due = self.loop.time() + self.delay
        self.seq += 1
        self.queues[direction].put_nowait((due, self.seq, data, sock, dest))

    async def _sender(self, queue, direction: int):
        while True:
            due, _, data, sock, dest = await queue.get()
            now = self.loop.time()
            if due > now:
                await asyncio.sleep(due - now)
            try:
                sock.sendto(data, dest)
                self.sent[direction] += len(data)
            except OSError:
                pass
            if self.rate_bps:
                await asyncio.sleep(len(data) * 8 / self.rate_bps)


def start_wan_proxy(listen_port: int, upstream_port: int, delay_ms: float,
                    loss: float, rate_mbps: float, seed: int):
    proxy = WanProxy(listen_port, upstream_port, delay_ms, loss, rate_mbps, seed)
    ready = threading.Event()

    def run():
        asyncio.run(proxy.run(ready))

    thread = threading.Thread(target=run, daemon=True)
    thread.start()
    if not ready.wait(timeout=5):
        raise RuntimeError("WAN proxy did not start")
    return thread, proxy


def worker(port: int, payload: bytes, duration: float, barrier,
           out: list, idx: int, timeout: float, pipeline: int = 1):
    result = {"idx": idx, "lat": [], "reqs": 0, "bytes": 0, "error": None}
    sock = None
    try:
        sock = socket.create_connection((HOST, port), timeout=20)
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
            for _ in range(pipeline):
                sock.sendall(payload)
            for _ in range(pipeline):
                got = 0
                while got < len(payload):
                    chunk = sock.recv(len(payload) - got)
                    if not chunk:
                        raise RuntimeError("echo closed")
                    got += len(chunk)
            result["lat"].append((time.perf_counter() - t0) * 1000 / pipeline)
            result["reqs"] += pipeline
            result["bytes"] += len(payload) * 2 * pipeline
    except Exception as exc:
        result["error"] = repr(exc)
    finally:
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass
        out[idx] = result


def hog(port: int, chunk: bytes, total_bytes: int, timeout: float, out: list, idx: int):
    """One TCP stream that writes without reading, filling the yamux send queue."""
    result = {"idx": idx, "sent": 0, "error": None}
    try:
        sock = socket.create_connection((HOST, port), timeout=20)
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.settimeout(timeout)
        while result["sent"] < total_bytes:
            sock.sendall(chunk)
            result["sent"] += len(chunk)
        sock.close()
    except Exception as exc:
        result["error"] = repr(exc)
    finally:
        out[idx] = result


def percentile(values, p: float) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    k = min(len(values) - 1, int(round((p / 100.0) * (len(values) - 1))))
    return values[k]


def phase(port: int, payload: bytes, duration: float, concurrency: int,
          worker_timeout: float, pipeline: int = 1) -> dict:
    barrier = threading.Barrier(concurrency)
    results = [None] * concurrency
    threads = [threading.Thread(target=worker,
                                args=(port, payload, duration, barrier, results, i,
                                      worker_timeout, pipeline))
               for i in range(concurrency)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    lat, reqs, total_bytes = [], 0, 0
    errors = []
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
        "worker_errors": errors,
        "workers_ok": concurrency - len(errors),
        "workers_total": concurrency,
    }


def read_text(path: Path) -> str:
    try:
        return path.read_text(errors="replace")
    except OSError:
        return ""


def run_attempt(binary: Path, variant: str, attempt: int, args,
                out_dir: Path) -> dict:
    tmp = Path(tempfile.mkdtemp(prefix=f"g6-{variant}-a{attempt}-", dir=str(out_dir)))
    echo_port = free_tcp_port()
    server_port = free_tcp_port()
    proxy_port = free_tcp_port()
    admin_port = free_tcp_port()
    wan_port = free_udp_port()
    cert, key = tmp / "server.crt", tmp / "server.key"
    subprocess.check_call([str(binary), "gen-cert", "--domain", "localhost",
                           "-o", str(tmp), "--force"], stdout=subprocess.DEVNULL)
    token_hash = hashlib.sha256(TOKEN.encode()).hexdigest()
    server_cfg = tmp / "server.toml"
    server_cfg.write_text(f'''[server]
bind_addr = "{HOST}"
control_port = {server_port}
tls_cert = "{cert}"
tls_key = "{key}"
quic = true

[auth]
clients = [{{ id = "g6", tenant = "default", token_sha256 = "{token_hash}" }}]

[ports]
range = [{proxy_port}, {proxy_port}]

[admin]
bind = "{HOST}:{admin_port}"
''')
    pool_section = ""
    if variant == "isolated":
        pool_section = '''
[client.pool]
enabled = true
size = 1
max_streams_per_conn = 1024
idle_timeout_s = 300
'''
    client_cfg = tmp / "client.toml"
    client_cfg.write_text(f'''[client]
server_addr = "{HOST}:{wan_port}"
server_name = "localhost"
server_ca_cert = "{cert}"
transport = "quic"
client_id = "g6"
tenant = "default"
token = "{TOKEN}"
{pool_section}
[[proxy]]
name = "echo"
type = "tcp"
local_addr = "{HOST}:{echo_port}"
remote_port = {proxy_port}
''')
    log_s = open(tmp / "server.log", "w")
    log_c = open(tmp / "client.log", "w")
    echo = start_echo(echo_port)
    _, proxy = start_wan_proxy(wan_port, server_port, args.delay_ms, args.loss,
                              args.rate_mbps, seed=attempt + (0 if variant == "baseline" else 100))
    server = subprocess.Popen([str(binary), "server", "-c", str(server_cfg)],
                              stdout=log_s, stderr=subprocess.STDOUT,
                              start_new_session=True)
    client = None
    try:
        wait_port(server_port, timeout=20)
        client = subprocess.Popen([str(binary), "client", "-c", str(client_cfg)],
                                  stdout=log_c, stderr=subprocess.STDOUT,
                                  start_new_session=True)
        deadline = time.time() + 30
        while time.time() < deadline:
            try:
                with socket.create_connection((HOST, proxy_port), timeout=1.0) as sock:
                    sock.settimeout(2.5)
                    sock.sendall(b"warmup")
                    if sock.recv(6) == b"warmup":
                        break
            except OSError:
                time.sleep(0.3)
        else:
            raise RuntimeError("proxy did not become ready")

        hog_results = [None] * args.hog_workers
        hogs = []
        chunk = bytes([0x5A]) * args.payload
        for i in range(args.hog_workers):
            thread = threading.Thread(target=hog,
                                      args=(proxy_port, chunk,
                                            int(args.hog_mb * 1_000_000),
                                            args.worker_timeout, hog_results, i),
                                      daemon=True)
            thread.start()
            hogs.append(thread)
        load = phase(proxy_port, chunk, args.duration,
                     args.concurrency, args.worker_timeout, args.pipeline)
        for thread in hogs:
            thread.join(timeout=5)
        time.sleep(args.post_load_wait)
        client_tail = read_text(tmp / "client.log")
        server_tail = read_text(tmp / "server.log")
        control_failures = client_tail.count("no Pong for")
        reconnects = client_tail.count("control session failed, reconnecting")
        ok = (not load["worker_errors"] and control_failures == 0
              and client.poll() is None)
        return {
            "variant": variant,
            "attempt": attempt,
            "ok": ok,
            "load": load,
            "hog": hog_results,
            "wan_egress_bytes": {"client_to_server": proxy.sent[0],
                                  "server_to_client": proxy.sent[1]},
            "control_pong_failures": control_failures,
            "client_reconnects": reconnects,
            "client_alive": client.poll() is None,
            "server_alive": server.poll() is None,
            "log_dir": str(tmp),
            "client_log_tail": client_tail.splitlines()[-12:],
            "server_log_tail": server_tail.splitlines()[-12:],
        }
    finally:
        if client is not None:
            client.terminate()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
        echo.terminate()
        try:
            echo.wait(timeout=3)
        except subprocess.TimeoutExpired:
            echo.kill()
        log_s.close()
        log_c.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--binary", default=str(ROOT / "target/release/meatus"))
    ap.add_argument("--variant", choices=["baseline", "isolated", "both"], default="both")
    ap.add_argument("--attempts", type=int, default=7)
    ap.add_argument("--duration", type=float, default=10.0)
    ap.add_argument("--concurrency", type=int, default=32)
    ap.add_argument("--payload", type=int, default=16384)
    ap.add_argument("--worker-timeout", type=float, default=90.0)
    ap.add_argument("--post-load-wait", type=float, default=8.0)
    ap.add_argument("--pipeline", type=int, default=1,
                    help="outstanding 16KB echoes per worker")
    ap.add_argument("--hog-mb", type=float, default=0.0,
                    help="extra bytes per write-only stream, in MB")
    ap.add_argument("--hog-workers", type=int, default=1)
    ap.add_argument("--delay-ms", type=float, default=150.0)
    ap.add_argument("--loss", type=float, default=0.01)
    ap.add_argument("--rate-mbps", type=float, default=4.0)
    ap.add_argument("--out-dir", default=str(ROOT / "planning/benchmarks/g6-01"))
    args = ap.parse_args()

    binary = Path(args.binary).resolve()
    if not binary.exists():
        raise SystemExit(f"binary not found: {binary}")
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    variants = ["baseline", "isolated"] if args.variant == "both" else [args.variant]
    summary = {
        "schema": "meatus-g6-spike/v1",
        "timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "binary": str(binary),
        "version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
        "params": {
            "duration_s": args.duration,
            "concurrency": args.concurrency,
            "payload_bytes": args.payload,
            "pipeline": args.pipeline,
            "hog_mb": args.hog_mb,
            "delay_ms": args.delay_ms,
            "loss": args.loss,
            "rate_mbps": args.rate_mbps,
            "attempts": args.attempts,
        },
        "variants": {},
    }
    for variant in variants:
        results = []
        for attempt in range(1, args.attempts + 1):
            print(f"[{variant}] attempt {attempt}/{args.attempts}", flush=True)
            result = run_attempt(binary, variant, attempt, args, out_dir)
            results.append(result)
            print(json.dumps({k: result[k] for k in
                              ("variant", "attempt", "ok", "control_pong_failures",
                               "client_reconnects", "load")}, ensure_ascii=False), flush=True)
        ok_count = sum(1 for item in results if item["ok"])
        summary["variants"][variant] = {
            "attempts": args.attempts,
            "ok": ok_count,
            "failed": args.attempts - ok_count,
            "success_rate": round(ok_count / args.attempts, 4),
            "results": results,
        }
    out_path = out_dir / f"g6-01-{'both' if args.variant == 'both' else args.variant}.json"
    out_path.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
    print(f"wrote {out_path}", flush=True)


if __name__ == "__main__":
    main()

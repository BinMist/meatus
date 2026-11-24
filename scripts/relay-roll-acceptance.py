#!/usr/bin/env python3
"""G5-01 本地双进程滚动验收。

在一台机器上启动两个 relay、一个 multi-home client、一个主动健康检查的
TCP 蓝绿入口，再用 `scripts/relay-roll.sh` 逐台 drain / 升级 / 回滚：
- 全程 1000 个新 TCP 请求，0 失败；
- relay-b 正常滚动、relay-a 正常滚动；
- relay-a 再注入一次启动失败，验证自动回滚，单台回滚耗时 <10 分钟。

这不是生产双主机 HA：两台 relay 共享内核与磁盘，只验证编排与零失败语义。
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import signal
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = ROOT / "target" / "debug" / "meatus"
TOKEN_HASH = "2bb80d537b1da3e38bd30361aa855686bde0eacd7162fef6a25fe97bf527a25b"
HOST = "127.0.0.1"


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind((HOST, 0))
        return sock.getsockname()[1]


def run(cmd: list[str], *, check: bool = True) -> subprocess.CompletedProcess:
    result = subprocess.run(cmd, cwd=ROOT, text=True, capture_output=True)
    if check and result.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} failed:\n{result.stdout}\n{result.stderr}")
    return result


def health(url: str) -> dict:
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            return json.loads(response.read())
    except Exception:
        return {}


def wait_health(url: str, timeout: float = 30) -> dict:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        payload = health(url)
        if payload.get("status") == "ok":
            relay = payload.get("relay") or {}
            if relay.get("rotation", {}).get("state") == "complete":
                return payload
        time.sleep(0.5)
    raise RuntimeError(f"relay not healthy: {url}")


def probe_echo(port: int) -> bool:
    try:
        with socket.create_connection((HOST, port), timeout=1) as sock:
            sock.sendall(b"probe")
            sock.settimeout(1)
            return sock.recv(64) == b"probe"
    except OSError:
        return False


def wait_proxy(port: int, timeout: float = 30) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if probe_echo(port):
            return
        time.sleep(0.2)
    raise RuntimeError(f"proxy port did not route: {port}")


def write_text(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")


def setup_environment(work: Path) -> dict:
    # 本地执行环境会阻塞所有信号；子进程启动前清空 mask，才能测真实 SIGTERM drain。
    unblock = work / "exec-unblocked.py"
    write_text(
        unblock,
        "import os, signal, sys\n"
        "signal.pthread_sigmask(signal.SIG_SETMASK, [])\n"
        "os.execv(sys.argv[1], sys.argv[1:])\n",
    )
    cert_dir = work / "certs"
    run([str(BIN), "gen-cert", "--domain", "localhost", "--domain", HOST, "--out", str(cert_dir)])
    cert = cert_dir / "server.crt"
    key = cert_dir / "server.key"

    signing_key = work / "relay-signing.pem"
    run([str(BIN), "relay", "bundle", "keygen", "--out", str(signing_key)])

    echo_port = free_port()
    control_a, control_b = free_port(), free_port()
    proxy_a, proxy_b = free_port(), free_port()
    admin_a, admin_b = free_port(), free_port()
    health_a, health_b = free_port(), free_port()

    bundle_draft = {
        "version": 1,
        "group": "prod",
        "epoch": 2,
        "generated_at": "2026-11-24T00:00:00Z",
        "relays": [
            {
                "id": "relay-a",
                "role": "primary",
                "control_addr": f"{HOST}:{control_a}",
                "http_domains": [],
                "peer_addr": f"{HOST}:{health_a}",
                "ports": [proxy_a, proxy_a],
            },
            {
                "id": "relay-b",
                "role": "secondary",
                "control_addr": f"{HOST}:{control_b}",
                "http_domains": [],
                "peer_addr": f"{HOST}:{health_b}",
                "ports": [proxy_b, proxy_b],
            },
        ],
    }
    draft_path = work / "bundle-draft.json"
    write_text(draft_path, json.dumps(bundle_draft, indent=2))
    bundle_path = work / "relay-bundle.json"
    exported = run([
        str(BIN), "relay", "bundle", "export",
        "--input", str(draft_path),
        "--signing-key", str(signing_key),
        "--out", str(bundle_path),
    ])
    public = next(
        line.split("pubkey", 1)[1].strip()
        for line in exported.stdout.splitlines()
        if line.startswith("pubkey")
    )

    peer = work / "relay-peer.key"
    peer.write_bytes(b"0123456789abcdef0123456789abcdef")
    peer.chmod(0o600)

    def server_config(name: str, control: int, proxy: int, admin: int, peer_port: int, role: str) -> str:
        return f"""
[server]
bind_addr = "{HOST}"
control_port = {control}
tls_cert = "{cert}"
tls_key = "{key}"
drain_grace_s = 5

[auth]
clients = [
  {{ id = "nas", tenant = "default", token_sha256 = "{TOKEN_HASH}" }},
]

[ports]
range = [{proxy}, {proxy}]

[acme]
cert_dir = "{work}/acme-{name}"

[admin]
bind = "{HOST}:{admin}"

[relay]
id = "{name}"
group = "prod"
role = "{role}"
advertise_addr = "{HOST}:{control}"
health_bind = "{HOST}:{peer_port}"
health_interval_s = 1
peer_timeout_s = 1
bundle = "{bundle_path}"
bundle_pubkey = "{public}"
peer_secret_file = "{peer}"
"""

    write_text(work / "server-a.toml", server_config("relay-a", control_a, proxy_a, admin_a, health_a, "primary"))
    write_text(work / "server-b.toml", server_config("relay-b", control_b, proxy_b, admin_b, health_b, "secondary"))

    client_config = f"""
[client]
server_addrs = ["{HOST}:{control_a}", "{HOST}:{control_b}"]
server_name = "localhost"
server_ca_cert = "{cert}"
client_id = "nas"
tenant = "default"
token = "secret"

[client.relay]
group = "prod"
multi_home = true
health_interval_s = 1

[[proxy]]
name = "echo"
type = "tcp"
local_addr = "{HOST}:{echo_port}"
"""
    write_text(work / "client.toml", client_config)

    return {
        "work": work,
        "unblock": unblock,
        "echo_port": echo_port,
        "control_a": control_a,
        "control_b": control_b,
        "proxy_a": proxy_a,
        "proxy_b": proxy_b,
        "admin_a": admin_a,
        "admin_b": admin_b,
        "health_url_a": f"http://{HOST}:{admin_a}/healthz",
        "health_url_b": f"http://{HOST}:{admin_b}/healthz",
    }


class EchoHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        while True:
            data = self.request.recv(65536)
            if not data:
                return
            self.request.sendall(data)


def start_echo(port: int) -> socketserver.ThreadingTCPServer:
    server = socketserver.ThreadingTCPServer((HOST, port), EchoHandler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def start_relay(work: Path, name: str, unblock: Path) -> subprocess.Popen:
    log = open(work / f"{name}.log", "a", encoding="utf-8")
    process = subprocess.Popen(
        [sys.executable, str(unblock), str(BIN), "server", "-c", str(work / f"server-{name[-1]}.toml")],
        cwd=ROOT,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    (work / f"{name}.pid").write_text(str(process.pid), encoding="utf-8")
    return process


def start_client(work: Path, unblock: Path) -> subprocess.Popen:
    log = open(work / "client.log", "a", encoding="utf-8")
    return subprocess.Popen(
        [sys.executable, str(unblock), str(BIN), "client", "-c", str(work / "client.toml")],
        cwd=ROOT,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )


class BlueGreen:
    """主动健康检查的 TCP 蓝绿入口，/healthz 503 或代理探测失败即摘除。"""

    def __init__(self, front_port: int, backends: list[tuple[str, int, str]]) -> None:
        self.front_port = front_port
        self.backends = backends
        self.ready = {name: False for name, _, _ in backends}
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server.bind((HOST, front_port))
        self.server.listen(256)
        self.server.settimeout(0.5)
        threading.Thread(target=self._health_loop, daemon=True).start()
        threading.Thread(target=self._accept_loop, daemon=True).start()

    def _health_loop(self) -> None:
        while not self.stop.is_set():
            for name, proxy_port, health_url in self.backends:
                payload = health(health_url)
                ok = payload.get("status") == "ok" and probe_echo(proxy_port)
                with self.lock:
                    self.ready[name] = ok
            self.stop.wait(0.5)

    def _pick(self) -> tuple[str, int] | None:
        with self.lock:
            ready = [(name, port) for name, port, _ in self.backends if self.ready[name]]
        return ready[0] if ready else None

    def _accept_loop(self) -> None:
        while not self.stop.is_set():
            try:
                client, _ = self.server.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            backend = None
            deadline = time.monotonic() + 2
            while backend is None and time.monotonic() < deadline:
                backend = self._pick()
                if backend is None:
                    time.sleep(0.1)
            if backend is None:
                client.close()
                continue
            threading.Thread(target=self._forward, args=(client, backend[1]), daemon=True).start()

    def _forward(self, client: socket.socket, port: int) -> None:
        try:
            upstream = socket.create_connection((HOST, port), timeout=2)
            upstream.settimeout(None)
            client.settimeout(None)

            def pump(src: socket.socket, dst: socket.socket) -> None:
                try:
                    while True:
                        data = src.recv(65536)
                        if not data:
                            break
                        dst.sendall(data)
                except OSError:
                    pass
                finally:
                    try:
                        dst.shutdown(socket.SHUT_WR)
                    except OSError:
                        pass

            thread = threading.Thread(target=pump, args=(client, upstream), daemon=True)
            thread.start()
            pump(upstream, client)
            thread.join(timeout=1)
        except OSError:
            pass
        finally:
            client.close()

    def snapshot(self) -> dict:
        with self.lock:
            return dict(self.ready)

    def close(self) -> None:
        self.stop.set()
        self.server.close()


def load_once(port: int, count: int, workers: int, delay: float) -> int:
    failures: list[str] = []
    lock = threading.Lock()
    per_worker = count // workers
    remainder = count % workers

    def worker(worker_id: int) -> None:
        requests = per_worker + (1 if worker_id < remainder else 0)
        for i in range(requests):
            payload = f"req-{worker_id}-{i}".encode()
            try:
                with socket.create_connection((HOST, port), timeout=3) as sock:
                    sock.settimeout(3)
                    sock.sendall(payload)
                    received = sock.recv(64)
                if received != payload:
                    raise RuntimeError(f"echo mismatch: {received!r} != {payload!r}")
            except Exception as error:  # noqa: BLE001 - 验收需要统计所有失败
                with lock:
                    failures.append(f"{worker_id}/{i}: {error}")
            if delay:
                time.sleep(delay)

    threads = [threading.Thread(target=worker, args=(i,)) for i in range(workers)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    return len(failures)


def load_command(args: argparse.Namespace) -> int:
    failures = load_once(args.port, args.count, args.workers, args.delay)
    print(json.dumps({"count": args.count, "failures": failures, "workers": args.workers}))
    return 0 if failures == 0 else 1


def run_roll(work: Path, env: dict, load_cmd: str) -> dict:
    script = ROOT / "scripts" / "relay-roll.sh"
    report = work / "roll-report.json"
    unblock = env["unblock"]

    def target(name: str, bad: bool) -> str:
        config = work / f"server-{name[-1]}.toml"
        pid = work / f"{name}.pid"
        log = work / f"{name}.log"
        start_script = work / f"start-{name}.sh"
        marker = work / f"bad-{name}"
        write_text(
            start_script,
            f"""#!/usr/bin/env bash
set -eu
if [[ -f "{marker}" ]]; then
  echo "injected start failure for {name}" >&2
  exit 1
fi
nohup "{sys.executable}" "{unblock}" "{BIN}" server -c "{config}" >>"{log}" 2>&1 &
echo $! >"{pid}"
""",
        )
        start_script.chmod(0o755)
        upgrade = f"touch '{marker}'" if bad else "-"
        rollback = f"rm -f '{marker}'" if bad else "-"
        stop = f"kill -TERM \"$(cat '{pid}')\""
        return f"{name}|{env[f'health_url_{name[-1]}']}|{stop}|{upgrade}|bash '{start_script}'|{rollback}"

    cmd = [
        str(script),
        "--target", target("relay-b", bad=False),
        "--target", target("relay-a", bad=False),
        "--target", target("relay-a", bad=True),
        "--load-cmd", load_cmd,
        "--load-delay", "2",
        "--settle", "60",
        "--timeout", "45",
        "--report", str(report),
        "--log", str(work / "relay-roll.log"),
    ]
    result = subprocess.run(cmd, cwd=ROOT, text=True, capture_output=True, timeout=300)
    print(result.stdout)
    print(result.stderr, file=sys.stderr)
    if result.returncode != 0:
        raise RuntimeError(f"relay-roll failed with {result.returncode}")
    return json.loads(report.read_text(encoding="utf-8"))


def main() -> int:
    if not BIN.exists():
        run(["cargo", "build", "--bin", "meatus", "--locked"])
    work = Path(tempfile.mkdtemp(prefix="meatus-g5-roll-"))
    print(f"workdir {work}")
    env = setup_environment(work)
    processes: list[subprocess.Popen] = []
    echo_server = None
    client = None
    lb = None
    try:
        echo_server = start_echo(env["echo_port"])
        processes = [start_relay(work, "relay-a", env["unblock"]), start_relay(work, "relay-b", env["unblock"])]
        wait_health(env["health_url_a"])
        wait_health(env["health_url_b"])
        client = start_client(work, env["unblock"])
        wait_proxy(env["proxy_a"])
        wait_proxy(env["proxy_b"])

        front = free_port()
        lb = BlueGreen(front, [
            ("relay-a", env["proxy_a"], env["health_url_a"]),
            ("relay-b", env["proxy_b"], env["health_url_b"]),
        ])
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if all(lb.snapshot().values()):
                break
            time.sleep(0.2)
        print(f"blue-green {HOST}:{front} ready={lb.snapshot()}")

        load_cmd = (
            f"{shlex.quote(sys.executable)} {shlex.quote(str(Path(__file__).resolve()))} load "
            f"--port {front} --count 1000 --workers 20 --delay 0.5"
        )
        report = run_roll(work, env, load_cmd)
        print(json.dumps(report, ensure_ascii=False, indent=2))

        if not report.get("ok"):
            raise RuntimeError("roll report not ok")
        if report.get("load_exit") != 0:
            raise RuntimeError("1000-request load failed")
        rollback_targets = [item for item in report["targets"] if item["rollback_seconds"] > 0]
        if not rollback_targets:
            raise RuntimeError("rollback path was not exercised")
        if rollback_targets[0]["rollback_seconds"] >= 600:
            raise RuntimeError("rollback exceeded 10 minutes")

        print("G5-01 acceptance: 1000 requests, 0 failures; rollback path exercised")
        return 0
    finally:
        if lb:
            lb.close()
        if client:
            client.terminate()
            try:
                client.wait(timeout=5)
            except subprocess.TimeoutExpired:
                client.kill()
        for process in processes:
            if process.poll() is None:
                process.terminate()
        for process in processes:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
        if echo_server:
            echo_server.shutdown()
            echo_server.server_close()
        for name in ("relay-a", "relay-b"):
            pid_file = work / f"{name}.pid"
            if not pid_file.exists():
                continue
            try:
                pid = int(pid_file.read_text().strip())
                os.kill(pid, signal.SIGTERM)
            except (ValueError, ProcessLookupError):
                continue
            for _ in range(40):
                time.sleep(0.25)
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    break
            else:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        print(f"logs kept in {work}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command")
    load_parser = sub.add_parser("load")
    load_parser.add_argument("--port", type=int, required=True)
    load_parser.add_argument("--count", type=int, default=1000)
    load_parser.add_argument("--workers", type=int, default=20)
    load_parser.add_argument("--delay", type=float, default=0.5)
    args = parser.parse_args()
    if args.command == "load":
        raise SystemExit(load_command(args))
    raise SystemExit(main())

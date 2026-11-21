#!/usr/bin/env python3
"""V1.4 10 client 并发验收：10 个独立 client 各 1 个 TCP proxy，共 1000 次请求。"""
import concurrent.futures
import http.client
import json
import os
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/release/meatus"
CLIENTS = 10
REQ_PER_CLIENT = 100
SERVER_PORT = 7070
HTTP_PORT = 19091
REMOTE_BASE = 21100
RESULT = ROOT / "bench-results/conc-10client-1.4.0.json"


def wait_port(port, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                return True
        except OSError:
            time.sleep(0.2)
    return False


def main():
    if not BIN.exists():
        sys.exit(f"missing {BIN}")
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="meatus-conc10-"))
    procs = []
    try:
        (tmp / "certs").mkdir()
        (tmp / "www").mkdir()
        (tmp / "www/index.html").write_text("conc-ok\n", encoding="utf-8")
        subprocess.run([str(BIN), "gen-cert", "--domain", "localhost", "--domain", "127.0.0.1",
                        "--out", str(tmp / "certs")], check=True, stdout=subprocess.DEVNULL)

        clients = []
        for i in range(CLIENTS):
            out = subprocess.check_output([str(BIN), "gen-token"], text=True)
            token = next(line.split()[1] for line in out.splitlines() if line.startswith("token "))
            sha = next(line.split()[1] for line in out.splitlines() if line.startswith("sha256 "))
            clients.append({"id": f"conc-{i+1:02d}", "token": token, "sha": sha})

        entries = ",\n".join(
            f'{{ id = "{c["id"]}", token_sha256 = "{c["sha"]}" }}' for c in clients
        )
        (tmp / "server.toml").write_text(f'''
[server]
bind_addr = "127.0.0.1"
control_port = {SERVER_PORT}
tls_cert = "{tmp}/certs/server.crt"
tls_key = "{tmp}/certs/server.key"
[auth]
clients = [
{entries}
]
[admin]
bind = "127.0.0.1:7470"
[log]
level = "info"
format = "text"
''', encoding="utf-8")

        for i, c in enumerate(clients):
            (tmp / f"client-{i+1:02d}.toml").write_text(f'''
[client]
server_addr = "127.0.0.1:{SERVER_PORT}"
server_name = "localhost"
server_ca_cert = "{tmp}/certs/server.crt"
client_id = "{c["id"]}"
tenant = "default"
token = "{c["token"]}"
[[proxy]]
name = "conc-{i+1:02d}"
type = "tcp"
local_addr = "127.0.0.1:{HTTP_PORT}"
remote_port = {REMOTE_BASE + i}
[log]
level = "info"
format = "text"
''', encoding="utf-8")

        procs.append(subprocess.Popen([sys.executable, "-m", "http.server", str(HTTP_PORT),
                                       "--bind", "127.0.0.1", "--directory", str(tmp / "www")],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        if not wait_port(HTTP_PORT, 5):
            sys.exit("http server failed")
        procs.append(subprocess.Popen([str(BIN), "server", "--config", str(tmp / "server.toml")],
                                      stdout=open(tmp / "server.log", "w"), stderr=subprocess.STDOUT))
        if not wait_port(SERVER_PORT, 10):
            print((tmp / "server.log").read_text(encoding="utf-8", errors="replace"))
            sys.exit("meatus server failed")
        for i, _ in enumerate(clients):
            procs.append(subprocess.Popen([str(BIN), "client", "--config", str(tmp / f"client-{i+1:02d}.toml")],
                                          stdout=open(tmp / f"client-{i+1:02d}.log", "w"), stderr=subprocess.STDOUT))
            if not wait_port(REMOTE_BASE + i, 20):
                sys.exit(f"client {i+1} proxy failed")

        def hit(port):
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            try:
                conn.request("GET", "/index.html")
                resp = conn.getresponse()
                body = resp.read()
                return resp.status == 200 and b"conc-ok" in body
            finally:
                conn.close()

        ok = fail = 0
        started = time.time()
        with concurrent.futures.ThreadPoolExecutor(max_workers=50) as pool:
            futures = [pool.submit(hit, REMOTE_BASE + i) for i in range(CLIENTS) for _ in range(REQ_PER_CLIENT)]
            for fut in concurrent.futures.as_completed(futures):
                try:
                    if fut.result():
                        ok += 1
                    else:
                        fail += 1
                except Exception:
                    fail += 1
        elapsed = round(time.time() - started, 2)
        result = {"clients": CLIENTS, "requests": CLIENTS * REQ_PER_CLIENT, "ok": ok,
                  "fail": fail, "elapsed_s": elapsed, "result": "PASS" if fail == 0 else "FAIL"}
        RESULT.parent.mkdir(exist_ok=True)
        RESULT.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(result, ensure_ascii=False))
        sys.exit(0 if fail == 0 else 1)
    finally:
        for p in reversed(procs):
            p.terminate()
        for p in reversed(procs):
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
        shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    main()

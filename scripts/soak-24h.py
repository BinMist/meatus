#!/usr/bin/env python3
"""Standalone 24h soak for Meatus.

Each interval:
1. Probe the public relay port with a real HTTP request through a TCP proxy.
2. Probe the admin /healthz endpoint.
3. Confirm the server and client processes are still alive.

Outputs JSONL samples plus a final summary JSON. PASS requires:
- at least 95% of expected checks completed;
- zero failed checks (HTTP non-200, connection error, healthz error, dead process).
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import statistics
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def process_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except OSError:
        return False


def probe_tcp(host: str, port: int, timeout: float) -> tuple[bool, float, str]:
    started = time.monotonic()
    try:
        with socket.create_connection((host, port), timeout=timeout) as sock:
            sock.settimeout(timeout)
            req = (
                b"GET /soak HTTP/1.1\r\n"
                b"Host: soak.local\r\n"
                b"Connection: close\r\n"
                b"User-Agent: meatus-soak/1.0\r\n\r\n"
            )
            sock.sendall(req)
            data = b""
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                data += chunk
                if len(data) > 16384:
                    break
        latency_ms = (time.monotonic() - started) * 1000
        ok = (
            data.startswith(b"HTTP/1.0 200") or data.startswith(b"HTTP/1.1 200")
        ) and b"soak-ok" in data
        return ok, latency_ms, "" if ok else f"unexpected response: {data[:160]!r}"
    except Exception as exc:  # noqa: BLE001 - soak records every probe error
        latency_ms = (time.monotonic() - started) * 1000
        return False, latency_ms, f"{type(exc).__name__}: {exc}"


def probe_health(url: str, timeout: float) -> tuple[bool, float, str]:
    started = time.monotonic()
    try:
        with urllib.request.urlopen(url, timeout=timeout) as resp:
            body = resp.read(4096)
            latency_ms = (time.monotonic() - started) * 1000
            ok = resp.status == 200 and (
                b'"ok"' in body or b'"status":"ok"' in body
            )
            return ok, latency_ms, "" if ok else f"status={resp.status} body={body[:160]!r}"
    except Exception as exc:  # noqa: BLE001
        latency_ms = (time.monotonic() - started) * 1000
        return False, latency_ms, f"{type(exc).__name__}: {exc}"


def percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    idx = min(len(values) - 1, max(0, int(round((len(values) - 1) * pct))))
    return values[idx]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--health-url", required=True)
    parser.add_argument("--duration-s", type=int, default=86400)
    parser.add_argument("--interval-s", type=int, default=60)
    parser.add_argument("--timeout-s", type=float, default=10.0)
    parser.add_argument("--server-pid", type=int)
    parser.add_argument("--client-pid", type=int)
    parser.add_argument("--out-dir", required=True)
    args = parser.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    jsonl = out_dir / "soak.jsonl"
    summary_path = out_dir / "soak-summary.json"

    started_at = utc_now()
    start_monotonic = time.monotonic()
    deadline = start_monotonic + args.duration_s
    expected = max(1, args.duration_s // args.interval_s)
    checks = 0
    failed = 0
    consecutive_failures = 0
    max_consecutive_failures = 0
    latency_samples: list[float] = []
    last_error = ""

    with jsonl.open("a", encoding="utf-8") as handle:
        while True:
            now = time.monotonic()
            if now >= deadline:
                break
            checks += 1
            tcp_ok, tcp_ms, tcp_err = probe_tcp(args.host, args.port, args.timeout_s)
            health_ok, health_ms, health_err = probe_health(
                args.health_url, args.timeout_s
            )
            server_ok = (
                process_alive(args.server_pid) if args.server_pid is not None else True
            )
            client_ok = (
                process_alive(args.client_pid) if args.client_pid is not None else True
            )
            ok = tcp_ok and health_ok and server_ok and client_ok
            if ok:
                consecutive_failures = 0
                latency_samples.append(tcp_ms)
            else:
                failed += 1
                consecutive_failures += 1
                max_consecutive_failures = max(
                    max_consecutive_failures, consecutive_failures
                )
                last_error = "; ".join(
                    part
                    for part in [
                        tcp_err if not tcp_ok else "",
                        health_err if not health_ok else "",
                        "server process dead" if not server_ok else "",
                        "client process dead" if not client_ok else "",
                    ]
                    if part
                )

            record = {
                "ts": utc_now(),
                "elapsed_s": round(time.monotonic() - start_monotonic, 1),
                "ok": ok,
                "tcp_ok": tcp_ok,
                "tcp_ms": round(tcp_ms, 1),
                "health_ok": health_ok,
                "health_ms": round(health_ms, 1),
                "server_alive": server_ok,
                "client_alive": client_ok,
                "error": last_error if not ok else "",
            }
            handle.write(json.dumps(record, ensure_ascii=False) + "\n")
            handle.flush()

            next_at = min(deadline, now + args.interval_s)
            sleep_s = max(0.0, next_at - time.monotonic())
            time.sleep(sleep_s)

    elapsed_s = time.monotonic() - start_monotonic
    success_rate = (checks - failed) / checks if checks else 0.0
    passed = failed == 0 and checks >= int(expected * 0.95)
    summary = {
        "started_at": started_at,
        "finished_at": utc_now(),
        "duration_s": round(elapsed_s, 1),
        "target_duration_s": args.duration_s,
        "interval_s": args.interval_s,
        "checks": checks,
        "expected_checks": expected,
        "passed_checks": checks - failed,
        "failed_checks": failed,
        "success_rate": round(success_rate, 6),
        "max_consecutive_failures": max_consecutive_failures,
        "latency_ms": {
            "p50": round(percentile(latency_samples, 0.50), 1),
            "p95": round(percentile(latency_samples, 0.95), 1),
            "max": round(max(latency_samples), 1) if latency_samples else 0.0,
        }
        if latency_samples
        else {"p50": 0.0, "p95": 0.0, "max": 0.0},
        "last_error": last_error,
        "result": "PASS" if passed else "FAIL",
    }
    summary_path.write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())

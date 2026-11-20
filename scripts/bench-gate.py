#!/usr/bin/env python3
"""E5-03 性能门禁：QUIC 吞吐 >= TCP 2/3，QUIC P99 <= TCP 1.5x。"""
import argparse
import json
import sys


def load(path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tcp", required=True)
    ap.add_argument("--quic", required=True)
    args = ap.parse_args()
    tcp, quic = load(args.tcp), load(args.quic)

    tput_ratio = quic["throughput_mbps"] / tcp["throughput_mbps"] if tcp["throughput_mbps"] else 0
    p99_ratio = quic["p99_ms"] / tcp["p99_ms"] if tcp["p99_ms"] else 0
    failures = []
    if tput_ratio < 2 / 3:
        failures.append(f"QUIC throughput {quic['throughput_mbps']} < 2/3 TCP {tcp['throughput_mbps']} (ratio {tput_ratio:.3f})")
    if p99_ratio > 1.5:
        failures.append(f"QUIC P99 {quic['p99_ms']}ms > 1.5x TCP {tcp['p99_ms']}ms (ratio {p99_ratio:.3f})")
    report = {
        "tcp": {k: tcp[k] for k in ("throughput_mbps", "p99_ms", "p50_ms")},
        "quic": {k: quic[k] for k in ("throughput_mbps", "p99_ms", "p50_ms")},
        "throughput_ratio": round(tput_ratio, 3),
        "p99_ratio": round(p99_ratio, 3),
        "ok": not failures,
        "failures": failures,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))
    sys.exit(0 if not failures else 1)


if __name__ == "__main__":
    main()

#!/usr/bin/env nix-shell
#!nix-shell -i python3 -p python3
"""Sample `ip route get` at high frequency during NIC transitions.

Usage:
    python3 scripts/bouncing_routes.py <TV_IP> <WIFI_CONN_NAME>

Example:
    python3 scripts/bouncing_routes.py 192.168.1.170 WTFi7

The script:
1. Samples `ip route get <TV_IP>` every 10ms in a background thread
2. Connects Wi-Fi (nmcli), waits for settle, samples
3. Disconnects Ethernet (nmcli), waits for settle, samples
4. Reconnects Ethernet (nmcli), waits for settle, samples
5. Disconnects Wi-Fi (nmcli), waits for settle, samples
6. Prints timestamped telemetry of every route change

Sequence: (eno2) -> (eno2+wifi) -> (wifi) -> (eno2+wifi) -> (eno2)
"""

import subprocess
import sys
import threading
import time
from datetime import datetime


def get_route(ip: str) -> tuple[str, str]:
    """Return (device, src_ip) from `ip route get`."""
    try:
        result = subprocess.run(
            ["ip", "route", "get", ip],
            capture_output=True, text=True, timeout=2,
        )
        if result.returncode != 0:
            return ("UNREACHABLE", "")
        line = result.stdout.strip().split("\n")[0]
        dev = ""
        src = ""
        parts = line.split()
        for i, p in enumerate(parts):
            if p == "dev" and i + 1 < len(parts):
                dev = parts[i + 1]
            if p == "src" and i + 1 < len(parts):
                src = parts[i + 1]
        return (dev, src)
    except Exception as e:
        return (f"ERROR:{e}", "")


def nmcli(action: str, conn: str):
    """Run nmcli connection up/down."""
    subprocess.run(
        ["nmcli", "connection", action, conn],
        capture_output=True, text=True, timeout=30,
    )


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <TV_IP> <WIFI_CONN_NAME>")
        sys.exit(1)

    tv_ip = sys.argv[1]
    wifi_conn = sys.argv[2]

    # Detect current wired interface
    dev, src = get_route(tv_ip)
    if not dev or dev == "UNREACHABLE":
        print(f"No route to {tv_ip}, ensure Ethernet is connected")
        sys.exit(1)
    wired_dev = dev
    print(f"Wired interface: {wired_dev} (src {src})")
    print(f"Wi-Fi connection: {wifi_conn}")
    print(f"TV IP: {tv_ip}")
    print()

    # Find the nmcli connection name for the wired device
    result = subprocess.run(
        ["nmcli", "-t", "-f", "NAME,DEVICE", "connection", "show", "--active"],
        capture_output=True, text=True,
    )
    wired_conn = None
    for line in result.stdout.strip().split("\n"):
        parts = line.split(":")
        if len(parts) >= 2 and parts[1] == wired_dev:
            wired_conn = parts[0]
            break
    if not wired_conn:
        print(f"Could not find nmcli connection for {wired_dev}")
        sys.exit(1)
    print(f"Wired connection: {wired_conn}")
    print()

    # Sampling state
    samples: list[tuple[float, str, str, str]] = []  # (time, phase, dev, src)
    running = True
    phase = "baseline"

    def sampler():
        while running:
            d, s = get_route(tv_ip)
            samples.append((time.monotonic(), phase, d, s))
            time.sleep(0.01)  # 10ms

    t = threading.Thread(target=sampler, daemon=True)
    t.start()

    settle_time = 5  # seconds to sample after each transition

    print("Phase 1: baseline (eno2 only)")
    time.sleep(2)

    print("Phase 2: connecting Wi-Fi...")
    phase = "wifi_connecting"
    nmcli("up", wifi_conn)
    phase = "wifi_connected"
    print(f"  Wi-Fi connected, sampling for {settle_time}s")
    time.sleep(settle_time)

    print("Phase 3: disconnecting Ethernet...")
    phase = "eth_disconnecting"
    nmcli("down", wired_conn)
    phase = "eth_disconnected"
    print(f"  Ethernet disconnected, sampling for {settle_time}s")
    time.sleep(settle_time)

    print("Phase 4: reconnecting Ethernet...")
    phase = "eth_reconnecting"
    nmcli("up", wired_conn)
    phase = "eth_reconnected"
    print(f"  Ethernet reconnected, sampling for {settle_time}s")
    time.sleep(settle_time)

    print("Phase 5: disconnecting Wi-Fi...")
    phase = "wifi_disconnecting"
    nmcli("down", wifi_conn)
    phase = "wifi_disconnected"
    print(f"  Wi-Fi disconnected, sampling for {settle_time}s")
    time.sleep(settle_time)

    running = False
    t.join(timeout=1)

    # Analyze: print transitions
    print()
    print("=" * 80)
    print("TELEMETRY")
    print("=" * 80)
    print()

    t0 = samples[0][0]
    prev_dev = None
    prev_src = None
    transitions = []

    for ts, ph, dev, src in samples:
        if dev != prev_dev or src != prev_src:
            elapsed = ts - t0
            transitions.append((elapsed, ph, dev, src))
            prev_dev = dev
            prev_src = src

    print(f"{'Time':>8s}  {'Phase':<22s}  {'Device':<12s}  {'Src IP'}")
    print("-" * 70)
    for elapsed, ph, dev, src in transitions:
        print(f"{elapsed:8.3f}s  {ph:<22s}  {dev:<12s}  {src}")

    print()
    print(f"Total samples: {len(samples)}")
    print(f"Total transitions: {len(transitions)}")

    # Check for bounces: transitions where dev changed and then changed back
    bounces = []
    for i in range(1, len(transitions) - 1):
        _, _, d_prev, _ = transitions[i - 1]
        _, _, d_curr, _ = transitions[i]
        _, _, d_next, _ = transitions[i + 1]
        if d_prev == d_next and d_prev != d_curr:
            duration = transitions[i + 1][0] - transitions[i][0]
            bounces.append((transitions[i][0], d_prev, d_curr, duration))

    if bounces:
        print(f"\nDETECTED {len(bounces)} BOUNCE(S):")
        for elapsed, from_dev, to_dev, duration in bounces:
            print(f"  {elapsed:.3f}s: {from_dev} -> {to_dev} -> {from_dev} (lasted {duration*1000:.0f}ms)")
    else:
        print("\nNo bounces detected.")


if __name__ == "__main__":
    main()

#!/usr/bin/env nix-shell
#!nix-shell -i python3 -p python3
"""Measure how long TVs take to come back after a power cycle, a PC sleep and
a network blip, against the installed stack.

`run` drives the cycles and records when it acted and when each TV answered
again; `report` lines those times up with the daemon's journal. The TVs go
dark while it runs, so it's unattended: progress goes to stdout and the
results to the output file.

    nix-shell -p python3 --run \\
        'sudo "$(command -v python3)" scripts/bench_wake.py run --lg <uuid> --cec <uuid> --out bench.json'
    scripts/bench_wake.py report bench.json

Before `run`: stop any dev stack (`just stop-all`) and keep the desktop from
locking or blanking the screen. `run` stops the user's tisvlay-session for the
duration, so lock and idle automation can't interfere, and restarts it after.
Needs root for the RTC alarm, the suspend and the link toggle.
"""

import argparse
import json
import os
import statistics
import subprocess
import sys
import time
from datetime import datetime

# sudo resets PATH; the installed CLI and the system tools live here on NixOS.
os.environ["PATH"] = "/run/current-system/sw/bin:" + os.environ.get("PATH", "")

SOCKET = "/run/tisvlay/zenoh.sock"
DAEMON_UNIT = "tisvlay-daemon"
POLL_S = 0.25
ANSWER_TIMEOUT_S = 180
SETTLE_S = 15

# Daemon journal messages worth timing, matched exactly.
EVENTS = [
    "system going to sleep",
    "system resumed",
    "carrier lost, parking connection",
    "carrier restored, redialing",
    "WoL packet sent",
    "Connected via wss://",
    "LG TV online after wake",
    "Registered with TV, got client key",
]


# --- run ------------------------------------------------------------------


def cli(*args: str, check: bool = True) -> str:
    out = subprocess.run(
        ["tisvlay", "--socket", SOCKET, *args],
        capture_output=True, text=True, check=check, timeout=60,
    )
    return out.stdout.strip()


def power_state(tv: str) -> str:
    try:
        return cli("power", tv, check=False) or "unknown"
    except subprocess.TimeoutExpired:
        return "timeout"


def wait_for(tv: str, want: str, timeout: float = ANSWER_TIMEOUT_S) -> float | None:
    """Poll the power getter until it reads `want`; return that wall time."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if power_state(tv) == want:
            return time.time()
        time.sleep(POLL_S)
    return None


def mark(text: str) -> float:
    now = time.time()
    subprocess.run(["logger", "-t", "tisvlay-bench", text], check=False)
    print(f"{datetime.fromtimestamp(now):%H:%M:%S.%f} {text}", flush=True)
    return now


def dpms(connector: str | None) -> str | None:
    if not connector:
        return None
    try:
        with open(f"/sys/class/drm/{connector}/dpms") as f:
            return f.read().strip()
    except OSError:
        return None


def power_cycle(tv: str, label: str, off_s: float) -> dict:
    cycle = {"kind": f"power/{label}", "tv": tv, "off_s": off_s}
    cycle["off_cmd"] = mark(f"{label}: power off")
    cli("power", tv, "off", check=False)
    cycle["off_seen"] = wait_for(tv, "off")
    time.sleep(off_s)
    cycle["on_cmd"] = mark(f"{label}: power on after {off_s:.0f} s off")
    cli("power", tv, "on", check=False)
    cycle["on_seen"] = wait_for(tv, "on")
    mark(f"{label}: answered on" if cycle["on_seen"] else f"{label}: no answer")
    return cycle


def sleep_cycle(tv: str, alarm_s: int) -> dict:
    cycle = {"kind": "pc-sleep", "tv": tv, "alarm_s": alarm_s}
    subprocess.run(["rtcwake", "-m", "no", "-s", str(alarm_s)], check=True)
    cycle["suspend_cmd"] = mark(f"pc-sleep: suspend, RTC alarm in {alarm_s} s")
    # Through logind, so the daemon sees the sleep and runs on_sleep/on_wake.
    subprocess.run(["systemctl", "suspend", "--check-inhibitors=no"], check=True)
    time.sleep(alarm_s + 20)
    cycle["back"] = mark("pc-sleep: running again")
    cycle["on_seen"] = wait_for(tv, "on")
    mark("pc-sleep: TV answered on" if cycle["on_seen"] else "pc-sleep: no answer")
    return cycle


def blip_cycle(tv: str, iface: str, down_s: float) -> dict:
    cycle = {"kind": "blip", "tv": tv, "iface": iface, "down_s": down_s}
    cycle["down_cmd"] = mark(f"blip: {iface} down")
    subprocess.run(["ip", "link", "set", iface, "down"], check=True)
    time.sleep(down_s)
    cycle["up_cmd"] = mark(f"blip: {iface} up after {down_s:.0f} s")
    subprocess.run(["ip", "link", "set", iface, "up"], check=True)
    cycle["on_seen"] = wait_for(tv, "on")
    mark("blip: TV answered on" if cycle["on_seen"] else "blip: no answer")
    return cycle


def user_session(user: str, verb: str) -> None:
    subprocess.run(
        ["systemctl", "--user", f"--machine={user}@.host", verb, "tisvlay-session"],
        check=False,
    )


def preflight(args: argparse.Namespace) -> dict:
    if os.geteuid() != 0:
        sys.exit("run needs root (the RTC alarm, suspend and link toggle): use sudo")
    dev = subprocess.run(
        ["pgrep", "-f", r"target/(debug|release)/tisvlay service"],
        capture_output=True, text=True,
    )
    if dev.stdout.strip():
        sys.exit("a dev stack is running; stop it first (just stop-all)")
    listing = json.loads(cli("--json", "list"))
    for tv in filter(None, [args.lg, args.cec]):
        if tv not in listing:
            sys.exit(f"{tv} isn't a configured TV")
    identity = listing[args.lg]["meta/identity"]
    route = cli_route(identity["primary"])
    return {"connector": identity.get("connector"), "iface": args.iface or route}


def cli_route(ip: str) -> str:
    out = subprocess.run(["ip", "-o", "route", "get", ip], capture_output=True, text=True, check=True)
    words = out.stdout.split()
    return words[words.index("dev") + 1]


def run(args: argparse.Namespace) -> None:
    env = preflight(args)
    user = os.environ.get("SUDO_USER") or sys.exit("run it through sudo, so the user's session can be found")
    results = {"started": time.time(), "env": env, "cycles": []}

    def save() -> None:
        with open(args.out, "w") as f:
            json.dump(results, f, indent=2)

    plan: list[tuple] = []
    plan += [("power", args.lg, "lg-short", args.short_off_s)] * args.short
    plan += [("power", args.lg, "lg-long", args.long_off_s)] * args.long
    if args.cec:
        plan += [("power", args.cec, "cec", args.short_off_s)] * args.short
    plan += [("sleep", args.lg, args.alarm_s)] * args.sleeps
    plan += [("blip", args.lg, env["iface"], args.down_s)] * args.blips

    user_session(user, "stop")
    mark(f"bench: start, {len(plan)} cycles, session stopped")
    try:
        for step in plan:
            if power_state(args.lg) != "on":
                cli("power", args.lg, "on", check=False)
                wait_for(args.lg, "on")
            before = dpms(env["connector"])
            match step:
                case ("power", tv, label, off_s):
                    cycle = power_cycle(tv, label, off_s)
                case ("sleep", tv, alarm_s):
                    cycle = sleep_cycle(tv, alarm_s)
                case ("blip", tv, iface, down_s):
                    cycle = blip_cycle(tv, iface, down_s)
            cycle["dpms"] = [before, dpms(env["connector"])]
            results["cycles"].append(cycle)
            save()
            time.sleep(SETTLE_S)
    finally:
        cli("power", args.lg, "on", check=False)
        user_session(user, "start")
        results["finished"] = mark("bench: done, session restarted")
        save()


# --- report ---------------------------------------------------------------


KERNEL_RESUME = "PM: suspend exit"


def journal(since: float, until: float) -> list[tuple[float, str]]:
    """The daemon's timed events, plus the kernel's resume, which comes before
    the network and the daemon hear about it."""
    window = ["-o", "json", "--no-pager", "--since", f"@{since:.3f}", "--until", f"@{until:.3f}"]
    events = []
    for selector, wanted in ((["-u", DAEMON_UNIT], EVENTS), (["-k"], [KERNEL_RESUME])):
        out = subprocess.run(["journalctl", *selector, *window], capture_output=True, text=True, check=True)
        for line in out.stdout.splitlines():
            entry = json.loads(line)
            if entry.get("MESSAGE") in wanted:
                events.append((int(entry["__REALTIME_TIMESTAMP"]) / 1e6, entry["MESSAGE"]))
    return sorted(events)


def first_after(events: list[tuple[float, str]], msg: str, t: float) -> float | None:
    return next((ts for ts, m in events if m == msg and ts >= t), None)


def report(args: argparse.Namespace) -> None:
    with open(args.results) as f:
        results = json.load(f)
    end = results.get("finished") or time.time()
    events = journal(results["started"] - 5, end + 5)
    rows: dict[str, dict[str, list[float]]] = {}

    def add(kind: str, name: str, start: float | None, stop: float | None) -> None:
        if start is not None and stop is not None:
            rows.setdefault(kind, {}).setdefault(name, []).append(stop - start)

    for c in results["cycles"]:
        kind = c["kind"]
        if kind.startswith("power/"):
            t = c["on_cmd"]
            add(kind, "on command → WoL sent", t, first_after(events, "WoL packet sent", t))
            add(kind, "on command → wss connected", t, first_after(events, "Connected via wss://", t))
            add(kind, "on command → LG online", t, first_after(events, "LG TV online after wake", t))
            add(kind, "on command → reads on", t, c["on_seen"])
        elif kind == "pc-sleep":
            s = c["suspend_cmd"]
            t = first_after(events, KERNEL_RESUME, s)
            add(kind, "suspend → daemon saw sleep", s, first_after(events, "system going to sleep", s))
            add(kind, "resume → carrier restored", t, first_after(events, "carrier restored, redialing", s))
            add(kind, "resume → daemon saw resume", t, first_after(events, "system resumed", s))
            add(kind, "resume → WoL sent", t, first_after(events, "WoL packet sent", t or s))
            add(kind, "resume → LG online", t, first_after(events, "LG TV online after wake", t or s))
            add(kind, "resume → reads on", t, c["on_seen"])
        elif kind == "blip":
            add(kind, "down → carrier lost", c["down_cmd"], first_after(events, "carrier lost, parking connection", c["down_cmd"]))
            add(kind, "up → carrier restored", c["up_cmd"], first_after(events, "carrier restored, redialing", c["up_cmd"]))
            add(kind, "up → reads on", c["up_cmd"], c["on_seen"])
        if c.get("dpms") and "Off" in c["dpms"]:
            print(f"note: DPMS was off around the {kind} cycle at {datetime.fromtimestamp(c.get('on_cmd') or c.get('suspend_cmd') or c.get('down_cmd')):%H:%M:%S}")

    for kind, metrics in rows.items():
        print(f"\n{kind}")
        for name, values in metrics.items():
            values.sort()
            print(f"  {name:32} n={len(values)}  min {values[0]:6.2f}  median {statistics.median(values):6.2f}  max {values[-1]:6.2f} s")
    misses = [c["kind"] for c in results["cycles"] if not c.get("on_seen")]
    if misses:
        print(f"\nno answer within {ANSWER_TIMEOUT_S} s: {', '.join(misses)}")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="drive the cycles (root)")
    r.add_argument("--lg", required=True, help="the LG TV's UUID")
    r.add_argument("--cec", help="the CEC TV's UUID, to power-cycle it too")
    r.add_argument("--out", required=True, help="results file, written after every cycle")
    r.add_argument("--iface", help="interface to blip (default: the route to the LG TV)")
    r.add_argument("--short", type=int, default=3, help="short power cycles per TV")
    r.add_argument("--short-off-s", type=float, default=20)
    r.add_argument("--long", type=int, default=1, help="long-standby LG power cycles")
    r.add_argument("--long-off-s", type=float, default=600)
    r.add_argument("--sleeps", type=int, default=2, help="PC sleep cycles")
    r.add_argument("--alarm-s", type=int, default=45, help="seconds asleep")
    r.add_argument("--blips", type=int, default=2, help="network blips")
    r.add_argument("--down-s", type=float, default=5, help="seconds the link stays down")
    r.set_defaults(func=run)
    rep = sub.add_parser("report", help="line a results file up with the journal")
    rep.add_argument("results")
    rep.set_defaults(func=report)
    args = p.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()

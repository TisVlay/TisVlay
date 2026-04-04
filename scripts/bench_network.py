#!/usr/bin/env nix-shell
#!nix-shell -i python3 -p "python3.withPackages(ps: [ ps.systemd-python ])"
"""Benchmark network event timing for tisvlayd.

Measures latency between key events in the daemon's lifecycle by
reading journald entries with microsecond precision.

Usage:
    ./scripts/bench_network.py [--since MINUTES] [--unit UNIT]

Examples:
    ./scripts/bench_network.py                    # last 10 minutes
    ./scripts/bench_network.py --since 60         # last hour
    ./scripts/bench_network.py --unit tisvlayd    # specific unit
"""

import argparse
import re
from datetime import datetime, timedelta, timezone
from systemd import journal


# Key events to track, in expected chronological order.
EVENT_PATTERNS = [
    ("system_resume",      re.compile(r"System resumed from sleep")),
    ("on_startup",         re.compile(r"Executing on_startup")),
    ("wol_sent",           re.compile(r"WoL packet sent")),
    ("connecting",         re.compile(r"Connecting to LG TV")),
    ("ws_connected",       re.compile(r"Connected via wss?://")),
    ("registered",         re.compile(r"Registered with TV, got client key")),
    ("subscribed",         re.compile(r"Subscribed to backend state updates")),
    ("sub_update",         re.compile(r"Forwarding subscription update")),
    ("conn_change",        re.compile(r"Forwarding connection change")),
    ("sub_ended",          re.compile(r"Backend state subscription ended")),
    ("nm_state",           re.compile(r"NetworkManager|Network event|network restored|network lost")),
]

# Interesting latency pairs to measure.
LATENCY_PAIRS = [
    ("wol_sent",       "ws_connected",  "WoL → WebSocket connected"),
    ("wol_sent",       "subscribed",    "WoL → subscribed (full ready)"),
    ("wol_sent",       "sub_update",    "WoL → first state push"),
    ("system_resume",  "subscribed",    "Wake → subscribed"),
    ("connecting",     "ws_connected",  "TCP connect time"),
    ("ws_connected",   "registered",    "Registration handshake"),
    ("registered",     "subscribed",    "Subscribe setup"),
    ("sub_ended",      "conn_change",   "Subscription end → Connected=false"),
    ("nm_state",       "subscribed",    "NM event → reconnected"),
]


def read_events(since_minutes: int, unit: str) -> list[tuple[datetime, str, str]]:
    """Read journal entries and extract matching events."""
    j = journal.Reader()
    j.add_match(SYSLOG_IDENTIFIER=unit)
    j.seek_realtime(datetime.now(timezone.utc) - timedelta(minutes=since_minutes))

    events = []
    for entry in j:
        msg = entry.get("MESSAGE", "")
        ts = entry.get("__REALTIME_TIMESTAMP")
        if not msg or not ts:
            continue
        for name, pattern in EVENT_PATTERNS:
            if pattern.search(msg):
                events.append((ts, name, msg.strip()))
                break
    return events


def find_pairs(events: list[tuple[datetime, str, str]]) -> None:
    """Find and display latency between event pairs."""
    # Build lookup: event_name → list of timestamps (multiple occurrences)
    by_name: dict[str, list[datetime]] = {}
    for ts, name, _ in events:
        by_name.setdefault(name, []).append(ts)

    print("\n=== Key Latencies ===")
    for start_name, end_name, label in LATENCY_PAIRS:
        starts = by_name.get(start_name, [])
        ends = by_name.get(end_name, [])
        if not starts or not ends:
            continue
        # Use the last occurrence of each (most recent cycle)
        ts_start = starts[-1]
        # Find the first end event after the start
        ts_end = next((t for t in ends if t >= ts_start), None)
        if ts_end is None:
            continue
        delta = (ts_end - ts_start).total_seconds()
        print(f"  {label}: {delta:.3f}s")


def main():
    parser = argparse.ArgumentParser(description="Benchmark tisvlayd network timing")
    parser.add_argument("--since", type=int, default=10,
                        help="Look back N minutes (default: 10)")
    parser.add_argument("--unit", default="tisvlayd",
                        help="Syslog identifier (default: tisvlayd)")
    args = parser.parse_args()

    events = read_events(args.since, args.unit)

    if not events:
        print(f"No events found in the last {args.since} minutes for {args.unit}")
        return

    print(f"=== Event Timeline ({len(events)} events) ===")
    for ts, name, msg in events:
        ts_str = ts.strftime("%H:%M:%S.%f")[:-3]
        print(f"  {ts_str}  {name:15s}  {msg}")

    find_pairs(events)
    print()


if __name__ == "__main__":
    main()

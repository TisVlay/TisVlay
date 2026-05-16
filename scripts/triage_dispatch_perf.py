#!/usr/bin/env python3
"""Triage the QoS-dispatch refactor's screen_on cold-start latency.

The user's manual smoke test surfaced a wall-clock gap (~14 s) between
the pre- and post-refactor reliable `screen_on` path. The dominant cost
is the LG client's `REQUEST_MS = 10_000` ms timeout when the daemon's
WebSocket has gone stale (kernel-level network blip while the TV's
power state is changing). This script automates the comparison so the
user can run it on each branch back-to-back without driving lock/unlock
events by hand.

What it does
------------
1. Reads `docs/configs/daemon.toml` (override via `--config`) and pulls
   the first `[[tv]]` entry's IP / client_key.
2. Out-of-band: opens its own SSAP WebSocket to the TV using the same
   client_key, sends `ssap://system/turnOff` (matching the LG client's
   `crates/tisvlay-lg/src/endpoints.rs::power::TURN_OFF`) to emulate
   a remote-button press / power-save event, and closes its own
   socket cleanly.
3. Waits `--prep-delay` seconds so the daemon's view either drifts
   into the "WS-thinks-it's-connected-but-isn't" state (long delays
   sometimes don't reproduce — see "Caveats" below) or settles.
4. Subscribes to `org.freedesktop.DBus.Properties.PropertiesChanged`
   for the TV's per-object path so it can detect a fresh transition
   to `PowerState == "active"`.
5. Auto-detects the D-Bus surface:
   - If `Tv1.Dispatch` is present (post-refactor), calls
     `Tv1.Dispatch(action, qos_json)` with the QoS tier from `--qos`.
   - Otherwise calls the pre-refactor `Tv1.Screen.SetScreen(true)`
     (`--qos` is ignored — pre-refactor's direct D-Bus surface is
     always best-effort; the reliable tier on that branch only
     engages via session events).
6. Times the D-Bus call → `PowerState == "active"` transition.
   Prints a single line that's easy to grep across runs.

Caveats
-------
- Whether the SSAP `turnOff` reproduces the stale-WS scenario depends
  on the TV's firmware: clean shutdown sends a CLOSE frame and the
  daemon notices immediately (fast escalation on both branches); abrupt
  power-down without a CLOSE leaves the daemon's WS thinking it's alive
  until `REQUEST_MS` fires. If the script reports similar timings on
  both branches, the regression isn't reproducible via SSAP — fall back
  to the kernel-level inducer described below.
- Kernel-level reproducer (needs sudo, not implemented here): drop
  egress packets to the TV IP for ~12 s right before the D-Bus call,
  e.g. `sudo iptables -I OUTPUT -d $TV_IP -j DROP` then `DELETE` after
  the timer. That deterministically forces the daemon's `REQUEST_MS`
  path on the first `screen_on` write. The script prints the exact
  command in `--manual` mode so the user can paste it in another
  terminal at the right moment.

Usage
-----
    nix-shell -p python313Packages.websockets --run \\
        "python3 scripts/triage_dispatch_perf.py --qos reliable"

    # Compare apples-to-apples best-effort cold-start on both branches:
    python3 scripts/triage_dispatch_perf.py --qos best_effort

    # Manual: prep the TV yourself (lock + remote press), then run
    python3 scripts/triage_dispatch_perf.py --inducer manual

Recommended workflow
--------------------
1. On `feat/action-qos` HEAD: run the script 3× with `--qos reliable`.
2. `git checkout 93a564a`, rebuild, restart the stack.
3. Run the script 3× (pre-refactor only supports best-effort via direct
   D-Bus; the `--qos` arg is ignored on that branch).
4. Compare the printed `dispatch_latency_seconds` values.
"""

import argparse
import asyncio
import json
import ssl
import subprocess
import sys
import time
import tomllib
from pathlib import Path


# -----------------------------------------------------------------------------
# TOML config + busctl helpers
# -----------------------------------------------------------------------------


def load_tv_config(config_path: Path) -> dict:
    """Return the first `[[tv]]` entry from the daemon TOML.

    Raises if no `[[tv]]` is present or the vendor isn't `lg` (only LG
    is wired up out-of-band here).
    """
    with config_path.open("rb") as f:
        cfg = tomllib.load(f)

    tvs = cfg.get("tv") or []
    if not tvs:
        raise SystemExit(f"no [[tv]] entries in {config_path}")

    tv = tvs[0]
    if tv.get("vendor") != "lg":
        raise SystemExit(
            f"first TV in {config_path} is not vendor='lg' "
            f"(got {tv.get('vendor')!r}); out-of-band SSAP is LG-only"
        )

    lg = tv.get("lg") or {}
    return {
        "uuid": tv["uuid"],
        "ip": lg["ip"],
        "client_key": lg["client_key"],
        "scheme": lg.get("scheme", "wss"),
        "port": 3001 if lg.get("scheme", "wss") == "wss" else 3000,
    }


def tv_object_path(uuid: str) -> str:
    """zbus's per-TV object path uses underscore-encoded UUIDs."""
    return f"/org/tisvlay/Tv/{uuid.replace('-', '_')}"


def busctl(args: list[str], capture: bool = False, timeout_s: float | None = None) -> str:
    """Wrap `busctl --user` for the dev/session-bus daemon."""
    cmd = ["busctl", "--user", "--no-pager"]
    if timeout_s is not None:
        # busctl 's reply timeout is 25 s by default. The reliable
        # screen_on helper can run > 30 s in the bad path (10 s
        # REQUEST_MS + WoL + reconnect + TV boot), so callers issuing
        # a `call` for that should bump this.
        cmd += [f"--timeout={int(timeout_s)}"]
    cmd += args
    if capture:
        out = subprocess.run(cmd, check=True, capture_output=True, text=True)
        return out.stdout
    subprocess.run(cmd, check=True)
    return ""


def detect_dispatch_surface(object_path: str) -> bool:
    """True iff Tv1.Dispatch is present (post-refactor)."""
    introspect = busctl(
        ["introspect", "org.tisvlay.Daemon", object_path, "org.tisvlay.Tv1"],
        capture=True,
    )
    return "Dispatch" in introspect


def call_dispatch(object_path: str, action: str, qos_json: str, timeout_s: float) -> None:
    """Issue Tv1.Dispatch(action, qos)."""
    busctl(
        [
            "call",
            "org.tisvlay.Daemon",
            object_path,
            "org.tisvlay.Tv1",
            "Dispatch",
            "ss",
            action,
            qos_json,
        ],
        timeout_s=timeout_s,
    )


def call_set_screen(object_path: str, on: bool, timeout_s: float) -> None:
    """Pre-refactor fallback: Tv1.Screen.SetScreen(b)."""
    busctl(
        [
            "call",
            "org.tisvlay.Daemon",
            object_path,
            "org.tisvlay.Tv1.Screen",
            "SetScreen",
            "b",
            "true" if on else "false",
        ],
        timeout_s=timeout_s,
    )


def get_power_state(object_path: str) -> str:
    """Read the cached Tv1.Power.PowerState property."""
    raw = busctl(
        [
            "get-property",
            "org.tisvlay.Daemon",
            object_path,
            "org.tisvlay.Tv1.Power",
            "PowerState",
        ],
        capture=True,
    ).strip()
    # busctl prints `s "active"` for string properties.
    if raw.startswith('s "') and raw.endswith('"'):
        return raw[3:-1]
    return raw


def journal_marker(message: str) -> None:
    """Drop a marker into the user journal so the script's wall-clock
    timeline is interleaved with daemon traces. Best-effort: silently
    skips if systemd-cat isn't available."""
    try:
        subprocess.run(
            ["systemd-cat", "-t", "triage-dispatch-perf", "echo", message],
            check=True,
            input="",
            text=True,
            timeout=2,
        )
    except (subprocess.SubprocessError, FileNotFoundError):
        pass


# -----------------------------------------------------------------------------
# Out-of-band SSAP: power off the TV from our own WebSocket
# -----------------------------------------------------------------------------


async def ssap_power_off(tv: dict) -> None:
    """Connect to the TV with the existing client_key and send
    `ssap://com.webos.service.tvpower/power/turnOff` — emulates a
    remote-button press without going through tisvlayd."""
    import websockets

    ssl_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ssl_ctx.check_hostname = False
    ssl_ctx.verify_mode = ssl.CERT_NONE

    uri = f"{tv['scheme']}://{tv['ip']}:{tv['port']}"
    print(f"[ssap] connecting to {uri}", flush=True)

    async with websockets.connect(uri, ssl=ssl_ctx) as ws:
        # Send the FULL manifest verbatim from
        # crates/tisvlay-lg/src/protocol.rs Manifest::default(). Sending
        # a reduced manifest with an existing client_key causes the TV
        # to mutate the per-key permissions to the lesser set, which
        # globally downgrades the daemon's session (observed in early
        # iterations of this script). Keep this list in sync with the
        # LG client whenever protocol.rs::Manifest::default() changes.
        reg = {
            "type": "register",
            "id": "triage_reg",
            "payload": {
                "pairingType": "PROMPT",
                "client-key": tv["client_key"],
                "manifest": {
                    "manifestVersion": 1,
                    "appVersion": "1.1",
                    "signed": {
                        "created": "20140509",
                        "appId": "com.lge.test",
                        "vendorId": "com.lge",
                        "serial": "2f930e2d2cfe083771f68e4fe7bb07",
                        "permissions": [
                            "TEST_SECURE",
                            "CONTROL_INPUT_TEXT",
                            "CONTROL_MOUSE_AND_KEYBOARD",
                            "READ_INSTALLED_APPS",
                            "READ_LGE_SDX",
                            "READ_NOTIFICATIONS",
                            "SEARCH",
                            "WRITE_SETTINGS",
                            "WRITE_NOTIFICATION_ALERT",
                            "CONTROL_POWER",
                            "READ_CURRENT_CHANNEL",
                            "READ_RUNNING_APPS",
                            "READ_UPDATE_INFO",
                            "UPDATE_FROM_REMOTE_APP",
                            "READ_LGE_TV_INPUT_EVENTS",
                            "READ_TV_CURRENT_TIME",
                        ],
                    },
                    "permissions": [
                        "CONTROL_POWER",
                        "READ_POWER_STATE",
                        "CONTROL_DISPLAY",
                        "CONTROL_INPUT_TV",
                        "CONTROL_INPUT_MEDIA_PLAYBACK",
                        "CONTROL_AUDIO",
                        "LAUNCH",
                        "LAUNCH_WEBAPP",
                        "CLOSE",
                        "READ_APP_STATUS",
                        "READ_INSTALLED_APPS",
                        "READ_INPUT_DEVICE_LIST",
                        "READ_NETWORK_STATE",
                        "READ_TV_CHANNEL_LIST",
                        "READ_RUNNING_APPS",
                        "READ_UPDATE_INFO",
                        "READ_CURRENT_CHANNEL",
                        "CONTROL_INPUT_JOYSTICK",
                        "CONTROL_MOUSE_AND_KEYBOARD",
                        "READ_NOTIFICATIONS",
                        "READ_SETTINGS",
                        "CONTROL_TV_SCREEN",
                        "WRITE_NOTIFICATION_TOAST",
                    ],
                    "signatures": [
                        {
                            "signature": (
                                "eyJhbGdvcml0aG0iOiJSU0EtU0hBMjU2Iiwia2V5SWQiOiJ0ZXN0LXNp"
                                "Z25pbmctY2VydCIsInNpZ25hdHVyZVZlcnNpb24iOjF9.hrVRgjCwXVvE2OOSpDZ"
                                "58hR+59aFNwYDyjQgKk3auukd7pcegmE2CzPCa0bJ0ZsRAcKkCTJrWo5iDzNhMBWR"
                                "yaMOv5zWSrthlf7G128qvIlpMT0YNY+n/FaOHE73uLrS/g7swl3/qH/BGFG2Hu4RlL"
                                "48eb3lLKqTt2xKHdCs6Cd4RMfJPYnzgvI4BNrFUKsjkcu+WD4OO2A27Pq1n50cMchmcaXadJhGrOqH5"
                                "LAcDNKfbu0ntmGnjK4XeUq5Duzp+6hGzNZL5g0ByaJ1gBWNqjnfU+d0DYPzQ8kFmGvS+Zf"
                                "q+SB6FBVhJ+OjP2vBJlObaUT9zLHw8GdjN8iCq+iH/A=="
                            ),
                            "signatureVersion": 1,
                        }
                    ],
                },
            },
        }
        await ws.send(json.dumps(reg))
        # Drain the registration handshake (acknowledgment then
        # `registered` with the client key).
        for _ in range(4):
            raw = await asyncio.wait_for(ws.recv(), timeout=5)
            msg = json.loads(raw)
            if msg.get("type") == "registered":
                break
            if msg.get("type") == "error":
                raise SystemExit(f"[ssap] register failed: {msg}")
        else:
            raise SystemExit("[ssap] no 'registered' response in 4 frames")

        # Correct URI per crates/tisvlay-lg/src/endpoints.rs::power::TURN_OFF.
        cmd = {
            "type": "request",
            "id": "triage_off",
            "uri": "ssap://system/turnOff",
        }
        await ws.send(json.dumps(cmd))
        try:
            ack_raw = await asyncio.wait_for(ws.recv(), timeout=3)
            print(f"[ssap] turnOff ack: {ack_raw[:120]}", flush=True)
        except asyncio.TimeoutError:
            print("[ssap] turnOff sent (no ack; TV may have closed already)", flush=True)


# -----------------------------------------------------------------------------
# Wait for PowerState == "active" via PropertiesChanged
# -----------------------------------------------------------------------------


async def wait_for_active(object_path: str, timeout_s: float) -> float | None:
    """Subscribe to PropertiesChanged on `object_path` and wait for
    `Tv1.Power.PowerState == "active"`. Returns the elapsed seconds,
    or None on timeout.

    Implemented by spawning `busctl monitor` and parsing its output.
    Avoids a hard dependency on `dbus-next` / `jeepney`.
    """
    cmd = [
        "busctl",
        "--user",
        "--no-pager",
        "monitor",
        "--match",
        (
            f"type='signal',interface='org.freedesktop.DBus.Properties',"
            f"member='PropertiesChanged',path='{object_path}'"
        ),
    ]
    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.DEVNULL,
    )
    assert proc.stdout is not None
    started = time.monotonic()
    try:
        while True:
            if time.monotonic() - started > timeout_s:
                return None
            try:
                line_b = await asyncio.wait_for(proc.stdout.readline(), timeout=1.0)
            except asyncio.TimeoutError:
                continue
            if not line_b:
                return None
            line = line_b.decode("utf-8", errors="replace")
            # busctl monitor output is multi-line per signal; just look
            # for STRING "active" anywhere in the stream. Coarse but
            # adequate — the only property that ever serializes to
            # `"active"` on this path is Tv1.Power.PowerState.
            if 'STRING "active"' in line:
                return time.monotonic() - started
    finally:
        proc.terminate()
        try:
            await asyncio.wait_for(proc.wait(), timeout=2.0)
        except asyncio.TimeoutError:
            proc.kill()


# -----------------------------------------------------------------------------
# Main
# -----------------------------------------------------------------------------


async def run(args: argparse.Namespace) -> int:
    config_path = Path(args.config).resolve()
    if not config_path.exists():
        raise SystemExit(f"config not found: {config_path}")

    tv = load_tv_config(config_path)
    object_path = tv_object_path(tv["uuid"])
    print(f"[setup] tv ip={tv['ip']} uuid={tv['uuid']}", flush=True)
    print(f"[setup] object path: {object_path}", flush=True)

    has_dispatch = detect_dispatch_surface(object_path)
    surface = "Tv1.Dispatch" if has_dispatch else "Tv1.Screen.SetScreen"
    print(f"[setup] D-Bus surface: {surface}", flush=True)

    initial_state = get_power_state(object_path)
    print(f"[setup] initial PowerState: {initial_state!r}", flush=True)

    try:
        return await _run_inner(args, tv, object_path, has_dispatch, surface)
    finally:
        # Belt-and-suspenders: force the daemon to drop its connection
        # and re-register with its own full manifest. Restores elevated
        # permissions in case the TV mutated the client_key's perms
        # during our out-of-band SSAP register, and also fixes any
        # stale-WS state we may have induced.
        print("[cleanup] forcing daemon Reconnect to restore session", flush=True)
        try:
            # Reconnect is fast in the happy path (~1 s); a 30 s cap
            # is generous and matches the dispatch-call ceiling.
            busctl(
                [
                    "call",
                    "org.tisvlay.Daemon",
                    object_path,
                    "org.tisvlay.Tv1",
                    "Reconnect",
                ],
                timeout_s=30.0,
            )
        except subprocess.CalledProcessError as e:
            print(f"[cleanup] Reconnect failed: {e}", flush=True)


async def _run_inner(
    args: argparse.Namespace,
    tv: dict,
    object_path: str,
    has_dispatch: bool,
    surface: str,
) -> int:

    # ------------------------------------------------------------------
    # Phase 1: induce the bad state
    # ------------------------------------------------------------------

    if args.inducer == "ssap-turnoff":
        await ssap_power_off(tv)
    elif args.inducer == "manual":
        print(
            "[manual] put the TV into its bad state now "
            "(lock + remote-press, or sudo iptables -I OUTPUT "
            f"-d {tv['ip']} -j DROP). Press Enter when ready...",
            flush=True,
        )
        sys.stdin.readline()
    else:
        raise SystemExit(f"unknown inducer: {args.inducer}")

    if args.prep_delay > 0:
        print(f"[setup] sleeping {args.prep_delay}s before Dispatch", flush=True)
        await asyncio.sleep(args.prep_delay)

    state_before = get_power_state(object_path)
    print(f"[setup] PowerState before Dispatch: {state_before!r}", flush=True)

    # ------------------------------------------------------------------
    # Phase 2: subscribe + issue Dispatch + time the transition
    # ------------------------------------------------------------------

    journal_marker(f"triage-dispatch-perf: BEGIN qos={args.qos} surface={surface}")

    # Start the monitor BEFORE the dispatch so we don't race the signal.
    waiter = asyncio.create_task(wait_for_active(object_path, args.timeout))
    # Tiny grace so busctl monitor has its subscription up.
    await asyncio.sleep(0.2)

    # busctl --timeout: the realistic worst-case for the reliable
    # smoke-test path is ~30-40 s (REQUEST_MS + escalation + WoL +
    # TV boot to Registration). Cap at 45 s — anything longer means
    # the daemon is stuck, not slow, and we should bail to let the
    # finally-block Reconnect recover the session.
    call_timeout_s = 45.0
    t0 = time.monotonic()
    call_failed_at: float | None = None
    try:
        if has_dispatch:
            qos_json = json.dumps({"reliability": args.qos})
            print(f"[call] Tv1.Dispatch screen_on {qos_json}", flush=True)
            call_dispatch(object_path, "screen_on", qos_json, timeout_s=call_timeout_s)
        else:
            # Pre-refactor branch — direct D-Bus is best-effort only.
            if args.qos != "best_effort":
                print(
                    "[warn] pre-refactor D-Bus has no reliable tier; "
                    "issuing Tv1.Screen.SetScreen(true) as best-effort",
                    flush=True,
                )
            print("[call] Tv1.Screen.SetScreen true", flush=True)
            call_set_screen(object_path, True, timeout_s=call_timeout_s)
        call_returned_at = time.monotonic() - t0
        print(f"[call] returned Ok in {call_returned_at:.2f}s", flush=True)
    except subprocess.CalledProcessError as e:
        call_failed_at = time.monotonic() - t0
        print(
            f"[call] returned Err in {call_failed_at:.2f}s "
            f"(stderr was on console; still waiting for property transition)",
            flush=True,
        )

    elapsed = await waiter

    journal_marker(
        f"triage-dispatch-perf: END qos={args.qos} elapsed={elapsed!r}"
    )

    if elapsed is None:
        print(
            f"[result] dispatch_latency_seconds=TIMEOUT "
            f"(no transition to 'active' within {args.timeout}s) "
            f"surface={surface} qos={args.qos}",
            flush=True,
        )
        return 2

    print(
        f"[result] dispatch_latency_seconds={elapsed:.2f} "
        f"surface={surface} qos={args.qos}",
        flush=True,
    )
    return 0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__.split("\n\n", 1)[0])
    p.add_argument(
        "--config",
        default="docs/configs/daemon.toml",
        help="daemon TOML to read TV IP / client_key from",
    )
    p.add_argument(
        "--inducer",
        choices=["ssap-turnoff", "manual"],
        default="ssap-turnoff",
        help=(
            "how to put the TV into a bad state: 'ssap-turnoff' sends "
            "an out-of-band power off; 'manual' prompts the user to "
            "prep the TV (useful with a sudo iptables drop rule)"
        ),
    )
    p.add_argument(
        "--qos",
        choices=["reliable", "best_effort"],
        default="reliable",
        help=(
            "QoS tier for Tv1.Dispatch (ignored on pre-refactor branch; "
            "direct D-Bus there is always best-effort)"
        ),
    )
    p.add_argument(
        "--prep-delay",
        type=float,
        default=3.0,
        help=(
            "seconds to wait after inducing the bad state before "
            "issuing Dispatch — tune this to land in the stale-WS window"
        ),
    )
    p.add_argument(
        "--timeout",
        type=float,
        default=45.0,
        help=(
            "seconds to wait for PowerState transition to 'active' "
            "after the dispatch returns. Real worst-case "
            "smoke-test latency is ~30-40 s (REQUEST_MS + WoL + boot); "
            "default 45 s is the bail-out for stuck-daemon territory."
        ),
    )
    return p.parse_args()


if __name__ == "__main__":
    asyncio.run(run(parse_args()))

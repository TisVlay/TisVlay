#!/usr/bin/env python3
"""Measure SSAP subscription responsiveness to a power-off transition.

Three phases in one run, on a single WebSocket:

1. **Discovery** — query LG introspection endpoints (`getServiceList`,
   `getConfigs`, etc.) and print whatever they expose. Lets us audit
   what subscribable endpoints actually exist on this firmware before
   committing to a probe set.

2. **Probe** — subscribe to a broad set of power/screen-related
   endpoints and record each subscription's initial response so we
   can tell which ones the firmware actually supports.

3. **Trigger + capture** — fire `ssap://system/turnOff` over the same
   WebSocket (single socket so no risk of two clients fighting over
   subscriptions), then record every push that arrives with elapsed
   time from the moment the turnOff request went out. WS will close
   once the TV reaches deep standby; that closes the capture window.

Output ends with a per-endpoint summary: first-push latency, total
pushes received, and a tail of the most recent payload. The fastest
& most accurate endpoint is the candidate for replacing
`getPowerState` in the LG client's subscription set.

**Stop the daemon before running** — two clients holding registrations
with the same key can confuse LG firmware. The script reuses the
daemon's stored client key.

Usage:
    nix-shell -p python313Packages.websockets --run \\
        "python3 scripts/probe_power_state_timing.py"

Caveat: SSAP `turnOff` and the IR/BLE remote button MAY take different
code paths in LG firmware. Subscriptions that fire fast under SSAP
turnOff don't necessarily fire fast under a remote press — confirm
with the daemon's logs after a manual remote-off test.
"""

import asyncio
import json
import ssl
import sys
import time
from dataclasses import dataclass, field

# --- Configuration ---
TV_IP = "192.168.1.170"
CLIENT_KEY = "bec7b2f5afe657547d367e84cbf59095"
TV_PORT = 3001  # wss
PROBE_INITIAL_RESPONSE_TIMEOUT = 3.0  # seconds per subscribe
CAPTURE_WINDOW = 45.0  # seconds to capture pushes after turnOff
RECV_IDLE_TIMEOUT = 5.0  # seconds; loop quits if no msg in this window AFTER turnOff

# --- Discovery endpoints ---
# One-shot queries to enumerate what LG exposes. Each entry is
# (uri, optional_payload, label_for_summary).
DISCOVERY = [
    ("ssap://api/getServiceList", None, "service list"),
    (
        "ssap://com.webos.service.config/getConfigs",
        {"configNames": ["tv.hw.*"]},
        "config tv.hw.*",
    ),
    (
        "ssap://com.webos.service.config/getConfigs",
        {"configNames": ["tv.config.*"]},
        "config tv.config.*",
    ),
    (
        "ssap://com.webos.service.config/getConfigs",
        {"configNames": ["system.*"]},
        "config system.*",
    ),
]

# --- Subscription candidates ---
# Cast a moderately wide net at power / screen / standby / display.
# Each entry: (uri, optional_payload). Items that LG doesn't expose
# will show up as `errorText` in the probe phase and we can drop them.
CANDIDATES = [
    # Power family
    ("ssap://com.webos.service.tvpower/power/getPowerState", None),
    ("ssap://com.webos.service.tvpower/power/getPowerStatus", None),
    ("ssap://com.webos.service.tvpower/power/getStandbyState", None),
    # Screen family
    ("ssap://com.webos.service.tvpower/screen/getScreenState", None),
    ("ssap://com.webos.service.tv.display/getStatus", None),
    # Foreground app / system app monitor — flips when LG goes to
    # screensaver / lock state.
    ("ssap://com.webos.applicationManager/getForegroundAppInfo", None),
    # Audio (control test — should fire on volume changes; baseline
    # responsiveness reference for comparison).
    ("ssap://audio/getVolume", None),
    ("ssap://audio/getMute", None),
    # System settings (picture / option category) — picture mode flips
    # when LG enters power-saving substates.
    (
        "ssap://settings/getSystemSettings",
        {"category": "picture", "keys": ["backlight"]},
    ),
    (
        "ssap://settings/getSystemSettings",
        {"category": "option", "keys": ["powerState"]},
    ),
    # Network / connectivity (NIC stays alive through ActiveStandby;
    # may or may not push state-change events tied to power transitions).
    ("ssap://com.webos.service.connectionmanager/getStatus", None),
    # System / device info — likely static but cheap to confirm.
    ("ssap://com.webos.service.deviceinfo/getInfo", None),
    # Notifications / overlays — LG sometimes pushes a banner when
    # entering standby.
    ("ssap://com.webos.notification/getStatus", None),
]


@dataclass
class SubInfo:
    """Tracks a single subscription's lifetime stats."""

    label: str
    uri: str
    payload: dict | None
    initial_status: str = "(no response)"
    initial_payload: str = ""
    pushes: list[tuple[float, str]] = field(default_factory=list)
    # (elapsed_from_t0_seconds, payload_excerpt)


def reg_request() -> dict:
    """Build the SSAP register request. Same manifest as the daemon."""
    return {
        "type": "register",
        "id": "reg_0",
        "payload": {
            "pairingType": "PROMPT",
            "client-key": CLIENT_KEY,
            "manifest": {
                "manifestVersion": 1,
                "appVersion": "1.1",
                "signed": {
                    "created": "20140509",
                    "appId": "com.lge.test",
                    "vendorId": "com.lge",
                    "localizedAppNames": {"": "LG Remote App"},
                    "localizedVendorNames": {"": "LG Electronics"},
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
                        "CONTROL_DISPLAY",
                        "CONTROL_TV_SCREEN",
                        "CONTROL_TV_STANDBY",
                        "READ_SETTINGS",
                        "READ_NETWORK_STATE",
                    ],
                    "serial": "2f930e2d2cfe083771f68e4fe3fb2c7",
                },
                "permissions": [
                    "LAUNCH",
                    "CONTROL_POWER",
                    "CONTROL_TV_SCREEN",
                    "CONTROL_TV_STANDBY",
                    "READ_SETTINGS",
                    "WRITE_SETTINGS",
                    "READ_NETWORK_STATE",
                ],
            },
        },
    }


def excerpt(payload: dict, limit: int = 140) -> str:
    s = json.dumps(payload, separators=(",", ":"))
    return s if len(s) <= limit else s[:limit] + "..."


async def main():
    import websockets

    ssl_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ssl_ctx.check_hostname = False
    ssl_ctx.verify_mode = ssl.CERT_NONE

    uri = f"wss://{TV_IP}:{TV_PORT}"
    print(f"Connecting to {uri}...")

    async with websockets.connect(uri, ssl=ssl_ctx, max_size=4 * 1024 * 1024) as ws:
        # ---- Register ----
        await ws.send(json.dumps(reg_request()))
        reg_response = json.loads(await ws.recv())
        if reg_response.get("type") != "registered":
            print(f"  registration failed: {reg_response}")
            return
        print(
            f"  registered (key: {reg_response.get('payload', {}).get('client-key', 'N/A')[:8]}...)"
        )

        req_id = 0

        # =====================================================
        # Phase 1: Discovery
        # =====================================================
        print("\n" + "=" * 70)
        print("DISCOVERY")
        print("=" * 70)

        for d_uri, d_payload, d_label in DISCOVERY:
            req_id += 1
            req = {"type": "request", "id": f"disc_{req_id}", "uri": d_uri}
            if d_payload is not None:
                req["payload"] = d_payload
            await ws.send(json.dumps(req))
            try:
                resp_raw = await asyncio.wait_for(
                    ws.recv(), timeout=PROBE_INITIAL_RESPONSE_TIMEOUT
                )
                resp = json.loads(resp_raw)
                p = resp.get("payload", {})
                ok = p.get("returnValue", None)
                err = p.get("errorText") or resp.get("error")
                marker = "✓" if ok is True else "✗"
                print(f"  {marker} {d_label}")
                if err:
                    print(f"      error: {err}")
                else:
                    print(f"      payload: {excerpt(p, 300)}")
            except asyncio.TimeoutError:
                print(f"  ⏱ {d_label}: timeout")

        # =====================================================
        # Phase 2: Probe (subscribe and record initial responses)
        # =====================================================
        print("\n" + "=" * 70)
        print("SUBSCRIPTION PROBE")
        print("=" * 70)

        subs: dict[str, SubInfo] = {}
        for c_uri, c_payload in CANDIDATES:
            req_id += 1
            sub_id = f"sub_{req_id}"
            req = {"type": "subscribe", "id": sub_id, "uri": c_uri}
            if c_payload is not None:
                req["payload"] = c_payload

            label = c_uri.split("/", 3)[-1]
            if c_payload is not None:
                # Tag with payload keys for disambiguation.
                tag = ",".join(c_payload.get("keys", [])) or json.dumps(c_payload)[:30]
                label = f"{label} [{tag}]"

            info = SubInfo(label=label, uri=c_uri, payload=c_payload)
            subs[sub_id] = info

            await ws.send(json.dumps(req))
            try:
                resp_raw = await asyncio.wait_for(
                    ws.recv(), timeout=PROBE_INITIAL_RESPONSE_TIMEOUT
                )
                resp = json.loads(resp_raw)
                p = resp.get("payload", {})
                subscribed = p.get("subscribed", False)
                ok = p.get("returnValue", None)
                err = p.get("errorText") or resp.get("error")

                if err:
                    info.initial_status = f"ERROR: {err}"
                elif subscribed:
                    info.initial_status = "SUBSCRIBED"
                elif ok is False:
                    info.initial_status = "FAILED"
                else:
                    info.initial_status = f"unsubscribed (returnValue={ok})"

                info.initial_payload = excerpt(p, 140)
                marker = "✓" if subscribed else ("✗" if err or ok is False else "?")
                print(f"  {marker} {label}: {info.initial_status}")
                print(f"      {info.initial_payload}")
            except asyncio.TimeoutError:
                info.initial_status = "TIMEOUT"
                print(f"  ⏱ {label}: timeout (no initial response)")

        # =====================================================
        # Phase 3: Trigger turnOff + capture pushes
        # =====================================================
        print("\n" + "=" * 70)
        print("TRIGGER turnOff + CAPTURE")
        print(f"  capturing for up to {CAPTURE_WINDOW:.0f}s "
              f"(or until {RECV_IDLE_TIMEOUT:.0f}s of silence)")
        print("=" * 70)

        req_id += 1
        turnoff_id = f"turnoff_{req_id}"
        turnoff_req = {"type": "request", "id": turnoff_id, "uri": "ssap://system/turnOff"}

        t0 = time.monotonic()
        await ws.send(json.dumps(turnoff_req))
        print(f"  [+0.000s] sent turnOff")

        last_msg_at = t0
        deadline = t0 + CAPTURE_WINDOW
        try:
            while time.monotonic() < deadline:
                remaining = deadline - time.monotonic()
                idle_left = RECV_IDLE_TIMEOUT - (time.monotonic() - last_msg_at)
                wait_for = max(0.05, min(remaining, idle_left))
                try:
                    msg_raw = await asyncio.wait_for(ws.recv(), timeout=wait_for)
                except asyncio.TimeoutError:
                    if time.monotonic() - last_msg_at >= RECV_IDLE_TIMEOUT:
                        print(f"  [+{time.monotonic() - t0:.3f}s] idle timeout, stopping")
                        break
                    continue

                elapsed = time.monotonic() - t0
                last_msg_at = time.monotonic()
                msg = json.loads(msg_raw)
                msg_id = msg.get("id", "?")
                p = msg.get("payload", {})
                excerpt_str = excerpt(p, 140)

                if msg_id == turnoff_id:
                    print(f"  [+{elapsed:.3f}s] turnOff ack: {excerpt_str}")
                    continue

                info = subs.get(msg_id)
                if info is None:
                    print(f"  [+{elapsed:.3f}s] unknown id {msg_id}: {excerpt_str}")
                    continue

                info.pushes.append((elapsed, excerpt_str))
                print(f"  [+{elapsed:.3f}s] {info.label}")
                print(f"      {excerpt_str}")
        except Exception as e:
            elapsed = time.monotonic() - t0
            print(f"  [+{elapsed:.3f}s] connection closed: {e}")

        # =====================================================
        # Summary
        # =====================================================
        print("\n" + "=" * 70)
        print("SUMMARY (by first-push latency)")
        print("=" * 70)

        active = [info for info in subs.values() if info.pushes]
        active.sort(key=lambda s: s.pushes[0][0])

        if not active:
            print("  (no pushes received from any subscription)")
        else:
            for info in active:
                first_latency, first_payload = info.pushes[0]
                last_latency, last_payload = info.pushes[-1]
                print(
                    f"  +{first_latency:6.3f}s  {info.label}  "
                    f"({len(info.pushes)} push{'es' if len(info.pushes) != 1 else ''})"
                )
                print(f"      first: {first_payload}")
                if len(info.pushes) > 1:
                    print(f"      last:  {last_payload}")

        silent = [info for info in subs.values() if not info.pushes]
        if silent:
            print(f"\n  silent subscriptions ({len(silent)}):")
            for info in silent:
                print(f"    - {info.label}  [{info.initial_status}]")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\ninterrupted")
        sys.exit(130)

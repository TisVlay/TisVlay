#!/usr/bin/env python3
"""Probe LG TV SSAP subscription support for various endpoints.

Connects to the TV, registers with existing client key, then subscribes
to a list of endpoints (both documented and undocumented) to see which
ones support push-based updates.

Usage:
    nix-shell -p python313Packages.websockets --run \
        "python3 scripts/probe_subscriptions.py"

Change volume, backlight, input, etc. on the TV remote while this runs
to see which subscriptions fire.
"""

import asyncio
import json
import ssl
import sys
import time

# --- Configuration ---
TV_IP = "192.168.1.170"
CLIENT_KEY = "bec7b2f5afe657547d367e84cbf59095"
TV_PORT = 3001  # wss
TIMEOUT = 60  # seconds to listen for subscription updates

# Endpoints to probe for subscription support.
# Format: (uri, optional_payload)
ENDPOINTS = [
    # Known working SSAP subscriptions
    ("ssap://com.webos.service.tvpower/power/getPowerState", {}),
    ("ssap://audio/getVolume", {}),
    ("ssap://audio/getMute", {}),
    ("ssap://tv/getExternalInputList", {}),
    ("ssap://com.webos.applicationManager/getForegroundAppInfo", {}),
    ("ssap://audio/getSoundOutput", {}),

    # Undocumented / experimental — picture settings
    ("ssap://settings/getSystemSettings", {"category": "picture", "keys": ["backlight"]}),
    ("ssap://settings/getSystemSettings", {"category": "picture", "keys": ["backlight", "brightness", "contrast", "color"]}),
    ("ssap://settings/getSystemSettings", {"category": "picture"}),

    # Sound settings (documented as subscribable via Luna)
    ("ssap://settings/getSystemSettings", {"category": "sound", "keys": ["soundOutput"]}),

    # System settings
    ("ssap://settings/getSystemSettings", {"category": "option", "keys": ["country"]}),

    # Luna-style via SSAP (may not work)
    ("ssap://com.webos.settingsservice/getSystemSettings", {"category": "picture", "keys": ["backlight"]}),

    # Other interesting endpoints
    ("ssap://com.webos.service.tv.display/getStatus", {}),
    ("ssap://com.webos.service.config/getConfigs", {"configNames": ["tv.hw.displayType"]}),
]


async def main():
    import websockets

    # Create SSL context that accepts self-signed certs
    ssl_ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ssl_ctx.check_hostname = False
    ssl_ctx.verify_mode = ssl.CERT_NONE

    uri = f"wss://{TV_IP}:{TV_PORT}"
    print(f"Connecting to {uri}...")

    async with websockets.connect(uri, ssl=ssl_ctx) as ws:
        # Register
        reg_payload = {
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
                            "TEST_SECURE", "CONTROL_INPUT_TEXT", "CONTROL_MOUSE_AND_KEYBOARD",
                            "READ_INSTALLED_APPS", "READ_LGE_SDX", "READ_NOTIFICATIONS",
                            "SEARCH", "WRITE_SETTINGS", "WRITE_NOTIFICATION_ALERT",
                            "CONTROL_POWER", "READ_CURRENT_CHANNEL", "READ_RUNNING_APPS",
                            "READ_UPDATE_INFO", "UPDATE_FROM_REMOTE_APP", "READ_LGE_TV_INPUT_EVENTS",
                            "READ_TV_CURRENT_TIME", "READ_APP_STATUS", "CONTROL_DISPLAY",
                            "CONTROL_INPUT_JOYSTICK", "CONTROL_INPUT_MEDIA_RECORDING",
                            "CONTROL_INPUT_MEDIA_PLAYBACK", "READ_INPUT_DEVICE_LIST",
                            "CONTROL_INPUT_TV", "LAUNCH", "LAUNCH_WEBAPP",
                            "READ_SETTINGS", "CONTROL_TV_SCREEN", "CONTROL_TV_STANDBY",
                            "CONTROL_FAVORITE_GROUP", "CONTROL_USER_INFO",
                            "CHECK_BLUETOOTH_DEVICE", "CONTROL_BLUETOOTH",
                            "CONTROL_TIMER_INFO", "STB_INTERNAL_CONNECTION",
                            "CONTROL_RECORDING", "READ_RECORDING_STATE",
                            "WRITE_RECORDING_LIST", "READ_RECORDING_LIST",
                            "READ_RECORDING_SCHEDULE", "WRITE_RECORDING_SCHEDULE",
                            "READ_STORAGE_DEVICE_LIST", "READ_TV_CHANNEL_LIST",
                            "CONTROL_CHANNEL_GROUP", "WRITE_CHANNEL_GROUP",
                            "READ_CHANNEL_GROUP"
                        ],
                        "serial": "2f930e2d2cfe083771f68e4fe3fb2c7",
                    },
                    "permissions": [
                        "LAUNCH", "LAUNCH_WEBAPP", "APP_TO_APP",
                        "CONTROL_AUDIO", "CONTROL_DISPLAY", "CONTROL_INPUT_JOYSTICK",
                        "CONTROL_INPUT_MEDIA_PLAYBACK", "CONTROL_INPUT_MEDIA_RECORDING",
                        "CONTROL_INPUT_TEXT", "CONTROL_INPUT_TV", "CONTROL_MOUSE_AND_KEYBOARD",
                        "CONTROL_POWER", "CONTROL_TV_SCREEN", "CONTROL_TV_STANDBY",
                        "READ_APP_STATUS", "READ_CURRENT_CHANNEL", "READ_INPUT_DEVICE_LIST",
                        "READ_INSTALLED_APPS", "READ_NETWORK_STATE", "READ_RUNNING_APPS",
                        "READ_TV_CHANNEL_LIST", "READ_TV_CURRENT_TIME", "REMOVE_APP",
                        "SEARCH", "WRITE_NOTIFICATION_ALERT", "WRITE_NOTIFICATION_TOAST",
                        "WRITE_SETTINGS", "READ_SETTINGS", "CHECK_BLUETOOTH_DEVICE",
                        "CONTROL_BLUETOOTH", "CONTROL_RECORDING",
                    ],
                },
            },
        }

        await ws.send(json.dumps(reg_payload))
        reg_response = json.loads(await ws.recv())
        if reg_response.get("type") == "registered":
            print(f"✓ Registered (key: {reg_response.get('payload', {}).get('client-key', 'N/A')[:8]}...)")
        else:
            print(f"✗ Registration failed: {reg_response}")
            return

        # Subscribe to all endpoints
        req_id = 1
        id_to_info = {}

        for uri_str, payload in ENDPOINTS:
            req_id += 1
            req = {
                "type": "subscribe",
                "id": f"sub_{req_id}",
                "uri": uri_str,
            }
            if payload:
                req["payload"] = payload

            label = uri_str.split("/")[-1]
            if payload:
                label += f" ({json.dumps(payload)[:50]})"

            await ws.send(json.dumps(req))
            id_to_info[f"sub_{req_id}"] = label

            # Read initial response
            try:
                resp_raw = await asyncio.wait_for(ws.recv(), timeout=3)
                resp = json.loads(resp_raw)
                resp_id = resp.get("id", "")
                resp_type = resp.get("type", "")
                subscribed = resp.get("payload", {}).get("subscribed", False)
                return_value = resp.get("payload", {}).get("returnValue", None)
                error = resp.get("error")

                if error:
                    status = f"✗ ERROR: {error}"
                elif subscribed:
                    status = f"✓ SUBSCRIBED"
                elif return_value is False:
                    error_text = resp.get("payload", {}).get("errorText", "unknown")
                    status = f"✗ FAILED: {error_text}"
                else:
                    status = f"? response (subscribed={subscribed})"

                # Show initial value snippet
                payload_str = json.dumps(resp.get("payload", {}))
                if len(payload_str) > 80:
                    payload_str = payload_str[:80] + "..."

                print(f"  [{resp_id}] {label}: {status}")
                print(f"          Initial: {payload_str}")

            except asyncio.TimeoutError:
                print(f"  [sub_{req_id}] {label}: ✗ TIMEOUT (no response)")

        print(f"\n{'='*60}")
        print(f"Listening for subscription updates for {TIMEOUT}s...")
        print(f"Change volume, backlight, input on the TV remote now!")
        print(f"{'='*60}\n")

        # Listen for subscription updates
        start = time.time()
        update_count = 0
        while time.time() - start < TIMEOUT:
            try:
                msg_raw = await asyncio.wait_for(ws.recv(), timeout=5)
                msg = json.loads(msg_raw)
                msg_id = msg.get("id", "?")
                label = id_to_info.get(msg_id, f"unknown ({msg_id})")
                payload_str = json.dumps(msg.get("payload", {}))
                if len(payload_str) > 120:
                    payload_str = payload_str[:120] + "..."

                elapsed = time.time() - start
                update_count += 1
                print(f"  [{elapsed:6.1f}s] [{msg_id}] {label}")
                print(f"           {payload_str}")

            except asyncio.TimeoutError:
                elapsed = time.time() - start
                print(f"  [{elapsed:6.1f}s] ... (no updates)")

        print(f"\nDone. Received {update_count} subscription updates.")


if __name__ == "__main__":
    asyncio.run(main())

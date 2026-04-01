#!/usr/bin/env python3
"""Targeted probe: volume subscription updates + backlight write methods.

Test 1: Subscribe to getVolume, then change volume with remote to see
        if updates push despite missing "subscribed" flag in initial response.

Test 2: Try writing backlight via various methods to see if any work
        without the Luna createAlert workaround.

Usage:
    nix-shell -p python313Packages.websockets --run \
        "python3 scripts/probe_volume_and_backlight.py"
"""

import asyncio
import json
import ssl
import time

TV_IP = "192.168.1.170"
CLIENT_KEY = "bec7b2f5afe657547d367e84cbf59095"
TV_PORT = 3001


async def main():
    import websockets

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
                            "READ_CHANNEL_GROUP", "CONTROL_AUDIO",
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
            print(f"✓ Registered\n")
        else:
            print(f"✗ Registration failed: {reg_response}")
            return

        req_id = 0

        async def send_req(type_, uri_, payload=None):
            nonlocal req_id
            req_id += 1
            req = {"type": type_, "id": f"r{req_id}", "uri": uri_}
            if payload:
                req["payload"] = payload
            await ws.send(json.dumps(req))
            try:
                resp_raw = await asyncio.wait_for(ws.recv(), timeout=5)
                return json.loads(resp_raw)
            except asyncio.TimeoutError:
                return {"error": "TIMEOUT"}

        async def drain_messages(seconds, label=""):
            """Read all messages for N seconds, print subscription updates."""
            start = time.time()
            count = 0
            while time.time() - start < seconds:
                try:
                    raw = await asyncio.wait_for(ws.recv(), timeout=2)
                    msg = json.loads(raw)
                    msg_id = msg.get("id", "?")
                    payload = msg.get("payload", {})
                    payload_str = json.dumps(payload)
                    if len(payload_str) > 150:
                        payload_str = payload_str[:150] + "..."
                    elapsed = time.time() - start
                    print(f"  [{elapsed:5.1f}s] [{msg_id}] {payload_str}")
                    count += 1
                except asyncio.TimeoutError:
                    pass
            return count

        # =====================================================================
        # TEST 1: Volume subscription push test
        # =====================================================================
        print("=" * 60)
        print("TEST 1: Volume subscription — do updates push?")
        print("=" * 60)

        resp = await send_req("subscribe", "ssap://audio/getVolume")
        print(f"\nInitial response:")
        print(f"  {json.dumps(resp, indent=2)[:300]}")
        vol_id = resp.get("id", "?")

        print(f"\n>>> Change volume with TV remote now! (10 seconds)")
        count = await drain_messages(10, "volume")
        if count > 0:
            print(f"\n✓ Volume subscription pushes updates! ({count} received)")
        else:
            print(f"\n✗ No volume updates received")

        # =====================================================================
        # TEST 2: Backlight write methods
        # =====================================================================
        print("\n" + "=" * 60)
        print("TEST 2: Backlight write methods (trying alternatives)")
        print("=" * 60)

        # First, read current backlight
        resp = await send_req("request", "ssap://settings/getSystemSettings",
                             {"category": "picture", "keys": ["backlight"]})
        current_bl = resp.get("payload", {}).get("settings", {}).get("backlight", "?")
        print(f"\nCurrent backlight: {current_bl}")
        target_bl = (int(current_bl) + 1) if isinstance(current_bl, int) else 50

        # Method 1: ssap://settings/setSystemSettings (known to fail with 401)
        print(f"\n--- Method 1: ssap://settings/setSystemSettings ---")
        resp = await send_req("request", "ssap://settings/setSystemSettings",
                             {"category": "picture", "settings": {"backlight": target_bl}})
        error = resp.get("error") or resp.get("payload", {}).get("errorText")
        if error:
            print(f"  ✗ Error: {error}")
        else:
            print(f"  ✓ Success: {json.dumps(resp.get('payload', {}))[:100]}")

        # Method 2: Luna-style URI via SSAP
        print(f"\n--- Method 2: ssap://com.webos.settingsservice/setSystemSettings ---")
        resp = await send_req("request", "ssap://com.webos.settingsservice/setSystemSettings",
                             {"category": "picture", "settings": {"backlight": target_bl}})
        error = resp.get("error") or resp.get("payload", {}).get("errorText")
        if error:
            print(f"  ✗ Error: {error}")
        else:
            print(f"  ✓ Success: {json.dumps(resp.get('payload', {}))[:100]}")

        # Method 3: Direct Luna service call
        print(f"\n--- Method 3: luna://com.webos.settingsservice/setSystemSettings ---")
        resp = await send_req("request", "luna://com.webos.settingsservice/setSystemSettings",
                             {"category": "picture", "settings": {"backlight": target_bl}})
        error = resp.get("error") or resp.get("payload", {}).get("errorText")
        if error:
            print(f"  ✗ Error: {error}")
        else:
            print(f"  ✓ Success: {json.dumps(resp.get('payload', {}))[:100]}")

        # Method 4: ssap://com.webos.service.settings/setSystemSettings
        print(f"\n--- Method 4: ssap://com.webos.service.settings/setSystemSettings ---")
        resp = await send_req("request", "ssap://com.webos.service.settings/setSystemSettings",
                             {"category": "picture", "settings": {"backlight": target_bl}})
        error = resp.get("error") or resp.get("payload", {}).get("errorText")
        if error:
            print(f"  ✗ Error: {error}")
        else:
            print(f"  ✓ Success: {json.dumps(resp.get('payload', {}))[:100]}")

        # Method 5: ssap://tv/setSystemSettings
        print(f"\n--- Method 5: ssap://tv/setSystemSettings ---")
        resp = await send_req("request", "ssap://tv/setSystemSettings",
                             {"category": "picture", "settings": {"backlight": target_bl}})
        error = resp.get("error") or resp.get("payload", {}).get("errorText")
        if error:
            print(f"  ✗ Error: {error}")
        else:
            print(f"  ✓ Success: {json.dumps(resp.get('payload', {}))[:100]}")

        # Verify backlight didn't change (if all methods failed)
        resp = await send_req("request", "ssap://settings/getSystemSettings",
                             {"category": "picture", "keys": ["backlight"]})
        final_bl = resp.get("payload", {}).get("settings", {}).get("backlight", "?")
        print(f"\nFinal backlight: {final_bl} (was {current_bl}, target was {target_bl})")

        if final_bl == target_bl:
            print("✓ Backlight changed successfully!")
        elif final_bl == current_bl:
            print("✗ Backlight unchanged — all write methods failed")
        else:
            print(f"? Backlight changed to unexpected value")

        # =====================================================================
        # TEST 3: Additional subscribable endpoints
        # =====================================================================
        print("\n" + "=" * 60)
        print("TEST 3: Probing additional undocumented endpoints")
        print("=" * 60)

        extra_endpoints = [
            ("ssap://audio/getStatus", {}),
            ("ssap://media/getActivePipeline", {}),
            ("ssap://com.webos.service.ime/getInputState", {}),
            ("ssap://tv/getCurrentChannel", {}),
            ("ssap://com.webos.service.networkinput/getPointerInputSocket", {}),
            ("ssap://system/getSystemInfo", {}),
            ("ssap://settings/getSystemSettings", {"category": "picture", "keys": ["energySaving", "eyeComfortMode"]}),
            ("ssap://settings/getSystemSettings", {"category": "other", "keys": ["autoOff"]}),
            ("ssap://settings/getSystemSettings", {"category": "general", "keys": ["screenOff"]}),
        ]

        for uri_str, payload in extra_endpoints:
            resp = await send_req("subscribe", uri_str, payload if payload else None)
            label = uri_str.split("/")[-1]
            if payload:
                label += f" ({json.dumps(payload)[:40]})"

            error = resp.get("error") or resp.get("payload", {}).get("errorText")
            subscribed = resp.get("payload", {}).get("subscribed", False)
            payload_str = json.dumps(resp.get("payload", {}))
            if len(payload_str) > 100:
                payload_str = payload_str[:100] + "..."

            if error:
                print(f"  ✗ {label}: {error}")
            elif subscribed:
                print(f"  ✓ {label}: SUBSCRIBED — {payload_str}")
            else:
                print(f"  ? {label}: {payload_str}")

        print("\nDone.")


if __name__ == "__main__":
    asyncio.run(main())

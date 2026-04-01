# LG WebOS SSAP Subscription Reference

Undocumented findings from probing the SSAP WebSocket API on LG OLED TVs.
Tested on OLED42C3PUA (2023, webOS 23) at firmware as of March 2026.

## Protocol

The SSAP (Simple Service Access Protocol) operates over WebSocket
(wss://port 3001 or ws://port 3000). After registering with a client
key, commands use `"type": "request"` and subscriptions use
`"type": "subscribe"` with the same URI.

Subscription updates reuse the original request's `"id"` field but do
**not** include a `"uri"` field. Consumers must track the mapping of
request ID → endpoint URI to identify which subscription an update
belongs to.

## Subscription support by endpoint

Tested by sending `"type": "subscribe"` and observing whether the
initial response contains `"subscribed": true` and whether subsequent
updates are pushed when the value changes on the TV.

### Confirmed subscribable

| Endpoint | Initial `subscribed` | Pushes updates? | Notes |
|----------|:---:|:---:|-------|
| `ssap://com.webos.service.tvpower/power/getPowerState` | Yes | Yes | Includes transitional states: `"processing": "Request Screen Saver"`, `"processing": "Screen On"` |
| `ssap://audio/getVolume` | **No** | **Yes** | Initial response lacks top-level `subscribed` flag but updates push reliably. Volume is nested in `volumeStatus.volume`. Confirmed with 28 updates in 10 seconds during remote volume changes. |
| `ssap://audio/getMute` | Yes | Yes | |
| `ssap://tv/getExternalInputList` | Yes | Yes | Full device list with connected status |
| `ssap://com.webos.applicationManager/getForegroundAppInfo` | Yes | Yes | Returns `appId`, `processId`; fires on input/app switch |
| `ssap://audio/getSoundOutput` | Yes | Yes | Returns `soundOutput` (e.g., `"tv_speaker"`, `"external_arc"`) |
| `ssap://settings/getSystemSettings` (picture/backlight) | Yes | **Yes** | **Undocumented.** Payload: `{"category": "picture", "keys": ["backlight"]}`. Pushes real-time updates with `caller` field indicating source (e.g., `"com.webos.app.quicksettings"` for TV quick settings changes). |
| `ssap://settings/getSystemSettings` (picture/multi-key) | Yes | Yes | Payload: `{"category": "picture", "keys": ["backlight", "brightness", "contrast", "color"]}`. Only changed keys appear in updates. |
| `ssap://settings/getSystemSettings` (sound/soundOutput) | Yes | Yes | Payload: `{"category": "sound", "keys": ["soundOutput"]}` |
| `ssap://settings/getSystemSettings` (option/country) | Yes | Unlikely | Payload: `{"category": "option", "keys": ["country"]}`. Subscribes but country rarely changes. |

### Not subscribable / errors

| Endpoint | Error | Notes |
|----------|-------|-------|
| `ssap://settings/getSystemSettings` (no keys) | 500: "keys are mandatory" | Category alone is not sufficient; must specify keys array |
| `ssap://com.webos.settingsservice/getSystemSettings` | 404: no such service | Luna service URIs don't work over SSAP |
| `ssap://com.webos.service.tv.display/getStatus` | 404: no such service | |
| `ssap://com.webos.service.config/getConfigs` | 404: no such service | |

## Subscription response format

### Initial response (on subscribe)

```json
{
  "type": "response",
  "id": "sub_8",
  "payload": {
    "subscribed": true,
    "category": "picture",
    "settings": {
      "backlight": 11
    },
    "returnValue": true
  }
}
```

### Subsequent update (pushed by TV)

```json
{
  "type": "response",
  "id": "sub_8",
  "payload": {
    "returnValue": true,
    "category": "picture",
    "caller": "com.webos.app.quicksettings",
    "settings": {
      "backlight": 14
    }
  }
}
```

Note: the `"caller"` field indicates which TV app triggered the change.
Known callers:
- `"com.webos.app.quicksettings"` — TV's quick settings menu
- (tisvlay's own changes via `createAlert` do not appear to trigger
  subscription updates, since they go through a different Luna path)

### Power state transitions

The power state subscription provides granular transition states:

```json
{"state": "Active", "processing": "Request Screen Saver"}
{"state": "Active", "processing": "Screen On"}
{"state": "Active"}
```

The `"processing"` field is transient and disappears when the
transition completes.

### Volume response quirk

The `ssap://audio/getVolume` initial response does NOT include a
top-level `"subscribed": true` flag. Instead, the volume data is nested
inside `"volumeStatus"`:

```json
{
  "type": "response",
  "id": "sub_3",
  "payload": {
    "returnValue": true,
    "volumeStatus": {
      "volumeLimitable": true,
      "activeStatus": true,
      "maxVolume": 100,
      "volume": 8,
      "muteStatus": false,
      "soundOutput": "tv_speaker"
    }
  }
}
```

Despite the missing `subscribed` flag, subscription updates push
reliably. Confirmed: 28 volume updates received in 10 seconds during
remote volume changes. The volume value is at `volumeStatus.volume`
(nested), not at the top level.

## Settings service categories and keys

The `ssap://settings/getSystemSettings` endpoint wraps the Luna
`com.webos.settingsservice` and accepts `category` + `keys[]` params.

### Known working categories

| Category | Keys tested | Subscribable? |
|----------|-------------|:---:|
| `picture` | `backlight`, `brightness`, `contrast`, `color` | Yes |
| `sound` | `soundOutput`, `soundOutputDigital` | Yes |
| `option` | `country`, `smartServiceCountryCode2` | Yes |

### Picture keys (from webOSTVjs source)

The `webOSTVjs` library references these picture-related settings:
- Via `tv.hw.displayType` config: `"OLED"` display type detection
- Via `tv.hw.panelResolution`: `"UD"` (4K) or `"8K"` resolution

These are accessed through `luna://com.webos.service.config/getConfigs`,
not through `settingsservice`, and are NOT available via SSAP subscribe.

## Backlight write methods tested

All direct write methods fail — the Luna `createAlert` workaround is
the only known way to set backlight programmatically.

| Method | Result |
|--------|--------|
| `ssap://settings/setSystemSettings` | 401 insufficient permissions |
| `ssap://com.webos.settingsservice/setSystemSettings` | 404 no such service |
| `luna://com.webos.settingsservice/setSystemSettings` | 404 no such service |
| `ssap://com.webos.service.settings/setSystemSettings` | 404 no such service |
| `ssap://tv/setSystemSettings` | 404 no such service |

The TV's firmware explicitly blocks direct picture settings writes even
with `WRITE_SETTINGS` permission in the manifest.

## Additional endpoint findings

| Endpoint | Result |
|----------|--------|
| `ssap://audio/getStatus` | Returns volume data (similar to getVolume) |
| `ssap://system/getSystemInfo` | **Subscribable.** Returns `serialNumber`, `modelName`, etc. |
| `ssap://com.webos.service.networkinput/getPointerInputSocket` | Returns WebSocket path for pointer/remote input |
| `ssap://media/getActivePipeline` | 404 |
| `ssap://com.webos.service.ime/getInputState` | 404 |
| `ssap://tv/getCurrentChannel` | 500 (no channel tuner on this model) |
| `ssap://settings/getSystemSettings` (picture/energySaving) | 500 |
| `ssap://settings/getSystemSettings` (other/autoOff) | 500 |
| `ssap://settings/getSystemSettings` (general/screenOff) | Returns wrong data (volume status — possible routing bug) |

## Probe methodology

Probe script: `scripts/probe_subscriptions.py`

Connects via wss://, registers with existing client key, sends
`"type": "subscribe"` to each endpoint, records initial response
(checking for `subscribed: true`), then listens for 60 seconds logging
all incoming updates with timestamps.

## References

- [aiowebostv](https://github.com/home-assistant-libs/aiowebostv) — Python async WebOS client (subscription reference implementation)
- [bscpylgtv](https://github.com/chros73/bscpylgtv) — Python WebOS client with Luna workarounds
- [LGTVCompanion](https://github.com/JPersson77/LGTVCompanion) — Windows companion app
- [webOS TV Developer — Luna Service Introduction](https://webostv.developer.lge.com/develop/references/luna-service-introduction)
- [webOS TV Developer — Settings Service](https://webostv.developer.lge.com/develop/references/settings-service)

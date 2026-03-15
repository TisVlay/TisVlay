# LG WebOS TV Control - Prior Art Survey

This document surveys existing open-source projects for controlling LG WebOS TVs over the network, analyzing their architectures, protocols, and features to inform the design of a Rust-based implementation.

## Executive Summary

All surveyed projects use the same underlying protocol: **LG WebOS SSAP (Simple Service Access Protocol)** over WebSocket connections. The TV exposes a WebSocket server that accepts JSON-formatted commands. Key findings:

- **Protocol**: WebSocket-based JSON RPC over ports 3000 (ws://) or 3001 (wss://)
- **Authentication**: Pairing prompt on TV, client receives persistent client key
- **Wake-on-LAN**: Required for powering on a TV in standby (network may be inactive)
- **TV Discovery**: SSDP (Simple Service Discovery Protocol) multicast on 239.255.255.250:1900

---

## Protocol Deep Dive

### Connection Flow

1. **Discovery** (optional): Send SSDP M-SEARCH to `239.255.255.250:1900` looking for `urn:schemas-upnp-org:device:MediaRenderer:1`
2. **Connect**: Open WebSocket to `ws://<ip>:3000` or `wss://<ip>:3001` (newer firmware requires SSL)
3. **Hello**: Send `{"type":"hello", "id":"hello", "payload":{}}` to get TV info
4. **Register**: Send registration payload with manifest and permissions; TV shows pairing prompt
5. **Receive Client Key**: After user accepts, receive `{"type":"registered", "payload":{"client-key":"..."}}`
6. **Send Commands**: Use the client key for subsequent sessions; send `{"type":"request", "uri":"ssap://...", "payload":{...}}`

### Registration Payload

The registration message includes a signed manifest requesting permissions:

```json
{
  "type": "register",
  "id": "register_0",
  "payload": {
    "forcePairing": false,
    "pairingType": "PROMPT",
    "manifest": {
      "permissions": [
        "LAUNCH", "CLOSE", "CONTROL_AUDIO", "CONTROL_DISPLAY",
        "CONTROL_INPUT_TV", "CONTROL_POWER", "READ_APP_STATUS",
        "READ_POWER_STATE", "READ_INSTALLED_APPS", ...
      ],
      "signatures": [{"signature": "<base64>", "signatureVersion": 1}]
    },
    "client-key": "<previously obtained key or null>"
  }
}
```

### Command Structure

```json
{
  "id": "<unique_id>",
  "type": "request",
  "uri": "ssap://<endpoint>",
  "payload": { ... }
}
```

### Key Endpoints (SSAP API)

| Category | Endpoint | Description |
|----------|----------|-------------|
| **Power** | `system/turnOff` | Power off (standby) |
| **Power** | `system/turnOn` | Power on (requires WoL first) |
| **Power** | `com.webos.service.tvpower/power/getPowerState` | Get power state |
| **Power** | `com.webos.service.tvpower/power/turnOffScreen` | Screen off (OLED saver) |
| **Power** | `com.webos.service.tvpower/power/turnOnScreen` | Screen on |
| **Input** | `tv/switchInput` | Switch HDMI input (`{"inputId":"HDMI_1"}`) |
| **Input** | `tv/getExternalInputList` | List available inputs |
| **Audio** | `audio/setVolume` | Set volume (`{"volume":50}`) |
| **Audio** | `audio/getVolume` | Get current volume |
| **Audio** | `audio/setMute` | Mute/unmute (`{"mute":true}`) |
| **Audio** | `audio/volumeUp` / `audio/volumeDown` | Volume step |
| **Apps** | `com.webos.applicationManager/launch` | Launch app |
| **Apps** | `system.launcher/close` | Close app |
| **Apps** | `com.webos.applicationManager/listApps` | List installed apps |
| **Info** | `system/getSystemInfo` | Get system information |
| **Info** | `com.webos.service.update/getCurrentSWInformation` | Software info |
| **Settings** | `settings/getSystemSettings` | Get settings by category |
| **Settings** | `com.webos.settingsservice/setSystemSettings` | Set settings (Luna API) |

### Button/Remote Control

A separate WebSocket connection for input commands is obtained via `com.webos.service.networkinput/getPointerInputSocket`. Button presses are sent as plain text:

```
type:button
name:VOLUMEUP

```

Valid buttons include: `LEFT`, `RIGHT`, `UP`, `DOWN`, `ENTER`, `BACK`, `HOME`, `EXIT`, `VOLUMEUP`, `VOLUMEDOWN`, `MUTE`, `PLAY`, `PAUSE`, `STOP`, `POWER`, etc.

---

## Project Analysis

### 1. LGTVCompanion (Windows)

**Repository**: `windows/LGTVCompanion`
**Language**: C++ (Windows-native)
**License**: MIT

#### Architecture

- **Service** (`LGTVsvc.exe`): Background Windows service monitoring power events
- **UI** (`LGTV Companion.exe`): Configuration GUI and command-line interface
- **Daemon** (`LGTVdaemon.exe`): User-mode process for idle detection

#### Key Features

- Automatic power management based on Windows power events (shutdown, suspend, resume)
- User idle detection (separate from Windows power settings)
- Monitor topology change detection (multi-monitor support)
- Extensive command-line interface with 100+ commands
- Service menu access (TPC/GSR disable for OLED burn-in protection bypass)
- Per-HDMI-input settings (PC mode, Game Optimizer, FreeSync)
- OLED-specific features: screen blanking, ambient lighting, curvature control

#### Wake-on-LAN Implementation

Uses Boost.Asio for networking. Supports multiple WoL methods:
1. Broadcast to 255.255.255.255
2. Send to specific IP address
3. Send to subnet broadcast address

#### Lessons Learned

- Windows service integration is complex; requires handling multiple power event types
- Need to detect restart vs. shutdown (different behaviors)
- DHCP leases should be static; IP changes break functionality
- TV may not respond to WoL during pixel refresh cycles

---

### 2. lgpowercontrol (Linux)

**Repository**: `linux/lgpowercontrol`
**Language**: Bash (wrapper around bscpylgtv)
**License**: MIT

#### Architecture

Simple shell scripts with systemd integration:
- `lgpowercontrol`: Main power control script (ON/OFF)
- `lgpowercontrol-boot.service`: Systemd service for boot
- `lgpowercontrol-shutdown.service`: Systemd service for shutdown
- `lgpowercontrol-dbus-events.sh`: D-Bus monitor for screen lock/unlock

#### D-Bus Integration

Monitors session D-Bus for screen lock events across desktop environments:
- **GNOME**: `org.gnome.ScreenSaver`
- **KDE**: `org.freedesktop.ScreenSaver`
- **Cinnamon**: `org.cinnamon.ScreenSaver`

```bash
dbus-monitor --session "interface='org.gnome.ScreenSaver'" |
    grep --line-buffered -E '^\s+boolean (true|false)' |
    while read -r line; do
        case "$line" in
        *"boolean true"*) power_off ;;
        *"boolean false"*) power_on ;;
        esac
    done
```

#### Lessons for Rust

- Use `zbus` crate for D-Bus integration
- Support multiple desktop environments
- Consider `logind` for system-wide sleep/wake events

---

### 3. bscpylgtv (Python)

**Repository**: `python/bscpylgtv`
**Language**: Python (asyncio)
**License**: MIT

#### Architecture

- Clean async/await API using `websockets` library
- SQLite-based key storage (compatible with aiopylgtv)
- Optional calibration functionality (numpy dependency)
- Extensive command-line tool `bscpylgtvcommand`

#### Key Implementation Details

**Client Creation**:
```python
client = await WebOsClient.create('192.168.1.18', ping_interval=None, states=[])
await client.connect()
```

**State Subscriptions**:
```python
states = ["system_info", "software_info", "power", "current_app",
          "muted", "volume", "apps", "inputs", "sound_output", "picture_settings"]
```

**WebSocket Configuration**:
- Ping interval for keepalive
- Handles both ws:// (port 3000) and wss:// (port 3001)
- Self-signed certificate handling (SSL verification disabled)

#### Calibration API

Advanced features for professional calibration:
- 1D/3D LUT uploads
- Tone mapping parameters
- Dolby Vision configuration
- Internal test pattern generator (iTPG)

**Warning**: Calibration API can brick TVs if misused.

#### Lessons for Rust

- Use `tokio-tungstenite` for async WebSocket
- Implement state subscription pattern
- Consider separate "lite" and "full" feature sets
- Store client keys persistently (SQLite or file-based)

---

### 4. LG_Buddy (Linux)

**Repository**: `linux/LG_Buddy`
**Language**: Bash (uses bscpylgtv)
**License**: MIT

#### Architecture

Shell scripts installed as systemd services:
- `LG_Buddy_Startup`: Runs at boot, powers on TV
- `LG_Buddy_Shutdown`: Runs at shutdown (via NetworkManager dispatcher)
- `LG_Buddy_sleep`: Handles sleep/wake (NetworkManager pre-down hook)

#### Network Timing Issues

Highlights an important race condition:
```bash
# Wait for network to be ready
nm-online -q -t 60

# TV network may be off in standby - check pingability
ping -c 1 $tv_ip && pingable=true || pingable=false
```

#### Shutdown Hook Placement

Uses NetworkManager dispatcher (`/etc/NetworkManager/dispatcher.d/pre-down.d/`) because:
1. Systemd services may run after network is down
2. `/lib/systemd/system-sleep/` scripts run with network already down

#### Lessons for Rust

- Network availability is not guaranteed at boot/shutdown
- Need timeout and retry logic
- Consider using NetworkManager D-Bus API for network state

---

### 5. LGWebOSRemote (Python)

**Repository**: `python/LGWebOSRemote`
**Language**: Python (threading with ws4py)
**License**: MIT

#### Architecture

- Uses older `ws4py` library (threaded, not async)
- JSON config file for TV storage
- Command-line focused design

#### TV Discovery (SSDP)

```python
request = b'M-SEARCH * HTTP/1.1\r\n' \
          b'HOST: 239.255.255.250:1900\r\n' \
          b'MAN: "ssdp:discover"\r\n' \
          b'MX: 2\r\n' \
          b'ST: urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n'

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.sendto(request, ('239.255.255.250', 1900))
response, address = sock.recvfrom(512)
# Parse USN for UUID, DLNADeviceName.lge.com for TV name
```

#### MAC Address Resolution

Uses `getmac` library for ARP-based MAC address discovery (required for WoL).

#### SSL Transition

As of January 2023, LG deprecated insecure WebSocket connections:
- Port 3000 (ws://) may be rejected on newer firmware
- Port 3001 (wss://) required with self-signed certificate handling

#### Lessons for Rust

- Implement SSDP discovery for automatic TV finding
- Handle both ws:// and wss:// with fallback
- Use `pnet` or similar for MAC address resolution

---

### 6. aiowebostv (Python)

**Repository**: `python/aiowebostv`
**Language**: Python (asyncio with aiohttp)
**License**: MIT (Home Assistant community)

#### Architecture

Modern async implementation designed for Home Assistant integration:
- Clean `WebOsClient` class with state management
- Callback-based state updates
- Proper connection lifecycle management

#### Key Implementation Patterns

**Connection with Fallback**:
```python
async def _create_main_ws(self):
    try:
        uri = f"ws://{self.host}:{WS_PORT}"
        return await self._ws_connect(uri)
    except (ClientConnectionError, WSServerHandshakeError):
        uri = f"wss://{self.host}:{WSS_PORT}"
        return await self._ws_connect(uri)
```

**State Model** (dataclass-based):
```python
@dataclass
class WebOsTvState:
    is_on: bool = False
    is_screen_on: bool = False
    current_app_id: str | None = None
    muted: bool | None = None
    volume: int | None = None
    apps: dict = field(default_factory=dict)
    inputs: dict = field(default_factory=dict)
    sound_output: str | None = None
    # ...
```

**Subscription Pattern**:
```python
async def subscribe_power_state(self, callback):
    return await self.subscribe(callback, ep.GET_POWER_STATE)
```

#### Configuration Constants

```python
WS_PORT = 3000
WSS_PORT = 3001
CONNECT_TIMEOUT = 2
RECEIVE_TIMEOUT = 10
REQUEST_TIMEOUT = 20
HEARTBEAT = 5
MAIN_WS_MAX_MSG_SIZE = 8 * 1024 * 1024  # 8MB for channel lists
```

#### Lessons for Rust

- Excellent reference implementation for async patterns
- State machine approach for connection management
- Subscription handlers with cleanup
- Volume step delay for external_arc sound output (ARC timing issues)

---

## Feature Comparison Matrix

| Feature | LGTVCompanion | lgpowercontrol | bscpylgtv | LG_Buddy | LGWebOSRemote | aiowebostv |
|---------|--------------|----------------|-----------|----------|---------------|------------|
| Power On/Off | Yes | Yes | Yes | Yes | Yes | Yes |
| Screen On/Off | Yes | No | Yes | No | Yes | Yes |
| HDMI Input | Yes | Yes | Yes | Yes | Yes | Yes |
| Volume Control | Yes | No | Yes | No | Yes | Yes |
| Mute | Yes | No | Yes | No | Yes | Yes |
| App Launch | Yes | No | Yes | No | Yes | Yes |
| Button Press | Yes | No | Yes | Yes | Yes | Yes |
| Picture Settings | Yes | No | Yes | No | Yes | No |
| Calibration | No | No | Yes | No | No | No |
| TV Discovery | Yes | No | No | No | Yes | No |
| System Integration | Windows | Linux | CLI | Linux | CLI | Library |
| Async | No | N/A | Yes | N/A | No | Yes |

---

## Recommendations for Rust Implementation

### Core Library Design

```
tisvlay/
├── src/
│   ├── lib.rs           # Public API
│   ├── client.rs        # WebOsClient implementation
│   ├── connection.rs    # WebSocket connection management
│   ├── protocol.rs      # SSAP protocol types and serialization
│   ├── endpoints.rs     # API endpoint constants
│   ├── discovery.rs     # SSDP TV discovery
│   ├── wol.rs           # Wake-on-LAN implementation
│   ├── error.rs         # Error types
│   └── state.rs         # TV state model
├── examples/
│   └── basic.rs
└── Cargo.toml
```

### Recommended Crates

| Purpose | Crate | Notes |
|---------|-------|-------|
| Async Runtime | `tokio` | Industry standard |
| WebSocket | `tokio-tungstenite` | Async WebSocket with TLS |
| JSON | `serde`, `serde_json` | Serialization |
| HTTP Client | `reqwest` | For any HTTP needs |
| D-Bus | `zbus` | Screen lock detection |
| SSDP | Custom or `ssdp-client` | TV discovery |
| MAC Address | `pnet` or `mac_address` | For WoL |
| TLS | `native-tls` or `rustls` | Self-signed cert handling |
| Config | `directories` + `serde` | Key storage |

### Feature Flags

```toml
[features]
default = ["cli"]
cli = ["clap"]
daemon = ["zbus", "systemd"]
discovery = []  # SSDP support
```

### Priority Features for Your Use Case

Based on your stated goals (C3 42" and G4 65" OLEDs):

1. **Power Management**: On/Off with WoL
2. **HDMI Input Switching**: Essential for PC use
3. **Screen Blanking**: OLED burn-in protection
4. **Volume Control**: Nice to have
5. **D-Bus Integration**: Lock/unlock screen triggers

### Known Quirks to Handle

1. **SSL Required**: Newer firmware requires wss:// on port 3001
2. **Pixel Refresh**: TV may ignore WoL during pixel refresh
3. **Network Standby**: TV network may be inactive in standby (requires "Always Ready" or "TV On via Wi-Fi")
4. **Timing**: Need delays between WoL and first command (~1-5 seconds)
5. **Connection State**: TV closes WebSocket on standby; need reconnection logic

---

## References

- [LG WebOS API Documentation](https://github.com/chros73/bscpylgtv/tree/master/docs) (bscpylgtv)
- [LGTVCompanion Command Line Docs](https://github.com/JPersson77/LGTVCompanion/blob/master/Docs/Commandline.md)
- [WebOS TV Developer Site](https://webostv.developer.lge.com/) (official but limited)
- [Home Assistant LG WebOS Integration](https://www.home-assistant.io/integrations/webostv/)

---

I've completed the survey of all six LG TV control projects and written a comprehensive reference document to reference.md. The IDE warnings are just spell-checker flags for technical acronyms (SSAP, SSDP, etc.) which are correct.

Summary of Key Findings
Protocol: All projects use the same LG WebOS SSAP protocol over WebSocket (port 3000 ws:// or 3001 wss://). The TV acts as a WebSocket server accepting JSON commands.

Core Flow:

Discover TV via SSDP multicast (optional)
Connect WebSocket, send hello/register messages
User accepts pairing prompt on TV, client receives persistent key
Send commands via {"type":"request", "uri":"ssap://...", "payload":{...}}
Linux-Specific Insights:

D-Bus monitoring for screen lock/unlock events (GNOME, KDE, Cinnamon)
NetworkManager dispatcher hooks for shutdown (network may be down in systemd services)
Wake-on-LAN required for power-on since TV network may be inactive in standby
Recommended Rust Crates:

tokio + tokio-tungstenite for async WebSocket
serde/serde_json for protocol serialization
zbus for D-Bus integration
pnet or mac_address for WoL MAC resolution
Priority Features for Your C3/G4 OLEDs:

Power on/off with WoL
HDMI input switching
Screen blanking (OLED burn-in protection)
D-Bus triggers for lock/unlock
The document includes detailed API endpoints, code examples from each project, a feature comparison matrix, and architectural recommendations for your Rust implementation.

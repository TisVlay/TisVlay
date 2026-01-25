# orlgtv Design Document

## Overview

**orlgtv** is a Rust utility for controlling LG WebOS TVs from a Linux desktop. It provides power management, HDMI input switching, screen blanking (OLED protection), and volume control via the LG WebOS SSAP protocol over WebSocket.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         User Space                                   │
│  ┌──────────┐   ┌──────────┐   ┌──────────┐                         │
│  │   CLI    │   │  Tray    │   │   GUI    │                         │
│  │ orlgtv   │   │  (ksni)  │   │ (Slint)  │                         │
│  └────┬─────┘   └────┬─────┘   └────┬─────┘                         │
│       │              │              │                                │
│       └──────────────┼──────────────┘                                │
│                      │                                               │
│                      ▼                                               │
│  ┌───────────────────────────────────────┐                          │
│  │         User Session Service          │                          │
│  │   (D-Bus session events: lock/unlock) │                          │
│  └───────────────────┬───────────────────┘                          │
│                      │ D-Bus System Bus                              │
└──────────────────────┼───────────────────────────────────────────────┘
                       │
┌──────────────────────┼───────────────────────────────────────────────┐
│                      ▼                     System Space              │
│  ┌───────────────────────────────────────┐                          │
│  │         orlgtvd (System Daemon)       │                          │
│  │  - WebSocket connections to TVs       │                          │
│  │  - Wake-on-LAN at boot                │                          │
│  │  - Responds to user service requests  │                          │
│  └───────────────────┬───────────────────┘                          │
│                      │                                               │
│                      ▼                                               │
│              ┌───────────────┐                                       │
│              │   LG TV(s)    │                                       │
│              │  WebSocket    │                                       │
│              │  3000/3001    │                                       │
│              └───────────────┘                                       │
└──────────────────────────────────────────────────────────────────────┘
```

### Components

| Component | Binary | Description |
|-----------|--------|-------------|
| **System Daemon** | `orlgtvd` | Systemd system service. Runs at boot, manages TV connections, executes commands. Owns WebSocket connections and client keys. |
| **User Service** | `orlgtv-session` | Systemd user service. Monitors D-Bus session events (screen lock/unlock, idle). Relays events to system daemon. |
| **CLI** | `orlgtv` | Command-line interface for manual control. Communicates with daemon via D-Bus. |
| **Tray App** | `orlgtv-tray` | System tray icon using `ksni`. Quick access menu for common actions. |
| **GUI** | `orlgtv-gui` | Slint-based configuration window for TV setup, pairing, and settings. |

---

## Inter-Process Communication

### D-Bus System Bus

All user-facing components communicate with `orlgtvd` via D-Bus system bus.

**Service Name**: `org.orlgtv.Daemon`

**Object Path**: `/org/orlgtv/Daemon`

**Interface**: `org.orlgtv.Daemon1`

```xml
<interface name="org.orlgtv.Daemon1">
  <!-- TV Discovery (requires firewall config for SSDP - see docs/firewall.md) -->
  <method name="DiscoverTvs">
    <arg name="timeout_ms" type="u" direction="in"/>
    <arg name="tvs" type="a(ssss)" direction="out"/> <!-- [(id, name, ip, mac)] -->
  </method>

  <!-- TV Management -->
  <method name="PairTv">
    <arg name="ip" type="s" direction="in"/>
    <arg name="name" type="s" direction="in"/>
    <arg name="tv_id" type="s" direction="out"/>
  </method>

  <method name="RemoveTv">
    <arg name="tv_id" type="s" direction="in"/>
  </method>

  <method name="ListTvs">
    <arg name="tvs" type="a(sssb)" direction="out"/> <!-- [(id, name, ip, connected)] -->
  </method>

  <!-- Power Control -->
  <method name="PowerOn">
    <arg name="tv_id" type="s" direction="in"/>
  </method>

  <method name="PowerOff">
    <arg name="tv_id" type="s" direction="in"/>
  </method>

  <method name="ScreenOn">
    <arg name="tv_id" type="s" direction="in"/>
  </method>

  <method name="ScreenOff">
    <arg name="tv_id" type="s" direction="in"/>
  </method>

  <!-- Input Control -->
  <method name="SwitchInput">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="input_id" type="s" direction="in"/> <!-- e.g. "HDMI_1" -->
  </method>

  <method name="GetInputs">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="inputs" type="a(ss)" direction="out"/> <!-- [(id, label)] -->
  </method>

  <!-- Audio Control -->
  <method name="SetVolume">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="volume" type="u" direction="in"/>
  </method>

  <method name="GetVolume">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="volume" type="u" direction="out"/>
  </method>

  <method name="SetMute">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="mute" type="b" direction="in"/>
  </method>

  <!-- Picture Control -->
  <method name="SetBacklight">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="backlight" type="u" direction="in"/> <!-- 0-100, controls OLED pixel brightness -->
  </method>

  <method name="GetBacklight">
    <arg name="tv_id" type="s" direction="in"/>
    <arg name="backlight" type="u" direction="out"/>
  </method>

  <!-- Signals -->
  <signal name="TvStateChanged">
    <arg name="tv_id" type="s"/>
    <arg name="state" type="s"/> <!-- JSON blob -->
  </signal>
</interface>
```

### PolicyKit Authorization

For privileged operations (pairing, removing TVs), PolicyKit rules control access.

**Policy File**: `/usr/share/polkit-1/actions/org.orlgtv.policy`

```xml
<policyconfig>
  <action id="org.orlgtv.pair-tv">
    <description>Pair a new LG TV</description>
    <message>Authentication is required to pair a new TV</message>
    <defaults>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>

  <action id="org.orlgtv.remove-tv">
    <description>Remove a paired TV</description>
    <message>Authentication is required to remove a TV</message>
    <defaults>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
  </action>

  <!-- Basic control allowed for any active user -->
  <action id="org.orlgtv.control">
    <description>Control TV power, input, volume</description>
    <defaults>
      <allow_active>yes</allow_active>
    </defaults>
  </action>
</policyconfig>
```

---

## Configuration

### System Configuration

**Path**: `/etc/orlgtv/config.toml`

```toml
# System-wide orlgtv configuration

[daemon]
# Boot behavior
wake_on_boot = true
wake_delay_ms = 3000        # Delay after WoL before sending commands

# Connection settings
websocket_port = 3001       # 3000 for ws://, 3001 for wss://
use_tls = true              # Use wss:// (required for newer firmware)
connect_timeout_ms = 5000
reconnect_interval_ms = 10000

[logging]
level = "info"              # trace, debug, info, warn, error

# TV definitions (managed by daemon, edited via GUI/CLI)
[[tv]]
id = "living-room"
name = "Living Room G4"
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123..."    # Obtained during pairing
default_input = "HDMI_1"    # Input to switch to on wake

[[tv]]
id = "office-left"
name = "Office C3 Left"
ip = "192.168.1.101"
mac = "AA:BB:CC:DD:EE:01"
client_key = "def456..."
default_input = "HDMI_2"
```

### User Configuration

**Path**: `~/.config/orlgtv/config.toml`

User-specific preferences that supplement system config.

```toml
[session]
# Screen lock/unlock
on_screen_lock = "screen_off"     # screen_off | power_off | none
on_screen_unlock = "screen_on"    # screen_on | power_on | none

# Greeter wake: unblank TV when user interacts with lock screen (Wayland only)
on_greeter_wake = "screen_on"     # screen_on | power_on | none

# Suspend/resume
on_sleep = "power_off"            # suspend/hibernate
on_wake = "power_on"              # resume

# Idle detection
idle_timeout_seconds = 300        # 0 to disable
on_idle_timeout = "screen_off"    # Action when idle

[tray]
show_volume_slider = true
show_backlight_slider = true
show_input_switcher = true
```

---

## Power & Screen State Machine

```
                    ┌─────────────────────────────────────┐
                    │                                     │
                    ▼                                     │
              ┌──────────┐                                │
    WoL ───▶  │    ON    │ ◀─── PowerOn                   │
              └────┬─────┘                                │
                   │                                      │
         ScreenOff │ ScreenOn                             │
                   ▼                                      │
              ┌──────────┐                                │
              │ BLANKED  │  (OLED-safe, instant resume)   │
              └────┬─────┘                                │
                   │                                      │
           PowerOff│                                      │
                   ▼                                      │
              ┌──────────┐                                │
              │ STANDBY  │ ───────────────────────────────┘
              └──────────┘
                   │
           (Network may be inactive,
            requires WoL to wake)
```

### Event → Action Mapping

| Event | Default Action | Notes |
|-------|---------------|-------|
| System boot | WoL → wait → switch input | Get TV ready for login screen |
| Screen lock | Screen off (blank) | Instant resume on unlock |
| Greeter wake | Screen on | User interacted with lock screen (Wayland only) |
| Screen unlock | Screen on | Resume from blank |
| User idle timeout | Screen off | Configurable |
| System suspend | Power off | Network unavailable during sleep |
| System resume | WoL → wait → power on | Restore to active state |
| System shutdown | Power off | Clean shutdown |

---

## Crate Structure

```
orlgtv/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── orlgtv-core/              # Core library (shared logic)
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── client.rs         # WebOsClient - WebSocket connection
│   │   │   ├── protocol.rs       # SSAP message types
│   │   │   ├── endpoints.rs      # API endpoint constants
│   │   │   ├── wol.rs            # Wake-on-LAN
│   │   │   ├── discovery.rs      # SSDP TV discovery
│   │   │   ├── config.rs         # Config parsing
│   │   │   ├── error.rs          # Error types
│   │   │   └── state.rs          # TvState model
│   │   └── Cargo.toml
│   │
│   ├── orlgtvd/                  # System daemon
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── dbus.rs           # D-Bus service implementation
│   │   │   ├── tv_manager.rs     # Manages TV connections
│   │   │   └── events.rs         # System event handlers
│   │   └── Cargo.toml
│   │
│   ├── orlgtv-session/           # User session service
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── session_monitor.rs # Screen lock/suspend detection
│   │   │   ├── wayland_idle.rs   # Wayland greeter wake detection
│   │   │   ├── idle.rs           # Idle timeout polling
│   │   │   └── daemon_client.rs  # Talks to orlgtvd
│   │   └── Cargo.toml
│   │
│   ├── orlgtv-cli/               # CLI tool
│   │   ├── src/
│   │   │   └── main.rs
│   │   └── Cargo.toml
│   │
│   ├── orlgtv-tray/              # System tray (ksni)
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   └── menu.rs
│   │   └── Cargo.toml
│   │
│   └── orlgtv-gui/               # GUI app (Slint)
│       ├── src/
│       │   └── main.rs
│       ├── ui/
│       │   └── main.slint
│       └── Cargo.toml
│
├── systemd/
│   ├── orlgtvd.service           # System service unit
│   └── orlgtv-session.service    # User service unit
│
├── polkit/
│   └── org.orlgtv.policy
│
└── docs/
    ├── reference.md
    └── design.md
```

---

## Dependencies

### Core Library (`orlgtv-core`) - IMPLEMENTED

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "net", "time", "sync", "macros", "signal"] }
tokio-tungstenite = { version = "0.24", features = ["rustls-tls-webpki-roots"] }
rustls = "0.23"                    # Changed from native-tls to avoid OpenSSL dependency
webpki-roots = "0.26"
futures-util = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
thiserror = "1"
tracing = "0.1"
socket2 = "0.5"                    # Wake-on-LAN
directories = "5"                  # XDG paths

[features]
default = []
discovery = ["ssdp-client", ...]   # SSDP TV discovery (see docs/firewall.md)
```

**Implementation Note**: Uses custom `InsecureCertVerifier` in `connection.rs` to accept
LG TV's self-signed certificates with rustls.

### System Daemon (`orlgtvd`) - IMPLEMENTED

```toml
[dependencies]
orlgtv-core = { path = "../orlgtv-core" }
tokio = { version = "1", features = ["rt-multi-thread", "signal"] }
zbus = { version = "5", default-features = false, features = ["tokio"] }
tracing-subscriber = "0.3"
```

### User Session Service (`orlgtv-session`) - COMPLETE

```toml
[dependencies]
orlgtv-core = { path = "../orlgtv-core" }
tokio = { version = "1", features = ["rt-multi-thread"] }
zbus = { version = "5", default-features = false, features = ["tokio"] }
tracing-subscriber = "0.3"

# Wayland greeter wake detection
wayland-client = "0.31"
wayland-protocols = { version = "0.32", features = ["client", "staging"] }
```

### CLI (`orlgtv-cli`) - IMPLEMENTED

```toml
[dependencies]
orlgtv-core = { path = "../orlgtv-core" }
tokio = { version = "1", features = ["rt-multi-thread"] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

**Note**: CLI currently connects directly to TVs. D-Bus integration is optional
for future work (see `docs/phase2-remaining.md`).

### Tray (`orlgtv-tray`) - Phase 4

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread"] }
zbus = { version = "5", default-features = false, features = ["tokio"] }
ksni = "0.2"
```

### GUI (`orlgtv-gui`) - Phase 5

```toml
[dependencies]
tokio = { version = "1", features = ["rt-multi-thread"] }
zbus = { version = "5", default-features = false, features = ["tokio"] }
slint = "1"
```

---

## CLI Interface

```
orlgtv - LG WebOS TV Control

USAGE:
    orlgtv [OPTIONS] <COMMAND>

COMMANDS:
    discover        Scan for LG TVs on the network
    pair            Pair a new TV
    list            List configured TVs
    remove          Remove a TV from configuration

    power           Power control (on/off)
    screen          Screen control (blank/unblank)
    input           Switch HDMI input
    volume          Volume control
    mute            Mute/unmute
    backlight       Backlight control

    status          Show TV status

OPTIONS:
    -t, --tv <ID>   Target TV (default: first configured)
    -v, --verbose   Verbose output
    -h, --help      Print help
    -V, --version   Print version

EXAMPLES:
    orlgtv discover
    orlgtv pair 192.168.1.100 --name "Living Room"
    orlgtv power on
    orlgtv --tv office-left input HDMI_2
    orlgtv volume 30
    orlgtv backlight 80
    orlgtv screen off
```

---

## System Tray Menu Structure

```
┌─────────────────────────────┐
│ 📺 orlgtv                    │
├─────────────────────────────┤
│ ▶ Living Room G4      [🟢] │
│   ├─ Power On/Off           │
│   ├─ Screen On/Off          │
│   ├─ Input ▶                │
│   │    ├─ HDMI 1            │
│   │    ├─ HDMI 2 ✓          │
│   │    └─ HDMI 3            │
│   ├─ Volume [====----] 60   │
│   └─ Backlight [======--] 80│
├─────────────────────────────┤
│ ▶ Office C3 Left      [🔴] │
│   └─ Wake Up                │
├─────────────────────────────┤
│ ⚙ Settings...               │
│ ❓ About                     │
│ ⏻ Quit                      │
└─────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: Core Library + CLI (MVP) - COMPLETE

**Goal**: Basic TV control from command line.

**Implemented**:
1. `orlgtv-core`: WebSocket client with rustls TLS, SSAP protocol, WoL, config parsing
2. `orlgtv-cli`: Power on/off, screen blank/unblank, input switch, volume, backlight, status
3. TV pairing flow with client key persistence
4. Multi-TV configuration support (named profiles)
5. XDG-compliant config storage (`~/.config/orlgtv/config.toml`)

**Deviations from Plan**:
- Used `rustls` instead of `native-tls` to avoid OpenSSL system dependency
- Added backlight control to MVP (originally planned for later)
- CLI connects directly to TVs; D-Bus integration deferred to Phase 2

### Phase 2: System Daemon - COMPLETE

**Goal**: Background service with D-Bus API.

**Implemented** (see `crates/orlgtvd/`):
1. `orlgtvd`: System service with D-Bus interface (`org.orlgtv.Daemon1`)
2. TV connection management with automatic reconnection (exponential backoff)
3. Boot-time WoL for configured TVs
4. TV pairing/removal via D-Bus with config persistence
5. TvStateChanged signal emission on connect/disconnect
6. Config reload via SIGHUP or D-Bus ReloadConfig

### Phase 3: User Session Integration - COMPLETE

**Goal**: React to desktop events.

**Implemented** (see `crates/orlgtv-session/`):
1. `orlgtv-session`: User service monitoring D-Bus session events
2. Screen lock/unlock detection via `org.freedesktop.ScreenSaver.ActiveChanged`
3. Suspend/resume detection via `org.freedesktop.login1.Manager.PrepareForSleep`
4. Greeter wake detection via Wayland `ext-idle-notify-v1` protocol
5. Idle timeout detection via polling `GetSessionIdleTime`
6. Configurable actions per event type (`SessionAction` enum)
7. SIGHUP for config reload
8. Systemd user service file (`systemd/orlgtv-session.service`)

**Greeter Wake Detection**: When the screen is locked and the user moves the mouse or presses a key, the TV unblanks so the user can see the password prompt. This uses the Wayland `ext-idle-notify-v1` protocol, supported on KDE Plasma 6+, Sway, Hyprland, GNOME, and other modern compositors. Falls back gracefully on X11 or when protocol unavailable.

**Session Configuration** (in `~/.config/orlgtv/config.toml`):
```toml
[session]
on_screen_lock = "screen_off"    # screen_off | power_off | none
on_screen_unlock = "screen_on"   # screen_on | power_on | none
on_greeter_wake = "screen_on"    # unblank TV when interacting with lock screen (Wayland only)
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 300       # 0 to disable
on_idle_timeout = "screen_off"
```

### Phase 4: Tray App

**Goal**: Quick access from system tray.

1. `orlgtv-tray`: ksni-based StatusNotifierItem
2. Power/screen/input controls
3. Volume slider
4. TV status indicators

### Phase 5: GUI Configuration

**Goal**: User-friendly setup and configuration.

1. `orlgtv-gui`: Slint-based settings window
2. TV discovery and pairing wizard
3. Per-TV configuration
4. Session behavior settings

---

## Known Challenges & Mitigations

| Challenge | Mitigation |
|-----------|------------|
| TV ignores WoL during pixel refresh | Retry WoL with exponential backoff; inform user if TV unresponsive |
| Newer firmware requires TLS (wss://) | Default to port 3001; fallback to 3000; accept self-signed certs |
| Network down during shutdown | Use NetworkManager pre-down dispatcher hook |
| WebSocket closed on TV standby | Automatic reconnection with backoff |
| DHCP IP changes | Use MAC address for WoL; re-discover via SSDP |
| Multi-user shared TV access | System-wide config; PolicyKit for privilege control |
| Wayland protocol version mismatch | Explicit version binding; cap at library-supported version |
| Greeter wake unavailable on X11 | Feature requires Wayland; X11 users won't see TV unblank on lock screen interaction |

---

## Testing Strategy

1. **Unit tests**: Protocol serialization, config parsing, state machine
2. **Integration tests**: Mock WebSocket server simulating TV responses
3. **Manual testing**: Real hardware (C3 42", G4 65")
4. **CI**: Build and unit tests on GitHub Actions

---

## References

- [Protocol Details](reference.md) - SSAP protocol documentation
- [LG WebOS API](https://github.com/chros73/bscpylgtv/tree/master/docs)
- [zbus Documentation](https://docs.rs/zbus)
- [ksni Documentation](https://docs.rs/ksni)
- [Slint Documentation](https://docs.slint.dev)

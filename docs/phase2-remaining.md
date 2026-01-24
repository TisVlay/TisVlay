# Phase 2 Remaining Tasks & Future Work

## Current Status

Phase 2 System Daemon is **complete**. The following items are implemented and working:

- `orlgtvd` daemon with D-Bus interface (`org.orlgtv.Daemon1`)
- All TV control methods: ListTvs, PairTv, RemoveTv, PowerOn/Off, ScreenOn/Off, SwitchInput, GetInputs, SetVolume, GetVolume, SetMute, SetBacklight, GetBacklight, GetState, ReloadConfig
- TvStateChanged D-Bus signal with emission on connect/disconnect events
- TV connection manager with connection pooling
- Automatic reconnection with exponential backoff (1s → 2s → 4s → max 10s)
- Boot-time Wake-on-LAN
- Config reload via SIGHUP or D-Bus ReloadConfig
- Systemd service files with `systemctl reload` support

---

## Remaining Phase 2 Tasks

### 1. CLI Migration to D-Bus (Optional)

**Current state**: CLI (`orlgtv-cli`) connects directly to TVs via WebSocket.

**Goal**: CLI should communicate with `orlgtvd` via D-Bus, with fallback to direct connection.

#### Implementation Steps

1. **Add zbus dependency to orlgtv-cli/Cargo.toml**:
   ```toml
   [dependencies]
   zbus = { workspace = true }
   ```

2. **Create D-Bus client module** (`crates/orlgtv-cli/src/dbus_client.rs`):
   ```rust
   use zbus::Connection;

   pub struct DaemonClient {
       connection: Connection,
   }

   impl DaemonClient {
       pub async fn connect() -> Result<Self, zbus::Error> {
           // Try session bus first (development), then system bus
           let connection = match Connection::session().await {
               Ok(conn) => conn,
               Err(_) => Connection::system().await?,
           };
           Ok(Self { connection })
       }

       pub async fn list_tvs(&self) -> Result<Vec<(String, String, String, bool)>, zbus::Error> {
           // Call org.orlgtv.Daemon1.ListTvs
       }

       pub async fn power_on(&self, tv_id: &str) -> Result<(), zbus::Error> { ... }
       pub async fn power_off(&self, tv_id: &str) -> Result<(), zbus::Error> { ... }
       pub async fn screen_on(&self, tv_id: &str) -> Result<(), zbus::Error> { ... }
       pub async fn screen_off(&self, tv_id: &str) -> Result<(), zbus::Error> { ... }
       // ... etc for all methods
   }
   ```

3. **Add --direct flag to CLI**:
   ```rust
   #[derive(Parser)]
   struct Cli {
       /// Connect directly to TV instead of using daemon
       #[arg(long)]
       direct: bool,
       // ...
   }
   ```

4. **Update main.rs command handling**:
   ```rust
   async fn main() -> Result<()> {
       let cli = Cli::parse();

       if cli.direct {
           // Current behavior: direct WebSocket connection
           run_direct_command(cli).await
       } else {
           // New behavior: use D-Bus daemon
           match DaemonClient::connect().await {
               Ok(client) => run_dbus_command(cli, client).await,
               Err(e) => {
                   eprintln!("Daemon not available ({}), falling back to direct connection", e);
                   run_direct_command(cli).await
               }
           }
       }
   }
   ```

5. **Benefits**:
   - Centralized connection management (one connection per TV instead of per-command)
   - Automatic reconnection handled by daemon
   - Config changes handled by daemon
   - Future: PolicyKit authorization for privileged operations

---

### 2. SSDP Discovery (Future)

**Goal**: Automatically discover LG TVs on the local network.

#### Implementation Steps

1. **Add ssdp-client dependency** (optional feature):
   ```toml
   # In orlgtv-core/Cargo.toml
   [dependencies]
   ssdp-client = { version = "0.2", optional = true }

   [features]
   discovery = ["ssdp-client"]
   ```

2. **Create discovery module** (`crates/orlgtv-core/src/discovery.rs`):
   ```rust
   use ssdp_client::SearchTarget;

   pub struct DiscoveredTv {
       pub name: String,
       pub ip: String,
       pub mac: Option<String>,
       pub model: Option<String>,
   }

   pub async fn discover_tvs(timeout_ms: u64) -> Result<Vec<DiscoveredTv>, Error> {
       // Search for LG WebOS TVs using SSDP
       // Target: "urn:lge-com:service:webostv:1"
       // Parse response headers for TV info
   }
   ```

3. **Add D-Bus method to orlgtvd**:
   ```rust
   async fn discover_tvs(&self, timeout_ms: u32) -> Result<Vec<(String, String, String, String)>, DaemonError> {
       // Returns: [(name, ip, mac, model)]
   }
   ```

4. **Add CLI command**:
   ```bash
   orlgtv discover [--timeout 5000]
   ```

---

### 3. PolicyKit Authorization (Future)

**Goal**: Require authentication for privileged operations (pairing, removing TVs).

#### Implementation Steps

1. **Create PolicyKit policy file** (`polkit/org.orlgtv.policy`):
   ```xml
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE policyconfig PUBLIC "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
     "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
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

     <action id="org.orlgtv.control">
       <description>Control TV power, input, volume</description>
       <defaults>
         <allow_active>yes</allow_active>
       </defaults>
     </action>
   </policyconfig>
   ```

2. **Add PolicyKit check in D-Bus methods**:
   ```rust
   use zbus::fdo::Properties;

   async fn pair_tv(&self, ip: &str, name: &str, mac: &str, #[zbus(header)] header: Header<'_>) -> Result<String, DaemonError> {
       // Check PolicyKit authorization
       let sender = header.sender().ok_or(DaemonError::PermissionDenied)?;
       self.check_authorization(sender, "org.orlgtv.pair-tv").await?;

       // Proceed with pairing...
   }
   ```

3. **Install policy file**:
   ```bash
   sudo cp polkit/org.orlgtv.policy /usr/share/polkit-1/actions/
   ```

---

## Phase 3: User Session Service (`orlgtv-session`)

**Goal**: React to desktop events (screen lock/unlock, idle, suspend/resume).

### Implementation Steps

1. **Create new crate** `crates/orlgtv-session/`:
   ```
   orlgtv-session/
   ├── Cargo.toml
   └── src/
       ├── main.rs
       ├── session_monitor.rs  # D-Bus session event listener
       └── dbus_client.rs      # Talks to orlgtvd
   ```

2. **Monitor D-Bus session events**:
   - `org.freedesktop.ScreenSaver.ActiveChanged` - screen lock/unlock
   - `org.freedesktop.login1.Session.Lock`/`Unlock` - systemd-logind
   - `org.freedesktop.PowerManagement.Inhibit` - idle detection
   - `org.freedesktop.login1.Manager.PrepareForSleep` - suspend/resume

3. **User configuration** (`~/.config/orlgtv/config.toml`):
   ```toml
   [session]
   on_screen_lock = "screen_off"     # screen_off | power_off | none
   on_screen_unlock = "screen_on"    # screen_on | power_on | none
   on_idle_timeout = "screen_off"    # Action when idle
   on_sleep = "power_off"            # suspend/hibernate
   on_wake = "power_on"              # resume
   idle_timeout_seconds = 300        # 0 to disable
   ```

4. **Systemd user service** (`systemd/orlgtv-session.service`):
   ```ini
   [Unit]
   Description=orlgtv Session Service
   After=graphical-session.target
   PartOf=graphical-session.target

   [Service]
   Type=simple
   ExecStart=/usr/bin/orlgtv-session
   Restart=on-failure

   [Install]
   WantedBy=graphical-session.target
   ```

---

## Phase 4: System Tray (`orlgtv-tray`)

**Goal**: StatusNotifierItem tray icon for quick TV control.

### Implementation Steps

1. **Add ksni dependency**:
   ```toml
   [dependencies]
   ksni = "0.2"
   ```

2. **Create tray application**:
   - Icon showing connection status
   - Menu with TV list
   - Power on/off per TV
   - Screen on/off per TV
   - Input switcher submenu
   - Volume slider
   - Backlight slider
   - Settings... (launches GUI)

3. **D-Bus integration**:
   - Subscribe to `TvStateChanged` signal for live updates
   - Call daemon methods for actions

---

## Phase 5: GUI Configuration (`orlgtv-gui`)

**Goal**: Slint-based configuration UI for TV setup.

### Features

1. **TV Discovery wizard**:
   - SSDP scan
   - Manual IP entry
   - Pairing flow with TV prompt

2. **TV Configuration**:
   - Name, MAC address
   - Default input
   - Wake-on-boot option

3. **Session behavior settings**:
   - Lock/unlock actions
   - Idle timeout
   - Suspend/resume actions

4. **Live TV status**:
   - Connection status
   - Current input
   - Volume level

---

## File Locations Reference

```
orlgtv/
├── crates/
│   ├── orlgtv-core/           # Core library (COMPLETE)
│   ├── orlgtv-cli/            # CLI tool (COMPLETE, D-Bus migration pending)
│   ├── orlgtvd/               # System daemon (COMPLETE)
│   ├── orlgtv-session/        # Phase 3 (TODO)
│   ├── orlgtv-tray/           # Phase 4 (TODO)
│   └── orlgtv-gui/            # Phase 5 (TODO)
├── systemd/
│   ├── orlgtvd.service        # System service (COMPLETE)
│   ├── org.orlgtv.Daemon.service  # D-Bus activation (COMPLETE)
│   ├── org.orlgtv.Daemon.conf     # D-Bus policy (COMPLETE)
│   └── orlgtv-session.service     # User service (Phase 3)
├── polkit/
│   └── org.orlgtv.policy      # PolicyKit rules (TODO)
└── docs/
    ├── design.md              # Architecture design
    ├── reference.md           # Protocol reference
    └── phase2-remaining.md    # This file
```

---

## Testing Commands

```bash
# Start daemon in development mode
orlgtvd --session

# List TVs
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ListTvs

# Power on TV
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 PowerOn s "office"

# Screen off
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ScreenOff s "office"

# Reload config
busctl --user call org.orlgtv.Daemon /org/orlgtv/Daemon org.orlgtv.Daemon1 ReloadConfig

# Introspect interface
busctl --user introspect org.orlgtv.Daemon /org/orlgtv/Daemon

# Monitor signals
busctl --user monitor org.orlgtv.Daemon
```

---

## Priority Order for Remaining Work

1. **High Priority** (Improved usability):
   - CLI D-Bus migration (optional, improves UX)
   - Phase 3: Session service (desktop integration)

2. **Medium Priority** (Enhanced features):
   - SSDP discovery (automatic TV discovery)

3. **Lower Priority** (Nice to have):
   - Phase 4: Tray app
   - Phase 5: GUI
   - PolicyKit authorization

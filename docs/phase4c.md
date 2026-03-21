# Phase 4C: System vs User Config Split

## Motivation

The current architecture uses a single config file (`Config` struct) containing everything:
daemon settings, TV definitions with vendor credentials, profile definitions, and per-TV
profile assignments. Both `tisvlayd` and `tisvlay-session` read from this file.

This creates several problems:

1. **Credential exposure**: The config file contains TV credentials (client keys, API tokens)
   that should be root-readable only, but the user session service needs to read profile
   settings from the same file.

2. **No per-user customization**: Different users on the same machine may want different
   idle timeouts, screen lock behaviors, or per-TV profile assignments. A data scientist
   reading documentation wants a long idle timeout; a gamer watching movies wants a short
   one. KDE Plasma and GNOME both allow per-user power/display settings for this reason.

3. **Ambiguous ownership**: The daemon writes to the config (pairing saves credentials),
   the session service reads from it, and any future GUI would need to modify it. With a
   single file, there's no clear owner.

4. **Session lifecycle gap**: When a user logs out, `tisvlay-session` dies with the session.
   Between logout and login (or during shutdown initiated from the login greeter), only
   `tisvlayd` is alive. The daemon needs its own power policy for these gaps, separate
   from user profile settings.

## Design Decisions

This section documents the rationale behind key decisions, developed through iterative
discussion of the architecture.

### Why split the config?

Profiles define personal preferences (idle timeout, lock behavior) that vary per user.
System-level settings (TV definitions, credentials, boot behavior) are machine-wide.
The daemon doesn't use profiles; the session service doesn't need credentials. A clean
split aligns ownership with responsibility.

### Why does the daemon need session awareness?

When all user sessions end (logout), the daemon is the only service running. It must
handle system lifecycle events (shutdown, reboot) and decide what to do with TVs when
no user is active. The login greeter (KDE `plasma-login-manager`) runs KWin with its own
DPMS for physical displays, but knows nothing about network-controlled TVs.

### Why inherit profiles from the system default?

When a user has no config file yet, their session service needs *some* profile to apply.
Rather than hard-coding defaults in the binary (which change between releases and confuse
admins), the session service inherits the daemon's `[profiles.default]` section. This
gives admins a single knob for system-wide defaults without requiring every user to
create a config file. Users override by creating their own `~/.config/tisvlay/config.toml`.

For fields that a user doesn't specify in their own profiles, those fields inherit from
the system's default profile rather than falling back to hard-coded defaults. This avoids
confusion where a seemingly rogue idle timeout differs from what the admin configured.
We can assume all components (daemon, session, CLI, tray, GUI) are pinned to the same
version, so config merging remains self-consistent across the stack.

### Why option (c) for session switching?

When a user's session is switched away (VT switch to greeter or another user), `logind`
sets `Session.Active = false`. Three options were considered:

- (a) Session service treats it like screen lock (`on_screen_lock`)
- (b) New `on_session_deactivated` / `on_session_activated` action pair
- (c) Session service does nothing; daemon handles via `on_no_active_session`

Option (c) was chosen because it avoids the session service and daemon fighting over TV
state. The daemon observes "no user session is active on this seat" and applies system
policy. When a session becomes active again, the session service detects `Active = true`
and applies its profile. This keeps the responsibility boundary clean: session service
handles within-session events; daemon handles between-session gaps.

### Why exclusive session control?

If user A and user B both have `tisvlay-session` running, only the foreground session
should control TVs. The daemon gives exclusive control to the active session and rejects
(or logs) commands from background sessions. This is simpler than arbitration and matches
how physical displays work: only the active session renders to the screen.

### Why keep HDMI input as system-level only (for now)?

`hdmi_input` and `only_when_active` were considered for user-level config, since different
users could have different GPU outputs mapped to different TV inputs. However, no concrete
multi-user HDMI input use case has been identified yet. The physical cabling is machine-
specific, not user-specific, in the common case. HDMI input remains system-level, with
the option to extend to user-level in a future phase if a use case emerges.

### Why D-Bus for config changes (not direct file editing)?

System config contains credentials and is root-owned. Users generally can't edit it
directly. D-Bus provides:

- Access control via polkit policies (admin restricts who can add/remove TVs)
- Validation before persistence (reject malformed config)
- Atomic reload (daemon applies changes immediately)
- Audit trail (D-Bus method calls are observable)

User config *could* be edited directly (it's user-owned), but the tray and GUI should
still go through D-Bus to the session service for consistency and to trigger live reload.

---

## Architecture Overview

```
                    +------------------------------------------+
                    |         tisvlayd (system daemon)          |
                    |                                          |
                    |  Config: /var/lib/tisvlay/config.toml    |
                    |                                          |
                    |  Owns:                                   |
                    |  - TV definitions + vendor credentials   |
                    |  - Daemon settings (wake, delays)        |
                    |  - System power policy                   |
                    |  - Default profile (inherited by users)  |
                    |  - HDMI input + only_when_active         |
                    |  - Active session tracking               |
                    |                                          |
                    |  Monitors (system bus):                   |
                    |  - PrepareForShutdown (reboot/poweroff)  |
                    |  - SessionNew / SessionRemoved            |
                    |  - Seat.ActiveSession changes             |
                    |                                          |
                    +------------------+-----------------------+
                                       | D-Bus (system bus)
                    +------------------+-----------------------+
                    |    tisvlay-session (per-user service)     |
                    |                                          |
                    |  Config: ~/.config/tisvlay/config.toml   |
                    |  (inherits system default profile)       |
                    |                                          |
                    |  Owns:                                   |
                    |  - Profile definitions                   |
                    |  - Per-TV profile assignments             |
                    |  - Idle timeout, user preferences         |
                    |                                          |
                    |  Monitors:                                |
                    |  - ScreenSaver.ActiveChanged (lock)       |
                    |  - PrepareForSleep (suspend)              |
                    |  - ext-idle-notify-v1 (Wayland idle)      |
                    |                                          |
                    +------------------------------------------+
```

```
                    +------------------+    +------------------+
                    |  tisvlay-tray    |    |  tisvlay-gui     |
                    |  (per-user)      |    |  (per-user)      |
                    +--------+---------+    +--------+---------+
                             |                       |
                    D-Bus to daemon          D-Bus to daemon
                    (TV control)             (TV control + system config)
                             |                       |
                    D-Bus to session         D-Bus to session
                    (profile switching)      (user config management)
```

---

## Config File Split

### System Config (`/var/lib/tisvlay/config.toml`)

Owned by `tisvlayd`. Contains machine-wide settings, TV hardware definitions,
vendor credentials, and system power policy. Readable by the session service
(for inheriting the default profile) but writable only by the daemon.

```toml
[daemon]
wake_on_boot = true             # Wake TVs on system boot
wake_delay_ms = 3000            # Delay after WoL before sending commands
on_shutdown = "power_off"       # Action when system shuts down / reboots
on_no_active_session = "none"   # Action when all user sessions end

# System default profile — inherited by users who have not created their own
# config file. Acts as the base layer for profile field inheritance.
[profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"
on_greeter_wake = "screen_on"
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 0
on_idle_timeout = "screen_off"

# TV definitions — daemon owns hardware identity and credentials
[[tv]]
id = "living-room"
name = "Living Room G4"
vendor = "lg"
hdmi_input = "HDMI_1"           # Which HDMI port this PC uses
only_when_active = true         # Only control when TV is on this input

[tv.lg]
ip = "192.168.1.100"
mac = "AA:BB:CC:DD:EE:FF"
client_key = "abc123..."        # Credential — never exposed over D-Bus

[[tv]]
id = "office"
name = "Office C3"
vendor = "lg"

[tv.lg]
ip = "192.168.1.101"
mac = "AA:BB:CC:DD:EE:02"
client_key = "def456..."
```

### User Config (`~/.config/tisvlay/config.toml`)

Owned by each user's `tisvlay-session` instance. Contains profile definitions
and per-TV profile assignments. This file is optional — if absent, the session
service inherits the system default profile for all TVs.

```toml
# User-defined profiles
[profiles.default]
# Overrides the system default profile for this user.
# The name "default" is reserved: it applies to any TV without an explicit
# profile assignment. Fields not specified here inherit from the system
# default profile, NOT from hard-coded defaults.
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"
idle_timeout_seconds = 600      # This user prefers a longer timeout

[profiles.gaming]
on_screen_lock = "none"
on_screen_unlock = "none"
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 0

# Per-TV profile assignments (reference TV IDs from system config)
[tv.living-room]
profile = "gaming"

# office TV has no entry — uses the user's default profile
# (or system default if user hasn't defined one)
```

### Profile Resolution Order

When the session service needs a profile for a given TV:

1. **User's per-TV assignment**: `[tv.<id>].profile` in user config
2. **User's default profile**: `[profiles.default]` in user config
3. **System default profile**: `[profiles.default]` in system config
4. **Hard-coded defaults**: `ProfileConfig::default()` (last resort)

For individual fields within a resolved profile:

1. **Value specified in the resolved profile** (user's gaming, user's default, etc.)
2. **Value from system default profile** (for unspecified fields)
3. **Hard-coded default** (if system default also doesn't specify it)

This means a user can create a minimal profile that only overrides what they care about:

```toml
[profiles.gaming]
on_screen_lock = "none"         # Override: don't blank during games
# idle_timeout_seconds not specified — inherits from system default
```

### TV Reference Validation

The user config references TV IDs defined in the system config. On startup and
config reload, the session service:

1. Queries the daemon via `GetTvs()` for the current TV list
2. Matches user config `[tv.*]` entries against the daemon's TV list
3. Logs warnings for unrecognized TV IDs (dangling references)
4. Applies profiles on a best-effort basis (unknown TVs are skipped)
5. TVs without a user config entry get the user's default profile (or system default)

The tray and GUI should surface dangling references to the user (e.g., warning icon
instead of connected/disconnected icon) so they can clean up stale entries.

---

## Daemon System Policy

### New Daemon Responsibilities

The daemon gains system-level event monitoring via `org.freedesktop.login1` on the
system bus:

| Signal / Property | Source | Daemon Action |
|---|---|---|
| `PrepareForShutdown(true)` | `login1.Manager` | Execute `on_shutdown` action for all TVs |
| `SessionNew` / `SessionRemoved` | `login1.Manager` | Track active user sessions |
| `Seat.ActiveSession` change | `login1.Seat` | Update which session has exclusive control |
| No active user session detected | (derived) | Execute `on_no_active_session` action |

### Active Session Tracking

The daemon tracks which user session is active on each seat using logind:

- On `SessionNew`: record the session, note its `Active` property
- On `Seat.ActiveSession` change: update which session is foreground
- On `SessionRemoved`: remove the session from tracking

When a `tisvlay-session` instance sends a D-Bus command (e.g., `ScreenOff`), the
daemon checks whether the calling session is the active one. If not, the command
is rejected and a debug-level log entry is emitted. The session service should
handle rejection gracefully (log and continue, not crash).

### Shutdown Handling

The daemon takes a `delay` inhibitor lock on startup:

```
Inhibit("shutdown", "tisvlay", "Turning off TVs", "delay")
```

When `PrepareForShutdown(true)` fires:

1. Execute `on_shutdown` action for all TVs (e.g., `power_off`)
2. Release the inhibitor lock
3. System proceeds with shutdown

The inhibitor gives the daemon up to `InhibitDelayMaxUSec` (default 5 seconds) to
gracefully shut down TVs before the system powers off.

### Boot Behavior

Existing `wake_on_boot` behavior is unchanged. The daemon wakes TVs and optionally
switches to `hdmi_input` so the login screen is visible. This happens before any
user session starts.

---

## D-Bus Interface Changes

### Daemon (org.tisvlay.Manager1) — New Methods

```
# System config management (polkit-protected)
SetDaemonOption(key: s, value: s)
    Set a daemon config option (e.g., "wake_on_boot" = "true").
    Saves to system config and applies immediately.

SetTvOption(tv_id: s, key: s, value: s)
    Set a system-level TV option (e.g., "hdmi_input" = "HDMI_2").
    Saves to system config and applies immediately.

GetSystemProfile() -> s
    Returns the system default profile as JSON.
    Used by session services to inherit defaults.
```

### Daemon (org.tisvlay.Tv1) — New Property

```
ActiveSessionOwner: s (read-only)
    The session ID that currently has exclusive control of this TV,
    or empty string if no user session is active.
```

### Session Service (org.tisvlay.Session1) — New Interface

The session service registers on the **session bus** under `org.tisvlay.Session`:

```
# Profile management
ListProfiles() -> a(ss)
    Returns [(profile_name, profile_json), ...] for all user-defined profiles.

GetTvProfile(tv_id: s) -> s
    Returns the profile name assigned to a TV (or "default" if unassigned).

SetTvProfile(tv_id: s, profile_name: s)
    Assign a profile to a TV. Saves to user config and applies immediately.

GetEffectiveProfile(tv_id: s) -> s
    Returns the fully resolved profile as JSON (with system default inheritance
    applied). Useful for UIs to show what will actually happen.

# Config management
ReloadConfig()
    Reload user config from disk.
```

---

## Config Struct Changes

### System Config (Daemon)

The existing `Config` struct is refactored for the daemon's needs:

```rust
/// System-level configuration (daemon-owned).
pub struct SystemConfig {
    pub daemon: DaemonConfig,
    /// System default profile — inherited by users without their own config.
    pub profiles: HashMap<String, ProfileConfig>,
    pub tv: Vec<TvConfig>,
}

pub struct DaemonConfig {
    pub wake_on_boot: bool,
    pub wake_delay_ms: u64,
    pub on_shutdown: SessionAction,        // NEW
    pub on_no_active_session: SessionAction, // NEW
}
```

`TvConfig` retains `hdmi_input` and `only_when_active` (system-level).
`TvConfig` loses the `profile` field (moved to user config).

### User Config (Session)

A new struct for the session service:

```rust
/// User-level configuration (session-owned).
pub struct UserConfig {
    /// User-defined profiles.
    pub profiles: HashMap<String, ProfileConfig>,
    /// Per-TV profile assignments. Key is TV ID from system config.
    pub tv: HashMap<String, UserTvConfig>,
}

/// User's per-TV settings.
pub struct UserTvConfig {
    /// Profile name to use for this TV.
    pub profile: String,
}
```

### Profile Merging

```rust
impl UserConfig {
    /// Resolve the effective profile for a TV, with system default inheritance.
    pub fn resolve_profile(
        &self,
        tv_id: &str,
        system_default: &ProfileConfig,
    ) -> ProfileConfig {
        // 1. Find which profile name applies to this TV
        let profile_name = self.tv.get(tv_id)
            .map(|t| t.profile.as_str())
            .unwrap_or("default");

        // 2. Get the user's profile (or empty if not defined)
        let user_profile = self.profiles.get(profile_name);

        // 3. Merge: user profile fields override system default
        match user_profile {
            Some(profile) => profile.merge_with_base(system_default),
            None => system_default.clone(),
        }
    }
}
```

To support field-level inheritance, `ProfileConfig` fields become `Option<T>`:

```rust
/// Profile configuration with optional fields for merge support.
///
/// When serialized from a user config, unspecified fields are `None`.
/// `merge_with_base()` fills `None` fields from the system default.
pub struct ProfileConfig {
    pub on_screen_lock: Option<SessionAction>,
    pub on_screen_unlock: Option<SessionAction>,
    pub on_greeter_wake: Option<SessionAction>,
    pub on_sleep: Option<SessionAction>,
    pub on_wake: Option<SessionAction>,
    pub idle_timeout_seconds: Option<u64>,
    pub on_idle_timeout: Option<SessionAction>,
}

/// Fully resolved profile with no optional fields.
/// Result of merging a ProfileConfig with a base.
pub struct ResolvedProfile {
    pub on_screen_lock: SessionAction,
    pub on_screen_unlock: SessionAction,
    pub on_greeter_wake: SessionAction,
    pub on_sleep: SessionAction,
    pub on_wake: SessionAction,
    pub idle_timeout_seconds: u64,
    pub on_idle_timeout: SessionAction,
}
```

---

## Session Service Changes

### Startup Sequence

1. Connect to system D-Bus
2. Query daemon: `GetTvs()` for current TV list
3. Query daemon: `GetSystemProfile()` for system default profile
4. Load user config from `~/.config/tisvlay/config.toml` (or empty if absent)
5. Resolve effective profiles for each TV (user + system default inheritance)
6. Validate TV references, log warnings for unknown IDs
7. Register `org.tisvlay.Session` on the session bus
8. Start event monitors (screen lock, suspend, idle)

### Systemd Dependencies

Production deployment (NixOS module) uses systemd ordering:

```ini
[Unit]
After=tisvlayd.service
Wants=tisvlayd.service
```

For development (all components on session bus), the session service retries
daemon connection with backoff if not immediately available, preserving the
current "warn and retry on events" behavior.

### Config Reload

On `SIGHUP` or `ReloadConfig()` D-Bus call:

1. Re-query daemon for TV list and system default profile
2. Reload user config from disk
3. Re-resolve all effective profiles
4. Log changes (new TVs, removed TVs, profile changes)

---

## NixOS Module Changes

The NixOS module needs updates for the config split:

```nix
# System daemon — unchanged path, but config schema changes
systemd.services.tisvlayd = {
  serviceConfig = {
    ExecStart = "${lib.getExe' cfg.package "tisvlayd"}";
    # /var/lib/tisvlay/config.toml — system config with credentials
    StateDirectory = "tisvlay";
    StateDirectoryMode = "0750";  # Changed: group-readable, not world-readable
  };
};

# Session service — no longer reads system config directly
# Instead queries daemon via D-Bus for TV list and system default profile
systemd.user.services.tisvlay-session = {
  serviceConfig = {
    ExecStart = lib.getExe' cfg.package "tisvlay-session";
    # No --config flag needed — reads from ~/.config/tisvlay/config.toml
    # (or inherits system defaults via D-Bus if no user config exists)
  };
  unitConfig = {
    # Prefer daemon to be running, but don't hard-fail if it isn't yet
    Wants = [ "tisvlayd.service" ];
    After = [ "tisvlayd.service" ];
  };
};
```

Note: `StateDirectoryMode` tightens from `0755` to `0750` since the session
service no longer needs to read the system config file directly. The session
service gets the system default profile via D-Bus (`GetSystemProfile()`).

---

## Tray App Changes

The tray app gains profile awareness:

### Profile Switching

Each TV's submenu gains a "Profile" submenu showing available profiles:

```
> Living Room G4  [connected]
  +-- Power Off
  +-- Profile >
  |     +-- (*) gaming
  |     +-- ( ) default
  |     +-- ( ) movie
  +-- Input >  ...
  +-- Volume: 60  ...
  +-- Screen Off
```

The tray queries the session service for available profiles (`ListProfiles()`)
and current assignments (`GetTvProfile()`). Selecting a profile calls
`SetTvProfile()` on the session service.

### Dangling TV References

If a TV in the user config is not found in the daemon's TV list:

```
> Unknown TV  [warning]
  +-- TV "old-tv" not found in system config
  +-- Remove from user config
```

---

## Implementation Plan

### Step 1: Config Struct Refactor

1. Create `SystemConfig` and `UserConfig` structs in `tisvlay-core`
2. Add `ProfileConfig` with `Option<T>` fields and `ResolvedProfile`
3. Add `merge_with_base()` method for profile inheritance
4. Keep backward-compatible `Config` as a type alias or migration helper
5. Add `GetSystemProfile()` to daemon D-Bus interface

### Step 2: Daemon System Policy

1. Add `on_shutdown` and `on_no_active_session` to `DaemonConfig`
2. Add logind session tracking (monitor `SessionNew`/`SessionRemoved`,
   `Seat.ActiveSession` on system bus)
3. Add `PrepareForShutdown` handler with inhibitor lock
4. Add active session enforcement (reject D-Bus commands from inactive sessions)

### Step 3: Session Service Config Split

1. Change session service to load `UserConfig` from XDG path
2. Query daemon for TV list and system default profile on startup
3. Implement profile resolution with inheritance
4. Add `org.tisvlay.Session1` D-Bus interface (ListProfiles, GetTvProfile,
   SetTvProfile, GetEffectiveProfile, ReloadConfig)
5. Update SIGHUP handler to re-query daemon + reload user config

### Step 4: Tray Profile Switching

1. Add profile submenu to per-TV menus
2. Query session service for profiles and assignments
3. Implement profile switching via `SetTvProfile()`
4. Add dangling TV reference warnings

### Step 5: NixOS Module Update

1. Remove `--config` from session service ExecStart
2. Tighten `StateDirectoryMode` to `0750`
3. Add systemd dependency ordering

### Step 6: Granular System Config D-Bus API

1. Add `SetDaemonOption()` and `SetTvOption()` methods
2. Add polkit policy for system config changes
3. Update NixOS module with polkit rules if needed

---

## Open Questions and Future Considerations

### User Session-Aware HDMI Input

Currently `hdmi_input` is system-level only. A future phase could allow users to
specify their own HDMI input per TV, enabling scenarios like:

- User A's desktop on HDMI_1, User B's desktop on HDMI_2
- Session service registers its HDMI input preferences with the daemon on startup
- Daemon uses this for input-aware filtering of commands

This would require a session registration mechanism (e.g.,
`RegisterSession(session_id, {tv_id: hdmi_input, ...})`) and careful handling of
the handoff when sessions switch. Deferred until a concrete multi-user HDMI input
use case is identified.

### Granular Profile Option API

The current design includes `ListProfiles`, `GetTvProfile`, and `SetTvProfile` on
the session service D-Bus interface, but defers per-field profile editing (e.g.,
`SetProfileOption(profile, key, value)`). This will be needed for the Phase 5 GUI
but the scope depends on what profile features and TV backend options emerge. The
GUI could initially use `GetConfig()`/`SetConfig()` bulk operations, with granular
methods added as specific needs arise.

### Polkit Policy Design

The `SetDaemonOption`, `SetTvOption`, and `AddTv`/`RemoveTv` D-Bus methods should
be protected by polkit policies. The exact policy structure (which actions require
admin auth, which can be delegated to specific groups) is left to the implementation
phase. A reasonable default:

- `AddTv` / `RemoveTv`: require admin authentication
- `SetDaemonOption`: require admin authentication
- `SetTvOption`: require admin authentication
- All `Tv1.*` control methods (PowerOn, ScreenOff, etc.): allow active session user

### Login Screen TV Behavior

The daemon's `on_no_active_session` action fires when all user sessions end. This
means the login screen (plasma-login-manager greeter) will show on physical
displays, but the TV's behavior depends on this setting. Admins should consider:

- `"none"`: TV stays in whatever state the last session left it (could be blanked)
- `"screen_on"`: TV unblanks so the login screen is visible
- `"power_off"`: TV turns off when nobody is logged in (saves power)

The right default depends on whether the TV is the primary display. This could be
refined with per-TV system policy in a future phase.

### Config Migration

Existing users have a single config file. A migration path is needed:

1. If the daemon finds a config with `[profiles]` and `[[tv]]` entries with
   `profile = "..."`, it should still work (backward compat)
2. A CLI command (`tisvlay config migrate`) could split an existing config into
   system and user configs
3. The NixOS module update should handle the transition cleanly

### D-Bus Signal for TV List Changes

When the daemon adds or removes a TV (via `AddTv`/`RemoveTv` or config reload),
session services need to know so they can re-validate their TV references. The
existing `TvAdded`/`TvRemoved` signals on `org.tisvlay.Manager1` serve this
purpose. Session services should subscribe to these signals and re-resolve
profiles when the TV list changes.

### Idle Timeout Changes Without Restart

Currently, idle timeout changes require restarting the session service because the
idle monitor is started once at launch. With the new D-Bus `ReloadConfig()` method
on the session service, it would be nice to also restart the idle monitor with the
new timeout. This is an implementation detail to address in Step 3.

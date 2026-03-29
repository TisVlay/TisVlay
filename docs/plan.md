# Refactor: Per-TV system profiles and unified profile architecture

## Motivation

Daemon lifecycle events (`on_startup`, `on_shutdown`, `on_reboot`,
`on_no_session`) currently apply the same action to all managed TVs.
There is no way for an admin to configure different shutdown behavior
per TV — e.g., power off the living room TV on shutdown but only blank
the guest room TV's screen.

Session/user profiles already support per-TV customization via named
profiles and per-TV assignment. This refactor extends the same concept
to system-level (daemon) events, creating a symmetric two-profile-type
architecture.

## Design decisions

- **Two profile types**: System profiles (daemon events) and user profiles
  (session events). Separate types prevent confusion about which fields
  each service reads.
- **Naming**: "system" / "user" (standard Linux terminology, describes
  purpose not reader identity — a future GUI editor would also read
  system profiles).
- **No backward compatibility constraints**. Config shapes, field names,
  D-Bus APIs can all change freely.
- **Single-file support preserved**: config shape avoids namespace
  collisions so system and user config can live in one file or be split.

---

## Config shape

### Example A: Single file (development / simple setup)

Both system and user config in one file. The daemon reads `system.*` and
`[[tv]]`; the session service reads `user.*` and `[tv.*]` (user TV
overrides at the end).

```toml
# System profiles — daemon lifecycle events
[system.profiles.default]
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_no_session = "none"

[system.profiles.guest]
on_startup = "none"
on_shutdown = "screen_off"

# User profiles — session event defaults (admin-set baseline)
# Users inherit these when they have no user config file.
[user.profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"
on_greeter_wake = "screen_on"
on_sleep = "power_off"
on_wake = "power_on"
idle_timeout_seconds = 0
on_idle_timeout = "screen_off"
on_idle_resume = "screen_on"

# TV definitions — daemon owns these
[[tv]]
id = "living-room"
name = "Living Room G4"
vendor = "lg"
managed = true
profile.system = "default"

[[tv]]
id = "guest-room"
name = "Guest Room"
vendor = "lg"
managed = true
profile.system = "guest"

# User per-TV overrides (in single-file mode, appended at end)
[tv.living-room]
profile.user = "default"
```

### Example B: System config (`/var/lib/tisvlay/config.toml`)

Admin-owned. Daemon reads the whole file; session service queries the
daemon via D-Bus for `user.profiles.default` (admin baseline) and TV
list. The admin does **not** assign user profiles to TVs — that's the
user's choice.

```toml
[system.profiles.default]
on_startup = "power_on"
on_shutdown = "power_off"
on_reboot = "none"
on_no_session = "none"

[system.profiles.guest]
on_startup = "none"
on_shutdown = "screen_off"

# Admin-set baseline for session behavior.
# Users without their own config inherit this.
[user.profiles.default]
on_screen_lock = "screen_off"
on_screen_unlock = "screen_on"
on_greeter_wake = "screen_on"
on_sleep = "power_off"
on_wake = "power_on"

[[tv]]
id = "living-room"
name = "Living Room G4"
vendor = "lg"
managed = true
profile.system = "default"

[[tv]]
id = "guest-room"
name = "Guest Room"
vendor = "lg"
managed = true
profile.system = "guest"
```

### Example C: User config (`~/.config/tisvlay/config.toml`)

Per-user. Session service reads this directly. Can define additional
user profiles and assign them to TVs. Cannot set system profiles —
that's admin-only.

```toml
# User's own profiles (override or extend admin defaults)
[user.profiles.default]
idle_timeout_seconds = 300
on_idle_timeout = "screen_off"

[user.profiles.gaming]
on_screen_lock = "none"
idle_timeout_seconds = 0

# Per-TV user profile assignment
[tv.living-room]
profile.user = "gaming"

# guest-room inherits user.profiles.default (no override)
```

### Key changes from current config

| Current | New |
|---------|-----|
| `[daemon]` with flat action fields | `[system.profiles.default]` |
| `[profiles.*]` | `[user.profiles.*]` |
| `profile = "gaming"` on UserTvConfig | `profile.user = "gaming"` |
| No per-TV daemon behavior | `profile.system = "guest"` on TvConfig |
| DaemonConfig struct | Removed — replaced by system profile |

---

## Profile types

### SystemProfileConfig

Fields (all `Option<SessionAction>` for merge support):
- `on_startup`
- `on_shutdown`
- `on_reboot`
- `on_no_session`

Defaults (via `ResolvedSystemProfile::default()`):
- `on_startup`: PowerOn
- `on_shutdown`: PowerOff
- `on_reboot`: None
- `on_no_session`: None

### UserProfileConfig (renamed from ProfileConfig)

Fields (unchanged from current `ProfileConfig`):
- `on_screen_lock`
- `on_screen_unlock`
- `on_greeter_wake`
- `on_sleep`
- `on_wake`
- `idle_timeout_seconds`
- `on_idle_timeout`
- `on_idle_resume`

Defaults (unchanged from current `ResolvedProfile::default()`).

### Merge chains

**System profile**: per-TV system profile → system default → hard-coded defaults.
Resolved by daemon at event dispatch time.

**User profile**: per-TV user profile (user config) → user default
(user config) → admin baseline `user.profiles.default` (system config,
queried from daemon via D-Bus) → hard-coded defaults.
Resolved by session service.

---

## Part 1: Core config restructure

### 1.1 Add SystemProfileConfig and ResolvedSystemProfile

**File:** `crates/tisvlay-core/src/config.rs`

- New `SystemProfileConfig` struct (Option fields, serde, merge_with_base,
  resolve)
- New `ResolvedSystemProfile` struct (concrete fields, Default impl)
- Mirror the existing ProfileConfig/ResolvedProfile pattern

### 1.2 Rename ProfileConfig → UserProfileConfig

**File:** `crates/tisvlay-core/src/config.rs`

- Rename `ProfileConfig` → `UserProfileConfig`
- Rename `ResolvedProfile` → `ResolvedUserProfile`
- Update all references across all crates

### 1.3 Restructure SystemConfig

**File:** `crates/tisvlay-core/src/config.rs`

Replace:
```rust
pub struct SystemConfig {
    pub daemon: DaemonConfig,
    pub profiles: HashMap<String, ProfileConfig>,
    pub tv: Vec<TvConfig>,
}
```

With:
```rust
pub struct SystemConfig {
    pub system: SystemSection,
    pub user: UserSection,
    pub tv: Vec<TvConfig>,
}

pub struct SystemSection {
    pub profiles: HashMap<String, SystemProfileConfig>,
}

pub struct UserSection {
    pub profiles: HashMap<String, UserProfileConfig>,
}
```

Remove `DaemonConfig` entirely — its fields move into `SystemProfileConfig`.

### 1.4 Restructure UserConfig

**File:** `crates/tisvlay-core/src/config.rs`

Replace:
```rust
pub struct UserConfig {
    pub profiles: HashMap<String, ProfileConfig>,
    pub tv: HashMap<String, UserTvConfig>,
}
```

With:
```rust
pub struct UserConfig {
    pub user: UserSection,
    pub tv: HashMap<String, UserTvConfig>,
}
```

(Reuse the same `UserSection` struct for both system and user config.)

### 1.5 Add profile assignment to TvConfig

**File:** `crates/tisvlay-core/src/config.rs`

Add to `TvConfig`:
```rust
pub profile: TvProfileAssignment,

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TvProfileAssignment {
    #[serde(default = "default_profile_name")]
    pub system: String,  // default: "default"
    // No `user` field — per-TV user profile assignment is in UserTvConfig only
}
```

### 1.6 Update UserTvConfig

**File:** `crates/tisvlay-core/src/config.rs`

Replace `profile: String` with:
```rust
pub profile: UserTvProfileAssignment,

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTvProfileAssignment {
    pub user: String,  // only user profile settable from user config
}
```

### 1.7 Add system profile resolution

**File:** `crates/tisvlay-core/src/config.rs`

Add method to `SystemConfig`:
```rust
pub fn resolve_system_profile(&self, tv: &TvConfig) -> ResolvedSystemProfile {
    let profile_name = &tv.profile.system;
    let profile = self.system.profiles.get(profile_name);
    let default = self.system.profiles.get("default");
    // merge: named profile → default → hard-coded defaults
}
```

### 1.8 Update tests

All config tests use the old shape — update TOML fixtures and assertions.

---

## Part 2: Daemon per-TV event dispatch

### 2.1 Store resolved system profiles in TvManager

**File:** `crates/tisvlayd/src/tv_manager.rs`

Add `system_profile: ResolvedSystemProfile` to `ManagedTv`. Resolved
on load and on config reload.

### 2.2 Replace global event dispatch with per-TV dispatch

**File:** `crates/tisvlayd/src/tv_manager.rs`

Replace `get_daemon_config_action(DaemonAction)` (returns one action for
all TVs) with per-TV resolution:

```rust
pub async fn execute_event_all_tvs(&self, event: SystemEvent, context: &str) {
    let tv_ids = self.list_managed_tv_ids().await;
    for tv_id in &tv_ids {
        let action = self.get_system_action(tv_id, event).await;
        if action != SessionAction::None {
            self.execute_action(tv_id, action, context).await;
        }
    }
}
```

Where `get_system_action` reads the per-TV `ResolvedSystemProfile`.

### 2.3 Remove DaemonAction enum and DaemonConfig

**Files:** `crates/tisvlayd/src/tv_manager.rs`, `crates/tisvlayd/src/logind.rs`

Replace `DaemonAction` with `SystemEvent` enum:
```rust
pub enum SystemEvent {
    Startup,
    Shutdown,
    Reboot,
    NoSession,
}
```

Update logind.rs to use `execute_event_all_tvs(SystemEvent::Shutdown, ...)`.

### 2.4 Update `on_startup` for per-TV profiles

**File:** `crates/tisvlayd/src/tv_manager.rs`

`startup_power_on` currently wakes all managed TVs. Under per-TV profiles,
each TV's `on_startup` may differ (PowerOn, ScreenOn, None). The startup
logic needs to iterate TVs, check each profile, and only WoL the ones
with `on_startup = PowerOn`.

### 2.5 Update `set_daemon_option` or replace with profile API

**File:** `crates/tisvlayd/src/tv_manager.rs`

The current `SetDaemonOption(key, value)` D-Bus method operates on the
global DaemonConfig. With per-TV profiles, this needs rethinking:
- Option A: `SetTvSystemProfile(tv_id, profile_name)` — switch a TV's
  system profile (analogous to session's `SetTvProfile`)
- Option B: Keep `SetDaemonOption` for backward compat, but it sets
  fields on the "default" system profile

Recommend Option A for consistency with the session profile API.

---

## Part 3: D-Bus API updates

### 3.1 Add system profile methods to Manager1

**File:** `crates/tisvlayd/src/dbus/manager.rs`

- `GetSystemProfiles() → a{sa{sv}}` — list system profiles with fields
- `GetSystemProfile(name) → a{sv}` — get a single system profile

### 3.2 Add system profile property to Tv1

**File:** `crates/tisvlayd/src/dbus/tv.rs`

- Add `SystemProfile` property (readable, writable) to `org.tisvlay.Tv1`
- `SetTvOption(key, value)` with key `"profile.system"` to change assignment
- Emit `PropertiesChanged` on update

### 3.3 Update DaemonClient

**File:** `crates/tisvlay-core/src/dbus_client.rs`

- Add `get_system_profiles()`, `get_system_profile(name)` methods
- Add `set_tv_system_profile(tv_id, profile_name)` method
- Update `list_tvs()` to include `system_profile` field in `TvInfo`

---

## Part 4: Session service updates

### 4.1 Update profile resolution for new config shape

**File:** `crates/tisvlay-session/src/main.rs`

- `SessionState::load()` reads `user.profiles` from user config, plus
  admin baseline `user.profiles.default` from daemon via D-Bus
- Per-TV user profile assignment comes only from user config
  (`[tv.{id}] profile.user`), not from system config

### 4.2 Update session D-Bus interface

**File:** `crates/tisvlay-session/src/session_dbus.rs`

- `ListProfiles()` returns user profiles only (no system profiles)
- `GetTvProfile()` / `SetTvProfile()` operate on `profile.user`
- `GetEffectiveProfile()` returns resolved user profile

---

## Part 5: Tray and CLI updates

### 5.1 Tray: dual profile submenus

**File:** `crates/tisvlay-tray/src/tray.rs`

- Rename existing "Profile" submenu to "User Profile"
- Add "System Profile" submenu alongside it
- Query daemon for available system profiles
- Switch system profile via daemon D-Bus, user profile via session D-Bus

### 5.2 CLI: explicit system/user profile commands

**File:** `crates/tisvlay-cli/src/main.rs`

Replace existing `tisvlay profile list/get/set` with explicit subcommands:

```
tisvlay profile system list
tisvlay profile system get [--tv ID]
tisvlay profile system set <name> [--tv ID]

tisvlay profile user list
tisvlay profile user get [--tv ID]
tisvlay profile user set <name> [--tv ID]
```

No bare `tisvlay profile list` — always require `system` or `user` to
avoid ambiguity.

---

## Part 6: Comprehensive profile rename

The shift from implicit "profile = user profile" to explicit system/user
duality requires renaming identifiers across the entire codebase. This
is mostly mechanical but must be thorough to avoid confusion.

### 6.1 Rust types and methods

| Current | New | File(s) |
|---------|-----|---------|
| `ProfileConfig` | `UserProfileConfig` | config.rs, lib.rs |
| `ResolvedProfile` | `ResolvedUserProfile` | config.rs, lib.rs |
| `get_tv_profile_name()` | `get_tv_user_profile_name()` | config.rs |
| `set_tv_profile()` | `set_tv_user_profile()` | config.rs |
| `resolve_profile()` | `resolve_user_profile()` | config.rs |
| `TrayCommand::SetProfile` | `TrayCommand::SetUserProfile` | tray.rs |
| `available_profiles` (CachedTvState) | `available_user_profiles` | state.rs, tray.rs |
| `profile` (CachedTvState) | `user_profile` | state.rs, tray.rs |
| `system_default` (SessionState) | `admin_user_default` | main.rs (session) |
| `system_profiles` (SessionState) | `admin_user_profiles` | main.rs (session) |

### 6.2 D-Bus interfaces and methods

**Daemon (`org.tisvlay.Manager1`):**

| Current | New | Notes |
|---------|-----|-------|
| `GetSystemProfile()` | `GetUserProfileDefault()` | Admin baseline for user profiles |
| `GetSystemProfiles()` | `ListUserProfiles()` | Admin-defined user profiles |
| `SetDaemonOption(key, value)` | Remove | Replaced by system profile API |
| *(new)* | `ListSystemProfiles()` | System profile definitions |
| *(new)* | `GetTvSystemProfile(tv_id)` | Per-TV system profile name |
| *(new)* | `SetTvSystemProfile(tv_id, name)` | Switch TV's system profile |

**Daemon (`org.tisvlay.Tv1`):**

| Current | New | Notes |
|---------|-----|-------|
| *(new)* | `SystemProfile` property | Readable + writable |

**Session (`org.tisvlay.Session1`):**

| Current | New | Notes |
|---------|-----|-------|
| `ListProfiles()` | `ListUserProfiles()` | Explicit user scope |
| `GetTvProfile(tv_id)` | `GetTvUserProfile(tv_id)` | Explicit user scope |
| `SetTvProfile(tv_id, name)` | `SetTvUserProfile(tv_id, name)` | Explicit user scope |
| `GetEffectiveProfile(tv_id)` | `GetEffectiveUserProfile(tv_id)` | Explicit user scope |

### 6.3 D-Bus client methods

| Current | New | File |
|---------|-----|------|
| `DaemonClient::get_system_profile()` | `get_user_profile_default()` | dbus_client.rs |
| `DaemonClient::get_system_profiles()` | `list_user_profiles()` | dbus_client.rs |
| `SessionClient::list_profiles()` | `list_user_profiles()` | dbus_client.rs |
| `SessionClient::get_tv_profile()` | `get_tv_user_profile()` | dbus_client.rs |
| `SessionClient::set_tv_profile()` | `set_tv_user_profile()` | dbus_client.rs |
| *(new)* | `DaemonClient::list_system_profiles()` | dbus_client.rs |
| *(new)* | `DaemonClient::get_tv_system_profile()` | dbus_client.rs |
| *(new)* | `DaemonClient::set_tv_system_profile()` | dbus_client.rs |

### 6.4 CLI subcommands

| Current | New |
|---------|-----|
| `tisvlay profile list` | `tisvlay profile user list` |
| `tisvlay profile get` | `tisvlay profile user get` |
| `tisvlay profile set <name>` | `tisvlay profile user set <name>` |
| *(new)* | `tisvlay profile system list` |
| *(new)* | `tisvlay profile system get` |
| *(new)* | `tisvlay profile system set <name>` |

### 6.5 Tray

| Current | New |
|---------|-----|
| "Profile: gaming" submenu | "User Profile: gaming" submenu |
| `TrayCommand::SetProfile` | `TrayCommand::SetUserProfile` |
| *(new)* | "System Profile: default" submenu |
| *(new)* | `TrayCommand::SetSystemProfile` |

---

## Part 7: Shared profile utilities

### 6.1 Generic merge/resolve

Both profile types follow the same pattern: Option fields, merge_with_base,
resolve to concrete defaults. Extract a generic approach or macro to
avoid duplicating the merge/resolve logic between SystemProfileConfig
and UserProfileConfig.

Consider a `ProfileMerge` trait:
```rust
trait ProfileMerge: Default {
    fn merge_with_base(&self, base: &Self) -> Self;
    fn resolve(&self) -> Self::Resolved;
}
```

Or simply keep two parallel impls if the trait adds more complexity than
it saves (two structs × ~20 lines each is manageable).

---

## Part 7: Update docs, configs, tests

- `docs/design.md` — config schema, D-Bus interface, event→action table
- `docs/configs/daemon.toml` → new shape
- `docs/configs/session.toml` → new shape
- `.claude/CLAUDE.md` — project structure, key implementation details
- All config test fixtures

---

## Checklist

- [ ] Part 1.1: Add SystemProfileConfig and ResolvedSystemProfile
- [ ] Part 1.2: Rename ProfileConfig → UserProfileConfig
- [ ] Part 1.3: Restructure SystemConfig (system/user sections)
- [ ] Part 1.4: Restructure UserConfig
- [ ] Part 1.5: Add TvProfileAssignment to TvConfig
- [ ] Part 1.6: Update UserTvConfig profile assignment
- [ ] Part 1.7: Add system profile resolution
- [ ] Part 1.8: Update config tests
- [ ] Part 2.1: Store resolved system profiles in ManagedTv
- [ ] Part 2.2: Per-TV event dispatch (replace global dispatch)
- [ ] Part 2.3: Remove DaemonAction/DaemonConfig, add SystemEvent
- [ ] Part 2.4: Update on_startup for per-TV profiles
- [ ] Part 2.5: Replace SetDaemonOption with profile API
- [ ] Part 3.1: System profile D-Bus methods on Manager1
- [ ] Part 3.2: SystemProfile property on Tv1
- [ ] Part 3.3: Update DaemonClient
- [ ] Part 4.1: Session profile resolution for new config shape
- [ ] Part 4.2: Update session D-Bus interface
- [ ] Part 5.1: Tray dual profile submenus
- [ ] Part 5.2: CLI explicit system/user profile commands
- [ ] Part 6.1: Rename Rust types and methods (ProfileConfig → UserProfileConfig, etc.)
- [ ] Part 6.2: Rename D-Bus interfaces and methods
- [ ] Part 6.3: Rename D-Bus client methods
- [ ] Part 6.4: Rename CLI subcommands
- [ ] Part 6.5: Rename tray commands and labels
- [ ] Part 7.1: Shared profile merge/resolve utilities
- [ ] Part 8: Update docs, configs, tests
- [ ] `cargo check` all crates
- [ ] `cargo test` all crates

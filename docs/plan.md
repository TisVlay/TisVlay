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

## Implementation order

The rename (old "Part 6") must happen first — before adding new system
profile features — to avoid building new code on top of ambiguous names.

### Phase A: Rename existing profile identifiers to `user_profile`

Mechanical rename across all crates. Must compile and pass tests before
proceeding. Uses the rename tables below.

**A.1 Core types and config** (`tisvlay-core/src/config.rs`, `lib.rs`):

| Current | New |
|---------|-----|
| `ProfileConfig` | `UserProfileConfig` |
| `ResolvedProfile` | `ResolvedUserProfile` |
| `get_tv_profile_name()` | `get_tv_user_profile_name()` |
| `set_tv_profile()` | `set_tv_user_profile()` |
| `resolve_profile()` | `resolve_user_profile()` |
| `DaemonConfig` | Keep for now — removed in Phase B |

**A.2 D-Bus interfaces and client** (`dbus_client.rs`, `session_dbus.rs`,
`dbus/manager.rs`):

Daemon (`org.tisvlay.Manager1`):

| Current | New |
|---------|-----|
| `GetSystemProfile()` | `GetUserProfileDefault()` |
| `GetSystemProfiles()` | `ListUserProfiles()` |

Session (`org.tisvlay.Session1`):

| Current | New |
|---------|-----|
| `ListProfiles()` | `ListUserProfiles()` |
| `GetTvProfile(tv_id)` | `GetTvUserProfile(tv_id)` |
| `SetTvProfile(tv_id, name)` | `SetTvUserProfile(tv_id, name)` |
| `GetEffectiveProfile(tv_id)` | `GetEffectiveUserProfile(tv_id)` |

Client methods:

| Current | New |
|---------|-----|
| `DaemonClient::get_system_profile()` | `get_user_profile_default()` |
| `DaemonClient::get_system_profiles()` | `list_user_profiles()` |
| `SessionClient::list_profiles()` | `list_user_profiles()` |
| `SessionClient::get_tv_profile()` | `get_tv_user_profile()` |
| `SessionClient::set_tv_profile()` | `set_tv_user_profile()` |

**A.3 Session service** (`tisvlay-session/src/main.rs`):

| Current | New |
|---------|-----|
| `system_default` (SessionState) | `admin_user_default` |
| `system_profiles` (SessionState) | `admin_user_profiles` |

**A.4 Tray** (`tisvlay-tray/src/tray.rs`, `state.rs`, `main.rs`):

| Current | New |
|---------|-----|
| `TrayCommand::SetProfile` | `TrayCommand::SetUserProfile` |
| `available_profiles` (CachedTvState) | `available_user_profiles` |
| `profile` (CachedTvState) | `user_profile` |
| "Profile: gaming" label | "User Profile: gaming" |

**A.5 CLI** (`tisvlay-cli/src/main.rs`):

| Current | New |
|---------|-----|
| `tisvlay profile list` | `tisvlay profile user list` |
| `tisvlay profile get` | `tisvlay profile user get` |
| `tisvlay profile set <name>` | `tisvlay profile user set <name>` |

Checkpoint: `cargo check`, `cargo test`, commit.

---

### Phase B: Core config restructure

**B.1** Add `SystemProfileConfig` and `ResolvedSystemProfile` to config.rs.
Mirror the UserProfileConfig pattern (Option fields, merge_with_base,
resolve). Consider extracting shared merge logic if it reduces
duplication without over-abstracting.

**B.2** Restructure `SystemConfig`:
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
Remove `DaemonConfig` — fields moved into `SystemProfileConfig`.

**B.3** Restructure `UserConfig`:
```rust
pub struct UserConfig {
    pub user: UserSection,
    pub tv: HashMap<String, UserTvConfig>,
}
```

**B.4** Add `TvProfileAssignment` to `TvConfig`:
```rust
pub struct TvProfileAssignment {
    pub system: String,  // default: "default"
    // No user field — owned by UserTvConfig
}
```

**B.5** Update `UserTvConfig` profile field:
```rust
pub struct UserTvProfileAssignment {
    pub user: String,
}
```

**B.6** Add `resolve_system_profile()` to `SystemConfig`.

**B.7** Update all config tests for new TOML shape.

Checkpoint: `cargo check`, `cargo test`, commit.

---

### Phase C: Daemon per-TV event dispatch

**C.1** Add `system_profile: ResolvedSystemProfile` to `ManagedTv`.
Resolve on load and config reload.

**C.2** Replace `DaemonAction` enum with `SystemEvent`:
```rust
pub enum SystemEvent { Startup, Shutdown, Reboot, NoSession }
```

**C.3** Replace global `execute_action_all_tvs` with per-TV dispatch
that reads each TV's resolved system profile.

**C.4** Update `on_startup` for per-TV profiles — only WoL TVs whose
system profile has `on_startup = PowerOn`.

**C.5** Remove `SetDaemonOption` D-Bus method. Replace with
`SetTvSystemProfile(tv_id, name)` on the per-TV object.

**C.6** Update logind.rs to use `SystemEvent`.

Checkpoint: `cargo check`, `cargo test`, commit.

---

### Phase D: D-Bus API + DaemonClient for system profiles

**D.1** Add to `org.tisvlay.Manager1`:
- `ListSystemProfiles() → a{sa{sv}}`
- `GetUserProfileDefault() → a{sv}` (already renamed in Phase A)
- `ListUserProfiles() → Vec<(String, String)>` (already renamed)

**D.2** Add to `org.tisvlay.Tv1`:
- `SystemProfile` property (readable, writable)
- `SetTvOption` key `"profile.system"` to change assignment + persist

**D.3** Update `DaemonClient`:
- `list_system_profiles()`
- `get_tv_system_profile(tv_id)`
- `set_tv_system_profile(tv_id, name)`
- Update `TvInfo` to include `system_profile`

Checkpoint: `cargo check`, `cargo test`, commit.

---

### Phase E: Session service + tray + CLI updates

**E.1** Session: update `SessionState::load()` for new config shape
(`user.profiles` namespace, `profile.user` assignment).

**E.2** Session D-Bus: methods already renamed in Phase A. Verify
correct behavior with new config shape.

**E.3** Tray: add "System Profile" submenu alongside "User Profile".
Add `TrayCommand::SetSystemProfile`. Query daemon for available
system profiles and current per-TV assignment.

**E.4** CLI: add `tisvlay profile system list/get/set` subcommands.

Checkpoint: `cargo check`, `cargo test`, commit.

---

### Phase F: Docs, configs, final cleanup

- `docs/design.md` — config schema, D-Bus interface, event→action table
- `docs/configs/daemon.toml` → new shape
- `docs/configs/session.toml` → new shape
- `.claude/CLAUDE.md` — project structure, key implementation details
- Verify all test fixtures updated

Checkpoint: `cargo check`, `cargo test`, commit.

---

## Checklist

- [x] A.1: Rename core types (ProfileConfig → UserProfileConfig, etc.)
- [x] A.2: Rename D-Bus interfaces and client methods
- [x] A.3: Rename session service identifiers
- [x] A.4: Rename tray identifiers and labels
- [x] A.5: Rename CLI subcommands
- [x] B.1: Add SystemProfileConfig and ResolvedSystemProfile
- [x] B.2: Restructure SystemConfig (system/user sections)
- [x] B.3: Restructure UserConfig
- [x] B.4: Add TvProfileAssignment to TvConfig
- [x] B.5: Update UserTvConfig profile assignment
- [x] B.6: Add system profile resolution
- [x] B.7: Update config tests
- [x] C.1: Store resolved system profiles in ManagedTv
- [x] C.2: Replace DaemonAction with SystemEvent
- [x] C.3: Per-TV event dispatch
- [x] C.4: Per-TV on_startup
- [x] C.5: Replace SetDaemonOption with SetTvSystemProfile
- [x] C.6: Update logind.rs
- [ ] D.1: System profile D-Bus methods on Manager1
- [ ] D.2: SystemProfile property on Tv1
- [ ] D.3: Update DaemonClient
- [ ] E.1: Session profile resolution for new config shape
- [ ] E.2: Verify session D-Bus with new config
- [ ] E.3: Tray dual profile submenus
- [ ] E.4: CLI system/user profile commands
- [ ] F: Update docs, configs, CLAUDE.md
- [ ] `cargo check` all crates
- [ ] `cargo test` all crates

# Implementation Plan

## Phase 5A: TV `managed` Flag + CLI Profile Parity

### Motivation

Users may have multiple TVs paired with the daemon but only want to actively manage
a subset at any given time (e.g. office TV vs. living room TV when moving between rooms).
Unpairing and re-pairing is too disruptive. A `managed` flag lets users designate which
TVs participate in automation (boot WoL, session events, shutdown) while keeping all
paired TVs available for manual control via the tray and CLI.

Separately, the CLI has no access to the session service and therefore cannot list or
assign profiles — functionality already available in the tray.

---

### Part 0: Tray Scroll Target Tracks Last-Interacted TV

- `crates/tisvlay-tray/src/tray.rs`:
  - Added `scroll_tv_id: Option<String>` to `TisvlayTray`
  - `volume_item` / `backlight_item` accept a `tv_id` parameter; on activation they set
    both `scroll_target` and `scroll_tv_id`
  - `scroll()` resolves target by checking `scroll_tv_id` first (if that TV is still
    connected and on), falling back to the first connected TV
- `crates/tisvlay-tray/src/main.rs`: initialize `scroll_tv_id: None`

---

### Part 1: `managed` Flag

#### 1.1 — Core: `TvConfig` + `TvInfo`

- `crates/tisvlay-core/src/config.rs`:
  - Added `managed: bool` to `TvConfig` (required TOML field, no serde default)
  - Removed `default_vendor()` serde helper and `with_vendor()` builder method
  - `TvConfig::new(id, name, vendor, managed)` — all four fields required;
    no default exists anywhere in the library
  - Struct-level doc updated to describe `managed` and automation semantics
  - Removed stale doc comment on `SystemConfig` referencing the removed `Config` alias
  - `only_when_active` no longer has `#[serde(default)]`; now required in TOML
  - Example TOML fixtures updated to include `only_when_active` and `managed`
- `crates/tisvlay-core/src/dbus_client.rs`: add `managed: bool` to `TvInfo`
  - Populated from the `Managed` D-Bus property in `list_tvs()`

#### 1.1b — Example config updated

- `docs/configs/daemon.toml`: added `on_shutdown`, `on_no_active_session` to `[daemon]`;
  added `managed = true` to the example `[[tv]]` entry

#### 1.2 — Daemon: enforce `managed` in all automation paths

- `crates/tisvlayd/src/tv_manager.rs`:
  - `type TvEntry = (bool, Arc<Mutex<ManagedTv>>)` — managed flag stored alongside the
    per-TV mutex so `list_managed_tv_ids` can scan it with only the outer read lock
  - `tvs` field type changed to `Arc<RwLock<IndexMap<String, TvEntry>>>`
  - Boot WoL loop: checks outer managed flag before acquiring per-TV lock, skipping
    unmanaged TVs entirely
  - `list_managed_tv_ids()` — simple `filter_map` over the outer map, no per-TV lock
  - `pair_tv(name, vendor, vendor_config, id, managed)` — threads `managed` through;
    inserts `(tv_config.managed, Arc::new(...))` into map
  - `remove_tv` — `shift_remove` now destructures `(_, arc)` from the tuple
  - `set_tv_option` — handles `"managed"` key via `apply_tv_option`; after the per-TV
    lock is released, acquires the outer write lock to sync the tuple's bool flag
  - `get_tv_info` — populates `TvInfo.managed` from `tv.config.managed`
  - `reload_config` — now uses `spawn_blocking` + `disk_write_lock` for disk I/O,
    consistent with all other config-load paths; updates the outer managed flag under
    the write lock for existing TVs; correctly destructures `(_, arc)` on removal
  - `apply_tv_option` — handles `"managed"` key → parses `"true"`/`"false"`
- `crates/tisvlayd/src/dbus/tv.rs`: expose `managed` as a D-Bus property on
  `org.tisvlay.Tv1`; defaults to `true` on lookup failure (assume managed)
- `crates/tisvlayd/src/dbus/manager.rs`:
  - `GetTvs` method removed — superseded by `GetManagedObjects` (Phase 5C)
  - `AddTv(name, vendor, config_json, managed)` — added `managed: bool` parameter;
    passes through to `pair_tv`
- `crates/tisvlayd/src/logind.rs`:
  - `execute_action_all_tvs` uses `list_managed_tv_ids()` instead of `list_tv_ids()`

#### 1.3 — Session service: live `managed` filtering

- `crates/tisvlay-session/src/main.rs`:
  - `SessionState::load`: builds profiles for **all** paired TVs (no managed filter at
    load time) so any TV has a ready profile if it becomes managed without a reload
  - `event_handler`: before each dispatch, calls `daemon.list_tvs()` to get the current
    managed TV IDs; fans out only to TVs in that set
  - Fallback on daemon unreachable: returns empty `HashSet`, skipping dispatch entirely
    (TV commands would fail anyway; dispatching to unmanaged TVs produces spurious logs)

#### 1.4 — Tray: visual indicator + toggle

- `crates/tisvlay-tray/src/state.rs` (`CachedTvState`): add `managed: bool`
  - Populated from `TvInfo.managed` in `fetch_all_state()`
- `crates/tisvlay-tray/src/tray.rs`:
  - `build_tv_submenu`: icon conveys managed state — `tvconnected` (managed+connected),
    `tvtrusted` (unmanaged+connected), `tvdisconnected` (disconnected)
  - `build_managed_toggle()` — new helper building "Manage"/"Unmanage" toggle item;
    placed directly below profile submenu in both connected and disconnected menus;
    sends `TrayCommand::SetManaged(String, bool)`
  - Profile submenu icon changed from `preferences-system` to `settings-configure`
  - Input submenu icon changed from `video-television` to `input-tvremote-symbolic`
  - `TrayCommand::SetManaged(String, bool)` added to enum
- `crates/tisvlay-tray/src/main.rs` (`process_commands`):
  - Handle `TrayCommand::SetManaged(id, managed)` → `daemon.set_tv_option(&id, "managed", ...)`
  - Explanatory comment: no session-service notification needed because `set_tv_option`
    updates in-memory state synchronously and the session service queries managed status
    live on each event

#### 1.5 — CLI: `manage` / `unmanage` + `list` managed status

- `crates/tisvlay-cli/src/main.rs`:
  - `Commands::Manage` / `Commands::Unmanage` — use first TV by default, like other
    commands → calls `client.set_tv_option(&id, "managed", "true"/"false")`
  - `Commands::List` output shows `[unmanaged]` suffix for unmanaged TVs
  - `Commands::Status` fetches managed status concurrently via `tokio::try_join!`
  - `Mute` argument changed from free-form `String` to typed `MuteState` ValueEnum
    (`on`/`off`/`toggle`, default `toggle`); free-form parsing and unreachable fallback removed
  - `Input` command refactored from two independent `if` blocks to `match input_id`
  - `Input --list` flag removed — omitting the input ID already implies listing
  - `pair --vendor` default `"lg"` removed; vendor is now a required positional argument
  - `pair --id` dead option removed (daemon assigns IDs automatically)
  - `pair --unmanaged` flag added; `managed = !unmanaged`; UX default (`managed = true`)
    lives exclusively in the CLI, not the library

---

### Part 2: CLI Profile Parity

#### 2.1 — CLI: `SessionClient` integration

- `crates/tisvlay-cli/src/main.rs`:
  - Optionally create a `SessionClient` (always session bus; separate from daemon bus choice)
  - Only required for profile subcommands; other commands proceed without it

#### 2.2 — CLI: `profile` subcommand

Added `Commands::Profile { action: ProfileAction }` with subcommands:

- `tisvlay profile list`
  → `session.list_profiles()` → prints available profile names
- `tisvlay profile get [--tv <id>]`
  → `session.get_tv_profile(&tv_id)` → prints assigned profile name
- `tisvlay profile set <profile> [--tv <id>]`
  → `session.set_tv_profile(&tv_id, &profile)` → confirms assignment

Exits with error message if the session service is unavailable.

---

## Phase 5B: Session Service — Live `managed` Filtering

### Motivation

Phase 5A filtered managed TVs at session-service startup only. Toggling
`managed` at runtime (via tray or CLI) has no effect on the session service
until it reloads (SIGHUP / tray Refresh). Worse, profiles are only resolved
for TVs that were managed at load time — re-managing a TV without a reload
leaves it without a resolved profile.

The daemon is the authority on `managed`. The session service should query
it live at event dispatch time rather than caching a stale snapshot.

### Changes

#### `crates/tisvlay-session/src/main.rs`

- **`SessionState::load`**: no `.filter(|t| t.managed)` — resolves profiles for
  **all** paired TVs so any TV has a ready profile if it becomes managed later
  without a reload.
- **`event_handler`**: before each dispatch, calls `daemon.list_tvs().await` and
  collects the current managed TV IDs into a `HashSet`. Fans out only to TVs in
  that set, using profiles already resolved in `state`. On D-Bus failure, falls
  back to an **empty set** — TV commands would fail anyway, and dispatching to
  unmanaged TVs produces spurious error logs.

### Checklist

- [x] `SessionState::load` builds profiles for all paired TVs
- [x] `event_handler` queries managed set per-event from daemon
- [x] Fallback on daemon failure returns empty set (not full TV list)
- [x] `cargo check --workspace`
- [x] `cargo test --workspace`

---

### Checklist

#### Part 1: `managed` flag

- [x] 1.1 `TvConfig.managed` + `TvInfo.managed` in core
- [x] 1.2 Daemon: WoL, shutdown, `SetTvOption`, `AddTv`, automation all respect `managed`
- [x] 1.3 Session service: live managed filtering per-event; fallback → empty set
- [x] 1.4 Tray: `CachedTvState.managed`, visual indicator, toggle command
- [x] 1.5 CLI: `manage`/`unmanage` commands, `list` managed status, `pair --unmanaged`

#### Part 2: CLI profile parity

- [x] 2.1 `SessionClient` in CLI
- [x] 2.2 `tisvlay profile list / get / set`

---

## Phase 5C: Replace Per-Property Fetches with `ObjectManager`

### Motivation

`list_tvs()` previously took 1 + 6N D-Bus round-trips: one `GetTvs` call to get object
paths, then six individual property fetches per TV (`Id`, `Name`, `Address`, `Vendor`,
`Connected`, `Managed`). `org.freedesktop.DBus.ObjectManager` is the standardized
mechanism for bulk object discovery — used by BlueZ, ModemManager, systemd-logind, and
Rust projects like InputPlumber and stratisd. zbus has zero-code built-in support via
`zbus::fdo::ObjectManager`. Adopting it reduces `list_tvs()` to a single round-trip and
makes the daemon introspectable by standard tools (`busctl`, `d-feet`).

### Changes

#### `crates/tisvlay-core/src/dbus_paths.rs`

- Added `OBJECT_MANAGER_PATH: &str = "/org/tisvlay"` — the common ancestor of
  `/org/tisvlay/Manager` and `/org/tisvlay/Tv/{id}`

#### `crates/tisvlayd/src/main.rs`

- Register `zbus::fdo::ObjectManager` at `/org/tisvlay` before Manager and TV objects,
  so zbus automatically emits `InterfacesAdded`/`InterfacesRemoved` for all subsequent
  `object_server.at()`/`object_server.remove()` calls

#### `crates/tisvlayd/src/dbus/manager.rs`

- Remove `get_tvs()` from `Manager1` — superseded by `GetManagedObjects`

#### `crates/tisvlay-core/src/dbus_client.rs`

- Remove `get_tvs()` from `TisvlayManager` proxy trait
- Add `object_manager: ObjectManagerProxy<'static>` field to `DaemonClient`
- Update `from_connection()` to build the proxy at `OBJECT_MANAGER_PATH`
- Update `is_available()` to use `object_manager.get_managed_objects()`
- Rewrite `list_tvs()`: single `GetManagedObjects` call; filters entries with the
  `org.tisvlay.Tv1` interface; extracts properties via `get_str` / `get_bool` closures
- `get_bool(key, default)` takes an explicit default: `Connected` defaults to `false`
  (assume disconnected); `Managed` defaults to `true` (assume managed — a missing
  property is more likely a transient issue than intentional opt-out)
- Diagnostic observability: property fallbacks are tracked in a shared
  `RefCell<Vec<String>>`; a single `debug!` per TV entry lists all properties that fell
  back to defaults, avoiding per-property log spam; the `id.is_empty()` skip emits
  `warn!` since a daemon-returned object being silently rejected is always actionable

#### `crates/tisvlay-core/tests/dbus_client.rs`

- Updated `MockManager`: removed `get_tvs()`, added `zbus::fdo::ObjectManager` registered
  at `/org/tisvlay` so the mock responds to `GetManagedObjects` with an empty object
  tree (no TV objects → `list_tvs()` returns `vec![]`)

### Checklist

- [x] `OBJECT_MANAGER_PATH` added to `dbus_paths.rs`
- [x] `ObjectManager` registered at `/org/tisvlay` in daemon `main.rs`
- [x] `GetTvs` removed from `Manager1` + proxy
- [x] `DaemonClient` updated: `ObjectManagerProxy` field + rewritten `list_tvs()`
- [x] `get_bool` takes explicit default; `Connected` → `false`, `Managed` → `true`
- [x] Diagnostic logging: `debug!` on property fallbacks, `warn!` on empty `Id`
- [x] Test mock updated to use `ObjectManager`
- [x] `cargo check --workspace`
- [x] `cargo test --workspace`

---

#### Verification

- [x] `cargo check --workspace`
- [x] `cargo test --workspace`
- [ ] Manual: pair two TVs, unmanage one, reboot — confirm WoL only fires for managed TV
- [ ] Manual: tray toggle flips managed state, icon updates on next refresh
- [ ] Manual: `tisvlay profile list/get/set` round-trips correctly

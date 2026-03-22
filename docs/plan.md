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

- `crates/tisvlay-core/src/config.rs`: add `managed: bool` to `TvConfig` (required field, no serde default)
  - Removed `default_vendor()`, `default_managed()` serde helpers; all fields are now required in TOML
  - `TvConfig::new(id, name, vendor)` — vendor is now a required constructor parameter; `with_vendor()` removed
- `crates/tisvlay-core/src/dbus_client.rs`: add `managed: bool` to `TvInfo`
  - Populated via `org.tisvlay.Tv1` `managed` D-Bus property

#### 1.1b — Example config updated

- `docs/configs/daemon.toml`: added `on_shutdown`, `on_no_active_session` to `[daemon]`;
  added `managed = true` to the example `[[tv]]` entry

#### 1.2 — Daemon: enforce `managed` in all automation paths

- `crates/tisvlayd/src/tv_manager.rs`:
  - Boot WoL loop: skip TVs where `managed = false`
  - `on_shutdown` action: skip unmanaged TVs via `list_managed_tv_ids()`
  - Added `list_managed_tv_ids()` — returns IDs where `config.managed == true`
- `crates/tisvlayd/src/dbus/tv.rs`: expose `managed` as a D-Bus property on `org.tisvlay.Tv1`
- `crates/tisvlayd/src/dbus/manager.rs`:
  - `SetTvOption`: handles `managed` key → parses `"true"`/`"false"` → updates config + reloads
- `crates/tisvlayd/src/logind.rs`:
  - `execute_action_all_tvs` uses `list_managed_tv_ids()` instead of `list_tv_ids()`

#### 1.3 — Session service: filter to managed TVs

- `crates/tisvlay-session/src/main.rs` (`SessionState::load`):
  - Filter `tv_ids` to only those where `tv_info.managed == true` before building profiles

#### 1.4 — Tray: visual indicator + toggle

- `crates/tisvlay-tray/src/state.rs` (`CachedTvState`): add `managed: bool`
  - Populated from `TvInfo.managed` in `fetch_all_state()`
- `crates/tisvlay-tray/src/tray.rs`:
  - `build_tv_submenu`: icon conveys managed state — `tvconnected` (managed+connected),
    `tvtrusted` (unmanaged+connected), `tvdisconnected` (disconnected)
  - `build_connected_tv_menu` / `build_disconnected_tv_menu`: "Manage" / "Unmanage" toggle
    placed directly below the profile submenu (no separator); sends `TrayCommand::SetManaged(String, bool)`
- `crates/tisvlay-tray/src/main.rs` (`process_commands`):
  - Handle `TrayCommand::SetManaged(id, managed)` → `daemon.set_tv_option(&id, "managed", &managed.to_string())`

#### 1.5 — CLI: `manage` / `unmanage` + `list` managed status

- `crates/tisvlay-cli/src/main.rs`:
  - `Commands::Manage` / `Commands::Unmanage` — use first TV by default, like other commands
    → calls `client.set_tv_option(&id, "managed", "true"/"false")`
  - `Commands::List` output shows `[unmanaged]` suffix for unmanaged TVs
  - `Commands::Status` fetches managed status concurrently via `tokio::try_join!`
  - `Mute` argument changed from free-form string to typed `MuteState` ValueEnum (`on`/`off`/`toggle`, default `toggle`)
  - `Input --list` flag removed — omitting the input ID already implies listing
  - `pair --vendor` default `"lg"` removed; vendor is now required
  - `pair --id` dead option removed (daemon assigns IDs automatically)

---

### Part 2: CLI Profile Parity

#### 2.1 — CLI: `SessionClient` integration

- `crates/tisvlay-cli/src/main.rs`:
  - Optionally create a `SessionClient` (always session bus; separate from daemon bus choice)
  - Only required for profile subcommands; other commands proceed without it

#### 2.2 — CLI: `profile` subcommand

Add `Commands::Profile { action: ProfileAction }` with subcommands:

- `tisvlay profile list`
  → `session.list_profiles()` → print `(name, json)` pairs (name only, or with summary)
- `tisvlay profile get [--tv <id>]`
  → `session.get_tv_profile(&tv_id)` → print assigned profile name
- `tisvlay profile set <profile> [--tv <id>]`
  → `session.set_tv_profile(&tv_id, &profile)` → confirm assignment

Error gracefully if the session service is unavailable (it's a user service, not system).

---

### Checklist

#### Part 1: `managed` flag

- [x] 1.1 `TvConfig.managed` + `TvInfo.managed` in core
- [x] 1.2 Daemon: WoL, shutdown, `SetTvOption`, `GetTvs` all respect `managed`
- [x] 1.3 Session service: filter `tv_ids` to managed-only
- [x] 1.4 Tray: `CachedTvState.managed`, visual indicator, toggle command
- [x] 1.5 CLI: `manage`/`unmanage` commands, `list` managed status

#### Part 2: CLI profile parity

- [x] 2.1 `SessionClient` in CLI
- [x] 2.2 `tisvlay profile list / get / set`

#### Verification

- [x] `cargo check --workspace`
- [x] `cargo test --workspace`
- [ ] Manual: pair two TVs, unmanage one, reboot — confirm WoL only fires for managed TV
- [ ] Manual: tray toggle flips managed state, icon updates on next refresh
- [ ] Manual: `tisvlay profile list/get/set` round-trips correctly

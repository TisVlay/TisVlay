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

### Part 1: `managed` Flag

#### 1.1 — Core: `TvConfig` + `TvInfo`

- `crates/tisvlay-core/src/config.rs`: add `managed: bool` to `TvConfig`
  - `#[serde(default = "default_true")]` so existing configs without the field stay managed
- `crates/tisvlay-core/src/dbus_client.rs`: add `managed: bool` to `TvInfo`
  - Populated from the daemon's D-Bus `GetTvs` response

#### 1.2 — Daemon: enforce `managed` in all automation paths

- `crates/tisvlayd/src/tv_manager.rs`:
  - Boot WoL loop: skip TVs where `managed = false`
  - `on_shutdown` action: skip unmanaged TVs
  - D-Bus `GetTvs` response: include `managed` field in returned structs
- `crates/tisvlayd/src/dbus/manager.rs`:
  - `SetTvOption`: wire up `managed` key → parse `"true"`/`"false"` → update config + reload
- `crates/tisvlayd/src/dbus/` (session event dispatch):
  - Any path that fans out events to all TVs must filter to managed-only

#### 1.3 — Session service: filter to managed TVs

- `crates/tisvlay-session/src/main.rs` (`SessionState::load`):
  - Filter `tv_ids` to only those where `tv_info.managed == true` before building profiles

#### 1.4 — Tray: visual indicator + toggle

- `crates/tisvlay-tray/src/state.rs` (`CachedTvState`): add `managed: bool`
  - Populated from `TvInfo.managed` in `fetch_all_state()`
- `crates/tisvlay-tray/src/tray.rs`:
  - `build_tv_submenu`: use a distinct icon or `[unmanaged]` label suffix for unmanaged TVs
  - `build_connected_tv_menu` / `build_disconnected_tv_menu`: add a "Managed" toggle
    checkbox item that sends a new `TrayCommand::SetManaged(String, bool)`
- `crates/tisvlay-tray/src/main.rs` (`process_commands`):
  - Handle `TrayCommand::SetManaged(id, managed)` → `daemon.set_tv_option(&id, "managed", &managed.to_string())`

#### 1.5 — CLI: `manage` / `unmanage` + `list` managed status

- `crates/tisvlay-cli/src/main.rs`:
  - Add `Commands::Manage { tv_id: Option<String> }` and `Commands::Unmanage { tv_id: Option<String> }`
    (tv_id optional: defaults to first TV like other commands)
    → calls `client.set_tv_option(&id, "managed", "true"/"false")`
  - Update `Commands::List` output to show `[managed]` / `[unmanaged]`
- `crates/tisvlay-core/src/dbus_client.rs`: add `set_tv_option(tv_id, key, value)` method if not already exposed

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

- [ ] `cargo check --workspace`
- [ ] `cargo test --workspace`
- [ ] Manual: pair two TVs, unmanage one, reboot — confirm WoL only fires for managed TV
- [ ] Manual: tray toggle flips managed state, menu updates on next refresh
- [ ] Manual: `tisvlay profile list/get/set` round-trips correctly

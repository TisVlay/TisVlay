# Refactor: Move sleep/wake from user profiles to system profiles

## Motivation

`on_sleep` and `on_wake` are currently in the user profile, dispatched by
the session service via `org.freedesktop.login1.Manager.PrepareForSleep`.
But sleep and wake are system-level events that affect the entire desktop
and all users simultaneously — they belong alongside `on_startup`,
`on_shutdown`, and `on_reboot` in the system profile.

With the per-TV system profile architecture now in place, admins can
express per-TV sleep/wake behavior without needing the user profile layer.

## Design

Move `on_sleep` and `on_wake` from `UserProfileConfig` / `ResolvedUserProfile`
to `SystemProfileConfig` / `ResolvedSystemProfile`.

The daemon's `logind.rs` already subscribes to logind signals. Add
`PrepareForSleep` handling there (like `PrepareForShutdown`), dispatching
per-TV via `SystemEvent::Sleep` / `SystemEvent::Wake`.

Remove `PrepareForSleep` handling from the session service's
`session_monitor.rs`.

## Changes

### Part 1: Move fields between profile types

**File:** `crates/tisvlay-core/src/config.rs`

- Remove from `UserProfileConfig`: `on_sleep`, `on_wake`
- Remove from `ResolvedUserProfile`: `on_sleep`, `on_wake`
- Add to `SystemProfileConfig`: `on_sleep`, `on_wake`
- Add to `ResolvedSystemProfile`: `on_sleep`, `on_wake`
- Update `full_default()`, `merge_with_base()`, `resolve()` on both types
- Defaults: `on_sleep = PowerOff`, `on_wake = PowerOn` (same as before)

### Part 2: Add Sleep/Wake to SystemEvent and daemon dispatch

**File:** `crates/tisvlayd/src/tv_manager.rs`

- Add `Sleep` and `Wake` variants to `SystemEvent`
- Update `get_system_action()` to handle the new variants

### Part 3: Add PrepareForSleep to daemon's logind.rs

**File:** `crates/tisvlayd/src/logind.rs`

- Subscribe to `PrepareForSleep` signal (alongside existing `PrepareForShutdown`)
- `PrepareForSleep(true)` → `execute_system_event(SystemEvent::Sleep, ...)`
- `PrepareForSleep(false)` → `execute_system_event(SystemEvent::Wake, ...)`
- No inhibitor lock needed for sleep (sleep is fast; TVs can handle it async)

### Part 4: Remove sleep/wake from session service

**File:** `crates/tisvlay-session/src/session_monitor.rs`

- Remove `PrepareForSleep` subscription
- Remove `SessionEvent::PrepareForSleep` and `SessionEvent::ResumeFromSleep`

**File:** `crates/tisvlay-session/src/main.rs`

- Remove the `PrepareForSleep` / `ResumeFromSleep` match arms in
  `get_action_for_event()`

### Part 5: Update all config examples, tests, and docs

- `docs/configs/daemon.toml`: add `on_sleep` / `on_wake` to system profile
- `docs/configs/session.toml`: remove `on_sleep` / `on_wake` from user profile
- `docs/design.md`: update event→action table, config schema, D-Bus section
- `.claude/CLAUDE.md`: update if any references
- All TOML test fixtures in `config.rs`
- `session_dbus.rs`: remove `on_sleep`/`on_wake` from `GetEffectiveUserProfile`

---

## Checklist

- [x] 1: Move on_sleep/on_wake fields between profile types
- [x] 2: Add Sleep/Wake to SystemEvent
- [x] 3: Add PrepareForSleep handling to daemon logind.rs
- [x] 4: Remove sleep/wake from session service
- [x] 5: Update configs, tests, docs
- [x] `cargo check` all crates
- [x] `cargo test` all crates

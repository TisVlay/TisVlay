# Extract `StateWatcher` into tisvlay-core

Move D-Bus service lifecycle watching and signal subscription logic from
`tisvlay-tray` into `tisvlay-core` so both the tray and future GUI share
a single implementation.

## Status: Complete

All steps implemented and passing `prek run -a`.

## Motivation

The tray's `watch_state()` (~175 lines) contained D-Bus wiring that is
**not ksni-specific** and would be duplicated by `tisvlay-gui`:

1. Subscribe to daemon `PropertiesChanged` / `ObjectManager` / `Manager1` signals
2. Watch daemon name appear/disappear on the correct bus (system vs session)
3. Watch session service name appear/disappear on the session bus
4. Subscribe to session `UserProfileChanged` signals
5. Debounce all signals within a 100ms window
6. Fallback poll every 30s

The only tray-specific parts were the `handle.update()` / `refresh_state()`
callbacks in each select branch.

## Design

### `StateEvent` enum

Tells the consumer *what happened*, not *what to do*:

```rust
pub enum StateEvent {
    DaemonStateChanged(TvStateChange),
    DaemonAppeared,
    DaemonDisappeared,
    SessionAppeared,
    SessionDisappeared,
    SessionProfileChanged,
    FallbackPoll,
}
```

### `StateWatcher` struct

```rust
pub struct StateWatcher { /* streams + config */ }

impl StateWatcher {
    pub async fn new(daemon: &DaemonClient, config: StateWatcherConfig) -> Result<Self>;
    pub async fn next(&mut self) -> Option<StateEvent>;
}
```

`next()` uses `tokio::select!` + debounce internally, returning the
`StateEvent` instead of performing UI actions.

### `StateWatcherConfig`

```rust
pub struct StateWatcherConfig {
    pub signal_debounce: Duration,       // default 100ms
    pub fallback_poll_interval: Duration, // default 30s
}
```

## What was implemented

### Step 1: Session constants + `connection()` accessor -- done

- Added `SESSION_SERVICE`, `SESSION_INTERFACE`, `SESSION_PATH` to `dbus_paths.rs`
- Added `pub fn connection(&self) -> &zbus::Connection` to `DaemonClient`
- Exported new constants from `lib.rs`

### Step 2: `StateEvent`, `StateWatcherConfig`, `StateWatcher` -- done

- Defined types in `dbus_client.rs` (behind `dbus` feature gate)
- Lifted setup from tray `watch_state()` into `StateWatcher::new()`
- Lifted select loop into `StateWatcher::next()`, replacing consumer
  actions with `return Some(StateEvent::...)`
- Debounce logic moved verbatim
- Exported from `lib.rs`

### Step 3: Rewrite tray `watch_state()` -- done

- Replaced ~175-line function with ~45-line `StateWatcher` consumer loop
- Removed `user_mode` parameter (derived from daemon connection)
- Removed `SIGNAL_DEBOUNCE` and `FALLBACK_POLL_INTERVAL` constants
- Dropped direct `zbus` and `futures-util` dependencies from tray

## What did NOT move (by design)

- **`SessionClient` lifecycle** -- consumers own it differently
  (`Arc<RwLock<Option<SessionClient>>>` in the tray; GUI may differ).
  The watcher emits `SessionAppeared`/`SessionDisappeared`; the consumer
  decides how to manage the client.

- **`CachedTvState` / `fetch_all_state()`** -- presentation-layer concern.
  Can be extracted later if tray and GUI converge on the same state shape.

## Files changed

| File | Change |
|------|--------|
| `crates/tisvlay-core/src/dbus_paths.rs` | Added session constants |
| `crates/tisvlay-core/src/dbus_client.rs` | Added `connection()`, `StateEvent`, `StateWatcherConfig`, `StateWatcher` |
| `crates/tisvlay-core/src/lib.rs` | Exported new types and constants |
| `crates/tisvlay-tray/src/main.rs` | Rewrote `watch_state()`, removed `user_mode`, cleaned imports |
| `crates/tisvlay-tray/Cargo.toml` | Removed direct `zbus` and `futures-util` dependencies |
| `crates/tisvlayd/src/tv_manager.rs` | Fixed misplaced doc comment on `connect_all_on_restart`/`on_startup` |

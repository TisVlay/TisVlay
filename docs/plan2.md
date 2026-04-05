# Extract `StateWatcher` into tisvlay-core

Move D-Bus service lifecycle watching and signal subscription logic from
`tisvlay-tray` into `tisvlay-core` so both the tray and future GUI share
a single implementation.

## Motivation

The tray's `watch_state()` (~175 lines) contains D-Bus wiring that is
**not ksni-specific** and will be duplicated by `tisvlay-gui`:

1. Subscribe to daemon `PropertiesChanged` / `ObjectManager` / `Manager1` signals
2. Watch daemon name appear/disappear on the correct bus (system vs session)
3. Watch session service name appear/disappear on the session bus
4. Subscribe to session `UserProfileChanged` signals
5. Debounce all signals within a 100ms window
6. Fallback poll every 30s

The only tray-specific parts are the `handle.update()` / `refresh_state()`
callbacks in each select branch.

## Design

### `StateEvent` enum

Tells the consumer *what happened*, not *what to do*:

```rust
pub enum StateEvent {
    /// Debounced daemon property/object signals. Inner value is the
    /// first signal in the debounce window (useful for logging).
    DaemonStateChanged(TvStateChange),
    /// Daemon well-known name appeared on D-Bus.
    DaemonAppeared,
    /// Daemon well-known name disappeared from D-Bus.
    DaemonDisappeared,
    /// Session service appeared on D-Bus.
    SessionAppeared,
    /// Session service disappeared from D-Bus.
    SessionDisappeared,
    /// Session UserProfileChanged signal received.
    SessionProfileChanged,
    /// Fallback poll timer fired.
    FallbackPoll,
}
```

### `StateWatcher` struct

```rust
pub struct StateWatcher { /* streams + config */ }

impl StateWatcher {
    /// Set up all subscriptions. Uses `daemon.connection()` to derive
    /// the correct bus, eliminating the `user_mode: bool` parameter.
    pub async fn new(
        daemon: &DaemonClient,
        config: StateWatcherConfig,
    ) -> Result<Self>;

    /// Receive the next event. Debounce and fallback poll are internal.
    pub async fn next(&mut self) -> Option<StateEvent>;
}
```

`next()` uses the same `tokio::select!` + debounce pattern as the current
tray code, but returns the `StateEvent` instead of performing UI actions.

### `StateWatcherConfig`

```rust
pub struct StateWatcherConfig {
    pub signal_debounce: Duration,       // default 100ms
    pub fallback_poll_interval: Duration, // default 30s
}
```

## Enabling changes

### `DaemonClient::connection()` accessor

`StateWatcher::new()` needs the raw `zbus::Connection` to call
`watch_service()` on the correct bus. One-line addition. Also eliminates
the `user_mode: bool` parameter — the connection already knows its bus.

### Session path constants in `dbus_paths.rs`

Currently hardcoded in the tray. Add to `dbus_paths.rs`:

```rust
pub const SESSION_SERVICE: &str = "org.tisvlay.Session";
pub const SESSION_INTERFACE: &str = "org.tisvlay.Session1";
pub const SESSION_PATH: &str = "/org/tisvlay/Session";
```

### Lifetime: `'static` streams

`subscribe_state_changes()` returns `impl Stream + '_` (borrows `&self`).
Rather than changing that API, `StateWatcher::new()` does the signal
subscription directly using `daemon.connection()`, producing `'static`
streams that the struct can own.

## What does NOT move

- **`SessionClient` lifecycle** — consumers own it differently
  (`Arc<RwLock<Option<SessionClient>>>` in the tray; GUI may differ).
  The watcher emits `SessionAppeared`/`SessionDisappeared`; the consumer
  decides how to manage the client.

- **`CachedTvState` / `fetch_all_state()`** — presentation-layer concern.
  Can be extracted later if tray and GUI converge on the same state shape.

## Implementation steps

### Step 1: Session constants + `connection()` accessor

- Add `SESSION_SERVICE`, `SESSION_INTERFACE`, `SESSION_PATH` to `dbus_paths.rs`
- Add `pub fn connection(&self) -> &zbus::Connection` to `DaemonClient`
- Export new constants from `lib.rs`

### Step 2: `StateEvent`, `StateWatcherConfig`, `StateWatcher`

- Define types in `dbus_client.rs` (behind `dbus` feature gate)
- Lift setup from tray `watch_state()` lines 198-267 into `StateWatcher::new()`
- Lift select loop from lines 269-365 into `StateWatcher::next()`,
  replacing consumer actions with `return Some(StateEvent::...)`
- Debounce logic moves verbatim
- Export from `lib.rs`

### Step 3: Rewrite tray `watch_state()`

- Replace ~175-line function with ~40-line `StateWatcher` consumer loop
- Remove `user_mode` parameter (derived from daemon connection)
- Clean up imports; evaluate dropping direct `zbus` dep from tray

## Result

Tray's `watch_state()` after extraction:

```rust
async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
) {
    let mut watcher = match StateWatcher::new(&daemon, Default::default()).await {
        Ok(w) => w,
        Err(e) => {
            warn!(error = %e, "Failed to create state watcher");
            return;
        }
    };

    loop {
        if handle.is_closed() { break; }
        let Some(event) = watcher.next().await else { break };

        match event {
            StateEvent::DaemonStateChanged(_)
            | StateEvent::DaemonAppeared
            | StateEvent::SessionProfileChanged
            | StateEvent::FallbackPoll => {
                refresh_state(&daemon, &session, &handle).await;
            }
            StateEvent::DaemonDisappeared => {
                handle.update(|t| {
                    t.daemon_available = false;
                    t.tvs.clear();
                }).await;
            }
            StateEvent::SessionAppeared => {
                if let Ok(client) = SessionClient::new().await {
                    if client.is_available().await {
                        *session.write().await = Some(client);
                        refresh_state(&daemon, &session, &handle).await;
                    }
                }
            }
            StateEvent::SessionDisappeared => {
                *session.write().await = None;
                refresh_state(&daemon, &session, &handle).await;
            }
        }
    }
}
```

## Files changed

| File | Change |
|------|--------|
| `crates/tisvlay-core/src/dbus_paths.rs` | Add session constants |
| `crates/tisvlay-core/src/dbus_client.rs` | Add `connection()`, `StateEvent`, `StateWatcherConfig`, `StateWatcher` |
| `crates/tisvlay-core/src/lib.rs` | Export new types and constants |
| `crates/tisvlay-tray/src/main.rs` | Rewrite `watch_state()`, remove `user_mode`, clean imports |
| `crates/tisvlay-tray/Cargo.toml` | Potentially remove direct `zbus` dependency |

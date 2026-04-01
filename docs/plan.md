# Refactor: Push-based tray updates via D-Bus signals

## Motivation

The tray currently polls the daemon every 5 seconds via `fetch_all_state()`
(which calls `list_tvs()` + `get_state()` per TV + `list_system_profiles()`
+ `list_user_profiles()` + `get_tv_user_profile()` per TV). This causes:

1. **Up to 5 seconds of stale data** after CLI commands, session events,
   or profile switches
2. **Wasted D-Bus round-trips** when nothing changed (the common case)
3. **Complex shadow cache + BTreeMap workarounds** to avoid ksni menu
   rebuilds from identical polls
4. **Per-field diff logging** that exists only to debug poll artifacts

The daemon already emits D-Bus signals when state changes:
- `StateChanged(state: s)` on `org.tisvlay.Tv1` per-TV objects
- `ConnectionChanged(connected: b)` on `org.tisvlay.Tv1` per-TV objects
- `InterfacesAdded`/`InterfacesRemoved` on the ObjectManager (when TVs
  are added/removed via pairing)

The tray should subscribe to these signals instead of polling.

## Design

### Signal-driven update model2. Push-based state instead of polling (bigger change, eliminates the poll entirely)
2. Push-based state instead of polling (bigger change, eliminates the poll entirely)

The daemon already emits TvStateChanged signals on the D-Bus. Instead of polling every 5 seconds and comparing, the tray could subscribe to those signals and only call handle.update() when the daemon says something changed. This eliminates the poll loop, the shadow cache, and the ordering issue all at once. But it's a bigger architectural change — the tray would need signal subscription logic.
The daemon already emits TvStateChanged signals on the D-Bus. Instead of polling every 5 seconds and comparing, the tray could subscribe to those signals and only call handle.update() when the daemon says something changed. This eliminates the poll loop, the shadow cache, and the ordering issue all at once. But it's a bigger architectural change — the tray would need signal subscription logic.

Replace the 5-second poll loop with D-Bus signal subscriptions:

1. **Subscribe once** at startup to:
   - `StateChanged` on all TV objects (wildcard path match on
     `/org/tisvlay/Tv/*`)
   - `ConnectionChanged` on all TV objects
   - `InterfacesAdded`/`InterfacesRemoved` on `/org/tisvlay` (TV
     added/removed)
   - `PropertiesChanged` on all TV objects (for Managed, SystemProfile
     changes via SetTvOption)

2. **On any signal**: do a targeted refresh of the affected TV's state
   (or full refresh for structural changes like TV add/remove), then
   call `handle.update()`.

3. **Keep a slow background poll as a fallback** (e.g., 30-60 seconds)
   for resilience against missed signals (D-Bus signals are not
   guaranteed delivery if the bus is congested). This is much less
   aggressive than the current 5-second poll.

### What gets removed

- `POLL_INTERVAL = 5s` constant (replaced by fallback interval)
- Shadow cache (`last_tvs`, `last_daemon_available`)
- Per-field diff logging (no longer needed — updates are event-driven)
- The `PartialEq` derive on `CachedTvState` (no longer needed for
  comparison, but harmless to keep for tests)

### What stays

- `fetch_all_state()` — still used for the initial fetch and fallback
  poll, just called less frequently
- `refresh_state()` — still used after tray commands (scroll, power,
  profile switch) for immediate feedback
- `BTreeMap` for profiles — good practice regardless of polling
- Eager initial state fetch — still needed for first render

---

## Part 1: Add signal subscription to DaemonClient

**File:** `crates/tisvlay-core/src/dbus_client.rs`

Add a `TvProxy` definition for `org.tisvlay.Tv1` with signal receivers:

```rust
#[proxy(
    interface = "org.tisvlay.Tv1",
    default_service = "org.tisvlay.Daemon",
)]
trait TisvlayTv {
    #[zbus(signal)]
    fn state_changed(&self, state: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    fn connection_changed(&self, connected: bool) -> zbus::Result<()>;
}
```

Add a method on `DaemonClient` that returns a stream of state change
events:

```rust
pub async fn subscribe_state_changes(&self) -> Result<impl Stream<Item = TvStateChange>> {
    // Use MessageStream on the connection with a match rule for
    // org.tisvlay.Tv1 signals on any path under /org/tisvlay/Tv/
}
```

The `TvStateChange` enum:
```rust
pub enum TvStateChange {
    /// A TV's state changed (volume, input, power, etc.)
    StateChanged { tv_id: String, state_json: String },
    /// A TV's connection status changed
    ConnectionChanged { tv_id: String, connected: bool },
    /// A TV was added or removed (requires full refresh)
    TvListChanged,
    /// A TV property changed (Managed, SystemProfile, etc.)
    PropertyChanged { tv_id: String },
}
```

### Alternative: use zbus match rules directly

Instead of per-TV proxy subscriptions (which would require creating a
proxy for each TV), use a single `MessageStream` on the connection with
a broad match rule:

```rust
connection.add_match_rule(
    zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.tisvlay.Tv1")?
        .build()
).await?;
```

This captures all Tv1 signals across all TV object paths. Parse the
object path to extract the TV ID.

---

## Part 2: Add signal watcher task in tray

**File:** `crates/tisvlay-tray/src/main.rs`

Replace the tight poll loop with a signal-driven watcher:

```rust
async fn watch_state(
    daemon: Arc<DaemonClient>,
    session: Arc<RwLock<Option<SessionClient>>>,
    handle: ksni::Handle<TisvlayTray>,
) {
    let mut signal_stream = match daemon.subscribe_state_changes().await {
        Ok(stream) => stream,
        Err(e) => {
            warn!("Failed to subscribe to state signals, falling back to polling");
            // Fall back to polling
            return poll_state_fallback(daemon, session, handle).await;
        }
    };

    loop {
        tokio::select! {
            Some(change) = signal_stream.next() => {
                debug!(change = ?change, "State signal received");
                refresh_state(&daemon, &session, &handle).await;
            }
            _ = tokio::time::sleep(FALLBACK_POLL_INTERVAL) => {
                // Periodic fallback poll for resilience
                refresh_state(&daemon, &session, &handle).await;
            }
        }
    }
}
```

The `FALLBACK_POLL_INTERVAL` would be 30-60 seconds (vs current 5s).

### Debouncing

Multiple signals can arrive in rapid succession (e.g., volume change
triggers both a StateChanged and a PropertiesChanged). Debounce by
coalescing signals within a short window (e.g., 100ms) before
refreshing:

```rust
loop {
    // Wait for first signal
    let change = signal_stream.next().await;

    // Drain any additional signals that arrive within 100ms
    let deadline = tokio::time::sleep(Duration::from_millis(100));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            Some(_) = signal_stream.next() => continue,
            _ = &mut deadline => break,
        }
    }

    // Now refresh once for all coalesced signals
    refresh_state(&daemon, &session, &handle).await;
}
```

---

## Part 3: Simplify tray main.rs

**File:** `crates/tisvlay-tray/src/main.rs`

- Replace `poll_state()` with `watch_state()`
- Remove `POLL_INTERVAL` constant, add `FALLBACK_POLL_INTERVAL`
- Remove shadow cache variables (`last_tvs`, `last_daemon_available`)
- Remove per-field diff logging (the debug instrumentation)
- Keep `refresh_state()` for command-triggered updates
- Keep eager initial state fetch
- Keep lazy session service connection (retry on fallback poll)

---

## Part 4: Session service signal handling (optional enhancement)

The session service also affects tray state (user profile changes).
Currently the tray queries the session service during `fetch_all_state()`.
For full push-based updates, the tray could also subscribe to session
service D-Bus signals. However, session profile changes are rare (user
manually switches), so the fallback poll handles this adequately.

If needed later:
- Add a `ProfileChanged` signal to `org.tisvlay.Session1`
- Subscribe in the tray alongside daemon signals

---

## Part 5: Daemon signal completeness audit

Verify the daemon emits signals for all state changes the tray cares
about. Currently emitted:

| Event | Signal | Emitted? |
|-------|--------|----------|
| TV connects/disconnects | `ConnectionChanged` | Yes |
| TV state changes (vol, input, etc.) | `StateChanged` | Yes (via `emit_state_change`) |
| TV added (pairing) | `InterfacesAdded` (ObjectManager) | Yes (automatic from zbus) |
| TV removed | `InterfacesRemoved` (ObjectManager) | Yes |
| Managed flag toggled | `PropertiesChanged`? | Needs check |
| System profile changed | `PropertiesChanged`? | Needs check |
| Config reloaded | ? | May need a new signal |

If `SetTvOption` changes (managed, profile.system) don't emit
`PropertiesChanged`, the tray won't know. Check whether zbus
auto-emits property changes, or if manual signal emission is needed.

Note: zbus properties with `emits_changed_signal = "false"` (currently
set on Managed and SystemProfile in tv.rs) explicitly suppress
automatic property change signals. These would need to be changed to
emit, or we emit a manual `StateChanged` signal when these options
change.

---

## Part 6: Update docs

- `.claude/CLAUDE.md`: note push-based tray architecture
- `docs/design.md`: update tray description from polling to signal-driven
- Remove poll-related comments and shadow cache docs

---

## Checklist

- [ ] Part 1: Add signal subscription to DaemonClient
- [ ] Part 2: Signal watcher task in tray with debouncing
- [ ] Part 3: Simplify tray main.rs (remove poll artifacts)
- [ ] Part 4: (Optional) Session service ProfileChanged signal
- [ ] Part 5: Daemon signal completeness audit
- [ ] Part 6: Update docs
- [ ] `cargo check` all crates
- [ ] `cargo test` all crates

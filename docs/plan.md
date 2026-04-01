# Refactor: End-to-end push-based state updates

## Motivation

State updates currently flow through two polling layers:

1. **Tray → Daemon**: The tray polls the daemon every 5 seconds via
   `fetch_all_state()` (multiple D-Bus round-trips per TV).
2. **Daemon → TV**: The daemon only queries TV state on demand — when
   `get_state()` is called. If the user changes volume with the TV
   remote, the daemon doesn't know until something asks.

This causes up to 5 seconds of stale data, wasted D-Bus round-trips,
and complex workarounds (shadow cache, BTreeMap ordering, per-field
diff logging) to avoid ksni menu rebuilds from identical polls.

## Goal

Full push-based pipeline:

```
TV remote/app → LG WebSocket push → LG backend → daemon StateChangeEvent
    → D-Bus signal → tray handle.update()
```

Every layer notifies the next instead of being polled.

## Architecture

### Layer 1: Backend state subscriptions (TV → daemon)

The LG SSAP protocol supports `subscribe` requests — the same URIs
used for `request` (GET) commands, but sent with `msg_type: "subscribe"`.
The TV pushes updates over the existing WebSocket whenever the value
changes. The Python `aiowebostv` library demonstrates this pattern:

```python
subscribe_state_updates = {
    self.subscribe_power_state(self.set_power_state),
    self.subscribe_current_app(self.set_current_app_state),
    self.subscribe_muted(self.set_muted_state),
    self.subscribe_volume(self.set_volume_state),
    self.subscribe_apps(self.set_apps_state),
    self.subscribe_inputs(self.set_inputs_state),
    self.subscribe_sound_output(self.set_sound_output_state),
}
```

Our `protocol.rs` already has `RequestType::Subscribe` and
`Request::subscribe()` — marked `#[allow(dead_code)]`, unused.

**Subscribable SSAP endpoints** (from `endpoints.rs` + `aiowebostv`):
- `ssap://com.webos.service.tvpower/power/getPowerState` (power state)
- `ssap://audio/getVolume` (volume)
- `ssap://audio/getMute` (mute)
- `ssap://tv/getExternalInputList` (available inputs)
- `ssap://com.webos.applicationManager/getForegroundAppInfo` (current input/app)
- `ssap://audio/getSoundOutput` (sound output mode)

~~Backlight was assumed not subscribable~~ — **UPDATE**: live probing
confirmed `ssap://settings/getSystemSettings` with `category: "picture",
keys: ["backlight"]` supports subscriptions and pushes real-time updates.
Backlight is now included in the LG subscription list alongside power,
volume, mute, and inputs. Writing backlight still requires the Luna
`createAlert` workaround (all direct setSystemSettings methods return
401 insufficient permissions).

### Layer 2: Daemon D-Bus signals (daemon → tray)

The daemon already emits signals:
- `StateChanged(state: s)` on `org.tisvlay.Tv1` per-TV objects
- `ConnectionChanged(connected: b)` on `org.tisvlay.Tv1`
- `InterfacesAdded/Removed` on ObjectManager (TV add/remove)

These need to be emitted whenever the backend pushes state, not just
on explicit commands.

### Layer 3: Tray signal subscription (tray → ksni)

The tray subscribes to daemon D-Bus signals instead of polling. On any
signal, it does a targeted refresh and calls `handle.update()`.

### Vendor-agnostic design

Not all vendors will support push-based state. The `TvConnection` trait
gets an optional `subscribe_state()` method:

```rust
async fn subscribe_state(&mut self) -> Result<mpsc::UnboundedReceiver<TvState>> {
    Err(Error::not_supported("subscribe_state", Capabilities::empty()))
}
```

Vendors that support push (LG) implement it. The daemon checks: if
subscriptions are available, use them; otherwise, fall back to periodic
polling within the daemon per-TV (not from the tray). This keeps the
tray always signal-driven regardless of backend capability.

---

## Part 1: LG backend — WebSocket state subscriptions

### 1.1 Add subscription support to WebOsClient

**File:** `crates/tisvlay-lg/src/client.rs`

Add a `subscribe()` method that sends a subscribe request and returns
the initial response:

```rust
pub async fn subscribe(&mut self, uri: &str) -> Result<Response> {
    let request = Request::subscribe(self.next_id(), uri);
    // ... send and recv initial response
}
```

Remove `#[allow(dead_code)]` from `RequestType::Subscribe` and
`Request::subscribe()`.

### 1.2 Add background message receiver to LgConnection

**File:** `crates/tisvlay-lg/src/backend.rs`

Currently `LgConnection` owns the `WebOsClient` which owns the
WebSocket. Subscription messages arrive asynchronously on the same
WebSocket. The connection needs a background task that:

1. After connecting and registering, sends subscribe requests for
   power, volume, mute, inputs, and current input
2. Continuously reads the WebSocket for incoming messages
3. Routes subscription updates to a state change channel
4. Routes command responses to the requesting command

This is a significant refactor of the connection model — currently
`WebOsClient` uses synchronous request/response (send, then recv
matching ID). With subscriptions, the WebSocket becomes multiplexed:
subscription updates and command responses arrive interleaved.

**Design options:**

A. **Split reader/writer**: Spawn a background task that reads the
   WebSocket and dispatches messages by type. Commands use a
   oneshot channel to receive their response. Subscription updates
   go to a broadcast channel.

B. **Polling model**: After each command, drain any pending subscription
   messages from the WebSocket before returning. Simpler but means
   subscription updates only arrive when commands are being sent.

Option A is more correct — subscription updates arrive in real time.

### 1.3 Implement TvConnection::subscribe_state()

**File:** `crates/tisvlay-lg/src/backend.rs`

The LG implementation subscribes to the SSAP endpoints and returns
a channel receiver that emits `TvState` snapshots on each update.
Each subscription callback updates the relevant field of a cached
`TvState` and sends the updated snapshot.

---

## Part 2: Daemon per-TV state monitoring

### 2.1 Add subscribe_state() to TvConnection trait

**File:** `crates/tisvlay-core/src/traits.rs`

```rust
/// Subscribe to state change notifications from the device.
///
/// Returns a channel that receives TvState snapshots when the device
/// reports a change (e.g., volume adjusted via TV remote). Backends
/// that don't support push-based state return NotSupported.
async fn subscribe_state(
    &mut self,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<TvState>> {
    Err(Error::not_supported("subscribe_state", Capabilities::empty()))
}
```

### 2.2 Daemon subscribes after connecting

**File:** `crates/tisvlayd/src/tv_manager.rs`

After `with_connection` establishes a connection, attempt to subscribe:

```rust
if let Ok(state_rx) = connection.subscribe_state().await {
    // Spawn task that reads state_rx and emits StateChangeEvent
    tokio::spawn(async move {
        while let Some(state) = state_rx.recv().await {
            emit_state_change(StateChangeEvent::from_tv_state(tv_id, &state));
        }
    });
}
```

For backends without subscription support, the daemon can optionally
run a per-TV poll loop (slower interval, e.g., 30s) to keep state
fresh for signal consumers.

### 2.3 Emit signals for all state sources

Currently `emit_state_change()` is only called for connection changes
and explicit commands. With backend subscriptions, it also fires on
push-based state updates. Verify the D-Bus signal emission path handles
the increased signal volume (debounce if needed).

---

## Part 3: Tray signal-driven updates

### 3.1 Add signal subscription to DaemonClient

**File:** `crates/tisvlay-core/src/dbus_client.rs`

Use a broad D-Bus match rule to capture all `org.tisvlay.Tv1` signals:

```rust
pub async fn subscribe_state_changes(&self) -> Result<impl Stream<Item = TvStateChange>>
```

Parse the object path to extract TV ID. The `TvStateChange` enum:
```rust
pub enum TvStateChange {
    StateChanged { tv_id: String, state_json: String },
    ConnectionChanged { tv_id: String, connected: bool },
    TvListChanged,
    PropertyChanged { tv_id: String },
}
```

### 3.2 Replace poll loop with signal watcher

**File:** `crates/tisvlay-tray/src/main.rs`

Replace `poll_state()` with `watch_state()`:
- Subscribe to daemon D-Bus signals at startup
- On signal: debounce (coalesce within 100ms), then refresh
- Fallback poll at 30-60s for resilience
- Fall back to full polling if signal subscription fails

### 3.3 Simplify tray

Remove:
- `POLL_INTERVAL = 5s`
- Shadow cache (`last_tvs`, `last_daemon_available`)
- Per-field diff logging

Keep:
- `refresh_state()` for command-triggered updates
- Eager initial state fetch
- `BTreeMap` for profiles (good practice)
- `PartialEq` on `CachedTvState` (useful for tests)

---

## Part 4: Daemon signal completeness

### 4.1 Fix suppressed property change signals

**File:** `crates/tisvlayd/src/dbus/tv.rs`

`Managed` and `SystemProfile` properties have
`emits_changed_signal = "false"`. Either:
- Change to `emits_changed_signal = "true"`, or
- Emit a manual `StateChanged` in `set_tv_option()` when these change

### 4.2 Config reload signal

Add a signal (e.g., on Manager1) when config is reloaded, so the tray
knows to refresh profile lists. Currently profiles only change on reload.

---

## Part 5: Session service signals (optional)

Add `UserProfileChanged(tv_id: s, profile: s)` signal to
`org.tisvlay.Session1` when the user switches profiles. The tray
subscribes alongside daemon signals. Low priority since profile
changes are rare and the fallback poll covers it.

---

## Part 8: Factor out D-Bus service discovery utility

The tray uses `NameOwnerChanged` to detect when `org.tisvlay.Session`
appears on the bus — connecting within ~2ms instead of waiting for a
fallback poll. This pattern is reusable: the GUI will face the same
startup ordering issue with both the daemon and session services.

Extract a shared utility into `tisvlay-core/src/dbus_client.rs`:

```rust
/// Watch for a D-Bus service name to appear on the bus.
///
/// Returns a stream that yields () each time the given well-known name
/// is acquired by any process. Useful for clients that need to discover
/// services like org.tisvlay.Daemon or org.tisvlay.Session that may
/// start after the client.
pub async fn watch_service_appear(
    connection: &zbus::Connection,
    name: &str,
) -> Result<impl Stream<Item = ()>>
```

The tray and future GUI import this instead of directly wiring
`DBusProxy::receive_name_owner_changed_with_args`. Could also add a
complementary `watch_service_disappear` or a combined stream that
yields `ServiceEvent { appeared: bool }`.

---

## Part 6: Update docs

- `docs/design.md`: architecture diagram showing push-based pipeline,
  update tray description, add subscribable endpoints list
- `.claude/CLAUDE.md`: note push-based architecture, update Known Quirks
  (backlight not subscribable)
- `docs/issue.md`: note that push-based updates further reduce ksni
  LayoutUpdated frequency

---

## Implementation order

Phase 1 (LG backend subscriptions) is the largest and most impactful
change. It requires refactoring the WebSocket connection from
synchronous request/response to multiplexed async. This should be done
first and tested independently.

Phase 2 (daemon plumbing) is straightforward — wire subscription
channels to StateChangeEvent emission.

Phase 3 (tray signals) can be done independently of Phase 1 — the tray
benefits from signal subscription even if the daemon only emits signals
on explicit commands (which it already does). Phase 1 makes it real-time.

Suggested order: Phase 3 → Phase 4 → Phase 2 → Phase 1 → Phase 5 → Phase 6

This way the tray gets signal-driven updates first (immediate UX win),
then the backend subscription adds real-time TV state awareness.

---

## Checklist

- [x] Part 1.1: LG subscribe() method on WebOsClient
- [x] Part 1.2: Multiplexed WebSocket reader in LG backend
- [x] Part 1.3: LG TvConnection::subscribe_state() implementation
- [x] Part 2.1: subscribe_state() on TvConnection trait
- [x] Part 2.2: Daemon subscribes after connecting
- [x] Part 2.3: Signal emission for subscription updates
- [x] Part 3.1: DaemonClient signal subscription API
- [x] Part 3.2: Tray signal watcher with debouncing
- [x] Part 3.3: Remove poll artifacts from tray
- [x] Part 4.1: Fix suppressed property change signals
- [ ] Part 4.2: Config reload signal
- [ ] Part 5: Session service UserProfileChanged signal
- [x] Part 6: Update docs
- [x] ~~Part 7~~ Backlight IS subscribable via ssap://settings/getSystemSettings — no polling needed. Added to LG subscription list.
- [x] `cargo check` all crates
- [x] `cargo test` all crates

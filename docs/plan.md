# Plan: NetworkManager-aware TV reconnection

## Motivation

The daemon currently detects TV disconnection passively — only when a
WebSocket subscription ends or a command fails. Reconnection happens
reactively on the next user action or fallback poll. This causes:

1. **Slow wake recovery**: After system resume, the TV's NIC takes a few
   seconds to come online. The daemon doesn't attempt reconnection until
   the next user interaction or the 30s tray poll.
2. **Stale `Connected` state**: If the network drops (Wi-Fi blip, NIC
   reset), the D-Bus `Connected` property stays `true` until the next
   command fails or the TCP keepalive times out (minutes).
3. **No proactive reconnect**: When the network comes back, the daemon
   waits passively instead of immediately attempting to reconnect.

## Design

### NetworkManager D-Bus signals

NetworkManager exposes state on the system bus at
`org.freedesktop.NetworkManager` (`/org/freedesktop/NetworkManager`):

| Property | Type | Values | emits-change |
|----------|------|--------|-------------|
| `State` | `u` | 0=Unknown, 10=Asleep, 20=Disconnected, 30=Disconnecting, 40=Connecting, 50=ConnectedLocal, 60=ConnectedSite, 70=ConnectedGlobal | yes |
| `Connectivity` | `u` | 0=Unknown, 1=None, 2=Portal, 3=Limited, 4=Full | yes |

`StateChanged(u)` signal fires on state transitions. `PropertiesChanged`
also fires for `State` and `Connectivity` (emits-change).

### What the daemon should do

**On network restored** (`State` → 70 `ConnectedGlobal`):
- For each managed TV that is not connected and not powered off:
  attempt reconnection in the background (same as `with_connection`
  auto-connect, but proactive)
- This covers wake-from-sleep, network blip recovery, and NIC restart

**On network lost** (`State` < 50):
- For each connected TV: drop the connection, update `Connected=false`
  via the forwarder, so clients see the disconnect immediately instead
  of waiting for TCP timeout

### Architecture

```
                                    system bus
                                        │
NetworkManager ──StateChanged──► NetworkWatcher
                                        │
                                StateChangeEvent
                                        │
                                   TvManager
                              ┌─────────┴─────────┐
                         reconnect_all()     disconnect_all()
```

The watcher is a lightweight task that:
1. Connects to the **system bus** (always, even when daemon runs in
   session mode — NetworkManager is system-bus-only)
2. Subscribes to `StateChanged` on `org.freedesktop.NetworkManager`
3. On state transition to `ConnectedGlobal`: sends a
   `StateChangeEvent::NetworkRestored` through the existing channel
4. On state transition below `ConnectedLocal`: sends a
   `StateChangeEvent::NetworkLost`

The `state_change_forwarder` handles these events:
- `NetworkRestored`: calls `TvManager::reconnect_disconnected()`
- `NetworkLost`: calls `TvManager::disconnect_all()` and updates
  D-Bus `Connected` properties

This avoids active probing (no `get_state()` polling) and uses the
existing event channel instead of callbacks.

### Graceful degradation

If NetworkManager is not running (e.g., systemd-networkd systems),
the watcher logs a warning and exits. The daemon falls back to the
current behavior (passive detection + tray fallback poll). No crash,
no busy-wait.

## Implementation plan

### Phase 1: NetworkManager watcher module

**File:** `crates/tisvlayd/src/network_watcher.rs`

- Use `zbus::proxy` macro to define a `NetworkManagerProxy` for
  `org.freedesktop.NetworkManager` with `state_changed` signal
- Connect to the system bus (separate connection from the daemon's
  session/system bus)
- Subscribe to `StateChanged` signal
- Send `StateChangeEvent::NetworkRestored` or `NetworkLost` through
  the existing `state_change_tx` channel
- Spawn as a background task from `main.rs`
- If the system bus or NetworkManager is unavailable, log and return
  (no panic, no retry)

### Phase 2: TvManager reconnect/disconnect methods

**File:** `crates/tisvlayd/src/tv_manager.rs`

- `reconnect_disconnected()`: iterate all TVs, for each that is
  managed + not connected + not powered_off, spawn a reconnection
  attempt (reuse existing `connect_tv` logic)
- `disconnect_all()`: iterate all TVs, drop connections, clear
  `last_known_input`, emit `ConnectionChanged { connected: false }`

### Phase 3: Wire into state_change_forwarder

**File:** `crates/tisvlayd/src/main.rs`

- Add `NetworkRestored` and `NetworkLost` variants to `StateChangeEvent`
- Handle in `state_change_forwarder`:
  - `NetworkRestored`: call `TvManager::reconnect_disconnected()`
  - `NetworkLost`: call `TvManager::disconnect_all()`, update D-Bus
    `Connected=false` for all TVs

### Phase 4: Debounce and edge cases

- Debounce rapid state transitions (NetworkManager may fire multiple
  signals during a reconnect cycle)
- Skip reconnection if a WoL/power_on is already in progress for a TV
- On daemon startup in session mode: the watcher still connects to the
  system bus for NetworkManager (independent of the daemon's bus choice)

---

## Checklist

- [ ] Phase 1: NetworkManager watcher with zbus proxy
- [ ] Phase 2: TvManager reconnect/disconnect methods
- [ ] Phase 3: Wire into state_change_forwarder
- [ ] Phase 4: Debounce and edge cases
- [ ] Update design.md and CLAUDE.md

---

## Notes

- The watcher always uses the system bus regardless of `--session` flag
  (NetworkManager is system-bus-only)
- No dependency on NetworkManager — graceful fallback if unavailable
- NM state 70 (`ConnectedGlobal`) means full internet connectivity;
  state 50 (`ConnectedLocal`) means LAN only. TVs are on the LAN, so
  we could trigger on 50 instead of 70. Start with 70 (conservative),
  tune if needed.
- For `on_wake`: logind's `PrepareForSleep(false)` fires before the
  network is restored. The NM watcher bridges this gap — logind tells
  us we woke up, NM tells us the network is ready.

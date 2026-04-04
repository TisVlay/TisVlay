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

### Phase 4: Prevent racing reconnect paths

On wake, two paths may try to reconnect the same TV concurrently:
1. logind `PrepareForSleep(false)` → `on_wake` → WoL retry loop
2. NM `StateChanged` → `NetworkRestored` → `reconnect_disconnected`

The duplicate connections confuse the TV (WebSocket close frames) and
double the reconnection time. Options:

- **A. Skip NM reconnect during active WoL**: add a per-TV
  `reconnecting` flag set by `power_on`/`on_wake`, checked by
  `reconnect_disconnected`. Simple, but requires flag lifecycle.
- **B. Suppress NM reconnect after wake**: ignore `NetworkRestored`
  for N seconds after `PrepareForSleep(false)`. Simpler, but uses a
  time window heuristic.
- **C. Remove WoL from on_wake entirely**: let the NM watcher handle
  reconnection. logind wake just clears state; NM restored does the
  actual reconnect. Clean separation, but changes the wake semantics
  (no WoL on wake, which may be needed if TV went to standby).
- **D. Debounce NM signals**: absorb NM oscillation during wake with a
  500ms window. Tested — adds latency without helping (the real issue
  is duplicate reconnect paths, not signal noise).

---

## Test matrix

Run on system bus via `all: start (system bus)`. Each test is a
desktop sleep cycle (suspend → resume). Measure with:

```
./scripts/bench_network.py --since 5
```

Also check full logs for racing/duplicate connection attempts:

```
journalctl -t tisvlayd --since "5 min ago" -o short-precise | \
  grep -E "sleep|wake|resumed|Network|Connecting|Connected|Subscribed|disconnected|WebSocket close"
```

### Baseline (no NM watcher)

Commit: `e9384c77` (before NM watcher)

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | never |
| Wake → subscribed | 18.6s |
| Duplicate connect attempts | no |
| Notes | logind on_wake WoL retry only |

### Variant 1: NM watcher, no debounce

Commit: first NM watcher implementation (no debounce)

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 8ms |
| Wake → subscribed | 16.1s |
| Duplicate connect attempts | yes (logind + NM race) |
| Notes | NM oscillated during wake: restored→lost→restored. First reconnect killed by second NM lost signal 47ms later |

### Variant 2: NM watcher + 500ms debounce

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 504ms |
| Wake → subscribed | 35.3s |
| Duplicate connect attempts | yes (logind + NM race) |
| Notes | Debounce absorbed NM oscillation but logind and NM paths still race. Much worse overall. Reverted. |

### Variant 3B: Wake window suppression (SELECTED)

Current HEAD — Option B from Phase 4.

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 9ms |
| Wake → subscribed | 18.4s |
| Duplicate connect attempts | no |
| Notes | All NM events (restored + lost) suppressed during 30s wake window. logind on_wake WoL retry runs cleanly. NM reconnect still works for non-wake network blips (tested: 136ms via nmcli cycle). |

### Variant A: Per-TV reconnecting flag (SELECTED)

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 8ms |
| Wake → subscribed | 14.8s |
| Duplicate connect attempts | no |
| Notes | `reconnecting` flag on ManagedTv set during WoL, checked by `reconnect_disconnected`. NM oscillation during wake is harmless — `disconnect_all` on a TV mid-WoL is a no-op. Fastest and cleanest. |

### Variant C: Deferred wake via NM

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 4ms |
| Wake → subscribed | 18.0s |
| Duplicate connect attempts | yes (NM oscillation) |
| Notes | logind emits SystemWake, forwarder defers on_wake until NM restored. NM oscillation kills in-flight connections and a late NM flap at +18s killed an already-working subscription. Fragile. |

### NM blip recovery (non-wake, nmcli off/on)

| Metric | Value |
|--------|-------|
| NM lost → Connected=false | <1ms |
| NM restored → subscribed | 136ms |
| Notes | Purely event-driven, no polling. |

### Summary

| Variant | Wake→sub | Sleep disconnect | Racing |
|---------|----------|-----------------|--------|
| Baseline (no NM) | 18.6s | never | no |
| V1 (no debounce) | 16.1s | 8ms | **yes** |
| V2 (500ms debounce) | 35.3s | 504ms | **yes** |
| V3B (wake suppression) | 18.4s | 9ms | no |
| **VA (reconnecting flag)** | **14.8s** | **8ms** | **no** |
| VC (deferred wake) | 18.0s | 4ms | **yes** |

**Variant A selected**: fastest wake (14.8s), instant disconnect (8ms),
no racing, simple implementation (one bool flag). Wake time is
bottlenecked by TV firmware boot (~11s of WebSocket close retries
before SSAP is ready). The NM watcher's value: instant disconnect
detection + sub-second network blip recovery + no interference with wake.

---

## Checklist

- [x] Phase 1: NetworkManager watcher with zbus proxy
- [x] Phase 2: TvManager reconnect/disconnect methods
- [x] Phase 3: Wire into state_change_forwarder
- [ ] Phase 4: Prevent racing reconnect paths
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
- The TV accepts WebSocket connections during boot but sends an
  immediate `close` frame — the SSAP service isn't ready yet. The
  backend's retry loop handles this (3s intervals, 10 attempts).
- WoL → TV NIC online → SSAP ready takes ~10s in practice (3-4
  retry cycles). The bottleneck is the TV firmware boot, not network.

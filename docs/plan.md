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

### Variant A: Per-TV reconnecting flag

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 8ms |
| Wake → subscribed | 14.8s |
| Duplicate connect attempts | no |
| Notes | `reconnecting` flag on ManagedTv set during WoL, checked by `reconnect_disconnected`. NM `disconnect_all` on a TV mid-WoL is a no-op (connection held by backend), but fires unnecessarily. |

### Variant A+: reconnecting flag guards both reconnect and disconnect (SELECTED)

Same as VA but `disconnect_all` also skips TVs with `reconnecting=true`.
NM oscillation during wake is fully suppressed — no interference at all.

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 3ms |
| Wake → subscribed | 16.4s |
| Duplicate connect attempts | no |
| NM interference during wake | none (fully suppressed) |
| Notes | 16.4s vs VA's 14.8s is TV firmware boot variance (±3s per retry cycle), not a regression. Clean logs: both NM restored and NM lost explicitly skipped during WoL. |

### D-Bus power cycle (no sleep, hardware minimum)

Power off + 5s wait + power on via `busctl`. No sleep, no NM
involvement. Measures pure TV firmware boot time.

With 3s retries:

| Metric | Value |
|--------|-------|
| WoL → subscribed | 6.9s |
| Retry attempts | 3 (2 WebSocket close + 1 success) |
| Notes | TV SSAP ready ~5.2s after WoL. 3s retry caught it by luck — could waste up to 2.9s on unlucky timing. |

With 500ms retries:

| Metric | Value |
|--------|-------|
| WoL → subscribed | 6.8s |
| Retry attempts | 8-9 (7-8 close + 1 success) |
| Notes | Catches TV within 500ms of SSAP ready. Each attempt is cheap (~50ms TCP connect + close frame). Consistent across runs. |

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

### VA with TV staying on (automate_when=on_primary, non-primary input)

TV not powered off during sleep (Automated=false skipped on_sleep).
On wake, `last_known_input` is `None` (cleared by disconnect_all),
so `is_automated` returns `true` (benefit of doubt) and on_wake
fires WoL. WoL is harmless to an already-on TV. Only WebSocket
reconnection was needed — no TV firmware boot wait.

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 4ms |
| Wake → subscribed | 8.1s |
| TV powered off during sleep | no |
| Notes | Fastest wake: TV NIC already up, only WebSocket reconnect needed. Benefit-of-doubt gate works correctly — unknown input after disconnect defaults to automated. |

### VA+ with 500ms retries (sleep cycle)

Same as VA+ but with 500ms retry interval instead of 3s. Catches the
TV within one poll of SSAP becoming ready instead of waiting up to
2.9s for the next 3s retry.

| Metric | Value |
|--------|-------|
| Sleep → disconnect detected | 3ms |
| Wake → TV online | 13.2s |
| Wake → session unlock | 13.0s |
| Wake → subscribed | 19.7s |
| Duplicate connect attempts | no |
| Notes | TV boots slower from deep sleep (~13s) vs short power off (~5.2s). 500ms polling catches SSAP within one cycle. "WoL resend failed" during NM oscillation is harmless (initial WoL already sent). |

### Summary

| Variant | Wake→login | Wake→sub | Sleep disconnect | Racing | NM clean |
|---------|------------|----------|-----------------|--------|----------|
| Baseline (no NM, 3s) | 16.5s | 18.6s | never | no | n/a |
| VA+ (3s retries) | 15.8s | 16.4s | 3ms | no | yes |
| **VA+ (500ms retries)** | **13.0s** | **19.7s** | **3ms** | **no** | **yes** |
| VA+ (TV stays on) | — | 8.1s | 4ms | no | yes |
| D-Bus power cycle (3s) | — | 6.9s | n/a | n/a | n/a |
| D-Bus power cycle (500ms) | — | 6.8s | n/a | n/a | n/a |
| Theoretical min | 11.1s | — | — | — | — |

**VA+ with 500ms retries selected**: 13.0s wake-to-login (2.8s faster
than 3s retries, 1.9s over theoretical minimum). Instant disconnect
(3ms), no racing, no NM interference, simple implementation. The
remaining 1.9s is TV deep-sleep boot variance — hardware, not code.

Retry costs are negligible: each attempt is ~50ms (TCP connect + WebSocket
close frame). 500ms interval × 20+ attempts is fine — the TV handles
concurrent discovery clients routinely.

---

## Checklist

- [x] Phase 1: NetworkManager watcher with zbus proxy
- [x] Phase 2: TvManager reconnect/disconnect methods
- [x] Phase 3: Wire into state_change_forwarder
- [x] Phase 4: Prevent racing reconnect paths (VA+ selected)
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

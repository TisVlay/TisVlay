# Review: NM removal, NIC failover, tracing-log fix, dead code removal (946ea98..HEAD)

Review of 11 code commits (plus docs) following the initial networking review.
Major changes: NM watcher removed, NIC failover via `survives_source_ip_change`,
route coalescing replaces trailing-edge debounce, tracing-log bridge disabled.

Build: clean. Tests: all pass.

---

## Y1. NetworkManager watcher removed entirely — correct decision

**Deleted:** `crates/tisvlayd/src/network_watcher.rs` (146 lines)

Testing with simultaneous wired + Wi-Fi showed NM's global `StateChanged` signal
oscillates through `Disconnected` during NIC transitions, racing with the per-device
carrier monitor. The kernel netlink layer handles all scenarios:
- Cable pull: `RTMGRP_LINK` carrier detection
- NM software disconnect: route re-resolution
- NIC failover: route change + `survives_source_ip_change`
- Sleep/wake: `reconnecting` flag prevents racing

`NetworkRestored` and `NetworkLost` `StateChangeEvent` variants also removed. Clean
removal — no orphan references. The `zbus` dependency on `tisvlayd` still needed for
logind and D-Bus service, so no crate dep changes.

**Severity:** Info (correct simplification backed by testing)

---

## Y2. `survives_source_ip_change()` on `TvConnector` — good abstraction

**File:** [crates/tisvlay-core/src/traits.rs](crates/tisvlay-core/src/traits.rs)

```rust
fn survives_source_ip_change(&self) -> bool {
    false
}
```

Default `false` is the safe choice — TCP/TLS connections die on source IP change.
This is a connector-level property (not transport or connection) since it depends on
the protocol: WebSocket (TCP) returns `false`; a future QUIC backend could return `true`.

The commit message documents an earlier attempt using `check_health()` (WebSocket Ping
probe) that failed: TCP `send()` succeeds into the kernel buffer even on a dead path.
The declarative approach is better than runtime probing.

**Severity:** Info (correct abstraction level)

---

## Y3. Route change handling: coalesce replaces trailing-edge debounce

**File:** [crates/tisvlayd/src/main.rs:271-295](crates/tisvlayd/src/main.rs#L271-L295)

The 500ms trailing-edge debounce is replaced with immediate resolve + 100ms coalesce:

```rust
StateChangeEvent::RouteChanged => {
    manager.re_resolve_all_interfaces().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    // drain queued RouteChanged events
    loop {
        match rx.try_recv() {
            Ok(StateChangeEvent::RouteChanged) => coalesced += 1,
            Ok(other) => { let _ = manager.state_change_sender().send(other); break; }
            Err(_) => break,
        }
    }
    if coalesced > 0 {
        manager.re_resolve_all_interfaces().await;
    }
}
```

The commit message explains why: testing confirmed RTM_GETROUTE returns correct results
at any point during route event bursts (no transient flaps). The first event triggers
immediate re-resolve, then the 100ms pause collects stragglers which trigger one more
re-resolve. Two passes max instead of waiting 500ms.

Non-RouteChanged events encountered during the drain are re-queued via
`state_change_sender().send(other)` — correct, they're not lost.

**Severity:** Info (correct, validated by testing)

---

## Y4. NIC failover route transition handling

**File:** [crates/tisvlayd/src/tv_manager.rs:428-457](crates/tisvlayd/src/tv_manager.rs#L428-L457)

New `RouteAction` case for interface-to-interface transitions:

```rust
} else if interface.is_some()
    && previous.is_some()
    && interface != previous
    && tv.is_connected()
    && !tv.reconnecting
    && !self.registry.survives_source_ip_change(&config)
{
    // Route shifted to different NIC, protocol can't survive
    RouteAction::Disconnect(conn)
}
```

After disconnect, if a route still exists (new NIC is up), an `InterfaceUp` event
is emitted to trigger reconnection via the new interface. This two-step
(disconnect + reconnect) avoids the recursive async issue documented in the original
design.

**Severity:** Info (correct failover handling)

---

## Y5. tracing-log bridge disabled — correct fix for noisy dependencies

**File:** `Cargo.toml`

```toml
tracing-subscriber = { version = "0.3", default-features = false, features = ["fmt", "ansi", "env-filter", "registry"] }
```

The `tracing-log` feature (default in tracing-subscriber) bridges `log` crate macros
to tracing. `netlink-packet-route` emits `log::debug!()` on every link event about
newer kernel attributes. These bypassed `EnvFilter` because the bridged events had
mismatched metadata (`target = "log"`).

Disabling `default-features` and explicitly listing needed features removes the
bridge. All tisvlay code uses `tracing` natively, so the bridge was only serving
dependency noise.

**Severity:** Info (correct, no functional loss)

---

## Y6. `subscribe_state_changes()` removed — X11 resolved

**Commit:** 63cfeb7

The method that duplicated StateWatcher's signal subscription logic is removed.
Zero callers after the tray switched to StateWatcher. This directly addresses
the X11 finding from the previous review.

**Severity:** Info (X11 resolved, dead code removed)

---

## Y7. Log level demotions — reduced noise

Several log levels changed:
- `info!` to `debug!` for InterfaceUp/InterfaceDown events in forwarder
- `debug!` to `trace!` for per-event route logs and route resolution
- "Monitored interface updated" message now uses `{previous:?} -> {interface:?}` format

These are appropriate — per-event logs at `debug` or `trace` don't pollute
production logs, while interface transitions still log at `info` within the
methods that actually perform disconnect/reconnect.

**Severity:** Info (correct log discipline)

---

## Y8. Route coalesce re-queues non-route events correctly

**File:** [crates/tisvlayd/src/main.rs:282-286](crates/tisvlayd/src/main.rs#L282-L286)

```rust
Ok(other) => {
    let _ = manager.state_change_sender().send(other);
    break;
}
```

Non-RouteChanged events encountered during the `try_recv` drain are pushed back
through the sender. This means they arrive at the end of the channel and will be
processed in the next loop iteration. The ordering change (non-route event
potentially delayed by ~100ms) is acceptable — these events are debounced by the
tray anyway.

One subtlety: only the FIRST non-route event breaks the drain. If there are
multiple non-route events queued behind route events (unlikely but possible),
the remaining route events in the queue will be processed individually in
subsequent loop iterations rather than coalesced. This is fine — the extra
re-resolves are cheap (microseconds).

**Severity:** Info (correct, edge case is harmless)

---

## Y9. Documentation consolidation

Plan.md, plan2.md, and review.md consolidated into permanent docs:
- `docs/watchers/network.md` — deep reference on the two-layer monitoring architecture
- `docs/design.md` — updated with per-device monitoring and daemon lifecycle sections
- `.claude/CLAUDE.md` — updated with netlink.rs, watchers/, rtnetlink crates

Good — temporary planning/review docs replaced with durable reference documentation.

**Severity:** Info (good documentation practice)

---

## Y10. Known issue: transient route flaps during Wi-Fi bring-up

The `survives_source_ip_change` commit message notes:

> Known issue: transient route flaps during Wi-Fi bring-up can trigger
> a false NIC change before the routing table settles back to the
> preferred interface. Needs a confirmation re-resolve.

This is documented but not yet fixed. During Wi-Fi bring-up, the kernel may
briefly route through the Wi-Fi NIC before metrics settle back to the preferred
wired NIC. This would trigger an unnecessary disconnect + reconnect cycle.

The 100ms coalesce window partially mitigates this (the flap may settle within
the window), but a dedicated confirmation re-resolve (e.g., re-check the route
200ms after a NIC change) would be more robust.

**Severity:** Low (documented known issue, partially mitigated by coalescing)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| Y1 | **Info** | NM watcher correctly removed, kernel netlink handles all cases |
| Y2 | **Info** | `survives_source_ip_change` is correct abstraction for NIC failover |
| Y3 | **Info** | Coalesce replaces debounce, validated by testing |
| Y4 | **Info** | NIC failover disconnect + reconnect avoids async recursion |
| Y5 | **Info** | tracing-log bridge disabled, removes dependency noise |
| Y6 | **Info** | `subscribe_state_changes()` removed (X11 resolved) |
| Y7 | **Info** | Log levels appropriately demoted |
| Y8 | **Info** | Route coalesce re-queues non-route events correctly |
| Y9 | **Info** | Documentation consolidated into permanent reference |
| Y10 | **Low** | Transient route flaps during Wi-Fi bring-up (documented, partially mitigated) |

## Overall Assessment

This set resolves the NM false-positive issue that motivated the original carrier
monitoring work. The NM watcher's removal is validated by comprehensive testing across
all scenarios (cable pull, NM disconnect, sleep/wake, NIC failover). The kernel netlink
layer handles everything without NM dependency.

The `survives_source_ip_change` trait method is a well-designed future-proofing
abstraction. The route coalescing is simpler and more correct than the trailing-edge
debounce it replaces.

One low-severity item: **Y10** — transient route flaps during Wi-Fi bring-up can cause
an unnecessary reconnect cycle. A confirmation re-resolve after NIC changes would fix it.

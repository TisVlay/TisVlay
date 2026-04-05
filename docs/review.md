# Review: Per-device carrier monitoring + StateWatcher extraction (main..HEAD)

Review of 15 commits covering kernel netlink carrier monitoring, route resolution,
NM software disconnect detection, daemon lifecycle improvements, StateWatcher extraction,
and LG connection retry refactor.

Build: clean. Tests: all pass.

---

## X1. Kernel netlink carrier monitoring — correct two-layer design

**File:** [crates/tisvlayd/src/netlink.rs](crates/tisvlayd/src/netlink.rs)

Clean separation of concerns:
- `resolve_interface_for_ip()` — one-shot route query (RTM_GETROUTE)
- `spawn_link_monitor()` — long-running multicast subscriber

The multicast connection subscribes to 5 groups in a single socket:
`Link`, `Ipv4Ifaddr`, `Ipv6Ifaddr`, `Ipv4Route`, `Ipv6Route`. Link events
emit `InterfaceUp`/`InterfaceDown`; route/address events emit `RouteChanged`.

Carrier detection uses both `IFF_LOWER_UP` (physical) and `OperState::Up`
(software). This catches both cable pull (carrier lost) and NM software
disconnect (OperState→Down but carrier stays true).

Test: `test_resolve_localhost` verifies localhost routes to `"lo"`.

**Severity:** Info (correct design)

---

## X2. RTM_F_LOOKUP_TABLE fix — critical correctness issue caught

**File:** [crates/tisvlayd/src/netlink.rs:51-67](crates/tisvlayd/src/netlink.rs#L51-L67)

Without `RouteFlags::LookupTable`, the kernel returned cached/stale routes for
deconfigured interfaces. Discovered via `strace` comparing behavior with
`ip route get`. The fix is a single line per route query:

```rust
msg.header.flags.insert(RouteFlags::LookupTable);
```

This is the kind of subtle kernel API behavior that's easy to miss — the commit
message (36471c4) documents the discovery process thoroughly.

**Severity:** Info (critical fix, well-documented)

---

## X3. IPv6 link-local filtering — correct address validation

**File:** [crates/tisvlayd/src/netlink.rs:97-115](crates/tisvlayd/src/netlink.rs#L97-L115)

After NM disconnect, `fe80::` link-local addresses persist on the interface. Without
filtering, `resolve_interface_for_ip` would return the interface as "usable" even
though it has no routable address. The fix checks:
- IPv4 addresses → always count as routable
- IPv6 addresses → only count if scope != Link

**Severity:** Info (correct edge case handling)

---

## X4. Trailing-edge debounce for route changes — correct pattern

**File:** [crates/tisvlayd/src/main.rs:199-220](crates/tisvlayd/src/main.rs#L199-L220)

NM disconnect fires dozens of route events in a burst. The trailing-edge debounce
resets a 500ms deadline on each `RouteChanged`, then re-resolves after the burst
settles. Implemented cleanly with a `tokio::select!` between the event channel and
a `tokio::time::sleep_until(deadline)`.

The commit message documents why the original leading-edge debounce failed: it ran
re-resolution on the FIRST event (before routes were flushed) and then blocked
subsequent events for 100ms, missing the final settled state.

**Severity:** Info (correct debounce pattern, well-documented failure mode)

---

## X5. `resolve_monitored_interface` with route transition actions

**File:** [crates/tisvlayd/src/tv_manager.rs:390-478](crates/tisvlayd/src/tv_manager.rs#L390-L478)

Uses an internal `RouteAction` enum to determine what to do after resolving:
- `Some("eno2") -> None`: route disappeared → disconnect TV, emit ConnectionChanged(false)
- `None -> Some("eno2")`: route reappeared → emit InterfaceUp to trigger reconnection

The `RouteAction::Reconnect` avoids recursive async by emitting an `InterfaceUp` event
instead of calling `reconnect()` directly. The forwarder handles the reconnection.
Clean design that avoids the `async fn` recursion footgun.

**Severity:** Info (correct design, avoids recursion)

---

## X6. Graceful shutdown closes WebSocket sessions

**File:** [crates/tisvlayd/src/main.rs:168-172](crates/tisvlayd/src/main.rs#L168-L172)

`close_all_connections()` collects connections outside locks, then closes gracefully
before D-Bus name release. This ensures TVs drop their SSAP sessions immediately —
without it, the TV holds the stale session until TCP keepalive timeout (~20s), causing
"Connection reset" on warm restart.

The shutdown sequence is correct: close connections → abort signal worker → drop
D-Bus connection (releases well-known name).

**Severity:** Info (important for warm restart reliability)

---

## X7. Warm restart connects without startup actions

**File:** [crates/tisvlayd/src/tv_manager.rs:558-590](crates/tisvlayd/src/tv_manager.rs#L558-L590)

`connect_all_on_restart()` is called when the boot marker exists (service restart,
not system boot). It connects to all TVs without WoL, screen_on, or input switching —
the TVs are already on from the previous daemon instance.

The LG backend's `connect()` now retries internally (5 attempts, 1s interval) to
handle the stale session race. This moved from the daemon's
`connect_all_on_restart` to `LgConnector::connect()` — correct placement since the
retry logic is an LG/SSAP-specific concern.

**Severity:** Info (correct separation of concerns)

---

## X8. LG connection retry moved to backend — good refactor

**File:** [crates/tisvlay-lg/src/backend.rs:423-437](crates/tisvlay-lg/src/backend.rs#L423-L437)

```rust
for attempt in 1..=5u32 {
    match WebOsClient::connect(&config.ip, config.client_key.clone()).await {
        Ok(client) => return Ok(Box::new(LgConnection::new(client, config))),
        Err(e) => { ... tokio::time::sleep(Duration::from_secs(1)).await; }
    }
}
Err(last_err.unwrap())
```

The `unwrap()` on `last_err` is safe — the loop always executes at least once
(5 iterations), so `last_err` is always `Some` after the loop.

This retry handles the LG-specific "one SSAP session at a time" constraint. The
daemon's `connect_all_on_restart` dropped from ~60 lines to ~10, becoming
transport-agnostic.

**Severity:** Info (correct, `unwrap` is safe)

---

## X9. `reconnect_disconnected` now reconnects all TVs regardless of managed

**Commit:** 1ca84c1

`managed` gates automation (actions like power on/off), not connectivity. An unmanaged
TV still needs a working connection for manual control via the tray and CLI. The
`reconnect_disconnected` method now skips the `!managed` check — all network TVs
get reconnected after a network blip.

**Severity:** Info (correct semantic distinction)

---

## X10. StateWatcher extraction — clean separation

**File:** [crates/tisvlay-core/src/dbus_client.rs](crates/tisvlay-core/src/dbus_client.rs)

`StateWatcher` encapsulates all D-Bus subscription and lifecycle watching:
- 5 signal/event sources (daemon signals, daemon name, session name, session signals,
  fallback poll)
- Signal debouncing within the watcher
- `next()` returns `StateEvent` enum — consumer decides how to react

The tray's `watch_state()` dropped from ~175 lines to ~45, lost its direct `zbus`
and `futures-util` dependencies, and became a simple match-on-event loop.

`StateWatcherConfig` with `Default` impl provides clean defaults (100ms debounce,
30s fallback). The future GUI imports `StateWatcher` and writes its own event handler.

**Severity:** Info (excellent reuse abstraction)

---

## X11. Duplicate signal subscription logic between StateWatcher and subscribe_state_changes

**File:** [crates/tisvlay-core/src/dbus_client.rs](crates/tisvlay-core/src/dbus_client.rs)

`StateWatcher::new()` (lines ~1280-1340) and `DaemonClient::subscribe_state_changes()`
(lines ~540-640) both set up the same three AddMatch rules and create the same
`MessageStream` with the same `filter_map` logic. The filter_map closures are
near-identical, matching on `PropertiesChanged`, `InterfacesAdded/Removed`,
`TvAdded/TvRemoved`, and `ConfigReloaded`.

`subscribe_state_changes` is now only used indirectly (StateWatcher calls AddMatch
directly), but it's still a public API method. If no external consumers need it,
it could be removed and the match rules + filter_map factored into a shared helper.

**Fix direction:** Either remove `subscribe_state_changes` (if StateWatcher fully
replaces it) or extract the shared match-rule setup and filter_map into a private
method that both call.

**Severity:** Low (code duplication, both implementations are correct)

---

## X12. `new_connection()` called per route resolution — consider caching

**File:** [crates/tisvlayd/src/netlink.rs:28-44](crates/tisvlayd/src/netlink.rs#L28-L44)

Every call to `resolve_interface_for_ip()` creates a new netlink connection, spawns
a connection task, resolves the route, then aborts the task. For a single TV this
is fine, but `re_resolve_all_interfaces()` calls it once per TV — N connections
for N TVs.

The connection creation is fast (~100us) and the abort is clean, so this is
acceptable for small TV counts (1-10). For larger deployments (unlikely but
possible), a persistent handle could be shared:

```rust
struct NetlinkResolver {
    handle: rtnetlink::Handle,
    _task: JoinHandle<()>,
}
```

Not needed now — premature optimization for the expected scale.

**Severity:** Info (acceptable for expected scale, noted for future)

---

## X13. `disconnect_tvs_on_interface` and `close_all_connections` could share pattern

**File:** [crates/tisvlayd/src/tv_manager.rs:948-1000](crates/tisvlayd/src/tv_manager.rs#L948-L1000)

Both methods follow the same pattern: collect connections outside locks, close
gracefully, emit ConnectionChanged. `close_all_connections` is a simpler version
(no interface filter, no reconnecting check). They could share a
`disconnect_tvs(filter: impl Fn(...) -> bool)` helper, but with only two call
sites and slight differences (close_all doesn't emit events or clear
last_known_input), the duplication is acceptable.

**Severity:** Info (mild duplication, not worth extracting)

---

## X14. Daemon name watch on correct bus — good detail

**Commit:** 92bbb45

The tray watches `org.tisvlay.Daemon` on the bus matching the daemon's actual
registration (system or session, derived from `daemon.connection()`). Previously
it only watched the session bus, so `tisvlayd` running on the system bus was
invisible to the tray's name watcher. The `StateWatcher` inherits this fix.

**Severity:** Info (correct bus resolution)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| X1 | **Info** | Two-layer netlink carrier monitoring is correct |
| X2 | **Info** | RTM_F_LOOKUP_TABLE fix prevents stale route returns |
| X3 | **Info** | IPv6 link-local filtering handles NM disconnect edge case |
| X4 | **Info** | Trailing-edge debounce for route changes is correct |
| X5 | **Info** | Route transition actions avoid async recursion |
| X6 | **Info** | Graceful shutdown prevents stale SSAP sessions |
| X7 | **Info** | Warm restart connects without startup actions |
| X8 | **Info** | LG retry moved to backend, `unwrap` is safe |
| X9 | **Info** | Reconnect ignores managed flag (correct semantics) |
| X10 | **Info** | StateWatcher extraction is an excellent reuse abstraction |
| X11 | **Low** | Duplicate signal subscription logic between StateWatcher and subscribe_state_changes |
| X12 | **Info** | Per-resolution netlink connection acceptable for expected scale |
| X13 | **Info** | Mild duplication in disconnect methods (not worth extracting) |
| X14 | **Info** | Daemon name watch uses correct bus |

## Overall Assessment

This is a robust networking infrastructure upgrade. The kernel netlink layer provides
millisecond-accurate carrier detection independent of NetworkManager, solving the
virbr0/docker0 false-positive problem. The RTM_F_LOOKUP_TABLE fix, IPv6 link-local
filtering, and trailing-edge debounce are all battle-tested through the documented
test matrix.

The StateWatcher extraction (plan2.md) is the standout refactoring: the tray dropped
from ~175 lines of D-Bus wiring to ~45 lines of event-driven business logic. The
future GUI gets signal debouncing, service lifecycle watching, and fallback polling
for free.

One low-severity item: **X11** — `subscribe_state_changes()` and `StateWatcher::new()`
duplicate the same AddMatch + filter_map logic. If `subscribe_state_changes` has no
external consumers, it could be removed.

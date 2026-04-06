# Plan: Handle NIC failover for live connections

## Status: Complete

All steps implemented and verified with hardware testing.

## Problem

When the route to a TV shifts from one NIC to another (e.g., wired → Wi-Fi), the daemon updated `monitored_interface` but left the existing connection in place. For TCP/TLS-based protocols (WebSocket), the connection was dead — bound to the old NIC's source IP. The tray showed the TV as "connected" but commands silently failed until something triggered a reconnect.

## Why `check_health` failed

The first attempt added `check_health()` to `TvConnection` — a runtime probe via WebSocket Ping. But `send(Ping)` returns `Ok` even on a dead TCP path because the kernel accepts data into the socket send buffer. The Ping never reaches the TV, but the send doesn't fail. A reliable probe would need a full request-response with timeout, which is slow and complex.

## The real insight

The backend **knows at design time** whether its protocol can survive a source IP change:
- WebSocket over TCP: **no** — TCP 4-tuple `(src_ip, src_port, dst_ip, dst_port)` is the connection identity
- HTTP request-response: **yes** — each request is independent
- WebTransport over QUIC: **theoretically yes** — QUIC uses Connection IDs, supports migration (RFC 9000 §9). Practically limited (server must enable, implementation support varies).

This is a design-time property of the backend's protocol choice, not a runtime state. No probing needed.

## FOSS research: transport vs protocol

Surveyed libp2p, GStreamer, Home Assistant, OpenHAB, and tower for how they separate transport from protocol and handle connection migration:

**Key findings**: no major project models connection migration at the transport type level.

- **libp2p**: TCP and QUIC implement the same `Transport` trait. QUIC migration is invisible to the framework — it's an implementation detail. The swarm handles reconnection uniformly.
- **OpenHAB / Home Assistant**: the framework cares about the *consequence* (online/offline), not the mechanism. Each binding/integration handles its own transport resilience.
- **tower**: deliberately transport-agnostic. Connection lifecycle is below its abstraction.

**Pattern**: connection migration is a **connector-level concern** (the thing that creates connections), not a transport-category concern. Our `Transport` enum is the right granularity for "should NM events affect this?" (`is_network_dependent()`). But "does this specific backend's protocol survive IP changes?" is a narrower question about the backend's protocol choice.

Putting it on `Transport` conflates two distinct concerns:
- `Transport::Network` → "this backend uses IP networking" (scopes carrier events)
- `survives_source_ip_change()` → "this backend's protocol can handle client IP migration" (scopes reconnection on NIC change)

These are independent. A WebSocket backend and an HTTP backend both use `Transport::Network`, but only one needs reconnection on IP change.

## Design: Declarative on `TvConnector`

```rust
trait TvConnector {
    fn transport(&self) -> Transport;

    /// Whether connections from this backend survive client source IP changes.
    /// Default: false (conservative — assumes connection dies).
    fn survives_source_ip_change(&self) -> bool { false }
}
```

Why `TvConnector`:
- It's the **backend-level** type that knows the protocol choice (WebSocket, HTTP, QUIC)
- `Transport` stays unchanged — it answers "what kind of physical transport?" not "what are the protocol's resilience properties?"
- Non-network backends never get asked (daemon gates on `is_network_dependent()` first)
- LG declares `false` via the default (WebSocket over TCP)
- A future QUIC backend overrides to `true`
- The daemon already accesses `TvConnector` via `BackendRegistry`

### Options rejected

**On `TvConnection`**: wrong level — the connection instance doesn't decide this; every connection from the same backend behaves the same. Also meaningless for Serial/CEC connections.

**On `Transport` enum**: conflates transport category with protocol resilience. All `Transport::Network` backends would be treated identically — HTTP request-response would be wrongly disconnected.

**Runtime probe (`check_health`)**: unreliable — TCP send buffers mask dead connections.

## Additional discoveries during testing

### NM watcher caused false disconnects

Testing with simultaneous wired + Wi-Fi showed NM's global `StateChanged` signal caused false disconnects during NIC transitions — it oscillated through `Disconnected` before settling, racing with the per-device carrier monitor. Removed the NM watcher entirely; kernel netlink handles all scenarios.

### Route debounce was unnecessary

High-frequency route sampling (`scripts/bouncing_routes.py` at 10ms) confirmed the kernel routing table never produces transient wrong results during NIC transitions — `ip route get` is always consistent with the current FIB state. Replaced the 500ms trailing-edge debounce with event coalescing (instant first event, drain duplicates after resolve).

### tracing-log bridge noise

The `netlink-packet-route` crate emits `log::debug!()` on every link event for newer kernel attributes. The `tracing-log` bridge forwarded these with mismatched metadata (`target="log"`, wrong journal priority), bypassing `EnvFilter`. Fixed by disabling the bridge (`default-features = false` on `tracing-subscriber`).

## What was implemented

- [x] `TvConnector::survives_source_ip_change()` with default `false`
- [x] `BackendRegistry::survives_source_ip_change()` mirroring `transport_for()`
- [x] Daemon route transition: NIC change + `!survives_source_ip_change()` → disconnect + reconnect
- [x] Removed `check_health` from `TvConnection`, LG backend, `Connection`
- [x] Removed NM watcher module, `NetworkRestored`/`NetworkLost` events, `reconnect_disconnected`/`disconnect_network_tvs`
- [x] Replaced route debounce with coalesce (instant first event, drain duplicates)
- [x] Demoted per-event route logs to trace; info only on actual transitions
- [x] Log messages include TV ID and NIC names
- [x] Disabled `tracing-log` bridge to suppress `netlink-packet-route` noise
- [x] Route bounce telemetry script (`scripts/bouncing_routes.py`)

## Tested scenarios

| Scenario | Result |
|----------|--------|
| Wi-Fi connect while wired active | No false trigger (route stays on eno2) |
| Wired disconnect (failover to Wi-Fi) | Instant disconnect + reconnect via wlp7s0 |
| Wired reconnect (failback from Wi-Fi) | NIC change detected, reconnect via eno2 |
| Wi-Fi disconnect while wired active | No effect (TV monitored on eno2) |
| Cable pull | Instant via carrier detection |
| Sleep/wake | Reconnecting flag prevents racing |

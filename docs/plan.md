# Plan: Handle NIC failover for live connections

## Problem

When the route to a TV shifts from one NIC to another (e.g., wired → Wi-Fi), the daemon updates `monitored_interface` but leaves the existing connection in place. For TCP/TLS-based protocols (WebSocket), the connection is dead — it was bound to the old NIC's source IP. The tray still shows the TV as "connected" but commands silently fail until something triggers a reconnect.

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
- `Transport::Network` → "this backend uses IP networking" (scopes NM events)
- `survives_source_ip_change()` → "this backend's protocol can handle client IP migration" (scopes reconnection on NIC change)

These are independent. A WebSocket backend and an HTTP backend both use `Transport::Network`, but only one needs reconnection on IP change.

## Design: Declarative on `TvConnector` ← selected

```rust
trait TvConnector {
    fn transport(&self) -> Transport;

    /// Whether connections from this backend survive client source IP changes.
    ///
    /// TCP/TLS-based protocols (WebSocket, raw TCP) return `false` — the
    /// connection is bound to a 4-tuple and dies when the source IP changes.
    /// QUIC-based protocols may return `true` if connection migration is
    /// supported. Stateless protocols (HTTP request-response) may also
    /// return `true`.
    ///
    /// Only called for network-dependent transports (gated by
    /// `transport().is_network_dependent()`). The daemon uses this when
    /// the route to a TV shifts to a different NIC: if `false`, the
    /// existing connection is closed and a new one established via the
    /// new interface.
    ///
    /// Default: `false` (conservative — assumes connection dies).
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

## `is_connected` vs `check_health`

- `is_connected()`: local check — "does the handle look valid?" Returns `true` on a dead TCP connection until a read/write actually fails.
- `check_health()`: intended as a network probe, but unreliable — TCP `send()` succeeds into the kernel buffer even on a dead path.

Neither can reliably detect a dead-but-not-yet-failed TCP connection. The declarative approach avoids the problem entirely.

## Implementation steps

### Step 1: Add `survives_source_ip_change` to `TvConnector`

In `traits.rs`, add the method with default `false`. No backend changes needed — LG uses the default.

### Step 2: Expose via `BackendRegistry`

Add a `survives_source_ip_change(&self, config: &TvConfig) -> bool` method to `BackendRegistry`, mirroring the existing `transport_for()` pattern.

### Step 3: Remove `check_health`

Remove from: `TvConnection` (trait default), `LgConnection`, `WebOsClient`, `Connection`.

### Step 4: Simplify daemon route transition

Replace `RouteAction::CheckHealth` with a synchronous check:

```rust
// Some(old) → Some(new), old != new, TV is connected:
if !self.registry.survives_source_ip_change(&tv.config) {
    take connection, close it, emit disconnect + InterfaceUp for reconnect
}
```

No async probe, no taking the connection out and putting it back.

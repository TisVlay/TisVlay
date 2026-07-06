# Routable dials: the connect budget only spends against a network that exists

Status: **draft — under review**.

## Problem

The resume wake race, observed twice in field QA (2026-07-06): a dial's whole attempt budget (`CONNECT_MAX_ATTEMPTS = 3` × `CONNECT_BACKOFF = 3 s` ≈ a 10-second window) starts the instant the triggering event fires, while the NIC takes its own time coming back. This morning's journal shows both the near-miss and the shape of the loss:

- Boot (09:58:59): two `WoL send failed, retrying` lines before the carrier arrived; the wake connected at +9 s — inside the budget by luck.
- Resume (10:00:58): route resolutions landed 0–9 s after `system resumed`; `LG TV online after wake` at +10 s — passed, again by margin, on a wired NIC with fast DHCP.

A Wi-Fi desktop, a slow DHCP server, or STP port delays would spend the budget into a dead NIC. And the structural gap behind the race: **a spent wake dial is not `CarrierParked`** — parking happens only when a *live* connection loses its carrier/route — so the eventual carrier-up redials nothing and the TV stays off until manually asked. Every dial trigger has this race (boot replay, Startup/Resume wakes, rpc power-on fired before the network settles); resume is just where the trigger and the NIC cycle are causally synchronized, so it loses most often.

## Design

One rule instead of two compensations: **no dial without a route**. The budget then measures attempts against a network that can carry them, and the race dissolves rather than being re-armed after the fact. Scope: all network dials — plain replay/reconnect dials and wake dials alike (decided 2026-07-06; a single rule beats "wakes defer but reconnects burn").

- **Defer while routeless.** `reconcile_connections` skips a TV whose `MonitoredInterface` is `Some(None)` — the netlink monitor already maintains exactly this per-TV fact ("no usable route to this address right now"), resolved at enrollment and re-resolved on every routing-table change. The `PendingConnect` stays armed, attempts unspent, wake flag intact. When the monitor delivers a route, `RouteResolved` arrives as an inbox event, so reconcile runs in that same batch and the dial fires with its full budget — and WoL then sends on a NIC that exists.
- **Component-absence semantics stay load-bearing**: `MonitoredInterface` exists only on network TVs after their first resolution. Absent (CEC, or the pre-first-resolution instant at enrollment) → dial as today; `Some(None)` → defer; `Some(Some(_))` → dial. CEC is structurally untouched, and the boot replay dial isn't gated on a resolution that hasn't happened yet.
- **Don't count carrier-killed attempts.** The mirror race: a dial already in flight when the NIC drops fails and increments `attempts` today. In the `ConnectFailed` arm, when the route is absent at failure time, don't increment and don't schedule `RetryAt` — clear `Reconnecting` and fall back to the deferred-pending state. An attempt the network never carried was not an attempt.
- **Observability instead of a timeout.** A deferred *wake* dial publishes one `progress/power` phase, `waiting_for_network`, on the transition into deferral (wake dials are the only phase publishers today; plain dials stay phase-silent and show through the link state as they do now). Deferral is unbounded: a TV that's asked to connect keeps wanting to until the network can carry the attempt — the dual of "a TV that goes down stays down until asked." The failure budget still exists and still fails loudly; it just can't be spent into a void.

What this deliberately does not change: a TV that is unreachable while the network is fine (unplugged at the wall) still has a route — `ip route get` resolves through the default route — so it burns its attempts and fails observably exactly as today. `CarrierParked` keeps its one meaning (a live connection lost its carrier); no second overloaded marker. And rpc-initiated dials ack early as they always have, with the phase stream carrying the "why is nothing happening" answer.

## Implementation sketch

1. World, `reconcile_connections`: query gains `Option<&MonitoredInterface>`; a `Some(None)` row is skipped before any attempt/backoff bookkeeping. Publish `waiting_for_network` once per deferral episode for wake dials (a flag on `PendingConnect`, cleared when a dial actually spawns, keeps the phase single-shot).
2. World, `ConnectFailed` arm: when `MonitoredInterface` is `Some(None)`, clear `Reconnecting`, leave `PendingConnect` untouched (no increment, no `RetryAt`), and let the deferral rule own the resume.
3. Tests (`tests/dispatch.rs` / `carrier.rs` fixtures; `InEvent::RouteResolved` injects route state directly): a Resume-shaped wake with route `None` spawns zero dials and publishes `waiting_for_network`; `RouteResolved(Some)` then fires the wake with a full budget (connect count goes 0 → 1); a refusing TV with a route present still spends 3 attempts and fails (the existing contract, pinned against regression); a mid-dial route loss doesn't consume an attempt.
4. Docs: architecture.md's connection-lifecycle paragraph gains the routable-dials sentence; docs/watchers/network.md notes the route resolution now gates dial spawning, not just parking.

## Non-goals

- No budget inflation or backoff tuning — the race is removed, not slowed.
- No overall deferral timeout — unbounded by design, observable via the phase key.
- No change to `CarrierParked` semantics or to CEC dial behavior.
- No re-arm-on-carrier-up compensation path — deferral makes it unnecessary.

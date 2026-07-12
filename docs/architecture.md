# TisVlay Architecture

The current (v0.3.0+) architecture: a **bevy_ecs world** owns every daemon state machine, and **zenoh over Unix sockets** is the IPC surface. This document is the durable successor to the plan documents that drove the rewrite ([plans/spike-zenoh-ecs.md](plans/spike-zenoh-ecs.md), [plans/parity-zenoh-ecs.md](plans/parity-zenoh-ecs.md) — kept as history, like [design.md](design.md) for the retired served-D-Bus stack ≤ v0.2.0). Consumed desktop D-Bus (logind, ScreenSaver, ksni tray, notifications, `org.freedesktop.Application` GUI activation) remains zbus; nothing serves D-Bus anymore.

## Process inventory

| Process | Role | Zenoh sessions |
|---------|------|----------------|
| `tisvlay-daemon` | System service: world + backends + zenoh bridge, store, discovery, watchers | peer, listens on the system socket |
| `tisvlay-session` | Per-user gateway: user profiles, lock/unlock + idle dispatch | client → system socket; **router** listening on the user socket |
| `tisvlay-tray` | ksni tray over a live mirror, desktop notifications, settings service | connect-only peer → system; supervised **client** → user socket (it serves there) |
| `tisvlay-greeter` | Login-greeter idle client (`ext-idle-notify` → `greeter/idle`) | connect-only peer → system socket |
| `tisvlay-gui` | Slint settings window | connect-only peers → both sockets |
| `tisvlay` (CLI) | Pure zenoh client | connect-only peers → both sockets |

## Two-socket topology

Zenoh peer routing is transitive: one connected mesh would let any client read any key, so separate sockets alone don't reproduce D-Bus's session isolation. The design is an **application-level gateway**:

- **System socket** (`/run/tisvlay/zenoh.sock`, 0660, group `tisvlay`): the daemon's peer listener. Owns `tisvlay/<site>/**` except the `user/` subtree.
- **User socket** (`$XDG_RUNTIME_DIR/tisvlay/session.sock`, 0700 runtime dir): the session service's **router** listener. Owns `tisvlay/<site>/user/**` — profiles, per-TV assignments, its liveliness token. Nothing routes between the session service's two zenoh sessions except explicit application logic, so user-private keys physically never reach the system peer.
- **Clients open two sessions**, one per socket, and route by key: `user/**` on the user session, everything else on the system session. A zenoh *client* keeps a single link (`single_link: true` — a multi-endpoint connect list is failover, not fan-out), so two sessions are structural, not stylistic.

### Link modes and the serve-side law

Every *consuming* link is a **connect-only peer** (`mode=peer`, empty listen, `exit_on_failure: false`): the session opens before any link exists, dials the hub forever, and liveliness tokens are the availability signal (the `NameOwnerChanged` equivalent). This buys start-order freedom — any client can boot before the daemon.

The law learned empirically: **a connect-only peer leaf cannot fan out what it serves.** Queryables, liveliness tokens, and cached publishers it declares are invisible to *other* leaves re-broadcast through a hub (both peer-mode and router-mode hubs) — but the **directly-dialed hub itself does observe them** (verified on zenoh 1.9's Regions routing; the rule governs third-party fan-out, not observation by the endpoint you dial). So anything that must serve *to other leaves* uses a **client-mode link into a router** (routers relay client-declared entities), supervised with a fail-fast open + retry loop so it re-serves after a hub restart — hence the user socket is a router and the tray's settings service is a supervised client link. Where only the dialed peer needs the signal, a connect-only peer suffices: the session declares its `alive/session` token on its system link and the daemon (the peer it dials) observes both the PUT and the crash-time DELETE.

## Key space

[`tisvlay-wire`](../crates/tisvlay-wire/src/lib.rs) is the single vocabulary source: key constructors (`keys::*`), payload structs (which double as bevy components and serde wire payloads), typed `WireError`, and session bootstrap configs. The shape by prefix, all under `tisvlay/<site>/`:

```
tv/<uuid>/state/{power,volume,input,backlight,link}   cached pubs (state fan-out)
tv/<uuid>/meta/{identity,config}                      cached pubs (name, vendor, managed, profile, …)
tv/<uuid>/rpc/<verb>                                  queryables (set-*, power-*, get/update-config, re-pair, …)
tv/<uuid>/progress/power                              power-cycle phases (wol_sent → connecting → connected/failed)
pairing/<uuid>/progress                               pairing phases (awaiting_tv → complete/failed, typed codes)
profile/system/<uuid>                                 cached pubs, PUT/DELETE lifecycle
profile/rpc/<verb>                                    create, update, duplicate, delete, defaults, effective
manager/vendors[, /<id>/schema]                       vendor catalog + pairing-form schema queryables
manager/rpc/<verb>                                    add-tv, pair, discover[-start/-stop], enrich, info, …
discovery/<session>/found/<id>                        streaming findings + alive/discovery/<session> token
event/session/<name>, event/connect-failed            observability taps
greeter/{idle,timeout}                                greeter idle events + published timeout
alive/{daemon,session,…}                              liveliness tokens (session = presence on its system link)
--- user socket only ---
user/profile/<uuid>, user/profile/rpc/<verb>          user-profile catalog + CRUD
user/tv/<uuid>/profile                                per-TV user-profile assignment
user/alive/{session,tray}                             per-user service liveliness
```

Secrets never reach the wire: `get-config` redacts through the vendor `config_schema` visibility tags (`Secret` fields dropped daemon-side).

## The world

[`tisvlay-world`](../crates/tisvlay-world/src) holds a bevy_ecs `World` owned by a single **reactor** task. All async I/O lives in tokio tasks outside the world; the world mutates only at sync points:

1. The reactor batches its inbox (`InEvent`: commands from the bridge, backend facts, watcher events, pairing updates, ticks) and writes them as bevy messages.
2. One `Schedule` run: ingest systems → dispatch/reconcile → derive/publish tail (deterministic ordering via a chained schedule).
3. Drained effects leave the world: `WireEffect` (publish/delete/reply) to the daemon's zenoh bridge, `Persist` snapshots to the store task, dial spawns as tokio tasks, route-monitor requests to the netlink watcher.

Payload structs are components — `publish_changed<T>` turns bevy change detection directly into wire publishes, and `derive_link_state` derives `Link` from world structure (`connected ⇔ ConnectionHandle exists`). Connection lifecycle is marker components: `PendingConnect` (the **single dial gate** — every recovery path funnels through `redial()`), `Reconnecting` (dial in flight), `RetryAt` (backoff), `CarrierParked` (network down), `AwaitingRoute` (dial deferred on a known route absence), `ConnectionHandle` (live task channel; dropping it closes the connection — RAII, no drain layer). A TV that goes down stays down until asked; only carrier/route restore redials parked TVs. **Dials are routable or deferred**: a `MonitoredInterface` reading `Some(None)` defers the dial with the attempt budget reset — attempts count only route-carried dials (a failure whose route vanished mid-flight returns its attempt but keeps the backoff, bounding route flap) — and the route's return resumes it structurally through the same reconcile pass, with `awaiting_network` on the power-phase key as the wake path's observable. Component absence is load-bearing: no resolution can precede the first dial (monitoring is requested by the dial spawn), so first dials, fixtures, and CEC never defer.

Screen actions ride a reliable tier with `DispatchVerify` as the **single screen-intent latch**: send, then verify against the cached power state after the backend's verify delay, retry within the budget, and escalate an unmet `screen_on` to the power path (in place for standby-reachable transports, wake dial otherwise). The latch survives a wake-driven teardown — verification defers while a dial is pending, and the reconnect re-asserts the panel and restarts the clock — with an `escalated` flag capping every dispatch at one wake dial, so a TV that ACKs dark can't ping-pong wakes. Entry points that route `screen_on` onto the power path instead of erroring or retrying into a void: a wake-needy cached power state on a live link (the pre-flight — `PowerState::needs_wake_preflight()`, where `Unknown` counts only for network transports since a standby-reachable TV reads `Unknown` while genuinely on), and a disconnected wake-capable TV (the elevation — acked when the dial arms, like `power-on`). The power path itself routes by transport: standby-reachable buses wake in place over the live handle, network transports spend the dispatch's wake dial (LG has no in-band `power_on` — WoL is the only wake). A `screen_off` while a dial is in flight latches and lands on the reconnect (the locked-idle timer racing a boot wake); a disconnected `screen_off` with no dial is a satisfied no-op. Exhausted intents die observably on the `event/dispatch-failed` key.

Primary-input restore rides the same shape: an event mapping can carry a `restore_input` modifier (string-or-table profile form, stock-on for the session-start wakes), which the resolving side - the session for user events, the world for system events - places on the `SetScreen`/`PowerOn` payloads as **resolved intent**; manual clients never set it, so manual actions are input-neutral by construction. A restore-flagged dispatch with a configured `primary_input` arms `DesiredInput`; the `reconcile_input` drain waits through dials, requires the advertised `INPUT` capability (CEC sheds the intent structurally - its wake already selects the adapter's port), compares the mirrored input state, re-sends within the verify budget, clears on first compliance, and fails onto `dispatch-failed`. A manual `switch-input` cancels any pending intent - the user is authoritative. The `automated` gate itself is profile policy (root parity): `automate_when` resolves through the assigned-profile - default-profile chain with an `Always` floor, and `meta.automate_when` is a read-only projection of that resolution - there is no per-TV storage.

Two invariants with their own history: **managed gates automation, not connectivity** (unmanaged TVs stay connected for state and manual control; only profile-driven actions stop), and **pairing terminals gate on the first real dial** (`PairVerifying`: `complete` publishes only once the TV actually connects; a fresh enrollment that fails its first dial rolls back entirely). Transport characteristics (`TransportTraits`, `supports_wake`) are config-derived by the reactor ahead of every dial — never inferred from dial outcomes.

Same-batch hazard: systems that read `MessageReader<InEvent>` and mutate via `Commands` see pre-flush component state for every message in a batch, so same-key message streams collapse to the latest per key before acting (`apply_route_resolutions`, `handle_carrier_events`).

Reply-side backpressure is load-bearing: backend commands ack on TV completion (`BackendEvent::CommandDone`), not on enqueue — this is what makes the tray's `Coalescer` "latest wins" actually bound queue depth.

## Backends

Vendor backends implement four `tisvlay-core` traits and register as one `VendorReg` entry in the daemon — see [backends/implementing-a-vendor.md](backends/implementing-a-vendor.md). The vendor config travels as an opaque `toml::Value`; the schema's first field is the **primary descriptor** (LG: IP, CEC: device path), used by log lines, the CLI list, the wizard's manual entry, and route monitoring.

## Persistence

The daemon's store is one TOML file (`/var/lib/tisvlay/daemon.toml` packaged, `--config` in dev) holding TVs (with credentials), settings, and system profiles. Config-shaped changes flip a `DirtyStore` flag; `snapshot_store` emits a `Persist` effect; the store task debounces and writes atomically (tmp + rename). Boot replays rows as `AddTv` commands. `--seed` applies a bottom-layer config only when the daemon has none, with UUIDv5 content-derived identity so re-seeding never duplicates TVs. The session service persists user profiles the same way under `~/.config/tisvlay/session.toml`.

## Watchers

- **netlink** ([watchers/network.md](watchers/network.md)): carrier transitions per NIC + route-based device resolution (`ip route get` per TV, `MonitoredInterface` scoping) — catches cable pulls and NM software disconnects without NetworkManager. MAC enrichment uses an `RTM_GETNEIGH` neighbour dump (`/proc/net` is invisible under the packaged sandbox's `ProcSubset=pid`).
- **DPMS**: per-connector sysfs watchers auto-spawned by the desktop probe's EDID matching; `--dpms-connector` adds a global-dispatch override. KWin writes DPMS through to the legacy sysfs attribute, so no compositor protocol is needed.
- **logind**: PrepareForSleep → sleep/resume events + shutdown inhibitor.
- **Session-side** ([the session crate](../crates/tisvlay-session/src)): ScreenSaver lock/unlock over the session bus (with a `GetActive` seed at startup — KDE's auto-login lock activates before the service and arrives with no edge), and a three-notification `ext-idle-notify` watch — a pinned 200 ms lock-wake detector, plus per-lock-state idle timeouts (`locked_/unlocked_idle_timeout_seconds`), each gated to its own lock state. The session also mirrors per-TV `state/power` for the **idle drift re-assert**: a panel that lights itself while an idle action holds (a stray remote press at the lock screen) gets one idle-timeout of grace, then the idle action re-dispatches — gated on `automated` (so `automate_when: on_primary` users watching another input are never fought), dark intents only, cancelled by any activity or lock transition. Desktop idle timers are edge-triggered and can't see TV-side drift; this closes that gap without making the world enforce persistent intent.

## Deployment and authorization

The packaged daemon runs as the `tisvlay` system user under a hardened unit; it chmods its socket 0660 after bind (the unit runs `UMask=0077`), and **`tisvlay` group membership is the authorization boundary** (`services.tisvlay.users` in the NixOS module). Within a socket's group there is no per-method authorization — the accepted regression vs D-Bus policy; polkit-shaped gating is a post-parity design if ever needed. Connect-side socket defaults fall back from the dev socket to `/run/tisvlay/zenoh.sock`; listen-side defaults never fall back (a dev daemon must not take over the packaged socket). Sandbox lesson worth remembering: hardening options change what the code can see (`ProcSubset=pid` broke a `/proc/net` parser that worked in dev) — prefer netlink/syscall interfaces over procfs.

## Empirical zenoh laws

Learned the hard way; violating any of these produces silent wrong behavior, not errors:

- Consume-only peer leaves / serve-via-client-link (topology section above).
- Advanced-cache hydration is subscriber-history-only: the cache's queryable sits under a verbatim `@adv` suffix, plain `get`s can't reach it.
- Multi-reply queries (discovery streaming) need `ConsolidationMode::None` querier-side, or consolidation eats same-key replies.
- `timestamping/enabled` must be on daemon-side for cache ordering.
- Publishers guarantee no cross-key sample ordering — tests wait on combined predicates in one drain, never two sequential waits on one subscriber.
- A `get` matching zero queryables completes instantly with no reply — and token arrival races queryable-route propagation, so liveliness cannot gate rpcs. Clients therefore gate every rpc on **querier matching status** (`tisvlay-wire`'s `caller::Caller`: matching ⇒ routable, absent-past-deadline ⇒ `rpc.unavailable`) — the pin is `crates/tisvlay-wire/tests/querier_matching.rs`, and every client (SDK, CLI, tray) rides the shared module.
- **Availability is liveliness, only; rpc outcomes are content.** Presence displays key on liveliness flags (the GUI writes every Slint copy through one `publish_availability` function); a failed or slow rpc renders as missing content, never as offline.
- Declare liveliness **after** rpc queryables: serve-side hygiene so a token never precedes the surface at the origin. (Client-side, the matching gate above covers the propagation window this ordering cannot.)
- Zenoh rejects tokio's current-thread scheduler — zenoh-touching tests need `#[tokio::test(flavor = "multi_thread")]`.
- Stale Unix socket files are fine: zenoh's `.lock` protocol takes over an unclean listener path on the next bind.

## Testing

See [testing.md](testing.md). Layers: `tisvlay-test` (private `dbus-daemon`, fake logind/ScreenSaver) for consumed-D-Bus adapters; `tisvlay-mock` (always-registered fake vendor, deliberately `Transport::Network` so carrier/route tests exercise real code paths); wire-level integration tests in each binary crate's `tests/` booting real stacks on temp sockets.

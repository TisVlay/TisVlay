# TisVlay Architecture

The current (v0.3.0+) architecture: a **bevy_ecs world** owns every daemon state machine, and **zenoh over Unix sockets** is the IPC surface. This document is the durable successor to the plan documents that drove the rewrite (the spike and parity plans, now retired to git history; [design.md](design.md) remains for the retired served-D-Bus stack ≤ v0.2.0). Consumed desktop D-Bus (logind, ScreenSaver, ksni tray, notifications, `org.freedesktop.Application` GUI activation) remains zbus; nothing serves D-Bus anymore.

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
- **User socket** (`$XDG_RUNTIME_DIR/tisvlay/session.sock`, 0700 runtime dir): the session service's **router** listener. Owns `tisvlay/<site>/user/**` — profiles, per-TV assignments, its liveliness token. Nothing routes between the session service's two zenoh sessions except explicit application logic, so user-private keys physically never reach the system peer. The session's split (user router + system peer) stands on **isolation alone**, not on `single_link`: a single session bridging both sockets would transitively route the two key spaces (user leaves could read `tv/**`/`manager/**`; the daemon could see `user/**`), so even with the serve-side law fully corrected, merging them defeats the gateway.
- **Clients open two sessions**, one per socket, and route by key: `user/**` on the user session, everything else on the system session. A zenoh *client* keeps a single link (`single_link: true` — a multi-endpoint connect list is failover, not fan-out), so the *clients'* two sessions are structural, not stylistic (a distinct rationale from the session service's isolation split above).

### Link modes and the serve-side law

Every *consuming* link is a **connect-only peer** (`mode=peer`, empty listen, `exit_on_failure: false`): the session opens before any link exists, dials the hub forever, and liveliness tokens are the availability signal (the `NameOwnerChanged` equivalent). This buys start-order freedom — any client can boot before the daemon.

The law learned empirically: **a connect-only peer leaf cannot fan out what it serves.** Queryables, liveliness tokens, and cached publishers it declares are invisible to *other* leaves re-broadcast through a hub (both peer-mode and router-mode hubs) — but the **directly-dialed hub itself does observe them** (verified on zenoh 1.9.0's ("Longwang", 2026-04-16) Regions routing rework, which is where the observation changed — the law was accurate for 1.0–1.8; the rule governs third-party fan-out, not observation by the endpoint you dial). So anything that must serve *to other leaves* uses a **client-mode link into a router** (routers relay client-declared entities), supervised with a fail-fast open + retry loop so it re-serves after a hub restart — hence the user socket is a router and the tray's settings service is a supervised client link. Where only the dialed peer needs the signal, a connect-only peer suffices: the session declares its `alive/session` token on its system link and the daemon (the peer it dials) observes both the PUT and the crash-time DELETE.

## Key space

[`tisvlay-wire`](../crates/tisvlay-wire/src/lib.rs) is the single vocabulary source: key constructors (`keys::*`), payload structs (which double as bevy components and serde wire payloads), typed `WireError`, and session bootstrap configs. The shape by prefix, all under `tisvlay/<site>/`:

```
tv/<uuid>/state/{power,volume,input,backlight,link}   cached pubs (state fan-out)
tv/<uuid>/meta/{identity,config,vendor}               cached pubs (name+primary descriptor, managed/profile, display rows)
tv/<uuid>/rpc/<verb>                                  queryables (set-*, power-*, get/update-config, re-pair, …)
tv/<uuid>/progress/power                              power-cycle phases (wol_sent → connecting → connected/failed)
pairing/<uuid>/progress                               pairing phases per attempt (awaiting_tv → complete/failed, typed codes)
profile/system/<uuid>                                 cached pubs, PUT/DELETE lifecycle
profile/rpc/<verb>                                    create, update, duplicate, delete, defaults, effective
manager/vendors[, /<id>/schema]                       vendor catalog + pairing-form schema queryables
manager/rpc/<verb>                                    add-tv, pair, discover[-start/-stop], enrich, info, …
discovery/<session>/found/<id>                        streaming findings + alive/discovery/<session> token
event/session/<name>, event/connect-failed            observability taps
greeter/{idle,timeout}                                greeter idle events + published timeout
remote/session                                        remote-session fact mirror (cached pub; daemon hydrates → world gate)
alive/{daemon,session,…}                              liveliness tokens (session = presence on its system link)
--- user socket only ---
user/profile/<uuid>, user/profile/rpc/<verb>          user-profile catalog + CRUD
user/tv/<uuid>/profile                                per-TV user-profile assignment
user/remote/session, user/remote/rpc/<verb>           remote-session fact (cached pub) + manual arm/disarm verbs
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

Primary-input restore rides the same shape: an event mapping can carry a `restore_input` modifier (string-or-table profile form, stock-on for the session-start wakes), which the resolving side - the session for user events, the world for system events - places on the `SetScreen`/`PowerOn` payloads as **resolved intent**; manual clients never set it, so manual actions are input-neutral by construction. A restore-flagged dispatch with a configured `primary_input` arms `DesiredInput`; the `reconcile_input` drain waits through dials, requires the advertised `INPUT` capability (CEC sheds the intent structurally - its wake already selects the adapter's port), compares the mirrored input state, re-sends within the verify budget, clears on first compliance, and fails onto `dispatch-failed`. A manual `switch-input` cancels any pending intent - the user is authoritative. The `automated` gate itself is profile policy (root parity): `automate_when` resolves through the assigned-profile - default-profile chain with an `Always` floor, and `meta.automate_when` is a read-only projection of that resolution - there is no per-TV storage. A second, orthogonal gate rides the same dispatch: while a TV is in the world's `RemoteArmed` set (fed by the `remote/session` mirror), an event whose resolved action has `while_remote = false` is suppressed — a pure gate (suppression only, never origination), additive to `automated`, that keeps a remote session's injected input from waking the panel for an empty room. The stock floor gates wake-ish events and exempts off-going ones, and it is per-event invertible; see [remote/detection.md](remote/detection.md).

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
- **Session-side** ([the session crate](../crates/tisvlay-session/src)): lock/unlock and idle, each over the transport the running desktop provides. **Lock** is logind `LockedHint` on the system bus — the one standardized cross-desktop lock-state signal (both kscreenlocker and gnome-shell set it), followed on this user's graphical *seat* session (found by enumeration: the service runs in the user@ manager scope, not a logind session, and keys on the seat — not the session `Type` — so a `startx` login qualifies alongside a display-manager one; the seat's `Active` session wins). It seeds silently at first attach (an auto-login lock arrives with no edge, and re-blanking a parked TV would be wrong) but fires a change that landed during a reconnect gap. This replaced the KDE-only `org.freedesktop.ScreenSaver` `ActiveChanged` interface — never a freedesktop spec, a de-facto `gnome-screensaver`/`xscreensaver` one KDE kept; GNOME's `gsd-screensaver` implements only the standardized idle-inhibit subset, so it never signals lock state. **Idle** is a three-notification watch — a pinned 200 ms lock-wake detector plus per-lock-state idle timeouts (`locked_/unlocked_idle_timeout_seconds`), each gated to its own lock state — over the `ext-idle-notify-v1` Wayland protocol (KWin, wlroots) with an `org.gnome.Mutter.IdleMonitor` D-Bus fallback where the compositor doesn't advertise it (GNOME/Mutter, still true as of GNOME 51). Because those tags gate on *opposite* lock states and `ext-idle-notify` fires `Idled` once per idle period (KWin delivers it even while locked), the lock state is seeded **synchronously** from `LockedHint` before the idle adapter is spawned — the supervised watcher's async seed can land after the first (locked) idle transition, which would drop it against the `is_locked = false` default *and* drop the later unlocked transition once the seed flips the flag, so a session that boots already locked (`LockOnStart`, autologin-then-lock) would otherwise never blank. The login **greeter** ([tisvlay-greeter](../crates/tisvlay-greeter/src)) drives the same two idle transports at the login screen (wayland socket first, Mutter D-Bus fallback), attaching to the DM's greeter-only session target — `plasma-login-wayland.target` (KWin) or `gnome-session@gnome-login.target` (GDM). The session also mirrors per-TV `state/power` for the **idle drift re-assert**: a panel that lights itself while an idle action holds (a stray remote press at the lock screen) gets one idle-timeout of grace, then the idle action re-dispatches — gated on `automated` (so `automate_when: on_primary` users watching another input are never fought), dark intents only, cancelled by any activity or lock transition. Desktop idle timers are edge-triggered and can't see TV-side drift; this closes that gap without making the world enforce persistent intent.
- **Remote-session detectors** ([remote/detection.md](remote/detection.md)): independent session-side tasks — a manual/hook verb, a PipeWire capture-node watcher, a `BecomeMonitor` portal watcher (attributed by caller exe), a logind graphical-`Remote=yes` watcher, and a Sunshine `/serverinfo` level poll — each reporting a kind-tagged source into an aggregator that crosses live sources with each TV's resolved user profile to compute the per-TV `armed` set. The set publishes as the `user/remote/session` fact and mirrors to `remote/session`; the world's `RemoteArmed` resource gates dispatch from it. Detection is the opt-in — nothing arms on a stock desktop.

## Deployment and authorization

The packaged daemon runs as the `tisvlay` system user under a hardened unit; it chmods its socket 0660 after bind (the unit runs `UMask=0077`). Access is layered: **`tisvlay` group membership is the durable grant** (`services.tisvlay.users` in the NixOS module; `tisvlay enable-access` is its one-click polkit enrollment) — it covers SSH, remote, headless, and lingering services — and the daemon additionally applies named POSIX ACL grants on the socket: the display manager's greeter group (`--greeter-access`/`--greeter-group` — static for the daemon's lifetime, so the dynamic-userdb GDM greeter can reach the socket) and, **presence-scoped**, each seat's active graphical user (`--seat-access`, default on — `uaccess` semantics for our socket; the entry follows logind's `Seat.ActiveSession`, gated `Class=user` ∧ `Type∈{x11,wayland}` ∧ `¬Remote`). Presence grants are transient by design: switching away removes the entry while active-session arbitration simultaneously suppresses that user's automation; already-open connections persist (the kernel checks at `connect()`). Within the socket there is no per-method authorization — the accepted regression vs D-Bus policy; polkit-shaped gating is a post-parity design if ever needed. Integration detail — per-channel wiring, maintainer-script lifecycle, ACL semantics, verification, and lessons — lives in [packaging/README.md § Socket access & the ACL integration](../packaging/README.md#socket-access--the-acl-integration). Connect-side socket defaults fall back from the dev socket to `/run/tisvlay/zenoh.sock`; listen-side defaults never fall back (a dev daemon must not take over the packaged socket). Sandbox lesson worth remembering: hardening options change what the code can see (`ProcSubset=pid` broke a `/proc/net` parser that worked in dev) — prefer netlink/syscall interfaces over procfs.

## Empirical zenoh laws

Learned the hard way; violating any of these produces silent wrong behavior, not errors:

- Consume-only peer leaves / serve-via-client-link (topology section above).
- Advanced-cache hydration is subscriber-history-only: the cache's queryable sits under a verbatim `@adv` suffix, plain `get`s can't reach it.
- Multi-reply queries (discovery streaming) need `ConsolidationMode::None` querier-side, or consolidation eats same-key replies.
- `timestamping/enabled` must be on daemon-side for cache ordering. Two-sided: a *bare* `.cache()` AdvancedPublisher hard-fails at construction without an HLC (timestamping is per-role — router on, peer/client off), but the project's standard `.cache(…).sample_miss_detection(…)` shape re-sequences by a plain counter *after* `.cache()` set the timestamp mode, so it constructs regardless of timestamping. So no timestamping requirement attaches to a connect-only-peer cached publisher (e.g. the session's `remote/session` mirror the daemon hydrates over its dialed link).
- Publishers guarantee no cross-key sample ordering — tests wait on combined predicates in one drain, never two sequential waits on one subscriber.
- A history subscriber's replayed cache and the live samples racing it have no guaranteed relative order, and per-publisher sequencing never delivers an older sample after a newer one: when a live sample wins, the replayed history behind it is dropped. A key that outlives sessions (pairing progress) therefore can't let watchers infer "theirs" from position — each sample carries its session's `attempt`, returned by the starting rpc, and watchers drain only that attempt.
- A `get` matching zero queryables completes instantly with no reply — and token arrival races queryable-route propagation, so liveliness cannot gate rpcs. Clients therefore gate every rpc on **querier matching status** (`tisvlay-wire`'s `caller::Caller`: matching ⇒ routable, absent-past-deadline ⇒ `rpc.unavailable`) — the pin is `crates/tisvlay-wire/tests/querier_matching.rs`, and every client (SDK, CLI, tray) rides the shared module.
- **Availability is liveliness, only; rpc outcomes are content.** Presence displays key on liveliness flags (the GUI writes every Slint copy through one `publish_availability` function); a failed or slow rpc renders as missing content, never as offline.
- Declare liveliness **after** rpc queryables: serve-side hygiene so a token never precedes the surface at the origin. (Client-side, the matching gate above covers the propagation window this ordering cannot.)
- Zenoh rejects tokio's current-thread scheduler — zenoh-touching tests need `#[tokio::test(flavor = "multi_thread")]`.
- Stale Unix socket files are fine: zenoh's `.lock` protocol takes over an unclean listener path on the next bind.

## Testing

See [testing.md](testing.md). Layers: `tisvlay-test` (private `dbus-daemon`, fake logind/ScreenSaver) for consumed-D-Bus adapters; `tisvlay-mock` (always-registered fake vendor, deliberately `Transport::Network` so carrier/route tests exercise real code paths); wire-level integration tests in each binary crate's `tests/` booting real stacks on temp sockets.

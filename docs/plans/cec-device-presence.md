# Local-device presence: reconnect CEC when its adapter returns

A CEC TV's connection depends on a local device node (`/dev/cecN`), not a network route. Today nothing tells the daemon when that node comes and goes. A failed CEC dial spends its budget (`CONNECT_MAX_ATTEMPTS` dials `CONNECT_BACKOFF` apart; about 6 s for an `ENOENT` open) and gives up, and a dropped CEC connection is never redialed on its own: `Disconnected` removes the handle and `PendingConnect` is "never inserted by a disconnect". So a re-attached adapter, or one that registers after the daemon's first dials, leaves CEC down until a manual `reconnect` or the next wake (found in [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md), § Related findings).

This plan gives local-device TVs the same event-driven model network TVs already have for their route: an availability fact fed by the daemon, a dial gate that waits on it, and a redial when it returns. It also adds the CEC claim and connect logs that were missing during that investigation. It supersedes steps 4 and 5 of [cec-pulse8-autonomous.md](cec-pulse8-autonomous.md). Design, correctness and maintainability reviews are folded in below.

## Decisions

- **Availability model, mirroring routes.** Network TVs carry `MonitoredInterface` (from the netlink route monitor), and the dial gate defers on a known absence instead of failing. Local-device TVs get the equivalent fact, `DeviceAvailable(bool)`, from a daemon-side device watcher. ("Presence" already names zenoh liveliness and the seat ACL's grant, so the code says "available".)
- **Separate facts, one gate.** Routes carry interface identity and shift semantics that devices lack, so the two facts stay separate. What they share is folded together: one deferral marker carrying the reason (`AwaitingDependency(Network | Device)`, replacing `AwaitingRoute`), one "dependency absent" predicate used by the dial gate, the attempt-returning failure branch and the link derivation, and one forget effect (`Effect::ForgetRoute` becomes `ForgetMonitors`) so the `TvId` hook drops both monitors.
- **Transport-agnostic in core.** A connector declares its local device through a new default method on `TvConnector` (`None` for network backends). The world and daemon only see "a TV whose connection needs this device path". CEC is the first user; a future serial or USB backend reuses it. No subsystem field: re-checking every monitored path on a udev event is a few `stat`s, and a Pulse-Eight's tty and its CEC node are different subsystems anyway.
- **udev events, then a re-check.** The watcher listens on the udev multicast group of `NETLINK_KOBJECT_UEVENT`, whose messages come after udev's rules have run (permissions, aliases). On any `add` or `remove` it re-checks every monitored path and reports changes only. Events are never trusted beyond triggering that re-check.
- **Availability means "usable", not "exists".** A node counts as available once it exists *and* udev has initialized it (`/run/udev/data/c<major>:<minor>` exists, libudev's own test), whenever `/run/udev` exists. That keeps the initial check and the safety-net poll from seeing devtmpfs's root-only node before udev applies `GROUP`/`MODE`.
- **Removal parks, return redials.** The rule in [docs/architecture.md](../architecture.md) ("a TV that goes down stays down until asked; only carrier/route restore redials parked TVs") generalizes: the unavailable edge drops the handle and parks the TV, exactly as a lost route does, and the available edge redials parked TVs. Dropping the handle at once also closes the old adapter's descriptors, so the kernel can reuse its minor number (see § Renumbering).
- **Backoff as the safety net, never for wakes.** A local-device TV whose device is available but whose dial fails at the device level (open or claim errors) keeps retrying: three tries 3 s apart, then the gap doubles to a 60 s cap. A wake dial keeps today's three-try budget and publishes `Failed`; the TV then continues as a plain `wake: false` retry and any stale screen intent riding the dial (`DispatchVerify`) is dropped. Network TVs keep the three-try budget, since network events handle their recovery.
- **Failure logs: warn once, info on recovery.** The first failed dial of a streak logs at warn with the error and publishes `event/connect-failed`; repeats go to debug and publish nothing; the recovery logs at info with the attempt count. A network TV that spends its budget logs at warn once, naming the recovery (`tisvlay reconnect`). The attempt counter saturates.
- **Claim logs live in the CEC backend.** `tisvlay-cec`'s link logs each claim at info (device, logical address, OSD name or none). A claim on a Backup address (12, 13) or Unregistered (15) logs at warn: the TV may not be able to name the device.
- **"Waiting" is visible to clients, derived from facts.** `Link` gains `waiting_for: Option<LinkWait>` (`Network`, `Device`; a typed enum with `as_str`), and `PowerPhase` gains `AwaitingDevice` next to `AwaitingNetwork`. `waiting_for` is derived in `derive_link_state` from the facts (`MonitoredInterface(None)`, `DeviceAvailable(false)`), not from the deferral marker, which only exists while a dial is pending. Both types stay exhaustive, so this is a `feat` with a lockstep minor bump, said in the commit body. `Network` is new behavior for LG and comes with carrier tests.
- **User guide edits held** until the CEC follow-ups near their end.

## Held wakes (decided)

A wake held on a missing dependency (the boot's `on_startup` power-on before inputattach or the network is up) should go out when the dependency returns a few seconds later, never hours later. Two rules, for **both** backends:

- **Expiry.** A held wake gives up after 60 s, publishes `Failed`, and the TV continues as a plain reconnect, which never powers it on. LG's deferral only happens with no route at all (a PC on any network still resolves a route, so its dial runs and fails within the budget), so the window changes LG behavior only when the network was down for over 60 s: today the TV turns on when it returns, with the expiry it just reconnects.
- **Supersession.** A newer off intent cancels a held wake: a power-off, or a screen-off on a backend that emulates the screen as power. Today the only place a pending dial is dropped is budget exhaustion (`facts.rs`), and a power-off on a disconnected TV is a no-op (`dispatch.rs`), so a boot wake held across a lock-and-idle power-off still turns the TV on when the route or adapter appears. LG's screen-off latch (`DispatchVerify`) re-asserts only after that wake, which flashes the panel. The TV still reconnects, as a plain connect.

## Retry scope (decided)

Only device-level failures retry forever (the node won't open, or the claim fails), which a busy adapter or udev's permission rule can fix on their own. A TV-level failure (the adapter claims, but the TV never answers its power query, e.g. unplugged at the wall) keeps today's three-try budget; the next wake or `reconnect` tries again, as now. Retrying it forever would re-claim the adapter and talk on the bus every minute while nobody listens. The connect errors need a device-level classification for this (a `DialFailure` code, or a typed error from the link's open and claim).

A stable udev alias for Pulse-Eight nodes is out of scope: [docs/ideas/cec-stable-device-alias.md](../ideas/cec-stable-device-alias.md).

## Renumbering

The kernel frees a CEC node's minor only when the last descriptor on it closes. If any descriptor on the old adapter is still open when the new one registers (the follower monitor, the transmit worker, a discovery probe), `/dev/cec0` comes back as `/dev/cec1` and the configured path never becomes available. Dropping the handle on the unavailable edge closes ours; the watcher also logs when a node it doesn't monitor appears while a monitored one is unavailable, so the user can see what happened.

## Implementation

Commit sequence, each one building and testing on its own:

### 1. `feat(cec)`: claim logs

- [x] Info on each claim (device, address, name or none); warn on 12, 13 or 15. Standalone.

### 2. `feat`: the availability model

- [x] Core: `TvConnector::local_device(&self, &VendorConfig) -> Option<LocalDevice>`, default `None`; `LocalDevice` holds the path, `#[non_exhaustive]` with a constructor, per [docs/build.md](../build.md) § Public API policy. `tisvlay-cec` returns its configured `device`. The mock backend gets a `local_device` config knob (like `reachable_in_standby`) for world tests.
- [x] Daemon `devices.rs`, shaped like `netlink.rs`: a pure state struct that reports changes only (deduplicated per TV, re-reported on every `Monitor` so a device-path change through update-config lands), a pure udev-message parser, and an injectable path check. One socket on the udev group through `rtnetlink::sys` (the re-exported `netlink-sys`; no new direct dependency). Bind before the first check, so an event between the two isn't lost. On `ENOBUFS` (a boot coldplug burst), re-check every path and keep going, as `netlink.rs` resyncs. Safety net: re-check unavailable paths every 60 s.
- [x] Reactor: `Monitor` from `request_route_monitor`'s call sites (enrollment and dial spawn); removal through the generalized forget effect.
- [x] World: `DeviceAvailable`, `AwaitingDependency` (replacing `AwaitingRoute`; the waiting reason is read from the facts, not stored on the marker), `Parked` (replacing `CarrierParked`), the shared predicate in the gate. Availability ingest runs before `reconcile_connections`, as route ingest does, so a return redials in the same batch.
- [x] Startup: when a device watcher is attached, a local-device TV's first dial waits for its first availability report (a `stat`, so instant), instead of racing it and logging a spurious failure at every boot. Only the daemon's `run` attaches the watcher; the public `serve` the wire tests use doesn't, so their CEC fakes dial as before.
- [x] The unavailable edge parks the TV and drops its handle. The available edge redials parked TVs and, for any TV without a handle or a dial in flight, clears `RetryAt` and resets `attempts`, keeping the `wake` flag.
- [x] `Disconnected` on a local-device TV redials after one `CONNECT_BACKOFF`, and the gate defers if the device is gone. That covers a fast remove-and-add the watcher saw as "no change".
- [x] A dial that fails while its dependency is absent returns the attempt and keeps the backoff, publishing no `connect-failed` (the existing routeless branch, generalized).
- [x] A TV in `PairVerifying` bypasses the availability gate, so a re-pair with the adapter missing fails with its typed reason instead of hanging. The bypass covers the route gate too, so an LG re-pair with no route fails the same way.
- [x] World tests in a new `tests/it/device.rs`, modeled on the carrier suite; daemon tests for the parser and the state struct.

### 3. `feat`: the safety-net backoff and failure logs

- [x] `retry_delay(attempts) -> Duration` as one pure function (the persistent decision stays with the caller) next to `CONNECT_BACKOFF`, with unit tests; a saturating attempt counter. A TV with a dial in flight when its device returns is marked so that a failure retries at once (with a constant 3 s backoff this only matters once the gap grows).
- [x] Persistent retry only for device-level failures of local-device TVs (§ Retry scope), with the connect errors classified; wake dials keep the budget and fall back to a plain retry.
- [x] Failure streak logging (warn on the first failure, debug after, info on the recovery) and `connect-failed` for the budgeted attempts only, so a persistent retry publishes nothing beyond them (no client consumes the event; it's an observability tap); the warn when a budget runs out. The device-level classification is a new core `Error::DeviceUnavailable` that the CEC connector wraps its adapter setup in, carried world-side as `DialFailure.device_level`.

### 3b. `feat`: held-wake expiry and supersession (both backends)

- [x] A held wake records when it was armed; after 60 s it publishes `Failed` and continues as `wake: false` (§ Held wakes).
- [x] A power-off (a profile action, or the `power-off` rpc, which a cancelled wake now answers with success instead of "not connected"), or a screen-off on an `emulate_screen_via_power` backend, downgrades a pending wake to `wake: false`, reports it failed, and drops a stale `DispatchVerify` with `target_on`. A wake already dialing is left to finish; only a held one (between attempts or deferred) is cancelled.
- [x] World tests for both, on a network TV and a local-device TV.

### 4. `feat` (wire-visible): the waiting state

- [x] Protocol: `Link.waiting_for` (`LinkWait`), `PowerPhase::AwaitingDevice`; `every_power_phase_decodes` gains the variant. A wake's waiting phase now goes out when the wake starts waiting (its `HeldWake` edge), not on the deferral's first edge, so a wake that upgrades a reconnect already deferred still reports it.
- [x] World: `derive_link_state` fills it from the facts; the `Link` literals in `publish.rs` and `systems/mod.rs`.
- [x] SDK `TvInfo` (`#[non_exhaustive]`, additive).
- [x] GUI: the view model, `state.rs` and `state_watch.rs`, a new property for the steady state (the existing text keys only on the power-cycle phase), wording that clearly means the adapter, not the TV; `just i18n-extract`.
- [x] Tray: `TvView`, and the new `PowerPhase` arm its match will demand.
- [x] CLI: `list` replaces its hardcoded connected/disconnected with the waiting reason; the attributes test literal.
- [x] Sandboxed: when udev announces a configured node that this process can't see (`PrivateDevices=true`), the watcher warns once per node, naming the sandbox. Log-only for now: a typed `connect-failed` would need a new wire failure code, and the GUI already says the TV waits for its adapter.

### 5. `docs`

- [ ] [docs/architecture.md](../architecture.md): the parking rule, the watchers list, CEC no longer "never defers".
- [ ] [docs/watchers/network.md](../watchers/network.md) ("non-IP transports dial as always") and a `docs/watchers/devices.md` for the device watcher.
- [ ] [docs/cec/protocol.md](../cec/protocol.md): reconnect behavior when the adapter goes and returns; renumbering.
- [ ] Packaging: the comment in `packaging/systemd/tisvlay-daemon.service` (the sandbox hides the node at boot too, not just a late one), and the `DeviceAllow=char-cec` hazard (resolved from `/proc/devices` at unit start: with `cec.ko` not yet loaded, a hot-plugged adapter returns `EPERM` until the daemon restarts).
- [ ] Retire this plan and the Pulse-Eight plan per [docs/plans/README.md](README.md).

## Verification

- [ ] Unit and world tests as above; clock-driven backoff tests.
- [ ] Hardware, the detach experiment: CEC reconnects within about a second of `pulse8-cec-inputattach` starting again, with no manual `reconnect`, and the GUI shows the waiting state in between.
- [ ] Hardware, the boot race: restart the daemon with inputattach stopped, wait 30 s, start inputattach; CEC connects at once, and no failure is logged at the daemon's start.
- [ ] Hardware, the wake: with the adapter detached, trigger a power-on; after 60 s it publishes `Failed`, and the TV doesn't power on when the adapter returns later. Again with a power-off between the two: the held wake is cancelled at once.

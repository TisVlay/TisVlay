# Local-device presence: reconnect CEC when its adapter returns

A CEC TV's connection depends on a local device node (`/dev/cecN`), not a network route. Today nothing tells the daemon when that node comes and goes. A failed CEC dial spends its budget (`CONNECT_MAX_ATTEMPTS` × `CONNECT_BACKOFF`, about 9 s) and gives up, and a dropped CEC connection is never redialed on its own: `PendingConnect` is "never inserted by a disconnect". So a re-attached adapter, or one that registers more than about 9 s after the daemon starts, leaves CEC down until a manual `reconnect` or the next wake (found in [docs/bugs/cec-pulse8-autonomous-label.md](../bugs/cec-pulse8-autonomous-label.md), § Related findings).

This plan gives local-device TVs the same event-driven model network TVs already have for their route: a presence fact fed by the daemon, a dial gate that waits on it, and a redial when it returns. It also adds the CEC claim and connect logs that were missing during that investigation. It supersedes steps 4 and 5 of [cec-pulse8-autonomous.md](cec-pulse8-autonomous.md).

## Decisions

- **Presence model, mirroring routes.** Network TVs carry `MonitoredInterface` (from the netlink route monitor), and the dial gate defers on a known absence (`AwaitingRoute`) instead of failing. Local-device TVs get the equivalent: a `DevicePresence` fact from a daemon-side device watcher, an `AwaitingDevice` deferral, and a redial when the device returns. Event-driven first; timers only as a safety net.
- **Transport-agnostic in core.** A connector declares its local device through a new default method on `TvConnector` (`None` for network backends). The world and daemon only see "a TV whose connection needs this device node, in this subsystem". CEC is the first user; a future serial or USB backend reuses it.
- **Backoff as the safety net, not the mechanism.** A local-device TV whose device is present but whose dial fails (busy, permission denied) keeps retrying instead of giving up: the first three tries stay 3 s apart, then the gap doubles to a 60 s cap. Network TVs keep today's three-try budget, since their recovery already comes from network events.
- **Failure logs: warn once, info on recovery.** The first failed dial of a streak logs at warn with the error; repeats go to debug; the eventual success logs at info with the attempt count. A network TV that spends its budget logs at warn once, naming the recovery (`tisvlay reconnect`).
- **Claim logs live in the CEC backend.** `tisvlay-cec`'s link logs each claim at info (device, logical address, OSD name or none). A claim on a Backup address (12, 13) or Unregistered (15) logs at warn: the TV may not be able to name the device.
- **"Waiting for its device" is visible to clients.** A wire-visible change, so it ships as a `feat` commit, with the GUI, tray and CLI strings alongside.
- **User guide edits held** until the CEC follow-ups near their end.

## Refinements (confirmed)

Two points that improved on the first design. Both are confirmed.

1. **Listen to udev's events, not the kernel's, and re-check paths instead of waking every waiting TV.** The agreed design woke every waiting CEC TV on any CEC `add`, because the kernel's event arrives before udev has applied permissions or created aliases (`/dev/cec-pulse8`). udev broadcasts its own event on the same netlink family (`NETLINK_KOBJECT_UEVENT`, the udev multicast group rather than the kernel's) *after* its rules have run. On that event the watcher re-checks every monitored path in the subsystem and reports only real changes. That's precise (no wasted dials), works with aliases, and the first dial no longer races udev's permission rule. The message format is simple to parse without libudev. Only privileged senders can multicast on this family, so the events can't be forged by an unprivileged process.
2. **Where the waiting state goes on the wire.** `PowerPhase::AwaitingDevice` (next to `AwaitingNetwork`) only appears during a wake flow, on `progress/power`. A CEC TV that lost its adapter and isn't being woken would still just read "disconnected". To say why a TV is down at any time, the steady state belongs on `state/link`: a new field such as `waiting_for: null | "network" | "device"`. Decided: add the `Link` field as the primary signal, and the phase too so the wake flow stays parallel to `AwaitingNetwork`. Both go in the one `feat` commit.

## Implementation

### 1. Core: declare a local device

- [ ] `TvConnector::local_device(&self, &VendorConfig) -> Option<LocalDevice>`, default `None`. `LocalDevice` carries the node path and its kernel subsystem (`"cec"`); `#[non_exhaustive]` with a constructor, per the public API policy in [docs/build.md](../build.md). An additive default method on a published crate.
- [ ] `tisvlay-cec` returns its configured `device` with subsystem `cec`.

### 2. Daemon: the device watcher

- [ ] A `devices` module beside `netlink.rs`, taking `Monitor { tv, device }` and `Forget { tv }` requests from the reactor, the way route monitoring takes `RouteRequest`.
- [ ] On `Monitor`, check the path (`metadata`, following symlinks) and report the initial presence.
- [ ] One netlink socket on the udev group (`netlink-sys`, already in the tree through `rtnetlink`; it becomes a direct dependency). On an `add` or `remove` whose `SUBSYSTEM` matches a monitored device, re-check every monitored path in that subsystem and report changes.
- [ ] Safety net for hosts without udev events (containers, minimal systems): re-check absent paths every 60 s. A path check is a `stat`, never an `open`.
- [ ] The parser is pure and unit-tested on captured udev messages.

### 3. World: presence gates the dial

- [ ] `DevicePresence(bool)`, present only on local-device TVs, and `AwaitingDevice` as the publish gate, mirroring `MonitoredInterface` and `AwaitingRoute`.
- [ ] Dial gate: `DevicePresence(false)` defers with the budget reset, exactly like a known route absence.
- [ ] Presence returning redials a TV with no live connection and no dial pending, which covers the dropped-connection case that today never redials.
- [ ] Present-but-failing local-device dials keep retrying on the doubling backoff instead of dropping `PendingConnect`.
- [ ] The reactor sends `Monitor` on enrollment and dial spawn, and `Forget` on removal, alongside the route requests.

### 4. Logs

- [ ] CEC claim at info (device, address, name or none); warn on 12, 13 or 15.
- [ ] Dial failure streaks: warn on the first, debug after, info on the recovery with the attempt count.
- [ ] Network budget exhausted: warn once, naming `tisvlay reconnect`.
- [ ] Presence changes at info ("device removed, waiting for it" / "device returned, reconnecting").

### 5. Wire and clients (`feat`)

- [ ] The waiting state on the wire, per refinement 2.
- [ ] GUI: a "Waiting for its device…" string beside the existing "Waiting for the network…"; run `just i18n-extract`.
- [ ] Tray and CLI (`list`, `watch`) show it.

### 6. Docs

- [ ] [docs/architecture.md](../architecture.md): the presence model next to route monitoring.
- [ ] [docs/cec/protocol.md](../cec/protocol.md): reconnect behavior when the adapter goes and returns.
- [ ] Packaging notes: under `PrivateDevices=true` the sandbox never sees a late node, so the waiting state is permanent there; the log line says so.

### 7. Verification

- [ ] Unit: the udev parser; the world's presence gate, redial on return and doubling backoff (clock-driven, like the existing dial-gate tests).
- [ ] Hardware, the detach experiment: CEC reconnects within about a second of `pulse8-cec-inputattach` starting again, with no manual `reconnect`, and the GUI shows the waiting state in between.
- [ ] Hardware, the boot race: restart the daemon with inputattach stopped, wait 30 s, start inputattach; CEC connects at once.

## Open questions

- Whether a TV that is removed while a dial is in flight needs any extra handling beyond the gate's next pass.

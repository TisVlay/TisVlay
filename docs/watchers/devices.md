# Device Watcher Architecture

How the daemon follows a local device node (a USB CEC adapter's `/dev/cecN`) so a TV whose connection runs through it waits while it's gone and reconnects when it's back. It's the local-device counterpart of route monitoring for network TVs ([network.md](network.md)).

## Which TVs it watches

A backend names the node a connection needs through `TvConnector::local_device` (default `None`); CEC returns its configured `device`. The reactor sends the device watcher a `DeviceRequest::Monitor { tv, path }` at enrollment and on every dial spawn, beside the route request, and the `TvId` removal hook sends `Forget`. A configured path may be a udev alias: it's checked through the symlink.

## Watching udev

File: `crates/tisvlay-daemon/src/devices.rs`

One `NETLINK_KOBJECT_UEVENT` socket joins **udev's** multicast group (group 2), not the kernel's (group 1). udev broadcasts a device only after its rules have run, so by the time an `add` arrives the node's group, mode and aliases are in place, and the first dial doesn't race the permission rule. The socket comes through `rtnetlink::sys` (the re-exported `netlink-sys`), so it needs no dependency of its own; the daemon unit already allows `AF_NETLINK`, and the kernel lets unprivileged processes receive on this family (only privileged ones can send).

The message is udev's own format: a `libudev\0` prefix, a big-endian `0xfeedcafe` magic, then host-endian header fields locating a block of NUL-separated `KEY=VALUE` properties. The parser reads `ACTION` and `DEVNAME` and nothing else; anything else, including the kernel's `add@/devices/...` format, is ignored. An event is never trusted beyond triggering a re-check: on any `add` or `remove`, every monitored path is checked again and only changes are reported (`InEvent::DeviceAvailability { tv, available }`). A `Monitor` request is always answered, change or not, since the world may be asking about a TV it just enrolled.

**Available means usable, not merely present.** A node counts once it exists and, where udev runs (`/run/udev` exists), udev has initialized it: `/run/udev/data/c<major>:<minor>` exists, libudev's own test. devtmpfs creates the node before udev applies `GROUP`/`MODE`, root-only, so existence alone would race.

### Staying current

- The socket is bound before the first check, so an event between the two isn't lost.
- `ENOBUFS` (the kernel overran the receive buffer, typically a boot coldplug burst) triggers a full re-check instead of ending the watcher; the buffer is also raised to 1 MiB.
- Hosts whose udev events never reach the daemon (containers, minimal systems) are covered by a re-check of unavailable paths every 60 s. A check is a `stat`, never an `open`.
- A failed socket reopens on a capped backoff (1 s doubling to 60 s), serving requests by direct checks meanwhile, the same supervision the network monitor uses.

## What the world does with it

A `DeviceAvailable(bool)` fact sits on the TV (only once a report has arrived), and the dial gate treats `DeviceAvailable(false)` exactly like a known route absence: it defers the dial with the attempt budget reset (`AwaitingDependency`), and the fact's return re-runs the gate in the same batch. One predicate (`dependency_absent`) covers both facts in the gate, in the failure branch and in the link derivation.

- **Gone:** a live connection is parked (`Parked`), which drops its handle and closes its descriptors.
- **Back:** a parked or pending TV redials at once with a fresh budget, clearing any backoff. A TV whose dial is in flight is marked (`RetryPromptly`) so that a failure of that dial retries immediately.
- **A dropped connection redials by itself after one backoff.** A local-device connection survives standby, so a drop means the device went or blinked; this also covers a remove-and-add too quick for the watcher to see as a change.
- **The first dial waits for the first report** when the watcher is attached (`DeviceWatch`), instead of racing the `stat` into a spurious failure at every boot with the adapter not yet up. Only the daemon's `run` attaches the watcher; the public `serve` the wire tests use doesn't, so their CEC fakes dial as before.
- **A failure while the device is reported gone isn't an attempt**: it returns its attempt and publishes no `connect-failed`, like a routeless failure.
- **Device-level failures keep retrying.** When the node exists but can't be opened or set up (`Error::DeviceUnavailable`, or `PermissionDenied`), a local-device TV retries past its budget, the gap doubling from 3 s to a 60 s cap; a TV that doesn't answer through a working adapter keeps the three-try budget, as a network TV does.
- **A pairing verification dials through the gate**, so a re-pair with the adapter missing fails with its reason instead of waiting.
- **Held wakes** (a wake deferred on the missing device) expire after 60 s into a plain reconnect, and a newer power-off cancels them; this applies to route-held wakes too ([docs/architecture.md](../architecture.md) § The world).

Clients see it on `state/link`: `waiting_for` is `"device"` while the TV is disconnected and its device is known gone, and a held wake publishes `awaiting_device` on `progress/power`.

## Pitfalls

### Renumbering

The kernel frees a CEC node's minor number only when the last descriptor on it closes. If a descriptor on the old adapter is still open when the new one registers, `/dev/cec0` comes back as `/dev/cec1` and the configured path never becomes available. Parking on removal closes ours; the watcher logs at info when a node with the same name stem appears while a configured one is unavailable. A stable udev alias would remove the hazard: [docs/ideas/cec-stable-device-alias.md](../ideas/cec-stable-device-alias.md).

### A suspend can take the node down

A Pulse-Eight's `/dev/cecN` exists only while `inputattach` holds the adapter's serial line discipline. A resume from suspend resets the adapter's USB link and hangs up the tty under it, and `inputattach` exits cleanly; found 2026-10-24 when the node didn't come back after a suspend. The NixOS module's `pulse8-cec-inputattach@` unit therefore restarts whatever the exit (`Restart=always`) and is bound to the tty device, so a real unplug stops it and the udev rule starts it again on re-attach. The watcher sees the gap like any detach: the TV waits for its adapter and reconnects when the node is back.

### The sandbox can hide the node

Under systemd `PrivateDevices=true` (the packaged unit's default) the daemon gets a private `/dev` populated once at start, without `/dev/cecN`: not a late node, and not one present at boot either. The netlink events still arrive, so when udev adds a configured node this process can't see, the watcher warns once per node, naming the sandbox. The fix is the unit change the packaged file describes (`DeviceAllow=char-cec rw` instead).

`DeviceAllow=char-cec` resolves the `cec` character-device major from `/proc/devices` when the unit starts. If the `cec` module isn't loaded yet then, a later adapter is visible but its opens fail with `EPERM` until the daemon restarts; load `cec` before the daemon (the NixOS module's `cec.pulse8.enable` loads `pulse8_cec`, which pulls it in at boot).

## Tested scenarios

On an LG C3 with a Pulse-Eight adapter (2026-10-24):

- Stopping and starting `pulse8-cec-inputattach@ttyACM0` with the daemon running: `list` reads `waiting for its adapter` while detached and `connected` within about a second of re-attach, with no `reconnect`.
- Restarting the daemon with the adapter detached, then attaching it 30 s later: no dial failure is logged at start, and CEC connects as soon as the adapter is back.
- A power-on with the adapter detached and the TV in standby: held (`awaiting_device`), expired after 60 s (`failed`), and the TV stayed in standby when the adapter came back. With a power-off sent while it was held, the wake was cancelled at once and the TV again stayed in standby.
- A suspend and resume with CEC connected, after the `inputattach` restart fix: at resume `inputattach` exited cleanly, systemd restarted it 2 s later, `/dev/cec0` came back and CEC reconnected on its own, with the input still labeled "TisVlay". Before the fix the same resume left the node gone until the unit was started by hand.

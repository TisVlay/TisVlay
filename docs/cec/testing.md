# CEC backend — testing

`tisvlay-cec` is fully testable with **no `/dev/cecN` present** — every test in the crate runs on a host with no CEC hardware, which is the point: real adapters are scarce (see [protocol.md](protocol.md) § Hardware reality).

## The seam: `CecLink`

`linux-cec` ships no fake server, so the testability boundary is one layer lower than LG's `FakeSsapServer`. LG fakes the *peer* and drives the real transport; here we fake the *library wrapper*:

- `CecLink` (in `link.rs`) is a small `pub(crate)` trait over exactly the operations the connector and connection need — `query_power`, `power_on`, `power_off`, `set_stream_path`. Every method is `&self` (the underlying `AsyncDevice` relays to its worker thread), so a connection drives it through a shared reference.
- `CecMonitor` is the receive half (push state): `recv` awaits one inbound message. `LinuxCecMonitor` is a second, follower-mode `AsyncDevice` on the same node; `FakeCecMonitor` (test-only) replays a scripted message queue then idles.
- `CecLinkOpener` is the factory seam: `LinuxCecLinkOpener` opens a real `/dev/cecN` (tx handle via `open`, follower handle via `open_monitor`); `FakeCecLinkOpener` (test-only) yields a `FakeCecLink` seeded with a fixed power status, and — via `with_monitor_script` — a `FakeCecMonitor` for `subscribe_state` tests.
- `FakeCecLink` (test-only) holds a mutable `PowerStatus` that `power_on`/`power_off` flip, plus an `ignore_wakes` counter that drops the first N wakes so the re-send loop can be exercised.

The honest tradeoff: faking at the library wrapper means the production `linux-cec` ioctls are **not** exercised by these unit tests. The trait is deliberately thin so there's little untested glue between it and `AsyncDevice`; the daemon-level state machine (always-reachable transport, property-cache forwarding) is covered against the **mock backend**, not this seam.

## What's covered

Inline `#[cfg(test)]` modules, by file:

- **`connector.rs`** — `transport() == Cec` and non-network; `connect` succeeds when the TV is `On` and **fails when it's in standby** (the load-bearing `power_on`-routing contract); `wake_and_connect` powers on then connects, and **re-sends until a flaky TV wakes** (a fake that drops the first two sends); the verify-loop sleeps run in virtual time via `#[tokio::test(start_paused = true)]`; `supports_wake`; `start_pairing` drains to `Complete` with empty credentials; config parse rejection; the `device` + `physical_address` schema fields.
- **`connection.rs`** — `PowerStatus` → `PowerState` mapping; `get_state` reports live power + the input list + `None` volume/backlight/current-input; `subscribe_state` **pushes a snapshot from a scripted bus message** and **closes its channel when the connection drops** (the disconnect-emit contract); `power_off` sends standby and drops the connected flag; `close` is non-blocking; capabilities are `POWER | INPUT`; `parse_hdmi_input` port→physical-address mapping; `switch_input` accept/reject.
- **`discovery.rs`** — `is_cec_device` matching; `discover` lists `cecN` nodes (sorted, fixture dir via `tempfile`), skips non-CEC entries, and returns empty for a missing/empty dir.

## Future: real-ioctl coverage with `vivid`

The kernel's `vivid` driver exposes **virtual CEC adapters** (real `/dev/cecN` nodes with no hardware), which would let an *opt-in, feature-gated* integration test drive the production `LinuxCecLink` through real ioctls. It's deferred, not wired: `vivid` isn't in the default NixOS kernel build, so such a test would never run in CI without a kernel-config change. Until then, `FakeCecLink` plus the mock-backed daemon tests are the coverage.

## Field testing

Once a CEC adapter that exposes `/dev/cecN` is available, exercise the real path with `cec-ctl` (v4l-utils) alongside the daemon, and grow `firmware-quirks.md` from what surfaces.

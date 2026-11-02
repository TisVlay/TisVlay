# CEC backend — testing

`tisvlay-cec` is fully testable with **no `/dev/cecN` present** — every test in the crate runs on a host with no CEC hardware, which is the point: real adapters are scarce (see [protocol.md](protocol.md) § Hardware reality).

## The seam: `CecLink`

`linux-cec` ships no fake server, so the testability boundary is one layer lower than LG's `FakeSsapServer`. LG fakes the *peer* and drives the real transport; here we fake the *library wrapper*:

- `CecLink` (in `link.rs`) is a small `pub(crate)` trait over exactly the operations the connector and connection need — `query_power`, `power_on`, `power_off`, `set_stream_path`. Every method is `&self` (the underlying `AsyncDevice` relays to its worker thread), so a connection drives it through a shared reference.
- `CecMonitor` is the receive half (push state): `recv` awaits one inbound message. `LinuxCecMonitor` is a second, follower-mode `AsyncDevice` on the same node; `FakeCecMonitor` (test-only) replays a scripted message queue then idles.
- `CecLinkOpener` is the factory seam: `LinuxCecLinkOpener` opens a real `/dev/cecN` (tx handle via `open`, follower handle via `open_monitor`); `FakeCecLinkOpener` (test-only) yields a `FakeCecLink` seeded with a fixed power status, and — via `with_monitor_script` — a `FakeCecMonitor` for `subscribe_state` tests (`with_monitor_disconnect` models an adapter unplug; `with_identity` scripts an OSD name + vendor for the identity-probe path).
- `FakeCecLink` (test-only) holds a mutable `PowerStatus` that `power_on`/`power_off` flip, an `ignore_wakes` counter that drops the first N wakes so the re-send loop can be exercised, and a scripted OSD name + vendor id backing the `query_osd_name`/`query_vendor_id` identity path.

The honest tradeoff: faking at the library wrapper means the production `linux-cec` ioctls are **not** exercised by these unit tests. The trait is deliberately thin so there's little untested glue between it and `AsyncDevice`; the daemon-level state machine (always-reachable transport, property-cache forwarding) is covered against the **mock backend**, not this seam.

## What's covered

Inline `#[cfg(test)]` modules, by file:

- **`connector.rs`** — `transport() == Cec` and non-network; `connect` succeeds whether the TV is `On` **or in standby** (the reachable-in-standby model — a standby TV is still on the bus, so the daemon wakes it by power state rather than refusing the connection); `wake_and_connect` powers on then connects, and **re-sends until a flaky TV wakes** (a fake that drops the first two sends); the verify-loop sleeps run in virtual time via `#[tokio::test(start_paused = true)]`; `supports_wake`; `emulate_screen_via_power` defaults on and honors opt-out; `display_config` surfaces a schema-defaulted field (the view/edit parity fix); `start_pairing` drains to `Complete` with empty credentials; config parse rejection; the `device` / `physical_address` / `emulate_screen_via_power` schema fields.
- **`connection.rs`** — `PowerStatus` → `PowerState` mapping; `get_state` reports live power + the input list + `None` volume/backlight/current-input; `subscribe_state` **pushes a snapshot from a scripted bus message**, **ignores non-TV initiators**, and **flips `is_connected()` on a monitor-side disconnect** (`monitor_disconnect_flips_is_connected`); `power_off` sends standby but **stays connected** (the reachable-in-standby decouple); `power_on` **wakes in place on the existing link**; `hardware_id`/`brand` come from the connect-time identity query (and are absent without one); `close` is non-blocking; capabilities are `POWER | INPUT`; `parse_hdmi_input` port→physical-address mapping; `switch_input` accept/reject.
- **`discovery.rs`** — `is_cec_device` matching; `discover` lists `cecN` nodes (sorted numerically, fixture dir via `tempfile`), skips non-CEC entries, returns empty for a missing/empty dir, and **probes each node's identity into the name + `HardwareId` + brand** (`discover_probes_identity_into_name_and_hardware_id`).

The CEC-motivated *daemon-level* contracts — the ones the seam can't reach — are pinned two ways. Against the **mock backend**, which mirrors CEC's `reachable_in_standby` / `emulate_screen_via_power` flags, in `crates/tisvlay-world/tests/it/dispatch.rs`: `wake_needy_screen_on_wakes_in_place_when_standby_reachable` and `emulated_screen_keeps_standby_link_up`. And over the wire with the real `CecConnector` on `FakeCecLink`, in `crates/tisvlay-daemon/tests/it/cec.rs`: `cec_power_cycle_keeps_the_link_up` and `cec_pair_fails_typed_when_device_is_inaccessible`. Identity following a dongle swap is `a_swapped_device_republishes_its_identity` in `crates/tisvlay-world/tests/it/device.rs`.

## Future: real-ioctl coverage with `vivid`

The kernel's `vivid` driver exposes **virtual CEC adapters** (real `/dev/cecN` nodes with no hardware), which would let an *opt-in, feature-gated* integration test drive the production `LinuxCecLink` through real ioctls. It's deferred, not wired: `vivid` isn't in the default NixOS kernel build, so such a test would never run in CI without a kernel-config change. Until then, `FakeCecLink` plus the mock-backed daemon tests are the coverage.

## Field testing

Exercise the real path with `cec-ctl` (v4l-utils) alongside the daemon — validated on the LG C3 via a Pulse-Eight USB-CEC adapter — and grow `firmware-quirks.md` from what surfaces.

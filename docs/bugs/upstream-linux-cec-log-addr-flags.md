# Upstream: linux-cec can't set the log-address flags

**Status:** draft merge request, not filed (2026-10-25). The change is on our fork's `rc-passthru` branch ([TisVlay/linux-cec](https://github.com/TisVlay/linux-cec/tree/rc-passthru), `334a097e`, based on the `linux-cec-0.2.1` tag), which the workspace's `[patch.crates-io]` uses. File it at [holo/linux-cec merge requests](https://gitlab.steamos.cloud/holo/linux-cec/-/merge_requests) after the TV-controls-the-PC soak, together with the OSD-name issue ([upstream-linux-cec-reclaim-drops-osd-name.md](upstream-linux-cec-reclaim-drops-osd-name.md)). Once a release carries the setter, drop the `[patch]` (docs/build.md, Review triggers).

TisVlay uses it for the system profile's `remote_keys`: RC passthrough on the claim, so the kernel forwards the TV remote's keys to the adapter's `rc-cec` input.

---

## Title

Add a getter and setter for the logical-address flags

## Description

`cec_log_addrs` carries a `flags` field (`CEC_LOG_ADDRS_FL_ALLOW_UNREG_FALLBACK`, `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`, `CEC_LOG_ADDRS_FL_CDC_ONLY`), and linux-cec-sys already defines it as the `CEC_LOG_ADDRS_FL` bitflags type, but `Device` has no way to set it. In particular a program can't claim an address with RC passthrough, which the kernel needs before it hands `<User Control Pressed>` to the adapter's RC device (`cec_receive_notify` in `drivers/media/cec/core/cec-adap.c`), so the TV's remote can't drive the desktop through the `rc-cec` keymap. `cec-ctl --playback` sets the flag; a linux-cec claim can't.

This follows the crate's existing patterns:
- `LogicalAddressFlags` re-exports linux-cec-sys's `CEC_LOG_ADDRS_FL` from `device`, as `Capabilities` re-exports `CEC_CAP`.
- `Device::set_logical_address_flags` stores the flags in the claim configuration for the next `set_logical_addresses`, like `set_vendor_id`, and `get_logical_address_flags` reads the configured ones back, like `get_vendor_id`.
- `AsyncDevice` relays both.

One caveat, the same as for the OSD name and vendor ID: when the adapter already holds a claim, `set_logical_addresses` clears it first, and the kernel's copy-back over `internal_log_addrs` resets the flags (issue: <link once filed>). Callers set the flags after their own `clear_logical_addresses`, as they do the name.

Tested on Linux 7.3 with a Pulse-Eight USB adapter and an LG C3: <the campaign-end hardware check's result: with `ALLOW_RC_PASSTHRU` set before the claim, `cec-ctl -d /dev/cec0` shows `Logical Addresses : 1 (Allow RC Passthrough)` and the remote's keys arrive on the `rc-cec` input>.

**Before filing:** fill in the test line from the hardware check (so far only `cec-ctl`'s own passthrough claim has been run, in Check C of docs/ideas/tv-controls-pc.md), and rebase the branch onto upstream `main` if it has moved.

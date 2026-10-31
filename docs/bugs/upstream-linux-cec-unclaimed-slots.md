# Upstream: linux-cec fails to read a claim in progress

**Status:** draft merge request, not filed (2026-10-31). The fix is on our fork's `rc-passthru` branch ([TisVlay/linux-cec](https://github.com/TisVlay/linux-cec/tree/rc-passthru), `03b1b0ef`, on top of the log-address flags setter), which the workspace's `[patch.crates-io]` uses. It's independent of the setter, so it goes as its own merge request at [holo/linux-cec merge requests](https://gitlab.steamos.cloud/holo/linux-cec/-/merge_requests), cherry-picked onto upstream `main`. File it with the setter's ([upstream-linux-cec-log-addr-flags.md](upstream-linux-cec-log-addr-flags.md)). Once a release carries both, drop the `[patch]` (docs/build.md, Review triggers).

TisVlay hit it when the daemon's CEC dial opened a Pulse-Eight while the `pulse8-cec` driver was restoring the claim stored in the adapter's EEPROM at attach (`persistent_config=1`): the dial failed with `Invalid value 255 for type LogicalAddress`, and its retry 3 s later connected ([the plan's step 8b](../plans/tv-controls-pc.md#8b-the-adapters-eeprom-decides-the-wake-found-on-hardware)).

---

## Title

Skip unclaimed slots in get_logical_addresses

## Description

While a claim is in progress, or for a slot that failed to claim, the kernel reports `CEC_LOG_ADDR_INVALID` (`0xff`) in `cec_log_addrs.log_addr` (the `CEC_ADAP_G_LOG_ADDRS` documentation: "If no logical address could be claimed, then it is set to `CEC_LOG_ADDR_INVALID`"). `LogicalAddress` has no variant for it, so `Device::get_logical_addresses` failed the whole read on the conversion. A program that opens an adapter while another claim is being configured, by the driver itself (`pulse8-cec` restoring the claim stored in the adapter's EEPROM when `persistent_config` is set) or by another process, got an error instead of the addresses claimed so far.

The fix skips those slots, as `Device`'s `TryFrom<File>` already tolerates an unclaimed first slot (`try_from_primitive(...).unwrap_or_default()`). Any other out-of-range value still fails. The conversion moves into a private `claimed_logical_addresses` helper so it can be tested without a device; `AsyncDevice` relays the method unchanged.

Tested with the helper's unit tests (a claimed slot, an unclaimed one alone and after a claimed one, and an out-of-range value), and on Linux 7.3 with a Pulse-Eight USB adapter: <the dial racing the driver's EEPROM restore connects at once, once a host runs the fixed build with `persistent_config=1`>.

**Before filing:** drop the fork's change-notice line from the file header, cherry-pick onto upstream `main`, and fill in the hardware line.

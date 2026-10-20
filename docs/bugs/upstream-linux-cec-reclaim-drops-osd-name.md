# Upstream: linux-cec re-claim drops the OSD name

**Status:** draft, not filed (2026-10-20). File at [holo/linux-cec issues](https://gitlab.steamos.cloud/holo/linux-cec/-/issues) once our workaround (`8f764d5`, `fix(cec): keep the advertised OSD name when re-claiming the adapter`) has soaked on the LG C3. When it's filed, link the issue here and from the `ClaimPlan` docs in `crates/tisvlay-cec/src/link.rs`. If upstream fixes it, our explicit clear can stay: it's harmless, and it's what upstream's own `cecd` does.

Our side of the story (the symptom, and why the #114 field test passed) is in [docs/cec/protocol.md](../cec/protocol.md) § "Advertising our own name (the reverse direction)".

---

## Title

`set_logical_addresses` drops the OSD name and vendor ID when the adapter already has a claim

## Body

**Version:** linux-cec 0.2.1 (`main` at `2b7a801a` has the same code). Seen on Linux 7.2.9 with a Pulse-Eight USB adapter (`pulse8-cec`).

`Device::set_osd_name`'s docs say to call it before `set_logical_addresses`. That works on an adapter with no logical addresses. But the kernel keeps a claim after the handle that made it closes, so an adapter often already has one, for example from an earlier run of the same program. In that case the OSD name is lost, and so is a vendor ID set with `set_vendor_id`.

### Cause

When `internal_log_addrs.num_log_addrs > 0`, `set_logical_addresses` calls `clear_logical_addresses()` before claiming. That passes `self.internal_log_addrs` to `CEC_ADAP_S_LOG_ADDRS` with `num_log_addrs = 0`. The ioctl is `_IOWR`. On a clear the kernel resets `osd_name[0]`, `vendor_id` and `cec_version` in its own config (`__cec_s_log_addrs` in `drivers/media/cec/core/cec-adap.c`), then copies that config back over the caller's struct (`cec_adap_s_log_addrs` in `cec-api.c`). That overwrites the name and vendor ID set earlier in `internal_log_addrs`, and the claim goes out without them.

With an empty name, the kernel answers `<Give OSD Name>` with Feature Abort (`cec_receive_notify`, `CEC_MSG_GIVE_OSD_NAME`), so the TV shows its own label for the input, e.g. "HDMI 1".

The requested logical-address types are probably affected too, though I haven't reproduced that. `set_logical_addresses` writes `log_addr_type` and `primary_device_type` before the clear. The kernel's clear path doesn't reset those fields, so the copy-back restores the *previous* claim's types. Re-claiming a held adapter as a different type would then claim the old type again.

### Steps to reproduce

1. Give the adapter a claim, e.g. `cec-ctl -d /dev/cec0 --playback`.
2. Run:

   ```rust
   use linux_cec::device::Device;
   use linux_cec::{InitiatorMode, LogicalAddressType};

   let mut dev = Device::open("/dev/cec0")?;
   dev.set_initiator_mode(InitiatorMode::Enabled)?;
   dev.set_osd_name("Example")?;
   dev.set_logical_address(LogicalAddressType::Playback)?;
   ```

3. Run `cec-ctl -d /dev/cec0`.

**Expected:** `OSD Name : 'Example'`.

**Actual:** `OSD Name : ''`. The claim succeeds, as Playback.

We hit this through `AsyncDevice`, with the same calls in the same order. Running `cec-ctl -d /dev/cec0 --clear` before the program, so it starts with no claim, gives the expected result.

### Workaround

Call `clear_logical_addresses()` yourself before `set_osd_name` and `set_vendor_id`. `set_logical_addresses` then finds no claim and doesn't clear. `cecd`'s `system.rs` already does this.

### Suggested fix

Clear with a scratch copy, so the copy-back can't overwrite the caller's configuration:

```rust
pub fn clear_logical_addresses(&mut self) -> Result<()> {
    let mut cleared = self.internal_log_addrs.clone();
    cleared.num_log_addrs = 0;
    unsafe {
        adapter_set_logical_addresses(self.file.as_raw_fd(), &mut cleared)?;
    }
    self.internal_log_addrs.num_log_addrs = 0;
    self.tx_logical_address = LogicalAddress::Unregistered;
    Ok(())
}
```

This keeps the name, the vendor ID and the requested types across the clear inside `set_logical_addresses`, and across an explicit clear followed by a claim. Alternatively, `set_logical_addresses` could save and restore those fields around its clear, or the `set_osd_name` docs could say to clear first.

### Related

On a device that already has a claim, `set_osd_name` sends `<Set OSD Name>` to the TV but doesn't update the kernel's config. The kernel keeps answering `<Give OSD Name>` with the old name until the next claim. That looks like it affects `cecd`'s D-Bus `SetOsdName`. Happy to file that separately if it's unintended.

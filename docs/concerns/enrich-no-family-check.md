## MAC enrichment doesn't check the address's IP family

**Priority**: low
**Found by**: the 2026-11 docs sweep's review (`docs/discovery.md` claimed a check that doesn't exist)

### Context

Manual-entry enrichment (`manager/rpc/enrich`) primes the vendor's ARP probe and reads the neighbor table for the address the user typed. Vendors declare the IP families their transport supports with the `IPV4` / `IPV6` capability bits, but neither side reads them here:

- The wizard asks whenever the vendor's primary field is an IP (`primary_is_ip` in `tisvlay-gui-app`'s `lib.rs`). It carries `supports-ipv4` / `supports-ipv6` on each `VendorOption`, and uses them only to switch the address placeholder for a v6-only vendor.
- The daemon's `handle_enrich` (`tisvlay-daemon`'s `bridge.rs`) has no family gate.

So a v6 address typed for a v4-only vendor still gets a probe and a lookup. Both come back empty, so the user sees what they'd see anyway ("MAC not detected"), after a wasted probe.

### Risk

Small: a pointless probe and a blank field, no wrong value. It matters more once a vendor supports only one family and the wizard should say why the address won't work.

### Mitigation options

- **Wizard-side:** skip enrichment, and flag the field, when the address's family isn't one the vendor supports. The bits are already on the option.
- **Daemon-side:** refuse or skip in `handle_enrich`, so the CLI and any other client get the same rule.

### Trigger

A vendor that supports only one IP family, or a user report of a confusing blank MAC.

# Pair-time MAC capture

After the manual-entry MAC autofill work ([docs/discovery.md](../discovery.md)), users who skip the MAC field at pair-time still end up with an empty `vendor_data["mac"]` in their saved config — Wake-on-LAN silently won't work the next time the TV goes to full-off state. The pair flow itself is a guaranteed-warm-ARP moment: it opens a real TCP connection (SSAP over wss for LG), which forces kernel ARP/NDP resolution as a side effect. Snooping the resulting cache entry at successful-pair time and writing it back to the saved config is a clean belt-and-suspenders for any path where the wizard didn't get a MAC up front.

## What this would add

A daemon-side hook in the pair flow:

- On `Manager1.AddTv` / `Tv1.RePair` reaching the terminal `complete` phase, look up the just-paired TV's IP in the kernel neighbour table (existing `netlink::arp_lookup`).
- If a MAC came back AND the saved config's `vendor_data["mac"]` is empty, update the config in-place (atomic write via the existing `apply_reload_diff` machinery or a narrower `update_tv_vendor_data` path).
- Emit a `PropertiesChanged` so the GUI's TVs-pane refreshes the MAC display without a reload.

Net effect: users who type only an IP at pair-time get WoL automatically the moment pair completes. They never have to know the MAC exists.

## Why it's worth doing even though manual-entry autofill exists

- **Manual-entry autofill misses when the ARP cache is cold and the active probe arrives too late.** Users on cold-cache LANs (firewall-blocked SSDP responses, fresh boot) might still see an empty MAC field. The pair flow's TCP handshake is a much more reliable cache-warming event than the manual-entry probe's single UDP byte.
- **Catches users who edited the MAC field to empty.** Even if autofill worked, a user might delete the value before submitting (thinking they don't need it). Pair-time capture is the safety net.
- **No additional UI surface.** This is pure daemon-side enrichment; the user sees the MAC populated in the TVs-pane after pair without any extra step.
- **Future-proofs against the GUI flow being skipped.** Users pairing via `tisvlay pair <name> <vendor> <ip>` from the CLI don't get the wizard's autofill path at all today.

## Mechanics

Touchpoints in the daemon's pair flow:

- **`run_pair_task` / `run_re_pair_task`** in [crates/tisvlayd/src/tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs) — these emit the terminal `PairingProgress(complete)` phase. Hook the MAC capture step between handshake-success and that emit, so the GUI's `complete`-observation can rely on the MAC already being in place.
- **`update_tv_vendor_data`** (or equivalent) — new narrow helper that updates one TV's `vendor_data["mac"]` field, persists via atomic write, emits `Tv1.PropertiesChanged` for `VendorConfig`.
- **Idempotence**: skip the capture if `vendor_data["mac"]` is already non-empty. Don't overwrite a user-typed MAC even if ARP returns a different value (the user-typed one might be intentional — VLAN aliasing, custom NIC swap).

## Edge cases worth handling

- **TV behind NAT / VPN.** ARP might return the gateway's MAC instead of the TV's. WoL with the gateway MAC is useless. Mitigation: skip capture when the resolved MAC doesn't match the local subnet's expected vendor OUI for the TV's vendor. Or: only capture when `resolve_interface_for_ip(tv_ip)` returns an on-link interface (not a default-route hop). The second is simpler and catches the gateway case directly.
- **Pair-time vs cache-stale.** A TV that gets a new NIC (or factory reset on certain firmwares) keeps the old MAC in the saved config. Pair-time re-capture *should* update on each re-pair so a user who re-pairs after a TV repair gets the fresh MAC. Re-pair already implies the user is making a deliberate update, so overwriting is fine in that path. AddTv is the always-write path; RePair could be the overwrite-anyway path.
- **Multiple TVs sharing one IP** (none in practice) — not a concern for v1.

## Trade-offs vs the wizard-time enrichment

- **Wizard-time enrichment** (shipped): user sees the MAC populated immediately, can verify before submitting. Subject to the cold-cache miss.
- **Pair-time capture** (this proposal): always-works for users who pair via any path. User doesn't see the MAC until *after* pair completes — slightly less transparent.

Both layers ship cleanly side-by-side. Wizard-time gives the trust signal; pair-time is the safety net.

## Triggers to revisit

- Users report WoL not working after manual-entry pair despite the wizard showing "Auto-detected" (would indicate the wizard captured something stale and pair-time didn't validate).
- Field reports of empty MAC for users who pair via CLI (bypassing the wizard).
- Implementation of the IPv6 enrichment follow-up (see [docs/ideas/ipv6-discovery-and-enrichment.md](ipv6-discovery-and-enrichment.md)) — pair-time NDP capture would slot in symmetrically.

## Out of scope (don't conflate)

- **MAC rotation detection over time.** Long-running TVs that swap MACs mid-pair-lifetime aren't covered by either wizard-time or pair-time capture. A periodic re-resolve would be its own thing.
- **WoL packet validation.** Verifying the captured MAC actually receives WoL is separate; that's covered by the daemon's existing `wake_and_connect` retry loop surfacing failures.

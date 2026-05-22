# Manual-entry MAC autofill

When a user picks a TV from the wizard's SSDP-discovered list, the MAC address is autofilled into the pairing form via `vendor_data["mac"]` — the daemon's `enrich_with_arp` helper reads the kernel ARP cache, which has already been primed by the SSDP response. The manual-entry path (user types an IP after picking a vendor) skips this enrichment entirely and leaves the MAC field empty, even though the same kernel-cache lookup would usually work. Add a new `Manager1.EnrichManualEntry(vendor, address)` RPC that mirrors the SSDP path's enrichment, with an active-probe fallback for users whose ARP cache is cold (the common case for users who reach for manual entry because their firewall blocks SSDP responses in the first place).

## Motivation

Two compounding problems in today's manual-entry path:

1. **Silent empty MAC.** The wizard synthesizes a `WizardEntry { vendor_data: HashMap::new(), … }` for manual entries, so the schema-form autofill mechanism — which reads from `vendor_data` — finds nothing and leaves the field blank. The field has no helper text explaining why, and the user has no idea Wake-on-LAN won't work until the next time the TV goes to off-state.
2. **The exact users who reach for manual entry have cold ARP caches.** Manual entry is the fallback when SSDP discovery doesn't work — most commonly because the user hasn't opened the firewall for SSDP responses. But SSDP responses are also what primes the kernel's ARP entry for the TV's IP. So the "free" passive netlink lookup that works perfectly in the SSDP path is exactly the case where it's *least* likely to hit in the manual path. Passive lookup alone would still help (users who've previously pinged or browsed the TV), but it's not enough.

Net result: WoL-driven power-on (a core feature) silently doesn't work for the population of users least equipped to debug it.

## Scope

In:

1. New `Manager1.EnrichManualEntry(vendor: String, address: String) -> a{ss}` D-Bus RPC. Returns the same `vendor_data`-shaped map that `DiscoveryFound` already carries.
2. Daemon-side enrichment chain: passive netlink read → active UDP probe to prime the ARP cache → passive netlink re-read. Best-effort throughout; missing-MAC is a normal outcome, not an error.
3. Per-vendor probe-port hook on the connector trait so future backends (Sony / Samsung) can opt into the same ARP-priming probe without modifying the daemon.
4. Wizard wiring: `on_manual_add` calls the RPC before transitioning to step 2; the returned `vendor_data` populates the `WizardEntry`. Same `autofill_schema` engine then fills the form fields uniformly with the SSDP path.
5. Schema-form helper text on the MAC field for manual entries: a short non-error line explaining what was attempted and why the user might want to fill it in by hand if blank. Likely requires a small extension to `PairingField` (see Design) — confirm before extending vs. piggybacking on an existing field.

Out:

- **Pair-time MAC capture.** Snoop ARP at successful-pair time, write the MAC back to the saved config. The pair flow already opens a real TCP connection to the TV, so the kernel's ARP cache is guaranteed-warm at that moment — write-back means users who skipped the MAC field at form time still get WoL after first pair. Filed as a follow-up because it touches the pair flow (which has its own async-spawn + progress-signal contract) rather than the enrichment layer.
- **Active SSDP fallback** ("if SSDP didn't work, retry it more aggressively"). Doesn't help — same firewall blocks the response.
- **mDNS-based MAC discovery.** Adjacent to the existing mDNS-fallback discovery idea in [docs/firewall.md](../firewall.md); out of scope here.
- **Raw-socket arping.** Would give a definitive probe but needs `CAP_NET_RAW` on the daemon, which the NixOS module's hardened systemd unit explicitly forbids. UDP probe is the right tool.

## Design

### `Manager1.EnrichManualEntry`

```
EnrichManualEntry(vendor: String, address: String) -> a{ss}
  - Returns the same vendor_data map shape that DiscoveryFound carries.
  - "mac" key present when ARP succeeded (passive or post-probe).
  - Empty map when address isn't a parseable IP, vendor is unknown,
    or the probe-and-lookup chain didn't yield anything.
  - Never an error — wizards should treat empty as "nothing to autofill"
    rather than as a failure.
```

Symmetric in shape with the `DiscoveryFound` signal's `vendor_data` column so the wizard can treat both paths uniformly: build a `WizardEntry`, pass `vendor_data` to `autofill_schema`, done.

### Enrichment chain (daemon-side)

```rust
async fn enrich_manual_entry(vendor: &str, address: &str) -> HashMap<String, String> {
    let Ok(ip) = address.parse::<IpAddr>() else { return HashMap::new(); };

    // 1. Passive lookup (free, succeeds for users who've previously
    //    pinged/browsed the TV — primes the ARP cache as a side effect
    //    of any prior IP traffic).
    if let Some(mac) = netlink::arp_lookup(ip).await {
        return vendor_data_from_mac(mac);
    }

    // 2. Active probe (only if passive missed). Pick the vendor's
    //    expected SSAP / control port and send a single UDP byte to it.
    //    The kernel ARP-resolves the destination before sending; the
    //    payload itself is discarded by the TV (or by its firewall).
    //    No TLS handshake side-effect, no inbound port needed, no raw
    //    sockets.
    if let Some(port) = registry.probe_port_for(vendor)
        && let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await
    {
        let _ = socket.send_to(b"x", (ip, port)).await;
        tokio::time::sleep(ARP_RESOLVE_TIMEOUT).await; // ~100-300 ms
    }

    // 3. Passive re-lookup.
    if let Some(mac) = netlink::arp_lookup(ip).await {
        return vendor_data_from_mac(mac);
    }
    HashMap::new()
}
```

Tuning:
- `ARP_RESOLVE_TIMEOUT = 250 ms` initial guess; netlink resolution on a healthy LAN typically completes in tens of milliseconds, but the TV being asleep can stretch it.
- The active-probe step short-circuits if the registry can't supply a port for that vendor (default), so unknown vendors degrade to passive-only.

### Per-vendor probe port

Extend `TvConnector` (or sit alongside it) with:

```rust
/// Port to send a one-byte UDP datagram to in order to prime the
/// kernel ARP cache for this TV's IP when SSDP hasn't done it for us.
/// `None` means the daemon should skip the active probe for this
/// vendor (defaults to None — vendors opt in).
fn probe_port(&self) -> Option<u16> { None }
```

- LG override: `Some(3001)` (SSAP wss port — definitely open if the TV is reachable for pairing).
- Mock override: `None` (no real network, no probe needed).
- Lives on `TvConnector` rather than a new trait so the connector registry already has it; one fewer indirection.

### Wizard wiring

```rust
// crates/tisvlay-gui/src/lib.rs — on_manual_add handler.
wizard.on_manual_add(move || {
    let vendor = …;
    let ip = …;

    // Brief loading state — the enrichment is sub-perception (~300 ms
    // worst case) but the user should see *something*. Reuse the
    // discovery-streaming spinner repurposed as "Looking up …".
    wizard.set_discovery_streaming(true);

    let d = d.clone();
    let app_weak = app.as_weak();
    let we = we.clone();
    tokio::spawn(async move {
        let vendor_data = d.enrich_manual_entry(&vendor, &ip).await.unwrap_or_default();
        on_ui(&app_weak, move |app| {
            let wizard = app.global::<PairingWizard>();
            wizard.set_discovery_streaming(false);
            let entry = WizardEntry { vendor, address: ip, vendor_data, .. };
            // Push to entries, set selected_index, transition to step 2.
            // Existing step_next case-1 logic then triggers schema fetch + autofill.
        });
    });
});
```

Latency budget: passive ARP ~5 ms, optional UDP probe + sleep ~250 ms, second passive ARP ~5 ms. Worst-case ~300 ms before step 2 paints. Sub-perception for the populated case, briefly noticeable for the active-probe case. Acceptable; the user just typed an IP and clicked a button, so brief processing feels expected.

### Schema-form helper text

The MAC field in LG's pairing schema currently autofills from `vendor_data["mac"]` and shows nothing when empty. For the manual-entry case we want a one-line hint:

> Couldn't auto-detect — leave blank if you don't need Wake-on-LAN (the TV won't auto-power-on from a full-off state without it).

Two possible implementations:
- **Add `helper_text: Option<String>` to `PairingField`** (in `tisvlay-core/src/pairing_schema.rs`). Renders below the input. Generic, useful beyond MAC.
- **Piggyback on an existing field** (e.g. `description`). Likely worse because `description` is rendered above the input as a label-like string.

Lean toward the first. Confirm `PairingField` structure during implementation; if a sibling field already serves this need, skip the schema extension and use it.

The hint should *only* render when the wizard is in the manual-entry path *and* the autofill returned empty. Achievable via: (a) two schema variants and picking based on path; (b) a wizard-side property the schema-form renderer respects; (c) inferring from `vendor_data` being empty at render time. Option (c) is cleanest — the schema engine already has `vendor_data` in hand.

## Sequencing

One PR, one commit. The change is contained (~150-250 LOC across daemon RPC, wizard callsite, schema-form hint) and the layers don't compose meaningfully in isolation — a daemon RPC with no consumer is dead weight, a wizard callsite without the RPC won't compile, and the schema hint without the wizard wiring renders for SSDP entries too. Bundle.

If the `PairingField` extension lands up being non-trivial (e.g. requires touching every existing vendor schema), split commit 1 = daemon RPC + connector trait extension + wizard wiring (autofill works, no hint text), commit 2 = schema hint. Decide during implementation.

Pre-merge:

- [ ] `prek run -a` clean.
- [ ] Manual smoke: SSDP off (drop the firewall rule temporarily), pair via manual entry, confirm MAC field is populated.
- [ ] Manual smoke: TV powered off entirely (so ARP can't resolve no matter what), pair via manual entry, confirm field is blank with the helper text visible.

## Alternatives considered

- **Pair-time MAC capture as primary path.** Compelling: pair always opens a real TCP connection, so ARP is guaranteed-warm at pair-success time. Write the MAC into the saved config from the daemon side. Wizard MAC field stays blank during form-fill. Decided against as the primary path because: (a) the wizard's `autofill` mechanism already exists and the symmetric SSDP/manual story is cleaner, (b) the user benefits from seeing what got auto-detected (trust signal), (c) pair-time capture is still valuable as belt-and-suspenders — filed as follow-up.
- **Raw-socket arping** via libpcap / `CAP_NET_RAW`. Definitive ARP probe, but requires elevating the daemon's privileges — which the NixOS module's hardened systemd unit ([packaging/nix/module.nix:255-257](../../packaging/nix/module.nix#L255-L257)) explicitly forbids (`CapabilityBoundingSet = [ "" ]`). UDP probe achieves ~the same hit rate without any privilege escalation.
- **TCP connect to the SSAP port** instead of UDP send_to. Same ARP-priming effect, but the TCP SYN initiates a TLS handshake from the TV's perspective; the daemon then drops the connection, which the TV may log. UDP send_to is silent (TV's UDP stack discards the unexpected byte without surfacing it to the application layer). Negligible practical difference; UDP is the marginally cleaner choice.
- **Always do the active probe** (even after passive lookup succeeds). Wastes ~250 ms for users whose ARP cache is already warm. Not worth it — passive-then-conditional-probe is strictly better.
- **Cache enrichment results in the daemon.** Tempting (avoids re-probing on every manual-add retry), but introduces a stale-cache failure mode (MAC changes after TV's NIC swap) for a problem we don't have. Defer until field reports indicate it matters.

## Open questions

- `PairingField` helper-text extension: is the trait already extensible cleanly, or does adding a field require touching every existing vendor's schema definitions? If invasive, split into a separate commit; if a one-line additive struct change, fold in.
- `ARP_RESOLVE_TIMEOUT` initial value: 250 ms is a guess. Worth field-tuning against real LAN behavior. If the probe consistently fails inside 250 ms (kernel resolution races the timeout), bump to 500 ms.
- Should `EnrichManualEntry` also try to fill non-MAC fields? Examples: model from a TV-side HTTP probe, an EDID-derived hardware ID. Out of scope for v1 — MAC is the load-bearing field for WoL. Add fields additively if a future use case justifies.
- Privacy posture: should the daemon log every IP address it actively probes? Probably yes (debug-level), since unsolicited UDP packets to user-specified IPs are a soft surveillance signal worth being able to audit. Pick a log line shape during implementation.

## Migration on merge

Per [docs/plans/README.md](README.md):

- The `--bind-port` / config / RPC mechanics around discovery now have two distinct enrichment entry points (`enrich_with_arp` in the streaming/one-shot path, `enrich_manual_entry` in the wizard manual-add path). When both ship, the architectural note worth folding into [docs/design.md](../design.md)'s future Discovery subsection is: *MAC enrichment is a kernel-ARP-cache read with vendor-aware active priming, not a separate protocol — and that's a deliberate scope choice over mDNS / vendor-specific HTTP probes.*
- Per-vendor `probe_port` hook on `TvConnector` becomes part of the public connector contract — document under the trait's API surface in `docs/design.md`'s vendor-backend section.
- Pair-time MAC capture follow-up: file as `docs/ideas/pair-time-mac-capture.md` if not implemented in the same PR.
- Delete this plan file. `grep -rn 'docs/plans/manual-entry-mac-autofill'` to find and re-point any inbound refs.

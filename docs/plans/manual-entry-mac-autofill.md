# Manual-entry MAC autofill

When a user picks a TV from the wizard's SSDP-discovered list, the MAC address is autofilled into the pairing form via `vendor_data["mac"]` — the daemon's `enrich_with_arp` helper reads the kernel ARP cache, which has already been primed by the SSDP response. The manual-entry path (user types an IP after picking a vendor) skips this enrichment entirely and leaves the MAC field empty, even though the same kernel-cache lookup would usually work. Add a new `Manager1.EnrichManualEntry(vendor, address)` RPC that mirrors the SSDP path's enrichment, with an active-probe fallback for users whose ARP cache is cold (the common case for users who reach for manual entry because their firewall blocks SSDP responses in the first place).

## Motivation

Two compounding problems in today's manual-entry path:

1. **Silent empty MAC.** The wizard synthesizes a `WizardEntry { vendor_data: HashMap::new(), … }` for manual entries, so the schema-form autofill mechanism — which reads from `vendor_data` — finds nothing and leaves the field blank. The MAC field has a generic "Required for Wake-on-LAN" helper text (at [crates/tisvlay-lg/src/backend.rs:740-756](../../crates/tisvlay-lg/src/backend.rs#L740-L756)) but no context-specific cue that auto-detection was *attempted and failed*, so the user has no idea Wake-on-LAN won't work until the next time the TV goes to off-state.
2. **The exact users who reach for manual entry have cold ARP caches.** Manual entry is the fallback when SSDP discovery doesn't work — most commonly because the user hasn't opened the firewall for SSDP responses. But SSDP responses are also what primes the kernel's ARP entry for the TV's IP. So the "free" passive netlink lookup that works perfectly in the SSDP path is exactly the case where it's *least* likely to hit in the manual path. Passive lookup alone would still help (users who've previously pinged or browsed the TV), but it's not enough.

Net result: WoL-driven power-on (a core feature) silently doesn't work for the population of users least equipped to debug it.

## Scope

In:

1. New `Manager1.EnrichManualEntry(vendor: String, address: String) -> a{ss}` D-Bus RPC. Returns the same `vendor_data`-shaped map that `DiscoveryFound` already carries.
2. Daemon-side enrichment chain: passive netlink read → vendor-supplied active probe → poll-with-deadline re-read. Best-effort throughout; missing-MAC is a normal outcome, not an error.
3. Vendor-owned probe mechanism: new `async fn probe_arp_cache(&self, ip: IpAddr) -> Result<()>` on `TvConnector` (default no-op). LG override sends a single UDP byte to port 3001. Vendor owns *how* the cache gets primed; daemon orchestrates.
4. Daemon-side safety: per-caller rate limit on the RPC, RFC1918/link-local/ULA private-subnet guard (overridable), per-IP request coalescing so retry-mashing doesn't fan out, outbound-interface binding so multi-NIC hosts probe the right segment.
5. Wizard wiring: `on_manual_add` calls the RPC before transitioning to step 2; a distinct `manual-lookup-streaming` Slint property drives a focused loading state (not the discovery spinner); the Continue button is disabled while in flight; a cancel handle stashed alongside `WizardDiscoveryCancel` drops on `reset_wizard`; the wizard caps its own wait at 1 s so a hung RPC can't freeze the UI.
6. Schema-form trust signal: when a field is populated via `autofill_schema(…)`, mark the rendered row with a small "auto-detected" badge that clears the moment the user edits. Applies uniformly to SSDP and manual paths (the new enrichment is the right moment to fix this).
7. Wizard-side helper-text override: when enrichment returns no MAC, replace the field's default helper at render time with *"MAC not detected. Enter it manually to enable Wake-on-LAN (otherwise the TV won't power on from full-off)."* No schema change — see Alternatives.
8. Observability: a `tracing` span around each enrichment chain with structured fields (`vendor`, `ip`, `source: passive | active | none`, `elapsed_ms`); `info!` line when the rate limiter trips so abuse leaves a footprint.
9. Tests: unit tests for the enrichment chain (passive-hit / probe-then-hit / miss / invalid input) with the netlink + probe boundaries stubbed; D-Bus integration test against the existing `TestDaemon` private-bus harness for the RPC contract.

Out:

- **Pair-time MAC capture.** Snoop ARP at successful-pair time, write the MAC back to the saved config. The pair flow already opens a real TCP connection to the TV, so the kernel's ARP cache is guaranteed-warm at that moment — write-back means users who skipped the MAC field at form time still get WoL after first pair. Filed as a follow-up because it touches the pair flow (which has its own async-spawn + progress-signal contract) rather than the enrichment layer.
- **Active SSDP fallback** ("if SSDP didn't work, retry it more aggressively"). Doesn't help — same firewall blocks the response.
- **mDNS-based MAC discovery.** Adjacent to the existing mDNS-fallback discovery idea in [docs/firewall.md](../firewall.md); out of scope here.
- **Raw-socket arping.** Would give a definitive probe but needs `CAP_NET_RAW` on the daemon, which the NixOS module's hardened systemd unit explicitly forbids. UDP probe is the right tool.
- **Re-detect on step-2 IP edit.** The current LG schema doesn't expose the IP for editing inside step 2 (it's a step-1 input), so there's no live "user changed the IP after enrichment ran" case to recover from. Re-add if/when a vendor schema starts exposing IP-editing post-step-1.
- **`PairingField.helper_text` schema extension.** First-draft plan proposed this; the field already exists at [crates/tisvlay-core/src/pairing_schema.rs:152](../../crates/tisvlay-core/src/pairing_schema.rs#L152) as `pub helper: String` and is rendered today. The manual-path-specific hint is a wizard-side render-time override, not a schema change.

## Design

### `Manager1.EnrichManualEntry`

```
EnrichManualEntry(vendor: String, address: String) -> a{ss}
  - Returns the same vendor_data map shape that DiscoveryFound carries.
  - "mac" key present when ARP succeeded (passive or post-probe).
  - Additional structured signals via vendor_data:
    - "_source" = "passive" | "active" | "none" (debug aid for the
       wizard's tooltip; underscore prefix flags "metadata not field
       autofill" so vendors don't accidentally pick it up).
  - Empty map when address isn't a parseable IP, vendor is unknown,
    the address is outside the allowed subnet set, or the rate limit
    is exhausted.
  - Never an error — wizards should treat empty as "nothing to autofill"
    rather than as a failure. Invalid IP, unknown vendor, rate-limit-
    refused all collapse to "empty map" so the client has one shape to
    handle.
  - Empty-input fast path: ("", "") returns an empty map without any
    netlink syscall (defensive against misfired callbacks).
```

Symmetric in shape with the `DiscoveryFound` signal's `vendor_data` column so the wizard can treat both paths uniformly: build a `WizardEntry`, pass `vendor_data` to `autofill_schema`, done.

### Enrichment chain (daemon-side)

```rust
async fn enrich_manual_entry(
    registry: &BackendRegistry,
    vendor: &str,
    address: &str,
) -> HashMap<String, String> {
    // Fast no-op for misfired empty callbacks.
    if vendor.is_empty() || address.is_empty() { return HashMap::new(); }

    let Ok(ip) = address.parse::<IpAddr>() else { return HashMap::new(); };
    if !is_allowed_subnet(ip) { return HashMap::new(); }
    let Some(connector) = registry.connector_for(vendor) else { return HashMap::new(); };

    // Coalesce concurrent calls for the same IP. A user retry-mashing
    // the manual-add button must not fan out into N parallel probes.
    let _shared = inflight.coalesce(ip).await;

    // 1. Passive lookup (free, succeeds for users who've previously
    //    pinged/browsed the TV — primes the ARP cache as a side effect
    //    of any prior IP traffic).
    if let Some(mac) = netlink::arp_lookup(ip).await {
        return mac_result(mac, "passive");
    }

    // 2. Vendor-supplied active probe. Connector chooses the mechanism
    //    (UDP byte to SSAP port for LG; a future vendor could do TCP
    //    connect, mDNS query, vendor-specific HTTP probe, etc.).
    //    Default impl on TvConnector is a no-op, so unknown / probe-
    //    less vendors degrade gracefully to passive-only.
    if connector.probe_arp_cache(ip).await.is_err() {
        // Probe itself failing is non-fatal — the netlink re-read
        // might still hit (e.g. another process primed the cache).
    }

    // 3. Poll netlink with early-out. Beats a fixed sleep: cache
    //    typically resolves in tens of ms on a healthy LAN; only the
    //    "TV unreachable" case rides the full budget.
    let deadline = Instant::now() + ARP_RESOLVE_BUDGET;  // 500 ms
    let mut poll = tokio::time::interval(Duration::from_millis(30));
    while Instant::now() < deadline {
        poll.tick().await;
        if let Some(mac) = netlink::arp_lookup(ip).await {
            return mac_result(mac, "active");
        }
    }
    mac_result_none()
}
```

Tuning:
- `ARP_RESOLVE_BUDGET = 500 ms` (cap on the post-probe poll window). Worst-case RPC wall-time is passive (~5 ms) + probe send (~1 ms) + poll (~500 ms) + final read (~5 ms) ≈ **~510 ms**.
- Daemon-side cap is 600 ms outermost; wizard-side cap is 1000 ms safety net. Either firing means we surface a "MAC not detected" outcome cleanly.

### Vendor-owned probe mechanism

Extend `TvConnector` with a default-impl method:

```rust
/// Send a probe to `ip` that primes the kernel ARP cache as a side
/// effect. Best-effort: the daemon uses this only as a fallback when
/// passive netlink lookup misses, and treats `Err(_)` as non-fatal
/// (it still re-reads netlink afterward — another process may have
/// primed the cache in the meantime).
///
/// Default no-op. Vendors that talk to an IP-addressed device (LG,
/// future Sony/Samsung) should override to send a single transport-
/// layer datagram or open a TCP connection that triggers ARP
/// resolution on the way out.
async fn probe_arp_cache(&self, ip: IpAddr) -> Result<()> { Ok(()) }
```

- **LG override**: `tokio::net::UdpSocket::bind(<interface-local addr>)` + `send_to(&[0], (ip, 3001))`. Single byte; SSAP wss port discards it silently. No TLS handshake side-effect, no inbound port needed, no raw sockets.
- **Mock override**: stays at default `Ok(())` (no real network, no probe needed).
- **Trait-level justification**: connector owns transport mechanics (consistent with `connect`, `wake`, etc.); daemon orchestrates the enrichment chain. A vendor whose pairing service is TCP-only, or whose UDP port is firewalled, can pick any priming mechanism without the daemon needing to know — symmetric with how `connect` lets each vendor own its connection setup. The first-draft "`probe_port() -> Option<u16>`" hook would have leaked the daemon's chosen mechanism into the trait, blocking future vendors whose ports don't fit the UDP model.

### Per-caller safety

The daemon must guard against `EnrichManualEntry` being called from a scripted loop to scan internal IPs:

- **Rate limit per caller** (token bucket keyed by D-Bus unique name): max 1 probe / sec sustained, burst of 5. On limit-trip: return an empty map and emit `info!(caller = ..., "EnrichManualEntry rate-limited")`. Standard token-bucket; no new deps (Arc<Mutex<HashMap<UniqueName, Bucket>>>).
- **Allowed-subnet filter**: probe only when `ip` is in RFC1918 (`10/8`, `172.16/12`, `192.168/16`), link-local (`169.254/16`), or IPv6 ULA (`fc00::/7`). Public/loopback addresses return empty without probing. Configurable via a new `[discovery] allow_subnets = ["..."]` knob in `daemon.toml` for lab setups / VPN-routed TVs / edge cases.
- **Per-IP coalesce**: `Arc<Mutex<HashMap<IpAddr, Shared<JoinHandle<…>>>>>` so a user re-clicking Add manually before the prior probe finishes joins the in-flight future instead of spawning a duplicate. Entries cleared on completion.
- **Outbound interface binding** (multi-NIC correctness): when sending the UDP probe, bind the socket to the local IP that routes to `ip`. Reuse `netlink::resolve_interface_for_ip(ip)` if it exists, or accept a routing-table query. Without this, multi-homed hosts (containers, namespaces, VLAN-bridged) probe the wrong segment and never get an ARP entry. **Verify whether this helper exists during implementation** — if not, this work converges with the SSDP plan's still-open "outbound interface selection" concern; coordinate.

### Wizard wiring

```rust
// crates/tisvlay-gui/src/lib.rs — on_manual_add handler.
wizard.on_manual_add(move || {
    let vendor = …;
    let ip = …;

    // Distinct loading state — *not* discovery_streaming, which
    // means "Scanning for TVs". manual-lookup-streaming reads as
    // "Looking up this IP" and drives a different label.
    wizard.set_manual_lookup_streaming(true);

    // Disable Continue while in flight so double-clicks can't fan
    // out parallel RPCs (the daemon-side coalesce backs this up;
    // the UI gate is the first line of defense and the only one
    // the user sees).
    // Slint binding: button-enabled := !manual-lookup-streaming.

    // Cancel handle: stash a oneshot::Sender like
    // WizardDiscoveryCancel. reset_wizard takes both and signals
    // both so closing mid-lookup doesn't leak a probe budget.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    *wizard_manual_lookup_cancel.lock() = Some(cancel_tx);

    let d = d.clone(); let app_weak = app.as_weak(); let we = we.clone();
    tokio::spawn(async move {
        let vendor_data = tokio::select! {
            _ = cancel_rx => HashMap::new(),
            // Wizard-side 1 s safety cap in case the daemon hangs.
            res = tokio::time::timeout(Duration::from_secs(1), d.enrich_manual_entry(&vendor, &ip)) =>
                res.unwrap_or_default().unwrap_or_default(),
        };
        on_ui(&app_weak, move |app| {
            let wizard = app.global::<PairingWizard>();
            wizard.set_manual_lookup_streaming(false);
            let entry = WizardEntry { vendor, address: ip, vendor_data, .. };
            // Existing push-entry + select + step transition.
        });
    });
});
```

`reset_wizard` extends to take `&WizardManualLookupCancel` alongside `&WizardDiscoveryCancel` and drops both — symmetric with the streaming-discovery commit's sidecar pattern.

### Schema-form: trust badge + helper override

Two render-time changes in `schema_form.rs` / the Slint form row:

1. **Trust badge.** Track per-field whether the current value came from `autofill_schema` (vs. user typing). On render, when `autofilled == true` and `value == autofill_source_value`, show a small "auto-detected" chip / leading mark beside the value. On any user edit, set `autofilled = false` permanently for that field. Applies uniformly to SSDP and manual paths.
2. **Manual-path helper override.** A new wizard-side property `manual-entry-mode: bool` (set true for manual-add entries, false for SSDP). When `true` *and* a field with autofill key `mac` ended up empty post-enrichment, render *"MAC not detected. Enter it manually to enable Wake-on-LAN (otherwise the TV won't power on from full-off)."* in place of the field's default `helper`. The schema field itself is untouched; the override is a wizard concern, not a backend concern.

The trust badge is generic; future autofill paths (e.g. pair-time MAC capture) get it for free.

## Sequencing

One PR, two commits — split unconditionally for review clarity:

1. **`feat(daemon): EnrichManualEntry RPC + connector probe_arp_cache + safety`** — daemon-side RPC, `TvConnector::probe_arp_cache` default + LG override, per-caller rate limit, subnet guard, per-IP coalesce, outbound-interface binding, tracing span, unit tests, D-Bus integration test against `TestDaemon`. Testable in isolation via `busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 EnrichManualEntry ss "lg" "192.168.1.170"`.
2. **`feat(gui): wire manual-add to EnrichManualEntry + trust badge`** — wizard call site, distinct `manual-lookup-streaming` property, Continue-disabled gate, cancel sidecar, schema-form trust badge, manual-path helper override.

Pre-merge:

- [ ] `prek run -a` clean.
- [ ] Manual smoke: SSDP firewall rule dropped, pair via manual entry, confirm MAC field is populated and trust badge appears.
- [ ] Manual smoke: TV powered off entirely (ARP can't resolve), pair via manual entry, confirm field is blank with the manual-path helper text visible.
- [ ] Manual smoke: spam the Add-manually button — confirm only one daemon-side probe fires (coalesce) and the Continue button is disabled mid-flight (UI gate).
- [ ] Manual smoke: type a public IP (e.g. `1.1.1.1`), confirm no UDP probe goes out (`tcpdump -i any 'host 1.1.1.1'` on the host) and the field stays empty.
- [ ] Multi-NIC manual smoke if you have one — confirm the probe binds to the interface routing to the target IP, not the default-route NIC.

## Alternatives considered

- **Pair-time MAC capture as primary path.** Compelling: pair always opens a real TCP connection, so ARP is guaranteed-warm at pair-success time. Write the MAC into the saved config from the daemon side. Wizard MAC field stays blank during form-fill. Decided against as the primary path because: (a) the wizard's `autofill` mechanism already exists and the symmetric SSDP/manual story is cleaner, (b) the user benefits from seeing what got auto-detected (trust signal), (c) pair-time capture is still valuable as belt-and-suspenders — filed as follow-up.
- **`probe_port() -> Option<u16>` instead of `probe_arp_cache(ip)`.** First-draft shape. Rejected: leaks the daemon's chosen priming mechanism (UDP) into the trait. A vendor whose pairing port is TCP-only, or whose UDP port is firewalled, has to lie to the trait or get nothing. `probe_arp_cache` puts the priming decision in the same crate that owns the protocol knowledge, matching the existing `connect` / `wake` shape.
- **Extend `PairingField` with a `helper_text` field.** First-draft assumption. Rejected — the field already exists as `helper`. Manual-path-specific text is a wizard-side render override, not a schema change.
- **Raw-socket arping** via libpcap / `CAP_NET_RAW`. Definitive ARP probe, but requires elevating the daemon's privileges — which the NixOS module's hardened systemd unit ([packaging/nix/module.nix:255-257](../../packaging/nix/module.nix#L255-L257)) explicitly forbids (`CapabilityBoundingSet = [ "" ]`). UDP probe achieves ~the same hit rate without any privilege escalation.
- **TCP connect to the SSAP port** instead of UDP send_to. Same ARP-priming effect, but the TCP SYN initiates a TLS handshake from the TV's perspective; the daemon then drops the connection, which the TV may log. UDP send_to is silent (TV's UDP stack discards the unexpected byte without surfacing it to the application layer). Negligible practical difference; UDP is the marginally cleaner choice.
- **Fixed `tokio::time::sleep(250ms)`** after probe instead of poll-with-deadline. Wastes ~245 ms in the common case where ARP resolves at 5-30 ms; only the "TV unreachable" path needs the full budget. Polling at 30 ms intervals trades a tiny CPU cost for noticeably snappier average latency.
- **Reuse `discovery_streaming` spinner** instead of a distinct `manual_lookup_streaming` property. Conflates "Scanning for TVs" with "Looking up this IP" in the user's visual model; would need the renderer to special-case the label, and the Continue-disabled gate has no natural hook. Distinct property is cleaner.
- **Always do the active probe** (even after passive lookup succeeds). Wastes the polling budget for users whose ARP cache is already warm. Not worth it — passive-then-conditional-probe is strictly better.
- **Cache enrichment results in the daemon.** Tempting (avoids re-probing on every manual-add retry), but introduces a stale-cache failure mode (MAC changes after TV's NIC swap) for a problem we don't have. The per-IP coalesce already handles the "user spammed the button" case. Defer until field reports indicate caching matters.

## Open questions

- **Does `netlink::resolve_interface_for_ip` exist today?** Plan assumes yes (mentioned in SSDP plan's open issues too). Verify during implementation; if not, this work needs to add it OR coordinate with the SSDP plan to share the helper. Multi-NIC correctness depends on it.
- `ARP_RESOLVE_BUDGET` initial value: 500 ms is a budget cap for the polling stage; on healthy LANs the typical hit is ≪ 100 ms. Worth field-tuning. If real LAN behavior consistently misses inside 500 ms (kernel resolution races the budget), bump to 750 ms.
- **Rate-limit shape.** Per-caller token bucket is conservative; per-source-IP would be even tighter but the wizard is the only legitimate caller today. Pick during implementation; if the GUI ever batches lookups (it doesn't today), bump the burst budget.
- **`_source` metadata key.** Adding the `"_source": "passive" | "active" | "none"` debug field to `vendor_data` is light surface. Worth exposing for the trust-badge tooltip ("Auto-detected via ARP cache (passive)") but adds one more thing that's part of the wire shape. Keep or drop during implementation — could also be threaded through a separate return field instead.

## Migration on merge

Per [docs/plans/README.md](README.md):

- The `--bind-port` / config / RPC mechanics around discovery now have two distinct enrichment entry points (`enrich_with_arp` in the streaming/one-shot path, `enrich_manual_entry` in the wizard manual-add path). When both ship, the architectural note worth folding into [docs/design.md](../design.md)'s future Discovery subsection is: *MAC enrichment is a kernel-ARP-cache read with vendor-controlled active priming, not a separate protocol — and that's a deliberate scope choice over mDNS / vendor-specific HTTP probes.*
- `TvConnector::probe_arp_cache` becomes part of the public connector contract — document under the trait's API surface in `docs/design.md`'s vendor-backend section. Default no-op means future vendors opt in only when relevant.
- Allowed-subnet config (`[discovery] allow_subnets`) becomes a config knob — document in [docs/configs/daemon.toml](../configs/daemon.toml) alongside `bind_port` and `poll_interval_ms`.
- Pair-time MAC capture follow-up: file as `docs/ideas/pair-time-mac-capture.md` if not implemented in the same PR.
- Schema-form trust badge becomes part of the schema-form rendering contract; document briefly in the wizard's section of `docs/design.md` so future field types know to participate.
- Delete this plan file. `grep -rn 'docs/plans/manual-entry-mac-autofill'` to find and re-point any inbound refs.

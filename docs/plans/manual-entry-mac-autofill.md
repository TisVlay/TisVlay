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
3. Vendor-owned probe mechanism: new `async fn probe_arp_cache(&self, ip: IpAddr) -> Result<()>` on `TvConnector` (default no-op). LG override sends a single UDP byte to port 3001, bound to the kernel-resolved source IP for multi-NIC correctness. Vendor owns *how* the cache gets primed; daemon orchestrates.
4. New netlink helper: `resolve_route_for_ip(ip) -> Option<RouteResolution>` returning both interface and `prefsrc` (preferred source IP) for the kernel's route to that destination. Reused by `probe_arp_cache` for multi-NIC-safe UDP socket binding; usable by SSDP and future enrichment paths too. Replaces the existing interface-only `resolve_interface_for_ip` helper (or lives alongside, TBD during implementation).
5. Daemon-side input gate: RFC1918 + link-local IPv4 subnets only. IPv6 inputs (including `::ffff:x.x.x.x`) return empty map. Out-of-subnet IPv4 returns empty. Single guard at the RPC entry point; the connector trait keeps the family-agnostic `IpAddr` signature so an IPv6 follow-up doesn't reshape it.
6. Wizard wiring: `on_manual_add` calls the RPC before transitioning to step 2; a distinct `manual-lookup-streaming` Slint property drives a focused loading state (not the discovery spinner); the Continue button is disabled while in flight (UI-side double-call guard); a cancel handle stashed alongside `WizardDiscoveryCancel` drops on `reset_wizard`; the wizard caps its own wait at 1 s so a hung RPC can't freeze the UI.
7. Schema-form trust signal: when a field is populated via `autofill_schema(…)`, mark the rendered row with a small "auto-detected" badge that clears the moment the user edits. Applies uniformly to SSDP and manual paths (the new enrichment is the right moment to fix this).
8. Wizard-side helper-text override: when enrichment returns no MAC, replace the field's default helper at render time with *"MAC not detected. Enter it manually to enable Wake-on-LAN (otherwise the TV won't power on from full-off)."* No schema change — see Alternatives.
9. Observability: a `tracing::info_span!` around each enrichment chain with structured fields (`vendor`, `ip`, `source: passive | active | none`, `elapsed_ms`). No client-facing wire shape for these — the daemon owns the observability surface; the wizard just gets `vendor_data` for autofill.
10. Tests: unit tests for the enrichment chain (passive-hit / probe-then-hit / miss / invalid input / out-of-subnet) with the netlink + probe boundaries stubbed via a small injection scaffold (see Test surface below). D-Bus integration test against the existing `TestDaemon` private-bus harness for the RPC contract.

Out:

- **IPv6 support.** Filed as [docs/ideas/ipv6-discovery-and-enrichment.md](../ideas/ipv6-discovery-and-enrichment.md). IPv6 touches the whole discovery stack (SSDP multicast scoping, NDP vs ARP, ULA subnet guard) and is bounded follow-up work, not a one-line type change. v1 of this plan rejects IPv6 inputs at the daemon RPC entry point; trait signatures stay family-agnostic so the follow-up doesn't churn the API.
- **Pair-time MAC capture.** Snoop ARP at successful-pair time, write the MAC back to the saved config. Pair always opens a real TCP connection, so ARP is guaranteed-warm at pair-success time. Filed as a follow-up because it touches the pair flow (which has its own async-spawn + progress-signal contract) rather than the enrichment layer.
- **Per-caller rate limit.** Users don't pair multiple TVs per second; scripted abuse is bounded by the subnet guard (probes can only reach private subnets the user can already probe directly). Adding a token bucket would just be a HashMap-leak source for no real security benefit.
- **Per-IP request coalesce.** The UI-side Continue-disabled gate prevents the only realistic GUI-driven double-call. A second simultaneous client (CLI, busctl) sending two probes to the same TV emits two UDP datagrams the TV silently discards — benign, not worth a coalesce layer's complexity.
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
  - Empty map when: address isn't a parseable IPv4, vendor is unknown,
    the address is outside the allowed subnet set, IP is IPv6, or the
    chain found nothing.
  - Never an error — wizards treat empty as "nothing to autofill"
    rather than as a failure. Invalid IP, IPv6, out-of-subnet, unknown
    vendor all collapse to "empty map" so the client has one shape to
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
    // (a) Fast no-op for misfired empty callbacks.
    if vendor.is_empty() || address.is_empty() { return HashMap::new(); }

    // (b) Parse + IPv4-only gate.
    let Ok(IpAddr::V4(ip)) = address.parse::<IpAddr>() else { return HashMap::new(); };

    // (c) Subnet allowlist (RFC1918 + link-local). Public IPs return
    //     empty without probing — limits the daemon's egress footprint
    //     to "places the user can already reach themselves."
    if !is_allowed_ipv4_subnet(ip) { return HashMap::new(); }

    // (d) Vendor lookup. Unknown vendors degrade to empty.
    let Some(connector) = registry.connector_for(vendor) else { return HashMap::new(); };

    let span = tracing::info_span!("enrich_manual_entry", %vendor, %ip);
    let _enter = span.enter();
    let started = Instant::now();
    let ip = IpAddr::V4(ip);

    // (e) Passive netlink read (free, succeeds for users who've
    //     previously pinged/browsed the TV).
    if let Some(mac) = netlink::arp_lookup(ip).await {
        tracing::info!(source = "passive", elapsed_ms = started.elapsed().as_millis() as u64, "MAC resolved");
        return mac_result(mac);
    }

    // (f) Vendor-supplied active probe. Connector chooses the
    //     mechanism (UDP byte to SSAP port for LG; future vendor
    //     could do TCP connect, mDNS query, vendor-specific HTTP).
    //     Default impl on TvConnector is a no-op, so unknown /
    //     probe-less vendors degrade gracefully to passive-only.
    //     Err is non-fatal — the netlink re-read might still hit.
    let _ = connector.probe_arp_cache(ip).await;

    // (g) Poll netlink with early-out. Beats a fixed sleep: cache
    //     typically resolves in tens of ms on a healthy LAN; only
    //     the "TV unreachable" case rides the full budget.
    //     tokio::time::sleep on the loop (rather than interval) so
    //     the first iteration always sleeps ≥ 30 ms — gives the
    //     probe a moment to round-trip before the first re-read.
    let deadline = Instant::now() + ARP_RESOLVE_BUDGET;  // 500 ms
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(30)).await;
        if let Some(mac) = netlink::arp_lookup(ip).await {
            tracing::info!(source = "active", elapsed_ms = started.elapsed().as_millis() as u64, "MAC resolved");
            return mac_result(mac);
        }
    }
    tracing::info!(source = "none", elapsed_ms = started.elapsed().as_millis() as u64, "MAC not resolved");
    HashMap::new()
}
```

Tuning:
- `ARP_RESOLVE_BUDGET = 500 ms` (cap on the post-probe poll window). Worst-case wall-time is passive (~5 ms) + probe send (~1 ms) + poll (~500 ms) + final read (~5 ms) ≈ **~510 ms**.
- Daemon-side cap is 600 ms outermost (wraps the whole function in `tokio::time::timeout`); wizard-side cap is 1000 ms safety net. Either firing surfaces a "MAC not detected" outcome cleanly.

No concurrency primitives — the function is a pure async chain. If two callers ask for the same IP at the same time, two probes go out; the kernel ARP cache is the same either way and the second pair of UDP datagrams is utterly trivial to the TV. The UI-side disabled-Continue gate handles the only realistic GUI-driven multi-call.

### Vendor-owned probe mechanism

Extend `TvConnector` with a default-impl method:

```rust
/// Send a probe to `ip` that primes the kernel ARP/NDP cache as a
/// side effect. Best-effort: the daemon uses this only as a fallback
/// when passive netlink lookup misses, and treats `Err(_)` as
/// non-fatal (it still re-reads netlink afterward — another process
/// may have primed the cache in the meantime).
///
/// Default no-op. Vendors that talk to an IP-addressed device (LG,
/// future Sony/Samsung) should override to send a single transport-
/// layer datagram or open a TCP connection that triggers the kernel's
/// neighbour resolution on the way out.
///
/// IP family agnostic by signature; today's daemon callers only
/// invoke this with IPv4. An IPv6 follow-up (see
/// `docs/ideas/ipv6-discovery-and-enrichment.md`) widens the daemon's
/// input gate without reshaping this trait.
async fn probe_arp_cache(&self, ip: IpAddr) -> Result<()> { Ok(()) }
```

- **LG override**:

  ```rust
  async fn probe_arp_cache(&self, ip: IpAddr) -> Result<()> {
      let route = netlink::resolve_route_for_ip(ip).await
          .ok_or_else(|| /* no route */)?;
      let socket = UdpSocket::bind((route.src_ip, 0)).await?;
      let _ = socket.send_to(&[0], (ip, 3001)).await;  // SSAP wss
      Ok(())
  }
  ```

  Single byte to the SSAP wss port; the port discards it silently. No TLS handshake side-effect, no inbound port needed, no raw sockets. Binding to `route.src_ip` is the multi-NIC fix — see next subsection.

- **Mock override**: stays at default `Ok(())` (no real network, no probe needed).

- **Trait-level justification**: connector owns transport mechanics (consistent with `connect`, `wake`, etc.); daemon orchestrates the enrichment chain. A vendor whose pairing service is TCP-only, or whose UDP port is firewalled, can pick any priming mechanism without the daemon needing to know — symmetric with how `connect` lets each vendor own its connection setup.

### Multi-NIC: route resolution → source-IP bind

The existing `netlink::resolve_interface_for_ip(ip) -> Option<String>` returns the interface *name* — useful for some purposes but the wrong primitive for binding an unprivileged UDP socket to the correct outbound NIC. Two ways to actually bind:

- **`SO_BINDTODEVICE` socket option** — requires `CAP_NET_RAW`, which the hardened NixOS module's systemd unit forbids ([packaging/nix/module.nix:256-257](../../packaging/nix/module.nix#L256-L257)). Dead end.
- **Bind the UDP socket to a local source IP** — the kernel's routing table tells you which local IP would be used as source for traffic to a given destination (`ip route get <ip>` exposes this as the `src` field, internally `prefsrc` in the netlink response). No elevated capabilities required.

Add a new netlink helper:

```rust
// crates/tisvlayd/src/netlink.rs
pub struct RouteResolution {
    pub iface: String,
    pub src_ip: IpAddr,
}

/// Resolve the kernel's outbound route for `dst_ip`. Returns both
/// the interface name and the preferred source IP that would be
/// used for traffic to that destination.
///
/// Equivalent to `ip route get <dst_ip>`. One RTM_GETROUTE round-
/// trip; no elevated capabilities required.
pub async fn resolve_route_for_ip(dst_ip: IpAddr) -> Option<RouteResolution> {
    // rtnetlink::IpRouteHandle::get(dst_ip).execute() →
    // pull oif_name + prefsrc from the response.
    // ~40-60 LOC including the rtnetlink message-building boilerplate.
}
```

LG's `probe_arp_cache` uses `route.src_ip` to bind the UDP socket. Single-NIC hosts: the only NIC's IP, same behavior as today. Multi-NIC hosts: probe goes out the correct segment. Reusable by the SSDP path eventually (the SSDP plan's still-open multi-NIC concern converges here).

Whether `resolve_interface_for_ip` survives alongside `resolve_route_for_ip` or gets superseded depends on whether anything else still needs just the interface name — settle during implementation.

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
    // out parallel RPCs. Slint binding:
    //     enabled := manual-ip != "" && !manual-lookup-streaming.

    // Cancel handle stashed like WizardDiscoveryCancel. reset_wizard
    // takes both and signals both so closing mid-lookup doesn't
    // leak the probe budget.
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
            wizard.set_manual_entry_mode(true);  // see Schema-form section
            let entry = WizardEntry { vendor, address: ip, vendor_data, .. };
            // Existing push-entry + select + step transition.
        });
    });
});
```

`reset_wizard` extends to take `&WizardManualLookupCancel` alongside `&WizardDiscoveryCancel`, drops both, and clears `manual-entry-mode` (so reopening the wizard for an SSDP-discovered TV doesn't carry the manual-mode helper text over). Symmetric with the streaming-discovery commit's sidecar pattern.

### Schema-form: trust badge + helper override

Two render-time changes in `schema_form.rs` / the Slint form row:

1. **Trust badge.** Extend `AutofilledRow` with `pub autofilled: bool` and thread it into the Slint `PairingFieldRow` struct as an `autofilled` property. On render, when `autofilled == true`, show a small "auto-detected" chip / leading mark beside the value. On any user edit, the Slint side flips `autofilled` to `false` for that row. Applies uniformly to SSDP and manual paths. *Touchpoints*: `AutofilledRow` is constructed in ~6 Rust sites + the Slint default — mechanical but not free; budget ~50-80 LOC for the ripple.

2. **Manual-path helper override.** A new wizard-side property `manual-entry-mode: bool` (set true post-enrichment for manual-add entries, cleared on `reset_wizard`). When `true` *and* the field's autofill key is `mac` *and* `value == ""`, render *"MAC not detected. Enter it manually to enable Wake-on-LAN (otherwise the TV won't power on from full-off)."* in place of the field's default `helper`. The schema field itself is untouched; the override is a wizard concern, not a backend concern.

The trust badge is generic; future autofill paths (e.g. pair-time MAC capture) get it for free. The helper override is MAC-specific in v1; can grow into a richer "manual-mode helper overrides per field" mechanism if a second case emerges.

### Test surface

The enrichment chain decomposes cleanly into testable units, but `arp_lookup` opens a real `new_connection()` to netlink today — there's no trait abstraction or feature shim. Two scaffolding options:

- **Function-pointer injection**: change `enrich_manual_entry`'s signature to take `arp_lookup: impl Fn(IpAddr) -> Future<Option<MacAddress>>`. Tests pass closures; production wires `netlink::arp_lookup`. ~10 LOC of refactoring.
- **Thin `NetlinkProvider` trait**: more principled but heavier. Defer unless a second consumer wants the same boundary.

Plan for: function-pointer injection (lighter), plus the same shape for `probe_arp_cache` indirection in unit tests. Budget ~60 LOC of scaffolding spread across the netlink module and the enrichment function.

Unit cases:
- Passive hit (arp_lookup returns Some on first call).
- Probe-then-hit (arp_lookup returns None initially, Some on second poll iteration).
- Miss (arp_lookup returns None throughout the budget).
- Invalid input (empty strings, malformed IP, IPv6, IPv4-mapped IPv6, public IP).
- Subnet guard config override (custom `[discovery] allow_subnets` accepts a public IP that the default would reject).
- Unknown vendor (registry returns None — empty map).

D-Bus integration test against `TestDaemon`:
- Call `EnrichManualEntry` over the private bus with a stub backend; assert the `a{ss}` wire shape round-trips correctly.
- Empty-input case returns empty map without error.

## Sequencing

One PR, two commits — split unconditionally for review clarity:

1. **`feat(daemon): EnrichManualEntry RPC + connector probe_arp_cache + route resolution`** — daemon-side RPC, `TvConnector::probe_arp_cache` default + LG override, `resolve_route_for_ip` netlink helper, subnet guard, IPv4 gate, allow-subnets config knob, tracing span, netlink-stubbing scaffold, unit tests, D-Bus integration test against `TestDaemon`. Testable in isolation via `busctl --user call org.tisvlay.Daemon /org/tisvlay/Manager org.tisvlay.Manager1 EnrichManualEntry ss "lg" "192.168.1.170"`. GUI-free.

2. **`feat(gui): wire manual-add to EnrichManualEntry + trust badge`** — wizard call site, distinct `manual-lookup-streaming` property, Continue-disabled gate, cancel sidecar, `reset_wizard` extension, `AutofilledRow.autofilled` field + `PairingFieldRow.autofilled` Slint property + per-field render badge, manual-path helper override via `manual-entry-mode` property. Daemon-side touch is zero.

LOC estimate (rough): commit 1 ~500 LOC (RPC + chain ~80, subnet guard ~20, route helper ~50, connector trait + LG override ~40, allow_subnets config ~20, tracing ~10, stubbing scaffold ~60, unit tests ~120, integration test ~80, misc ~20); commit 2 ~250 LOC (wizard wiring ~80, trust badge ripple ~80, helper override ~30, Slint property updates ~30, reset_wizard extension ~10, misc ~20). **Total ~750 LOC.** Larger than the v1 plan's ~150-250 estimate suggested; primarily route-helper + test scaffolding + trust-badge ripple.

Pre-merge:

- [ ] `prek run -a` clean.
- [ ] Manual smoke: SSDP firewall rule dropped, pair via manual entry, confirm MAC field is populated and trust badge appears.
- [ ] Manual smoke: TV powered off entirely (ARP can't resolve), pair via manual entry, confirm field is blank with the manual-path helper text visible.
- [ ] Manual smoke: type a public IP (e.g. `1.1.1.1`), confirm no UDP probe goes out (`tcpdump -i any 'host 1.1.1.1'` on the host) and the field stays empty.
- [ ] Manual smoke: type an IPv6 address, confirm empty return with no error.
- [ ] Manual smoke: spam the Add-manually button — confirm the Continue gate keeps double-calls from happening (UI-side).
- [ ] Multi-NIC manual smoke if you have one — confirm the probe binds to the route-derived source IP (`tcpdump -i any 'udp port 3001'`, check source address matches the expected NIC), not the default-route NIC.

## Alternatives considered

- **Pair-time MAC capture as primary path.** Compelling: pair always opens a real TCP connection, so ARP is guaranteed-warm at pair-success time. Write the MAC into the saved config from the daemon side. Wizard MAC field stays blank during form-fill. Decided against as the primary path because: (a) the wizard's `autofill` mechanism already exists and the symmetric SSDP/manual story is cleaner, (b) the user benefits from seeing what got auto-detected (trust signal), (c) pair-time capture is still valuable as belt-and-suspenders — filed as follow-up.
- **`probe_port() -> Option<u16>` instead of `probe_arp_cache(ip)`.** First-draft shape. Rejected: leaks the daemon's chosen priming mechanism (UDP) into the trait. A vendor whose pairing port is TCP-only, or whose UDP port is firewalled, has to lie to the trait or get nothing. `probe_arp_cache` puts the priming decision in the same crate that owns the protocol knowledge, matching the existing `connect` / `wake` shape.
- **Extend `PairingField` with a `helper_text` field.** First-draft assumption. Rejected — the field already exists as `helper`. Manual-path-specific text is a wizard-side render override, not a schema change.
- **Per-caller token-bucket rate limit.** Pre-v3 plan included this. Dropped: the realistic abuse model (scripted scanning loop) is bounded by the subnet guard to "places the user can already probe directly." The HashMap eviction work + the D-Bus caller-identity workarounds (zbus 5.x doesn't expose unique-name via `#[zbus(header)]`) are real costs for no real security benefit. Subnet guard alone is sufficient.
- **Per-IP request coalesce** (`Mutex<HashMap<IpAddr, broadcast::Sender<…>>>` keyed per IP). Pre-v3 plan included this. Dropped: the UI-side Continue-disabled gate handles the GUI-driven double-call case. A second simultaneous client (CLI, busctl) sending two probes to the same TV emits two trivial UDP datagrams the TV silently discards — benign, not worth the coalesce-machinery complexity.
- **`SO_BINDTODEVICE` for outbound-interface binding** instead of source-IP binding. Cleaner conceptually but requires `CAP_NET_RAW`, which the hardened systemd unit forbids. Source-IP binding via route lookup achieves the same outbound-interface selection with zero elevated capabilities.
- **Raw-socket arping** via libpcap / `CAP_NET_RAW`. Definitive ARP probe, but requires elevating the daemon's privileges — same `CAP_NET_RAW` blocker. UDP probe achieves ~the same hit rate without any privilege escalation.
- **TCP connect to the SSAP port** instead of UDP send_to. Same ARP-priming effect, but the TCP SYN initiates a TLS handshake from the TV's perspective; the daemon then drops the connection, which the TV may log. UDP send_to is silent (TV's UDP stack discards the unexpected byte without surfacing it to the application layer). Negligible practical difference; UDP is the marginally cleaner choice.
- **Fixed `tokio::time::sleep(250ms)`** after probe instead of poll-with-deadline. Wastes ~245 ms in the common case where ARP resolves at 5-30 ms; only the "TV unreachable" path needs the full budget. Polling at 30 ms intervals trades a tiny CPU cost for noticeably snappier average latency.
- **Reuse `discovery_streaming` spinner** instead of a distinct `manual_lookup_streaming` property. Conflates "Scanning for TVs" with "Looking up this IP" in the user's visual model; would need the renderer to special-case the label, and the Continue-disabled gate has no natural hook. Distinct property is cleaner.
- **Always do the active probe** (even after passive lookup succeeds). Wastes the polling budget for users whose ARP cache is already warm. Not worth it — passive-then-conditional-probe is strictly better.
- **Cache enrichment results in the daemon.** Tempting (avoids re-probing on every manual-add retry), but introduces a stale-cache failure mode (MAC changes after TV's NIC swap) for a problem we don't have. Defer until field reports indicate caching matters.
- **`_source` debug key in the returned `a{ss}` map** (e.g. `{"mac": "…", "_source": "active"}`). Considered for the wizard's trust-badge tooltip. Dropped: leaks daemon implementation detail into the wire shape, and the underscore-prefix-flag-as-metadata convention is fragile (a future vendor could pick `_foo` and break it). The daemon emits `source` in its `tracing` span for ops; the wizard's badge just keys off "value was autofilled," no source detail needed.
- **Address parsing via `ipnet` crate for the subnet guard.** Considered. The guard is a small fixed set (RFC1918 + link-local IPv4); `std::net::Ipv4Addr` has `is_private()` and `is_link_local()` methods that cover exactly the v1 scope. No new dep needed for v1. The `[discovery] allow_subnets` config knob (for lab/VPN edge cases) parses single IPs or `a.b.c.d/n` strings; `ipnet` becomes worthwhile only if the knob actually gets used (defer).

## Open questions

- **Does `resolve_interface_for_ip` survive alongside `resolve_route_for_ip`?** Plan adds the latter; the former exists at [crates/tisvlayd/src/netlink.rs:29](../../crates/tisvlayd/src/netlink.rs#L29). If no caller needs interface-only, replace; if anything still does, keep both. Verify during implementation.
- `ARP_RESOLVE_BUDGET` initial value: 500 ms is a budget cap for the polling stage; on healthy LANs the typical hit is ≪ 100 ms. Worth field-tuning. If real LAN behavior consistently misses inside 500 ms (kernel resolution races the budget), bump to 750 ms.
- **MAC freshness under DHCP / NIC swap.** Probed MAC is "current as of the lookup"; a TV that gets a different MAC (NIC replacement, factory reset on certain firmwares) would persist a stale value in the saved config. v1 doesn't address this — the pair-time MAC capture follow-up would naturally subsume re-detection on every successful pair.

## Migration on merge

Per [docs/plans/README.md](README.md):

- The `--bind-port` / config / RPC mechanics around discovery now have two distinct enrichment entry points (`enrich_with_arp` in the streaming/one-shot path, `enrich_manual_entry` in the wizard manual-add path). When both ship, the architectural note worth folding into [docs/design.md](../design.md)'s future Discovery subsection is: *MAC enrichment is a kernel-ARP-cache read with vendor-controlled active priming, not a separate protocol — and that's a deliberate scope choice over mDNS / vendor-specific HTTP probes.*
- `TvConnector::probe_arp_cache` becomes part of the public connector contract — document under the trait's API surface in `docs/design.md`'s vendor-backend section. Default no-op means future vendors opt in only when relevant.
- `resolve_route_for_ip` netlink helper is reusable beyond the manual-entry path (SSDP outbound-interface selection, future enrichment paths) — document under `docs/watchers/network.md` alongside the existing carrier/route resolution.
- Allowed-subnet config (`[discovery] allow_subnets`) becomes a config knob — document in [docs/configs/daemon.toml](../configs/daemon.toml) alongside `bind_port` and `poll_interval_ms`.
- Pair-time MAC capture follow-up: file as `docs/ideas/pair-time-mac-capture.md` if not implemented in the same PR.
- IPv6 deferred work already filed at [docs/ideas/ipv6-discovery-and-enrichment.md](../ideas/ipv6-discovery-and-enrichment.md) — cross-link from `docs/design.md`'s Discovery subsection so future contributors find it.
- Schema-form trust badge becomes part of the schema-form rendering contract; document briefly in the wizard's section of `docs/design.md` so future field types know to participate.
- Delete this plan file. `grep -rn 'docs/plans/manual-entry-mac-autofill'` to find and re-point any inbound refs.

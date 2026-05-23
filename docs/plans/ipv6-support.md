# Full IPv6 support

Bring full IPv6 support to the tisvlay stack — daemon, GUI, CLI, NixOS module. Today's stack is IPv4-only by deliberate scope; per-layer audit (three parallel research agents) confirmed the abstraction layers are already mostly v6-ready, but a small number of concrete IPv4 chokepoints in the LG vendor crate, one daemon gate, and the SSDP transport need addressing — along with GUI display widening for v6's ~39-char addresses and a per-vendor family-declaration mechanism so the wizard can surface "this vendor supports v6 control but WoL requires v4" up front.

## Status

**SUDO PLAN** (skeleton) — agreed decisions captured. One major open question on per-vendor family-declaration shape (see [Open questions](#open-questions)) needs cross-project research before this is implementation-ready. Promoting from the now-deleted `docs/ideas/ipv6-discovery-and-enrichment.md`.

## Motivation

The "v6 deferred" framing held while SSDP+streaming-discovery and manual-entry MAC autofill shipped — both intentionally rejected v6 inputs at the daemon entry. With both PRs merged, the v6 deferral is now the load-bearing limit on the project's network-topology reach. Several visible consequences:

- Users on v6-only LANs (uncommon today on home networks, more common in enterprise / IPv6-mandate environments) can't use tisvlay at all.
- Users on dual-stack LANs whose TVs prefer the v6 address advertised in DHCPv6 hit a degraded path: SSDP discovery returns the v4 address, but later control attempts may silently route over v4 even when the user expected v6.
- Future vendor backends (Sony, Samsung, Roku) that prioritize v6 in their device discovery cannot be cleanly added on a v4-only foundation.
- The "deliberate IPv4-only correctness gate" comments now scattered through the codebase have shelf-life; they should resolve to "IPv4-or-IPv6, depending on vendor capability" once we lift the gates.

Goal: full IPv6 support across the entire workspace, with per-vendor capability declarations so v4-only vendors degrade gracefully (the wizard surfaces what's supported; nothing silently fails).

## Scope

In:

1. **Lift the three concrete daemon-side IPv4 gates** — `manager.rs:128` (`if !ip.is_ipv4()` in `EnrichManualEntry`), `connection.rs:134,142` (v6 URL bracketing), `backend.rs:800` (UDP bind branching).
2. **Extend `tisvlay-ssdp` to dual-stack** — `ff02::c` multicast group, per-interface scoping (v6 link-local multicast requires explicit outbound interface), `SearchOptions::bind_addr` accepts v6, vendor-neutral `Error::Ipv6BindUnsupported` removed.
3. **GUI display widening** — five Slint widget sites need `overflow: elide` / `wrap: word-wrap` for v6's ~39-char addresses: `DiscoveryRow` caption, `StepConfigure` + `StepDone` address Texts, `TvDetailHeader` subtitle (`pairing_wizard.slint:183,593,869`, `tvs_pane.slint:477`). Placeholder `"192.168.1.100"` updates to a generic v6-aware example (stays outside i18n).
4. **GUI input validation** — reuse the schema-row validator at the manual-add entry to bounce malformed input client-side before the EnrichManualEntry round-trip. Already accepts both families; no new validation logic.
5. **Per-vendor family declaration** — vendors declare which families they support, so the wizard / pair flow can surface degraded capabilities (v6-only TV: WoL gated off; v4-only vendor: reject v6 manual-add). **Shape TBD — see Open questions.**
6. **LG packet-capture validation** — before SSDP-v6 ships, packet-capture an LG TV to confirm: WebSocket on port 3001 over v6 works, SSDP responses on `ff02::c` (the survey's prediction: WebSocket yes, SSDP no, WoL never). Drives whether SSDP-v6 ships in this PR or as a follow-up.
7. **Workspace dep audit** — confirm no transitive deps assume v4-only addresses. `rtnetlink` is already v6-clean; the v6 arm in `resolve_route` (`netlink.rs:62`) just needs filling in.
8. **NixOS module** — confirm `bindPort` + `openFirewall` story extends to v6 (port number is family-agnostic; firewall rule is per-port). Document that `services.tisvlay.bindPort` opens both v4 and v6 listeners on the configured port.

Out:

- **`CAP_NET_RAW` WoL** — filed as [docs/ideas/wol-raw-socket.md](../ideas/wol-raw-socket.md). The WoL-via-raw-socket work unlocks WoL on more network topologies (cross-VLAN, VPN-default-route, v6-only-LAN) but isn't "v6 WoL" in any standardized sense. Deliberate non-expansion of the privilege surface for v1.
- **mDNS as discovery fallback** — separate scope; `mdns-sd` is the picked crate when we go there. The v6 plan keeps SSDP as the only discovery transport, dual-stack.
- **CIDR / subnet types** — no current consumer in the workspace (subnet allowlist was dropped during the manual-entry MAC autofill work). std::net covers v1 needs; `ipnet` adds only when a concrete use site appears (mDNS scope, WoL directed-broadcast, allowlist revival).
- **IPv4 broadcast directed-wake** — different topology problem; not what v6 support is about.

## Audit findings (per-layer summary)

### Daemon-side address handling

**Already v6-clean** (no work needed):

- All wire shapes (`AddTv` config_json, `DiscoveryFound` columns, `Tv1.UpdateConfig`, `EnrichManualEntry` args) are strings — no v4 type bound at any RPC boundary.
- All persistence (`LgVendorConfig.ip: String`, TOML round-trip) accepts v6 strings today.
- `PairingField::validate` for `Ip` type already accepts both v4 + v6 via `IpAddr::from_str` (`pairing_schema.rs:243-246`).
- `netlink::resolve_route` + `arp_lookup` are already family-aware; v6 paths are dead code only because callers gate v6 out.
- `TvConnector::probe_arp_cache(ip: IpAddr)` is already family-agnostic at the trait level.

**Three concrete v4 chokepoints** (this is the entire "lift the gates" work):

| # | Site | Change |
|---|---|---|
| 1 | `crates/tisvlayd/src/dbus/manager.rs:128` | Remove `if !ip.is_ipv4()` gate |
| 2 | `crates/tisvlay-lg/src/connection.rs:134,142` | Bracket v6 literals in `format!("wss://{}:{}", host, …)` via a helper |
| 3 | `crates/tisvlay-lg/src/backend.rs:800` | Branch `UdpSocket::bind` on `ip.is_ipv6()` (v4 → `0.0.0.0:0`, v6 → `[::]:0`) |

Plus the empty v6 arm in `netlink::resolve_route` (`netlink.rs:62`) needs filling in (currently truncated; the v4 arm above is the template).

### GUI / CLI / session / tray

- **GUI manual-entry input**: `PairingField::validate` already accepts v6; the manual-add handler should reuse it before the daemon round-trip. Today's `ip.is_empty()` check is the only client-side gate.
- **GUI display**: five widget sites need overflow/wrap policy bumps. TVs master list doesn't render addresses (safe). LineEdits scroll horizontally already.
- **CLI**: `tisvlay pair`'s `address: String` stays untyped at clap — daemon-side per-vendor validation is the right level (matches the existing design).
- **Session + tray**: zero address surface. No work.

### WoL

**There is no real IPv6 WoL.** IEEE Wake-on-LAN is L2-by-design; no IETF v6 equivalent. NIC firmware "wake on v6 multicast" is rare-to-nonexistent on consumer hardware. LG TVs implement classic v4 WoL only.

Recommendation: keep `WakeOptions.broadcast_addr: Ipv4Addr`. The per-vendor family declaration (Open question below) lets us surface "v6-only TV → WoL not available" cleanly in the wizard. `CAP_NET_RAW`-based topology flexibility filed as a separate idea.

## Crate picks (resolved)

| Role | Pick | Why |
|---|---|---|
| Host address | `std::net::IpAddr` | Already pervasive; v6 variants stable |
| Address parsing | `IpAddr::from_str` | Already pervasive; codebase's `address.parse::<IpAddr>()` lines are v6-correct |
| CIDR / subnet | **Deferred** — no current use site | std covers v1; `ipnet` is the future pick if needed |
| WoL packet | **Stay in-tree** (`crates/tisvlay-core/src/wol.rs`) | ~130 LOC, no protocol-level upside from a crate; `wol-rs` works but adds no value |
| Netlink | **Keep `rtnetlink`** (existing dep) | Already v6-clean; `IpVersion` enum + `RouteMessageBuilder<Ipv6Addr>` already in use |
| SSDP | **Extend in-tree `tisvlay-ssdp`** | Already own the v4 path; adding `ff02::c` + per-interface bind keeps single ownership |
| mDNS (separate scope) | `mdns-sd` | Pure Rust, dual-stack, NixOS-friendly (no Avahi/Bonjour C dep) |

## Open questions

### Per-vendor family declaration shape (BLOCKING — research needed)

Vendors need a way to declare which IP families they support so the wizard / pair flow can surface degraded capabilities cleanly (the load-bearing case: v6-only TV → WoL not available → wizard should disable the wake-related UI rather than silently fail at first power-on attempt).

Two natural shapes:

**(a) Extend `Capabilities` bitflag.** Add `IPV4_NETWORK`, `IPV6_NETWORK`, `WAKE_REQUIRES_IPV4` bits to the existing per-vendor `tisvlay_core::Capabilities` (currently `DISCOVERY`, `POWER`, `SCREEN`, etc.). Vendor-level statement, coarse-grained, matches existing pattern. Pros: single source of truth, derived behavior throughout. Cons: address-shape gating lives away from where format validation already lives.

**(b) Per-field schema validation directive.** Extend `PairingFieldType` with `Ipv4Only` / `Ipv6Only`, or add per-field `validation: Some("ipv4-only")` to `PairingField`. Address-shape validation co-located with format validation; surfaces in the wizard's per-field inline error. Pros: tight UX feedback loop, vendor declares per-field. Cons: spreads "what this vendor supports" across multiple fields rather than a single capability statement.

User leans (a) for single-source-of-truth, with reservations about whether colocating gating near the interface (b) is cleaner. **Worth a research pass** before committing — how do other multi-vendor projects handle vendor-level capability declarations vs per-field validation? Candidates to survey:

- **Home Assistant integrations** — `config_flow.py` per-integration declares supported connection types; would map to (b) but is per-flow not per-field.
- **Homebridge plugins** — `accessory.json` declares capabilities at the accessory level; closer to (a).
- **MQTT2HASS** / **Tasmota** — per-device "supported features" declarations; varies.
- **OpenWrt LuCI** — per-protocol field validation; closer to (b).
- **systemd-networkd** unit-file model — per-section capabilities.

Pick after the survey resolves. Until then, the plan is otherwise complete.

### LG packet-capture protocol

Before SSDP-v6 ships, capture an LG TV's responses to confirm the survey's predictions. Specifically:

```bash
# WebSocket SSAP over v6 — expected to work
nc -6 -v "$LG_V6_ADDR" 3001  # TCP handshake should connect
curl --insecure "wss://[$LG_V6_ADDR]:3001"  # WebSocket handshake

# SSDP M-SEARCH via ff02::c — expected to fail
echo -e "M-SEARCH * HTTP/1.1\r\nHOST: [ff02::c]:1900\r\nMAN: \"ssdp:discover\"\r\nST: ssdp:all\r\nMX: 2\r\n\r\n" | nc -6 -u "ff02::c%$IFACE" 1900

# Then tcpdump on the interface to see what LG replies (if anything)
sudo tcpdump -i "$IFACE" -A 'host <LG_V6_ADDR> or net ff02::c/16'
```

Drives whether SSDP-v6 ships in this PR or as a follow-up.

## Implementation phasing (after open question resolved)

Tentative — refines once question (5) resolves:

1. **Phase 1: Lift the daemon gates + LG vendor chokepoints + per-vendor families declaration.** The three concrete daemon-side IPv4 gates + LG URL bracketing + LG UDP-bind branching + the chosen family-declaration mechanism. GUI input gate. Daemon and core only — no SSDP transport changes yet. Pure transport-level v6 support; discovery stays v4. Testable: type a v6 address into the manual-add field, pair successfully, control the TV. Estimated ~250-400 LOC depending on which family-declaration shape we pick.
2. **Phase 2: SSDP-v6 in `tisvlay-ssdp` + GUI display widening.** Dual-stack discovery (or fall-through-to-v4-only if packet capture says LG doesn't answer on `ff02::c`). Slint widget overflow/wrap pass. Estimated ~200-300 LOC, gated on packet-capture validation.
3. **Phase 3: LG-specific fix-ups + NixOS module + docs.** Whatever the packet capture surfaces. `docs/discovery.md` updates (drop "IPv4-only" framing throughout). `docs/firewall.md` cross-link to v6 rule recipes if needed. NixOS module confirmation that `bindPort` opens both families.

## Pre-merge checklist

(Filled in once Phase 1 is scoped.)

## Migration on merge

Per [docs/plans/README.md](README.md):

- Architecture / decisions worth re-reading → fold into [docs/discovery.md](../discovery.md). The current "IPv4-only correctness gate" subsections widen to "IPv4 + IPv6, gated per-vendor."
- Per-vendor family-declaration mechanism → document at the `TvConnector` trait surface in [docs/discovery.md](../discovery.md) and [docs/design.md](../design.md)'s vendor-backend section.
- LG packet-capture findings → file under `docs/lg/ipv6-behavior.md` (or fold into `docs/lg/firmware-quirks.md`).
- Delete this plan file. `grep -rn 'docs/plans/ipv6-support'` to find and re-point any inbound refs.

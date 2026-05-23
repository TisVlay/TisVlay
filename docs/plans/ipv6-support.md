# Full IPv6 support

Bring full IPv6 support to the tisvlay stack — daemon, GUI, CLI, NixOS module. Today's stack is IPv4-only by deliberate scope; per-layer audit (three parallel research agents) confirmed the abstraction layers are already mostly v6-ready, but a small number of concrete IPv4 chokepoints in the LG vendor crate, one daemon gate, and the SSDP transport need addressing — along with GUI display widening for v6's ~39-char addresses and a per-vendor family-declaration mechanism so the wizard can surface "this vendor supports v6 control but WoL requires v4" up front.

## Status

**Implementation-ready.** Promoted from the now-deleted `docs/ideas/ipv6-discovery-and-enrichment.md` after a per-layer architectural audit (three parallel research agents) and a follow-on cross-project survey of multi-vendor capability/validation patterns (Home Assistant, Homebridge, OpenWrt LuCI, KDE Solid, rclone) resolved the per-vendor family-declaration question — see [Alternatives considered](#alternatives-considered).

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
5. **Per-vendor family declaration** — extend `Capabilities` bitflag (`tisvlay-core::traits::Capabilities`) with `IPV4` and `IPV6` bits. Vendor-level statement, queryable by every layer (wizard, pair flow, GUI, daemon's `Transport::Network`-scoping path). LG WebOS today: `IPV4`. WoL availability stays derived from `WOL && !IPV6` — not stored separately. `PairingField::validate` stays shape-only; no `PairingFieldType::Ipv4Only`. The wizard reads `capabilities()` *before* rendering the form and either omits incompatible fields or shows a one-time disclosure ("LG WebOS uses IPv4 — enter the v4 address of your TV"). Future per-field gating for the multi-transport-variant-per-vendor case is reserved as a rclone-style `Provider`-on-field extension point (`requires_capability: Option<Capability>` on `PairingField`), documented in code but not shipped in v1.
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

## Alternatives considered

### Per-vendor family declaration shape — resolved: extend `Capabilities` bitflag

The forcing question was where "vendor X supports IPv4 but not IPv6, so WoL requires a v4 secondary address" should live. Two natural shapes weighed:

- **(a) Extend `Capabilities` bitflag** with `IPV4` / `IPV6` bits. Vendor-level statement; single source of truth; queryable by every layer.
- **(b) Per-field schema validation directive** — `PairingFieldType::Ipv4Only` or per-field `validation: Some("ipv4-only")`. Address-shape gating co-located with format validation; surfaces per-field at form time.

**Resolution: (a) wins**, based on cross-project survey:

- **OpenWrt LuCI** is the cleanest precedent — two orthogonal typed surfaces: per-field `datatype` validators (`ip4addr`, `ip6addr`, `ipaddr`, `macaddr`) for shape, *and* per-protocol `isVirtual()`-style methods on a `Protocol` subclass for capability. Neither leaks into the other. ([LuCI Network.Protocol API](https://openwrt.github.io/luci/jsapi/LuCI.network.Protocol.html), [LuCI CBI docs](https://github.com/openwrt/luci/blob/master/docs/CBI.md))
- **Home Assistant** deliberately *doesn't* embed transport assumptions in form schemas. webostv's `config_flow.py` asks for *just the host string*; MAC is harvested from discovery; WoL availability lives downstream. The recent Samsung implicit-WoL deprecation ([core#166226](https://github.com/home-assistant/core/issues/166226)) was them paying down the bill from putting "needs MAC for WoL" knowledge in the integration's form surface — the fix was *removing* that surface, not refining it.
- **Homebridge** is the cautionary tale for (b) without (a): JSON-Schema `condition.functionBody` (raw JS!) + retrofitted runtime characteristic clamping to handle constraints the form layer couldn't see. Tell-tale of an underexpressive data model.
- **rclone** is the most mature analogue — three mechanisms (`Options`, `Features`, and crucially **`Provider` on each `Option`** for sub-vendor variant gating). The `Provider` trick is the smart half-step between (a) and (b): per-field gating without polluting a separate capability surface, but only when one vendor has multiple transport variants. ([rclone fs.Option](https://github.com/rclone/rclone/blob/master/fs/registry.go), [rclone fs.Features](https://github.com/rclone/rclone/blob/master/fs/features.go))
- **KDE Solid** uses capability-by-interface-query (`is<StorageVolume>()`/`as<StorageVolume>()`) — stronger than a bitflag, hard to lie about. Right precedent for "vendor X supports operation Y" being a typed dispatch table, not a runtime opaque check.

The specific shape chosen for tisvlay:

1. Add `IPV4` and `IPV6` to `Capabilities`. Per-connector facts about the *transport*.
2. **Don't** add `WAKE_REQUIRES_IPV4` — derive it (`WOL && !IPV6`). Storing derivations is how flag sets rot (Samsung's implicit-WoL deprecation is the receipt).
3. **Don't** add `PairingFieldType::Ipv4Only`. The "this connector only speaks v4" gate belongs on the connector, not the field.
4. **Reserve** the rclone `Provider`-on-field trick (`requires_capability: Option<Capability>` on `PairingField`) for when one vendor ships multiple transport variants. Document the extension point in code; don't ship until justified by a real second-vendor case.
5. **UX flow**: wizard reads `capabilities()` *before* form-build, omits incompatible fields or shows a one-time disclosure. Per-field validators still catch malformed input at submit time. Capability mismatches never reach submit.

### `WAKE_REQUIRES_IPV4` as a stored bit — rejected

Considered briefly as a sibling of `IPV4`/`IPV6` for clarity. Rejected per the rclone+HA lessons: derive it, don't store it. Adding a stored bit for "logical consequence of other bits" creates a class of "the flags disagree" bugs (Samsung's implicit-WoL story).

### `PairingFieldType::Ipv4Only` — rejected

Considered briefly as the simpler "per-field" choice. Rejected per the Homebridge cautionary tale: form-time field validation can't see protocol-layer constraints without escape hatches (raw JS, runtime clamping). Keep `validate` for shape; capability lives on the connector.

## Open questions

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

## Implementation phasing

Three phases, each landing as its own PR:

1. **Phase 1 — Capabilities + transport-level v6 support.** Add `Capabilities::IPV4 | IPV6`; LG declares `IPV4`. Lift the three daemon-side IPv4 gates (`manager.rs:128`, `connection.rs:134,142`, `backend.rs:800`). Fill in `netlink::resolve_route`'s empty v6 arm. Add the URL-bracketing helper for v6 literals. Wizard reads `capabilities()` at form-build time; GUI input gate reuses the schema-row validator (already v6-accepting). No SSDP transport changes yet — discovery stays v4. **Testable**: type a v6 address into the manual-add field, pair successfully, control the TV. Estimated ~250-350 LOC.
2. **Phase 2 — SSDP-v6 + GUI display widening.** Gated on Phase 0 packet capture (see Open questions): if LG answers on `ff02::c`, extend `tisvlay-ssdp` to dual-stack (`ff02::c` group, per-interface scoping, `SearchOptions::bind_addr` accepts v6, vendor-neutral `Error::Ipv6BindUnsupported` removed). If LG doesn't answer, Phase 2 ships GUI display widening only — five Slint widget overflow/wrap bumps for v6's ~39-char addresses + placeholder update. Estimated ~150-300 LOC depending on capture outcome.
3. **Phase 3 — NixOS module + docs migration.** Confirm `services.tisvlay.bindPort` opens both v4 and v6 listeners on the configured port. `docs/discovery.md` updates (drop "IPv4-only" framing throughout; document the `Capabilities::IPV4|IPV6` mechanism). `docs/firewall.md` v6 rule-recipe cross-link. LG packet-capture findings filed under `docs/lg/ipv6-behavior.md`. Plan deletes per migration-on-merge convention. Estimated ~50-100 LOC + docs.

**Phase 0 (pre-implementation)**: LG packet-capture validation (see [Open questions § LG packet-capture protocol](#lg-packet-capture-protocol)). Drives Phase 2's scope. Not a commit — a research session that updates the plan with findings.

## Pre-merge checklist

Per-phase. Phase 1:

- [ ] `prek run -a` clean.
- [ ] Unit test: schema validator accepts both v4 and v6 inputs uniformly for the `Ip` field type (regression guard).
- [ ] Unit test: `Capabilities::IPV4|IPV6` bits round-trip through the existing `to_strings()` / `from_strings()` if those exist.
- [ ] Manual smoke: type a v6 address (e.g. host's link-local) into the manual-add IP field — confirm wizard accepts, daemon's `EnrichManualEntry` returns (now-empty since LG is `IPV4`-only and the wizard hides the v6 case in Phase 1, OR returns the MAC if v6 enrichment lands here).
- [ ] Manual smoke: verify NetworkManager-style scenarios where v6 link-local is the only available address — wizard's disclosure / field-omission renders correctly.
- [ ] `docs/discovery.md`'s "IPv4-only correctness gate" subsections updated to reflect lifted gates.

Phase 2:

- [ ] LG packet-capture protocol completed (see Phase 0); findings folded into the plan + idea docs as appropriate.
- [ ] If SSDP-v6 ships: dual-stack discovery returns LG TVs over `ff02::c` *or* gracefully degrades to v4-only with a `tracing::warn!` line.
- [ ] GUI display visual eyeball at 0/1/2/3+ TVs with v6 addresses — no clipping on `DiscoveryRow`, `StepConfigure`, `StepDone`, `TvDetailHeader`.

Phase 3:

- [ ] `services.tisvlay.bindPort = 1901; openFirewall = true;` on a NixOS host — confirm both v4 and v6 listeners on port 1901 (`ss -ulnp | grep tisvlayd` shows both `0.0.0.0:1901` and `[::]:1901`).
- [ ] All inbound refs to `docs/plans/ipv6-support.md` repointed to durable homes.

## Migration on merge

Per [docs/plans/README.md](README.md):

- Architecture / decisions worth re-reading → fold into [docs/discovery.md](../discovery.md). The current "IPv4-only correctness gate" subsections widen to "IPv4 + IPv6, gated per-vendor."
- Per-vendor family-declaration mechanism → document at the `TvConnector` trait surface in [docs/discovery.md](../discovery.md) and [docs/design.md](../design.md)'s vendor-backend section.
- LG packet-capture findings → file under `docs/lg/ipv6-behavior.md` (or fold into `docs/lg/firmware-quirks.md`).
- Delete this plan file. `grep -rn 'docs/plans/ipv6-support'` to find and re-point any inbound refs.

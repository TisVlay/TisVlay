# Discovery

`tisvlay` ships its own SSDP transport rather than depending on third-party crates or delegating to a Linux system service. Discovery has three rpc entry points on the daemon (`manager/rpc/discover`, the `discover-start`/`discover-stop` session pair, and `enrich`) sharing one vendor-neutral transport crate and one vendor-owned active-probe hook.

This doc covers the architectural decisions worth re-reading. For the firewall rules that pair with the bind-port story see the user guide's [Firewall & Discovery](book/src/operations/firewall-discovery.md), and for why SSDP needs one, [firewall.md](firewall.md). For deferred follow-up work see [docs/ideas/ssdp-notify-passive-listener.md](ideas/ssdp-notify-passive-listener.md) (passive NOTIFY listener) and [docs/ideas/pair-time-mac-capture.md](ideas/pair-time-mac-capture.md) (MAC capture at pair-success time).

## Why we own the SSDP transport

The workspace ships `tisvlay-ssdp` (vendor-neutral) exposing a single `search(targets, timeout, options) -> Stream<SsdpResponse>` with a configurable `bind_addr`. Three forces pushed us off the third-party-crate path:

- **Firewall-friendly source-port pinning.** Third-party crates like [`ssdp-client`](https://crates.io/crates/ssdp-client) always bind an ephemeral source port. Strict firewalls (NixOS by default, hardened workstations, container/VM hosts) can't write a tight `--dport` rule against an ephemeral range; they fall back to "accept any UDP from subnet," which is broader than necessary. Owning the crate lets us expose `bind_addr` directly.
- **No live Linux system service to delegate to.** KDE's Cagibi was dropped with Plasma 5, and GNOME's dleyna can't be asked for vendor URNs ([firewall.md § Why it's a Linux problem](firewall.md#why-its-a-linux-problem) has the survey). A system daemon doing the multicast would hit the same firewall reply drop anyway.
- **Forking `ssdp-client` doesn't solve the publish story.** Upstream PR [#13](https://github.com/jakobhellermann/ssdp-client/pull/13) attempted to add `bind_addr` but bundled it with IPv6 and quality issues; the maintainer hasn't engaged with it in over a year. Forking via `[patch.crates-io]` works for our distro builds but `cargo install` users would get unpatched code. Our usage surface was small enough (~4 call sites, one search function, four response accessors) that owning the crate outright is less long-term burden than maintaining a fork.

## Trait boundary

The `TvDiscovery::discover(timeout) -> Vec<DiscoveredTv>` trait in [`crates/tisvlay-core/src/traits.rs`](../crates/tisvlay-core/src/traits.rs) is the per-vendor discovery contract — **a bounded scan that returns a list**. Streaming (the wizard's "walk to the TV, walk back" UX) lives one layer up in the daemon, orchestrating bounded scans over time. This split keeps the vendor trait simple (no Stream return type, no session lifecycle), and means a future vendor backend that does discovery via a non-streamable mechanism (vendor cloud API, mDNS one-shot, etc.) can implement `TvDiscovery::discover` without confronting streaming machinery.

LG's discovery layer in [`crates/tisvlay-lg/src/discovery.rs`](../crates/tisvlay-lg/src/discovery.rs) calls `tisvlay_ssdp::search` with all three LG-specific search targets on a single shared socket (one timeout window), then applies LG-specific predicates (`is_lg_tv` against DLNA header / server string / USN) and parsing (DLNA name → model, USN → UUID, EDID PNPID `GSM`).

## Streaming discovery

The wizard's scan has to keep finding TVs while the user walks to the TV and back, so discovery runs as a session rather than one bounded call:

- `manager/rpc/discover-start` (optionally restricted to one vendor) opens a session and returns its id. A daemon-side task loops a 3 s scan of each discovery-capable backend, then sleeps 2 s, until stopped. The session holds a liveliness token at `alive/discovery/<session>`.
- Each finding is published at `discovery/<session>/found/<finding>`, with the MAC filled from the ARP cache when the vendor didn't report one. A finding seen again is the same key, so the subscriber sees one entry per TV.
- `manager/rpc/discover-stop` aborts the task, which drops its token. An unknown session id is refused.
- `manager/rpc/discover` is the bounded one-shot for scripted callers: one query whose findings stream back as separate replies, so the querier needs `ConsolidationMode::None` (see [architecture.md](architecture.md)). Its window is defaulted and clamped daemon-side to stay under zenoh's query timeout. `tisvlay list` doesn't scan; it reads the TVs' cached state.

The daemon keeps each session's task handle in a map keyed by session id, so stopping it is an abort. Nothing bounds a session that's never stopped; the wizard stops its own when it closes.

## Manual-entry MAC enrichment

`manager/rpc/enrich` (vendor and IP) autofills the pairing wizard's manual-entry path the same way discovered entries are autofilled: its reply carries the MAC, when found, for the wizard's `autofill_schema` engine. The chain:

1. **Vendor-supplied active probe** (`TvConnector::probe_arp_cache`; see [§ Connector probe hook](#connector-probe-hook)). The connector decides the mechanism (LG sends one UDP byte to the SSAP wss port), and the kernel resolves ARP/NDP as a side effect of the send. An unknown vendor skips the probe.
2. **Netlink neighbor-table read** of the address (`RTM_GETNEIGH`, not `/proc/net/arp`, which the packaged unit's `ProcSubset=pid` hides).

It never fails: a missing MAC means "nothing to autofill" (an address that doesn't parse, another subnet or VLAN, or a neighbor entry that hasn't resolved). The wizard gates the address field by the vendor's transport family before it asks (see [§ Per-vendor transport family](#per-vendor-transport-family)). The wizard treats empty as a soft outcome (the field stays blank, its helper prefixed with "MAC not detected.") rather than a hard failure. A blank MAC isn't the end of the line: for vendors that implement `TvConnection::get_link_mac`, the daemon reads the MAC from the device itself when the pairing completes and fills the field then. That's the only path that works for a TV on a routed or VLAN-isolated segment, where the ARP/NDP chain above only ever sees the gateway; see [architecture.md](architecture.md) (the `PairVerifying` invariant) and, for LG, [lg/firmware-quirks.md](lg/firmware-quirks.md). A remaining pair-time ARP snoop for vendors without a device-side MAC is parked in [docs/ideas/pair-time-mac-capture.md](ideas/pair-time-mac-capture.md).

## Per-vendor transport family

Vendors declare which IP families their transport supports via two bits on the `Capabilities` bitflag: `IPV4` and `IPV6`. Deliberately *not* part of `Capabilities::STOCK` (the conventional baseline feature set), so vendors that use `STOCK` for the feature set don't silently auto-claim a transport family they haven't verified. LG WebOS today declares both (SSAP WebSocket works identically over v4 and v6 — verified end-to-end, including the rustls cert chain and the SSAP register/subscribe handshake).

These bits are read in three places:

- **The wizard** reads each vendor's capabilities from the daemon's vendor catalog (`manager/vendors`) at form-build time. The manual-entry IP field's placeholder switches to the RFC 3849 documentation prefix `2001:db8::1` for v6-only vendors; v4-or-dual vendors keep the familiar `192.168.1.100` example.
- **The daemon** would use them to gate any future transport-specific code paths.
- **WoL availability is *derived*, not stored** — it's `WOL && IPV4`-locally-reachable. WoL is L2-by-design (IEEE magic packet over UDP broadcast); there is no IETF v6 equivalent and consumer NIC firmware effectively never supports "wake on v6 multicast." On a v6-only TV that shares an L2 segment with the daemon, today's v4 broadcast WoL still works because the host has a v4 address on that segment — verified on a SLAAC-ULA-only TV configuration. The cross-VLAN / VPN-default-route topology that would actually require lifted privileges is filed in [docs/ideas/wol-raw-socket.md](ideas/wol-raw-socket.md).

A stored `WAKE_REQUIRES_IPV4` bit was rejected during the design — derived flags are how flag sets rot (HA's recent Samsung implicit-WoL deprecation is the cautionary tale). The same forcing question pushed against putting "this connector is v4-only" on the schema-field validator: form-time field validation can't see protocol-layer constraints without escape hatches (Homebridge's `condition.functionBody` raw-JS pattern is the warning).

When a single vendor eventually ships multiple transport variants (one v6-capable, one not — rclone's `Provider`-on-field shape), the extension point is a `requires_capability: Option<Capability>` on `PairingField`. Documented here so the future addition has a place to land; not implemented until a real second-vendor case demands it.

### SSDP discovery is v4-only

A separate finding from the transport-family work: LG TVs do not join any v6 SSDP multicast group (`ff02::c`, `ff05::c`, `ff0e::c`) — verified by packet capture, see [docs/lg/firmware-quirks.md § IPv6 SSDP non-participation](lg/firmware-quirks.md#ipv6-ssdp-non-participation--tvs-are-silent-on-ff02c). Combined with the survey-time observation that other consumer TV vendors largely mirror LG's stance, this makes the v4-only SSDP transport the right design today. `tisvlay-ssdp::Error::Ipv6Unsupported` exists as a deliberate hard reject; the doc comment on the variant points back at the LG quirks finding so a future reader doesn't mistake it for a TODO. Lift the rejection (and thread v6 through the crate) when a vendor that actually answers on `ff02::c` is added.

## Connector probe hook

`TvConnector::probe_arp_cache(&self, ip: IpAddr) -> Result<()>` is the daemon's vendor-owned hook for priming the kernel ARP cache when passive lookup misses. Default no-op; vendor crates that talk to IP-addressed devices override to send a single transport-layer datagram or open a TCP connection that triggers kernel neighbor resolution on the way out. The trait method is family-agnostic (`IpAddr`, not `Ipv4Addr`) so an IPv6 follow-up doesn't reshape the trait.

LG's override sends a single UDP byte to the SSAP wss port (3001), bound to `0.0.0.0:0` — the kernel routing table picks the correct outbound NIC + source IP for the destination, which is the right behavior on typical multi-NIC hosts. The TV's UDP stack discards the unexpected byte silently (no TLS handshake side-effect, no inbound port needed, no `CAP_NET_RAW`). Explicit source-IP binding via netlink route lookup would only matter for VPN-policy-routing edge cases that aren't in scope today; filed as deferred work if anyone hits it.

## Shared SSDP socket (concurrent-session coalescing)

Discovery binds **one** SSDP source socket per daemon — lazily, on the first scan — and shares it across every concurrent discovery: the one-shot `discover` and any number of streaming sessions. It lives in `tisvlay-ssdp`'s `SharedSearcher`: a single background task owns the socket's `recv`, parses each datagram, and fans it out to all active subscribers over a `broadcast` channel. Each `search(timeout)` subscribes, sends an M-SEARCH burst, and collects replies for its own window.

This is load-bearing for the pinned default port. Concurrent discovery sessions all send the *same* targets and differ only by caller, so the old per-session socket bought nothing but a bind collision: two binds to a fixed `bind_port` would `EADDRINUSE`, and the loser silently returned empty. One shared socket removes the collision (a single bind, ever), keeps exactly one firewall port open, and — because a lone reader owns `recv` — stops concurrent searches from stealing each other's datagrams. If the pinned port is already held by another process at first bind, `SharedSearcher` falls back to an ephemeral port once and logs the degradation.

Bursts are *not* coalesced — each `search` still sends its own M-SEARCH. Only the socket is shared, not the M-SEARCH cadence: with a single reader the extra datagrams are cheap, and it avoids a late-joining subscriber missing replies to a burst it didn't trigger.

Per-protocol scope: this coalescing is within one discovery protocol (SSDP). A future vendor on a different transport (mDNS, a proprietary probe) gets its own socket with its own `bind_port`/firewall knob — it is not a subscriber to this one.

## Deployment-knob CLI pattern

Discovery's bind-port story exposes a pattern worth naming: **deployment-knob CLI flags stay out of the daemon's writable state**. The shape:

- The daemon's writable state file (`/var/lib/tisvlay/daemon.toml`) is read/written by `tisvlay-daemon` itself — pairing persists vendor credentials there. Owned by the daemon, not by any declarative-config tool.
- The CLI flag (`tisvlay service daemon --ssdp-port <PORT>`) overrides the built-in default (1901) — so an unset port still resolves to a known, firewall-targetable value. The NixOS module's `bindPort` option (itself defaulting to 1901) translates to an `--ssdp-port` arg on `ExecStart`. The state file stays fully daemon-owned; declarative deployment still controls the firewall-shaped knob.

`services.tisvlay.openFirewall = true` then adds the port to `networking.firewall.allowedUDPPorts`. The two options are asserted together — `openFirewall` without `bindPort` is rejected at NixOS evaluation time.

This pattern generalizes: future deployment-time-only knobs (e.g. interface bindings, allowed subnets if we ever add them back) should prefer CLI flags over `settings` rendering for the same reason.

## Schema-form trust signal

Both autofill paths (SSDP-discovered and manual-entry MAC enrichment) flow through the same `autofill_schema` engine, which sets a per-row `autofilled: bool` on the resulting `PairingFieldRow`. `SchemaForm` renders a small "Auto-detected" badge beside the field label when the row is autofilled; the badge clears the moment the user edits the value (`on_field_value_edited` flips `autofilled = false` on any edit). Applies uniformly so users can tell at a glance what the system populated vs what they typed. Future autofill paths (pair-time MAC capture, vendor-cloud HTTP probes, EDID-derived hardware IDs) inherit the trust signal automatically by setting `AutofilledRow.autofilled = true`.

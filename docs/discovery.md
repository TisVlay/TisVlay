# Discovery

`tisvlay` ships its own SSDP transport rather than depending on third-party crates or delegating to a Linux system service. The discovery story has three RPC entry points (`DiscoverTvs`, `StartDiscovery`/`StopDiscovery`, `EnrichManualEntry`) sharing one vendor-neutral transport crate and one vendor-owned active-probe hook.

This doc covers the architectural decisions worth re-reading. For firewall-rule recipes that pair with the bind-port story see [docs/firewall.md](firewall.md). For deferred follow-up work see [docs/ideas/ssdp-notify-passive-listener.md](ideas/ssdp-notify-passive-listener.md) (passive NOTIFY listener), [docs/ideas/ipv6-discovery-and-enrichment.md](ideas/ipv6-discovery-and-enrichment.md) (IPv6 support), and [docs/ideas/pair-time-mac-capture.md](ideas/pair-time-mac-capture.md) (MAC capture at pair-success time).

## Why we own the SSDP transport

The workspace ships `tisvlay-ssdp` (~250 LOC incl. unit tests, `publish = true`, vendor-neutral) exposing a single `search(targets, timeout, options) -> Stream<SsdpResponse>` with a configurable `bind_addr`. Three forces pushed us off the third-party-crate path:

- **Firewall-friendly source-port pinning.** Third-party crates like [`ssdp-client`](https://crates.io/crates/ssdp-client) always bind an ephemeral source port. Strict firewalls (NixOS by default, hardened workstations, container/VM hosts) can't write a tight `--dport` rule against an ephemeral range; they fall back to "accept any UDP from subnet," which is broader than necessary. Owning the crate lets us expose `bind_addr` directly.
- **No live Linux system service to delegate to.** KDE shipped **Cagibi** in KDE SC 4.5 (2010), a system-bus SSDP cache/proxy daemon that was exactly the right shape — but it was dropped during the KF5/Plasma 5 transition (now in [KDE's "unmaintained" group](https://invent.kde.org/unmaintained/cagibi), last commit 2017). GNOME's [dleyna](https://gitlab.gnome.org/World/dLeyna) is alive but its Manager filters at the C-source level against `urn:schemas-upnp-org:device:MediaServer:` / `MediaRenderer:1` — no API surface for vendor-specific URNs like `urn:lge-com:service:webostv:1`. Also not default-installed on Ubuntu/Arch/NixOS/Fedora. And the firewall problem persists regardless — a system daemon doing the multicast hits the same stateful-firewall response-drop.
- **Forking `ssdp-client` doesn't solve the publish story.** Upstream PR [#13](https://github.com/jakobhellermann/ssdp-client/pull/13) attempted to add `bind_addr` but bundled it with IPv6 and quality issues; the maintainer hasn't engaged with it in over a year. Forking via `[patch.crates-io]` works for our distro builds but `cargo install` users would get unpatched code. Our usage surface was small enough (~4 call sites, one search function, four response accessors) that owning ~250 LOC outright is less long-term burden than maintaining a fork.

## Trait boundary

The `TvDiscovery::discover(timeout) -> Vec<DiscoveredTv>` trait in [`crates/tisvlay-core/src/traits.rs`](../crates/tisvlay-core/src/traits.rs) is the per-vendor discovery contract — **a bounded scan that returns a list**. Streaming (the wizard's "walk to the TV, walk back" UX) lives one layer up in the daemon, orchestrating bounded scans over time. This split keeps the vendor trait simple (no Stream return type, no session lifecycle), and means a future vendor backend that does discovery via a non-streamable mechanism (vendor cloud API, mDNS one-shot, etc.) can implement `TvDiscovery::discover` without confronting streaming machinery.

LG's discovery layer in [`crates/tisvlay-lg/src/discovery.rs`](../crates/tisvlay-lg/src/discovery.rs) calls `tisvlay_ssdp::search` with all three LG-specific search targets on a single shared socket (one timeout window), then applies LG-specific predicates (`is_lg_tv` against DLNA header / server string / USN) and parsing (DLNA name → model, USN → UUID, EDID PNPID `GSM`).

## Streaming discovery (`Manager1.StartDiscovery`)

The streaming RPC pair was designed to honor an existing wire contract — `DiscoveryFound` was already declared as a signal, but the daemon emitted it in a burst right before `DiscoverTvs` returned. The new methods make the daemon honor the signal over time:

- `StartDiscovery(timeout_ms) → session_id`: spawns a daemon-side task that loops M-SEARCH bursts at the configured `poll_interval` (default 8 s, configurable via `[discovery] poll_interval_ms`). `timeout_ms = 0` runs until `StopDiscovery` or the 5-minute server-side hard cap (backstop against a crashed GUI client). Returns a UUID session id.
- `StopDiscovery(session_id)`: idempotent — unknown session id is not an error.
- `DiscoveryFound` signal: unchanged wire shape (`(id, name, address, hardware_id, vendor, vendor_data)`); emitted per newly-seen TV during the session lifetime. Daemon-side dedup is a per-session `HashSet<(vendor, id)>` so the same TV doesn't re-announce on every poll cycle.
- `DiscoverTvs` stays unchanged for `tisvlay list` and scripted callers — bounded scan, returns the full list, no session.

Per-session task state lives in `ManagerService::discovery_sessions: Arc<Mutex<HashMap<String, Arc<Notify>>>>` so `StopDiscovery` can wake the loop via `notify_one()`. The task always removes its own session-map entry on exit (cancel, timeout, hard cap), so no map leaks.

## Manual-entry MAC enrichment

`Manager1.EnrichManualEntry(vendor, address) → a{ss}` autofills the pairing wizard's manual-entry path the same way SSDP-discovered entries are autofilled. Symmetric in return shape with `DiscoveryFound`'s `vendor_data` column, so the wizard's `autofill_schema` engine handles both paths uniformly. The chain:

1. **IPv4-only correctness gate.** IPv6 (including `::ffff:x.x.x.x`) returns an empty map — today's `arp_lookup` and `probe_arp_cache` only handle v4. The gate widens to IPv4|IPv6 once those grow v6 support; see [docs/ideas/ipv6-discovery-and-enrichment.md](ideas/ipv6-discovery-and-enrichment.md).
2. **Passive netlink read.** Free; hits for users who've previously pinged or browsed the TV (any prior IP traffic primes the ARP cache as a side effect).
3. **Vendor-supplied active probe** (`TvConnector::probe_arp_cache` — see next subsection). Connector decides the mechanism (LG sends one UDP byte to the SSAP wss port); the kernel resolves ARP as a side effect of `send_to`.
4. **Poll netlink with deadline** — 30 ms intervals, 500 ms budget. Outer `tokio::time::timeout` caps the whole chain at 600 ms regardless.

Always returns `Ok(...)`. Empty map means "nothing to autofill" — invalid IP, IPv6, unknown vendor, or the ARP chain missed. The wizard treats empty as a soft outcome (field stays blank with a manual-entry-specific helper) rather than a hard failure. A follow-up at [docs/ideas/pair-time-mac-capture.md](ideas/pair-time-mac-capture.md) covers the belt-and-suspenders path: snoop ARP at successful-pair time, write the MAC back to the saved config, so users who skipped the field at form time still get WoL after first pair.

## Connector probe hook

`TvConnector::probe_arp_cache(&self, ip: IpAddr) -> Result<()>` is the daemon's vendor-owned hook for priming the kernel ARP cache when passive lookup misses. Default no-op; vendor crates that talk to IP-addressed devices override to send a single transport-layer datagram or open a TCP connection that triggers kernel neighbour resolution on the way out. The trait method is family-agnostic (`IpAddr`, not `Ipv4Addr`) so an IPv6 follow-up doesn't reshape the trait.

LG's override sends a single UDP byte to the SSAP wss port (3001), bound to `0.0.0.0:0` — the kernel routing table picks the correct outbound NIC + source IP for the destination, which is the right behavior on typical multi-NIC hosts. The TV's UDP stack discards the unexpected byte silently (no TLS handshake side-effect, no inbound port needed, no `CAP_NET_RAW`). Explicit source-IP binding via netlink route lookup would only matter for VPN-policy-routing edge cases that aren't in scope today; filed as deferred work if anyone hits it.

## Deployment-knob CLI pattern

Discovery's bind-port story exposes a pattern worth naming: **deployment-knob CLI flags override config when declarative wrappers can't accept a writable file**. The shape:

- Daemon's writable config file (`/var/lib/tisvlay/config.toml`) is read/written by `tisvlayd` itself — the pairing wizard persists vendor credentials here. Owned by the daemon, not by any declarative-config tool.
- A NixOS user who wants to manage the bind port declaratively via the NixOS module ([`packaging/nix/module.nix`](../packaging/nix/module.nix)) can't put it in `services.tisvlay.settings` without making the whole config file read-only — which breaks the pairing wizard's write path.
- The CLI flag (`tisvlayd --bind-port <PORT>`) overrides `[discovery] bind_port` from the config. NixOS module's `bindPort` option translates to a `--bind-port` arg on `ExecStart`. Config file stays writable; declarative deployment still controls the firewall-shaped knob.

`services.tisvlay.openFirewall = true` then adds the port to `networking.firewall.allowedUDPPorts`. The two options are asserted together — `openFirewall` without `bindPort` is rejected at NixOS evaluation time.

This pattern generalizes: future deployment-time-only knobs (e.g. interface bindings, allowed subnets if we ever add them back) should prefer CLI flags over `settings` rendering for the same reason.

## Schema-form trust signal

Both autofill paths (SSDP-discovered and manual-entry MAC enrichment) flow through the same `autofill_schema` engine, which sets a per-row `autofilled: bool` on the resulting `PairingFieldRow`. `SchemaForm` renders a small "Auto-detected" badge beside the field label when the row is autofilled; the badge clears the moment the user edits the value (`on_field_value_edited` flips `autofilled = false` on any edit). Applies uniformly so users can tell at a glance what the system populated vs what they typed. Future autofill paths (pair-time MAC capture, vendor-cloud HTTP probes, EDID-derived hardware IDs) inherit the trust signal automatically by setting `AutofilledRow.autofilled = true`.

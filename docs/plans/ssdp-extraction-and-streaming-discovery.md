# SSDP extraction + streaming discovery

Spin SSDP transport out of `ssdp-client` into a new vendor-neutral `tisvlay-ssdp` crate with a configurable bind port (firewall-friendly), refactor `tisvlay-lg` discovery onto it, and convert the pairing wizard's discovery flow from a 5-second one-shot to a daemon-driven streaming session so users can walk over to a powered-off TV, turn it on, and walk back without racing a timeout.

## Status

**Implementation complete on `feat/gui-tweaks-2`; pending merge + first publish of `tisvlay-ssdp` 0.1.0 to crates.io.**

Planned commits (all landed):

1. ✅ `feat(ssdp): extract tisvlay-ssdp crate; migrate LG discovery off ssdp-client` — new workspace crate (~250 LOC incl. unit tests), LG refactor uses single shared M-SEARCH burst, `ssdp-client` dep dropped from workspace.
2. ✅ `feat(ssdp): configurable bind port for firewall-friendly discovery` — `[discovery] bind_port` plumbed through `DaemonConfig` → `LgDiscovery` → `tisvlay_ssdp::search`; [docs/firewall.md](../firewall.md) rewritten with the new config recipe and reframed Linux-system-service section. **Field-verified** by the project owner with a tight `--dport`-only NixOS rule.
3. ✅ `feat(discovery): streaming Manager1.StartDiscovery + wizard integration` — new daemon-side session task with `(vendor, id)` HashSet dedup; wizard switched to streaming with `WizardDiscoveryCancel` oneshot sidecar, cleanup hooked at cancel / step 1→2 / manual-add / on-finish. `poll_interval` default 8 s, hard cap 5 min.

Emergent follow-ons (rode along with the same PR; not in original scope):

4. ✅ `style(gui): stabilize StepDiscover layout to remove first-TV-found jump` — merged the empty / populated branches under a single fixed-min-height region so the wizard's height stays constant as TVs arrive (previously the manual-entry panel was pushed off-screen at min window sizes).
5. ✅ `style(gui): unify wizard StepDiscover ScanLine across empty/populated` — single ScanLine at the bottom of the region rather than two state-specific ones with different sizes.
6. ✅ `feat(nix): bindPort + openFirewall via new tisvlayd --bind-port CLI flag` — declarative NixOS knob for the bind port that doesn't trade away config-file writability. The CLI flag is useful outside Nix too (container images, ad-hoc dev runs).
7. ✅ `style(gui): size StepDiscover list to 2.5 DiscoveryRow heights for scroll hint` — 140 px min-height so a half-clipped third row signals "scroll me" when 3+ TVs are found.

Pre-merge checklist:

- [ ] `nixos-rebuild switch` smoke test with `services.tisvlay = { bindPort = 1901; openFirewall = true; }` — confirm `systemctl cat tisvlayd` shows `--bind-port 1901` and the firewall rule is in place.
- [ ] Visual eyeball of StepDiscover at 0 / 1 / 2 / 3+ TVs with the wizard at minimum window size to confirm the 2.5-row scroll hint reads as intended.
- [ ] `cargo publish -p tisvlay-ssdp` from a clean tree (first publish — needs crates.io account verification, doc gen check).

## Motivation

Two problems compound each other in today's wizard:

1. **Firewall fragility.** `ssdp-client` 2.1.0 binds to an OS-assigned ephemeral port and offers no knob to override. Strict firewalls (NixOS by default, hardened workstations, container/VM hosts) need a `-s <subnet>` rule that accepts *any* UDP from the subnet — overly broad. A pinned bind port lets users open a single-port rule. Upstream PR [#13](https://github.com/jakobhellermann/ssdp-client/pull/13) attempts this but has been stalled since April 2025 with quality issues blocking merge. See [docs/firewall.md](../firewall.md).
2. **One-shot races user behavior.** `Manager1.DiscoverTvs` runs for a fixed 5s window. The wizard's natural use case — open wizard, walk over to TV, power it on, walk back — exceeds that window. Today the user has to either pre-power the TV (defeating the point) or hit "Rescan" repeatedly.

The D-Bus surface already half-supports streaming: `Manager1.DiscoveryFound` is declared as a signal and [the client subscribes to it via `discovery_found_stream()`](../../crates/tisvlay-gui/src/wizard.rs#L117-L128). The daemon currently emits the signals in a burst right before `discover_tvs` returns rather than over time, so this is a behavior change inside an existing wire contract rather than a redesign.

## Scope

In:

1. New workspace crate `tisvlay-ssdp` — vendor-neutral SSDP `M-SEARCH` transport with `bind_addr: Option<SocketAddr>`, published to crates.io.
2. Refactor [crates/tisvlay-lg/src/discovery.rs](../../crates/tisvlay-lg/src/discovery.rs) to call `tisvlay_ssdp::search()` directly; remove `ssdp-client` from the workspace.
3. Wire a `[discovery] bind_port` setting through `daemon.toml`; rewrite [docs/firewall.md](../firewall.md) — replace the "Potential Upstream Improvement" section with a concrete config recipe + tighter `--dport` iptables rule, and reframe "Why this is a Linux-specific problem" to acknowledge that the firewall issue is intrinsic to SSDP on Linux regardless of who runs the multicast (see the new "Delegate to a system service" alternative below for the historical context this section should capture).
4. New `Manager1.StartDiscovery` / `Manager1.StopDiscovery` D-Bus methods with session-id dedup and a server-side max-duration backstop; daemon spawns a task that loops `M-SEARCH` bursts and emits the existing `DiscoveryFound` signal incrementally.
5. Switch [crates/tisvlay-gui/src/wizard.rs](../../crates/tisvlay-gui/src/wizard.rs) to start a session on wizard open, drain `DiscoveryFound` until close, stop on close / selection / cancel.

Out:

- **Passive `NOTIFY` listener** for `ssdp:alive` / `ssdp:byebye` events. Filed as [docs/ideas/ssdp-notify-passive-listener.md](../ideas/ssdp-notify-passive-listener.md) — the polling design is intentionally a foundation it can layer onto without API churn.
- **mDNS / DNS-SD fallback discovery.** Already filed under [docs/firewall.md#future-alternative-mdnsdns-sd-discovery](../firewall.md#future-alternative-mdnsdns-sd-discovery); not blocked by anything here and would be a sibling `tisvlay-mdns` crate.
- **Sony / Samsung backends.** This PR makes future vendor crates easier by sharing the SSDP transport, but no new backend ships here.
- **Reactive coalesced discovery** (registry-side dispatcher that runs one `M-SEARCH` for all vendors and routes responses by predicate). Worth considering only once we have 3+ SSDP-using backends — see [Alternatives](#alternatives) below.

## Design

### `tisvlay-ssdp` crate

```rust
// crates/tisvlay-ssdp/src/lib.rs
pub struct SsdpResponse {
    // location, usn, server, extra_headers — same accessors as today's
    // tisvlay-lg call sites use.
}

pub struct SearchOptions {
    pub bind_addr: Option<SocketAddr>,  // None = ephemeral (today's behavior)
    pub mx: u8,                          // default 2
    pub ttl: Option<u32>,                // default 2
}

pub async fn search(
    targets: &[&str],
    timeout: Duration,
    opts: SearchOptions,
) -> Result<impl Stream<Item = Result<SsdpResponse>>>;
```

Targets are `&[&str]` rather than a typed `SearchTarget` enum — the 187-line `SearchTarget` enum in upstream is mostly a `Display` helper, and vendor crates know their own strings as constants. Dropping the enum trims the surface by ~half.

Crate is `publish = true` from day one. README + LICENSE-MIT/APACHE + CHANGELOG mandatory before first publish. Semver discipline: anything in `pub` is part of the contract.

### LG refactor

[crates/tisvlay-lg/src/discovery.rs](../../crates/tisvlay-lg/src/discovery.rs) shrinks to ~30 lines of orchestration plus the LG-specific predicate/parser, all of which stays:

- 3 LG `SearchTarget` strings (`lge-com:service:webostv:1`, `MediaRenderer:1`, `RootDevice`).
- `is_lg_tv()` predicate (DLNA header, server-string sniff, USN sniff).
- `parse_response()` (UUID extraction, IP from URL, DLNA name + model extraction, `GSM` PNPID).
- Dedup-by-IP-prefer-DLNA-name logic.

`impl TvDiscovery for LgDiscovery` keeps the same `discover(timeout) -> Vec<DiscoveredTv>` contract — the trait at the registry boundary is already the right abstraction and doesn't change.

Workspace `Cargo.toml` drops `ssdp-client = "2"`.

### Configurable bind port

New `daemon.toml` section:

```toml
[discovery]
# Pin the SSDP client's source UDP port so a firewall rule can target it.
# Omit (or set to 0) for OS-assigned ephemeral (today's behavior).
bind_port = 1901
```

Plumbed through `tisvlay-core::config::DaemonConfig` → `TvManager` → `LgDiscovery::new(opts)` → `tisvlay_ssdp::search(opts)`. Optional everywhere; default is ephemeral so unchanged behavior for users who don't opt in.

Update [docs/firewall.md](../firewall.md) "Potential Upstream Improvement" section: replace with concrete recipe pointing at the new config and the tighter iptables rule.

### Streaming discovery D-Bus surface

```
Manager1.StartDiscovery(timeout_ms: u32) -> (session_id: String)
  - timeout_ms = 0 means "until StopDiscovery or hard cap"
  - returns a session_id the caller passes to StopDiscovery

Manager1.StopDiscovery(session_id: String) -> ()
  - idempotent; unknown session_id is not an error

Manager1.DiscoveryFound  (existing signal, semantics unchanged on wire)
  - now emitted incrementally over the session lifetime, deduped by
    (vendor, hardware-identity) within a single session so the same TV
    isn't re-announced on every poll cycle
```

Daemon-side mechanics:

- One `DiscoverySession` task per active session_id, holding a `HashSet` of seen TVs (keyed by `(vendor, usn-or-equivalent)`).
- Loop: `M-SEARCH` burst → wait `poll_interval` → repeat. Default `poll_interval = 8s`, configurable as `[discovery] poll_interval_ms`.
- Hard server-side cap: 5 minutes regardless of `timeout_ms`. Backstop against a crashed wizard leaking a forever-loop.
- `DiscoverTvs(timeout)` (existing) stays unchanged — `tisvlay list` and scripted users want bounded scans with one return value. Internally it can be a thin wrapper over a short `StartDiscovery` + collect, but the surface stays.

### Wizard UX

[crates/tisvlay-gui/src/wizard.rs](../../crates/tisvlay-gui/src/wizard.rs)'s `run_wizard_discovery` becomes:

- On wizard open: `StartDiscovery(0)`, subscribe to `DiscoveryFound`, append rows as they arrive.
- On wizard close / TV selected / Cancel: `StopDiscovery(session_id)`.
- Discovery-error becomes a soft banner (auto-clears on next found TV) rather than a terminal state.
- Spinner shows a running count: "Scanning… 3 TVs found".
- Manual "Rescan" button still useful for an immediate burst even mid-session.

The `WizardGen` cancel-safety pattern at [.claude/CLAUDE.md:148-152](../../.claude/CLAUDE.md#L148-L152) extends naturally: each `StartDiscovery` captures a generation, and a stale `DiscoveryFound` callback (after wizard reset) no-ops.

## Sequencing

One PR, three commits. Commit boundaries follow the "scaffolding + first usage in one commit" principle:

1. `feat(ssdp): extract tisvlay-ssdp crate; migrate LG discovery off ssdp-client`
   New crate + LG refactor + workspace dep removal in one commit so we never have a "ssdp crate exists but nothing uses it" state.
2. `feat(ssdp): configurable bind port for firewall-friendly discovery`
   Config plumbing + [docs/firewall.md](../firewall.md) rewrite: replace "Potential Upstream Improvement" with the new config recipe, and revise "Why this is a Linux-specific problem" to drop the implicit "missing system service is the problem" framing in favor of "the firewall issue is intrinsic to SSDP on Linux." Preserve Cagibi (2010-2017) and dleyna (DLNA-scoped) as the historical answer to "why doesn't a system service exist?" so future contributors don't re-ask.
3. `feat(discovery): streaming Manager1.StartDiscovery + wizard integration`
   Daemon D-Bus methods + dedup + GUI wizard switch in one commit so the new D-Bus surface ships with its consumer.

Pre-publish prerequisites for the first commit landing on `main`: `tisvlay-ssdp` 0.1.0 must be on crates.io. Sequence on merge day: tag + publish `tisvlay-ssdp` from a clean tree → merge this PR → bump downstream crate versions.

## Alternatives considered

- **Delegate to a system UPnP/SSDP service** (analogous to Windows' Function Discovery). No live, suitable-coverage option exists on Linux. KDE shipped **Cagibi** in KDE SC 4.5 (2010) — a system-bus SSDP cache/proxy daemon, *exactly* the shape we'd want — backing `kio_network` and `kio-upnp-ms`. It was dropped during the KF5/Plasma 5 transition and now lives at `invent.kde.org/unmaintained/cagibi` (last commit 2017). GNOME's **dleyna** ([gitlab.gnome.org/World/dLeyna](https://gitlab.gnome.org/World/dLeyna)) is alive (0.8.3, Sept 2023; one maintainer, ~one merge/year) but its Manager filters discovered devices at the C-source level against `urn:schemas-upnp-org:device:MediaServer:` / `MediaRenderer:1` — no API to ask for `urn:lge-com:service:webostv:1`. LG TVs *do* advertise `MediaRenderer:1`, so `dleyna-renderer` would surface them — but only with the MPRIS transport surface, not the SSAP control channel we need. Also not default-installed on Ubuntu/Arch/NixOS/Fedora, so we'd be forcing a transitive runtime dep on every user. **And the firewall problem persists regardless** — a system daemon doing the multicast hits the same stateful-firewall response-drop from a different process. The abandonment of Cagibi suggests there isn't community momentum toward a replacement either; nothing in `xdg-desktop-portal` or `systemd-resolved` covers SSDP today.
- **Fork `ssdp-client`** instead of writing our own. Forking via `[patch.crates-io]` with a git URL works for our distro builds but `cargo install tisvlay-*` users would get unpatched code (crates.io rejects git deps in published manifests). Renaming-the-fork and publishing it under a tisvlay-namespaced name works but is fork pollution. Our usage surface is small enough (~4 call sites, one search function, four response accessors) that a focused 150-200 line crate is less long-term burden than a maintained fork.
- **Reactive `VendorDiscovery` trait** that lets the registry coalesce all SSDP-using vendors into one `M-SEARCH` burst and dispatch responses by predicate. Tempting but premature with 1 live vendor and an `M-SEARCH` cost of one UDP datagram per vendor per poll cycle. The trait at `TvDiscovery::discover(timeout) -> Vec<DiscoveredTv>` is the right boundary today; coalescing is a future registry-internal optimization that wouldn't change the public surface.
- **Wait for upstream PR #13** to merge in `ssdp-client`. The maintainer (`jakobhellermann`) shipped 2.1.0 in April 2025 but hasn't engaged with #13 in 13 months. Even if it merged tomorrow, it bundles IPv6 + bind-addr in one diff with quality issues (typos, `?` replaced with `.expect(…)`) that would likely need a rewrite first. Not a credible unblock path.
- **Passive `NOTIFY` listener** as part of this PR. Filed as a follow-up — see [Out](#scope) and the linked idea doc. Polling-first is the right scope; the listener layers on cleanly later.

## Open questions

- **Resolved (shipped at 8 s):** default `poll_interval` is `DiscoveryConfig::poll_interval()` returning 8 s when `poll_interval_ms` is unset. Tune via config if field feedback says it feels sluggish or noisy.
- **Resolved (shipped at 5 min):** hard server-side session cap is `STREAMING_DISCOVERY_HARD_CAP = 5 * 60 s`. Revisit only if a real user reports their wizard session being cut short.
- **Still deferred:** `Manager1.StartDiscovery` does not accept a `vendor_filter: Vec<String>`. Additive; can be added without breaking the v1 signature when a second SSDP-using vendor arrives and a "search only X" UI path is justified.
- **Emerged during NixOS work:** the `--bind-port` CLI flag overrides `[discovery] bind_port` from the config file. Future deployment knobs (e.g. `--poll-interval-ms`) could follow the same pattern, but none are wired today — add per-flag on demand to avoid speculative CLI bloat.

## Migration on merge

Per [docs/plans/README.md](README.md):

- Architecture/decisions (crate layout, why we own SSDP, trait boundary, the `--bind-port`-overrides-config CLI/Nix pattern) → fold into [docs/design.md](../design.md) as a "Discovery" subsection.
- Firewall recipe + "Why this is a Linux-specific problem" reframe → already in [docs/firewall.md](../firewall.md) by design as part of commit 2; no further migration needed. *Possible add:* a one-line cross-reference to the NixOS module's `bindPort`/`openFirewall` options now that they exist as the declarative wrapper.
- Deferred follow-up → [docs/ideas/ssdp-notify-passive-listener.md](../ideas/ssdp-notify-passive-listener.md) already filed; cross-link from `docs/design.md`'s Discovery subsection.
- GUI follow-ons (commits 4, 5, 7) are pure layout polish — no architectural content to migrate. Git history is the archive.
- Delete this plan file. `grep -rn 'docs/plans/ssdp-extraction-and-streaming-discovery'` to find and re-point any inbound refs.

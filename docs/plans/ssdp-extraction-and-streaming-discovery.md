# SSDP extraction + streaming discovery

Spin SSDP transport out of `ssdp-client` into a new vendor-neutral `tisvlay-ssdp` crate with a configurable bind port (firewall-friendly), refactor `tisvlay-lg` discovery onto it, and convert the pairing wizard's discovery flow from a 5-second one-shot to a daemon-driven streaming session so users can walk over to a powered-off TV, turn it on, and walk back without racing a timeout.

## Motivation

Two problems compound each other in today's wizard:

1. **Firewall fragility.** `ssdp-client` 2.1.0 binds to an OS-assigned ephemeral port and offers no knob to override. Strict firewalls (NixOS by default, hardened workstations, container/VM hosts) need a `-s <subnet>` rule that accepts *any* UDP from the subnet — overly broad. A pinned bind port lets users open a single-port rule. Upstream PR [#13](https://github.com/jakobhellermann/ssdp-client/pull/13) attempts this but has been stalled since April 2025 with quality issues blocking merge. See [docs/firewall.md](../firewall.md).
2. **One-shot races user behavior.** `Manager1.DiscoverTvs` runs for a fixed 5s window. The wizard's natural use case — open wizard, walk over to TV, power it on, walk back — exceeds that window. Today the user has to either pre-power the TV (defeating the point) or hit "Rescan" repeatedly.

The D-Bus surface already half-supports streaming: `Manager1.DiscoveryFound` is declared as a signal and [the client subscribes to it via `discovery_found_stream()`](../../crates/tisvlay-gui/src/wizard.rs#L117-L128). The daemon currently emits the signals in a burst right before `discover_tvs` returns rather than over time, so this is a behavior change inside an existing wire contract rather than a redesign.

## Scope

In:

1. New workspace crate `tisvlay-ssdp` — vendor-neutral SSDP `M-SEARCH` transport with `bind_addr: Option<SocketAddr>`, published to crates.io.
2. Refactor [crates/tisvlay-lg/src/discovery.rs](../../crates/tisvlay-lg/src/discovery.rs) to call `tisvlay_ssdp::search()` directly; remove `ssdp-client` from the workspace.
3. Wire a `[discovery] bind_port` setting through `daemon.toml`; update [docs/firewall.md](../firewall.md) to recommend the tighter `--dport` rule.
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
   Config plumbing + [docs/firewall.md](../firewall.md) recipe rewrite.
3. `feat(discovery): streaming Manager1.StartDiscovery + wizard integration`
   Daemon D-Bus methods + dedup + GUI wizard switch in one commit so the new D-Bus surface ships with its consumer.

Pre-publish prerequisites for the first commit landing on `main`: `tisvlay-ssdp` 0.1.0 must be on crates.io. Sequence on merge day: tag + publish `tisvlay-ssdp` from a clean tree → merge this PR → bump downstream crate versions.

## Alternatives considered

- **Fork `ssdp-client`** instead of writing our own. Forking via `[patch.crates-io]` with a git URL works for our distro builds but `cargo install tisvlay-*` users would get unpatched code (crates.io rejects git deps in published manifests). Renaming-the-fork and publishing it under a tisvlay-namespaced name works but is fork pollution. Our usage surface is small enough (~4 call sites, one search function, four response accessors) that a focused 150-200 line crate is less long-term burden than a maintained fork.
- **Reactive `VendorDiscovery` trait** that lets the registry coalesce all SSDP-using vendors into one `M-SEARCH` burst and dispatch responses by predicate. Tempting but premature with 1 live vendor and an `M-SEARCH` cost of one UDP datagram per vendor per poll cycle. The trait at `TvDiscovery::discover(timeout) -> Vec<DiscoveredTv>` is the right boundary today; coalescing is a future registry-internal optimization that wouldn't change the public surface.
- **Wait for upstream PR #13** to merge in `ssdp-client`. The maintainer (`jakobhellermann`) shipped 2.1.0 in April 2025 but hasn't engaged with #13 in 13 months. Even if it merged tomorrow, it bundles IPv6 + bind-addr in one diff with quality issues (typos, `?` replaced with `.expect(…)`) that would likely need a rewrite first. Not a credible unblock path.
- **Passive `NOTIFY` listener** as part of this PR. Filed as a follow-up — see [Out](#scope) and the linked idea doc. Polling-first is the right scope; the listener layers on cleanly later.

## Open questions

- Default `poll_interval` — 8s vs 5s vs 10s? Bias toward 8s as the first guess; tune after field-testing the wizard UX.
- Should `Manager1.StartDiscovery` accept a `vendor_filter: Vec<String>` so a future "look only for Sony TVs" UI path is reachable? Could add later additively without breaking the v1 signature. Defer.
- Hard server-side session cap — 5 minutes or 10? Pick whatever balances "user goes to make coffee" against "wedged session never cleans up." 5 minutes feels right; revisit if anyone reports timeouts.

## Migration on merge

Per [docs/plans/README.md](README.md):

- Architecture/decisions (crate layout, why we own SSDP, trait boundary) → fold into [docs/design.md](../design.md) as a "Discovery" subsection.
- Firewall recipe → already lives in [docs/firewall.md](../firewall.md) by design; just sweep that file's "Potential Upstream Improvement" section into a "Configuration" section.
- Deferred follow-up → [docs/ideas/ssdp-notify-passive-listener.md](../ideas/ssdp-notify-passive-listener.md) already filed; cross-link from `docs/design.md`'s Discovery subsection.
- Delete this plan file. `grep -rn 'docs/plans/ssdp-extraction-and-streaming-discovery'` to find and re-point any inbound refs.

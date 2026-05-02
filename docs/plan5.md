# Plan 5 — Power-cycle robustness + MAC capture in pairing

> **Status (drafting):** This plan addresses a concrete bug
> hit during dev: the office TV was paired via the GUI's SSDP
> discovery flow, which captured IP but not MAC. Subsequent
> `Power off` worked; `Power on` then silently no-op'd
> (`WakeNotSupported`), and the daemon then refused to
> auto-reconnect when the TV was woken manually
> (`powered_off=true` gate). This plan fixes that and
> related rough edges in the power-cycle UX.
>
> Open questions are flagged inline as **❓**. Answer them
> before execution.

## Motivation

Concrete sequence that surfaced the bugs (29 Apr 2026):

1. Office TV paired via GUI → SSDP-discovered, picked from
   list, `Pair` clicked, success.
2. Resulting `[tv.lg]` block in `daemon.toml`:
   ```toml
   [tv.lg]
   client_key = "..."
   ip = "192.168.1.170"
   # no mac
   ```
3. User clicks `Power off` — works (TV off, daemon sets
   `powered_off=true`).
4. User clicks `Power on` six times. Daemon receives all six
   D-Bus calls, logs `D-Bus: PowerOn`, then silence —
   `power_on()` returns `WakeNotSupported` immediately at
   `tv_manager.rs:1211` because `LgBackend::supports_wake()`
   returns `false` when `mac.is_none()`. No `warn!`, no
   `PowerCyclePhase::Failed` emit, just an Err propagated
   to the GUI which logs it at `warn!` (invisible to the
   user) and clears the spinner.
5. User turns TV on with the remote. Daemon's auto-reconnect
   paths
   ([tv_manager.rs:513](../crates/tisvlayd/src/tv_manager.rs#L513),
   [1092](../crates/tisvlayd/src/tv_manager.rs#L1092),
   [1142](../crates/tisvlayd/src/tv_manager.rs#L1142))
   bail out because `powered_off=true`. TV stays disconnected.

The only ways out today: edit `daemon.toml` to add MAC + reload,
or restart the daemon (`powered_off` resets to `false` on TV
load).

## Scope

Four items, ordered by leverage:

| # | Item | Effort |
|---|---|---|
| 1 | Capture MAC during SSDP pairing | medium |
| 2 | Daemon: emit `Failed` + log on `WakeNotSupported` | small |
| 3 | GUI: surface daemon errors visibly to the user | small-medium |
| 4 | Recovery path for already-paired TVs lacking MAC | small-medium |

Items 1 + 4 together cover both new pairings and the existing
no-MAC office TV. Item 2 fixes the silent-fail. Item 3 makes
similar errors visible going forward.

## Phase 1 — Capture MAC during SSDP pairing

### Why

SSDP responses don't carry MAC directly (SSDP runs over UDP
multicast and the application-layer payload is just the device
description URL). MAC has to come from a separate channel:

- **ARP table lookup** for the discovered IP. Works on the
  same L2 segment. Fast (kernel-local, no network round-trip
  beyond an initial ARP ping if entry is stale).
- **Device description XML** (the `LOCATION` URL the SSDP
  response points at). Some manufacturers include MAC in the
  XML; LG WebOS does not in our limited testing — we'd have
  to check Samsung / Sony separately.
- **TV's WebSocket handshake**: the LG WebOS pairing handshake
  exchanges metadata that includes a device id, but not MAC.
  Same for vendor protocols generally.

ARP is the universal fallback. It only works when the GUI host
is on the same L2 segment as the TV — typical home setup,
breaks down in segmented VLAN environments. Acceptable for v1.

### Steps

1. **Backend trait surface**: extend `DiscoveredTv` with
   `mac: Option<String>`. The LG `discover` impl populates it
   from an ARP table read after collecting the IP. (Linux: read
   `/proc/net/arp`; or use `nl80211` / `rtnetlink` for proper
   neighbor-table query — the daemon already pulls in
   `netlink-packet-route` for carrier monitoring, so this
   route is consistent.)

2. **D-Bus wire change** on `Manager1.DiscoverTvs` and
   `Manager1.DiscoveryFound`. Current return tuple is
   `(s, s, s, (s,s,s), s)`. Either:
   - **Option A**: add a 6th `s` for MAC. Compact, minor
     shape change.
   - **Option B**: replace the inner `(sss)` hardware-id tuple
     with `(ssss)` adding MAC. Co-locates identifiers.
   - **Option C**: switch to a dict / struct with named fields.
     Future-proof against more discovery fields.

   ❓ **Question 1**: Which option? I'd lean **A** — minimal
   churn for one-field addition. **C** is over-engineering
   unless we expect more discovery fields soon.

3. **Wizard data model**: extend
   `WizardDiscoveredTv` (in `globals/pairing_wizard.slint`)
   with a `mac: string` field. The discovery row layout
   gains an optional inline "MAC: AA:BB:CC:DD:EE:FF" caption
   when present.

4. **Wizard configure step**: add a "MAC address" form row,
   auto-filled from discovery, editable. Helper text:
   "Required for Wake-on-LAN. Auto-filled when discovered;
   leave blank if your TV doesn't support WoL."

   ❓ **Question 2**: Should empty MAC be **blocked** at the
   wizard's Next button (i.e. require MAC), **warned**
   (allow but with a "you won't be able to wake this TV"
   note), or **silently allowed**? My recommendation: warn
   but allow. Some users genuinely have TVs that don't
   support WoL, or want to manage them manually.

5. **Manual-entry path**: the wizard's "Add manually" form
   already has a MAC field per the design handoff. Verify
   it's wired to the daemon. Apply the same warn-on-empty
   policy.

## Phase 2 — Daemon: emit `Failed` + log on `WakeNotSupported`

### Why

Currently `power_on()` returns `Err(WakeNotSupported)` without
logging or emitting a `PowerCyclePhase::Failed`. The GUI sees
the error via D-Bus, logs it at `warn!` (not visible), and
clears the spinner — the user sees "click did nothing."

### Steps

1. In
   [`tv_manager::power_on`](../crates/tisvlayd/src/tv_manager.rs#L1211),
   before returning `WakeNotSupported`:
   - `warn!(tv_id = %tv_id, "PowerOn requested but TV has no
     MAC configured — can't wake via WoL");`
   - `self.emit_power_cycle_phase(tv_id,
     PowerCyclePhase::Failed);` so the GUI's spinner
     transitions to a visible Failed state instead of just
     clearing.

   ❓ **Question 3**: Should `PowerCyclePhase` get a new
   variant `WakeNotSupported` distinct from `Failed`?
   Distinguishes "we tried and the TV didn't wake" from "we
   couldn't even try". My take: stick with `Failed` — the
   GUI surface in plan 3 below carries a separate error
   message anyway, so the phase enum doesn't need to encode
   the cause.

## Phase 3 — GUI: surface daemon errors visibly

### Why

Right now any daemon error path beyond pairing's progress
stream falls into Rust's `match result { Err(e) => warn!(…) }`
and never reaches the user. Power-cycle, set-volume,
set-input, set-managed, etc. all silently fail.

### Steps

1. Add a `LastError` global in `ui/globals/last_error.slint`:
   ```slint
   export global LastError {
       in property <string> message;
       in property <string> kind;     // "power" | "config" | …
       in-out property <bool> visible;
   }
   ```

2. Toast component at the bottom of `AppWindow` (above the
   status footer). Renders when `LastError.visible == true`,
   auto-dismisses after ~5s (`Timer { interval: 5s; }`),
   click-to-dismiss.

3. Rust: `register_callbacks` error paths push into
   `LastError` (via `on_ui` helper) instead of just
   `warn!`-ing. Keep the `warn!` for journald correlation.

   ❓ **Question 4**: Toast position — bottom-of-window
   floating above content (current convention in KDE
   apps), or inline below the pane header? My take:
   bottom-floating, auto-dismiss. Less intrusive.

   ❓ **Question 5**: Scope — surface ALL daemon errors via
   this mechanism, or just the user-initiated ones from
   button clicks? Auto-reconnect failures could spam the
   toast. My take: only user-initiated. Auto-reconnect
   failures stay in journald; toast is for "I clicked a
   thing and it didn't work."

## Phase 4 — Recovery path for already-paired TVs lacking MAC

### Why

Even after Phase 1 ships, the user's existing office TV (and
any other previously-paired TV) still has no MAC. They need
either a way to add MAC post-hoc, or a way to power-on without
WoL when the TV is already reachable.

### Steps — pick one approach (or combine):

**Approach A: "Connect" button in TV detail.** A second action
button next to "Power on/off" that's visible when
`powered_off=true` AND we have a `connected=false` TV. Calls
a new `Tv1.Power.Connect` D-Bus method that does direct TCP
connect (skips WoL) and clears `powered_off` on success. UX
is explicit but adds a button + a method.

**Approach B: Make `power_on` connect-first.** Change the
semantics: try a direct connect; if that fails, fall back to
WoL. If neither works, return `Failed`. Reduces button count.
Side effect: `powered_off` becomes a soft hint, not a gate
(or we keep it gating auto-reconnect but not user-initiated
PowerOn).

**Approach C: "Edit MAC" affordance.** Edit-in-place pencil
icon next to the MAC display in TV detail. Click → inline
input → save → daemon writes the toml. Doesn't help for
ARP-table-impossible deployments but helps everyone else.
Could ship alongside A or B.

❓ **Question 6**: Pick one or combine. My recommendation:
**B + C**. B fixes the silent-trap regardless of MAC; C
gives users a clean way to backfill MAC for WoL. A is
nice but redundant if B is in place.

❓ **Question 7**: When `power_on` succeeds via direct connect
(no WoL), should it still emit `PowerCyclePhase::WolSent`
followed by `Connecting` followed by `Connected`? Or skip
to `Connecting` → `Connected` directly? My take: skip WolSent
so the GUI doesn't claim it sent a WoL it didn't.

## Out of scope

- Discovery for TVs not on the same L2 segment as the GUI
  host (cross-VLAN). Requires either a daemon-side ARP probe
  + multicast routing or vendor-protocol metadata exchange.
  Defer to a future plan.
- Per-vendor MAC-discovery quirks (Samsung / Sony might
  expose MAC in their description XML — worth checking
  during Phase 1 implementation but not promised).
- Background reachability probes (TCP ping every N seconds
  to surface "TV is back" automatically). Mentioned by
  user as a possible direction in the original triage but
  not in this plan's scope.

## Open questions (consolidated)

1. **D-Bus wire format for added MAC field**: A (extra
   trailing `s`), B (extend hardware-id tuple to `(ssss)`),
   or C (switch to dict / struct)?
2. **Wizard policy for empty MAC**: block / warn / allow
   silently?
3. **`PowerCyclePhase::WakeNotSupported` variant**: add
   distinct variant or fold into `Failed`?
4. **Toast position**: bottom-floating or inline?
5. **Toast scope**: all daemon errors or only user-initiated?
6. **Recovery approach**: A (Connect button), B (PowerOn
   connect-first), C (Edit MAC), or a combination?
7. **Phase emit when PowerOn succeeds via direct connect**:
   include WolSent or skip it?

## Dependencies between phases

- Phase 2 is independent and small — could land first as a
  hotfix.
- Phase 3 (toast UI) is a prerequisite for Phase 2 actually
  being user-visible. Can land in either order; Phase 2
  alone improves journald visibility.
- Phase 1 (capture MAC) is independent of 2/3.
- Phase 4 (recovery) builds on Phase 2 (Failed phase)
  and Phase 3 (error toast for the still-failing case).

Sequence I'd suggest absent other input: 2 → 3 → 4B → 4C →
1. Each step makes the next observable / verifiable.

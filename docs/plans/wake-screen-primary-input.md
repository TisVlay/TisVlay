# Wake, screen, and the primary input: reliable actions that land on the right port

Status: **approved** — review round + policy decisions folded (2026-07-05). §1–§3 ready to implement; §4 implementation waits on the C3 WoL field check.

## Problem

Three behaviors that looked separate turn out to be one interconnected design, and the current stack has parity gaps in all three:

1. **Screen elevation regression** (QA, 2026-07-05): `on_session_unlock = screen_on` did nothing for a powered-off LG TV. Root's reliable `screen_on` had a pre-flight wake — a wake-needy or disconnected TV went straight to the power path (WoL + reconnect) instead of erroring `not-connected` — plus a desired-screen latch re-applied after the wake. The port kept the CEC screen-via-power *emulation* and the connected-but-noop verify-escalate tier (`DispatchVerify`), but dropped the disconnected *elevation* entry point and the across-teardown intent carry.
2. **Wake paths land on different inputs.** Empirically verified: **CEC power-on both wakes the panel and switches the TV to the HDMI port that sent the request** (Image View On + Active Source is "show *me*"). Presumed (needs field verification on the C3): **WoL wakes the TV to whatever input it was last on** — which, for a TV shared between the PC and a Chromecast on different ports, may not be the PC.
3. **`primary_input` is only a gate today.** The per-TV `primary_input` exists in config and drives `automate_when` (on_primary/off_primary) — but nothing ever *switches to it*. The concept is exactly "the port TisVlay considers the PC"; automation that wakes the TV for a desktop session should be able to land it there.

The unifying observation: post-wake input restoration is the same shape as root's desired-screen latch — **intent recorded at dispatch time, applied once the connection is live**. The design below deliberately does *not* add a parallel intent mechanism for the screen half: `DispatchVerify` already is the screen-intent latch, and the review round's central finding is that it must remain the only one.

## Design

### 1. Screen elevation (root parity — the regression fix)

In `dispatch_screen`, non-emulating backend with no `ConnectionHandle`:

- `screen_on` + `link.supports_wake` → route to `power_on_tv` (the wake path, which upgrades an in-flight plain retry to a wake dial). For LG, the wake itself lights the panel. **Ack semantics**: `power_on_tv` replies ok when it arms the wake dial, not when the panel lights — an elevated `screen_on` rpc gets the same early ack as `power-on` does today, with progress observable on the power phase keys. The plan does not pretend this path delivers a screen-verified ack; the automation callers pass no reply anyway.
- `screen_off` → reply ok as a satisfied no-op — a dark, disconnected TV already *is* screen-off. Scope note: for `reachable_in_standby` backends (CEC) a standby TV is *connected*, so `screen_off` there routes through the emulating power path and never reaches this arm — the no-op resolution only ever applies to network transports and a truly-unreachable CEC device. *(Resolved in review: no latch — it buys nothing until a backend exists where a disconnected screen-off intent must survive to reconnect.)*

### 2. Pre-flight wake (root parity — latency + the ActiveStandby quirk)

*Connected* TV whose cached `Power` is wake-needy + `screen_on` → skip the verify-retry loop, go straight to the power path. Root's rationale holds: `screen_on` over the existing socket no-ops or stalls against a half-asleep TV, and LG's `ActiveStandby` ACKs commands without ever transitioning.

- **The predicate is root's, exactly**: `needs_wake_preflight()` on core's `PowerState` = `!matches!(Active | ScreenSaver | ScreenOff)`. `ScreenOff` is deliberately **not** wake-needy — the lock-screen wake path leaves the cache at `ScreenOff` and must stay on the cheap verify path (root optimized this case explicitly). `Unknown` (and any future variant, per the `#[non_exhaustive]` fall-through core's `PowerState` docs already prescribe) maps to wake-needy. Core is the right home: `PowerState`'s doc comments already name the wake-preflight set as the classification vendor authors must slot new sub-states into — the method completes a concept core documents but doesn't yet expose, alongside `is_on`/`is_screen_on`.
- **Transport guard at the call site**: for a `reachable_in_standby` backend (CEC), `Unknown` does *not* trigger pre-flight. A CEC TV that ignores power-status queries reads `Unknown` while genuinely on — pre-flight would fire Image View On and its Active Source grab, yanking a TV that was watching the Chromecast over to the PC's port. `Unknown`-means-wake is a network-transport assumption (a network TV that's on has a live socket and a real cached state); the call site combines `power.needs_wake_preflight()` with `TransportTraits`, which is already in `dispatch_screen`'s row.
- **Query widening**: cached power lives in the `payload::Power` component, currently read only by `verify_dispatch` — `dispatch_screen`'s `LinkQuery` row (and the system-event dispatch query) must gain `Option<&payload::Power>`. This is the substantive world-side change, not the predicate.

### 3. One verify mechanism, one new marker

**Division of labor (the invariant):** `DispatchVerify` is intent over a live connection — verify, retry, escalate. `DesiredInput` is the one genuinely new fact — the input to land on once live — and it is the *only* new component (a single-field marker, `DesiredInput(String)`, in the house one-concept-per-component style). There is no `DesiredState { screen, input }` struct: a second screen latch alongside `DispatchVerify` would mean two components that can disagree about `target_on` and double-apply across a wake reconnect.

**Screen intent across a wake teardown**: when a `screen_on` escalates (or elevates) onto the wake path, `DispatchVerify` itself survives the teardown — verification defers while the dial is pending, and the `Connected` arm re-asserts (satisfied by the connect snapshot → done; otherwise re-send and restart the verify clock). One verify path, re-armed, never duplicated. *(Implementation note, settled during the build: the re-arm resets the surviving component directly rather than re-entering `dispatch_screen`, because the component carries an `escalated` flag that must persist across the reconnect — it caps every dispatch at one wake dial, so a TV that keeps ACKing dark, or a wake dial that ultimately fails, ends in an observable `dispatch-failed` instead of ping-ponging wakes forever.)*

**Drain semantics**: the reconcile runs in the `BackendEvent::Connected` arm of `apply_backend_events` (or a system ordered directly after it), **once, then clears** — never on subsequent state pushes. The initial `Connected` snapshot already carries `current_input` + the input list for both mock and LG (seeded from the initial subscription responses), so the "switch only if not already there" comparison reads the just-applied `payload::Input.current`. Draining on state pushes is explicitly rejected: a user who switches to the Chromecast right after an automation wake has issued an authoritative override, and a persistent latch would yank the input back on every push. The latch's job ends at the first post-wake apply.

**Order of operations post-Connected**: power verify → screen re-assert (via `dispatch_screen` → `DispatchVerify`) → input switch. The input switch reuses the same bounded verify-retry discipline as screen (re-send until `current == desired` or the `DispatchRetry` budget is spent — the 1–5 s WoL-settle window means a switch fired at `Connected` can be silently dropped exactly like an early `screen_on`). On budget exhaustion: emit the existing `dispatch-failed` observability tap with `action: "switch-input"` and clear the marker — bounded, observable, never silent or unbounded.

### 4. Primary input restore — a per-event profile policy

**Fact/policy split (the settled shape):** the *fact* — which port is the PC — stays where it is, `primary_input` in the device config, with no new device fields and no overloaded meaning. The *policy* — which events land the TV on that port — is a behavior preference, so it lives in profiles, per event. Restore fires when a profile-dispatched `power_on`/`screen_on` whose mapping sets the restore flag targets a TV with `primary_input` set and a backend advertising the `INPUT` capability; the dispatch inserts `DesiredInput(primary_input)` and the post-connect reconcile lands the switch.

**Why per-event (the scenario matrix):** no fixed qualifying-event set survives contact with the personas. Restore = "switch to `primary_input` after the action lands"; P1 = dedicated monitor, P2 = PC-first with Chromecast evenings, P3 = jiggle-to-return (a mouse wiggle *is* the "give me my PC back" gesture), P4 = power-management-only.

| Event | P1 monitor | P2 PC-first | P3 jiggle-return | P4 power-only |
|---|---|---|---|---|
| Startup / Resume | don't care | restore | restore | no |
| Session unlock | don't care | restore | restore | no |
| DPMS-on (mouse wiggle) | don't care | no — family watching | **yes** — the whole point | no |
| Idle-resume (activity) | don't care | no | yes | no |
| Lock-wake / greeter-resume | don't care | no | probably yes | no |

P2 and P3 disagree on exactly the wiggle rows, so the set is configuration, not a constant. Two compositions fall out: P2's real protection while the family watches is `automate_when = on_primary`, which gates the *whole* dispatch off when the TV is awake on another port (including the idle `screen_off` that would blank their movie) — only wake-from-off events pass, and those restoring to primary is what P2 wants anyway. P3 is explicitly `always` + restore on the wiggle events. The gate and the flag compose instead of overlapping.

**Config form — one schema, two shapes of one grammar:** an event mapping is a string, or a table when it carries modifiers. Not a compat layer — both shapes are the single current schema, and the store round-trips whichever the user wrote:

```toml
on_session_unlock = "screen_on"
on_dpms_on = { action = "screen_on", restore_input = true }
```

**The seam (resolved intent, not provenance):** session events (unlock, lock-wake, idle) never enter the daemon world as `SystemEvent`s — the session service resolves the profile action itself and fires plain `set-screen`/`power-on` rpcs, byte-identical to a manual tray click. Per-event policy dissolves the review round's origin-flag design: whoever resolves the profile resolves the flag too, and the `SetScreen`/`PowerOn` payloads carry the **resolved intent** — `restore_input: bool`, serde-defaulted false. The session sets it from its profile; the world's `dispatch_system_events` sets it from the system profile; manual clients (GUI/CLI/tray) never set it, so manual stays input-neutral by construction. World-side it arrives as an explicit parameter into `power_on_tv`/`dispatch_screen` — never inferred from `reply.is_none()` — and the world needs no event vocabulary for it: future events need zero world changes.

**Stock defaults — the matrix's consensus rows**: restore on for `Startup` / `Resume` / `on_session_unlock`, off for DPMS-on / lock-wake / greeter / idle-resume. P1 never notices, P2 gets it free, P3 flips the wiggle checkboxes, P4 doesn't set `primary_input`. Accepted consequence (no-backwards-compat): a user who set `primary_input` purely for `on_primary` gating gains restore-on-unlock at upgrade — release-note it, no kill-switch.

- **CEC never arms**: `DesiredInput` requires the `INPUT` capability, which CEC deliberately doesn't advertise — the "CEC can't honor a different primary port" case is structurally absent, not documented around. CEC wake already selects the requesting port; if the adapter rides the PC's video path the behavior is inherently right.
- **`automate_when` composition**: the `should_automate` gate runs *before* the dispatch that would arm `DesiredInput`, so input restoration inherits its semantics (see the matrix discussion above). With `OnPrimary`, a wake-from-off passes (unknown current input gets the benefit of the doubt) and lands on primary; an awake TV on another port is gated off entirely and never dragged back — that is `OnPrimary`'s meaning, and it is intended.

### 5. Config schema version tag

While the profile schema is open anyway, every persisted config surface (daemon store, system/seed config, user-profile store, notification settings) gains `version = 1`. **Absence is defined as version 1** — the pre-tag baseline — which is a schema definition, not a compat shim; existing stores load unchanged and are stamped on next write. No migration machinery lands now: compatibility logic waits until a v2 exists, closer to production. The rule stays: exactly one current schema at any time.

## Sequencing

§1–§3's screen work (elevation, pre-flight, re-arm-through-dispatch) rests on verified facts and fixes a live QA regression — it lands and ships independently. §4 (`DesiredInput` arming + the profile flag + the wire `restore_input` field) rests on the unverified WoL-last-input premise — it is a separately mergeable follow-up gated on the C3 field check, in its own commit(s), so the regression fix is never blocked on hardware QA. §5 rides whichever commit first touches the config surfaces.

## Empirical checklist (before/while implementing §4)

- [ ] Verify on the C3: WoL wake restores the previous input (the premise for needing the switch at all).
- [ ] Verify on the C3: post-WoL `switch-input` timing — how soon after `Connected` does SSAP accept an input switch (bounds the verify-retry budget, not the design).
- [ ] Verify CEC-on-G4 (field batch): Image View On input-selection behavior matches the C3 observation.

## Implementation sketch

1. Core: `PowerState::needs_wake_preflight()` = `!(Active | ScreenSaver | ScreenOff)`, spec'd against the wake-preflight prose already in `PowerState`'s docs; unit tests pin the set including the non-exhaustive fall-through (`Unknown` ⇒ true) and `ScreenOff` ⇒ false.
2. World, screen half (§1–§3): elevation arms in `dispatch_screen`; pre-flight via the widened `LinkQuery` (+ system-event query) reading `payload::Power`, with the `reachable_in_standby`-`Unknown` guard; post-`Connected` reconcile re-issues the pending screen action through `dispatch_screen`.
3. Core + world + wire, input half (§4, after the field check): profile action type grows the string-or-table form (untagged serde enum) with `restore_input: bool`, threaded through `SystemProfileConfig`, the user profile config, and the resolved-profile types, with stock defaults per §4; `restore_input: bool` (serde default false) on the `SetScreen`/`PowerOn` payloads, set by the session's profile dispatch and `dispatch_system_events`, never by manual clients; explicit parameter into `power_on_tv`/`dispatch_screen`; `DesiredInput(String)` component; `Connected`-arm drain comparing `payload::Input.current`, bounded verify-retry, `dispatch-failed` tap on exhaustion; GUI profile editor gains a restore checkbox beside the action dropdown for `screen_on`/`power_on` actions.
4. Config version tag (§5): `version = 1` field on the persisted store/profile/settings schemas, absence defined as 1, stamped on write; no load-path branching.
5. Tests (in `tests/dispatch.rs`'s existing fixtures — no new scaffolding): elevation — unlock-shaped `screen_on` on an off TV yields a wake dial; pre-flight — connected TV with cached `ActiveStandby` + `screen_on` goes straight to the wake path with no verify retries, while cached `ScreenOff` stays on the verify path; the cross-teardown case — a `screen_on` that escalates to wake re-arms `DispatchVerify` after reconnect (the one-latch invariant); input — mock seeded with `initial_input` ≠ `primary_input` (the "wrong input after wake" scenario is static seeding — the mock has no wake-changes-input knob, and doesn't need one) asserts a `switch-input` to primary lands after `Connected` (ordering, not exact tick — publish order across keys is not guaranteed); manual `power-on`/`set-screen` rpcs never insert `DesiredInput`; the string and table config forms round-trip through the profile store and resolve to the same action.
6. Docs: architecture.md's world-marker enumeration gains **both** `DispatchVerify` (currently undocumented there) and `DesiredInput`, with the division-of-labor invariant, written as present-tense doc vocabulary (no commit hashes, no tier shorthand from the design discussion); docs/cec/protocol.md notes the Image View On input-selection side effect as load-bearing for primary-input semantics.

## Non-goals

- No input restoration for manual actions — structural, not configurable (see §4). Automation events are all configurable per profile; none are hardcoded in or out.
- No CEC input switching (capability stays unadvertised; `DesiredInput` never arms for CEC).
- No persistent input enforcement — the latch drains once at `Connected`; a user's post-wake input change is authoritative.
- No projector warm-up port (`WarmingUp` handling) — LG never reports it; revisit with the first projector-class backend.

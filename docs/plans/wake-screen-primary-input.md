# Wake, screen, and the primary input: reliable actions that land on the right port

Status: **draft — under discussion**.

## Problem

Three behaviors that looked separate turn out to be one interconnected design, and the current stack has parity gaps in all three:

1. **Screen elevation regression** (QA, 2026-07-05): `on_session_unlock = screen_on` did nothing for a powered-off LG TV. Root's reliable `screen_on` had a pre-flight wake — an off-ish/disconnected TV went straight to the power path (WoL + reconnect) instead of erroring `not-connected` — plus a desired-screen latch re-applied by reconcile after the wake. The port kept the CEC screen-via-power *emulation* and the connected-but-noop verify-escalate tier, but dropped the disconnected *elevation* entry point and the latch.
2. **Wake paths land on different inputs.** Empirically verified: **CEC power-on both wakes the panel and switches the TV to the HDMI port that sent the request** (Image View On is "show *me*"). Presumed (needs field verification on the C3): **WoL wakes the TV to whatever input it was last on** — which, for a TV shared between the PC and a Chromecast on different ports, may not be the PC.
3. **`primary_input` is only a gate today.** The per-TV `primary_input` exists in config and drives `automate_when` (on_primary/off_primary) — but nothing ever *switches to it*. The concept is exactly "the port TisVlay considers the PC"; automation that wakes the TV for a desktop session should be able to land it there.

The unifying observation: post-wake input restoration is the same mechanism as root's desired-screen latch — **desired state recorded at dispatch time, applied by reconcile once the connection is live**. What was tier-C deferred work in the screen discussion becomes the natural carrier for input handling.

## Design

### 1. Screen elevation (root parity — the regression fix)

In `dispatch_screen`, non-emulating backend with no `ConnectionHandle`:

- `screen_on` + `link.supports_wake` → route to `power_on_tv` (the wake path; since de969ac it also upgrades an in-flight plain retry). For LG, the wake itself lights the panel.
- `screen_off` → reply ok as a satisfied no-op — a dark, disconnected TV already *is* screen-off; erroring makes profile dispatch noisy at shutdown-ish moments. *(Open question: latch the intent instead, root-style? Proposed: no-op — the latch buys nothing until a backend exists where "connected but screen state unknown at reconnect" matters.)*

### 2. Pre-flight wake (root parity — latency + the ActiveStandby quirk)

*Connected* TV whose cached `Power` is off-ish (`Standby` / `Suspend` / `ActiveStandby` / `Unknown`) + `screen_on` → skip the verify-retry loop, go straight to the power path. Root's rationale holds verbatim: `screen_on` over the existing socket no-ops or stalls against a half-asleep TV, and LG's `ActiveStandby` ACKs commands without ever transitioning. Needs a `needs_wake_preflight` predicate on `PowerState` in core (root had it daemon-side; it retired with the old stack).

### 3. Desired-state latch + post-connect reconcile (the carrier)

A `DesiredState` component recorded at automation-dispatch time and drained by a reconcile step when `BackendEvent::Connected` lands (and on state pushes for the connected case):

- `screen: Option<bool>` — root's `desired_screen`, re-asserting the panel intent after a wake-driven reconnect.
- `input: Option<String>` — new: the input to land on once live.

Reconcile applies whatever is recorded, through the existing verify tier where the action supports it, then clears. This also gives the defer-while-reconnecting behavior for free: a screen action arriving mid-wake updates the latch instead of racing the dial.

### 4. Primary input on automation wakes

When an **automation event** (profile-dispatched: startup, resume, unlock, lock-wake, dpms-on — not manual GUI/CLI/tray actions) resolves to `power_on` or `screen_on` for a TV with `primary_input` set, the dispatch also records `DesiredState.input = primary_input`. Post-connect reconcile switches input if the TV didn't come up there.

- **Manual actions stay input-neutral**: a user clicking power-on in the tray may want the Chromecast; only automation implies "desktop session incoming".
- **CEC**: input capability is deliberately not advertised (docs/cec/protocol.md), and CEC wake already selects the requesting port — if the CEC adapter rides the PC's video path, the behavior is inherently correct; if `primary_input` names a *different* port, CEC cannot honor it (document, don't warn-spam).
- **LG**: switch via the normal `switch-input` backend command after `Connected` + power verify; the input list is live by then.
- *(Open question: opt-out knob? Proposal: none initially — setting `primary_input` is itself the opt-in; clearing it opts out. A later `automate_when`-style policy field can grow if the field demands it.)*

## Empirical checklist (before/while implementing)

- [ ] Verify on the C3: WoL wake restores the previous input (the premise for needing the switch at all).
- [ ] Verify on the C3: post-WoL `switch-input` timing — how soon after `Connected` does SSAP accept an input switch (interacts with the 1–5 s WoL settle quirk)?
- [ ] Verify CEC-on-G4 (field batch): Image View On input-selection behavior matches the C3 observation.

## Implementation sketch

1. Core: `PowerState::needs_wake_preflight()` (+ unit tests pinning the set).
2. World: `DesiredState` component; `dispatch_screen` elevation arms (§1) and pre-flight (§2); automation dispatch records `input` from `Meta.primary_input` (§4); reconcile drains the latch on `Connected` / relevant state pushes.
3. Tests (mock has inputs + switch machinery): elevation — unlock-shaped `screen_on` on an off TV yields a wake dial and, post-connect, a `switch-input` to primary; pre-flight — connected TV with cached `ActiveStandby` + `screen_on` goes straight to the wake path with no verify retries; manual `power-on` rpc does *not* record an input intent.
4. Docs: architecture.md dispatch section gains the latch/reconcile sentence; docs/cec/protocol.md notes the input-selection side effect as load-bearing for primary-input semantics.

## Non-goals

- No input restoration for manual actions (see §4).
- No CEC input switching (capability stays unadvertised).
- No projector warm-up port (`WarmingUp` handling) — LG never reports it; revisit with the first projector-class backend.

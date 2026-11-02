# A wake reports success only once the TV is awake

## Goal

A power-on that the TV doesn't act on should keep trying, and fail visibly when it gives up, instead of reporting success with the TV dark. Today two backend paths call a wake done before the TV has woken:

- **LG's `wake_and_connect`** returns as soon as SSAP registers. An LG in `ActiveStandby` (for a few seconds after a remote-off) still accepts SSAP, so the dial connects on its first attempt, sends one WoL, and returns. On the #228 hardware checks, a power-on 1 s after a remote-off connected on attempt 1 and the TV stayed in `activestandby` until the remote turned it on; `progress/power` said `connected`.
- **CEC's in-place `power_on`** (a power-on over the live link of a TV in standby, which is how most CEC power-ons go since the rewrite, because a CEC TV stays connected in standby) sends one Image View On and returns. A lost frame leaves the TV off, and only the journal hears of it.

This retires [concerns/power-on-unverified.md](../concerns/power-on-unverified.md).

## Background

- **The contract.** [docs/backends/implementing-a-vendor.md](../backends/implementing-a-vendor.md): "`wake_and_connect()` owns *all* retry/backoff/probe logic for the wake path — the daemon calls it and treats it as atomic." The same guide keeps link and power as separate axes (`state/link`, `state/power`); `progress/power` is the wake flow's progress.
- **May 2026** (`645f94de`, the dispatch QoS tiers): wake-side actions became reliable, but power-on was left out of the verify tier on purpose, since "`power_on` already has WoL + retry inside". Its reliability was delegated to each backend's wake.
- **July 2026** (`378439a8`): CEC's dial-path wake got a re-send-and-poll loop after a field failure (a single Image View On lost 7 s after standby). `CecConnector::wake_and_connect` sends Image View On up to `WAKE_VERIFY_ATTEMPTS` (4) times, `WAKE_VERIFY_DELAY` (1.5 s) apart, and polls power until the TV reports `On` or `ToOn`. The in-place `CecConnection::power_on` never got the same loop.
- **The world's side is already right** after #228: a power-on routes in place for a standby-reachable bus, to a wake dial for a network TV that needs waking, and is satisfied for an awake one. The gaps are inside the backends.

## Decisions

1. **The backends honor the wake contract; the world gains no power-on verify tier.** A wake (dial or in place) succeeds only once the device reports an awake power state. Reason: the guide already assigns wake-path probing to the backend, the transport-specific retry (WoL resend, Image View On re-send, re-claiming a logical address) already lives there, and a world-level verify would add a second retry layer over the backends' own loops. Considered and rejected for now: adding power-on to the world's reliable tier (`DispatchVerify`).
2. **"Awake" is `PowerState::is_awake`** (`Active`, `ScreenSaver`, `ScreenOff`): the rule the CLI's `power` getter, the GUI, the tray and #228's routing already use. CEC keeps its `reachable()` rule (`On` / `ToOn`), which maps onto it.
3. **`progress/power`'s phases keep their meaning.** `connected` stays "the wake-and-connect returned a live connection"; the contract makes that mean the wake landed. No key, payload or phase changes, so this ships as `fix` commits in a patch release.
4. **A failed in-place power-on publishes `progress/power` `failed`,** as a failed wake dial does. The connection task sends `BackendEvent::PowerPhase(Failed)` when `power_on` errors (other than the closed link `PowerOnLost` already covers); the world's existing handler publishes it. No new event or wire value.
5. **The loops are tested without hardware.** LG's wake-and-confirm loop runs against the fake SSAP server (`test_server.rs`, whose `getPowerState` payload is settable) or is factored over closures so it runs under paused time; CEC's in-place loop runs on `FakeCecLink`, as the dial path's `wake_and_connect_*` tests do.

## Steps

1. **LG: the wake confirms the TV is awake.** In `LgConnector::wake_and_connect`, after a connect succeeds, read the power state (`get_power_state`). Return the connection only if it's awake; otherwise keep the connection, resend WoL (`wol::wake_best_effort`) and poll the power state each `WAKE_CONNECT_RETRY_INTERVAL`, within the existing attempt budget, then fail with a typed error. A dropped SSAP session falls back into the connect loop. Tests: a TV that answers `Active Standby` for N polls then `Active` returns on the N+1th, with WoL resent between; one that never leaves `Active Standby` fails within the budget; one that's already `Active` returns at once (no extra WoL).
2. **CEC: the in-place power-on re-sends and polls.** Extract the dial path's re-send-and-poll loop into one function the connector's `wake_and_connect` and `CecConnection::power_on` both use. `power_on` returns `Ok` once the TV reports on, and an error naming the device when the attempts run out. Tests on `FakeCecLink`: a fake that ignores the first two Image View Ons wakes on the third in place; one that never wakes fails after `WAKE_VERIFY_ATTEMPTS`; the dial-path tests keep passing.
3. **World: a failed in-place power-on is reported.** The backend task sends `BackendEvent::PowerPhase(Failed)` when an in-place `PowerOn` fails with anything other than the closed link `PowerOnLost` handles. The mock gains a knob for an in-place power-on that fails without closing the link. World test: a standby-reachable mock whose in-place power-on fails publishes `progress/power` `failed`, and the link stays up.
4. **Docs.** The vendor guide says a wake (`wake_and_connect`, or an in-place `power_on`) succeeds only once the device reports awake, not when the link opens. `architecture.md`'s power-path sentence notes the same. The concern retires, and `lg/firmware-quirks.md`'s remote-off latch section points at step 1's loop.
5. **Hardware check** (unattended, logging to a file, with CEC Image View On as the safety net, as in #228's probe):
   - LG: a CEC `<Standby>` (which leaves the LG link up in `activestandby`), then an LG `power on` after 0.2, 0.5, 1 and 2 s, twice each: every one must end with the TV awake and `progress/power` `connected`, or `failed` when the TV refused for the whole budget.
   - CEC in place: `power on` to the CEC TV from standby, several times: the TV wakes each time. A lost frame can't be forced on hardware, so step 2's fake covers that.
   - A real remote-off then `power on` needs the maintainer at the remote; optional.

## Open questions for review

- **CEC's in-place loop blocks the connection's command task** for up to about 6 s (4 × 1.5 s) while it polls, so other commands to that TV queue behind a wake. The dial path holds the TV unavailable for the same window today. Acceptable, or should the in-place loop run off the command task?
- **The GUI's power-on progress (#230) waits 2 s after the ack for a first phase,** then stops following. An in-place power-on publishes no phase until it fails, which can take about 6 s, so the GUI would miss that `failed`. Options: publish `connecting` when an in-place wake starts (it isn't a dial, so the word fits poorly), or have the GUI follow longer for a standby-reachable TV.
- **LG: hold the connection while polling, or reconnect each attempt?** Holding avoids a TLS handshake and registration per attempt; reconnecting reuses the existing loop unchanged. The plan holds it.
- **Should `WarmingUp` count as awake for the purpose of returning?** It isn't awake by `is_awake`, so the loop keeps polling through it, which a projector-style backend might need bounded differently.

## Deviations

None yet.

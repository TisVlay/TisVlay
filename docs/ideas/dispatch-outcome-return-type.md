# Richer `DispatchOutcome` return type for screen_on / screen_off

Today `tv_manager::screen_on` and `tv_manager::screen_off` both return `Result<(), TvManagerError>`. The bare `Ok(())` collapses three semantically distinct success paths:

- **Reached target.** Command landed, verify passed, cached state is at the requested target. The happy path.
- **Graceful fallback (LG firmware quirk).** Pre-flight wake fired but the post-wake settle phase timed out — LG firmware honored a recent remote-off latch and refused to wake into `Active`. The TV sits in `ActiveStandby` with the panel already dark. Daemon accepts this as functionally screen-off and returns `Ok` with an info log. Documented in [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md).
- **Verify-retry exhausted, no escalation.** For `screen_off` only: LG rejected the `turnOffScreen` command repeatedly (panel refused to blank), but the TV is otherwise reachable. The daemon doesn't power-cycle — logs warn and returns Ok. The user's intent isn't satisfied, but the daemon has no recourse.

All three look identical on the wire. Clients (GUI, tray, telemetry) can't distinguish "panel went dark cleanly" from "TV ignored you and we accepted it."

## Why this is worth fixing

- **Telemetry need.** Now that the GUI's Profiles pane has landed, users will benefit from seeing per-TV reliability — "this TV falls back X% of the time" surfaces firmware quirks early.
- **Smarter retry logic.** A future session-side event handler could decide differently based on outcome (e.g., suppress repeated screen_off events when graceful-fallback is being hit, since the panel is already off and the command is fighting LG firmware).
- **Future-proofing the return type.** Retrofitting `Result<()>` → `Result<DispatchOutcome>` later is harder than starting with it. The code review specifically flagged this as the "one design choice most likely to age into a regret."

## Path forward

When picked up:

- Add `DispatchOutcome` enum in `tisvlay-core::traits` (or a new `dispatch` module):

  ```rust
  pub enum DispatchOutcome {
      Reached,
      AcceptedFallback,  // LG firmware quirk graceful-accept
      Refused,           // verify exhausted, panel didn't blank
  }
  ```

  `#[non_exhaustive]` so adding e.g. `RejectedByPolicy` later isn't breaking.

- Change `tv_manager::screen_on` / `screen_off` to return `Result<DispatchOutcome, TvManagerError>`. Internal call sites (system-event dispatcher, session-event handler, `Tv1.Screen.SetScreen` D-Bus handler) can ignore the variant initially by `.map(|_| ())`.

- Decide on the D-Bus surface: either a new optional signal `Tv1.Screen.DispatchOutcome(s)` emitted after each dispatch, or extend the existing `SetScreen` to return the outcome (breaks the existing `b → ()` signature; awkward).

- Surface in the daemon's structured logs first — that gets immediate operational value with no D-Bus contract change.

## Trade-offs

- The internal API change is small (one method on `TvManager`, three call sites).
- Risk of orphan plumbing — if no client consumes the variant initially, the enum is dead weight. The "Future-proofing" framing has to actually be paid back by a real consumer.
- Mitigation: only land this when the GUI Profiles pane or a telemetry consumer is being built and would use the distinction.

## Trigger condition

File this when one of:

- The GUI Profiles pane is being implemented and wants to surface quirk-fallback frequency per TV.
- The session event handler is refactored to make smarter dispatch decisions based on prior outcomes.
- A field bug reports the daemon "lying about success" because a real `Refused` looks the same as a `Reached` on the wire.

## Related

- LG firmware quirk graceful-fallback path: [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md) ("Recent remote-off latches LG firmware against `turnOn`").
- Reliable screen-state contract: [docs/design.md](../design.md) — "Reliable screen-state actions" subsection.
- Multi-agent code review on `feat/screen-on-reliable` flagged this; deferred per "build it before you need it."

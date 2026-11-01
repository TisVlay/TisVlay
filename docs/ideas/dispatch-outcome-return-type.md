# Richer `DispatchOutcome` return type for screen_on / screen_off

Today a screen action rides `DispatchVerify` in `tisvlay-world`: the rpc answers once the command is sent, and a dispatch that runs out of retries and its one wake escalation publishes `event/dispatch-failed`. Short of that give-up, the outcome isn't published, so three distinct endings look the same:

- **Reached target.** Command landed, verify passed, cached state is at the requested target. The happy path.
- **Graceful fallback (LG firmware quirk).** Pre-flight wake fired but the post-wake settle phase timed out — LG firmware honored a recent remote-off latch and refused to wake into `Active`. The TV sits in `ActiveStandby` with the panel already dark. Daemon accepts this as functionally screen-off and returns `Ok` with an info log. Documented in [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md).
- **Verify-retry exhausted, no escalation.** For `screen_off` only: LG rejected the `turnOffScreen` command repeatedly (panel refused to blank), but the TV is otherwise reachable. The daemon doesn't power-cycle — logs warn and returns Ok. The user's intent isn't satisfied, but the daemon has no recourse.

All three look identical on the wire. Clients (GUI, tray, telemetry) can't distinguish "panel went dark cleanly" from "TV ignored you and we accepted it."

## Why this is worth fixing

- **Telemetry need.** Users will benefit from seeing per-TV reliability — "this TV falls back X% of the time" surfaces firmware quirks early.
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

- Record the outcome where `verify_dispatch` in `tisvlay-world`'s `systems/dispatch.rs` ends a `DispatchVerify`.

- Decide on the wire surface: an `event/dispatch-outcome` published after each dispatch, or an outcome field beside the existing `event/dispatch-failed`. The `screen` rpc itself answers before verification, so it can't carry the outcome without waiting for it.

- Surface in the daemon's structured logs first — that gets immediate operational value with no wire change.

## Trade-offs

- The internal change is small: the verify system already knows which ending it reached.
- Risk of orphan plumbing — if no client consumes the variant initially, the enum is dead weight. The "Future-proofing" framing has to actually be paid back by a real consumer.
- Mitigation: only land this when the GUI Profiles pane or a telemetry consumer is being built and would use the distinction.

## Trigger condition

File this when one of:

- The GUI Profiles pane is being implemented and wants to surface quirk-fallback frequency per TV.
- The session event handler is refactored to make smarter dispatch decisions based on prior outcomes.
- A field bug reports the daemon "lying about success" because a real `Refused` looks the same as a `Reached` on the wire.

## Related

- LG firmware quirk graceful-fallback path: [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md) ("Recent remote-off latches LG firmware against `turnOn`").
- Reliable screen-state contract: [architecture.md](../architecture.md) (`DispatchVerify`, the single screen-intent latch).
- Multi-agent code review on `feat/screen-on-reliable` flagged this; deferred per "build it before you need it."

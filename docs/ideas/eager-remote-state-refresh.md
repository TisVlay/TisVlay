# Eager remote-state refresh on gated events

The remote-session detectors publish state on their own cadence — the Sunshine `/serverinfo` level poll ticks every ~2 s while a stream is present. So the *disarm* latency after a stream ends is bounded by that cadence: up to ~2 s before the armed source clears and a wake stops being suppressed. In the common case that is invisible (a returning user is far more than 2 s behind the stream ending, so the source has long since cleared), but a user who walks up within the poll window can hit a suppressed wake for a couple of seconds.

The idea: keep the background poll rate low, and additionally do an on-demand `/serverinfo` pull *at the moment a gated wake event fires* — the exact instant the armed state's freshness decides whether to suppress or wake. Poll lazily, refresh eagerly. This decouples responsiveness from the background rate.

## Session-side: implemented

The session gate (`Core::action_for` / dispatch) and the Sunshine detector live in the **same process**, so a session-side gated wake event (unlock, lock-wake, idle-resume) can trigger a **synchronous, tightly-bounded** `/serverinfo` pull and decide fresh — stream ended → wake immediately. This is the responsive path for session-driven wakes and is implemented (a bounded `probe_once` consulted when a wake would otherwise be suppressed by a live Sunshine source).

## World-side (DPMS-on / resume): deferred — this idea

The world gate (`dispatch_system_events`: DPMS-on, resume) lives in the **daemon**, cross-process from the detector, and this is where an eager refresh gets expensive:

- An **async poke** (tell the session to re-poll) does not wake the TV: the DPMS-on that just got suppressed will not fire again — the display is already on, so there is no second edge to un-suppress. The TV stays dark until the next blank/unblank cycle.
- The only ways to make the world path eager are (a) the **daemon pulls `/serverinfo` itself** — but that puts Sunshine-specific HTTP into the deliberately vendor/detector-agnostic daemon, an architecture smell; or (b) **retroactive re-issue** — after a wake is suppressed and the source then disarms within a short window, re-issue the wake. (b) is the same mechanism as the already-planned *retroactive correction* (there in the inverse direction, for the arm race); a single bidirectional "recent-suppression / recent-arm correction" window could serve both.

## When to revisit

Only if the DPMS-on disarm latency actually feels sluggish in the field with the ~2 s background poll — measure first. The clean implementation is the bidirectional retroactive-correction window (a suppressed wake re-issues if the source clears within N seconds), which also subsumes the planned arm-race correction. Until then the ~2 s poll plus the session-side eager pull covers the interactive cases.

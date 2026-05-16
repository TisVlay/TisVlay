# LG WebOS firmware quirks

Behaviors of LG WebOS firmware (tested on OLED42C3PUA / OLED65G4SUB) that bit us during the Action QoS work and the post-merge perf triage that followed. Each quirk includes the trace, the fix, and the regression coverage. Future LG firmware revisions could reintroduce variants — keep this doc in sync if a smoke-test surfaces a new pattern.

For SSAP protocol reference (subscription endpoints, request/response shape), see [webos.md](webos.md). For LG-backend test infrastructure (`FakeSsapServer` etc.) see [testing.md](testing.md).

## Power-state mapping: `ActiveStandby` must map to `is_screen_on = false`

**Symptom.** After lock → remote Power button → mouse-move, the reliable `screen_on` path returned `Ok` but the panel stayed dark. The verify check read cached `PowerState == "active"` and exited "successful".

**Root cause.** When the user presses Power on the LG remote from Screen Off, the OLED enters `ActiveStandby`: NIC alive (so SSAP keeps responding), panel off. The original `PowerState::update_power` had:

```rust
self.is_screen_on = !matches!(
    state,
    PowerState::ScreenOff | PowerState::Standby | PowerState::Suspend
);
```

`ActiveStandby` fell through the suppression list with `is_screen_on = true`. The forwarder mapped `(is_on=true, is_screen_on=true)` → `"active"`, defeating the reliable verify.

**Fix.** [`tisvlay-core/src/state.rs::PowerState::update_power`](../../crates/tisvlay-core/src/state.rs) — `ActiveStandby` joined the screen-off suppression list. Now `(is_on=true, is_screen_on=false)` → derived `"screen_off"`, verify fails, reliable tier escalates.

**Regression coverage.**

- `tisvlay-core/src/state.rs` — `active_standby_marks_screen_off` (focused regression) + `update_power_screen_on_mapping_is_complete` (table-driven mapping pin).
- `tisvlay-session/tests/session_events.rs` — `reliable_screen_on_escalates_when_tv_slipped_to_active_standby`.

## `screen_on` / `screen_off` error discrimination: only mask code `500`

**Symptom.** LG client's `screen_on` was wrapping the SSAP call in a blanket `Err(TvError(_)) => Ok(())` mask under the comment "screen likely already on". This collapses the `500 method not allowed` idempotency case (correct UX) with the `-102 not in screen-off substate` failure case (which the reliable dispatch tier needs to detect to escalate to `power_on`).

**Root cause.** [`tisvlay-lg/src/client.rs::screen_on`](../../crates/tisvlay-lg/src/client.rs) treated every `TvError` as idempotent-success. `-102` was indistinguishable from `500` to the daemon.

**Fix.** A new `is_idempotent_screen_error(error: &str) -> bool` discriminator splits the LG error string on whitespace and only matches leading code `500`. Anything else (`-102`, `401`, `404`, etc.) propagates as `Err`.

**Regression coverage.**

- `crates/tisvlay-lg/src/client.rs` — `screen_on_propagates_minus_102_not_in_screen_off_substate` + `screen_on_masks_500_idempotent_error`.
- `FakeSsapServer` gained `set_command_error(uri, error_str)` so the regression tests can drive arbitrary error strings.

## Field-test verification trace (post-fix)

For reference, the journal trace from the same manual repro (lock → remote power-off → mouse) **after** the Bug A + Bug B fixes landed. Total wake-from-`ActiveStandby` is ~8 s, dominated by the LG WoL → boot → SSAP registration sequence:

```
00:20:32  Dispatch screen_on reliable  (attempt #1)
          screen_on returned idempotent 500
            (LG returned 500 here, not -102)
            → treated as success
          [300 ms verify wait]
          cached PowerState ≠ "active"          ← Bug A fix at work
00:20:32  Dispatch screen_on reliable  (attempt #2)
          screen_on returned idempotent 500
          cached PowerState ≠ "active"
          attempts exhausted ─────────────────►
00:20:32  Escalating to power_on
          Direct-connect probe skipped/failed
          Sent Wake-on-LAN packet
00:20:33  Registration response (phase 1)
          Registered with TV, got client key
          LG TV online after wake
00:20:40  Subscribed to backend state updates
          TV is online                          ← end-to-end ~8 s
```

Three things worth knowing:

1. **Bug A was the load-bearing fix on this firmware.** The G4 returned `500` (not `-102`) for `screen_on` while in `ActiveStandby` — so Bug B's discrimination would have masked it as success either way. What made the reliable tier escalate was the corrected `ActiveStandby` → `is_screen_on = false` mapping (Bug A fix), which made the cached PowerState read non-`"active"` and the verify fail.
2. **Bug B is a genuine safety net.** Other firmware revisions / state transitions return `-102` instead. The new discriminator routes those to the same escalation path.
3. **Reliable-tier overhead is ~600 ms** (2 × 300 ms verify). The rest is hardware wake time — the TV's wake-from-`ActiveStandby` dominates.

## The 10 s `REQUEST_MS` is load-bearing — don't bypass `screen_on`

**Context.** [`tisvlay-lg/src/endpoints.rs::timeouts::REQUEST_MS`](../../crates/tisvlay-lg/src/endpoints.rs) = `10_000` ms. When the WebSocket goes half-dead (TCP up, TV not replying — observed when the LG firmware drops a WS connection without sending CLOSE), `screen_on` stalls the full 10 s waiting on a reply before erroring out. Post-merge smoke testing reproduced this: total wake-from-half-dead-WS lands around 13 s.

The instinctive fix is "shorten the wait" — either by adding a tighter timeout race around `screen_on`, by predicating a short-circuit on WS staleness, or by lowering `REQUEST_MS` directly. We tried all three. **All three regressed.** This section is here to save the next engineer the time.

### Why each bypass loses

The LG client's `with_retry` wrapper ([backend.rs](../../crates/tisvlay-lg/src/backend.rs)) responds to `Error::RequestTimeout` (i.e., `REQUEST_MS` expiring) by tearing down the current WebSocket, opening a fresh one with `register`, and retrying the command. That fresh-WS handshake **physically rouses the TV** — by the time the second attempt also fails and the dispatch escalates to `power_on`, the TV is already warm enough that the subsequent WoL → Registration happens in <1 s.

Any approach that *bypasses* `screen_on` to avoid the 10 s wait skips this accidental knock. The TV slips deeper into standby, and full WoL recovery now takes 9-12 s instead of <1 s. The arithmetic: ~7-10 s saved on the bypassed wait, ~9-12 s lost on slower recovery. Net pessimization in every case.

### Spike data

Three bypass attempts. Each was implemented on the live `feat/action-qos` branch, smoke-tested on real hardware, then reverted. The cherry-pick + revert pairs are in the git history.

#### Spike A — `screen_on` timeout race (commits `f384f60` + `6845097`)

Wrap `self.screen_on(tv_id)` in `tokio::time::timeout(Duration::from_secs(2), ...)`. Happy path returns in ~10 ms; 2 s well clear of that and well under `REQUEST_MS`. On timeout, treat as backend `Err` — break the verify-retry loop and escalate.

| Trial | Path                              | Total | Δ vs 13 s pre-fix |
|-------|-----------------------------------|-------|-------------------|
| 1     | Fast `500` (TV responsive)        | 9 s   | —                 |
| 2     | Timed out → power_on              | 15 s  | **+2 s**          |
| 3     | Timed out → power_on              | 17 s  | **+4 s**          |
| 4     | Timed out → power_on              | 15 s  | **+2 s**          |

Trial 2 breakdown:

```
22:04:45  Dispatch
22:04:47  screen_on timed out (+2 s)         ← fix fired
22:04:49  Direct-connect probe failed (+4 s)
22:04:49  WoL packet sent
22:04:54  Registration response (+9 s)       ← 5 s after WoL (vs <1 s pre-fix)
22:05:00  Subscribed (+15 s)
```

#### Spike B — WS liveness predicate (commits `4159cba` + `083fb10`)

Track `last_rx_at` on the LG `Connection`, expose via `TvConnection::last_activity_at()`. In `dispatch_reliable_screen_on`, short-circuit to `power_on` when the WS has been quiet for >5 s.

| Trial | Path                              | Total |
|-------|-----------------------------------|-------|
| 1     | Stale-WS short-circuit            | 18 s  |
| 2     | Stale-WS short-circuit            | 12 s  |
| 3     | Stale-WS short-circuit            | 14 s  |
| 4     | Stale-WS short-circuit            | 14 s  |
| 5     | Threshold miss → `REQUEST_MS`     | 28 s  |

When the predicate fires (4/5), total is comparable to the pre-fix 13 s — the predicate works, but again at a net cost. When the predicate misses (trial 5; WS quiet 5 s before wake, right at the threshold), the slow path takes **28 s** — `REQUEST_MS` fires AND the subsequent recovery is the slow shape.

#### Spike D — Lower `REQUEST_MS` to 3 s (not committed)

Edit `tisvlay-lg/src/endpoints.rs` `REQUEST_MS: 10000 → 3000` and `CONNECT_MS: 5000 → 3000`. The simplest change of all — no helper-side logic, just a constant.

| Trial | Path                              | Total |
|-------|-----------------------------------|-------|
| 1     | Fast `500` (TV responsive)        | 5 s   |
| 2     | `Stale connection` at +4 s        | 18 s  |
| 3     | `Stale connection` at +3 s        | 20 s  |
| 4     | `Stale connection` at +3 s        | 15 s  |

Trial 4 breakdown vs pre-fix:

```
Pre-fix (REQUEST_MS=10000):       This trial (REQUEST_MS=3000):
  Dispatch                          Dispatch
  +10 s   Stale (REQUEST_MS)        +3 s    Stale (REQUEST_MS)
  +11 s   Command failed            +6 s    Command failed
  +11 s   WoL + Registration        +8 s    WoL packet sent
            (same second — warm!)   +12 s   Registration (+4 s after WoL)
  +13 s   TV is online              +15 s   TV is online
```

Net: 7 s saved on `REQUEST_MS`, 9 s lost on recovery.

### The pre-flight short-circuit is still correct (just narrower)

The one bypass that *does* belong is the pre-flight short-circuit in `dispatch_reliable_screen_on`: when `last_known_power_state == "standby"` (TV's `is_on == false`, i.e., already past Screen Off into deep standby), skip `screen_on` and dispatch `power_on` directly. This is correct because:

- In the deeper-standby case, `screen_on` would fail anyway (the TV is genuinely unreachable, not just half-dead WS).
- The "knock" mechanism doesn't help here because the TV's WS is fully torn down, not just stale — `with_retry`'s reconnect would also fail.
- Going straight to `power_on` skips a guaranteed-failure round-trip.

The short-circuit fires on cached `"standby"`, not on `"screen_off"`. Stale-WS-but-cached-screen-off scenarios continue to use the natural `REQUEST_MS` → `with_retry` → escalate path, because the knock is load-bearing there.

### Bottom line

Future engineers tempted to "shorten the slow path" should benchmark against Spike A's and B's per-trial numbers before shipping. Anything that bypasses `screen_on` under stale-WS conditions will lose the with_retry knock and end up slower than the pre-fix 13 s baseline. If you have a fix that *preserves* the knock (e.g., explicit synchronous reconnect-then-screen_on), it's worth trying — `SO_KEEPALIVE` with aggressive timing (`TCP_KEEPIDLE=5`, `TCP_KEEPINTVL=2`, `TCP_KEEPCNT=2`) is the only candidate we considered that fits this shape, but its blast radius (every WS on the daemon, fiddly to extract the raw fd from `MaybeTlsStream`) didn't justify proceeding without a stronger signal that the 13 s floor is unacceptable.

The autonomous reproducer used for the spike measurements is at [`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py). It opens an out-of-band SSAP WebSocket via the daemon's existing `client_key`, sends `ssap://system/turnOff` to put the TV in `screen_off`, then dispatches reliable `screen_on` and times `t0 → PowerState == "active"`. Useful for repeat measurement of cold-start dispatch latency from a known TV state without driving the actual desktop lock/unlock flow.

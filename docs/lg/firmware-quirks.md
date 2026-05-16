# LG WebOS firmware quirks

Behaviors of LG WebOS firmware (tested on OLED42C3PUA / OLED65G4SUB) that bit us while making `screen_on` / `screen_off` inherently reliable. Each quirk includes the trace, the fix, and the regression coverage. Future LG firmware revisions could reintroduce variants — keep this doc in sync if a smoke-test surfaces a new pattern.

For SSAP protocol reference (subscription endpoints, request/response shape), see [webos.md](webos.md). For LG-backend test infrastructure (`FakeSsapServer` etc.) see [testing.md](testing.md).

## Power-state mapping: `ActiveStandby` must map to `is_screen_on = false`

**Symptom.** After lock → remote Power button → mouse-move, the inherently-reliable `screen_on` path returned `Ok` but the panel stayed dark. The verify check read cached `PowerState == "active"` and exited "successful".

**Root cause.** When the user presses Power on the LG remote from Screen Off, the OLED enters `ActiveStandby`: NIC alive (so SSAP keeps responding), panel off. The original `PowerState::update_power` had:

```rust
self.is_screen_on = !matches!(
    state,
    PowerState::ScreenOff | PowerState::Standby | PowerState::Suspend
);
```

`ActiveStandby` fell through the suppression list with `is_screen_on = true`. The forwarder mapped `(is_on=true, is_screen_on=true)` → `"active"`, defeating the verify check.

**Fix.** [`tisvlay-core/src/state.rs::PowerState::update_power`](../../crates/tisvlay-core/src/state.rs) — `ActiveStandby` joined the screen-off suppression list. Now `(is_on=true, is_screen_on=false)` → derived `"screen_off"`, verify fails, the reliable-dispatch loop escalates.

**Regression coverage.**

- `tisvlay-core/src/state.rs` — `active_standby_marks_screen_off` (focused regression) + `update_power_screen_on_mapping_is_complete` (table-driven mapping pin).
- `tisvlayd/tests/dbus_contract.rs` — `screen_on_escalates_via_verify_retry_loop` (injects `ActiveStandby` + flips `MockHandle::set_screen_on_noop` so the verify-retry loop actually exhausts before escalating; without the noop knob the mock's default `screen_on` would let verify pass on attempt #1).

## `screen_on` / `screen_off` error discrimination: only mask code `500`

**Symptom.** LG client's `screen_on` was wrapping the SSAP call in a blanket `Err(TvError(_)) => Ok(())` mask under the comment "screen likely already on". This collapses the `500 method not allowed` idempotency case (correct UX) with the `-102 not in screen-off substate` failure case (which the verify check needed to detect to escalate to `power_on`).

**Root cause.** [`tisvlay-lg/src/client.rs::screen_on`](../../crates/tisvlay-lg/src/client.rs) treated every `TvError` as idempotent-success. `-102` was indistinguishable from `500` to the daemon.

**Fix.** A new `is_idempotent_screen_error(error: &str) -> bool` discriminator splits the LG error string on whitespace and only matches leading code `500`. Anything else (`-102`, `401`, `404`, etc.) propagates as `Err`.

**Regression coverage.**

- `crates/tisvlay-lg/src/client.rs` — `screen_on_propagates_minus_102_not_in_screen_off_substate` + `screen_on_masks_500_idempotent_error`.
- `FakeSsapServer` gained `set_command_error(uri, error_str)` so the regression tests can drive arbitrary error strings.

## `getPowerState` `state` field lies during transitions; honor `processing` / `onOff`

**Symptom.** After remote-press power-off, the daemon's cached `last_known_power_state` stayed at `Active` for ~3 seconds while the panel was already dark. A `screen_off` click within that window hit a pre-flight wake gate that read `Active` and skipped the wake. The screen_off command then landed on a half-asleep TV, got `500` rejected, and verify exhausted against the still-stale `Active`.

**Root cause.** LG's `getPowerState` subscription pushes (and the synchronous query response) carry **three** signals during a transition:

| Field        | Behavior during off-direction transition                                       |
| ------------ | ------------------------------------------------------------------------------ |
| `state`      | Stays `"Active"` for ~3 s while the TV transitions to `Active Standby`.        |
| `processing` | Set within ~200 ms: `"Request Power Off"` → `"Request Active Standby"` → `"Prepare Active Standby"`. Absent or `"None"` in steady state. |
| `onOff`      | Set within ~200 ms: `"off"` during off-direction transitions.                  |

[`scripts/probe_power_state_timing.py`](../../scripts/probe_power_state_timing.py) captures the wire payloads — see the "Reference probe data" section below. Before this fix, the daemon read only the `state` field, so it saw the lie.

**Fix.** New `parse_power_state_payload(&Value) -> Option<PowerState>` helper in [`tisvlay-lg/src/backend.rs`](../../crates/tisvlay-lg/src/backend.rs) projects to `ActiveStandby` whenever `onOff == "off"` OR `processing` is present and not `"None"`. Both call sites route through it:

- `update_state_from_subscription` (the subscription push path).
- `WebOsClient::get_power_state` (the synchronous query path used by `tv_manager::refresh_power_state`).

`ActiveStandby` is sufficient to trigger the daemon's `needs_wake_preflight` discriminator, so pre-flight fires within ~200 ms of the remote press instead of waiting ~3 s for the `state` field to catch up.

**Regression coverage.**

- `tisvlay-lg/src/backend.rs` — `subscription_power_transition_to_off_projects_active_standby` (all three captured `processing` values), `subscription_power_on_off_field_alone_projects_active_standby`, `subscription_power_processing_none_falls_through_to_state_field`, and the direct-helper tests `parse_payload_*` pin the contract for both subscription and synchronous-query callers.

## Recent remote-off latches LG firmware against `turnOn` for ~1 s

**Symptom.** When the user pressed Power on the remote and then clicked GUI screen-off within ~1 second, the daemon's pre-flight wake gate fired correctly (`cached=ActiveStandby` detected, `power_on` dispatched, WebSocket reconnected in ~700 ms), but the subsequent `screen_off` command got `500` rejected and verify exhausted with `cached=ActiveStandby` for the entire retry budget. The TV stayed in `ActiveStandby` — panel dark, red LED on, daemon reporting "verify-retry exhausted; panel did not blank" despite the panel being already off.

**Root cause.** When the user's remote-off has just landed (~1 s prior), the LG firmware honors that intent over an incoming `turnOn`/`turnOnScreen` from the network. `power_on` succeeds at the WebSocket layer (reconnect happens, initial subscribe response arrives) but the TV's substate never transitions out of `ActiveStandby` — LG keeps reporting `ActiveStandby` (with `onOff="off"` / `processing` fields) until it eventually settles into deep `Standby` on its own. Wait ~5 s before sending `turnOn` and LG accepts the wake cleanly; click faster and LG won't cooperate. Hardware behavior, not something the daemon can override.

**Fix.** `tv_manager::screen_off` ([crates/tisvlayd/src/tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs)) adds a post-wake **settle phase** after pre-flight `power_on`: await the cached state leaving the wake-preflight set, bounded by `verify_delay × max_attempts`. Three outcomes:

- Settled to `Active` / `ScreenSaver`: TV woke cleanly. Fall through to verify-retry loop, screen_off lands.
- Settled to `ScreenOff`: LG took the direct path through to the target on wake-up. Return Ok.
- Did not settle (timeout): LG firmware refused the wake. TV sits in `ActiveStandby` with the panel already dark and the red LED on, which is functionally what `screen_off` is asking for. Return Ok with an info log rather than burning more retries on `turnOffScreen` rejections.

The graceful fallback declares success when the panel is demonstrably off, even if LG's substate isn't the literal `ScreenOff` we'd prefer. From the user's POV, `ActiveStandby` (red LED on, panel dark) and `ScreenOff` (red LED off, panel dark) are visually indistinguishable; both leave the TV reachable for the next command.

**Regression coverage.** No mock-level test — the firmware-quirk path is hard to reproduce without per-connection initial-state knobs on the mock connector. Live-hardware smoke covers it; the info log `"screen_off: TV did not settle into Active after power_on; accepting wake-preflight substate as panel-off (LG firmware quirk)"` is the verification signal.

## Field-test verification trace (post-fix)

For reference, the journal trace from the manual repro (lock → remote power-off → mouse) **after** the Bug A + Bug B fixes landed, with the `processing`/`onOff` discriminators in place. Total wake-from-`ActiveStandby` is ~8 s, dominated by the LG WoL → boot → SSAP registration sequence:

```
+0.000s   screen_on pre-flight: powering on
            (cached=ActiveStandby — discriminator-projected within ~200ms of remote)
+0.001s   PowerOn requested
+0.001s   Direct-connect probe skipped/failed
          Sent Wake-on-LAN packet
+1.000s   Registration response (phase 1)
          Registered with TV, got client key
          LG TV online after wake
+7.000s   Subscribed to backend state updates
+7.001s   TV is online                           ← end-to-end ~7-8 s
```

Three things worth knowing:

1. **The discriminator fix made the pre-flight reliable.** Pre-fix, the daemon's cached state read stale `Active` for ~3 s after a remote-off, so pre-flight skipped the wake on fast clicks. With the `processing`/`onOff` parsing, cached transitions to `ActiveStandby` within ~200 ms — well inside the human reaction-time window for a follow-up click.
2. **The WoL recovery path itself is unchanged.** The reliable-dispatch layer's only job is to decide *when* to escalate to `power_on`; once it does, the existing WoL → Registration sequence in `tv_manager::power_on` carries the weight.
3. **Verify overhead is the floor.** ~600 ms (2 × 300 ms verify-delay budget) on the pre-flight path, plus the LG firmware's own wake time. The verify-delay is bounded by [`RetryConfig::verify_delay`](../../crates/tisvlay-core/src/traits.rs); event-driven awaits via watch channels (`TvManager::await_power_state`) return on the first matching subscription push, so the budget is an upper bound rather than a guaranteed wait.

## The 10 s `REQUEST_MS` is load-bearing — don't bypass `screen_on`

**Context.** [`tisvlay-lg/src/endpoints.rs::timeouts::REQUEST_MS`](../../crates/tisvlay-lg/src/endpoints.rs) = `10_000` ms. When the WebSocket goes half-dead (TCP up, TV not replying — observed when the LG firmware drops a WS connection without sending CLOSE), `screen_on` stalls the full 10 s waiting on a reply before erroring out. Smoke testing reproduced this: total wake-from-half-dead-WS lands around 13 s.

The instinctive fix is "shorten the wait" — either by adding a tighter timeout race around `screen_on`, by predicating a short-circuit on WS staleness, or by lowering `REQUEST_MS` directly. We tried all three. **All three regressed.** This section is here to save the next engineer the time.

### Why each bypass loses

The LG client's `with_retry` wrapper ([backend.rs](../../crates/tisvlay-lg/src/backend.rs)) responds to `Error::RequestTimeout` (i.e., `REQUEST_MS` expiring) by tearing down the current WebSocket, opening a fresh one with `register`, and retrying the command. That fresh-WS handshake **physically rouses the TV** — by the time the second attempt also fails and the dispatch escalates to `power_on`, the TV is already warm enough that the subsequent WoL → Registration happens in <1 s.

Any approach that *bypasses* `screen_on` to avoid the 10 s wait skips this accidental knock. The TV slips deeper into standby, and full WoL recovery now takes 9-12 s instead of <1 s. The arithmetic: ~7-10 s saved on the bypassed wait, ~9-12 s lost on slower recovery. Net pessimization in every case.

### Spike data

Three bypass attempts. Each was implemented during the original spike, smoke-tested on real hardware, then reverted. The spike commits are archived on the `feat/action-qos` branch for the design-exploration record.

#### Spike A — `screen_on` timeout race

Wrap `self.screen_on(tv_id)` in `tokio::time::timeout(Duration::from_secs(2), ...)`. Happy path returns in ~10 ms; 2 s well clear of that and well under `REQUEST_MS`. On timeout, treat as backend `Err` — break the verify-retry loop and escalate.

| Trial | Path                              | Total | Δ vs 13 s pre-fix |
|-------|-----------------------------------|-------|-------------------|
| 1     | Fast `500` (TV responsive)        | 9 s   | —                 |
| 2     | Timed out → power_on              | 15 s  | **+2 s**          |
| 3     | Timed out → power_on              | 17 s  | **+4 s**          |
| 4     | Timed out → power_on              | 15 s  | **+2 s**          |

Trial 2 breakdown:

```
+0 s    Dispatch
+2 s    screen_on timed out                  ← spike fired
+4 s    Direct-connect probe failed
+4 s    WoL packet sent
+9 s    Registration response                ← 5 s after WoL (vs <1 s pre-fix)
+15 s   Subscribed
```

#### Spike B — WS liveness predicate

Track `last_rx_at` on the LG `Connection`, expose via `TvConnection::last_activity_at()`. Short-circuit `screen_on` to `power_on` when the WS has been quiet for >5 s.

| Trial | Path                              | Total |
|-------|-----------------------------------|-------|
| 1     | Stale-WS short-circuit            | 18 s  |
| 2     | Stale-WS short-circuit            | 12 s  |
| 3     | Stale-WS short-circuit            | 14 s  |
| 4     | Stale-WS short-circuit            | 14 s  |
| 5     | Threshold miss → `REQUEST_MS`     | 28 s  |

When the predicate fires (4/5), total is comparable to the pre-fix 13 s — the predicate works, but again at a net cost. When the predicate misses (trial 5; WS quiet 5 s before wake, right at the threshold), the slow path takes **28 s** — `REQUEST_MS` fires AND the subsequent recovery is the slow shape.

#### Spike D — Lower `REQUEST_MS` to 3 s

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

### The pre-flight wake gate is narrower (and survives)

The bypass that *does* belong is the pre-flight wake gate in `tv_manager::screen_on` / `screen_off`: when `last_known_power_state` is in the [`needs_wake_preflight`](../../crates/tisvlayd/src/power_state.rs) set (`Standby` / `Suspend` / `ActiveStandby` / `Unknown`), skip the in-band `screen_on` attempt and dispatch `power_on` directly. This is correct because:

- In the deeper-standby case, `screen_on` would fail anyway (the TV is genuinely unreachable, not just half-dead WS).
- The "knock" mechanism doesn't help here because the TV's WS is fully torn down, not just stale — `with_retry`'s reconnect would also fail.
- Going straight to `power_on` skips a guaranteed-failure round-trip.

Stale-WS-but-cached-`ScreenOff` scenarios continue to use the natural `REQUEST_MS` → `with_retry` → escalate path, because the knock is load-bearing there. `ScreenOff` deliberately stays out of `needs_wake_preflight` for exactly this reason.

### Bottom line

Future engineers tempted to "shorten the slow path" should benchmark against Spike A's and B's per-trial numbers before shipping. Anything that bypasses `screen_on` under stale-WS conditions will lose the with_retry knock and end up slower than the pre-fix 13 s baseline. If you have a fix that *preserves* the knock (e.g., explicit synchronous reconnect-then-screen_on), it's worth trying — `SO_KEEPALIVE` with aggressive timing (`TCP_KEEPIDLE=5`, `TCP_KEEPINTVL=2`, `TCP_KEEPCNT=2`) is the only candidate we considered that fits this shape, but its blast radius (every WS on the daemon, fiddly to extract the raw fd from `MaybeTlsStream`) didn't justify proceeding without a stronger signal that the 13 s floor is unacceptable.

The autonomous reproducer used for the spike measurements is at [`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py). It opens an out-of-band SSAP WebSocket via the daemon's existing `client_key`, sends `ssap://system/turnOff` to put the TV in `screen_off`, then dispatches reliable `screen_on` and times `t0 → PowerState == "active"`. Useful for repeat measurement of cold-start dispatch latency from a known TV state without driving the actual desktop lock/unlock flow.

## Reference probe data

The `processing` / `onOff` discriminator behavior was characterized by [`scripts/probe_power_state_timing.py`](../../scripts/probe_power_state_timing.py), which subscribes to a broad set of power/screen endpoints over a single WebSocket and then fires SSAP `turnOff` to measure response timing. Captured wire payloads from the OLED65G4SUB:

```
[+0.000s]  sent turnOff
[+0.118s]  turnOff ack: {"returnValue":true}
[+0.191s]  power/getPowerState
           {"state":"Active","processing":"Request Power Off","onOff":"off","reason":"remoteKey"}
[+0.289s]  power/getPowerState
           {"state":"Active","processing":"Request Active Standby","onOff":"off","reason":"remoteKey"}
[+0.435s]  power/getPowerState
           {"state":"Active","processing":"Prepare Active Standby","onOff":"off","reason":"remoteKey"}
[+3.266s]  power/getPowerState
           {"state":"Active Standby"}
```

Three things worth knowing:

1. **`getPowerState` is the only useful endpoint.** Every other plausible power/screen candidate (`power/getPowerStatus`, `power/getStandbyState`, `screen/getScreenState`, `display/getStatus`, `deviceinfo/getInfo`, `notification/getStatus`) returned 404 / 401 on this firmware. The protocol-shaped fix had to work through the endpoint the daemon already subscribes to.
2. **The `reason` field labels the source.** SSAP `turnOff` sent by the probe was reported back as `"reason":"remoteKey"`, suggesting the LG firmware routes SSAP turnOff through the same internal handler as an IR/BLE remote press. Pushes from a real remote button should look the same on the wire.
3. **Settled state has no discriminators.** Once the TV reaches a steady substate (`"Active Standby"` here at +3.266 s), `processing` and `onOff` drop out of the payload entirely. The state-field-only fall-through in `parse_power_state_payload` handles this case correctly.

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

**Fix.** New `parse_power_state_payload(&Value) -> Option<PowerState>` helper in [`tisvlay-lg/src/connection.rs`](../../crates/tisvlay-lg/src/connection.rs) projects to `ActiveStandby` whenever `onOff == "off"` OR `processing` is present and not `"None"`. Both call sites route through it:

- `update_state_from_subscription` (the subscription push path).
- `WebOsClient::get_power_state` (the synchronous query path used by `tv_manager::refresh_power_state`).

`ActiveStandby` is sufficient to trigger the daemon's `needs_wake_preflight` discriminator, so pre-flight fires within ~200 ms of the remote press instead of waiting ~3 s for the `state` field to catch up.

**Regression coverage.**

- `tisvlay-lg/src/connection.rs` — `subscription_power_transition_to_off_projects_active_standby` (all three captured `processing` values), `subscription_power_on_off_field_alone_projects_active_standby`, `subscription_power_processing_none_falls_through_to_state_field`, and the direct-helper tests `parse_payload_*` pin the contract for both subscription and synchronous-query callers.

## Recent remote-off latches LG firmware against `turnOn` for ~1 s

**Symptom.** When the user pressed Power on the remote and then clicked GUI screen-off within ~1 second, the daemon's pre-flight wake gate fired correctly (`cached=ActiveStandby` detected, `power_on` dispatched, WebSocket reconnected in ~700 ms), but the subsequent `screen_off` command got `500` rejected and verify exhausted with `cached=ActiveStandby` for the entire retry budget. The TV stayed in `ActiveStandby` — panel dark, red LED on, daemon reporting "verify-retry exhausted; panel did not blank" despite the panel being already off.

**Root cause.** When the user's remote-off has just landed (~1 s prior), the LG firmware honors that intent over an incoming `turnOn`/`turnOnScreen` from the network. `power_on` succeeds at the WebSocket layer (reconnect happens, initial subscribe response arrives) but the TV's substate never transitions out of `ActiveStandby` — LG keeps reporting `ActiveStandby` (with `onOff="off"` / `processing` fields) until it eventually settles into deep `Standby` on its own. Wait ~5 s before sending `turnOn` and LG accepts the wake cleanly; click faster and LG won't cooperate. Hardware behavior, not something the daemon can override.

**Fix.** `tv_manager::screen_off` ([crates/tisvlayd/src/tv_manager.rs](../../crates/tisvlayd/src/tv_manager.rs)) adds a post-wake **settle phase** after pre-flight `power_on`: await the cached state leaving the wake-preflight set, bounded by `verify_delay × max_attempts`. Three outcomes:

- Settled to `Active` / `ScreenSaver`: TV woke cleanly. Fall through to verify-retry loop, screen_off lands.
- Settled to `ScreenOff`: LG took the direct path through to the target on wake-up. Return Ok.
- Did not settle (timeout): LG firmware refused the wake. TV sits in `ActiveStandby` with the panel already dark and the red LED on, which is functionally what `screen_off` is asking for. Return Ok with an info log rather than burning more retries on `turnOffScreen` rejections.

The graceful fallback declares success when the panel is demonstrably off, even if LG's substate isn't the literal `ScreenOff` we'd prefer. The two substates differ on the front-panel red LED (on for `ActiveStandby`, off for `ScreenOff`) but the panel is dark in both, and both leave the TV reachable for the next command — functionally indistinguishable from the screen-off-intent perspective.

**Regression coverage.**

- `tisvlayd/tests/dbus_contract.rs` — `screen_off_accepts_active_standby_when_wake_refuses` uses the `MockHandle::set_initial_power_state_override` knob to force every `MockConnection` (including the one `power_on` creates after pre-flight fires) to stay in `ActiveStandby`. Pins that the call returns `Ok` via the graceful-fallback arm without iterating the verify-retry loop (`screen_off_call_count == 0`) and that the TV ends with `power_state == "standby"`.
- Live-hardware smoke remains the secondary check; the info log `"screen_off: TV did not settle into Active after power_on; accepting wake-preflight substate as panel-off (LG firmware quirk)"` is the on-device verification signal.

## Field-test verification trace (post-fix)

A single representative run of [`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py) on the OLED65G4SUB after the Bug A, Bug B, discriminator-parser, and post-wake-settle fixes (n=1; expect a few-hundred-ms wall-clock variance run-to-run depending on TV firmware state and network conditions):

```
[setup] initial PowerState: 'active'
[ssap] turnOff ack
[setup] sleeping 3.0s before SetScreen
[setup] PowerState before SetScreen: 'standby'
[call] Tv1.Screen.SetScreen true
[call] returned Ok in 9.59s
[result] dispatch_latency_seconds=9.79
```

Approximate cost breakdown of the 9.79 s, in dispatch order:

1. **~0 s** — `screen_on` pre-flight reads cached `Standby` and short-circuits to `power_on` (no in-band `screen_on` round-trip).
2. **~1 s** — WoL packet sent, TV firmware begins booting. **The OLED panel physically lights up within this window** — well before the daemon's WS handshake completes. For "TV-as-monitor" UX the user sees output here, long before the 9.79 s ends.
3. **~7-8 s** — TCP+TLS handshake, SSAP register, initial subscribe-state response. This is the LG firmware's WoL → SSAP-server-ready latency; not under daemon control.
4. **~0 s** — initial subscription push lands, cached `PowerState` flips to `"active"`, `state_forwarder` emits `PropertiesChanged`. The measurement ends here.

What `dispatch_latency_seconds` actually measures: "daemon-knows-TV-is-fully-online". Not the same as user-visible "panel-lit"; the panel lights up several seconds earlier. If you need a measurement closer to perceived latency, the right hook is the `Tv1.Power.PowerCycleProgress` D-Bus signal — `Connecting` → `Connected` brackets the SSAP handshake portion; the panel itself lights between `WolSent` and `Connecting`. (Not currently captured by the triage script; a follow-up enhancement.)

Three things worth knowing:

1. **No single load-bearing fix.** The pre-`feat/screen-on-reliable` baseline needed *all* of: Bug A (`ActiveStandby` → `is_screen_on = false`), Bug B (LG `500` vs `-102` discrimination), and the `processing`/`onOff` discriminator parsing. Removing any one of these reintroduces a different silent-success failure mode, traced in the earlier sections.
2. **The WoL recovery path itself is unchanged.** The reliable-dispatch layer's only job is to decide *when* to escalate to `power_on`; once it does, the existing WoL → Registration sequence in `tv_manager::power_on` carries the weight. The ~7-8 s SSAP-handshake portion of the 9.79 s is bounded by LG firmware boot speed.
3. **Verify overhead is an upper bound, not a floor.** The pre-flight wake gate and the verify-retry loops route through `TvManager::await_power_state`, which uses a `tokio::sync::watch` keyed on cached `PowerState` (fired by every subscription push and live `getPowerState` query). Each step returns on the first matching transition, so the `verify_delay × max_attempts` budget rarely cashes in — in the happy path the step completes well under the 300 ms `verify_delay` (most subscription pushes from the LG OLED G4 arrive within tens of milliseconds during steady-state-to-steady-state transitions).

## The 10 s `REQUEST_MS` is load-bearing — don't bypass `screen_on`

**Context.** [`tisvlay-lg/src/endpoints.rs::timeouts::REQUEST_MS`](../../crates/tisvlay-lg/src/endpoints.rs) = `10_000` ms. When the WebSocket goes half-dead (TCP up, TV not replying — observed when the LG firmware drops a WS connection without sending CLOSE), `screen_on` stalls the full 10 s waiting on a reply before erroring out. Smoke testing reproduced this: total wake-from-half-dead-WS lands around 13 s.

The instinctive fix is "shorten the wait" — either by adding a tighter timeout race around `screen_on`, by predicating a short-circuit on WS staleness, or by lowering `REQUEST_MS` directly. We tried all three. **All three regressed.** This section is here to save the next engineer the time.

### Why each bypass loses

The LG client's `with_retry` wrapper ([connection.rs](../../crates/tisvlay-lg/src/connection.rs)) responds to `Error::RequestTimeout` (i.e., `REQUEST_MS` expiring) by tearing down the current WebSocket, opening a fresh one with `register`, and retrying the command. That fresh-WS handshake **physically rouses the TV** — by the time the second attempt also fails and the dispatch escalates to `power_on`, the TV is already warm enough that the subsequent WoL → Registration happens in <1 s.

Any approach that *bypasses* `screen_on` to avoid the 10 s wait skips this accidental knock. The TV slips deeper into standby, and full WoL recovery now takes 9-12 s instead of <1 s. The arithmetic: ~7-10 s saved on the bypassed wait, ~9-12 s lost on slower recovery. Net pessimization in every case.

### Spike data (V1-era measurements; conclusion still applies)

Three bypass attempts. Each was implemented during the original `feat/action-qos` spike (V1 architecture: only `screen_on` was reliable, gate was on cached `"standby"` only, no `processing`/`onOff` parsing yet), smoke-tested on real hardware, then reverted. The spike commits are archived on the `feat/action-qos` branch for the design-exploration record.

The per-trial numbers below are pre-V2; the V2-era architecture still routes the in-band `screen_on` attempt through `with_connection` → backend `with_retry`, so the "knock" mechanism that costs each bypass its win is intact. If you re-measure on V2, expect the same qualitative shape (each bypass pessimizes the bad path) even if the absolute numbers shift. Re-running [`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py) is the quickest way to validate.

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

### The pre-flight wake gate is the only bypass that survives

The one short-circuit that *does* belong is the pre-flight wake gate in `tv_manager::screen_on` / `screen_off`. V1 keyed it on cached `"standby"` only; V2 expanded the gate to the full [`needs_wake_preflight`](../../crates/tisvlayd/src/power_state.rs) set — `Standby` / `Suspend` / `ActiveStandby` / `Unknown`. When cached is one of those:

- `Standby` / `Suspend`: TV is genuinely unreachable, `screen_on` would fail anyway and the "knock" doesn't help because the WS is fully torn down (not just stale) — `with_retry`'s reconnect would also fail. Going straight to `power_on` skips a guaranteed-failure round-trip.
- `ActiveStandby`: transitional substate after a remote-press power-off. LG firmware ACKs `screen_on` at the wire but won't actually transition the panel out of `ActiveStandby`; the with_retry knock has nothing to recover from (WS is alive). `power_on` is the only path that takes the TV out of `ActiveStandby`.
- `Unknown`: no subscription push received yet; safest to treat as off.

Stale-WS-but-cached-`ScreenOff` scenarios continue to use the natural `REQUEST_MS` → `with_retry` → escalate path, because the knock is load-bearing there. `ScreenOff` deliberately stays out of `needs_wake_preflight` for exactly this reason.

### Bottom line

Future engineers tempted to "shorten the slow path" should reproduce on V2 hardware with `scripts/triage_dispatch_perf.py` before shipping; the V1 spike numbers above tell you the *shape* of the regression (bypass wins on the wait, loses on recovery), not the V2 absolute numbers. Anything that bypasses `screen_on` under stale-WS conditions will lose the with_retry knock and pessimize the bad-path total. If you have a fix that *preserves* the knock (e.g., explicit synchronous reconnect-then-screen_on), it's worth trying — `SO_KEEPALIVE` with aggressive timing (`TCP_KEEPIDLE=5`, `TCP_KEEPINTVL=2`, `TCP_KEEPCNT=2`) is the only candidate we considered that fits this shape, but its blast radius (every WS on the daemon, fiddly to extract the raw fd from `MaybeTlsStream`) didn't justify proceeding without a stronger signal that the current floor is unacceptable.

The autonomous reproducer used for the spike measurements is at [`scripts/triage_dispatch_perf.py`](../../scripts/triage_dispatch_perf.py). It opens an out-of-band SSAP WebSocket via the daemon's existing `client_key`, sends `ssap://system/turnOff` to put the TV in `screen_off`, then dispatches reliable `screen_on` and times `t0 → PowerState == "active"`. Useful for repeat measurement of cold-start dispatch latency from a known TV state without driving the actual desktop lock/unlock flow.

## Post-WoL input-switch timing (unmeasured)

The primary-input restore (`DesiredInput`) fires a `switch-input` in the `Connected` arm after a WoL wake, then verifies with the same bounded retry as `screen_on`. How soon after `Connected` SSAP actually *accepts* an input switch is unmeasured — the 1–5 s WoL-settle window means a switch fired at `Connected` can be silently dropped, exactly like an early `screen_on`. The shipped verify-retry already absorbs this, so this bounds the retry *budget*, not the design. Measure with the same technique as the dispatch-latency spikes (out-of-band SSAP over the daemon's `client_key`): after a WoL wake, time `Connected → first accepted input switch`. Trigger: a report of the input not landing on primary after a wake, or a retry-budget tune.

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

## IPv6 SSDP non-participation — TVs are silent on `ff02::c`

**Symptom.** After enabling Settings → Network → IPv6 on the TV and exposing it on a SLAAC-configured LAN, the daemon's SSDP discovery returns zero TVs even though SSAP control over IPv6 works fine and the TV is reachable on its ULA (`ping6`, neighbor cache, full openssl handshake to `[<tv-ula>]:3001`).

**Finding.** LG TVs (OLED65G4SUB verified; OLED42C3PUA expected equivalent per the matching SSAP firmware family) **do not join any v6 SSDP multicast group**. Not `ff02::c` (link-local), not `ff05::c` (site-local), not `ff0e::c` (global). They neither NOTIFY on the v6 groups nor answer M-SEARCH on them. SSAP over IPv6 is the only v6 control path the TV exposes.

**Capture protocol.** Probe both passively and actively on the interface the TV is reachable on (substitute `$IFACE`):

```bash
# Passive: watch all v6 SSDP traffic for 1–2 minutes.
# Power-cycling the TV during this window forces a re-NOTIFY burst
# from anything that joined the group.
sudo tcpdump -i "$IFACE" -nn 'ip6 and udp port 1900'

# Active: M-SEARCH all scopes. Use socat (or nix run nixpkgs#socat)
# rather than nc -6 -u, which is variant-dependent for v6 multicast.
printf 'M-SEARCH * HTTP/1.1\r\nHOST: [ff02::c]:1900\r\nMAN: "ssdp:discover"\r\nST: ssdp:all\r\nMX: 2\r\n\r\n' \
    | socat - "UDP6-DATAGRAM:[ff02::c%$IFACE]:1900"
```

**Captured evidence (representative).** On a test network with two paired LG TVs, the only v6 SSDP traffic seen comes from the LAN's home-gateway UPnP IGD (`MiniUPnPd` is the standard UPnP daemon shipped on most consumer router firmware). Sanitized response shape:

```
<gateway-link-local>.1900 > <host>.<eph>: HTTP/1.1 200 OK
  SERVER: <linux-kernel> UPnP/1.1 MiniUPnPd/<version>
  ST: urn:schemas-upnp-org:device:InternetGatewayDevice:2
  USN: uuid:<gateway-uuid>
  LOCATION: http://[<gateway-ula>]:<port>/rootDesc.xml
```

The router announces the full IGD service catalog (`InternetGatewayDevice`, `WANDevice`, `WANConnectionDevice`, `Layer3Forwarding`, `WANIPConnection`, `WANPPPConnection`, `WANCommonInterfaceConfig`, `WANIPv6FirewallControl`, plus the `upnp:rootdevice` and per-UUID anchors). The `WANIPv6FirewallControl` entry is the most plausible false positive — but it's still the gateway's own IGD profile, nothing to do with LG. Zero responses ever come from either TV's v6 address despite both being v6-reachable on the same L2 segment.

**Implications for the codebase.**

- `tisvlay-ssdp` stays v4-only by design. `Error::Ipv6BindUnsupported` is a deliberate hard reject, not "we haven't gotten around to it" — see the variant's doc comment for the cross-reference back here.
- The `Capabilities::IPV6` bit on the LG backend means *control* over v6, not *discovery*. The two are independent and have to be evaluated separately for every vendor backend.
- A future vendor that does join `ff02::c` (e.g., a hypothetical Sony/Samsung backend that follows the UPnP spec more literally) is what would justify lifting `Ipv6BindUnsupported` and threading v6 through `tisvlay-ssdp`. Until then, the v4-only stance is correct.
- Toggling other TV settings that *sound* related — DLNA, "TV On via Mobile", LG ConnectApp — does not flip this behavior on G4 firmware as of 2026-05. Could change with future firmware; rerun the capture protocol if a new revision claims UPnP-over-v6.

## `connectionmanager` reports each NIC's MAC and which one carries the TV's IP

**Finding.** Two SSAP calls together identify the TV's attached interface and its MAC. Verified on the OLED42C3PUA over the production client and manifest (which requests `READ_NETWORK_STATE`); the OLED55G3PUA shares the firmware family and is expected equivalent.

`ssap://com.webos.service.connectionmanager/getinfo` returns one MAC per interface and nothing else:

```json
{"returnValue":true,"subscribed":false,
 "wiredInfo":{"macAddress":"64:E4:A5:6C:AB:B4"},
 "wifiInfo":{"macAddress":"84:90:0A:63:A9:94"},
 "p2pInfo":{"macAddress":"86:90:0A:63:A9:94"}}
```

`ssap://com.webos.service.connectionmanager/getstatus` (lowercase; the camel-case `getStatus` returns 404) returns per-interface link state and addressing (abridged):

```json
{"returnValue":true,"isInternetConnectionAvailable":true,
 "wifi":{"state":"connected","interfaceName":"wlan0","ipAddress":"192.168.4.170",
         "netmask":"255.255.255.0","gateway":"192.168.4.1","method":"dhcp",
         "ssid":"<ssid>","isWakeOnWifiEnabled":false},
 "wired":{"plugged":false,"state":"disconnected"},
 "wifiDirect":{"state":"disconnected"}}
```

The attached NIC is the interface whose `state` is `connected` and whose `ipAddress` matches the address the daemon dials; its MAC comes from the matching `getinfo` block (`wired` → `wiredInfo`, `wifi` → `wifiInfo`). `p2pInfo` is Wi-Fi Direct and never a WoL target. Probed siblings that don't exist on this firmware: `getStatus`, `wired/getstatus`, `wifi/getstatus`, `getNetworkInfo`, `getinterfaceinfo`, `getIpInfo` (all 404).

**Implications.**

- It works for a TV off the daemon's link (a routed IoT VLAN, where ARP only ever sees the gateway), with no guessing between NICs.
- `wifi.isWakeOnWifiEnabled` is the TV's own "turn on via Wi-Fi" setting. When it's `false`, a Wi-Fi-attached TV ignores magic packets, so WoL can't work whatever MAC is configured. The OLED42C3PUA above is in exactly that state. A Wi-Fi TV with this off can't be woken from full standby.

**Reproduce.** The ignored `field_probe` test in `crates/tisvlay-lg/src/client.rs` queries both endpoints with a paired TV's stored address and client key; the run command is in its doc comment, and `TISVLAY_PROBE_URIS` overrides the endpoint list.

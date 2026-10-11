# Triage script: capture PowerCycleProgress phases for panel-lit measurement

`scripts/triage_dispatch_perf.py` measures cold-start dispatch latency by timing `t0 → PropertiesChanged → PowerState == "active"`. On the LG OLED G4 this lands around 9.79 s after `Tv1.Screen.SetScreen(true)` from `Standby`.

The captured number is **daemon-knows-TV-is-fully-online**, not user-visible **panel-lit**. The OLED physically lights up well before the daemon's SSAP handshake completes — typically within ~1 s of WoL — but the script can't see it because it only watches the property cache, and the property cache only updates after the daemon's initial subscribe-state response lands (~7-8 s into the timeline). For "TV-as-monitor" UX the user is looking at the screen for ~6-7 s while the script thinks the dispatch hasn't finished.

The daemon already emits the right signal for a closer-to-perceived measurement: `Tv1.Power.PowerCycleProgress(s)` fires through phases `WolSent` → `Connecting` → `Connected` (or `DirectConnect` → `Connecting` → `Connected` on the warm-WS path; `Failed` on either). The panel lights between `WolSent` and `Connecting`.

## Path forward

Enhance `triage_dispatch_perf.py` to subscribe to `PowerCycleProgress` in addition to `PropertiesChanged`:

```python
# In _run_inner, alongside the existing wait_for_active task:
power_cycle_waiter = asyncio.create_task(
    wait_for_phases(object_path, ["wol_sent", "connecting", "connected"], args.timeout)
)
```

Where `wait_for_phases` is structurally identical to `wait_for_active` but listens for `Tv1.Power.PowerCycleProgress` signals via `busctl monitor` (or a proper `dbus_next` / `pydbus` subscriber) and records the elapsed time at each named phase.

Output one line per phase:

```
[result] phase=wol_sent          elapsed=0.05s
[result] phase=connecting        elapsed=1.02s    ← panel ~here
[result] phase=connected         elapsed=7.84s
[result] dispatch_latency_seconds=9.79
```

The `connecting` phase is the closest proxy we have to panel-lit on the WoL path without an external optical sensor.

## Caveats

- The phase timestamps are still daemon-side observations of LG firmware state, not direct panel-light measurement. There's still a small gap between when LG's WS becomes responsive (~`Connecting`) and when the panel actually shows pixels. Empirically the gap is small (sub-second) but it's not zero.
- The `DirectConnect` path (TV already reachable, WS just dropped) won't have `WolSent`. Code path needs both shapes.
- For the bad-path scenarios where the daemon's `with_retry` loop fires inside `screen_on` before escalation, no `PowerCycleProgress` events are emitted — that signal only emits during the `power_on` flow. Manual `iptables`-drop-style reproducers won't benefit.

## Trigger condition

Pick this up when:

- Someone (probably the GUI's wake-progress UX) wants a more user-perceived latency number than `dispatch_latency_seconds`.
- A performance regression suspected in the LG `with_retry` knock or WoL retry budget needs phase-by-phase visibility to localize.
- A second TV model is paired and the LG firmware-boot-to-SSAP-ready timing differs meaningfully (then per-phase numbers tell us whether to retune `RetryConfig` or wait on the firmware).

## Related

- Triage script today: [scripts/triage_dispatch_perf.py](../../scripts/triage_dispatch_perf.py).
- The `progress/power` phases: `tisvlay_wire::payload::PowerPhase` in [crates/tisvlay-wire/src/lib.rs](../../crates/tisvlay-wire/src/lib.rs).
- Why bypassing `screen_on` to "shorten the slow path" is the wrong instinct: [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md) "The 10 s `REQUEST_MS` is load-bearing" section.

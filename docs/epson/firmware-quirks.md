# Epson firmware quirks

The LG analog of this doc ([docs/lg/firmware-quirks.md](../lg/firmware-quirks.md)) is a *grown* inventory: each entry is a behavior that bit us on real hardware, captured as Symptom → Root Cause → Fix → Regression Coverage. **This doc is a stub** — `tisvlay` has no Epson hardware yet, so it instead seeds the **known-unknowns as explicit open questions**, framed in the same shape but with the Symptom / Root Cause / Fix cells written as "open — to confirm on hardware." Each fills in once an Epson Pro Cinema LS12000 is on the bench (the hardware-validation follow-up); until then, the backend behaves as the [protocol reference](protocol.md) describes, with the v1 assumptions called out below.

For the ESC/VP21 wire protocol and the command vocabulary, see [protocol.md](protocol.md). For the fake-server test infrastructure see [testing.md](testing.md).

## Open: are `PWR?` `02` / `03` directionally distinct, or is it `02`-for-both?

**Symptom (anticipated).** A passively-warming projector (powered on by its own remote) and a passively-cooling one might report the *same* busy `PWR?` code, so a state read can't tell warmup from cooldown by the code alone.

**Root cause (open).** The Python reference library that informed the design only recognizes `02` as a *generic busy* code. Whether the LS12000 firmware distinguishes `02` (warming) from `03` (cooling), or reports one indistinct busy code for both directions, is unconfirmed.

**Fix (shipped, pending validation).** The backend handles both cases without needing to know which is true: a directionally-distinct code wins outright, and an indistinct busy code (classified `Unknown`) is resolved against the last `PWR ON` / `PWR OFF` the client sent (the tracked `PowerIntent`). See [protocol.md § Power-code state machine](protocol.md#power-code-state-machine) and [`escvp21::power_state_from`](../../crates/tisvlay-epson/src/escvp21.rs). The intent inference is *unfalsifiable without hardware* — it is wired and tested, but its correctness against real firmware is exactly this open question.

**Regression coverage.** The codec table tests pin both branches (`power_state_distinct_codes_win`, `power_state_indistinct_busy_uses_intent` in `escvp21.rs`), and the connection-level `intent_disambiguates_warmup_vs_cooldown` integration test drives the indistinct-busy path end-to-end against `FakeEpsonServer`. **To confirm on hardware:** probe `PWR?` during a real warmup and a real cooldown and record the codes; if they are distinct, the intent fallback becomes belt-and-suspenders rather than load-bearing.

## Open: the `LUMLEVEL` range and units

**Symptom (anticipated).** The backlight slider maps to a 0–100 percentage, but `LUMLEVEL` may use a different scale (e.g. 0–255, or a discrete step set), so a read-back may not match what was set and the slider may not span the projector's real range.

**Root cause (open).** The real `LUMLEVEL` range and units are undocumented in the sources consulted.

**Fix (shipped, pending validation).** v1 treats the value as a 0–100 percentage, clamping a larger reading down to 100 (`EpsonClient::get_backlight` / `set_backlight`). If hardware shows a different range, the mapping moves into the codec as a scale conversion.

**Regression coverage.** `backlight_set_get_via_lumlevel` pins the set/get round-trip and the clamp against `FakeEpsonServer`. **To confirm on hardware:** read `LUMLEVEL?` at the projector's min and max brightness and record the bounds.

## Open: WoL + Communication-On standby/wake behavior and timing

**Symptom (anticipated).** A `wake_and_connect` may fail or be slow if the magic packet doesn't wake the projector, or if the projector's NIC takes longer than the connect-retry budget to come up after deep standby.

**Root cause (open).** The wake path assumes **Standby Mode = Communication On** (required for any network reachability in standby; see [protocol.md § Standby Mode](protocol.md#standby-mode--communication-on)) and a WoL magic packet for deep standby. The real wake latency, whether WoL alone suffices versus needing a network `PWR ON`, and the warmup duration are all unmeasured.

**Fix (shipped, pending validation).** `EpsonConnector::wake_and_connect` sends the magic packet (gated on a configured `mac`), retries the send while the local NIC recovers, retries the connect across a tens-of-seconds window, then drives `PWR ON` and **polls `PWR?` through warmup to `Active` before returning** — so action chains (`on_startup = power_on → switch_input`) don't fire while the laser is still warming and no usable image is projected. The connect-retry count and warmup-poll timeout are first-cut constants in [`connector.rs`](../../crates/tisvlay-epson/src/connector.rs).

**Regression coverage.** `wake_without_mac_is_noop_ok` and `supports_wake_gated_on_mac` pin the gating; `power_on_reports_warming_then_active` pins the warmup-poll progression against a scripted `FakeEpsonServer`. **To confirm on hardware:** measure WoL-to-reachable and reachable-to-`Active` latencies on a cold projector and tune `WAKE_CONNECT_MAX_ATTEMPTS` / `WARMUP_POLL_TIMEOUT` and `verify_retry_config` from the measured timings.

## Open: does the v2.00+ Web API offer any capability edge worth a second transport?

**Symptom (anticipated).** None functional — this is a "should we add a second transport" question, not a bug.

**Root cause (open).** The LS12000 gained a Web Control browser UI and a programmatic Web API in firmware v2.00 (maintained through v3.04). Research found no confirmed capability edge over TCP 3629, but that was not validated on hardware. The Web API's one theoretical advantage is stateless request-per-command HTTP, which would survive a client source-IP change (TCP 3629's persistent socket does not — `survives_source_ip_change()` is `false`).

**Fix (deferred).** TCP 3629 is the v1 transport; the Web API and PJLink are documented as future transports behind the command/transport boundary (see [protocol.md § Transport landscape](protocol.md#transport-landscape-dated)). No code today.

**Regression coverage.** N/A until a second transport lands. **To confirm on hardware:** enumerate the Web API surface on v3.04+ and compare its reachable controls against ESC/VP21; decide whether IP-change resilience justifies the added transport.

## Open: lamp / laser-hours query availability

**Symptom (anticipated).** No place to surface laser-life today — there is no `TvState` slot for it.

**Root cause (open).** ESC/VP21 exposes lamp/laser-hours, but whether the LS12000 answers the query and in what units is unconfirmed, and the daemon has no state field to carry it.

**Fix (deferred).** Filed as the [projector lamp/laser-hours diagnostic idea](../ideas/projector-lamp-hours-diagnostic.md). A future PJLink transport would expose lamp-hours natively.

**Regression coverage.** N/A. **To confirm on hardware:** probe the ESC/VP21 lamp-hours query and record the response shape and units, to inform the diagnostic-surface design.

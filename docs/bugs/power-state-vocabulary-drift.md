# GUI power-state gate misses the wire's new power-state spellings

**Status:** closed (2026-07-04). `power_state_str_is_on` now delegates to `PowerState::parse(...).is_on()`, so the string gate and the enum share one classification; a table test pins every serde wire spelling against `is_on`.

`TvInfo::power_state` now carries the serde `lowercase` serialization of `tisvlay_core::PowerState` — `"screenoff"`, `"warmingup"`, `"coolingdown"`, `"activestandby"` — but `tisvlay_core::power_state_str_is_on` (consumed by the GUI's TVs pane) still blocklists the retired collapsed codes `"standby" | "unknown" | "cooling_down"`. Consequence: `"coolingdown"` and `"activestandby"` fall through the blocklist and read as *on*.

Fix direction needs a decision first: either align the wire vocabulary back to the collapsed root codes (touches the `state/power` payload consumers) or teach `power_state_str_is_on` the full lowercase `PowerState` set (localized fix; blocklist grows `"coolingdown"`, `"activestandby"`, arguably `"warmingup"` belongs on the *on* side as root treated warming as on). Whichever way, add a table test pinning every `PowerState` variant's spelling to the expected on/off verdict so serde-rename drift can't silently reopen this.

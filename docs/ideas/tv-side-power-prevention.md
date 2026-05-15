# TV-side power prevention

Deferred from the [Action QoS plan](../plans/action-qos.md) when v1 shipped client-side reliability only. This file captures the design discussion so it isn't re-derived if and when the prevention path un-defers.

## Why this is deferred

Writing Luna persistent settings (`automaticStandBy = "Off"` and friends) would be tisvlay's **first persistent TV-setting write**. Every existing `Tv1.*` setter is ephemeral (volume, input, screen-on/off — runtime state the TV forgets on reboot or that the next remote button-press changes). The persistent-settings category has a long tail (TV input names, parental controls, NTP, energy mode, no-signal screensaver, network settings…) and committing to managing it deserves more architectural deliberation than v1 allowed.

v1 ships client-side reliable only, which handles the symptom (TV slipped past Screen Off → WoL-escalate to power-on). If field use shows the symptom is common enough to warrant the prevention path, revisit with option B below.

## Failure-mode taxonomy

| Failure mode | Mechanism | Luna key |
|---|---|---|
| Idle auto-off | TV counts "no user input" hours even in Screen Off; transitions to Power Off | `category="time"`, `automaticStandBy` (hours; 0 = Off) |
| No-signal off | HDMI handshake loss → No-Signal screensaver → eventual power off | `category="general"`, `noSignalScreenSaver` (on/off) |
| Fixed 2-hour timer | Distinct 2-hour countdown on some firmware | `category="time"`, `autoOff2HourOnTimer` (on/off) |
| Hidden firmware descent | "Screen Off → Active Standby" timer with no user-facing knob | — (no key; client-side reliable handles this) |

The first row (`automaticStandBy`) is the dominant cause in field reports. The hidden-firmware row has no Luna remedy and is precisely what the client-side `reliable` path exists for — so even if TV-side prevention never lands, the worst cases are still covered.

## Three commitment levels considered

**A — Defer entirely.** Ship client-side reliable only in v1. Document "set your TV's Auto Power Off to Off via the remote" as a manual prerequisite. ← **Chosen for v1.**

**B — Opaque "configure for management" capability.** Single trait method `TvConnection::configure_for_management() -> Result<ConfigSnapshot, Error>`. LG impl writes whatever Luna keys it decides (`automaticStandBy = "Off"`, `wolwowlOnOff = "true"`, possibly more) — implementation details, not surfaced in tisvlay's TOML or D-Bus. Triggered by `managed = true` on pair, with a GUI "Sync TV settings" button for re-running on demand. Snapshot original Luna values pre-write; `TvConnection::restore_pre_managed_settings(snapshot)` reverses on un-manage. ← **Expected design when this is un-deferred.**

**C — Full TV-settings-management category.** Explicit `[tv.power] auto_off_hours = N` field with read-on-connect, drift detection, "(out of sync)" GUI badge. Commits tisvlay to being a TV-settings-manager. Each future setting becomes a new field. Future-flexible but bigger ongoing surface. ← **Likely overkill for the immediate need; revisit only if multiple TV settings turn out to need explicit knobs.**

## Implementation sketch for the eventual un-deferral

When option B is picked up:

- Add `Capabilities::CONFIGURE_AUTO_OFF` bit (NOT in v1 — bundled with the LG impl below to avoid orphan plumbing).
- Add `TvConnection::configure_for_management(&mut self) -> Result<ConfigSnapshot, Error>` — returns a snapshot of pre-change values for later restoration.
- Add `TvConnection::restore_pre_managed_settings(&mut self, snapshot: &ConfigSnapshot) -> Result<()>`.
- LG impl: write `automaticStandBy = "Off"`, ensure `wolwowlOnOff = "true"`, snapshot prior values.
- Pairing wizard step (optional checkbox): "Configure this TV for management?" — explains what gets changed, links to per-backend docs enumerating the settings.
- GUI: per-TV "Sync TV settings" affordance for re-running on demand.
- Persistent snapshot stored in TV's `[tv.*]` block under a managed-by-tisvlay sub-table (e.g., `[tv.managed_snapshot]`), so restoration survives daemon restarts.
- Per-backend doc in `docs/lg/managed-settings.md` lists exactly what changes when management is enabled.

## Note on bscpylgtv quirk

CLAUDE.md notes that direct `setSystemSettings` returns 401 for some categories (specifically picture/backlight, hence the Luna `createAlert` workaround for that path). For `category="time"` (where `automaticStandBy` lives) and `category="general"` (where `noSignalScreenSaver` lives), bscpylgtv calls `setSystemSettings` directly without the workaround — implying these categories don't have the 401 issue. Worth verifying empirically against the user's specific firmware (LG OLED C3 2023, G4 2024) when the prevention path is un-deferred.

## Related

- Action QoS plan (client-side reliability path that ships in v1): [docs/plans/action-qos.md](../plans/action-qos.md)
- Event-driven power-management redesign (broader power-state work): [docs/ideas/event-driven-power-management.md](event-driven-power-management.md)

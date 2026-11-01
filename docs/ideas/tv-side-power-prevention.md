# TV-side power prevention

Deferred at the point that inherently-reliable `screen_on` / `screen_off` shipped client-side. This file captures the design discussion so it isn't re-derived if and when the prevention path un-defers.

## Why this is deferred

Writing Luna persistent settings (`automaticStandBy = "Off"` and friends) would be tisvlay's **first persistent TV-setting write**. Every existing setter rpc is ephemeral (volume, input, screen-on/off — runtime state the TV forgets on reboot or that the next remote button-press changes). The persistent-settings category has a long tail (TV input names, parental controls, NTP, energy mode, no-signal screensaver, network settings…) and committing to managing it deserves more architectural deliberation than the initial reliability work allowed.

The inherently-reliable dispatch path handles the symptom client-side (TV slipped past Screen Off → WoL-escalate to power-on). If field use shows the symptom is common enough to warrant the prevention path, revisit with option B below.

## Failure-mode taxonomy

| Failure mode | Mechanism | Luna key |
|---|---|---|
| Idle auto-off | TV counts "no user input" hours even in Screen Off; transitions to Power Off | `category="time"`, `automaticStandBy` (hours; 0 = Off) |
| No-signal off | HDMI handshake loss → No-Signal screensaver → eventual power off | `category="general"`, `noSignalScreenSaver` (on/off) |
| Fixed 2-hour timer | Distinct 2-hour countdown on some firmware | `category="time"`, `autoOff2HourOnTimer` (on/off) |
| Hidden firmware descent | "Screen Off → Active Standby" timer with no user-facing knob | — (no key; client-side reliable handles this) |

The first row (`automaticStandBy`) is the dominant cause in field reports. The hidden-firmware row has no Luna remedy and is precisely what the client-side `reliable` path exists for — so even if TV-side prevention never lands, the worst cases are still covered.

## Three commitment levels considered

**A — Defer entirely.** Ship inherently-reliable dispatch client-side only. Document "set your TV's Auto Power Off to Off via the remote" as a manual prerequisite. ← **Currently chosen.**

**B — Opaque "configure for management" capability.** Single trait method `TvConnection::configure_for_management() -> Result<ConfigSnapshot, Error>`. LG impl writes whatever Luna keys it decides (`automaticStandBy = "Off"`, `wolwowlOnOff = "true"`, possibly more) — implementation details, not surfaced in tisvlay's TOML or on the wire. Triggered by `managed = true` on pair, with a GUI "Sync TV settings" button for re-running on demand. Snapshot original Luna values pre-write; `TvConnection::restore_pre_managed_settings(snapshot)` reverses on un-manage. ← **Expected design when this is un-deferred.**

**C — Full TV-settings-management category.** Explicit `[tv.power] auto_off_hours = N` field with read-on-connect, drift detection, "(out of sync)" GUI badge. Commits tisvlay to being a TV-settings-manager. Each future setting becomes a new field. Future-flexible but bigger ongoing surface. ← **Likely overkill for the immediate need; revisit only if multiple TV settings turn out to need explicit knobs.**

## Implementation sketch for the eventual un-deferral

When option B is picked up:

- Add `Capabilities::CONFIGURE_AUTO_OFF` bit — bundle with the LG impl below to avoid orphan plumbing.
- Add `TvConnection::configure_for_management(&mut self) -> Result<ConfigSnapshot, Error>` — returns a snapshot of pre-change values for later restoration.
- Add `TvConnection::restore_pre_managed_settings(&mut self, snapshot: &ConfigSnapshot) -> Result<()>`.
- LG impl: write `automaticStandBy = "Off"`, ensure `wolwowlOnOff = "true"`, snapshot prior values.
- Pairing wizard step (optional checkbox): "Configure this TV for management?" — explains what gets changed, links to per-backend docs enumerating the settings.
- GUI: per-TV "Sync TV settings" affordance for re-running on demand.
- Persistent snapshot stored in TV's `[tv.*]` block under a managed-by-tisvlay sub-table (e.g., `[tv.managed_snapshot]`), so restoration survives daemon restarts.
- Per-backend doc in `docs/lg/managed-settings.md` lists exactly what changes when management is enabled.

## Prior art

Surveyed during the inherently-reliable dispatch design. Useful as a starting point for the un-deferred path or for future vendor backends with similar wake-recovery problems.

- **LG_Buddy** (Rust, Linux session service). Two-tier wake: try `turn_screen_on` first; on error `-102` (TV rejects unblank because it's not in screen-off substate) escalate to a full WoL+retry loop (6 attempts, 2-30 s backoff, ~48 s worst case). Uses session-scoped marker files to track whether *it* blanked the screen, so it knows whether to attempt restore. Conservative-vs-aggressive policy: conservative requires its own marker; aggressive will restore even if another device powered the TV on. Input-aware fallback on screen-off: if the TV is on a non-HDMI source, considers blank successful without escalating to power-off.

- **lgpowercontrol** (Bash, Linux event-driven). 5-retry / 3 s loop on `turn_screen_off`; single-attempt `turn_screen_on` after 1 s post-WoL wait (LG_Buddy says 1 s is empirically too short — they wait 6 s). State-aware: queries `get_power_state` before any dispatch, short-circuits if already in target state. Two-mode config (`POWER_MODE=screen|power` and `BOOT_SHUTDOWN_MODE=power|screen`) — independent policies for idle versus boot/shutdown. README explicitly notes hibernate/suspend unsupported because the network drops before commands complete.

- **LGTVCompanion** (C++, Windows service). Power-on timeout is user-configurable (5-100 s, default 40 s). Parallel WoL flood + TCP retry every 1-2 s for the entire timeout window. Pre-WoL transient ARP entry so MAC resolution survives the TV's network silence (Linux equivalent via `ip neighbor` would translate). HDMI-input guard: `CheckHdmiInputWhenPoweringOff` skips power-off if the TV's foreground input doesn't match the PC. Per-device WoL strategy (broadcast / unicast / subnet broadcast / auto). Reads `getPowerState` to decide whether to send `unblank` (→ "Screen Off") versus `toggle` (→ "Active Standby"). User Idle Mode (blank screen) is wired separately from System Suspend (power off) — distinct policy per trigger.

- **Python libs** (`bscpylgtv`, `aiowebostv`, `LGWebOSRemote`). All expose `turnOffScreen` with `{"standbyMode": "active"}` (passive variant documented but not recommended). All subscribe to `getPowerState` and surface the explicit "Screen Off" state. `bscpylgtv` exposes the TV's own auto-power-off knobs via Luna `setSystemSettings`: `automaticStandBy` (hours; 0 = Off), `autoOff2HourOnTimer`, `noSignalScreenSaver`, `wolwowlOnOff`. None of the three libraries implements WoL-on-wake fallback themselves — that logic always lives in the application layer.

## Note on bscpylgtv quirk

CLAUDE.md notes that direct `setSystemSettings` returns 401 for some categories (specifically picture/backlight, hence the Luna `createAlert` workaround for that path). For `category="time"` (where `automaticStandBy` lives) and `category="general"` (where `noSignalScreenSaver` lives), bscpylgtv calls `setSystemSettings` directly without the workaround — implying these categories don't have the 401 issue. Worth verifying empirically against the user's specific firmware (LG OLED C3 2023, G4 2024) when the prevention path is un-deferred.

## Related

- LG firmware quirks (Bug A, Bug B, the `processing` / `onOff` discriminators, post-wake settle, `REQUEST_MS`-is-load-bearing): [docs/lg/firmware-quirks.md](../lg/firmware-quirks.md)
- Event-driven power-management redesign (broader power-state work): [docs/ideas/event-driven-power-management.md](event-driven-power-management.md)
- Reliable screen-state actions: [architecture.md](../architecture.md) (`DispatchVerify`)

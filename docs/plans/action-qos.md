# Action QoS — reliability tier for screen / power dispatch

## Motivation

Today the session service dispatches each TV action (`screen_off`, `screen_on`, `power_on`, `power_off`) in a single fire-and-forget call. The default user-profile maps `on_session_lock → screen_off` and `on_session_unlock → screen_on`, which gives a fast unblank when it works — but newer LG firmware may transition the TV from "Screen Off" to "Active Standby" or full "Power Off" on its own after some interval. Once the TV has slipped to a deeper power state, a follow-up `turnOnScreen` call is unreliable: it returns `-102` ("not in screen-off substate"), times out, or silently succeeds without actually waking the panel. The user is then staring at a dark TV after unlocking the desktop.

The proposal is a **QoS knob** on TV actions: let users declare which actions should be _reliable_ (verify state, retry, escalate to WoL+power-on if the TV has slipped past screen-off) versus _best-effort_ (today's one-shot dispatch). Reliable mode has higher worst-case latency but won't leave the desktop with a dark panel; best-effort keeps the ~100 ms snap-back when the TV is well-behaved.

## V1 scope

**In this PR:**

- Per-event nested `qos.reliability` field in profile config (TOML + serde + merge + resolve).
- Asymmetric default driven by action type (`screen_on` / `power_on` → reliable; `screen_off` / `power_off` → best-effort).
- Backward-compat with bare-string action config (`on_session_unlock = "screen_on"` keeps parsing).
- Client-side `reliable` dispatch helper: pre-action PowerState snapshot, verify post-action, retry, escalate `screen_on` → `power_on` with WoL, bounded by overall timeout.
- Read-only D-Bus property on `Tv1` exposing the resolved per-action reliability tier.
- `TvBackend` capability bit + trait method stub for future TV-side prevention. No implementation in v1; the stub exists so a future PR can plug in the LG implementation without API churn.

**Deferred to follow-up:**

- **TV-side prevention** (Luna `automaticStandBy` and friends) — see [Deferred — TV-side prevention](#deferred--tv-side-prevention) below. Slated for archival to `docs/ideas/tv-side-power-prevention.md` when this PR lands.
- Per-TV reliability override (today: profile-level only).
- Tunable retry count / overall timeout (today `reliable` is an opaque bundle; will become `qos.deadline` and friends).
- Additional QoS dimensions: `deadline`, `durability` (the namespace is ready; the fields aren't).
- GUI ghost-text behavior for unset reliability values — a small M4 detail; reasonable defaults will land with the Profiles-pane work.

## What tisvlay has today

- `SessionAction` enum in [config.rs:69-81](../../crates/tisvlay-core/src/config.rs) — `None | ScreenOff | ScreenOn | PowerOff | PowerOn`. No QoS attribute.
- Profile-level event mapping in `SystemProfileConfig` and `UserProfileConfig` — `on_*` fields, no sidecar metadata.
- LG `screen_off` / `screen_on` already idempotent at [client.rs:367-404](../../crates/tisvlay-lg/src/client.rs) (500 treated as success).
- `power_on` has WoL + retry logic in [tv_manager.rs:1394-1522](../../crates/tisvlayd/src/tv_manager.rs) — but the equivalent escalation does NOT exist for `screen_on`.
- Session-service `event_handler` in [lib.rs:286-359](../../crates/tisvlay-session/src/lib.rs) is fire-and-forget; tracks `last_action` per TV to skip redundant commands but has no retry / verify / timeout.
- LG `getPowerState` subscription already pushes `PowerState` (`Active | ScreenOff | ActiveStandby | Suspend | PowerOff`) into D-Bus via the cached `Tv1.Power` property — the daemon knows the TV's substate; nothing reads it back to gate escalation.
- [docs/ideas/event-driven-power-management.md](../ideas/event-driven-power-management.md) already articulates "distinguish fast blank from slow power-off" but doesn't address auto-power-off escalation.

## Research summary — what other projects do

**LG_Buddy** (Rust, Linux session service). Two-tier wake: try `turn_screen_on` first; on error `-102` (TV rejects unblank because it's not in screen-off substate) escalate to a full WoL+retry loop (6 attempts, 2-30 s backoff, ~48 s worst case). Uses session-scoped marker files to track whether *it* blanked the screen, so it knows whether to attempt restore. Conservative-vs-aggressive policy: conservative requires its own marker; aggressive will restore even if another device powered the TV on. Input-aware fallback on screen-off: if the TV is on a non-HDMI source, considers blank successful without escalating to power-off.

**lgpowercontrol** (Bash, Linux event-driven). 5-retry / 3 s loop on `turn_screen_off`; single-attempt `turn_screen_on` after 1 s post-WoL wait (LG_Buddy says 1 s is empirically too short — they wait 6 s). State-aware: queries `get_power_state` before any dispatch, short-circuits if already in target state. Two-mode config (`POWER_MODE=screen|power` and `BOOT_SHUTDOWN_MODE=power|screen`) — independent policies for idle versus boot/shutdown. README explicitly notes hibernate/suspend unsupported because the network drops before commands complete.

**LGTVCompanion** (C++, Windows service). Power-on timeout is user-configurable (5-100 s, default 40 s). Parallel WoL flood + TCP retry every 1-2 s for the entire timeout window. Pre-WoL transient ARP entry so MAC resolution survives the TV's network silence (Linux equivalent via `ip neighbor` would translate). HDMI-input guard: `CheckHdmiInputWhenPoweringOff` skips power-off if the TV's foreground input doesn't match the PC. Per-device WoL strategy (broadcast / unicast / subnet broadcast / auto). Reads `getPowerState` to decide whether to send `unblank` (→ "Screen Off") versus `toggle` (→ "Active Standby"). User Idle Mode (blank screen) is wired separately from System Suspend (power off) — distinct policy per trigger.

**Python libs** (`bscpylgtv`, `aiowebostv`, `LGWebOSRemote`). All expose `turnOffScreen` with `{"standbyMode": "active"}` (passive variant documented but not recommended). All subscribe to `getPowerState` and surface the explicit "Screen Off" state. `bscpylgtv` exposes the TV's own auto-power-off knobs via Luna `setSystemSettings`: `automaticStandBy` (hours; 0 = Off), `autoOff2HourOnTimer`, `noSignalScreenSaver`, `wolwowlOnOff`. None of the three libraries implements WoL-on-wake fallback themselves — that logic always lives in the application layer.

## Decisions

1. **Scope (v1)** — client-side reliability only. TV-side prevention is deferred to a follow-up; the architectural discussion is captured in [Deferred — TV-side prevention](#deferred--tv-side-prevention) below for eventual archival to `docs/ideas/`.
2. **Reliability tiers** — binary: `best_effort` (today's single dispatch) and `reliable` (verify state via `getPowerState`, retry the same command up to N times with backoff, escalate to WoL + power-on if the TV has slipped past Screen Off, bounded by an overall timeout). `reliable` is an opaque bundle in v1; tunable knobs (retry count, timeout) can be added later as sibling `qos.deadline` / `qos.max_retries` fields without schema break.
3. **Default** — asymmetric: wake-side actions (`screen_on`, `power_on`) default to `reliable`; sleep-side actions (`screen_off`, `power_off`) default to `best_effort`. Rationale: a dark TV when you expected bright is a bug the user immediately notices; the reverse is rarely catastrophic.
4. **Attachment / namespace** — per-event storage. Each `on_*` slot is an `EventAction { action, qos }` table; QoS is a namespace (`qos.reliability` today, room for `qos.deadline`, `qos.durability` later). Asymmetric default is type-driven: `default_reliability_for(action)` derives the tier when no explicit value is set anywhere in the resolve chain.
5. **Vendor abstraction for the (deferred) TV-side path** — when the prevention path lands, it'll surface as a vendor-agnostic `TvBackend::configure_for_management` trait method behind a `CONFIGURE_AUTO_OFF` capability bit. v1 lands the capability bit and the trait method as a no-op stub (returning `Unsupported`) so the LG implementation can be added later without API churn. No Luna keys appear in tisvlay's TOML or D-Bus surface.
6. **D-Bus surface** — read-only property on `Tv1` (or `Tv1.Qos`) exposing the resolved per-action reliability tier. `PropertiesChanged` fires on profile reload. All edits flow through `Manager1.UpdateProfile` / `Session1.UpdateProfile` — profile is the single source of truth.
7. **Verification source for `reliable`** — cached `Tv1.Power` PowerState property. Snapshot pre-dispatch, send the command, wait briefly (50-200 ms), check the cached value updated. LG's `getPowerState` subscription is sub-100 ms so the cache stays fresh; no ad-hoc query needed.
8. **TV-side prevention deferred** — writing persistent TV settings (Luna `automaticStandBy = "Off"` etc.) would be tisvlay's first such write-level API. Every existing `Tv1.*` setter is ephemeral (volume, input, screen-on/off — state the TV forgets on reboot). The persistent-settings category has a long tail (TV input names, parental controls, NTP, energy mode, no-signal screensaver…) and committing to managing it deserves more architectural deliberation than this PR allows. Client-side `reliable` handles the symptom in v1; if field use shows it's insufficient, revisit with option B in the deferred section.

## Data model — per-event storage with action-derived QoS defaults

Storage is **per event**, table-typed. Each `on_*` slot grows from `Option<SessionAction>` to `Option<EventAction>`, where `EventAction` holds an action plus a nested `QosConfig`:

```rust
pub struct EventAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<SessionAction>,
    #[serde(default, skip_serializing_if = "QosConfig::is_empty")]
    pub qos: QosConfig,
}

#[derive(Default)]
pub struct QosConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<Reliability>,
    // Future siblings (not part of v1):
    //   pub deadline: Option<Duration>,
    //   pub durability: Option<Durability>,
}

pub enum Reliability {
    BestEffort,
    Reliable,
}
```

The `qos` field is nested-but-flattened-friendly: `#[serde(skip_serializing_if = "QosConfig::is_empty")]` keeps the table absent from emitted TOML when no qos field is set, so default configs stay terse. As more dimensions land (deadline, durability), they're sibling fields on `QosConfig` — no schema break.

Bare-string back-compat via `#[serde(untagged)]` — `on_session_unlock = "screen_on"` parses as `EventAction { action: Some(ScreenOn), qos: QosConfig::default() }`. New configs may use any of: dotted keys (`on_session_unlock.action`, `on_session_unlock.qos.reliability`), inline tables (`{ action = "...", qos = { reliability = "..." } }`), or table headers. The toml crate emits inline tables when fields fit on one line; the GUI saver prefers that form.

`merge_with_base` recurses through `EventAction` then `QosConfig`, independently merging each leaf via `.or()`. The user can override action, reliability, or any future qos dimension at any tier (per-TV profile → named profile → "default" → cross-scope fallback) without touching the others.

The asymmetric default lives in ONE function:

```rust
fn default_reliability_for(action: SessionAction) -> Reliability {
    match action {
        SessionAction::ScreenOn  | SessionAction::PowerOn  => Reliability::Reliable,
        SessionAction::ScreenOff | SessionAction::PowerOff | SessionAction::None => Reliability::BestEffort,
    }
}
```

`ResolvedEventAction { action: SessionAction, qos: ResolvedQos }` where `ResolvedQos { reliability: Reliability, … }` is the fully-resolved value. The resolve chain:

1. Resolve `action` via the existing `.or()` chain → `SessionAction`.
2. Resolve `qos.reliability` via the same `.or()` chain → `Option<Reliability>`.
3. If still `None`, derive via `default_reliability_for(action)`.

The asymmetric default is *type-driven*, not slot-driven: overriding `on_session_lock.action = "power_off"` automatically gives best-effort reliability (matching the per-action-type rule). To keep an action but flip reliability, set `on_session_lock.qos.reliability = "reliable"` and let `action` fall through.

### Example configs

Defaults expand cleanly when written out (`--write-defaults` form):

```toml
[user.profiles.default]
on_session_lock   = { action = "screen_off", qos = { reliability = "best_effort" } }
on_session_unlock = { action = "screen_on",  qos = { reliability = "reliable"    } }
on_greeter_wake   = { action = "screen_on",  qos = { reliability = "reliable"    } }
on_idle_timeout   = { action = "screen_off", qos = { reliability = "best_effort" } }
on_idle_resume    = { action = "screen_on",  qos = { reliability = "reliable"    } }
idle_timeout_seconds = 0
```

A "gaming" profile that overrides just one event's reliability:

```toml
[user.profiles.gaming]
on_session_lock.qos.reliability = "reliable"  # action falls through to default profile's screen_off
```

Bare-string short form (action only; reliability derived from action):

```toml
[user.profiles.minimal]
on_session_lock   = "screen_off"
on_session_unlock = "screen_on"
```

## V1 implementation outline

Suggested commit sequence (each on its own, reviewable in isolation):

1. **Core types** — `Reliability` enum, `QosConfig`, `EventAction` in `tisvlay-core/src/config.rs`; bare-string back-compat via `#[serde(untagged)]`; `default_reliability_for(action)` helper. Unit tests for the four shapes (bare string, dotted, inline table, table header).
2. **Profile types migrate** — `SystemProfileConfig` / `UserProfileConfig` event slots from `Option<SessionAction>` to `Option<EventAction>`; `merge_with_base` recurses; `resolve` derives default reliability; new `ResolvedEventAction`, `ResolvedQos` types; all existing call sites updated.
3. **Reliable dispatch helper** — new `dispatch_reliable_action` in `tv_manager.rs`: snapshot pre-action PowerState, dispatch, poll cached state, retry, escalate `screen_on` → `power_on` + WoL on continued failure, bounded by overall timeout (default 40 s a la LGTVCompanion).
4. **Wire to event handler** — `tisvlay-session/src/lib.rs::event_handler` reads `ResolvedEventAction.qos.reliability` and dispatches via the right helper (existing best-effort path or new reliable path).
5. **D-Bus surface** — new read-only `Tv1.Qos.Reliability` (or similar) property exposing the resolved per-action tier; fires `PropertiesChanged` on profile reload.
6. **Vendor trait stub** — `Capabilities::CONFIGURE_AUTO_OFF` bit; `TvBackend::configure_for_management() -> Result<(), Unsupported>` default. No LG impl yet — this is the seam for the deferred TV-side path.
7. **Tests** — profile resolution (per-TV → named → default → fallback with nested qos), bare-string back-compat round-trip, reliable dispatch (verify, retry, escalate, timeout), D-Bus property visibility + `PropertiesChanged` on reload.
8. **Docs** — update `docs/design.md` with QoS section; refresh `docs/configs/daemon.toml` sample with explicit reliability values.
9. **GUI Profiles pane** (lands as part of M4, not this PR) — per-event row with action + reliability combos; ghost-text or "(default)" hint for unset reliability values.

## Deferred — TV-side prevention

Archive this section to `docs/ideas/tv-side-power-prevention.md` when this PR lands. Captured here in full so the context isn't lost.

### Why deferred

Writing Luna persistent settings (`automaticStandBy = "Off"` and friends) would be tisvlay's **first persistent TV-setting write**. Every existing `Tv1.*` setter is ephemeral (volume, input, screen-on/off — runtime state the TV forgets on reboot or that the next remote button-press changes). The persistent-settings category has a long tail (TV input names, parental controls, NTP, energy mode, no-signal screensaver, network settings…) and committing to managing it deserves more architectural deliberation than the v1 PR allows.

For v1 we ship client-side reliable only, which handles the symptom (TV slipped past Screen Off → WoL-escalate to power-on). If field use shows the symptom is common enough to warrant the prevention path, revisit with option B below.

### Failure-mode taxonomy

| Failure mode | Mechanism | Luna key |
|---|---|---|
| Idle auto-off | TV counts "no user input" hours even in Screen Off; transitions to Power Off | `category="time"`, `automaticStandBy` (hours; 0 = Off) |
| No-signal off | HDMI handshake loss → No-Signal screensaver → eventual power off | `category="general"`, `noSignalScreenSaver` (on/off) |
| Fixed 2-hour timer | Distinct 2-hour countdown on some firmware | `category="time"`, `autoOff2HourOnTimer` (on/off) |
| Hidden firmware descent | "Screen Off → Active Standby" timer with no user-facing knob | — (no key; client-side reliable handles this) |

The first row (`automaticStandBy`) is the dominant cause in field reports. The hidden-firmware row has no Luna remedy and is precisely what the client-side `reliable` path exists for — so even if TV-side prevention never lands, the worst cases are still covered.

### Three commitment levels considered

**A — Defer entirely.** Ship client-side reliable only in v1. Document "set your TV's Auto Power Off to Off via the remote" as a manual prerequisite. ← **Chosen for v1.**

**B — Opaque "configure for management" capability.** Single trait method `TvBackend::configure_for_management() -> Result<ConfigSnapshot, Error>`. LG impl writes whatever Luna keys it decides (`automaticStandBy = "Off"`, `wolwowlOnOff = "true"`, possibly more) — implementation details, not surfaced in tisvlay's TOML or D-Bus. Triggered by `managed = true` on pair, with a GUI "Sync TV settings" button for re-running on demand. Snapshot original Luna values pre-write; `TvBackend::restore_pre_managed_settings(snapshot)` reverses on un-manage. ← **Expected design when this is un-deferred.**

**C — Full TV-settings-management category.** Explicit `[tv.power] auto_off_hours = N` field with read-on-connect, drift detection, "(out of sync)" GUI badge. Commits tisvlay to being a TV-settings-manager. Each future setting becomes a new field. Future-flexible but bigger ongoing surface. ← **Likely overkill for the immediate need; revisit only if multiple TV settings turn out to need explicit knobs.**

### Implementation sketch for the eventual un-deferral

When option B is picked up:

- `Capabilities::CONFIGURE_AUTO_OFF` (already added in v1 as part of the trait stub).
- `TvBackend::configure_for_management(&self) -> Result<ConfigSnapshot, Error>` — returns a snapshot of pre-change values for later restoration.
- `TvBackend::restore_pre_managed_settings(&self, snapshot: &ConfigSnapshot) -> Result<()>`.
- LG impl: write `automaticStandBy = "Off"`, ensure `wolwowlOnOff = "true"`, snapshot prior values.
- Pairing wizard step (optional checkbox): "Configure this TV for management?" — explains what gets changed, links to per-backend docs enumerating the settings.
- GUI: per-TV "Sync TV settings" affordance for re-running on demand.
- Persistent snapshot stored in TV's `[tv.*]` block under a managed-by-tisvlay sub-table (e.g., `[tv.managed_snapshot]`), so restoration survives daemon restarts.
- Per-backend doc in `docs/lg/managed-settings.md` lists exactly what changes when management is enabled.

### Note on bscpylgtv quirk

CLAUDE.md notes that direct `setSystemSettings` returns 401 for some categories (specifically picture/backlight, hence the Luna `createAlert` workaround for that path). For `category="time"` (where `automaticStandBy` lives) and `category="general"` (where `noSignalScreenSaver` lives), bscpylgtv calls `setSystemSettings` directly without the workaround — implying these categories don't have the 401 issue. Worth verifying empirically against the user's specific firmware (LG OLED C3 2023, G4 2024) when the prevention path is un-deferred.

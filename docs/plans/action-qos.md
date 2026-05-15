# Action QoS — reliability tier for screen / power dispatch

## Motivation

Today the session service dispatches each TV action (`screen_off`, `screen_on`, `power_on`, `power_off`) in a single fire-and-forget call. The default user-profile maps `on_session_lock → screen_off` and `on_session_unlock → screen_on`, which gives a fast unblank when it works — but newer LG firmware may transition the TV from "Screen Off" to "Active Standby" or full "Power Off" on its own after some interval. Once the TV has slipped to a deeper power state, a follow-up `turnOnScreen` call is unreliable: it returns `-102` ("not in screen-off substate"), times out, or silently succeeds without actually waking the panel. The user is then staring at a dark TV after unlocking the desktop.

The proposal is a **QoS knob** on TV actions: let users declare which actions should be _reliable_ (verify state, retry, escalate to WoL+power-on if the TV has slipped past screen-off) versus _best-effort_ (today's one-shot dispatch). Reliable mode has higher worst-case latency but won't leave the desktop with a dark panel; best-effort keeps the ~100 ms snap-back when the TV is well-behaved.

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

## Design space

The QoS feature has at least four orthogonal dimensions, each with a real design choice:

1. **Where reliability is enforced.** Client-side (we retry / verify / escalate at dispatch time, so it works on any TV state) versus TV-side (we set the TV's own `automaticStandBy` to Off via Luna so it never slips to deeper sleep, attacking root cause). These are complementary, not exclusive — but each is a chunk of work.

2. **Attachment point in the config.** Per profile (one knob covers every `on_*` in that profile), per event (`on_session_unlock = "screen_on"` + sidecar `on_session_unlock_qos = "reliable"`), per action type (a profile-level `screen_on_qos`, `power_on_qos`, etc.), or per TV (override profile QoS for one device with quirky firmware). Each has a different denormalization story.

3. **Semantic gradient — what "reliable" means.** Best-effort (today). Verify (dispatch + read-back, log mismatch but no remediation). Verify + retry (re-send the same command up to N times). Verify + escalate (try `screen_on`, fall back to `power_on` with WoL when the TV has slipped past Screen Off). Bounded retry window (overall timeout, like LGTVCompanion's 40 s). Each tier adds latency in the unhappy path.

4. **Default policy.** Keep today's best-effort default (no behavior change for existing users). Or switch to reliable as the new default (better OOTB UX, slower worst case). Or asymmetric default (reliable on wake-side actions `screen_on` / `power_on`; best-effort on sleep-side `screen_off` / `power_off`, since dark-when-you-wanted-dark is harder to mistake than dark-when-you-wanted-bright).

## Decisions

1. **Scope** — both client-side reliability *and* TV-side prevention. Defense in depth: TV-side prevention (Luna `automaticStandBy` and friends) attacks the root cause for LG fleets where it works; client-side reliability is the universal safety net that covers everything else, including future backends.
2. **Reliability tiers** — binary: `best-effort` (today's single dispatch) and `reliable` (verify state via `getPowerState`, retry the same command up to N times with backoff, escalate to WoL + power-on if the TV has slipped past Screen Off, bounded by an overall timeout). `reliable` is an opaque bundle; tunable knobs (retry count, timeout) can be added later if needed.
3. **Default** — asymmetric: wake-side actions (`screen_on`, `power_on`) default to `reliable`; sleep-side actions (`screen_off`, `power_off`) default to `best-effort`. Rationale: a dark TV when you expected bright is a bug the user immediately notices; the reverse is rarely catastrophic.

## Data model — per-event storage with action-derived QoS defaults

Storage is **per event**, table-typed. Each `on_*` slot grows from `Option<SessionAction>` to `Option<EventAction>` where `EventAction` itself has optional inner fields:

```rust
pub struct EventAction {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<SessionAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qos: Option<Qos>,
}

pub enum Qos {
    BestEffort,
    Reliable,
}
```

Bare-string backward compatibility via `#[serde(untagged)]` — accept `on_session_unlock = "screen_on"` as `EventAction { action: Some(ScreenOn), qos: None }`. New configs may use any of: dotted keys (`on_session_unlock.action`, `on_session_unlock.qos`), inline tables (`{ action = "...", qos = "..." }`), or table headers. The toml crate emits inline tables when both fields fit on one line; the GUI saver prefers that form for readability.

`merge_with_base` recurses into each event slot, independently merging `action` and `qos` via `.or()`. The user can override either dimension at any tier (per-TV profile → named profile → "default" → cross-scope fallback) without touching the other.

The asymmetric default lives in ONE function:

```rust
fn default_qos_for(action: SessionAction) -> Qos {
    match action {
        SessionAction::ScreenOn  | SessionAction::PowerOn  => Qos::Reliable,
        SessionAction::ScreenOff | SessionAction::PowerOff | SessionAction::None => Qos::BestEffort,
    }
}
```

`ResolvedEventAction { action: SessionAction, qos: Qos }` is the fully-resolved value. The resolve chain is:

1. Resolve `action` via the existing `.or()` chain → `ResolvedAction`.
2. Resolve `qos` via the same `.or()` chain → `Option<Qos>`.
3. If `qos` is still `None`, derive via `default_qos_for(ResolvedAction)`.

The asymmetric default is *type-driven*, not slot-driven: overriding `on_session_lock.action = "power_off"` automatically gives that slot best-effort qos (matching the per-action-type rule), no manual qos field needed. To keep an action but flip its qos, set `on_session_lock.qos = "reliable"` and let `action` fall through.

### Example configs

Defaults expand cleanly when written out (this is what `--write-defaults` would emit):

```toml
[user.profiles.default]
on_session_lock   = { action = "screen_off", qos = "best-effort" }
on_session_unlock = { action = "screen_on",  qos = "reliable" }
on_greeter_wake   = { action = "screen_on",  qos = "reliable" }
on_idle_timeout   = { action = "screen_off", qos = "best-effort" }
on_idle_resume    = { action = "screen_on",  qos = "reliable" }
idle_timeout_seconds = 0
```

A "gaming" profile that overrides just one slot's qos:

```toml
[user.profiles.gaming]
on_session_lock.qos = "reliable"  # action falls through to default profile's screen_off
```

A profile that takes the bare-string short form (action only, qos derived):

```toml
[user.profiles.minimal]
on_session_lock   = "screen_off"
on_session_unlock = "screen_on"
```

## Open questions

1. **Terminology** — keep "QoS" or rename to something less networking-flavored (e.g., "Reliability", "Mode")? It'll show up in TOML keys, D-Bus property names, and GUI labels.
2. **Vendor abstraction** — reliability is naturally vendor-agnostic (verify + retry + escalate fits any backend); but TV-side prevention is vendor-specific (only some backends will have a `setAutoPowerOff`-equivalent). Should the `TvBackend` trait surface a capability flag, or keep the TV-side path inside the LG backend with a stub trait method that other backends return `Unsupported` for?
3. **D-Bus surface** — does QoS land as a property on `Tv1` (read-only, computed from profile), a method like `Tv1.SetActionQos`, or only via `Manager1.UpdateProfile` (no Tv1 surface, profile is the only source of truth)?
4. **Verification source for client-side reliable** — use the existing cached `Tv1.Power` PowerState property, or do an ad-hoc `getPowerState` query each time? Cached is faster but may be stale by 100-500 ms; ad-hoc is fresh but adds round-trip latency to every action.
5. **TV-side prevention scope** — for the Luna prevention path, which knobs do we expose: just `automaticStandBy` (auto-power-off hours), or also `autoOff2HourOnTimer` and `noSignalScreenSaver`? And do we expose them as profile-level fields (managed-by-tisvlay) or as a one-shot "configure TV" wizard?
6. **GUI surfacing of resolved-vs-explicit qos** — in edit mode, do unset qos values show as ghost text ("(reliable)" derived from action) or as an empty placeholder? Affects how clear it is when a value is inherited vs explicit.

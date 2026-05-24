# Tray desktop notifications

Add user-configurable desktop notifications to TisVlay so that background events on the daemon side surface on the desktop. Owner is `tisvlay-tray`. Settings are mutable both via D-Bus (GUI / `busctl`) and via editing `~/.config/tisvlay/tray.toml` directly; both paths emit the same `NotificationSettingsChanged` signal so subscribers stay in sync.

## Goals

- Emit `org.freedesktop.Notifications` for a small fixed set of categories that benefit from background awareness (TV connection, pairing outcomes, wake outcomes, automation, daemon offline).
- Let the user toggle each category from the GUI **Settings** pane, with changes taking effect live without a restart.
- Let the user hand-edit `~/.config/tisvlay/tray.toml` and pick up changes via a D-Bus `ReloadConfig` call (no signal-handler restart needed), symmetric with `Session1.ReloadConfig`.

## Non-goals (v1)

- Per-TV scoping of notification categories. Toggles are global per category — one switch per category applies to every TV.
- Action buttons on notifications (`Reconnect`, `Re-pair`, …). Notification bodies are display-only for v1; capability detection via `GetServerInformation` and `notify-rust` action-handler wiring are deferred.
- Notification grouping, history pruning, replace-by-tag logic, or sound cues.
- Settings GUI surface beyond a single `NOTIFICATIONS` group in the existing `settings_pane.slint`.

## Why the tray owns this (not the session service)

The tray already has every moving piece this needs:

- A `StateWatcher` subscription that sees every relevant transition, and a `refresh_state` path that already overwrites cached TV state — the natural place to slot in old-vs-new diffing.
- gettext catalog (`tisvlay-tray.mo`) and icon-name plumbing already wired through `bindtextdomain`.
- A graphical-session lifecycle (autostart, follows logind sessions) — which is the right surface for a feature that talks to `org.freedesktop.Notifications`, itself a graphical-session bus name.
- A `Tray1` D-Bus interface whose existing docstring already pre-allocates this slot ("Future tray-side functionality (desktop notifications + their settings, click-event stats, etc.) lands as additional methods and properties on `Tray1`").

The session service is the wrong home because it owns `UserConfig` as a single writer with rollback-on-IO-failure semantics; bolting tray-only settings onto that file would either fight the single-writer invariant or split one TOML across two processes. A separate `tray.toml` keeps the two single-writer invariants independent.

## Architecture

```
┌─────────────────┐      ┌─────────────────┐     ┌──────────────────┐
│ tisvlay-gui     │      │ tisvlay-tray    │     │ tisvlayd /       │
│ Settings pane   │      │ TrayService     │     │ tisvlay-session  │
│  NOTIFICATIONS  │      │  + Notifier     │     │                  │
│  group          │      │                 │     │                  │
└────────┬────────┘      └────────┬────────┘     └────────┬─────────┘
         │                        │                       │
         │ Tray1.Update*          │ StateWatcher signals  │
         │ Tray1.ReloadConfig     │ (existing)            │
         │ ──────────────────►    │ ◄──────────────────── │
         │                        │                       │
         │ NotificationSettings   │ Session1.             │
         │ Changed signal         │ AutomationFired       │
         │ ◄────────────────────  │ ◄──────────────────── │
         │                        │                       │
         │                        │ org.freedesktop.      │
         │                        │ Notifications.Notify  │
         │                        │ ──────────────────►   │  (DE)
         │                        │                       │
         │                        ▼                       │
         │                  ~/.config/tisvlay/tray.toml   │
         │                  (single-writer = tray)        │
```

Mirrors the `Session1` user-profile CRUD shape exactly:

1. **Validate** caller input (unknown category key → `InvalidArgs`).
2. **Mutate** in-memory `TrayConfig` under a write lock.
3. **Persist** via `TrayConfig::save_to(path)` with **rollback** on IO failure (prior snapshot is restored before the error returns).
4. **Emit** `NotificationSettingsChanged` carrying the full new `a{sb}` after persistence completes, so subscribers re-fetch into a guaranteed-consistent state.

## Wire surface

Service `org.tisvlay.Tray`, path `/org/tisvlay/Tray`, interface `org.tisvlay.Tray1`.

### Properties

| Name | Type | Notes |
|---|---|---|
| `Version` | `s` | existing, const |
| `ConfigPath` | `s` | existing; now resolves to `tray.toml` (today returns empty) |
| `NotificationSettings` | `a{sb}` | full bag; `emits_changed_signal = "false"`, an explicit signal carries the new state |

### Methods

| Name | Args | Returns | Notes |
|---|---|---|---|
| `UpdateNotificationSettings` | `a{sb}` (subset to mutate) | `()` | atomic multi-key update; unknown keys → `org.freedesktop.DBus.Error.InvalidArgs` |
| `SetNotificationSetting` | `s` (key), `b` (enabled) | `()` | convenience wrapper around the atomic path, matches `busctl` ergonomics |
| `ReloadConfig` | — | `()` | re-reads disk, diffs against prior, emits `NotificationSettingsChanged` if any key changed |

### Signals

| Name | Args | Notes |
|---|---|---|
| `NotificationSettingsChanged` | `a{sb}` (full new state) | payload small enough (~5 bool entries) that bundling it saves clients a round-trip |

### Error names

Prefix `org.tisvlay.Tray1.Error.*`, plus the standard `org.freedesktop.DBus.Error.InvalidArgs` for unknown category keys (matches the convention used in `SessionError::InvalidArgs`).

| Variant | Maps to |
|---|---|
| `IoError(String)` | `TrayConfig::save_to` failure after rollback |
| `OperationFailed(String)` | TOML serialization failures and other unexpected internals |
| `InvalidArgs(String)` | unknown category key on update (standard `freedesktop.DBus.Error.InvalidArgs` wire name) |

## Categories

Stable snake_case keys (these are wire identifiers in the `a{sb}` map):

| Key | Trigger | Default | Urgency | Tray-side source |
|---|---|---|---|---|
| `tv_connection` | TV `connected` transitions true ↔ false | **on** | `Low` (connect), `Normal` (disconnect) | `refresh_state` diff |
| `tv_pairing` | terminal pairing phase `complete` / `failed` | **on** | `Normal` / `Critical` | `Manager1.PairingProgress` (already watched by `StateWatcher`) |
| `tv_wake` | `Tv1.Power.PowerCycleProgress` reaches `connected` / `failed` after a `wol_sent` | **on** | `Low` / `Critical` | per-TV `Tv1.Power.PowerCycleProgress` stream (see "Per-TV power-cycle subscription" below) |
| `automation_fired` | session action ran on a TV | **off** | `Low` | new `Session1.AutomationFired` signal |
| `daemon_offline` | `StateEvent::DaemonDisappeared` | **on** | `Critical`, hint `resident` (sticky) | existing tray arm |

The `automation_fired` default-off matches the user's earlier "noisiest one off" answer — it can fire many times per day on a managed TV (session lock/unlock cycles).

## File changes

### `tisvlay-tray` (new `src/config.rs`)

`TrayConfig` stays tray-side rather than going into `tisvlay-core`. `UserConfig` lives in `tisvlay-core` because it has two distinct in-process consumers (`tisvlay-session` writes, `tisvlayd` reads). `TrayConfig` has exactly one writer (tray) and one external consumer (GUI, via the `a{sv}` wire shape on `TrayClient`) — the struct never crosses a crate boundary, only the wire body does. Keeping it in the tray crate avoids coupling core to a tray-only schema.

```text
pub struct TrayConfig {
    pub notifications: NotificationSettings,
}

pub struct NotificationSettings {
    pub tv_connection: bool,
    pub tv_pairing: bool,
    pub tv_wake: bool,
    pub automation_fired: bool,
    pub daemon_offline: bool,
}

impl Default for NotificationSettings { /* all true except automation_fired */ }

impl TrayConfig {
    pub fn config_path() -> Result<PathBuf>;        // ~/.config/tisvlay/tray.toml
    pub fn load() -> Result<Option<Self>>;
    pub fn load_from(path: &Path) -> Result<Option<Self>>;
    pub fn save(&self) -> Result<()>;
    pub fn save_to(&self, path: &Path) -> Result<()>;
}

impl NotificationSettings {
    pub fn as_map(&self) -> HashMap<String, bool>;        // for the a{sb} property
    pub fn apply(&mut self, updates: &HashMap<String, bool>) -> Result<(), String>;  // unknown key -> Err
    pub fn known_keys() -> &'static [&'static str];
}
```

Follows `UserConfig`'s missing-file → `Ok(None)` convention; `Default::default()` is the fallback. `as_map` / `apply` keep the wire ↔ struct boundary in one place so the D-Bus handler stays thin.

### `tisvlay-core/src/dbus_client.rs` (new `TrayClient`)

Mirror of `SessionClient`:

```text
pub struct TrayClient { proxy: TisvlayTrayProxy<'static> }

impl TrayClient {
    pub async fn new() -> Result<Self>;                          // session bus only
    pub async fn from_connection(c: zbus::Connection) -> Result<Self>;
    pub async fn is_available(&self) -> bool;                    // ping NotificationSettings
    pub async fn version(&self) -> Result<String>;
    pub async fn config_path(&self) -> Result<String>;
    pub async fn list_notification_settings(&self) -> Result<HashMap<String, bool>>;
    pub async fn update_notification_settings(&self, updates: HashMap<String, bool>) -> Result<()>;
    pub async fn set_notification_setting(&self, key: &str, enabled: bool) -> Result<()>;
    pub async fn reload_config(&self) -> Result<()>;
    pub fn proxy(&self) -> &TisvlayTrayProxy<'static>;           // for direct signal subscription
}
```

Extend the existing `TisvlayTrayProxy` (currently a no-method stub used only for version probing in the GUI's About pane) with the new methods/properties/signal.

### `tisvlay-core/src/dbus_client.rs` (extend `StateEvent` + `StateWatcher`)

```text
pub enum StateEvent {
    /* existing */
    TrayAppeared,                      // already present (bus-name lifecycle)
    TrayDisappeared,                   // already present (bus-name lifecycle)
    TrayNotificationSettingsChanged,   // new (payload-less; subscribers re-fetch via TrayClient)
    SessionAutomationFired {           // new
        tv_id: String,
        event_kind: String,            // stable snake_case from SessionEvent::Display
        action_kind: String,           // stable snake_case from SessionAction::Display (NOT Debug)
    },
}
```

`StateWatcher` today only watches `Tray` *bus-name* lifecycle (`tray_watch` at `dbus_client.rs:2800`); there is **no `tray_signal_stream`** parallel to `session_signal_stream`. Both new signals require explicit plumbing, not a one-line subscription:

1. **`Tray1.NotificationSettingsChanged`** — new `tray_signal_stream` filter following the shape of `session_signal_stream` at `dbus_client.rs:2820-2860` (match rule on member name, new `TraySignal::NotificationSettingsChanged` enum, new `next()` branch around `dbus_client.rs:2955`). The const path/interface need exporting from `dbus_paths.rs` (`TRAY_SERVICE` exists; `TRAY_INTERFACE`/`TRAY_PATH` need adding).
2. **`Session1.AutomationFired`** — extends the existing `session_signal_stream`: new `SessionSignal::AutomationFired { tv_id, event_kind, action_kind }` variant, new match arm in the filter block at `dbus_client.rs:2834`, new `next()` branch.

`StateEvent` convention today is payload-less (subscribers re-query state). `TrayNotificationSettingsChanged` follows that convention — the GUI re-fetches via `TrayClient::list_notification_settings`. `SessionAutomationFired` is the deliberate exception: it represents an *ephemeral event*, not a state change — there is nothing to re-query (the session service doesn't persist "last automation fired"), so the payload travels in-band. The three fields are kept as stable snake_case identifiers (callable by `gettext("automation.event.{event_kind}")`-style key lookup, not interpolated into translated strings) so future enum extensions don't break wire compat.

Existing tray + GUI `match` blocks add new arms; today the tray's `TrayAppeared`/`TrayDisappeared` arms already no-op for self-events.

### `tisvlay-tray/src/dbus.rs` (extend `Tray1`)

Existing `TrayService` gains:

```text
pub(crate) struct TrayService {
    config: Arc<RwLock<TrayConfig>>,    // new
    config_path: Option<PathBuf>,       // existing, now load-bearing
}
```

Methods + signal implemented in the validate→mutate→persist→emit shape from `session_dbus.rs::set_tv_user_profile`. `UpdateNotificationSettings` takes a snapshot, applies updates, attempts save, rolls back to snapshot on save failure, then emits.

### `tisvlay-tray/src/notifier.rs` (new)

```text
pub(crate) struct Notifier {
    config: Arc<RwLock<TrayConfig>>,
    handle: NotificationServer,         // lazy; connects on first emit
}

impl Notifier {
    pub fn new(config: Arc<RwLock<TrayConfig>>) -> Self;
    pub async fn notify(
        &self,
        category: NotificationCategory,
        summary: String,
        body: String,
        urgency: Urgency,
    );
}

pub(crate) enum NotificationCategory {
    TvConnection, TvPairing, TvWake, AutomationFired, DaemonOffline,
}
```

`notify` first snapshots the category bool under a brief read lock (no holding the lock across the `notify-rust` D-Bus call), then no-ops if disabled. Notifier connection is lazy and cached — a failed connect (notification server not yet ready on slow logins) is cached for ~5s before retry so initial bursts don't all stall trying to reconnect.

App name `"TisVlay"`, icon name `"video-display"` (matches the tray's active-state icon — DE notification history will group under TisVlay's app entry).

### `tisvlay-tray/src/main.rs`

- Construct `TrayConfig` snapshot during startup (after `bindtextdomain`), pass to `dbus::register(...)`. Tray's `register` signature gains a `config: Arc<RwLock<TrayConfig>>`.
- Build the `Notifier` from the same `Arc<RwLock<TrayConfig>>`, share with `refresh_state` + the `automation_fired` listener.
- In `refresh_state`, take an `old_tvs = tray.tvs.clone()` snapshot, fetch new, run `diff_for_notifications(&old_tvs, &new_tvs)` to compute a `Vec<NotificationEvent>`, push each into the `Notifier`, then do the existing `handle.update(...)` overwrite. The diff helper lives in `state.rs` so it's pure and unit-testable.
- In the `StateEvent::DaemonDisappeared` arm, additionally call `notifier.notify(DaemonOffline, …)`.
- New tokio task subscribed to `Session1.AutomationFired` → `notifier.notify(AutomationFired, …)`. The session-service `is_available()` probe pattern from `main.rs` lines 100–110 handles "session not yet up" cleanly.

### `tisvlay-session` (one new signal)

`Session1.AutomationFired(tv_id: s, event_kind: s, action_kind: s)`. Fired from `event_handler` in `lib.rs:289` after `execute_action_for_tv` returns `Ok(())`, inside the per-TV `for (tv_id, action)` loop. Both `event_kind` and `action_kind` are **stable snake_case identifiers** intended as gettext lookup keys (`automation.event.session_locked`, `automation.action.screen_off`), **not** translated strings and **not** Debug-formatted values.

- `event_kind` is the existing `SessionEvent::Display` impl (already snake_case: `session_locked`, `session_unlocked`, `idle_timeout`, …).
- `action_kind` requires either a new `SessionAction::Display` impl returning snake_case (`screen_off`, `screen_on`, `power_off`, `power_on`, `none`) or a dedicated `fn kind(&self) -> &'static str`. The existing `Debug` impl is unstable across refactors and renders poorly in toast bodies (`"PowerOff { force: false }"`); the new helper is a one-screen patch and lives next to the enum in `tisvlay-core`.

Purely additive on the wire — existing subscribers ignore it.

### `tisvlay-gui`

- `crates/tisvlay-gui/ui/globals/tray.slint` (new): mirrors `globals/daemon.slint`'s shape — exposes a `tray-notifications` struct property keyed by the category names, and a `set-notification-setting(category, enabled)` callback wired Rust-side to `TrayClient::set_notification_setting`.
- `crates/tisvlay-gui/src/state_watch.rs`: handle `StateEvent::TrayNotificationSettingsChanged` by re-fetching the bag and pushing into the Slint global.
- `crates/tisvlay-gui/src/lib.rs`: wire `Tray.set-notification-setting` callback to dispatch through `TrayClient`. On error, route through the same `Errors.tvs`-style inline-error surface (probably a new `Errors.settings` slot to keep it pane-scoped).
- `crates/tisvlay-gui/ui/components/settings_pane.slint`: drop the placeholder text, add a `NOTIFICATIONS` `Group` with five `FormRow { ... Switch }` lines. Labels + helpers wrapped in `@tr(...)`:

  | Label | Helper |
  |---|---|
  | `TV connection` | `Notify when a TV connects or disconnects.` |
  | `Pairing outcome` | `Notify when pairing or re-pairing completes or fails.` |
  | `Wake outcome` | `Notify when a Wake-on-LAN attempt succeeds or fails.` |
  | `Automation` | `Notify when a session action (lock, unlock, idle) fires on a TV.` |
  | `Daemon offline` | `Notify when the connection to the daemon is lost.` |

Pane gains a "When the tray is unavailable, settings are read-only" empty state when `Tray.available == false` — mirrors `TvList`'s "Daemon offline" branch.

## Concurrency / lifecycle notes

- **Per-TV power-cycle subscription (for `tv_wake`).** `CachedTvState` doesn't carry `power_cycle_phase` — diffing it against the new state wouldn't reveal `wol_sent → connected` vs `wol_sent → failed` transitions, only the post-cycle `connected` flip. The `notify-rust` path needs the per-TV `Tv1.Power.PowerCycleProgress` signal stream directly. The proxy + `power_cycle_progress_stream` helper already exists at `crates/tisvlay-core/src/dbus_client.rs:1375`, but the tray needs subscription lifecycle: `subscribe(uuid)` on each `InterfacesAdded` for that TV (or on `DaemonAppeared` + the initial `list_tvs`), `drop(uuid)` on `InterfacesRemoved` / `DaemonDisappeared`. A `HashMap<String, JoinHandle<()>>` keyed by TV UUID, owned by the watcher task, handles this. Each per-TV task reads the stream, emits to the same `Notifier` (gated by `tv_wake`), and exits on stream end.

- **Diff baseline must be the prior `tray.tvs` inside `handle.update`'s closure, not a pre-await snapshot.** `RefreshTrigger` (`crates/tisvlay-core/src/refresh_trigger.rs:62-116`) already prevents concurrent re-entry via an `in_flight: AtomicBool` — a second `fire()` sets `dirty` and returns. The actual race that the diff must guard against is `StateEvent::DaemonDisappeared` at `main.rs:383-390`, which clears `tray.tvs` *outside* the trigger path. Without care, the next refresh after a daemon flap would see "all TVs reappeared" and emit a `tv_connection` notification storm. Fix: compute the diff *inside* the `handle.update(|t| { let events = diff_for_notifications(&t.tvs, &new_tvs); t.tvs = new_tvs; events })` closure where the old `t.tvs` is the authoritative baseline, then dispatch the returned events to `Notifier` after the closure returns.
- **Lock discipline**: the config snapshot for `Notifier::notify` is read under a `tokio::sync::RwLock` read guard that drops immediately. Saving (write guard) happens inside `Tray1::UpdateNotificationSettings`, with the lock held across the `save_to` IO — same pattern as `set_tv_user_profile`.
- **Notification server availability**: `notify-rust`'s `Notification::show()` is async-blocking on the session bus call. If `org.freedesktop.Notifications` isn't yet owned (very early login), the call errors. The `Notifier` caches the last failure timestamp; subsequent calls within ~5s drop silently rather than re-erroring. After 5s a fresh attempt is made.
- **Burst protection**: A single state transition can in principle fire multiple notifications (a TV disconnecting cascades into a power-cycle-failed). The diff helper is deduplicating by `(tv_id, category)` per refresh — at most one notification per TV per category per refresh tick.
- **Single-writer for `tray.toml`**: only the tray binary writes this file. `ReloadConfig` always reads, never writes back. A hand-edit during a live `UpdateNotificationSettings` call is at worst a wasted RPC (the next ReloadConfig picks up the user's edit).

## i18n

All notification summaries / bodies and the new settings-pane labels pass through `gettext("…")` (tray side) or `@tr(...)` (GUI side). No new gettext infrastructure — the `tisvlay-tray` / `tisvlay-gui` catalogs already exist. After string changes land, `just i18n-extract` refreshes the `.pot` template.

Notification format strings use the `gettext("Disconnected: {0}").replace("{0}", &tv.name)` pattern (per CLAUDE.md i18n rule — `format!()` strings can't be extracted by `xgettext`). TV names are user-provided so they pass through verbatim.

## Testing

- **Unit (`tisvlay-core::tray_config`)**: round-trip a `TrayConfig` through `save_to`/`load_from`, missing-file → `Ok(None)`, `as_map` ↔ `apply` symmetry, unknown-key `apply` → `Err`.
- **Unit (`tisvlay-tray::notifier`)**: diff helper — empty→non-empty (no notification, initial sync), connect transition, disconnect transition, wake `wol_sent`→`connected` vs `wol_sent`→`failed`, dedup of cascade transitions in a single refresh.
- **Integration (`tisvlay-tray/tests/notifications_dbus.rs` new)**: drive `Tray1.UpdateNotificationSettings` via a `TestTray` harness, assert `NotificationSettingsChanged` fires, assert `tray.toml` round-trips. Mock `notify-rust` is out of scope — the notifier behind a trait + a `NoopNotifier` in test mode keeps the test from depending on a real `org.freedesktop.Notifications` peer.
- **Hand-edit smoke**: write a hand-modified `tray.toml`, call `Tray1.ReloadConfig` via `busctl`, observe `NotificationSettingsChanged` payload matches the file.

## Commit shape

Three commits, ordered so each commit is independently runnable + `busctl`-pokeable:

1. **`feat(core,tray): TrayConfig + notification settings D-Bus surface`** — `tisvlay-core::tray_config`, `TrayClient`, extended `Tray1` interface (properties + methods + signal + `ReloadConfig`), tray loads config on startup. No emission yet but the wire surface is complete and the settings pane can already toggle the bools.
2. **`feat(tray): emit desktop notifications on state transitions`** — `Notifier` module, `refresh_state` diff helper, `DaemonDisappeared` hook. Covers four of five categories — everything the tray already sees today.
3. **`feat(session,tray): AutomationFired signal + notification`** — session-side signal emit + tray listener. Smallest of the three; cleanly skippable if we decide to defer.

GUI `NOTIFICATIONS` group lands as a sub-commit of (1) once the wire surface is in place.

## Open questions

- **`replaces_id` / `category` hints?** `notify-rust` supports the freedesktop `category` hint (`"device"`, …) and ID-based replacement. Worth setting `category("device")` so DEs that group/filter by category route TisVlay notifications sensibly; replacement-by-id is deferred (would dedup back-to-back wake retries, but adds state-tracking complexity).
- **Critical-urgency stickiness for `daemon_offline`**: most DEs honor `Urgency::Critical` as a persistent (non-timeout) toast. Worth setting; users who silence other categories will likely still want this one to remain on-screen until acknowledged.
- **Settings-pane availability when tray is offline**: should the toggles be disabled or hidden when `Tray.available == false`? Plan currently says disabled with an empty-state — confirm before laying out the Slint scaffold.

## Pending review decisions

The following items came out of the agent-review pass on this plan. None are blockers for the plan's architectural shape, but each should be resolved before or during the relevant commit.

### ⚠️ Tightening (single-reviewer, solid)

- **Switch `NotificationSettings` to typed dict-derive (`a{sv}` via `NotificationSettingsBody`)** rather than `a{sb}`. Every other config wire body in the workspace (`UserProfileBody`, `SystemProfileBody`) uses dict-derive; an `a{sb}` map decodes arbitrary unknown keys at the wire layer and loses forward-compat for future non-bool fields (rate limits, per-TV scoping). Decision point: commit 1.
- **Drop `SetNotificationSetting(s, b)`**; keep only the atomic `UpdateNotificationSettings`. `Session1` has only `Update*` mutators — single mutator means a single rollback path. `busctl` ergonomics for one-key edits are preserved by passing a single-entry map. Decision point: commit 1.
- **Signal payload shape**: emit changed-keys (`as`) rather than the full new state, **OR** drop the custom signal entirely and rely on auto-emitted `org.freedesktop.DBus.Properties.PropertiesChanged`. Current "suppress auto + custom-emit full state" is half-decided; the project's convention elsewhere is signal-as-cache-invalidator + client re-fetches. Decision point: commit 1.
- **`Notifier::notify` lock-discipline foot-gun**: current signature taking `&self` with an `Arc<RwLock<TrayConfig>>` inside encourages the wrong implementation (lock held across the `notify-rust` D-Bus call). Either (a) make `notify` take `enabled: bool` so the caller reads the flag and drops the guard explicitly, or (b) have `Notifier` cache an `AtomicU8` bitset of enabled categories refreshed on `NotificationSettingsChanged`. Option (b) is also cheaper at the call site (no lock on the hot path at all). Decision point: commit 2.
- **`AutomationFired` correlation ID** (`serial: t` monotonic): cheap on the session side, lets receivers `replaces_id` back-to-back lock/unlock storms (KDE screensaver bounces). Probably defer to v2 unless we hit the bounce-storm problem in field testing. Decision point: commit 3.

### 💡 UX judgment calls (need product decision)

- **Urgency mapping rethink**: reviewer argues current Critical-heavy mapping will annoy. Suggested: `tv_connection` Low for both, `tv_pairing` Normal for both, `tv_wake` Low / Normal, reserve `Critical` *only* for `daemon_offline`. Rationale: pairing and wake are usually user-initiated foreground operations — the user already knows they triggered them.
- **Defaults rethink**: if the urgency tightening above lands, current defaults are fine. Otherwise reviewer recommends conservative defaults: only `daemon_offline` + `tv_pairing` on by default, others off with a "want more?" nudge in the settings pane.
- **`failures_only: bool` sub-toggle per category**: addresses the likely top user request ("tell me when wake fails but stay quiet on success") at lower UX cost than splitting `tv_pairing` / `tv_wake` into success/failure categories. Worth considering for v1.
- **"Send test notification" button on the settings pane**: ~one-line cost, big discoverability win. Without it, users can't distinguish "category disabled" from "DE notification stack broken." Recommend in.
- **`config_reloaded` category (default off)**: for power users hand-editing the TOML, surfaces a "Reload applied: 2 profiles, 1 TV updated" toast as a feedback signal. Cheap, opt-in by default — adds one category, no architectural change.
- **Notification body copy patterns**: reviewer sketched concrete summary/body strings per category — see review notes (not duplicated here to avoid drift). Lock in the copy in commit 2 (`Notifier` module) so it's reviewed alongside the emit-site code.
- **Settings-pane offline copy**: prefer "Tray service is not running — start it to change these settings" over "settings are read-only" (clearer about cause).

## References

- `notify-rust` crate: `~/ws/lgtv/libs/notify-rust` (vendored) — see `examples/` for the `Notification::new().summary(...).body(...).urgency(...).show()` pattern.
- Mirrored pattern: [`crates/tisvlay-session/src/session_dbus.rs`](../../crates/tisvlay-session/src/session_dbus.rs) and [`crates/tisvlay-session/src/lib.rs`](../../crates/tisvlay-session/src/lib.rs) for the validate→mutate→persist→emit shape and `ReloadConfig` diff-and-emit logic.
- Existing tray D-Bus stub: [`crates/tisvlay-tray/src/dbus.rs`](../../crates/tisvlay-tray/src/dbus.rs) — comment block at lines 12–13 pre-allocates this work.
- Settings pane today: [`crates/tisvlay-gui/ui/components/settings_pane.slint`](../../crates/tisvlay-gui/ui/components/settings_pane.slint) — comment at lines 5–8 explicitly anticipates a "show tray notifications" toggle here.

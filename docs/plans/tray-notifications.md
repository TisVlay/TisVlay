# Tray desktop notifications

Add user-configurable desktop notifications to TisVlay so that background events on the daemon side surface on the desktop. Owner is `tisvlay-tray`. Settings are mutable both via D-Bus (GUI / `busctl`) and via editing `~/.config/tisvlay/tray.toml` directly; both paths surface the change via `org.freedesktop.DBus.Properties.PropertiesChanged` for `NotificationSettings` so subscribers stay in sync.

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
         │ PropertiesChanged      │ Session1.             │
         │ (NotificationSettings) │ AutomationFired       │
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
4. **Emit** `org.freedesktop.DBus.Properties.PropertiesChanged` for `NotificationSettings` after persistence completes (zbus auto-emits on D-Bus property writes; `ReloadConfig` emits programmatically). Subscribers re-fetch into a guaranteed-consistent state.

## Wire surface

Service `org.tisvlay.Tray`, path `/org/tisvlay/Tray`, interface `org.tisvlay.Tray1`.

### Properties

| Name | Type | Notes |
|---|---|---|
| `Version` | `s` | existing, const |
| `ConfigPath` | `s` | existing; now resolves to `tray.toml` (today returns empty) |
| `NotificationSettings` | `a{sv}` (dict-derive `NotificationSettingsBody`) | full body; auto-emits `org.freedesktop.DBus.Properties.PropertiesChanged` on every write (default zbus behavior — no `emits_changed_signal` override) |

`NotificationSettingsBody` is a `SerializeDict`/`DeserializeDict`/`Type` struct mirroring the fields of `NotificationSettings` (typed bools, not a free-form map). Matches the existing `UserProfileBody` / `SystemProfileBody` shape and gives forward-compat for non-bool fields (rate limits, per-TV scoping) without a wire migration.

### Methods

| Name | Args | Returns | Notes |
|---|---|---|---|
| `UpdateNotificationSettings` | `a{sv}` (`NotificationSettingsBody` subset) | `()` | atomic multi-key update; absent keys leave existing values intact; unknown keys → `org.freedesktop.DBus.Error.InvalidArgs`. Sole mutator — matches `Session1.UpdateUserProfile`'s single-mutator pattern (busctl ergonomics for one key: `Update '{"tv_wake":true}'`) |
| `ReloadConfig` | — | `()` | re-reads disk, diffs against prior, programmatically emits `PropertiesChanged` for `NotificationSettings` if any key changed (zbus auto-emit only fires on D-Bus-driven property writes, not in-memory state mutations) |
| `SendTestNotification` | — | `()` | emit a single `Normal`-urgency test notification (summary: *"Test notification"*, body: *"TisVlay can reach your desktop's notification service."*). Bypasses the category bitset gate so it works even when every category is off — its job is to verify the notification stack. Backs the GUI's "Send test notification" button |

### Signals

No custom signals. Property changes propagate via the standard `org.freedesktop.DBus.Properties.PropertiesChanged` — auto-emitted by zbus on D-Bus property writes, manually emitted by the `ReloadConfig` handler. The GUI subscribes to `PropertiesChanged` filtered on `interface_name == "org.tisvlay.Tray1"` + `property_name == "NotificationSettings"`.

### Error names

Prefix `org.tisvlay.Tray1.Error.*`, plus the standard `org.freedesktop.DBus.Error.InvalidArgs` for unknown category keys (matches the convention used in `SessionError::InvalidArgs`).

| Variant | Maps to |
|---|---|
| `IoError(String)` | `TrayConfig::save_to` failure after rollback |
| `OperationFailed(String)` | TOML serialization failures and other unexpected internals |
| `InvalidArgs(String)` | unknown category key on update (standard `freedesktop.DBus.Error.InvalidArgs` wire name) |

## Categories

Stable snake_case keys (these are the field names in `NotificationSettingsBody`):

| Key | Trigger | Default | Urgency | Tray-side source |
|---|---|---|---|---|
| `tv_connection` | TV `connected` transitions true ↔ false | **on** | `Low` (both) | `refresh_state` diff |
| `tv_pairing` | terminal pairing phase `complete` / `failed` | **on** | `Normal` (both) | `Manager1.PairingProgress` (already watched by `StateWatcher`) |
| `tv_wake` | `Tv1.Power.PowerCycleProgress` reaches `connected` / `failed` after a `wol_sent` | **on** | `Low` (success) / `Normal` (failure) | per-TV `Tv1.Power.PowerCycleProgress` stream (see "Per-TV power-cycle subscription" below) |
| `automation_fired` | session action ran on a TV | **off** | `Low` | new `Session1.AutomationFired` signal |
| `config_reloaded` | hand-edit + `ReloadConfig` produces a non-trivial diff | **off** | `Low` | `StateEvent::SystemProfileChanged` / `UserProfileChanged` / `TvUserProfileChanged` / `TrayNotificationSettingsChanged` (already in `StateWatcher` after this plan lands) |
| `daemon_offline` | `StateEvent::DaemonDisappeared` | **on** | `Critical`, hint `resident` (sticky) | existing tray arm |

Urgency rationale: `Critical` is reserved for the single event that genuinely needs user acknowledgment (`daemon_offline`). Pairing and wake are usually user-initiated foreground operations — `Normal` for failures gives them banner visibility without escalating to a sticky toast. `tv_connection` stays `Low` for both directions because the daemon auto-reconnects; sustained-disconnect detection (>30s) is a v2 idea that would need debouncing in the diff helper.

`automation_fired` default-off matches the earlier "noisiest one off" answer — it can fire many times per day on a managed TV. `config_reloaded` is a power-user feedback signal (was your hand-edit picked up?) so default-off keeps it invisible to normies.

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
    pub config_reloaded: bool,
    pub daemon_offline: bool,
}

impl Default for NotificationSettings { /* all true except automation_fired + config_reloaded */ }

impl TrayConfig {
    pub fn config_path() -> Result<PathBuf>;        // ~/.config/tisvlay/tray.toml
    pub fn load() -> Result<Option<Self>>;
    pub fn load_from(path: &Path) -> Result<Option<Self>>;
    pub fn save(&self) -> Result<()>;
    pub fn save_to(&self, path: &Path) -> Result<()>;
}

impl NotificationSettings {
    pub fn to_body(&self) -> NotificationSettingsBody;                    // for the property getter
    pub fn apply(&mut self, body: &NotificationSettingsBody) -> Result<(), String>;
    pub fn enabled_bitset(&self) -> u8;                                   // Notifier hot-path cache
}

// Wire body — typed dict-derive, mirrors NotificationSettings field names.
// Optional fields: absent on the wire means "no change" for Update; absent
// in the property getter means "category not present in this version" (forward
// compat).
#[derive(SerializeDict, DeserializeDict, Type, Debug, Clone, Default)]
#[zvariant(signature = "a{sv}")]
pub struct NotificationSettingsBody {
    pub tv_connection: Option<bool>,
    pub tv_pairing: Option<bool>,
    pub tv_wake: Option<bool>,
    pub automation_fired: Option<bool>,
    pub config_reloaded: Option<bool>,
    pub daemon_offline: Option<bool>,
}
```

Follows `UserConfig`'s missing-file → `Ok(None)` convention; `Default::default()` is the fallback. `to_body` / `apply` keep the wire ↔ struct boundary in one place so the D-Bus handler stays thin. `enabled_bitset` returns the 8-bit pack consumed by `Notifier`'s `AtomicU8` cache — bit positions are the field declaration order.

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
    pub async fn notification_settings(&self) -> Result<NotificationSettingsBody>;
    pub async fn update_notification_settings(&self, updates: NotificationSettingsBody) -> Result<()>;
    pub async fn reload_config(&self) -> Result<()>;
    pub fn connection(&self) -> &zbus::Connection;               // for PropertiesChanged subscription
}
```

Extend the existing `TisvlayTrayProxy` (currently a no-method stub used only for version probing in the GUI's About pane) with the new property + `UpdateNotificationSettings` method + `ReloadConfig` method. No custom signal — the GUI subscribes to `PropertiesChanged` via the standard `org.freedesktop.DBus.Properties` proxy.

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

Both new variants need explicit `StateWatcher` plumbing — neither is a one-line subscription:

1. **`Tray1` `PropertiesChanged` for `NotificationSettings`** — new `tray_properties_stream` filtered on `interface_name == "org.tisvlay.Tray1"` + property `NotificationSettings` in the changed-properties dict. Today `StateWatcher` only watches `Tray` *bus-name* lifecycle (`tray_watch` at `dbus_client.rs:2800`); there is no `tray_signal_stream` or property-change stream parallel to `session_signal_stream`. Pattern follows the `session_signal_stream` shape at `dbus_client.rs:2820-2860`: new internal enum, new match arm, new `next()` branch around `dbus_client.rs:2955`. Const path/interface need exporting from `dbus_paths.rs` (`TRAY_SERVICE` exists; `TRAY_INTERFACE`/`TRAY_PATH` need adding). Listening to `PropertiesChanged` rather than a custom signal is more code in `StateWatcher` but zero extra zbus interface surface and zero extra subscription on the GUI side (it already speaks the `org.freedesktop.DBus.Properties` proxy elsewhere).
2. **`Session1.AutomationFired`** — extends the existing `session_signal_stream`: new `SessionSignal::AutomationFired { tv_id, event_kind, action_kind }` variant, new match arm in the filter block at `dbus_client.rs:2834`, new `next()` branch.

`StateEvent` convention today is payload-less (subscribers re-query state). `TrayNotificationSettingsChanged` follows that convention — the GUI re-fetches via `TrayClient::notification_settings`. `SessionAutomationFired` is the deliberate exception: it represents an *ephemeral event*, not a state change — there is nothing to re-query (the session service doesn't persist "last automation fired"), so the payload travels in-band. The three fields are stable snake_case identifiers (callable by `gettext("automation.event.{event_kind}")`-style key lookup, not interpolated into translated strings) so future enum extensions don't break wire compat.

Existing tray + GUI `match` blocks add new arms; today the tray's `TrayAppeared`/`TrayDisappeared` arms already no-op for self-events.

### `tisvlay-tray/src/dbus.rs` (extend `Tray1`)

Existing `TrayService` gains:

```text
pub(crate) struct TrayService {
    config: Arc<RwLock<TrayConfig>>,    // new — single source of truth on disk
    enabled_bits: Arc<AtomicU8>,        // new — Notifier hot-path cache; mirror of config.notifications
    config_path: Option<PathBuf>,       // existing, now load-bearing
}
```

Methods implemented in the validate→mutate→persist→rollback shape from the canonical helpers in `tisvlay-session/src/lib.rs` (`update_user_profile`, `create_user_profile`, `delete_user_profile`) — note `session_dbus.rs::set_tv_user_profile` *omits* rollback (only warns on save failure), so it is not the pattern to copy here.

`UpdateNotificationSettings` flow:
1. Validate body (zbus dict-derive rejects malformed `Variant` types automatically; unknown fields would have been caught at the body's type boundary).
2. Acquire write lock on `config`.
3. Snapshot `config.notifications` (`NotificationSettings: Clone`).
4. `config.notifications.apply(&body)` — returns `Err` only if a value is type-incompatible after dict-decode (defensive; should be impossible).
5. `config.save_to(path)` — on `Err`, restore snapshot, drop write lock, return `IoError`.
6. `enabled_bits.store(config.notifications.enabled_bitset(), Ordering::Release)` — fence makes the new bitset visible to `Notifier` before the next emit.
7. Drop write lock. zbus auto-emits `PropertiesChanged` for `NotificationSettings`.

`ReloadConfig` flow: load disk → compare to in-memory → if any diff, replace config + update `enabled_bits` → call `interface_ref.notification_settings_changed(&context).await?` to programmatically emit `PropertiesChanged` (auto-emit only fires on D-Bus-driven writes).

### `tisvlay-tray/src/notifier.rs` (new)

```text
pub(crate) struct Notifier {
    enabled_bits: Arc<AtomicU8>,        // shared with TrayService; lock-free check
    last_failure_at: Mutex<Option<Instant>>,  // 5s connect-failure backoff
}

impl Notifier {
    pub fn new(enabled_bits: Arc<AtomicU8>) -> Self;
    pub async fn notify(
        &self,
        category: NotificationCategory,
        summary: String,
        body: String,
        urgency: Urgency,
    );
}

pub(crate) enum NotificationCategory {
    TvConnection      = 0,
    TvPairing         = 1,
    TvWake            = 2,
    AutomationFired   = 3,
    ConfigReloaded    = 4,
    DaemonOffline     = 5,
}
```

`notify` flow: `if enabled_bits.load(Ordering::Acquire) & (1 << category as u8) == 0 { return; }` — a single atomic read with no lock, no Arc clone, on the hot path. The bitset mirrors `NotificationSettings` field declaration order; the `TrayService` updates it under the write lock after each successful `UpdateNotificationSettings` / `ReloadConfig`. Stale-read window between the user toggling a category and the new bit being visible is at most one notification — acceptable.

Notifier connection is implicit in `notify-rust`'s API (each `Notification::show()` opens or reuses the session-bus connection). Connect failures (notification server not yet ready on slow logins) are recorded in `last_failure_at`; subsequent calls within ~5s short-circuit without attempting `show()` so initial bursts don't all stall.

App name `"TisVlay"`, icon name `"video-display"` (matches the tray's active-state icon — DE notification history will group under TisVlay's app entry). Set `category("device")` hint so DEs that group/filter by category route TisVlay notifications sensibly.

### `tisvlay-tray/src/main.rs`

- Construct `TrayConfig` (and the matching `enabled_bits: Arc<AtomicU8>` initialized from `config.notifications.enabled_bitset()`) during startup after `bindtextdomain`. Pass both to `dbus::register(...)`. Tray's `register` signature gains `config: Arc<RwLock<TrayConfig>>, enabled_bits: Arc<AtomicU8>`.
- Build the `Notifier` from `enabled_bits` alone (no `RwLock` reference — that's the whole point of the bitset). Share with `refresh_state`, the daemon-offline arm, the per-TV power-cycle subscriptions, and the `automation_fired` listener.
- In `refresh_state`, compute the diff *inside* the `handle.update(|t| { let events = diff_for_notifications(&t.tvs, &new_tvs); t.tvs = new_tvs; events })` closure (see Concurrency notes for why), then dispatch the returned events to `Notifier` after the closure returns. The diff helper lives in `state.rs` so it's pure and unit-testable.
- In the `StateEvent::DaemonDisappeared` arm, additionally call `notifier.notify(DaemonOffline, …)`.
- New per-TV tokio tasks subscribed to `Tv1.Power.PowerCycleProgress` → `notifier.notify(TvWake, …)` on `connected` / `failed` terminal phases. Subscribed on each TV's `InterfacesAdded` (or initial `list_tvs` after `DaemonAppeared`), dropped on `InterfacesRemoved` / `DaemonDisappeared`. See "Per-TV power-cycle subscription" in Concurrency notes for the lifecycle map.
- New tokio task subscribed to `Session1.AutomationFired` → `notifier.notify(AutomationFired, …)`. The session-service `is_available()` probe pattern from `main.rs:100-110` handles "session not yet up" cleanly.
- New tokio task watching `StateEvent::{System,User,TvUser}ProfileChanged` / `…Added` / `…Removed` → `notifier.notify(ConfigReloaded, …)` with a debounce window (~500ms) so a `ReloadConfig` that touches multiple profile rows surfaces as one notification, not five.

### `tisvlay-session` (one new signal)

`Session1.AutomationFired(tv_id: s, event_kind: s, action_kind: s)`. Fired from `event_handler` in `lib.rs:289` after `execute_action_for_tv` returns `Ok(())`, inside the per-TV `for (tv_id, action)` loop. Both `event_kind` and `action_kind` are **stable snake_case identifiers** intended as gettext lookup keys (`automation.event.session_locked`, `automation.action.screen_off`), **not** translated strings and **not** Debug-formatted values.

- `event_kind` is the existing `SessionEvent::Display` impl (already snake_case: `session_locked`, `session_unlocked`, `idle_timeout`, …).
- `action_kind` requires either a new `SessionAction::Display` impl returning snake_case (`screen_off`, `screen_on`, `power_off`, `power_on`, `none`) or a dedicated `fn kind(&self) -> &'static str`. The existing `Debug` impl is unstable across refactors and renders poorly in toast bodies (`"PowerOff { force: false }"`); the new helper is a one-screen patch and lives next to the enum in `tisvlay-core`.

Purely additive on the wire — existing subscribers ignore it.

### `tisvlay-gui`

- `crates/tisvlay-gui/ui/globals/tray.slint` (new): mirrors `globals/daemon.slint`'s shape — exposes a `notifications` struct property mirroring `NotificationSettingsBody`'s field names, a `set-notification(category, enabled)` callback (Rust-side: builds a single-field body and calls `TrayClient::update_notification_settings`), and a `send-test-notification()` callback.
- `crates/tisvlay-gui/src/state_watch.rs`: handle `StateEvent::TrayNotificationSettingsChanged` by re-fetching via `TrayClient::notification_settings` and pushing into the Slint global. Source is the `org.freedesktop.DBus.Properties.PropertiesChanged` subscription added in `StateWatcher` above.
- `crates/tisvlay-gui/src/lib.rs`: wire `Tray.set-notification` and `Tray.send-test-notification` callbacks. On error, route through a new `Errors.settings` slot to keep the inline-error surface pane-scoped (parallels existing `Errors.tvs` / `Errors.profiles`).
- `crates/tisvlay-gui/ui/components/settings_pane.slint`: drop the placeholder text, add a `NOTIFICATIONS` `Group` (heading copy: *"Notify me about"*) with six `FormRow { ... Switch }` lines + a "Send test notification" `Button` row below:

  | Label | Helper |
  |---|---|
  | `TV connection` | `Notify when a TV connects or disconnects.` |
  | `Pairing outcome` | `Notify when pairing or re-pairing completes or fails.` |
  | `Wake outcome` | `Notify when a Wake-on-LAN attempt succeeds or fails.` |
  | `Automation` | `Notify when a session action (lock, unlock, idle) fires on a TV.` |
  | `Config reloaded` | `Notify when the daemon or session config reloads from disk.` |
  | `Daemon offline` | `Notify when the connection to the daemon is lost.` |

  Test-notification button label: *"Send test notification"*; helper: *"Verify your desktop's notification stack is working."* Fires a single `Normal`-urgency notification via the tray (new `Tray1.SendTestNotification` method — bypasses category gating but otherwise routes through `Notifier`).

Pane shows "Tray service is not running — start it to change these settings." empty state when `Tray.available == false` — mirrors `TvList`'s "Daemon offline" branch.

## Concurrency / lifecycle notes

- **Per-TV power-cycle subscription (for `tv_wake`).** `CachedTvState` doesn't carry `power_cycle_phase` — diffing it against the new state wouldn't reveal `wol_sent → connected` vs `wol_sent → failed` transitions, only the post-cycle `connected` flip. The `notify-rust` path needs the per-TV `Tv1.Power.PowerCycleProgress` signal stream directly. The proxy + `power_cycle_progress_stream` helper already exists at `crates/tisvlay-core/src/dbus_client.rs:1375`, but the tray needs subscription lifecycle: `subscribe(uuid)` on each `InterfacesAdded` for that TV (or on `DaemonAppeared` + the initial `list_tvs`), `drop(uuid)` on `InterfacesRemoved` / `DaemonDisappeared`. A `HashMap<String, JoinHandle<()>>` keyed by TV UUID, owned by the watcher task, handles this. Each per-TV task reads the stream, emits to the same `Notifier` (gated by `tv_wake`), and exits on stream end.

- **Diff baseline must be the prior `tray.tvs` inside `handle.update`'s closure, not a pre-await snapshot.** `RefreshTrigger` (`crates/tisvlay-core/src/refresh_trigger.rs:62-116`) already prevents concurrent re-entry via an `in_flight: AtomicBool` — a second `fire()` sets `dirty` and returns. The actual race that the diff must guard against is `StateEvent::DaemonDisappeared` at `main.rs:383-390`, which clears `tray.tvs` *outside* the trigger path. Without care, the next refresh after a daemon flap would see "all TVs reappeared" and emit a `tv_connection` notification storm. Fix: compute the diff *inside* the `handle.update(|t| { let events = diff_for_notifications(&t.tvs, &new_tvs); t.tvs = new_tvs; events })` closure where the old `t.tvs` is the authoritative baseline, then dispatch the returned events to `Notifier` after the closure returns.
- **Lock discipline**: `Notifier::notify` never touches the `RwLock<TrayConfig>` — it reads the shared `Arc<AtomicU8>` bitset with a single `Ordering::Acquire` load. The write side (D-Bus `UpdateNotificationSettings` / `ReloadConfig`) holds the write lock across the `save_to` IO + the atomic `enabled_bits.store(…, Release)` immediately after, ensuring the new bitset is published before zbus auto-emits `PropertiesChanged`. There is no path where `Notifier::notify` can be holding the lock when an `UpdateNotificationSettings` is in flight, so the write side never blocks on the notification hot path.
- **Notification server availability**: `notify-rust`'s `Notification::show()` is async-blocking on the session bus call. If `org.freedesktop.Notifications` isn't yet owned (very early login), the call errors. The `Notifier` caches the last failure timestamp; subsequent calls within ~5s drop silently rather than re-erroring. After 5s a fresh attempt is made.
- **Burst protection**: A single state transition can in principle fire multiple notifications (a TV disconnecting cascades into a power-cycle-failed). The diff helper is deduplicating by `(tv_id, category)` per refresh — at most one notification per TV per category per refresh tick.
- **Single-writer for `tray.toml`**: only the tray binary writes this file. `ReloadConfig` always reads, never writes back. A hand-edit during a live `UpdateNotificationSettings` call is at worst a wasted RPC (the next ReloadConfig picks up the user's edit).

## i18n

All notification summaries / bodies and the new settings-pane labels pass through `gettext("…")` (tray side) or `@tr(...)` (GUI side). No new gettext infrastructure — the `tisvlay-tray` / `tisvlay-gui` catalogs already exist. After string changes land, `just i18n-extract` refreshes the `.pot` template.

Notification format strings use the `gettext("Disconnected: {0}").replace("{0}", &tv.name)` pattern (per CLAUDE.md i18n rule — `format!()` strings can't be extracted by `xgettext`). TV names are user-provided so they pass through verbatim.

## Testing

- **Unit (`tisvlay-tray::config`)**: round-trip a `TrayConfig` through `save_to`/`load_from`, missing-file → `Ok(None)`, `to_body` ↔ `apply` symmetry, `enabled_bitset` produces stable bit positions matching the declaration order, unknown-key `apply` → `Err`.
- **Unit (`tisvlay-tray::notifier`)**: diff helper — empty→non-empty (no notification, initial sync), connect transition, disconnect transition, dedup of cascade transitions in a single refresh, bitset gating (notify no-ops when the relevant bit is 0). `tv_wake` cases (`wol_sent → connected` vs `wol_sent → failed`) test against the per-TV power-cycle-stream handler, not the `CachedTvState` diff.
- **Integration (`tisvlay-tray/tests/notifications_dbus.rs` new)**: drive `Tray1.UpdateNotificationSettings` via a `TestTray` harness, assert `PropertiesChanged` fires with the new body, assert `tray.toml` round-trips. Drive `ReloadConfig` after a hand-edit, assert `PropertiesChanged` fires. Mock `notify-rust` is out of scope — the notifier behind a trait + a `NoopNotifier` in test mode keeps the test from depending on a real `org.freedesktop.Notifications` peer.
- **Hand-edit smoke (manual)**: write a hand-modified `tray.toml`, call `Tray1.ReloadConfig` via `busctl`, observe `PropertiesChanged` payload matches the file.

## Commit shape

Three commits, ordered so each commit is independently runnable + `busctl`-pokeable:

1. **`feat(tray,core,gui): TrayConfig + notification settings D-Bus surface`** — `tisvlay-tray::config`, `TrayClient` in `tisvlay-core`, extended `Tray1` interface (`NotificationSettings` property + `UpdateNotificationSettings` + `ReloadConfig` + `SendTestNotification`), tray loads config on startup, `StateWatcher` extension for `Tray1` `PropertiesChanged`. GUI `NOTIFICATIONS` group + test-notification button lands here too so the wire surface is exercised end-to-end (no orphan plumbing). No emission yet; settings pane can toggle bools and `SendTestNotification` round-trips through `Notifier` as a stub that always emits.
2. **`feat(tray): emit desktop notifications on state transitions`** — full `Notifier` module (bitset gate + 5s backoff), `refresh_state` diff helper inside `handle.update` closure, `DaemonDisappeared` hook, per-TV `Tv1.Power.PowerCycleProgress` subscription lifecycle, `config_reloaded` debounced watcher. Covers five of six categories — everything that doesn't require a daemon-side signal addition.
3. **`feat(session,core,tray): AutomationFired signal + notification`** — session-side signal emit at `event_handler`, `SessionAction::Display` (snake_case `kind`) helper in core, `StateWatcher::SessionSignal::AutomationFired` plumbing, tray listener. Smallest of the three; cleanly skippable if we decide to defer.

## Open questions

- **Settings-pane availability when tray is offline**: confirmed wording is *"Tray service is not running — start it to change these settings."* but should the switches *also* be disabled vs hidden entirely while the pane shows this empty state? Plan currently says disabled inline with the empty-state copy — confirm before the Slint scaffold lands.

## References

- `notify-rust` crate: `~/ws/lgtv/libs/notify-rust` (vendored) — see `examples/` for the `Notification::new().summary(...).body(...).urgency(...).show()` pattern.
- Mirrored pattern: [`crates/tisvlay-session/src/session_dbus.rs`](../../crates/tisvlay-session/src/session_dbus.rs) and [`crates/tisvlay-session/src/lib.rs`](../../crates/tisvlay-session/src/lib.rs) for the validate→mutate→persist→emit shape and `ReloadConfig` diff-and-emit logic.
- Existing tray D-Bus stub: [`crates/tisvlay-tray/src/dbus.rs`](../../crates/tisvlay-tray/src/dbus.rs) — comment block at lines 12–13 pre-allocates this work.
- Settings pane today: [`crates/tisvlay-gui/ui/components/settings_pane.slint`](../../crates/tisvlay-gui/ui/components/settings_pane.slint) — comment at lines 5–8 explicitly anticipates a "show tray notifications" toggle here.

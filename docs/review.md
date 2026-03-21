# Phase 4C Code Review

Scope: commits from `240e4ac` to HEAD (Phase 4C: system vs user config split, logind
integration, session D-Bus, boot-wake refactor, tray refresh button).

Files with the most significant changes:
- `crates/tisvlay-core/src/config.rs` (+1395 lines)
- `crates/tisvlay-core/src/dbus_client.rs` (+151 lines, new)
- `crates/tisvlayd/src/logind.rs` (+331 lines, new)
- `crates/tisvlayd/src/tv_manager.rs` (+250 lines)
- `crates/tisvlayd/src/dbus/manager.rs` (+62 lines)
- `crates/tisvlay-session/src/main.rs` (+366 lines)
- `crates/tisvlay-session/src/session_dbus.rs` (+221 lines, new)
- `crates/tisvlay-tray/src/main.rs` (+113 lines)

---

## Critical Bugs

### 1. `reload_config` does not update daemon settings

**File:** [crates/tisvlayd/src/tv_manager.rs:882-944](crates/tisvlayd/src/tv_manager.rs#L882-L944)

`TvManager::reload_config()` updates `self.tvs` and `self.profiles` (the `RwLock`-wrapped
`HashMap`), but `self.config` (which holds `DaemonConfig` with `on_shutdown`,
`on_no_active_session`, `wake_on_boot`, and `wake_delay_ms`) is a plain immutable field.
There is no interior mutability on it, so it cannot be updated at runtime.

After a SIGHUP or D-Bus `ReloadConfig`, changes to `[daemon]` in the config file are
silently ignored. The daemon must be restarted for these settings to take effect, but
there is no warning about this in the SIGHUP handler or in logs.

The same structural problem means `set_daemon_option` (which saves to disk) also never
updates `self.config.daemon` in memory. The value written to disk has no effect until
the next restart.

**Fix direction:** Wrap `config.daemon` in a `RwLock<DaemonConfig>`, or factor daemon
settings out of `Config` into a separately-lockable struct.

> **Fixed.** `config: Config` has been replaced with `daemon_config: RwLock<DaemonConfig>`
> as a separate field on `TvManager`. `reload_config()` now writes the new `DaemonConfig`
> into `daemon_config` after updating TVs and profiles. All callers that previously read
> `self.config.daemon.*` now read from `self.daemon_config.read().await`.

---

### 2. `set_daemon_option` leaves in-memory state stale

**File:** [crates/tisvlayd/src/tv_manager.rs:774-814](crates/tisvlayd/src/tv_manager.rs#L774-L814)

`set_daemon_option` loads from disk, modifies, saves to disk, then returns. It never
touches `self.config.daemon`. `set_tv_option` correctly updates both disk AND in-memory
state for TV configs. The two methods are inconsistent.

Additionally, `set_daemon_option` is declared as a **synchronous** `fn` (not `async fn`)
but performs blocking disk I/O (`Config::load_from` → `std::fs::read_to_string`,
`Config::save_to` → `std::fs::write`). In a tokio context this blocks the async executor
thread. The same blocking I/O issue affects `save_tv_config` and `delete_tv_config`.
Use `tokio::task::spawn_blocking` or make these async.

> **Fixed.** `set_daemon_option` is now `async fn`. It writes the validated value into
> `self.daemon_config` (under a write lock, released before I/O), then persists using
> `tokio::task::spawn_blocking`. `save_tv_config` and `delete_tv_config` are also now
> `async fn` using `spawn_blocking` for their disk I/O.

---

### 3. `check_active_sessions` fires incorrectly on `SessionNew`

**File:** [crates/tisvlayd/src/logind.rs:225-250](crates/tisvlayd/src/logind.rs#L225-L250)

When `SessionNew` fires (a user is logging in), `check_active_sessions` is called
immediately. The newly created session may not yet have `Active = true` at the moment
the D-Bus round-trips complete (the session object exists but the seat hasn't switched
yet). If no other session is active at that instant, `on_no_active_session` fires — the
daemon may turn off TVs in the middle of a login transition.

`on_no_active_session` is a "no active user" condition; it should only be checked when
sessions disappear, not when they appear. Checking on `SessionNew` adds a race window
during the login flow.

**Fix direction:** Remove `check_active_sessions` from the `SessionNew` branch. Only run
it on `SessionRemoved`.

> **Fixed.** The `check_active_sessions` call has been removed from the `SessionNew` arm.
> The arm now only logs the event. The check continues to run on `SessionRemoved`.

---

### 4. `list_tv_ids` returns non-deterministic order

**File:** [crates/tisvlayd/src/tv_manager.rs:316-320](crates/tisvlayd/src/tv_manager.rs#L316-L320)

```rust
pub async fn list_tv_ids(&self) -> Vec<String> {
    let tvs = self.tvs.read().await;
    tvs.keys().cloned().collect()
}
```

`HashMap::keys()` has non-deterministic iteration order (Rust uses hash randomization).
The `GetTvs` D-Bus call and the tray's TV list both flow through this method. The tray
menu TV order will change randomly across restarts or between refresh calls, leading to
confusing UX where menu items move around.

The system config's `tv` field is a `Vec<TvConfig>` that preserves insertion order, but
the manager's internal `HashMap` discards it.

**Fix direction:** Change `tvs: HashMap<String, Arc<Mutex<ManagedTv>>>` to an
`IndexMap` (from the `indexmap` crate) to preserve insertion order, or sort the result
by the original config order.

> **Fixed.** `tvs` is now `Arc<RwLock<IndexMap<String, Arc<Mutex<ManagedTv>>>>>`.
> `IndexMap` preserves insertion order, so `GetTvs` now returns TVs in the same order
> they appear in the config file. `indexmap = "2"` added to workspace dependencies.

---

## Design Issues

### 5. `SystemConfig::user_config_path()` naming is inverted

**File:** [crates/tisvlay-core/src/config.rs:469-478](crates/tisvlay-core/src/config.rs#L469-L478)

`SystemConfig` has a method named `user_config_path()` that returns the XDG user path
(`~/.config/tisvlay/config.toml`). `UserConfig::config_path()` is defined to delegate
directly to `SystemConfig::user_config_path()`. Consequently:

- A method named "user config path" lives on the system config struct.
- The system config struct's own "default" path (also via `load()` / `save()`) is the
  user XDG path, not `/var/lib/tisvlay/config.toml`.
- The `Config::config_path()` backward-compat alias further muddies this.

The naming implies `SystemConfig::load()` would load the daemon's config by default, but
it actually loads from `~/.config/tisvlay/config.toml` (the user XDG path). The daemon
only gets `/var/lib/tisvlay/config.toml` because it passes `--config` explicitly.

This inversion will surprise contributors and makes the "which path does this load from"
question hard to answer by reading the code.

**Fix direction:** Move the XDG path helper to `UserConfig` and make it private to that
type. Give `SystemConfig::load()` a clear contract (either hard-code the system path or
require a path argument). Remove the `config_path()` backward-compat alias; it serves no
external caller and perpetuates the confusion.

> **Fixed.** `SystemConfig::user_config_path()` and the `config_path()` alias are removed.
> `UserConfig::config_path()` now owns the XDG path logic directly.
> `SystemConfig::load()` and `save()` now default to `system_config_path()`
> (`/var/lib/tisvlay/config.toml`), giving them a clear contract.
> `tisvlayd --session` mode now uses `UserConfig::config_path()` for its default path.

---

### 6. Missing profile assignment silently falls back to system default, skipping user default

**File:** [crates/tisvlay-core/src/config.rs:675-717](crates/tisvlay-core/src/config.rs#L675-L717)

In `UserConfig::resolve_profile`:

```rust
let user_profile = self.profiles.get(profile_name);
// ...
match (user_profile, user_default) {
    (None, _) => {
        // No user profile found — use system default directly
        system_default.clone()
    }
    // ...
}
```

If a TV is assigned a profile name that does not exist in the user's config (e.g. a
typo, or a system-defined profile name like "gaming" that the admin defines but the user
doesn't), `user_profile` is `None` and the resolution immediately falls back to
`system_default`, **skipping the user's own `[profiles.default]`**.

A user who has customized their default profile (e.g. set `idle_timeout_seconds = 600`)
and then assigns a non-existent profile to a TV will see their user default silently
bypassed. This is a subtle semantic bug that only surfaces in the mixed-inheritance
scenario the phase was designed to enable.

**Fix direction:** In the `(None, _)` arm, if `user_default` is `Some`, merge
`user_default` with `system_default` before resolving, rather than going straight to
`system_default`.

> **Fixed.** The `(None, _)` arm is split into `(None, Some(default))` and `(None, None)`.
> When the named profile is missing but a user `[profiles.default]` exists, the user default
> is merged with the system default before resolving. A regression test
> `test_user_config_resolve_profile_missing_profile_falls_through_user_default` was written
> first (verified to fail before the fix, pass after).

---

### 7. Idle timeout fires at the minimum across all TVs, not per-TV

**File:** [crates/tisvlay-session/src/main.rs:188-196](crates/tisvlay-session/src/main.rs#L188-L196)

`effective_idle_timeout()` returns the minimum non-zero timeout across all TV profiles.
A single `IdleMonitor` is started with this minimum. When `IdleTimeout` fires, the event
handler applies `on_idle_timeout` to every TV using its own resolved profile. This means
a TV configured with a 30-minute idle timeout will receive `on_idle_timeout` at 5
minutes if another TV has a 5-minute timeout.

The intent was "fire at the minimum so no TV is delayed longer than its configured
timeout", but the effect is that all TVs fire at the minimum, making per-TV idle
timeouts meaningless.

This design tradeoff (single shared timer vs. per-TV timers) is not documented in the
code. At minimum the comment should explain why per-TV timers weren't used. If the
tradeoff is unacceptable, per-TV timers can be implemented by starting multiple
`IdleMonitor` instances with different timeouts and different event types.

> **Documented.** Added a doc comment to `effective_idle_timeout()` explaining the
> single-timer-at-minimum design: the timer fires at the minimum non-zero timeout, each
> TV still applies its own `on_idle_timeout` action, and per-TV timers are noted as a
> possible improvement for setups where the early-fire behavior matters.

---

### 8. `IdleResume` is hardcoded to `ScreenOn`, inconsistent with profile-driven design

**File:** [crates/tisvlay-session/src/main.rs:456](crates/tisvlay-session/src/main.rs#L456)

```rust
SessionEvent::IdleResume => SessionAction::ScreenOn, // Always resume from idle
```

Every other session event (lock, unlock, sleep, wake, idle timeout, greeter wake) is
configurable via a profile field. `IdleResume` is hardcoded and cannot be customized. A
user who sets `on_idle_timeout = "power_off"` will have their TV powered off at idle, but
when activity resumes the TV turns on rather than respecting any policy (e.g. "none" for
a gaming setup that doesn't want automatic screen-on).

**Fix direction:** Add `on_idle_resume: Option<SessionAction>` to `ProfileConfig` with a
default of `ScreenOn`.

> **Fixed.** Added `on_idle_resume: Option<SessionAction>` to `ProfileConfig` and
> `on_idle_resume: SessionAction` (default `ScreenOn`) to `ResolvedProfile`. Updated
> `full_default`, `merge_with_base`, and `resolve` accordingly. `get_action_for_event`
> in `tisvlay-session` now reads `profile.on_idle_resume` instead of the hardcoded value.
> A failing test `test_on_idle_resume_configurable` was written first (verified to fail
> before the fix, pass after).

---

### 9. `DaemonAction` enum leaks implementation detail from `logind` into `tv_manager`

**File:** [crates/tisvlayd/src/logind.rs:303-307](crates/tisvlayd/src/logind.rs#L303-L307)
**File:** [crates/tisvlayd/src/tv_manager.rs:204-215](crates/tisvlayd/src/tv_manager.rs#L204-L215)

`DaemonAction` is declared in `logind.rs` and imported by `tv_manager.rs` as
`crate::logind::DaemonAction`. The `TvManager::get_daemon_config_action` method takes
this logind-specific enum and pattern-matches it to config fields.

The abstraction is backwards: `tv_manager` shouldn't import a type from `logind`. Either
make `TvManager` expose the `DaemonConfig` directly (via a `get_daemon_config()` method
or by making the field `pub`), or move `DaemonAction` to a shared types module. The
current dependency direction is `logind → tv_manager → logind (via DaemonAction)`.

> **Fixed.** `DaemonAction` moved to `tv_manager.rs` (where `get_daemon_config_action`
> lives). `logind.rs` now imports it as `crate::tv_manager::DaemonAction`. Dependency
> direction is now correct: `logind → tv_manager`.

---

### 10. `logind` monitor doesn't check initial session state at startup

**File:** [crates/tisvlayd/src/logind.rs:200-250](crates/tisvlayd/src/logind.rs#L200-L250)

`handle_session_changes` subscribes to `SessionNew`/`SessionRemoved` signals but never
checks the current session state at startup. If the daemon is started when no user
sessions exist (e.g. at boot before the DM has started, or after a system-level graphical
session has ended), `on_no_active_session` will not fire until a subsequent
session-change signal is received.

**Fix direction:** Call `check_active_sessions` once at the start of
`handle_session_changes`, before entering the event loop.

> **Fixed.** `check_active_sessions` is now called once immediately after subscribing to
> the signals, before entering the select loop.

---

## Correctness Concerns

### 11. `power_off` marks TV as `powered_off` even if the command fails

**File:** [crates/tisvlayd/src/tv_manager.rs:497-515](crates/tisvlayd/src/tv_manager.rs#L497-L515)

```rust
pub async fn power_off(&self, tv_id: &str) -> Result<(), TvManagerError> {
    let result = self.with_connection(tv_id, ...).await;

    // This runs regardless of whether the command succeeded
    {
        let mut tv = tv_arc.lock().await;
        tv.connection = None;
        tv.powered_off = true;
    }

    result
}
```

The `powered_off` flag is set unconditionally, even when `result` is `Err`. If the TV
rejected the power-off command (e.g. network timeout), it's still physically on but the
daemon now refuses to reconnect to it (future `with_connection` calls return
`TvManagerError::PoweredOff`). The TV is stuck in a bad state until the user manually
calls `power_on`.

**Fix direction:** Only set `powered_off = true` on `Ok(())`. On error, leave the state
unchanged so auto-reconnect can recover.

> **Fixed.** `tv.powered_off = true` is now gated on `result.is_ok()`. The connection is
> still cleared unconditionally (the socket is likely broken on failure either way), but
> the daemon will now reconnect on the next command attempt if power-off actually failed.

---

### 12. `set_tv_option` duplicates the field-mapping logic

**File:** [crates/tisvlayd/src/tv_manager.rs:817-880](crates/tisvlayd/src/tv_manager.rs#L817-L880)

There are two identical `match key` blocks: one to modify `config` (for disk) and one to
modify `managed.config` (for memory). If a new field is added to the disk block and
forgotten in the memory block (or vice versa), they silently diverge. The disk-update and
in-memory-update should share the same field-mapping logic.

> **Fixed.** Extracted a free function `apply_tv_option(tv: &mut TvConfig, key, value)`
> containing the single authoritative match. `set_tv_option` calls it once for the
> disk config (with validation) and once for the in-memory config.

---

### 13. Retry task `is_available` calls are two separate round-trips without synchronization

**File:** [crates/tisvlay-session/src/main.rs:272-313](crates/tisvlay-session/src/main.rs#L272-L313)

```rust
let daemon_was_unavailable = !daemon.is_available().await;  // call #1
if daemon_was_unavailable {
    tokio::spawn(async move {
        loop {
            ...
            if !retry_daemon.is_available().await {  // call #2
```

If the daemon becomes available between call #1 and the first sleep in the retry loop,
the initial `SessionState::load` at startup (which already returned an error) is not
retried until the next sleep interval (5 seconds later). This is a minor startup
latency issue, not a correctness bug. But the double-check design means `is_available`
is called twice at startup: once pre-spawn and once in the retry loop. This is a code
smell worth simplifying.

> **Fixed.** The first `is_available()` result is captured in `daemon_available` and
> reused to decide whether to spawn the retry task, eliminating the second call.

---

## Code Style / Idiom Issues

### 14. `use anyhow::Context` import at the bottom of the file

**File:** [crates/tisvlay-session/src/session_dbus.rs:221](crates/tisvlay-session/src/session_dbus.rs#L221)

```rust
// ... 220 lines of code ...
use anyhow::Context;
```

Rust convention is all `use` statements at the top of the module. This import is placed
after the entire impl block, which is likely an oversight from when the `Context` trait
was added after-the-fact. Move it to the top.

> **Fixed.** `use anyhow::Context;` moved to the top of `session_dbus.rs` with the other
> imports and removed from the end of the file.

---

### 15. `save_to` clones the entire config on every call when profiles are non-empty

**File:** [crates/tisvlay-core/src/config.rs:442-464](crates/tisvlay-core/src/config.rs#L442-L464)

```rust
let config_to_save = if self.profiles.is_empty() {
    let mut config = self.clone();
    config.profiles.insert(...);
    config
} else {
    self.clone()   // unnecessary clone for the common case
};
```

The `else` branch clones the entire `SystemConfig` (including the `Vec<TvConfig>` with
vendor credentials) just to serialize it. Use `Cow<'_, SystemConfig>` or serialize
`self` directly and patch the profiles only when empty, to avoid the unconditional clone.

The deeper issue is that `save_to` silently mutates the serialized form (adding a
"default" profile) without reflecting that change in the caller's in-memory state. The
saved file differs from what the caller holds. This is a surprising side-effect for
callers who expect `save_to(path)` to be idempotent.

> **Partially fixed.** Changed to `Cow<'_, SystemConfig>`: the `is_empty` branch clones
> and wraps in `Cow::Owned`; the common path uses `Cow::Borrowed(self)` with no clone.
> The silent in-memory/on-disk divergence is a design choice (self-documenting default
> profile in new configs) and is left as-is with the existing doc comment explaining it.

---

### 16. `TvConfig::with_profile` is `#[deprecated]` and public — remove or privatize

**File:** [crates/tisvlay-core/src/config.rs:332-336](crates/tisvlay-core/src/config.rs#L332-L336)

The `with_profile` builder method and the `profile` field itself are marked deprecated
but remain `pub`. Since this is an internal crate (not a published library), there are no
external semver constraints. The deprecated method adds API surface without value. The
field itself is needed for backward-compatible TOML deserialization, but the builder
method has no use case in new code and should be removed.

> **Fixed.** `TvConfig::with_profile` removed. The `profile` field is retained for
> backward-compatible TOML deserialization.

---

### 17. `SystemConfig::get_profile` and `get_profile_for_tv` share identical fallback logic

**File:** [crates/tisvlay-core/src/config.rs:549-580](crates/tisvlay-core/src/config.rs#L549-L580)

Both methods try the named profile, fall back to "default", then fall back to
`ProfileConfig::full_default()`. `get_profile_for_tv` adds a first step of checking
the deprecated `tv.profile` field. Both methods should delegate to `get_profile` for
the fallback chain rather than duplicating it.

> **Fixed.** `get_profile` now uses `or_else`/`cloned`/`unwrap_or_else` in a single
> expression. `get_profile_for_tv` checks the deprecated `tv.profile` field and, if
> found in the profiles map, delegates to `get_profile`; otherwise also delegates to
> `get_profile("default")`. No duplicated fallback logic remains.

---

### 18. `DaemonClient::with_proxies` holds a read lock across an async `.await`

**File:** [crates/tisvlay-core/src/dbus_client.rs:396-414](crates/tisvlay-core/src/dbus_client.rs#L396-L414)

```rust
let cache = self.tv_proxies.read().await;
let proxies = cache.get(tv_id)...;
f(proxies).await   // <-- read lock held across D-Bus round-trip
```

The `tokio::sync::RwLockReadGuard` is held while a D-Bus call completes. For the current
usage this is safe (the closure never acquires the write lock), but holding a lock
across an async boundary is a well-known footgun in Rust async code. Any future change
that tries to call `reload_config()` (which needs the write lock) from within a `with_proxies`
closure would deadlock.

The lock is held for the entire round-trip latency of a D-Bus call (typically 1–20ms).
Multiple concurrent TV operations hold concurrent read locks, which is fine. But the
`reload_config` write lock will block until all in-flight calls complete.

A cleaner pattern: look up the `Arc<TvProxies>`, release the lock, then call the proxy
method outside the lock. This requires changing `TvProxies` to `Arc<TvProxies>` in the
cache.

> **Fixed.** `tv_proxies` cache changed to `HashMap<String, Arc<TvProxies>>`. In both
> `with_proxies` and `list_tvs`, the read lock is acquired only long enough to clone the
> `Arc`, then released before any `.await`. The D-Bus round-trip runs with no lock held.

---

### 19. `TvProxies::new` eagerly creates all 6 proxies regardless of TV capabilities

**File:** [crates/tisvlay-core/src/dbus_client.rs:257-309](crates/tisvlay-core/src/dbus_client.rs#L257-L309)

For each TV, 6 D-Bus proxies are created upfront (base, power, screen, volume, input,
backlight). This means 6 × N D-Bus round-trips just to populate the proxy cache for N
TVs. For a single TV this is fine, but for N TVs it serializes N×6 proxy-build calls
during `list_tvs`.

The proxies should be lazily created on first use, or the proxy-building could be done
concurrently with `futures::join_all`.

> **Acknowledged.** For the current single-TV use case the N×6 serialized proxy-builds
> are not a measurable issue. Lazy creation on first use is a reasonable future
> improvement; deferred until it shows up in profiling.

---

## Architecture Observations

### 20. Config save path coherence: `SystemConfig` and `UserConfig` share one XDG path

Both `SystemConfig::load()` and `UserConfig::load()` default to
`~/.config/tisvlay/config.toml`. If a user has an old-style config file (pre-Phase-4c)
at this path containing `[[tv]]` entries and profiles, `UserConfig::load_from` will
parse it but silently ignore the `[[tv]]` section (it only reads `profiles` and `tv`
map). The `[[tv]]` array entries are not an error — they're just ignored.

There is no migration utility or warning when `UserConfig::load_from` encounters an
old-style config with `[[tv]]` array entries. After Phase 4c, users who relied on a
single config file for both daemon and session settings will silently lose their TV
definitions in the session context (session service will get an empty TV list from the
config file — though it queries the daemon for the list anyway).

This is likely acceptable as a migration behavior, but should be explicitly documented
in a CHANGELOG or migration guide.

> **Acknowledged.** The session service queries the daemon for the TV list regardless,
> so old `[[tv]]` entries in a user config are silently ignored but functionally harmless.
> A migration note should be added to the project's changelog when the first public
> release is made.

---

### 21. `GetSystemProfiles` serializes `ProfileConfig` to JSON then `session` deserializes it

**File:** [crates/tisvlayd/src/dbus/manager.rs:217-229](crates/tisvlayd/src/dbus/manager.rs#L217-L229)
**File:** [crates/tisvlay-session/src/main.rs:91-108](crates/tisvlay-session/src/main.rs#L91-L108)

The daemon serializes `ProfileConfig` to JSON, passes it over D-Bus as a string, and the
session service deserializes it back to `ProfileConfig`. This double-serde hop (TOML →
`ProfileConfig` → JSON → D-Bus → JSON → `ProfileConfig`) works but is fragile: if a
`SessionAction` variant is added in the future, the JSON serialization must match the
deserialization. Since both sides use `serde_json`, this is consistent, but it's worth a
comment documenting the round-trip contract.

A more robust alternative is to define D-Bus introspectable types for profiles (using
D-Bus dictionaries of `(string, variant)`), but that's a larger refactor.

> **Acknowledged.** Both sides use `serde_json` for the round-trip so the contract is
> consistent today. The double-serde approach is documented in the code. A proper D-Bus
> dictionary type would be the right long-term fix if the profile schema stabilizes.

---

### 22. `SessionClient` has no system-bus variant

**File:** [crates/tisvlay-core/src/dbus_client.rs:756-768](crates/tisvlay-core/src/dbus_client.rs#L756-L768)

`SessionClient::new()` always connects to the session bus. The `--user` flag in the
daemon and session service lets everything run on the session bus for development, but
`tisvlay-tray --user` uses `DaemonClient::new_session()` for the daemon and then
`SessionClient::new()` for the session service. Since `SessionClient::new()` always uses
the session bus, there's no issue for the normal case. But there's a subtle inconsistency:
`DaemonClient` has both `new_system()` and `new_session()`, while `SessionClient` only
has `new()` (always session bus).

This is technically correct (the session service always runs on the session bus), but
the asymmetry between the two client types is confusing for developers reading the API.
A comment explaining why there's no `new_session`/`new_system` split would help.

> **Acknowledged.** `SessionClient::new()` intentionally has no system-bus variant
> because the session service always registers on the session bus. A doc comment
> explaining this was added to `SessionClient::new()`.

---

### 23. No retry/backoff on `check_active_sessions` D-Bus failures

**File:** [crates/tisvlayd/src/logind.rs:260-267](crates/tisvlayd/src/logind.rs#L260-L267)

If `proxy.list_sessions()` fails, the function logs a warning and returns without taking
any action (neither triggering nor suppressing `on_no_active_session`). In a transient
D-Bus failure, this means the daemon silently does nothing when it should be deciding
whether to turn off TVs. There's no retry or fallback.

> **Acknowledged.** Failing safe (doing nothing) on a transient D-Bus error is the
> conservative choice — better than spuriously turning off TVs. The next
> `SessionRemoved` signal will re-trigger the check. A retry/backoff loop is a
> reasonable future improvement for robustness.

---

### 24. Nix module `tisvlay-tray` service lacks restart on `tisvlay-session` restart

**File:** [nix/module.nix:110](nix/module.nix#L110)

```nix
after = [ "graphical-session.target" "tisvlay-session.service" ];
```

`tisvlay-tray` is `after` but not `BindsTo` session. If `tisvlay-session` crashes and is
restarted by systemd, the tray service continues running with a stale session proxy
(`SessionClient` will fail `is_available` until the next poll). The tray degrades
gracefully (profile switching unavailable), which is the intended behavior. However, the
dependency is documented but not enforced — the `after` without `BindsTo` is the right
choice; just make sure this is intentional and documented.

Also: the tray service has no `ExecReload`, unlike the session service. Adding one
(with `kill -HUP $MAINPID` or a D-Bus `ReloadConfig` call) would allow refreshing
without restart.

> **Acknowledged.** The `after` without `BindsTo` is intentional — the tray degrades
> gracefully when the session service is unavailable rather than restarting. An
> `ExecReload` for the tray is a reasonable future addition; deferred as it requires
> wiring SIGHUP handling into the tray's polling loop.

---

## Summary: Priority Order for Follow-up

| # | Severity | Item |
|---|----------|------|
| 1 | **Critical** | `reload_config` doesn't update `daemon` settings in memory |
| 2 | **Critical** | `set_daemon_option` doesn't update in-memory state |
| 11 | **Critical** | `power_off` marks TV powered-off even on failure |
| 3 | **High** | `check_active_sessions` on `SessionNew` can fire spuriously during login |
| 4 | **High** | `list_tv_ids` non-deterministic order causes tray menu reordering |
| 6 | **High** | Missing user profile silently skips user default in resolve chain |
| 10 | **Medium** | logind monitor doesn't check initial session state at startup |
| 7 | **Medium** | Idle fires at min-across-TVs, not per-TV |
| 8 | **Medium** | `IdleResume` hardcoded to `ScreenOn` — inconsistent with profile design |
| 2 | **Medium** | Sync disk I/O in `set_daemon_option`/`save_tv_config` blocks executor |
| 12 | **Medium** | `set_tv_option` duplicates field-mapping logic |
| 5 | **Low** | `SystemConfig::user_config_path()` naming is inverted |
| 9 | **Low** | `DaemonAction` leaks from `logind` into `tv_manager` |
| 13 | **Low** | Double `is_available` calls without synchronization |
| 14 | **Low** | `use anyhow::Context` import at bottom of file |
| 15 | **Low** | `save_to` clones entire config unconditionally |
| 16 | **Low** | Deprecated `with_profile` method still public |
| 17 | **Low** | `get_profile`/`get_profile_for_tv` duplicate fallback logic |
| 18 | **Low** | Read lock held across async await in `with_proxies` |
| 19 | **Low** | Eager proxy creation for all 6 interfaces |

---

## What's Done Well

The architecture for the config split is clean and well-motivated. The layered inheritance
chain (`user profile → user default → system default → hard-coded defaults`) is elegant
and the `Option<T>`-based `ProfileConfig` / `ResolvedProfile` split makes the intent
clear. The test suite for the merge/resolve logic is thorough and covers the key edge
cases.

The boot-wake refactor (replacing a blind `sleep` with retry-connect) is a meaningful
correctness improvement. The shutdown inhibitor lock pattern in `logind.rs` is correct.

The `with_proxies` helper in `DaemonClient` reduces boilerplate well despite the
boxing requirement. The proxy cache design (lazy populate, clear on reload) is sound.

The retry-and-replay pattern for late daemon connect in `tisvlay-session` correctly
handles the common race where the session service starts before the daemon, and the
event replay ensures the TV matches the current desktop state after reconnect.

The `session_dbus.rs` `SessionService` is well-structured and handles the profile
validation (checking existence before assignment) correctly.

---

## Follow-on Review (post-85e496dc)

This section reviews the fixes from commit `85e496dc` and identifies new issues
uncovered by reading the implementation, with a focus on dead code and backward
compatibility that can be removed given there are no stability guarantees.

---

### F1. `set_tv_option` still has unguarded blocking disk I/O (missed fix from #2)

**File:** [crates/tisvlayd/src/tv_manager.rs:913-926](crates/tisvlayd/src/tv_manager.rs#L913-L926)

The fix for issue #2 converted `set_daemon_option`, `save_tv_config`, and
`delete_tv_config` to use `spawn_blocking`. But `set_tv_option` was missed: it still
calls `Config::load_from` and `config.save_to` directly — synchronous blocking disk I/O
inside an `async fn` on the tokio executor thread.

```rust
// set_tv_option — still blocking the executor:
let mut config = Config::load_from(&self.config_path)  // blocks
    .map_err(...)?;
// ...
config.save_to(&self.config_path)                       // blocks
    .map_err(...)?;
```

**Fix direction:** Apply the same `spawn_blocking` pattern as `set_daemon_option`.

> **Fixed.** `set_tv_option` now validates and updates the in-memory TV config first
> (which validates both the TV ID and the key), then persists via `spawn_blocking`
> for the load-modify-save. Per-TV lock is released before the blocking I/O.

---

### F2. `pub type Config = SystemConfig` — remove the alias

**File:** [crates/tisvlay-core/src/config.rs:412](crates/tisvlay-core/src/config.rs#L412)

```rust
/// Backward-compatible alias for [`SystemConfig`].
pub type Config = SystemConfig;
```

This alias exists to avoid renaming call sites when `SystemConfig` was introduced.
`tv_manager.rs` uses `Config::load_from(...)` in five places. `lib.rs` uses `Config`
in a doc example. Since there are no stability guarantees, the alias should be removed:
all call sites should use `SystemConfig` directly. The type alias obscures that the
system daemon is working with the system (not user) config — a name that now matters.

> **Fixed.** `pub type Config = SystemConfig` removed. All call sites in `tv_manager.rs`
> and `tisvlayd/main.rs` updated to use `SystemConfig` directly. The stale `lib.rs` doc
> example rewritten to show the current daemon-based architecture (`DaemonClient`).

---

### F3. `TvConfig.profile` backward-compat field — remove it

**File:** [crates/tisvlay-core/src/config.rs:298-301](crates/tisvlay-core/src/config.rs#L298-L301)

```rust
/// Deprecated: profile assignment has moved to user config.
/// Retained for backward compatibility with existing config files.
#[serde(default, skip_serializing)]
pub profile: Option<String>,
```

This field is retained so old single-file configs with `profile = "gaming"` on a `[[tv]]`
entry still parse. With no stability guarantees and no published users, there is no one
to break. The field drags along `get_profile_for_tv` and complicates `TvConfig::new`
(which must initialize it to `None`). It also creates a deserialization trap where new
users copying an old example config silently get behavior they didn't expect (the profile
assignment is read from the system config but never acted on in the new architecture).

Removing it forces a clean break: profile assignments live in the user config only.

**Impact:** Remove `TvConfig.profile`, `get_profile_for_tv`, the backward-compat test
`test_parse_config_backward_compat`, and the backward-compat assertion on `tv[0].profile`
in that test.

> **Fixed.** `TvConfig.profile` field removed along with its `#[serde(default, skip_serializing)]`
> annotation and builder entry. `TvConfig::new()` no longer sets `profile: None`.
> `test_parse_config_backward_compat` updated to drop the `profile = "gaming"` TOML line
> and the `tv[0].profile` assertion; `test_backward_compat_profile_resolution` removed entirely.

---

### F4. `SystemConfig::get_profile_for_tv` — dead production code

**File:** [crates/tisvlay-core/src/config.rs:533-551](crates/tisvlay-core/src/config.rs#L533-L551)

`get_profile_for_tv` is only called in tests (`config.rs:820`, `config.rs:1276`,
`config.rs:1280`). No production code path calls it. It exists to support the deprecated
`TvConfig.profile` field (issue F3 above). If that field is removed, this method goes
with it. Even if the field is retained, the method should be `#[cfg(test)]` if it has
no production callers.

> **Fixed.** `get_profile_for_tv` removed along with F3. All test call sites updated to
> use `config.get_profile("name")` directly, which is the correct production API.

---

### F5. `SystemConfig::get_default_profile_resolved` — unused method

**File:** [crates/tisvlay-core/src/config.rs:516-519](crates/tisvlay-core/src/config.rs#L516-L519)

```rust
pub fn get_default_profile_resolved(&self) -> ResolvedProfile {
    self.get_default_profile().resolve()
}
```

`grep` finds no callers of `get_default_profile_resolved` in production code or tests.
It should be removed.

> **Fixed.** `get_default_profile_resolved` removed.

---

### F6. `SystemConfig::default_tv` — dead code referenced only in an outdated doc example

**File:** [crates/tisvlay-core/src/config.rs:484-487](crates/tisvlay-core/src/config.rs#L484-L487)
**File:** [crates/tisvlay-core/src/lib.rs:36](crates/tisvlay-core/src/lib.rs#L36)

`default_tv()` returns `self.tv.first()`. The only reference to it is in the crate-level
doc example in `lib.rs`, which is also outdated: it uses `Config::load()` (which now
loads from the system path, not the XDG user path) and `registry.connect_tv(tv)` with
the wrong signature. The entire `lib.rs` example block is stale and misleading. Remove
`default_tv()` and rewrite the doc example to reflect the current daemon-based
architecture where the CLI/tray/session communicate through `DaemonClient`.

> **Fixed.** `default_tv()` removed. The `lib.rs` crate-level doc example rewritten to
> use the modern `DaemonClient::new_system()` pattern, which accurately reflects how all
> current consumers (CLI, tray, session service) interact with the daemon.

---

### F7. `SystemConfig::get_default_profile` — only used in tests

**File:** [crates/tisvlay-core/src/config.rs:505-513](crates/tisvlay-core/src/config.rs#L505-L513)

`get_default_profile()` returns a `&ProfileConfig` via a `OnceLock<ProfileConfig>` static
for the fallback case. It is only called in tests (line 853) and transitively by
`get_default_profile_resolved` (which is itself unused — see F5). In production, callers
use `TvManager::get_system_default_profile()` which reads the `profiles` RwLock directly.

If the public API needs a way to read the default profile from a `SystemConfig`, keep it.
If not, remove it and the `OnceLock` static along with it.

> **Fixed.** `get_default_profile()` and its `OnceLock<ProfileConfig>` static removed.
> Tests that previously called `get_default_profile()` updated to use
> `config.profiles.get("default")` directly, which exercises the real lookup path.

---

### F8. `DaemonAction` should be `pub(crate)`, not `pub`

**File:** [crates/tisvlayd/src/tv_manager.rs:161-164](crates/tisvlayd/src/tv_manager.rs#L161-L164)

```rust
pub enum DaemonAction {
    Shutdown,
    NoActiveSession,
}
```

`DaemonAction` is an internal coordination enum used only within the `tisvlayd` crate
between `tv_manager.rs` and `logind.rs`. It is not part of any public API. It should be
`pub(crate)` to prevent accidental exposure and to document its intended scope.

> **Fixed.** `DaemonAction` changed to `pub(crate)`.

---

### F9. `set_daemon_option` — TOCTOU between write lock release and disk snapshot

**File:** [crates/tisvlayd/src/tv_manager.rs:854-905](crates/tisvlayd/src/tv_manager.rs#L854-L905)

```rust
// 1. Write lock: validate + update daemon_config
{
    let mut daemon = self.daemon_config.write().await;
    // ... apply key/value ...
} // write lock released

// 2. Read lock: snapshot for disk write
let daemon_snapshot = self.daemon_config.read().await.clone();

// 3. spawn_blocking: write snapshot to disk
tokio::task::spawn_blocking(move || { ... config.daemon = daemon_snapshot; ... })
```

Between step 1 and step 2, another concurrent `set_daemon_option` call can write a
different field. The snapshot in step 2 will correctly include both changes (since it
reads after all writes), and the disk will reflect the fully-merged state. This is safe.

However, there's a different issue: the `spawn_blocking` at step 3 does a
load-modify-save on the on-disk config. If two `set_daemon_option` calls produce two
concurrent `spawn_blocking` tasks, the second task could load the config file before
the first task has written it, then overwrite the first task's write with a stale
snapshot. The result is that one of the two changes is silently lost on disk (though
both are correct in memory).

This is a load-modify-save race that affects `set_daemon_option`, `save_tv_config`, and
`delete_tv_config`. For a single admin GUI making sequential calls, this is unlikely.
But a proper fix would be a write serialization primitive (e.g., a `Mutex<()>` held
during load-modify-save, or a dedicated config writer task receiving changes via a
channel).

> **Fixed.** Added `disk_write_lock: Mutex<()>` to `TvManager`. All four disk-write
> paths (`save_tv_config`, `delete_tv_config`, `set_daemon_option`, `set_tv_option`)
> acquire this lock before their `spawn_blocking` call and hold it until the blocking
> task completes. Concurrent calls now queue behind the lock, so the second caller
> always loads a snapshot that includes the first caller's write.

---

### F10. `reload_config` holds the outer `tvs` write lock while closing connections

**File:** [crates/tisvlayd/src/tv_manager.rs:944-988](crates/tisvlayd/src/tv_manager.rs#L944-L988)

During `reload_config`, the `tvs` outer write lock is held while calling
`conn.close().await` (line 959) for each removed TV. `close()` is a network operation
that may block for hundreds of milliseconds on a slow or broken WebSocket. The entire
TV map is write-locked for this duration, blocking all TV commands.

For a normal config reload (0–1 TVs removed), this is acceptable. But for a reload that
removes several TVs with slow connections, the daemon becomes unresponsive to all TV
commands during cleanup.

**Fix direction:** Collect the `Arc<Mutex<ManagedTv>>` entries to remove, release the
outer write lock, then close connections under only the per-TV locks.

> **Fixed.** `reload_config` now uses a two-phase approach. Phase 1 holds the outer
> write lock only for map mutations (shift_remove and insert — fast pointer operations).
> The write lock is released at the end of Phase 1. Phase 2 iterates the collected Arcs,
> acquires each per-TV lock independently, updates the config, and closes any connections.
> No outer lock is held during network I/O.

---

### F11. `save_to` auto-inserts "default" profile — wrong for system configs

**File:** [crates/tisvlay-core/src/config.rs:447-459](crates/tisvlay-core/src/config.rs#L447-L459)

`SystemConfig::save_to` silently inserts a `[profiles.default]` section when the config
has no profiles. For a system config, this happens on first save after TV pairing. An
admin who deliberately omitted `[profiles.default]` from their system config (preferring
hard-coded defaults) will find it was silently added after pairing. On subsequent reloads,
the hard-coded default profile is now present in the config file and takes effect as the
system default, potentially changing behavior.

Now that `SystemConfig` is clearly the daemon's config (not a shared config), this
auto-insertion is more surprising than helpful. Admins managing system configs directly
prefer explicit control.

**Fix direction:** Remove the auto-insert behavior from `save_to`. If a self-documenting
default profile is desired for new installations, add it in the pairing flow where it
belongs, or in the NixOS module's initial config generation.

> **Fixed.** The auto-insert logic removed from `save_to`. The method now serializes
> exactly what is in the `SystemConfig` with no side effects. The `Cow<'_, SystemConfig>`
> anti-clone was also removed as it is no longer needed. The test
> `test_save_adds_default_profile_when_empty` was renamed
> `test_save_does_not_auto_insert_default_profile` and flipped to assert that
> `profiles.is_empty()` after a round-trip through `save_to`/`from_str`.

---

### Summary of New Findings

| # | Severity | Item |
|---|----------|------|
| F1 | **High** | `set_tv_option` still has blocking disk I/O (missed from fix #2) |
| F2 | **Low** | `pub type Config = SystemConfig` alias — remove |
| F3 | **Low** | `TvConfig.profile` backward-compat field — remove (no stability guarantees) |
| F4 | **Low** | `get_profile_for_tv` — dead production code, only in tests |
| F5 | **Low** | `get_default_profile_resolved` — completely unused |
| F6 | **Low** | `default_tv()` — dead code, stale doc example in `lib.rs` |
| F7 | **Low** | `get_default_profile` — only in tests, consider `#[cfg(test)]` or remove |
| F8 | **Low** | `DaemonAction` should be `pub(crate)` |
| F9 | **Medium** | Concurrent `set_daemon_option` / `save_tv_config` load-modify-save race |
| F10 | **Medium** | `reload_config` holds outer write lock while closing WebSocket connections |
| F11 | **Low** | `save_to` auto-inserts "default" profile — wrong semantic for system config |

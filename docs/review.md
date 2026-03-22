# Phase 5A Code Review

Review of commits `da11788..487000e` (Phase 5A: `managed` flag + CLI profile parity
+ live managed filtering).

Build: clean. Tests: all pass.

---

## H1. Stale doc comment on `SystemConfig` — references removed `Config` alias

**File:** [crates/tisvlay-core/src/config.rs:378](crates/tisvlay-core/src/config.rs#L378)

```rust
/// This is also aliased as [`Config`] for backward compatibility.
```

The `pub type Config = SystemConfig` alias was removed in the F2 fix (commit `172f483`).
This doc comment was not updated, leaving a dangling `[`Config`]` rustdoc link that will
produce a broken link warning. The line should be deleted.

**Severity:** Low

**Resolution:** Fixed — stale doc comment line deleted.

---

## H2. `reload_config` does blocking disk I/O on the async executor

**File:** [crates/tisvlayd/src/tv_manager.rs:988-992](crates/tisvlayd/src/tv_manager.rs#L988-L992)

```rust
pub async fn reload_config(&self) -> Result<(), TvManagerError> {
    info!("Reloading configuration");

    let new_config = SystemConfig::load_from(&self.config_path)
        .map_err(|e| TvManagerError::ConfigError(format!("Failed to load config: {}", e)))?;
```

All other config-load sites (`save_tv_config`, `delete_tv_config`, `set_daemon_option`,
`set_tv_option`) were converted to `spawn_blocking` in the F1/F9 fixes. `reload_config`
was missed. It calls `SystemConfig::load_from` — a synchronous `std::fs::read_to_string`
— directly from an `async fn` on the tokio executor thread.

This is the same class of bug as F1. The config file is small, so the read is fast, but
it's still inconsistent with the pattern established everywhere else. For consistency
and to prevent future issues if the config grows (or lives on a slow filesystem like NFS),
wrap the load in `spawn_blocking`.

**Fix direction:** Same pattern as `set_daemon_option`: move `SystemConfig::load_from`
into a `spawn_blocking` call, pass the loaded config back out.

**Severity:** Medium

**Resolution:** Fixed — `reload_config` now acquires `disk_write_lock` and wraps
`SystemConfig::load_from` in `spawn_blocking`, consistent with all other disk I/O paths.

---

## H3. `event_handler` calls `daemon.list_tvs()` on every single session event

**File:** [crates/tisvlay-session/src/main.rs:401-409](crates/tisvlay-session/src/main.rs#L401-L409)

```rust
// Query current managed set from daemon (live, not cached).
let managed_ids: HashSet<String> = match daemon.list_tvs().await {
    Ok(tvs) => tvs.into_iter().filter(|t| t.managed).map(|t| t.id).collect(),
    Err(e) => {
        warn!(error = %e, "Failed to query managed TVs; falling back to full TV list");
        state.read().await.tv_ids.iter().cloned().collect()
    }
};
```

Every session event (lock, unlock, suspend, resume, idle, idle-resume, greeter-wake) now
makes a full D-Bus round-trip to the daemon to list all TVs, create proxy objects for each,
and query their `managed` property. `list_tvs()` is not cheap — it calls `get_tv_proxies()`
and fetches 6 properties per TV (id, name, address, vendor, connected, managed).

For a single TV and infrequent events this is fine. But the approach scales poorly: with
N TVs, each event does O(N) D-Bus property fetches just to filter the managed set.

**Alternative approaches (pick one):**

1. **Dedicated `list_managed_tv_ids()` D-Bus method:** The daemon already has
   `TvManager::list_managed_tv_ids()` — expose it on `org.tisvlay.Manager1` and call it
   from the session service. Returns just a `Vec<String>`, no proxy creation needed.
   This is the simplest fix and preserves the "live query" semantic.

2. **Cache with invalidation:** Subscribe to `PropertiesChanged` signals on each TV's
   `org.tisvlay.Tv1` interface. Rebuild the managed set only when a `managed` property
   changes. More complex but avoids any per-event round-trip.

Option 1 is recommended — it's one new D-Bus method, minimal surface, and still live.

**Severity:** Medium (correctness is fine; performance and D-Bus load are the concern)

**Resolution:** Resolved by Phase 5C — `list_tvs()` now calls `GetManagedObjects`, a
single D-Bus round-trip that returns all object paths and their properties at once. No
proxy objects are created and no per-TV property fetches occur. The O(N) scaling concern
is gone; the per-event cost is now one cheap call regardless of TV count.

---

## H4. `get_tv_id` eagerly queries daemon for commands that don't need a TV ID

**File:** [crates/tisvlay-cli/src/main.rs:188](crates/tisvlay-cli/src/main.rs#L188)

```rust
let tv_id = get_tv_id(&client, cli.tv.as_deref()).await;
```

`get_tv_id` is called unconditionally before the `match` on `cli.command`. For `List`,
`Pair`, and `Remove`, the result is never used — but the `list_tvs()` D-Bus call still
fires (and `Pair` will fail with "No TVs configured" if the user is pairing their first
TV without `--tv`).

This was the pattern before Phase 5A, but it should be noted: moving `get_tv_id` into
the arms that actually use it would avoid the unnecessary D-Bus call and fix the
first-pair edge case. The `tv_id` is already a `Result` and is `?`-ed lazily in each
arm, so the current code doesn't *crash* on `List`/`Pair`/`Remove`, but it does waste
a round-trip.

**Fix direction:** Move `let tv_id = get_tv_id(...)` inside the `match` arms that need
it, or gate it with a helper that returns `Option` only when the command requires a TV.

**Severity:** Low (waste, not bug)

**Status:** Intentionally kept — the eager call was reviewed and the simpler unconditional
pattern was preferred. The `tv_id` result is a `Result` and is `?`-ed lazily in each arm,
so `List`/`Pair`/`Remove` arms do not fail on the superfluous call.

---

## H5. `Input` command has unreachable-looking control flow

**File:** [crates/tisvlay-cli/src/main.rs:316-329](crates/tisvlay-cli/src/main.rs#L316-L329)

```rust
Commands::Input { input_id } => {
    let tv_id = tv_id?;
    if input_id.is_none() {
        let inputs = client.get_inputs(&tv_id).await?;
        println!("Available inputs:");
        for (id, label) in inputs {
            println!("  {} - {}", id, label);
        }
    }
    if let Some(id) = input_id {
        client.switch_input(&tv_id, &id).await?;
        println!("Switched to input: {}", id);
    }
}
```

The two `if` branches are mutually exclusive (`input_id` is either `None` or `Some`),
but written as two independent `if` statements. This reads as if both could fire. Using
`match` or `if let ... else` would make the mutual exclusion explicit:

```rust
match input_id {
    None => { /* list inputs */ }
    Some(id) => { /* switch input */ }
}
```

**Severity:** Low (readability, not a bug)

**Resolution:** Fixed — converted to `match input_id { None => { … }, Some(id) => { … } }`.

---

## H6. `TvConfig::new` defaults `managed: true` — not symmetric with serde requiring it

**File:** [crates/tisvlay-core/src/config.rs:315-325](crates/tisvlay-core/src/config.rs#L315-L325)

Commit `f950100` removed serde defaults from `TvConfig` so that missing fields in TOML
error rather than silently defaulting. But `TvConfig::new()` still sets `managed: true`
implicitly. This creates a split:

- **TOML path:** `managed` is required — omitting it is a parse error. Explicit.
- **Code path:** `TvConfig::new("id", "name", "lg")` silently gets `managed = true`.

This is intentional (the commit message says so), and it makes sense for `pair_tv()`
which constructs TVs programmatically. However, the asymmetry may surprise callers who
expect `TvConfig::new` to mirror the TOML contract. Document the default in the
`TvConfig::new` doc comment (currently absent) so it's explicit rather than discovered
from reading the source.

Also note: `only_when_active` defaults to `false` in `TvConfig::new` without a
`with_managed()` builder method. There is `with_only_when_active()` and
`with_hdmi_input()` but no `with_managed()`. If the builder pattern is the intended
API for programmatic construction, add `with_managed()` for completeness.

**Severity:** Low (API consistency, not a bug)

**Resolution:** Fixed — `TvConfig::new` now takes `managed: bool` as a required fourth
parameter. No default exists in the library; the doc comment explains the semantics.
`with_managed()` is unnecessary since the value is required in the constructor. All test
call sites updated to pass `true` explicitly.

---

## H7. `pair_tv` creates TVs with `managed: true` — no way to pair as unmanaged

**File:** [crates/tisvlayd/src/tv_manager.rs:775](crates/tisvlayd/src/tv_manager.rs#L775)

```rust
let mut tv_config = TvConfig::new(&tv_id, name, vendor);
```

`pair_tv` creates new TVs via `TvConfig::new`, which defaults to `managed: true`.
There is no parameter to override this. If a user wants to pair a TV but not
auto-manage it (e.g., a neighbor's TV they want to control manually but not have
react to their screen lock), they must pair first, then immediately run
`tisvlay unmanage <id>`.

This is acceptable for now, but worth noting for Phase 5 GUI: the pairing dialog
should include a "managed" checkbox.

**Severity:** Info (design note for Phase 5)

**Resolution:** Fixed — `managed` is now threaded explicitly through all layers:
`TvConfig::new(id, name, vendor, managed)` → `pair_tv(..., managed)` → D-Bus
`AddTv(name, vendor, config_json, managed)` → `DaemonClient::pair_tv(..., managed)` →
CLI `tisvlay pair ... [--unmanaged]`. The UX default (`managed = !unmanaged = true`) lives
exclusively in the CLI. No library layer assumes a default.

---

## H8. Session service `event_handler` fallback leaks unmanaged TVs into action set

**File:** [crates/tisvlay-session/src/main.rs:405-408](crates/tisvlay-session/src/main.rs#L405-L408)

```rust
Err(e) => {
    warn!(error = %e, "Failed to query managed TVs; falling back to full TV list");
    state.read().await.tv_ids.iter().cloned().collect()
}
```

When the daemon is unreachable, the fallback uses the full `tv_ids` list (all paired
TVs, including unmanaged ones). This means a daemon hiccup could cause session events
to fire on TVs the user explicitly unmanaged. The window is narrow (daemon must be
down at the exact moment of a session event), but the behavior is surprising.

**Alternative:** Fall back to an empty set (do nothing) rather than the full list.
The rationale: if the daemon is down, TV commands will fail anyway — sending actions
to unmanaged TVs just produces spurious error logs. Or fall back to the *last known*
managed set rather than the full set.

**Severity:** Low (narrow window, commands will fail anyway)

**Resolution:** Fixed — fallback now returns an empty `HashSet`, skipping dispatch entirely.
If the daemon is unreachable, TV commands would fail regardless; dispatching to unmanaged
TVs only produces spurious error logs.

---

## H9. Tray `SetManaged` command doesn't trigger session service reload

**File:** [crates/tisvlay-tray/src/main.rs:303-310](crates/tisvlay-tray/src/main.rs#L303-L310)

```rust
TrayCommand::SetManaged(id, managed) => {
    if let Err(e) = daemon
        .set_tv_option(&id, "managed", if managed { "true" } else { "false" })
        .await
    {
        warn!(...);
    }
}
```

After toggling `managed`, the tray does a `refresh_state` (line 314) which updates the
tray's own UI. But it does not notify the session service. With the Phase 5B fix
(commit `487000e`), the session service queries managed status live per-event, so it will
pick up the change on the next event. But there's no immediate effect — if the user
unmanages a TV and then locks their screen in the same second, there's a theoretical
race where the session service's `list_tvs()` call and the `set_tv_option` call overlap.

In practice this is fine because `set_tv_option` updates the daemon's in-memory state
synchronously before returning, and `list_tvs` reads from the same in-memory state.
No action needed; documenting for completeness.

**Severity:** Info (no actual race — daemon's in-memory update is sequenced)

**Resolution:** Added an explanatory comment at the `SetManaged` call site in
`tisvlay-tray/src/main.rs` so future readers don't mistake the omission for a bug.

---

## H10. Scroll handler `volume_item`/`backlight_item` use `backlight_item` for screen on/off

**File:** [crates/tisvlay-tray/src/tray.rs:424-433](crates/tisvlay-tray/src/tray.rs#L424-L433)

```rust
if tv.is_screen_on {
    items.push(backlight_item("Screen Off", "view-hidden", cmd_tx, &id, { ... }));
} else {
    items.push(backlight_item("Screen On", "view-visible", cmd_tx, &id, { ... }));
}
```

"Screen Off" and "Screen On" use `backlight_item`, which sets `scroll_target =
ScrollTarget::Backlight` and `scroll_tv_id` on activation. Clicking "Screen Off" thus
changes the scroll wheel to control backlight for that TV — a side effect the user
didn't ask for. `tv_item` (which doesn't change scroll target) would be the correct
helper here, since screen on/off is not a backlight adjustment.

**Fix direction:** Change `backlight_item` to `tv_item` for the Screen Off/On entries.

**Severity:** Low (UX glitch, not a data bug)

**Resolution:** By design — screen on/off is the discrete pole of the backlight axis,
the same relationship mute has to volume. Clicking "Screen Off" contextualizing the scroll
wheel to backlight is the expected interaction pattern, not a side effect.

---

## H11. `list_managed_tv_ids` acquires per-TV locks sequentially under outer read lock

**File:** [crates/tisvlayd/src/tv_manager.rs:400-410](crates/tisvlayd/src/tv_manager.rs#L400-L410)

```rust
pub async fn list_managed_tv_ids(&self) -> Vec<String> {
    let tvs = self.tvs.read().await;
    let mut ids = Vec::new();
    for (id, tv_arc) in tvs.iter() {
        let tv = tv_arc.lock().await;
        if tv.config.managed {
            ids.push(id.clone());
        }
    }
    ids
}
```

This acquires the outer `tvs` read lock, then for each TV acquires the per-TV mutex
to read `config.managed`. The outer read lock is held for the entire iteration. If any
per-TV mutex is held by a long-running operation (e.g., `power_on` waiting for WoL),
this method blocks — and because it holds the outer read lock, any concurrent write
operation (e.g., `remove_tv`, `pair_tv`, `reload_config`) is also blocked.

This is the same anti-pattern that was fixed in `reload_config` (F10). For a managed
flag that rarely changes, consider storing `managed` outside the per-TV mutex (e.g.,
in the `IndexMap` value alongside the `Arc<Mutex<ManagedTv>>`, or as a separate
`HashMap<String, bool>`). This would allow reading `managed` without any per-TV lock.

**Severity:** Low (unlikely to be a hot path, but architecturally inconsistent
with the F10 fix philosophy)

**Resolution:** Fixed — the `managed` flag is now stored as a `bool` alongside the
`Arc<Mutex<ManagedTv>>` in the `IndexMap` value, readable with only the outer read lock.
`list_managed_tv_ids` scans the map directly via `filter_map` with no per-TV lock
acquisition. `set_tv_option` and `reload_config` keep the outer flag in sync whenever
`managed` changes. `boot_wake` uses the outer flag to skip the per-TV lock for unmanaged
TVs entirely.

---

## H12. `TvConfig` doc comment still references removed fields

**File:** [crates/tisvlay-core/src/config.rs:272-277](crates/tisvlay-core/src/config.rs#L272-L277)

```rust
/// Configuration for a single TV.
///
/// This struct contains vendor-agnostic identity and hardware settings.
/// Connection parameters (IP, MAC, etc.) are vendor-specific and stored in
/// the vendor config map, parsed by each vendor crate.
///
```

The doc comment is fine but incomplete after the Phase 5A changes. It doesn't mention
the new `managed` field or explain its semantics. The `managed` field's own doc comment
(lines 298-303) is good, but the struct-level doc should mention that `managed` controls
automation participation.

**Severity:** Info (docs quality)

**Resolution:** Fixed — struct-level doc updated to describe `managed` and its
automation semantics.

---

## Summary

| # | Severity | Item |
|---|----------|------|
| H1 | **Low** | Stale doc comment references removed `Config` alias |
| H2 | **Medium** | `reload_config` still does blocking disk I/O (missed from F1 pattern) |
| H3 | **Medium** | `event_handler` calls `list_tvs()` on every event — expose `list_managed_tv_ids()` via D-Bus instead |
| H4 | **Low** | `get_tv_id` eagerly queries daemon for commands that don't need a TV ID |
| H5 | **Low** | `Input` command uses two independent `if` instead of `match`/`else` |
| H6 | **Low** | `TvConfig::new` silently defaults `managed: true` — add doc comment and `with_managed()` builder |
| H7 | **Info** | `pair_tv` always creates managed TVs — note for Phase 5 GUI pairing dialog |
| H8 | **Low** | Fallback in event_handler leaks unmanaged TVs into action set when daemon is down |
| H9 | **Info** | `SetManaged` doesn't notify session service (fine because of live query) |
| H10 | **Low** | Screen Off/On menu items use `backlight_item` — should use `tv_item` to avoid scroll side-effect |
| H11 | **Low** | `list_managed_tv_ids` locks per-TV mutexes under outer read lock (inconsistent with F10 fix) |
| H12 | **Info** | `TvConfig` struct-level doc doesn't mention `managed` |

### Overall Assessment

Phase 5A is a clean, well-scoped feature addition. The `managed` flag is threaded
correctly through all layers (core, daemon, session, tray, CLI). The Phase 5B live-query
fix is correct — it removes the startup-only managed filter and queries the daemon
per-event. The CLI profile parity is straightforward and uses the existing
`SessionClient` API.

The two medium-severity items are:

- **H2** is a consistency issue — `reload_config` was missed when all other disk I/O
  paths were converted to `spawn_blocking`.
- **H3** is a performance concern — `list_tvs()` is expensive for a managed-ID filter.
  Exposing `list_managed_tv_ids()` as a dedicated D-Bus method would be a clean fix.

No critical bugs found. The dead code cleanup from the F-series review is holding.

---

## Follow-on Review (post-72351264)

This section reviews the fixes from commits `9c86010..3ab23ac` (6 commits responding
to findings H1–H12) and the new Phase 5C `ObjectManager` work.

Build: clean. Tests: all pass.

---

### Verification of H-series Resolutions

All 12 findings were addressed. Assessment of each response:

| # | Resolution | Verdict |
|---|-----------|---------|
| H1 | Stale doc line deleted | **Correct** — verified line 378 no longer references `Config` alias. |
| H2 | `reload_config` now uses `spawn_blocking` + `disk_write_lock` | **Correct** — consistent with all other disk I/O paths. |
| H3 | Replaced with Phase 5C `ObjectManager` | **Correct** — see I1 below for detailed review. |
| H4 | Intentionally kept (eager `get_tv_id`) | **Agreed** — the lazy `Result` pattern means the extra call is harmless. |
| H5 | Converted to `match` | **Correct** — cleaner control flow. |
| H6 | `managed` now required 4th parameter of `TvConfig::new` | **Correct** — no default in library; UX default lives in CLI only. |
| H7 | `managed` threaded through all layers to `pair_tv` | **Correct** — `--unmanaged` CLI flag, D-Bus `AddTv` takes `managed: bool`. |
| H8 | Fallback returns empty `HashSet` | **Correct** — prevents spurious dispatch to unmanaged TVs. |
| H9 | Explanatory comment added | **Correct** — documents the intentional omission. |
| H10 | Intentionally kept (screen off is backlight axis) | **Agreed** — the rationale is sound: screen off/on is the extreme of the backlight axis, so contextualizing scroll to backlight is deliberate UX, not a side effect. |
| H11 | `managed` stored outside per-TV mutex as `TvEntry = (bool, Arc<Mutex<ManagedTv>>)` | **Correct** — see I2 below for detailed review. |
| H12 | `TvConfig` struct-level doc updated | **Correct** — now mentions `managed` and its automation semantics. |

---

### I1. Phase 5C `ObjectManager` — review of new `list_tvs()` implementation

**File:** [crates/tisvlay-core/src/dbus_client.rs:451-500](crates/tisvlay-core/src/dbs_client.rs#L451-L500)

The new `list_tvs()` uses `GetManagedObjects` — a single D-Bus round-trip returning
all object paths with their interface properties. This eliminates the previous 1 + 6N
pattern. The implementation is clean:

- Filters for objects exposing `org.tisvlay.Tv1` (correct interface name).
- `get_str` / `get_bool` closures extract typed values with safe fallbacks.
- `id.is_empty()` check skips objects without a valid ID.
- Property names (`Id`, `Name`, `Address`, `Vendor`, `Connected`, `Managed`) match
  the D-Bus property names from the `#[zbus(property)]` annotations in `tv.rs`.

**One concern:** `get_bool` defaults to `false` for both `Connected` and `Managed`.
For `Connected`, `false` is the safe default (assume disconnected). For `Managed`,
`false` means "skip all automation" — so if the `Managed` property is somehow missing
from the `GetManagedObjects` response (shouldn't happen, but defensive), the TV silently
becomes unmanaged. The previous `list_tvs()` used `unwrap_or(true)` for `managed`.
This is a behavioral change: old code defaulted to managed, new code defaults to
unmanaged. Consider whether `unwrap_or(true)` is the safer default for `Managed`
specifically, since a missing property is more likely a transient issue than an
intentional unmanage.

**Severity:** Low (the property should always be present; defensive default question)

**Resolution:** Fixed — `get_bool` now takes an explicit `default: bool` parameter.
`Connected` passes `false` (assume disconnected) and `Managed` passes `true` (assume
managed — a missing property is more likely a transient issue than intentional opt-out).
The different defaults are documented inline at each call site.

**Daemon-side registration:** `zbus::fdo::ObjectManager` is registered at
`/org/tisvlay` *before* TV interfaces are registered at `/org/tisvlay/Tv/{id}`,
which is the correct order for `InterfacesAdded` signals to fire. The test mock
also registers `ObjectManager` at `/org/tisvlay`.

**`GetTvs` removal:** The `GetTvs` method was removed from `org.tisvlay.Manager1`
and the `TisvlayManager` proxy. This is a clean break — no callers remain. The
`is_available()` check was updated to use `get_managed_objects()` instead.

---

### I2. `TvEntry` pattern — `managed` flag outside per-TV mutex

**File:** [crates/tisvlayd/src/tv_manager.rs:134-139](crates/tisvlayd/src/tv_manager.rs#L134-L139)

The `type TvEntry = (bool, Arc<Mutex<ManagedTv>>)` approach is clean and solves the
lock contention issue. Key sync points verified:

1. **`TvManager::new`** — outer flag initialized from `tv_config.managed`. Correct.
2. **`set_tv_option`** — after applying to in-memory config, acquires outer write lock
   and updates the flag. Correct.
3. **`reload_config`** — Phase 1 (under write lock) updates `*managed_flag =
   tv_config.managed` for existing TVs. Correct.
4. **`pair_tv`** — inserts `(tv_config.managed, ...)`. Correct.
5. **`boot_wake`** — checks `*managed` before acquiring per-TV lock, skipping
   unmanaged TVs entirely. Correct optimization.
6. **`list_managed_tv_ids`** — now a simple `filter_map` with no per-TV locks. Correct.
7. **`remove_tv`** — destructures `(_, arc)` correctly. Correct.

All paths where `managed` can change (`set_tv_option`, `reload_config`) update the
outer flag under the outer write lock. No sync gap.

**Minor style note:** The tuple `(bool, Arc<Mutex<ManagedTv>>)` is fine with the type
alias, but a named struct would be slightly more self-documenting (e.g., accessing
`.managed` vs `.0`). Not worth changing unless the tuple grows more fields.

**Status:** No action needed — verified correct.

---

### I3. `set_tv_option` managed sync acquires write lock after disk persist

**File:** [crates/tisvlayd/src/tv_manager.rs:972-981](crates/tisvlayd/src/tv_manager.rs#L972-L981)

```rust
// Keep the outer managed flag in sync when toggled via SetTvOption.
if key == "managed" {
    if let Ok(managed_val) = value.parse::<bool>() {
        let mut tvs = self.tvs.write().await;
        if let Some((flag, _)) = tvs.get_mut(tv_id) {
            *flag = managed_val;
        }
    }
}
```

This runs *after* the `spawn_blocking` disk persist. The in-memory per-TV config was
already updated at line 958. There's a brief window between the per-TV lock release
(line 959) and the outer flag update (line 976) where `list_managed_tv_ids` could read
the stale outer flag. In practice this is harmless — a single event might use the
old managed status — and the alternative (holding the outer write lock across the
entire persist) would block all TV lookups during disk I/O.

The `if let Ok(...)` silently ignores parse failures. But `apply_tv_option` at line
958 already validates the value — if the parse fails there, the function returns early
with an error before reaching this block. So the double-parse is redundant but safe.

**Severity:** Info (no action needed)

**Status:** No action needed. The double-parse is a minor redundancy but introduces no
risk — `apply_tv_option` gates the early return, so the second parse is unreachable on
failure. The brief flag-lag window is acceptable given the alternative (holding the outer
write lock across disk I/O) would be strictly worse.

---

### I4. `AddTv` D-Bus signature change is breaking for existing clients

**File:** [crates/tisvlayd/src/dbus/manager.rs:93-96](crates/tisvlayd/src/dbus/manager.rs#L93-L96)

The `AddTv` method signature changed from `(name, vendor, config_json)` → `(name,
vendor, config_json, managed)`. This changes the D-Bus type signature from `sss` to
`sssb`. Any existing client calling `AddTv` with the old signature will get a D-Bus
error.

Since there are no stability guarantees and all consumers (CLI, tray) are in-tree and
were updated, this is fine. Just noting it for the record.

**Severity:** Info (intentional API change, no external consumers)

**Status:** Acknowledged. All in-tree consumers (CLI, tray) were updated atomically
with the daemon change. No action needed.

---

### Summary of New Findings

| # | Severity | Item |
|---|----------|------|
| I1 | **Low** | `get_bool` defaults `Managed` to `false` — old code defaulted to `true` |
| I2 | **N/A** | `TvEntry` pattern verified correct; all sync points covered |
| I3 | **Info** | Brief window where outer managed flag lags behind per-TV config (harmless) |
| I4 | **Info** | `AddTv` D-Bus signature change is breaking (no external consumers) |

### Overall Assessment

The H-series fixes are all correctly implemented. The Phase 5C `ObjectManager` adoption
is a significant improvement — `list_tvs()` goes from 1 + 6N round-trips to exactly 1.
The `TvEntry` pattern for `managed` outside the per-TV mutex is clean and eliminates the
lock contention issue.

The only actionable item is I1: consider whether `Managed` should default to `true`
(old behavior, safe-by-default for automation) rather than `false` (new behavior,
safe-by-default for *not* automating) when the property is missing from
`GetManagedObjects`. This is a judgment call on which failure mode is less surprising.

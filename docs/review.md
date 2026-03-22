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

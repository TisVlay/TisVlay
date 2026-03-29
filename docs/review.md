# Review: Per-TV System Profiles and Unified Profile Architecture (594a963..66304e4)

Review of 8 commits implementing per-TV system profiles. The refactor introduces
two profile types (system for daemon lifecycle events, user for session events),
replaces the flat `DaemonConfig` with named system profiles assignable per TV,
and renames all existing profile identifiers to the explicit `user_profile` namespace.

Build: clean. Tests: all pass.

---

## O1. Duplicated resolution logic between `SystemConfig` and `tv_manager`

**Files:**
- [crates/tisvlay-core/src/config.rs:630-645](crates/tisvlay-core/src/config.rs#L630-L645)
- [crates/tisvlayd/src/tv_manager.rs:236-254](crates/tisvlayd/src/tv_manager.rs#L236-L254)

`resolve_system_profile_from_section()` in `tv_manager.rs` is a copy-paste of
`SystemConfig::resolve_system_profile()` in `config.rs`, adapted to take a
`&SystemSection` instead of `&self`. The logic (named → default → hard-coded) is
identical in both.

The `tv_manager` version exists because the manager stores `system_section: RwLock<SystemSection>`
separately from the full `SystemConfig`, so it can't call the `SystemConfig` method.
But the resolution logic could live on `SystemSection` itself:

```rust
impl SystemSection {
    pub fn resolve_profile(&self, profile_name: &str) -> ResolvedSystemProfile { ... }
}
```

Then `SystemConfig::resolve_system_profile` delegates to `self.system.resolve_profile(...)`,
and `tv_manager` calls `system.resolve_profile(...)` directly. One implementation,
zero duplication.

**Severity:** Low (correctness is fine; maintenance risk from logic divergence)

---

## O2. `reload_config` Phase 3 holds per-TV locks under outer read lock

**File:** [crates/tisvlayd/src/tv_manager.rs:1192-1205](crates/tisvlayd/src/tv_manager.rs#L1192-L1205)

```rust
{
    let system = self.system_section.read().await;
    let tvs = self.tvs.read().await;
    for (_, tv_arc) in tvs.values() {
        let mut tv = tv_arc.lock().await;
        tv.system_profile = resolve_system_profile_from_section(...);
    }
}
```

This acquires per-TV mutexes while holding both the `system_section` read lock and the
`tvs` read lock. The profile resolution itself is pure computation (no I/O), so the
per-TV lock durations are short. However, if any per-TV mutex is held by a concurrent
`power_on` (which does network I/O), this loop blocks until that mutex is released —
and the outer `tvs` read lock stays held for the entire duration.

This is the same pattern flagged in the original F10 review, but mitigated here because:
1. Profile re-resolution is rare (only on config reload)
2. The `tvs` lock is read-only (doesn't block TV commands, only blocks `pair_tv`/`remove_tv`)
3. The `system_section` read lock doesn't block anything else during this window

A cleaner approach would be to snapshot the `SystemSection` and release its lock, then
iterate TVs. But the practical impact is negligible for config reloads.

**Severity:** Info (acceptable for rare config reload path)

---

## O3. `set_tv_option("profile.system", ...)` doesn't validate profile name exists

**File:** [crates/tisvlayd/src/tv_manager.rs:1039-1047](crates/tisvlayd/src/tv_manager.rs#L1039-L1047)

```rust
if key == "profile.system" {
    let system = self.system_section.read().await;
    managed.system_profile = resolve_system_profile_from_section(
        &system,
        &managed.config.profile.system,
    );
}
```

When a user sets `profile.system = "nonexistent"`, `apply_tv_option` accepts it
(just a string assignment), and `resolve_system_profile_from_section` falls through
to the "default" profile or hard-coded defaults. The assignment is persisted to disk
with a profile name that doesn't match any defined profile.

This is silent and potentially confusing: `tisvlay profile system get` returns
`"nonexistent"` but the TV behaves like "default". Compare with `set_tv_user_profile`
in session_dbus.rs which validates the name exists before accepting.

**Fix direction:** Check `system.profiles.contains_key(value)` and return an error
if the profile doesn't exist (same pattern as the session service). Alternatively,
accept it with a `warn!` since the admin may intend to add the profile later.

**Severity:** Medium (silent misconfiguration that persists to disk)

---

## O4. `SystemProfileConfig::full_default()` and `UserProfileConfig::full_default()` — good pattern

**Files:**
- [crates/tisvlay-core/src/config.rs:114-122](crates/tisvlay-core/src/config.rs#L114-L122)
- [crates/tisvlay-core/src/config.rs:235-247](crates/tisvlay-core/src/config.rs#L235-L247)

Both profile types have `full_default()` that creates an `Option<T>` config with
all fields set to the resolved defaults. This makes saved configs self-documenting.
Clean pattern.

**Severity:** Info (good design)

---

## O5. `DaemonConfig` cleanly removed — no orphan references

The `DaemonConfig` struct, its `Default` impl, `default_on_startup()`,
`default_on_shutdown()`, and the `set_daemon_option` D-Bus method are all removed.
The `daemon_config: RwLock<DaemonConfig>` field on `TvManager` is replaced by
`system_section: RwLock<SystemSection>`. `DaemonAction` is replaced by `SystemEvent`.

Grepped for `DaemonConfig`, `DaemonAction`, `set_daemon_option`, `daemon_config` —
no stale references remain. The old `profiles: RwLock<HashMap<String, ProfileConfig>>`
is replaced by `user_section: RwLock<UserSection>`. Clean removal.

**Severity:** Info (thorough cleanup)

---

## O6. Phase A rename is mechanical and complete

Commit `05e84f8` does the rename (ProfileConfig → UserProfileConfig, etc.) with no
behavioral changes. Verified by checking that:
- All `ProfileConfig` references become `UserProfileConfig`
- All `ResolvedProfile` references become `ResolvedUserProfile`
- D-Bus method names updated consistently across proxy, service, and client
- CLI subcommand nesting (`profile user list/get/set`)
- Tray labels updated to "User Profile:" / "System Profile:"

The rename is separated from the structural changes in Phase B, which makes
the git history reviewable. Good commit discipline.

**Severity:** Info (clean mechanical rename)

---

## O7. `on_startup` per-TV dispatch — two-pass approach is correct

**File:** [crates/tisvlayd/src/tv_manager.rs:396-529](crates/tisvlayd/src/tv_manager.rs#L396-L529)

The startup flow now iterates managed TVs twice:
1. First pass: handle `PowerOn` TVs (WoL + connect + screen on + input switch)
2. Second pass: handle non-PowerOn actions (ScreenOn, ScreenOff, PowerOff)

This two-pass approach is necessary because `PowerOn` requires wake-and-connect
(slow, sequential per TV), while other actions can be dispatched directly. Interleaving
them would complicate the flow without benefit.

Each TV now gets its own action from its resolved system profile. A guest room TV
with `on_startup = "none"` is skipped entirely, while the living room TV wakes.

**Severity:** Info (correct per-TV dispatch design)

---

## O8. `TvProfileAssignment` and `UserTvProfileAssignment` — clean TOML representation

**Files:**
- [crates/tisvlay-core/src/config.rs:370-406](crates/tisvlay-core/src/config.rs#L370-L406)

```rust
pub struct TvProfileAssignment {
    #[serde(default = "default_profile_name")]
    pub system: String,
}

pub struct UserTvProfileAssignment {
    #[serde(default = "default_profile_name")]
    pub user: String,
}
```

These produce the TOML shapes `profile.system = "default"` on `TvConfig` and
`profile.user = "gaming"` on `UserTvConfig`. The `#[serde(default)]` on the
containing field means existing configs without a `profile` key parse cleanly
with the "default" profile name. Good backward compatibility.

**Severity:** Info (correct serde design)

---

## O9. `list_tvs()` now populates `system_profile` from ObjectManager

**File:** [crates/tisvlay-core/src/dbus_client.rs:498-503](crates/tisvlay-core/src/dbus_client.rs#L498-L503)

```rust
let system_profile = {
    let sp = get_str("SystemProfile");
    if sp.is_empty() { "default".to_string() } else { sp }
};
```

The `SystemProfile` property is read from the ObjectManager response. If missing
or empty, it falls back to "default". This is consistent with the Tv1 D-Bus property
implementation which returns `"default"` when the TV is not found.

The tray uses this to populate the system profile radio group selection, and the CLI
uses it for `profile system get`. Both work correctly with the fallback.

**Severity:** Info (correct fallback)

---

## O10. Tray nested Profile submenu — clean UX

**File:** [crates/tisvlay-tray/src/tray.rs:503-607](crates/tisvlay-tray/src/tray.rs#L503-L607)

The final commit (66304e4) collapses the two separate top-level "User Profile" and
"System Profile" submenus into a single "Profile" parent with "User: active" and
"System: active" as nested sub-submenus. This reduces menu clutter.

The `build_profile_submenu` function now builds both radio groups conditionally
(only if profiles are available for that type). The parent "Profile" label is
static — it doesn't show the active profile names inline, which is fine since
both sub-submenu labels show them.

**Severity:** Info (good UX refinement)

---

## O11. `SystemProfile` D-Bus property is read-only — assignment via `SetTvOption`

**File:** [crates/tisvlayd/src/dbus/tv.rs:102-110](crates/tisvlayd/src/dbus/tv.rs#L102-L110)

The `SystemProfile` property is read-only (no setter). To change it, callers use
`SetTvOption("profile.system", "name")`. This is asymmetric with a typical D-Bus
property pattern (where a writable property has a setter), but consistent with the
existing `Managed` property which also uses `SetTvOption` for writes.

The reason is that `SetTvOption` handles both in-memory update and disk persistence
in one call, while a property setter would need to duplicate that logic. The plan
notes this in C.5. Acceptable trade-off for now; Phase 5 GUI can use the same
`SetTvOption` path.

**Severity:** Info (consistent with existing pattern)

---

## O12. Config TOML `[[tv]]` with `[tv.lg]` vendor section — namespace collision risk?

**File:** [crates/tisvlay-core/src/config.rs:867-891](crates/tisvlay-core/src/config.rs#L867-L891)

The system config uses `[[tv]]` (array of tables) for TV definitions, but also has
`[tv.living-room]` in the user config for per-TV user settings. In a single-file
config (plan Example A), these coexist:

```toml
[[tv]]
id = "living-room"
...

[tv.living-room]
profile.user = "gaming"
```

TOML allows `[[tv]]` (array) and `[tv.living-room]` (table) to coexist because
they're different key types (`tv` as an array vs `tv` as a table). Wait — actually
this would be a TOML conflict. `tv` cannot be both an array of tables and a table.

Let me check: in the system config, `[[tv]]` makes `tv` an array. In the user config,
`[tv.living-room]` makes `tv` a table. These are separate files, so no conflict.
But the plan's Example A shows them in one file — that would fail to parse.

**Severity:** Medium (single-file config example in plan is invalid TOML — `tv` cannot
be both `[[tv]]` array and `[tv.x]` table in the same file. Split-file mode works
fine. The plan's Example A should be corrected or removed if single-file mode isn't
actually supported.)

---

## Summary

| # | Severity | Item |
|---|----------|------|
| O1 | **Low** | Duplicated resolution logic — move to `SystemSection` method |
| O2 | **Info** | `reload_config` holds per-TV locks under read locks (acceptable) |
| O3 | **Medium** | `set_tv_option("profile.system", ...)` doesn't validate profile name exists |
| O4 | **Info** | `full_default()` pattern is clean |
| O5 | **Info** | `DaemonConfig` removal is thorough, no orphan references |
| O6 | **Info** | Phase A rename is mechanical and complete |
| O7 | **Info** | Two-pass `on_startup` dispatch is correct |
| O8 | **Info** | Profile assignment TOML shapes are clean |
| O9 | **Info** | `SystemProfile` ObjectManager fallback is correct |
| O10 | **Info** | Nested Profile submenu is good UX |
| O11 | **Info** | Read-only property + SetTvOption is consistent pattern |
| O12 | **Medium** | Plan Example A single-file config is invalid TOML (`[[tv]]` + `[tv.x]`) |

## Overall Assessment

This is a substantial and well-structured refactor. The six-phase plan was executed
cleanly with good commit separation: Phase A (mechanical rename) is isolated from
Phase B (structural config changes), Phase C (per-TV dispatch), and Phases D/E
(D-Bus + UI surface). The `DaemonConfig` removal is thorough.

Two actionable items:
- **O3** (medium): Validate system profile name in `set_tv_option` before accepting
  and persisting, to prevent silent misconfiguration.
- **O12** (medium): The single-file config example in the plan is invalid TOML.
  Either fix the example (separate the TV array from per-TV user overrides into
  different namespaces) or document that single-file mode is not supported.

The remaining `O1` (low) duplication is a minor cleanup opportunity — move resolution
logic to `SystemSection` to eliminate the copy in `tv_manager.rs`.

# Profile CRUD v2 — per-profile D-Bus objects + typed properties + UUIDs

## Motivation

The first attempt at the profile CRUD work shipped as the unmerged `feat/profile-crud` branch ([PR #58](https://github.com/ruffsl/TisVlay/pull/58)). It was functional but the editor's performance was poor under smoke-testing — every field edit took a JSON serialize → `UpdateProfile` method call → daemon writes TOML → `SystemProfileChanged` signal → GUI refetch of all three inheritance layers → JSON parse → Slint model rebuild → push. The TVs pane's comboboxes (HDMI input, primary input) are fast by comparison because they ride on typed D-Bus properties and per-property `PropertiesChanged` signals — the standard zbus pattern.

Patching v1 would mean caching GUI-side, debouncing edits, and otherwise routing around an architecture mismatch. Instead we scrap v1 and rebuild on the same architecture the TVs pane uses ([PR #36](https://github.com/ruffsl/TisVlay/pull/36) set this up): each profile is a first-class D-Bus object at its own path, identified by an immutable UUID, with one typed `Property` per field. Property edits are single `Properties.Set` round-trips; updates propagate via `Properties.PropertiesChanged`. Immutable UUIDs also unblock rename for free — v1's plan explicitly deferred rename because name-based references required atomically rewriting every TV that used the profile.

Inspired by [PR #56](https://github.com/ruffsl/TisVlay/pull/56) / [PR #57](https://github.com/ruffsl/TisVlay/pull/57), where we similarly redesigned screen-on/off for correctness rather than patching the layered helpers.

## Carry-over vs. drop from v1

Keep (visual + UX patterns held up):

- Stacked-sections master column (System + User), `Nx` referenced-by count, `(default)` badge.
- Used-by clickable TV chips in the detail header.
- `ProfileFieldRow` visual shape: gutter accent + ComboBox + reset glyph (always visible when locally-set).
- `ConfirmDialog` + `NameInputDialog` primitives in `widgets.slint`.
- `Errors.profiles` slot + InlineError banner on `ConfigReloaded(success=false)`.
- All five `docs/ideas/` deferral docs from v1, where they still apply (see "What's deferred" below).

Drop (architecture mismatch with the typed-property approach):

- JSON-string `Create / Update / Get / Delete` methods on `Manager1` / `Session1`.
- `GetResolvedProfileDefaults` — the baseline is baked into the daemon's inheritance walk; clients never need a separate "what's the floor" call.
- `SystemProfileChanged(name, kind)` / `UserProfileListChanged(name, kind)` whole-profile signals — replaced by per-field `PropertiesChanged` + ObjectManager's `InterfacesAdded` / `InterfacesRemoved`.
- `ListSystemProfiles` / `ListUserProfiles` — replaced by `ObjectManager.GetManagedObjects` (same as TVs).
- `crates/tisvlay-gui/src/profiles_state.rs` and its three-layer GUI-side cache — the editor binds directly to per-profile proxy properties; there's nothing left to cache.

## What's decided

1. **Per-profile D-Bus object paths.** `/org/tisvlay/SystemProfile/<UUID_>` on the system bus, `/org/tisvlay/UserProfile/<UUID_>` on the user bus. Mirrors `/org/tisvlay/Tv/<UUID_>`. ObjectManager publishes lifecycle via standard `InterfacesAdded` / `InterfacesRemoved` from `/org/tisvlay` and `/org/tisvlay/Session`.

2. **Profile identity is an immutable UUID.** The TOML on-disk format stores UUIDs as section keys (`[system.profiles."a1b2-..."]`) and as the value of `tv.profile.system` / `tv.profile.user`. Names live as a writable `name` field inside each profile section. Hand-editing TOML is no longer ergonomic but GUI editing is the intended surface; rename via the GUI works without touching any reference site. No backwards-compatibility constraint — we have no shipped users, so the old name-keyed TOML format is dropped outright.

3. **Each profile field is a typed Property.** `SessionAction` and `AutomateWhen` ride as strings using their serde `rename_all = "snake_case"` representation (matches `busctl introspect`'s readability and the existing on-disk TOML). `IdleTimeoutSeconds: u` on user profiles is a raw integer. `Properties.Set` is the single instant-commit path; `Properties.PropertiesChanged` is the single notification path.

4. **Inheritance via a `LocallySetFields: as` companion property.** Each profile's typed properties always report the *resolved* value — the daemon walks the chain (this profile → "default" → hardcoded baseline) server-side. A single `LocallySetFields: as` companion lists the field names the user has explicitly set; the GUI uses this to drive the gutter accent + `(inherit: X)` placeholder + reset glyph visibility. Single companion array beats per-field bool pairs (half the property count, same information density, one signal when the override shape shifts). Modeled on gsettings's `get_value` + `get_user_value` split, just packed differently for D-Bus.

5. **"default" profile has a stable hardcoded UUID.** `00000000-0000-0000-0000-000000000001` for the system-side default, `00000000-0000-0000-0000-000000000002` for the user-side default. The daemon ensures they exist on startup; `Delete` is rejected on both. `Tv1.SystemProfile` defaults to the system-default UUID when unassigned.

6. **Rename ships in v2.** Properties.Set on the `Name` property is the entry point. Trivial because rename is just a property write — no reference walking needed (references are all UUID-keyed). GUI gets an inline pencil-on-name affordance in the detail header.

7. **Scope: daemon refactor + GUI rebind + rename, single PR.** Same scope shape as PR #36. Reviewer reads the new D-Bus surface and the GUI consumer together so any ergonomics mismatch surfaces during review, not after merge.

## D-Bus surface

### `Manager1` (system bus)

Reduced — creation + deletion only; ongoing CRUD lives on each profile's own object.

- `CreateSystemProfile(name: s) -> object_path: o` — generates a UUID, registers a new SystemProfile1 object with `Name = name` and every field inheriting, returns the new object path. Rejects empty name and an existing name.
- `DeleteSystemProfile(uuid: s)` — removes the profile + its object path (fires `ObjectManager.InterfacesRemoved`). Rejects the default UUID. Rejects if any TV's `Tv1.SystemProfile` references it; error names the offending TV(s).
- Signal `ConfigReloaded(success: b, error_msg: s)` — unchanged from v1.

Kept from before v1: `GetSupportedVendors`, `GetPairingSchema`, `AddTv`, `ReloadConfig`, `CancelPair`, `DiscoverTvs`, `PairingProgress` signal.

Dropped: every JSON-string profile method + `SystemProfileChanged` signal + `ListSystemProfiles` (replaced by `ObjectManager.GetManagedObjects`) + `GetResolvedProfileDefaults` + `GetSystemProfile` + `UpdateSystemProfile`.

### `SystemProfile1` (per-profile interface)

Properties (read-write — `Properties.Set` is the edit path):

- `Name: s` — friendly name. Rename = `Properties.Set("Name", ...)`. Daemon validates non-empty + uniqueness; rejects duplicates with a typed error.
- `OnStartup: s` (SessionAction)
- `OnShutdown: s`
- `OnReboot: s`
- `OnNoSession: s`
- `OnSleep: s`
- `OnWake: s`
- `AutomateWhen: s` (AutomateWhen)

Properties (read-only):

- `Id: s` (const) — the UUID.
- `LocallySetFields: as` — names of fields explicitly set on this profile (e.g., `["on_startup", "on_shutdown"]`). Drives the gutter accent / reset-glyph / inherit-placeholder visibility on the GUI side.
- `ReferencedBy: as` — UUIDs of TVs that use this profile. Drives the "Used by [chip][chip]" detail-header row and the in-use Delete rejection. `PropertiesChanged` fires when TVs reassign.

Methods:

- `ResetField(field: s)` — clears one field back to inheriting. Triggers `PropertiesChanged` on both the field's property (resolved value may have shifted) and `LocallySetFields`.
- `ResetAll()` — clears every field. One signal burst.

### `Session1` (user bus)

Mirror reduced surface:

- `CreateUserProfile(name: s) -> object_path: o`
- `DeleteUserProfile(uuid: s)`

Kept: existing per-TV assignment methods (`GetTvUserProfile`, `SetTvUserProfile`, etc.), `GetEffectiveUserProfile`, `ReloadConfig`. Signal: `UserProfileChanged(tv_id, profile_uuid)` — payload migrates from name to UUID.

Dropped: every JSON-string profile method + `UserProfileListChanged` + `ListUserProfiles` (replaced by `ObjectManager.GetManagedObjects`) + `GetUserProfile` + `UpdateUserProfile`.

### `UserProfile1` (per-profile interface)

Properties (read-write):

- `Name: s`
- `OnSessionLock: s`
- `OnSessionUnlock: s`
- `OnGreeterWake: s`
- `OnIdleTimeout: s`
- `OnIdleResume: s`
- `IdleTimeoutSeconds: u` — `0` means "use the value from LocallySetFields to disambiguate"; the inheritance walk treats unset vs `Some(0)` differently because `0` is a legitimate "disable idle" value.

Properties (read-only): `Id`, `LocallySetFields`, `ReferencedBy` (same shape as SystemProfile1).

Methods: `ResetField`, `ResetAll`.

## Inheritance model

Three-layer chain unchanged from v1:

```
edited profile
        ↑ inherits when LocallySetFields doesn't list a field
"default" profile body (resolves same way recursively against the baseline)
        ↑ inherits when "default" doesn't have the field set
hardcoded ResolvedSystemProfile::default() / ResolvedUserProfile::default()
```

The walk happens server-side. Properties always report the resolved (post-walk) value. `LocallySetFields` is the GUI's only signal that distinguishes "the user picked this value" from "this value was inherited" — same information density as v1's three-layer GUI cache, but pushed once per change instead of fetched on every refresh.

User profiles add the admin baseline as an additional layer inside what the session service resolves — same handling as v1 (the admin/user split is invisible to the wire, just like before).

## Config model

```toml
# /var/lib/tisvlay/config.toml

[system]
default_profile = "00000000-0000-0000-0000-000000000001"

[system.profiles."00000000-0000-0000-0000-000000000001"]
name = "default"
on_startup = "power_on"
on_shutdown = "power_off"

[system.profiles."a1b2c3d4-..."]
name = "guest"
on_session_unlock = "screen_on"

[[tv]]
uuid = "..."
profile.system = "00000000-0000-0000-0000-000000000001"
```

Migration from the existing main-branch TOML (which still uses name-keyed sections): daemon detects a string profile reference vs a UUID reference at load time, fails fast with a clear error pointing at the manual rewrite. We don't auto-migrate — the upgrade path is "edit your config once" and there are no users. This keeps the daemon load path simple and the failure mode loud.

## GUI

Visual structure stays the same as v1 (stacked sections, TV chips, ProfileFieldRow with gutter accent + reset glyph, dialogs, error banner). The data flow changes substantially:

- **Master list** is built from `ObjectManager.GetManagedObjects` on `/org/tisvlay` (system) and `/org/tisvlay/Session` (user). Each row reads `Id`, `Name`, `LocallySetFields` count (for the trailing `Nx` hint? no — `ReferencedBy.length` for the Nx hint), `ReferencedBy` from the per-profile object. `InterfacesAdded` / `InterfacesRemoved` drives live add/remove.
- **Selection** binds the editor to one profile's object. zbus's Proxy auto-caches property values and updates them from `PropertiesChanged`; the Rust-side model owns the proxy and pushes Slint-side updates per signal.
- **Field edit** = a single `Properties.Set` call. The echoed `PropertiesChanged` updates the Rust-side cached value; Slint re-renders the one row. No JSON parse on read or write; no rebuild-all-rows on signal.
- **Reset glyph** = `ResetField(field_name)` method call. Reset-all = `ResetAll()`.
- **Rename** = `Properties.Set("Name", new_name)`. New GUI affordance: pencil-on-name in the detail header (commits on Enter or blur).
- **New profile** = `Manager1.CreateSystemProfile(name)`. The new object appears via `InterfacesAdded`; GUI auto-selects.
- **Delete** = `Manager1.DeleteSystemProfile(uuid)`. `InterfacesRemoved` fires; GUI clears selection if the deleted profile was selected. The dialog still surfaces referenced-by-TVs (read from `ReferencedBy` before issuing Delete).
- **Used-by chips** read the per-profile `ReferencedBy` property directly; `PropertiesChanged` keeps them live.

## Implementation outline

Branch off `origin/main` (already done — `feat/profile-crud-v2` is checked out). Each commit compiles + ships incremental functionality. Same convention as v1.

### Commit 1 — Profile UUIDs + per-profile object scaffolding

Daemon-side: assign UUIDs to profiles at config load. Generate object paths. Register `SystemProfile1` interface stubs with only the `Id` property (no field properties yet). Mirror on the user bus. Tests verify the objects appear via `ObjectManager.GetManagedObjects`. TOML format migration: error-on-old-format with a clear message.

### Commit 2 — Typed properties on `SystemProfile1`

Add the 8 read-write properties (`Name` + 7 fields) to `SystemProfile1`. Each reports the resolved value (inheritance-walked server-side). `Properties.Set` wired for each. `LocallySetFields: as` read-only companion + `ResetField` / `ResetAll` methods. `ReferencedBy: as` populated from the live TV list. Integration tests cover Set / Get / Reset / PropertiesChanged emission.

### Commit 3 — `UserProfile1` mirror

Same as Commit 2 on the session bus. Handles the admin/user split inside the resolution walk. `IdleTimeoutSeconds: u` exposed; the `0`-vs-unset disambiguation flows through `LocallySetFields`.

### Commit 4 — TV profile references migrate to UUIDs

`Tv1.SystemProfile` and `Tv1.UserProfile` properties carry UUIDs instead of names. The daemon's cached resolved profile per TV continues to work, just routed through the new UUID-keyed map. Properties.Set takes a UUID; rejects unknown UUIDs.

### Commit 5 — `Manager1.{Create,Delete}SystemProfile` + `Session1.{Create,Delete}UserProfile`

Reduced surface plus the kind-rejection logic (no `default` delete, no in-use delete). `DaemonClient` / `SessionClient` Rust helpers. Integration tests cover the lifecycle + error variants.

### Commit 6 — GUI Profiles pane: master list rebound to ObjectManager

Replace the placeholder pane with the stacked-sections master list + TV-chip detail-header + empty detail body. Rust side: build the Slint model from `ObjectManager.GetManagedObjects` + listen to `InterfacesAdded` / `InterfacesRemoved`. No detail editor yet.

### Commit 7 — GUI detail editor: bind `ProfileFieldRow` to per-profile properties

Each row's `current-value` is fed from the Rust-side proxy cache (which auto-updates from `PropertiesChanged`). Combobox selection → `Properties.Set`. Reset glyph → `ResetField`. `LocallySetFields` drives the gutter accent + placeholder render. Reset-all toolbar button → `ResetAll`. Confirm + name-input dialogs ported from v1's primitives.

### Commit 8 — Rename UX

Inline edit affordance on the detail-header profile name (pencil glyph; commits on Enter / blur). `Properties.Set("Name", new_name)`. The master list re-renders the row from `PropertiesChanged`. Conflict surfaces as a toast (server-side dup-name rejection).

### Commit 9 — Polish: error banner, a11y, ConfigReloaded handling

Wire `Errors.profiles` slot to `ConfigReloaded(success=false)` (kept from v1). A11y pass: `accessible-role` / `accessible-label` / `accessible-action-default` on dialogs, section-header `+`, profile-list rows, reset glyph, rename pencil.

### Commit 10 — design.md write-up + delete plan doc

Per `docs/plans/README.md` convention. Architecture lands in `docs/design.md` under "Profile catalog (system + user)" — replaces what v1 wrote there if anything stuck. Idea docs survive where still relevant (numeric spinner, free-text field, save/cancel revisit); `profile-rename.md` gets marked done; `profile-create-dialog-dup-name-validation.md` gets revisited (with per-profile objects + UUIDs, dup-name validation can be done client-side from the master list's Names — but the GUI still has to handle the server-side rejection for race conditions).

## What's deferred / out of scope

- **Effective-profile inspector.** Same as v1 — file as a new idea if it surfaces.
- **Per-TV profile override editor.** Already exists in the TVs pane.
- **Free-text profile fields.** Idea doc stays.
- **Save/cancel revisit.** Idea doc stays.
- **Numeric spinner row.** Idea doc stays, even though `IdleTimeoutSeconds: u` is now wire-native — the spinner UX is still a separate problem.

## Acceptance criteria

- [ ] `just check` green.
- [ ] Per-field edit latency feels instant under smoke-testing (single D-Bus round-trip — target ≤ 50 ms perceived, vs v1's hundreds-of-ms refresh cycle).
- [ ] Rename: open a profile, change its name in the GUI, verify referencing TVs continue to resolve correctly (UUID stayed the same).
- [ ] Round-trip create → assign-to-TV → delete-blocked → reassign → delete via GUI; CLI sees consistent state at every step.
- [ ] Edit conflict (external `tisvlay reload` while editor open): property values + `LocallySetFields` refresh; no JSON-parse round-trip.
- [ ] Delete blocked when TV references the profile; dialog names the offending TV.
- [ ] ConfigReloaded(success=false) surfaces in `Errors.profiles` banner.
- [ ] No editor-wide save/cancel button.

## Reference

- v1 plan (unmerged): `docs/plans/profile-crud-and-pane.md` on `feat/profile-crud`. Deviation log + acceptance-criteria carry forward where still relevant.
- [PR #36](https://github.com/ruffsl/TisVlay/pull/36) — TVs to first-class D-Bus objects with UUIDs. Architectural precedent.
- [PR #56](https://github.com/ruffsl/TisVlay/pull/56) / [PR #57](https://github.com/ruffsl/TisVlay/pull/57) — screen-on/off redesign cycle. Reliability-by-architecture rather than reliability-by-patching.
- [PR #58](https://github.com/ruffsl/TisVlay/pull/58) — v1 attempt; smoke-test failure on UI responsiveness is what motivates this redo.
- Inheritance chains documented on `SystemProfileConfig` / `UserProfileConfig` doc-comments in [crates/tisvlay-core/src/config.rs](../../crates/tisvlay-core/src/config.rs).
- gsettings inheritance model (`g_settings_get_value` vs `g_settings_get_user_value`) — closest D-Bus-adjacent prior art for the "is this field overridden vs inherited" split. We pack the same information differently to keep the property surface compact.

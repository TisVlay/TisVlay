# Profile CRUD daemon methods + Profiles pane (M3.5 + M4)

## Status (2026-05-16)

Commits 1–9a have landed on `feat/profile-crud`; 9b (reset-all + error banner + a11y) and 10 (this doc gets deleted per `docs/plans/README.md` convention) are still pending. Live behavior matches the acceptance criteria except where called out in "Deviations from this plan" below. The implementation outline at the bottom tracks per-commit status with ✓ markers.

## Motivation

The Profiles pane in `tisvlay-gui` is a placeholder shipped at M3 ("System and user profile editing arrives in M4."). The daemon-side CRUD surface it would consume doesn't exist yet either. Both were deferred from the GUI roadmap in `docs/plan.md`. This plan lands them together — M3.5 (daemon methods) is a one-PR prerequisite for M4 (GUI pane); sequencing them as separate PRs adds friction without buying anything.

The deferred-from-V2 `GetEffectiveSystemProfile` / `GetEffectiveUserProfile` methods are NOT included. They returned fully-resolved profiles, which is the wrong shape for an editor that needs to distinguish "this profile explicitly set this field" from "this field inherits from default." The right surface for that distinction is `GetSystemProfile(name) -> raw_json_with_nulls` plus client-side inheritance walk — see the inheritance section below.

## What's already decided (from research + earlier discussion)

1. **Inherit-UX pattern**: per-field combobox with the inherited value injected as the first entry (`(inherit: screen_off)`). Selecting it sets the field to `None`. A 2 px left-gutter accent marks rows whose value is locally-set in the editing profile. Trailing reset glyph (visible on hover when locally-set) clears the field back to `None`. Modeled on VS Code's "modified" indicator + Visual Studio property pages' "evaluated value" inline pattern. Avoids needing custom Slint widgets — stock `ComboBox` with a Rust-computed model handles it.

2. **Baseline source**: hardcoded `ResolvedSystemProfile::default()` / `ResolvedUserProfile::default()` exposed via new daemon method (`Manager1.GetResolvedProfileDefaults()` / `Session1.GetResolvedProfileDefaults()`), not duplicated in the GUI as a const table. Industry convention (NetworkManager, systemd, polkit, BlueZ) — daemons expose canonical values; clients query, don't compile in copies. Fetched once when the Profiles pane mounts, cached for the editor's lifetime.

3. **Commit semantics**: instant-commit per field. Combobox selection → `UpdateSystemProfile`; numeric/text field → commit on blur or Enter; reset glyph → commit on click. No global save/cancel button. Adds a "Reset all to inherited" toolbar button for bulk undo. Rationale: save/cancel is friction tax for the drive-by-edit case (the common case); profile fields are almost all enum-typed where partial state can't be invalid; concurrent reload-vs-edit is unavoidable regardless of editor model and is bounded by serializing daemon writes-and-reloads under a mutex.

4. **Concurrent reload UX**: when `ConfigReloaded` / `SystemProfileChanged` fires while the editor is open, the editor refreshes silently — pending edits visible in unsaved fields would already have been committed (instant-commit), so there's nothing to lose. If a future field requires save/cancel semantics (e.g., a free-text "profile description"), wrap that field in commit-on-blur, not the whole editor.

5. **Reactivity**: Slint property bindings handle the "inherited-value-changes-while-editor-is-open" case naturally. GUI state lives in a Rust-side `Profiles` global; D-Bus signals trigger refetch which pushes to the global which updates Slint bindings. Same pattern the TVs pane uses for `Tv1.*` properties.

## Daemon surface (M3.5)

### `Manager1` (system bus)

- `GetResolvedProfileDefaults() -> json: s` — returns `ResolvedSystemProfile::default()` serialized to JSON. One call at editor open; cached for the editor's lifetime.
- `CreateSystemProfile(name: s, json: s) -> ()` — `name` must not be `"default"` (the daemon owns the default's lifecycle); fails if name already exists. `json` is a `SystemProfileConfig` with optional `null` fields; rejected if unparseable or contains unknown keys.
- `UpdateSystemProfile(name: s, json: s) -> ()` — fails if name doesn't exist. Same JSON validation.
- `DeleteSystemProfile(name: s) -> ()` — fails if `name == "default"` or any `[[tv]]` entry's `profile.system` field references it. Error message names the offending TV(s) so the user knows what to reassign first.
- `GetSystemProfile(name: s) -> json: s` — returns the raw unmerged `SystemProfileConfig` (with `Option<T>` fields preserved as JSON `null`s). Errors `ProfileNotFound` if the name isn't in the system config.
- Signal `SystemProfileChanged(name: s, kind: s)` — `kind` ∈ `"created"`, `"updated"`, `"deleted"`. Fires after a successful CRUD method completes. Also fires post-`ReloadConfig` for any profile whose disk-read body differs from the in-memory snapshot (one signal per affected profile).
- Signal `ConfigReloaded` extended: `ConfigReloaded(success: b, error_msg: s)` — replaces the current zero-arg form. Pre-v1, no compatibility shim; one-line update at the two existing client call sites (tray, GUI).

### `Session1` (user bus)

Mirror of `Manager1`'s profile surface, but for user profiles:

- `GetResolvedProfileDefaults() -> json: s` — returns `ResolvedUserProfile::default()` serialized.
- `CreateUserProfile(name: s, json: s) -> ()` — `name` must not be `"default"` (the merged admin baseline owns that name; users override via their own config file).
- `UpdateUserProfile(name: s, json: s) -> ()`.
- `DeleteUserProfile(name: s) -> ()` — fails if any `[[tv]]` in the user config assigns it.
- `GetUserProfile(name: s) -> json: s` — returns the user-config profile (unmerged) OR the admin baseline body if the profile isn't user-defined.
- Signal `UserProfileListChanged(name: s, kind: s)` — distinct from the existing per-TV `UserProfileChanged(tv_id, profile_name)`. Fires for CRUD operations on the user profile catalog.

### Validation policy

- Server-side validation rejects malformed JSON, unknown keys, and out-of-range numeric values (`idle_timeout_seconds < 0`). Daemon mutates in-memory state first; only persists to disk after validation passes; emits the change signal last so subscribers observing the signal can refetch a coherent state.
- Concurrent writes are serialized under the daemon's existing config mutex. Reload-vs-write race: write-then-reload sees the write, reload-then-write sees old disk state then overwrites with the in-flight write.

## GUI Profiles pane (M4)

### Layout

Single window pane, master-detail. The original plan placed System / User as a top-bar segmented control; shipped layout instead stacks them as two sections inside the master column (KDE / GNOME / Firefox-preferences sidebar idiom) — frees the detail to use the full window height when the editor grows.

- **Left list**: two stacked sections (`SYSTEM PROFILES`, `USER PROFILES`), each with its own header band + `+` button. Profile names with `(default)` badge on the reserved `default` profile and a trailing `Nx` count indicating how many TVs reference the profile. Single selection across both sections; clicking a row sets `Profiles.selected-kind` + `selected-name`.
- **Right detail**: profile name + Duplicate / Delete actions + a `Used by [chip][chip]` row in the header (clickable chips navigate to the TVs pane via `Profiles.open-tv(uuid)` — "Find Usages" idiom from GitHub package pages / VS Code peek). Below: one row per inheritable field.
- **Field row layout** (live):

```
┌─────────────────────────────────────────────────────────────┐
│ ▌ on_session_lock    [ screen_off          v ]  ↺          │  ← locally-set
│   on_session_unlock  [ (inherit: screen_on) v ]             │  ← unset / inheriting
│ ▌ on_idle_timeout    [ power_off           v ]  ↺          │  ← locally-set
└─────────────────────────────────────────────────────────────┘
```

- Left gutter accent (▌) is a 2 px-wide vertical bar in the theme's accent color, visible only on rows whose value is locally set in the editing profile.
- Reset glyph (↺) renders only when locally-set; it stays always visible at low opacity (IconBtn ghost style brightens on hover) — the original "hover-revealed via row-wide overlay" pattern would have swallowed ComboBox clicks.
- Combobox model is computed Rust-side: `["(inherit: <effective_fallthrough>)", "power_on", "power_off", "screen_on", "screen_off", "none"]`. Selecting the first entry → `Manager1.UpdateSystemProfile(name, {field: null, ...})` ; selecting any other → `UpdateSystemProfile(name, {field: "X", ...})`.
- The numeric `idle_timeout_seconds` row is **not shipped** in M4 — it needs a spinner + commit-on-blur that doesn't fit the shared ComboBox-based `ProfileFieldRow`. Deferred per `docs/ideas/profile-user-idle-timeout-spinner.md`; users edit it via TOML until then.

### List actions

- **New** — opens editor with empty form (all fields `null`, inheriting). User-supplied name; validation rejects `"default"` and existing names.
- **Duplicate** — `Get` + `Create` client-side; name `<orig>-copy` (user can rename in a sibling rename dialog if/when that lands per `docs/ideas/profile-rename.md`). Preserves `null`s so the copy inherits the same way the original did, not a frozen-snapshot of the resolved values.
- **Delete** — confirmation dialog showing referencing TVs if any (the daemon's `DeleteSystemProfile` error message is the source of truth here).
- **Reset all to inherited** — toolbar button on the editor; clears every field in the editing profile to `null` via one `UpdateSystemProfile` call.

### Inheritance computation (Rust-side)

For each field on the editing profile, compute the inherited fallthrough display value:

```rust
fn inherited_value(field: SystemField, editing: &str, list: &HashMap<String, SystemProfileConfig>, defaults: &ResolvedSystemProfile) -> SessionAction {
    if editing == "default" {
        // The "default" profile only falls through to the hardcoded baseline.
        return defaults.get(field);
    }
    if let Some(v) = list["default"].get(field) {
        return v;
    }
    defaults.get(field)
}
```

User profiles add one more step: the editing-profile → admin baseline (`Manager1.GetUserProfileDefault()`) → `Session1.GetResolvedProfileDefaults()`. The function shape is the same, just an extra chain link.

The inheritance computation runs in `state.rs` (or a new `profiles_state.rs` module) as a pure function over the cached `Profiles` global; results are pushed as a per-row `inherited-value: string` property on the Slint model. Slint bindings auto-update when the underlying global changes.

### Concurrent edit / live update handling

- `Manager1.ConfigReloaded(success: b, error_msg: s)`: GUI refetches the full profile list + the editing profile body. On `success == false`, surface `error_msg` in an inline banner via the existing `InlineMessage` widget; the editor stays open with last-known good state.
- `Manager1.SystemProfileChanged(name, kind)`: GUI refetches the named profile (or removes it from the list if `kind == "deleted"`). If the editing profile itself is deleted, close the editor with a toast.
- `Session1.UserProfileListChanged(name, kind)`: same shape on the user sub-tab.
- No banner for the silent-refresh case (concurrent edit by another client) — instant-commit means the user's last action was already on disk before the reload picked up. The combobox snapping to the reloaded value is rare and recoverable (user re-picks).

## Implementation outline

Branch off `origin/main`. Each commit compiles + ships incremental functionality.

### Commit 1 — `GetResolvedProfileDefaults` on Manager1 + Session1 ✓

Add the two D-Bus methods that return the hardcoded `Resolved*Profile::default()` values as JSON. Cheap; unblocks the inheritance computation on the GUI side. Add CLI smoke test (`tisvlay defaults` or via `busctl call`) and a doc-test on the JSON shape.

### Commit 2 — System profile CRUD on Manager1 ✓

Add `CreateSystemProfile`, `UpdateSystemProfile`, `DeleteSystemProfile`, `GetSystemProfile` + `SystemProfileChanged` signal. JSON validation, mutex-serialized writes, TV-reference check on Delete. Integration tests under `tisvlayd/tests/` cover the success / not-found / conflict / TV-reference cases.

### Commit 3 — User profile CRUD on Session1 ✓

Mirror of Commit 2 on the user bus. `tisvlay-session/tests/` integration coverage.

### Commit 4 — Extend `ConfigReloaded` signal ✓

`Manager1.ConfigReloaded(success: b, error_msg: s)` (replaces zero-arg form). Update tray and GUI to accept the new args. Pre-v1, no shim. Add a regression test that the daemon emits with `success=false` + non-empty error when an on-disk reload fails.

### Commit 5 — Profiles pane scaffolding ✓

Replace the placeholder `profiles_pane.slint` with a real master-detail layout. Two stacked sections in the master column (System / User) instead of the originally-planned top-bar segmented control. List + empty editor; no field rows yet. Wires up `ListSystemProfiles` / `ListUserProfiles` for the list, `GetSystemProfile` / `GetUserProfile` for selection. Used-by clickable chips in the detail header (navigate to TVs pane). Create / Duplicate / Delete actions are stub toasts until 9a.

### Commit 6 — Inheritable combobox + field row ✓

Add a `ProfileFieldRow` Slint component that renders one row of the editor with the gutter accent + inherit-prefix combobox + reset glyph. Driven by a new `ProfileFieldRow` Rust model (mirrors the existing `PairingFieldRow` pattern in `schema_form.rs`). Combobox model is `["(inherit: <fallthrough>)", ...enum_values]`. Selecting the first entry emits `value-edited(None)`; selecting another emits `value-edited(Some(s))`. Reset glyph always-visible-when-locally-set instead of hover-revealed (row-wide TouchArea would block ComboBox clicks).

### Commit 7 — Wire system profile editor ✓

Inheritance computation in Rust (`crates/tisvlay-gui/src/profiles_state.rs`). On combobox selection: instant-commit via `Manager1.UpdateSystemProfile`. Live refresh on `ConfigReloaded` (re-runs `refresh_system_editor` for the currently-selected profile). Dedicated `SystemProfileChanged` subscription deferred to 9b.

### Commit 8 — Wire user profile editor ✓

Mirror of Commit 7 against `Session1`. Refactored `ProfilesEditorState.system_selected` into a `SelectedProfile::{System, User}` enum so the field-edit handler can dispatch by match. `idle_timeout_seconds` (the only non-`SessionAction` field) deferred — see `docs/ideas/profile-user-idle-timeout-spinner.md`. Five `SessionAction` rows ship today.

### Commit 9a — Dialogs ✓

`ConfirmDialog` promoted from `tvs_pane.slint` into `widgets.slint`; new `NameInputDialog` added. Both auto-grow vertically with content. Wired in `ProfilesPane`:

- **Delete-confirm**: flips into "cannot delete" mode when the profile is referenced — message names the offending TVs (`referenced_by_summary` pre-built Rust-side), Confirm disabled, only Cancel works.
- **New (Create)**: name-input dialog with client-side validation for empty + reserved (`"default"`). Duplicate-name validation falls back to a daemon-error toast — Slint pure functions can't iterate the existing-names list, so client-side dup-check is deferred.
- **Duplicate**: no dialog; `Get` + `Create("<orig>-copy[-N]")` with the existing-names list pre-fetched for collision avoidance. Auto-selects the new profile.

### Commit 9b — Reset-all + error banner + a11y →

Reset-all-to-inherited toolbar button on the detail header (one `Update*Profile(name, {})` clears every field). `InlineMessage`-style error surfacing on `ConfigReloaded(success=false)` — probably wants a new `Errors.profiles` slot distinct from `Errors.tvs`. Dedicated `SystemProfileChanged` / `UserProfileListChanged` subscription so cross-client edits propagate without waiting for the next `ConfigReloaded`. Final accessibility pass (tab order, screen reader labels per `crates/tisvlay-gui/ui/components/` conventions).

### Commit 10 — Delete this plan doc

Per `docs/plans/README.md` convention. Architecture lands in `docs/design.md` ("Profiles" section under D-Bus surface); deferred follow-ups (rename UX, profile-description free-text field, save/cancel revisit, numeric spinner row, client-side duplicate-name check) get filed under `docs/ideas/`.

## Deviations from this plan

Recorded during implementation. None invalidated the plan's core decisions; they're documented here so a reviewer reading the branch history doesn't have to diff prose against code to spot the differences.

- **Sub-tab placement.** Plan: top-bar segmented control. Shipped: two stacked sections inside the master column (KDE / GNOME / Firefox-preferences sidebar idiom). Frees the editor for full window-height usage and scales naturally if a third profile kind ever lands.
- **TV-reference chips.** Not in the original plan. Added a clickable `Used by [chip][chip]` row in the detail header — Rust precomputes a `referenced_by` list per profile from the existing TV state; clicks route via a new `Profiles.open-tv(uuid)` callback that flips `AppWindow.active-pane` + `selected-tv-index`. "Find Usages" idiom from GitHub package pages / VS Code peek.
- **Reset-glyph visibility.** Plan: hover-revealed (visible only when the row is hovered). Shipped: always visible when locally-set, ghost-style `IconBtn` for the hover feedback on the glyph itself. A row-wide hover-detecting `TouchArea` would have intercepted ComboBox clicks; the always-visible variant gives the same visual result without the click conflict.
- **`idle_timeout_seconds` numeric row deferred.** Plan: spinner row with `placeholder=N` ghost text. Shipped: not rendered at all (`apply_user_field_edit` returns a hard error on this field name so a stray emit fails loudly). Filed in `docs/ideas/profile-user-idle-timeout-spinner.md` with the rationale and design sketch. User-profile editor renders 5 `SessionAction` rows for v1; the 6th remains TOML-editable.
- **Commit 9 split into 9a + 9b.** The polish work didn't fit one commit cleanly: 9a covers the dialogs (Delete-confirm, Duplicate, New) and the reusable widgets that came out of that work (`ConfirmDialog` promoted to `widgets.slint`, new `NameInputDialog`). 9b covers reset-all + error banner + the cross-client `*ProfileChanged` subscription + a11y.
- **Duplicate-name validation on Create.** Plan: "validation rejects `'default'` and existing names." Shipped: client-side check for `"default"` + empty; duplicate-name check happens server-side and surfaces as a toast (Slint pure functions can't iterate the existing-names list in a `function`). Filed as a follow-up if the toast UX feels bad in practice.
- **Live-refresh signal subscription.** Plan: "Subscribe to `SystemProfileChanged` and `ConfigReloaded` for live refresh." Shipped through 9a: `ConfigReloaded` only. The dedicated `SystemProfileChanged` / `UserProfileListChanged` subscription is part of 9b — until then, cross-client edits propagate via the next `ConfigReloaded` rather than instantly.
- **Selection-row clearing on Delete.** Plan didn't specify behavior. Shipped: successful Delete clears `selected-kind` + `selected-name` + pushes empty field rows, returning the detail to the "Select a profile" empty state. Same path covers the "selected profile was deleted by another client" case via the existing `apply_snapshot` re-resolve.

## What's deferred / out of scope

- **Profile rename.** Filed in `docs/ideas/profile-rename.md`. Daemon would need to walk all TVs referencing the old name and update them; transactional semantics around partial failure get hairy. Pick up when there's a real user complaint.
- **Free-text fields on profiles.** No current `SystemProfileConfig` / `UserProfileConfig` field is free-text. If someone adds e.g. a `description: Option<String>` field later, that single row gets commit-on-blur semantics — the editor-wide instant-commit model isn't worth abandoning for one field.
- **Per-TV profile override editor.** This plan is about editing the *named profile catalog* (the `[system.profiles.guest]` blocks). The per-TV "which profile does this TV use" selector already exists in the TVs pane (`Tv1.SystemProfile` writable property). No work needed here.
- **Effective-profile display.** If a user wants "show me what this TV will actually do for event X" rather than "show me what the profile says about event X", that's a separate "TV inspector" feature — file as a new idea if it surfaces.
- **Save/cancel revisit.** If field-blur instant-commit ends up feeling fiddly in practice (especially for the numeric spinner), revisit by wrapping the spinner in commit-on-explicit-Enter. Editor-wide save/cancel stays off the roadmap unless field complexity demands it.
- **Numeric `idle_timeout_seconds` row.** Deferred during 8 — see `docs/ideas/profile-user-idle-timeout-spinner.md`. Filed because the shared `ProfileFieldRow` is ComboBox-based; a numeric row needs a spinner + commit-on-blur that doesn't fit the same primitive. Pick up when there's a second numeric field or user complaints about TOML editing this one.
- **Client-side duplicate-name validation in the New dialog.** Server-side check + toast covers the case today; inline validation would need either a Rust-pushed `Profiles.create-error` property or a Slint substring-contains trick on a sentinel-joined names string. Revisit if the toast UX feels bad.

## Acceptance criteria

- [x] `just check` green.
- [x] Round-trip: create a new system profile via the GUI, see it appear in `ListSystemProfiles` from the CLI, delete it via the CLI, see it disappear from the GUI list. (Wired through 9a; CLI round-trip verifiable today.)
- [x] Edit conflict: open the editor on `guest`, edit `[system.profiles.default]` on disk, run `tisvlay reload`, verify the `(inherit: ...)` combobox entries refresh in-place to the new defaults without losing locally-set rows in `guest`. (`refresh_system_editor` fires on `ConfigReloaded`.)
- [x] Delete blocked: assign `guest` to a TV, try to delete `guest`, verify the error names the offending TV. (Client-side: dialog shows the referenced-by list and disables Confirm. Server-side: daemon also rejects with the named TVs in the error message as a safety net.)
- [ ] Reset-all clears the editing profile to the inheriting state in one daemon call (`UpdateSystemProfile(name, {})`). (Pending 9b.)
- [x] No editor-wide save/cancel button anywhere in the pane.

## Reference

- GUI roadmap context: [docs/plan.md](../plan.md) §M3.5 + §M4.
- Existing schema-form pattern reused for the new field row: `crates/tisvlay-gui/src/schema_form.rs` + `crates/tisvlay-gui/ui/components/schema_form.slint`.
- Hardcoded baselines exposed by the new method: `tisvlay_core::ResolvedSystemProfile::default()` + `tisvlay_core::ResolvedUserProfile::default()`.
- Inheritance chains are documented on the `SystemProfileConfig` / `UserProfileConfig` doc-comments in [crates/tisvlay-core/src/config.rs](../../crates/tisvlay-core/src/config.rs).
- UX research synthesis (prior-art survey across `about:config`, VS Code Settings UI, Visual Studio property pages, dconf-editor, KWin Rules, nm-connection-editor) is captured in the design-discussion record on the planning conversation; the dominant pattern adopted here is "bold/marker + reset-per-row" with the inherit value surfaced inline as a prefixed combobox entry.

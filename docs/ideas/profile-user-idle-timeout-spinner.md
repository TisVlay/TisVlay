# Profiles pane: numeric editor for `idle_timeout_seconds`

The user-profile editor in `tisvlay-gui` ships without an inline editor for `idle_timeout_seconds` (the `Option<u64>` field on `UserProfileConfig`). All five `SessionAction`-typed fields render as inheritable comboboxes via the shared `ProfileFieldRow` component; the numeric field is the odd one out.

## Why deferred

The `ProfileFieldRow` component takes a `[string]` model and routes selection back as `value-edited(field-name, value)`. A numeric spinner needs:

- A different widget — Slint `SpinBox` (clamped to a `min..=max` range) or a free-form `LineEdit` with input validation.
- A different commit semantic: instant-commit on every keystroke would issue a `Session1.UpdateUserProfile` per character. The plan calls out commit-on-blur or commit-on-Enter as the natural fit for free-form numerics.
- An "inherit" affordance distinct from a ComboBox: the plan's mock used a placeholder text reading `placeholder=0` (the fallthrough value rendered as ghost text inside the input). That doesn't map onto Slint's built-in `SpinBox` either.

Spending a single commit on the spinner-and-validate plumbing without bleeding into the rest of the editor felt cleaner than half-landing a numeric input alongside the simpler enum rows.

## Workaround

For now, users edit `idle_timeout_seconds` via the on-disk session TOML config. The session service's `ReloadConfig` D-Bus method picks up the change without a restart; the GUI's existing `ConfigReloaded` listener re-renders the rest of the profile editor automatically.

## When to do it

Reasonable triggers:

- A second numeric field appears on `SystemProfileConfig` or `UserProfileConfig` — at that point a numeric `ProfileFieldRow` variant earns its keep.
- User feedback complains about editing the TOML for this one field.
- The polish commit (M4 step 9 in the original plan) gets prioritized over deferred follow-ups.

## Sketch

A workable shape:

1. Extend the Slint `ProfileField` struct with a `kind` discriminator: `enum_choice | integer`.
2. Add `int-value: int`, `int-min: int`, `int-max: int` properties on the row.
3. In `ProfileFieldRow`, render either the existing `ComboBox` or a `SpinBox` based on `kind`.
4. Wire a separate `int-value-edited(field-name, value)` callback (or reuse `value-edited` with the integer rendered as `to_string`).
5. Rust-side `apply_user_field_edit("idle_timeout_seconds", "300")` parses + clamps + writes.
6. Commit on `SpinBox.edited` (likely on-blur — verify Slint's emit semantics) or on explicit Enter.

## See also

- `docs/plans/profile-crud-and-pane.md` § "What's deferred / out of scope" — flags this and a few related "needs more design" follow-ups.
- `crates/tisvlay-gui/src/profiles_state.rs::apply_user_field_edit` — currently returns `Err("idle_timeout_seconds editor is not implemented yet")` on this field name so a stray emit fails loudly rather than silently no-opping.

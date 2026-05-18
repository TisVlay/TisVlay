# User-profile idle-timeout spinner row

`UserProfile1.IdleTimeoutSeconds: u` is wire-native — daemon stores it, `Properties.Set` updates it, `LocallySetFields` round-trips it. But the Profiles-pane field-editor's `ProfileFieldRow` only handles `SessionAction` enums via a ComboBox; there's no row type for an unbounded numeric value. So the field is intentionally absent from `USER_FIELDS` in `crates/tisvlay-gui/src/profiles.rs`.

## Why a ComboBox doesn't cover it

`SessionAction` is a closed set of five values rendered as a dropdown. `IdleTimeoutSeconds` is a non-negative integer (seconds; `0` is a legitimate "disable idle" value, distinct from inheriting). A dropdown would either pre-bake a handful of bucket options (`30`, `60`, `300`, …) — losing free-input — or scale poorly across the realistic range (10s … hours).

## Path forward

Drop a numeric-spinner variant of `ProfileFieldRow`:

- A bespoke `NumberInputField` widget paralleling `LineEditField`: text input restricted to digits, with up/down step buttons. Slint has no built-in spinbox skin worth using (std `SpinBox` is platform-themed and doesn't match the rest of the pane).
- `FieldDef` grows a kind tag (`Enum { options }` / `Number { min, max, step }`) so `build_row` can dispatch to either ComboBox or NumberInputField on the Rust side.
- `ProfileFieldEntry` Slint struct gains a `kind` discriminator and a `number_value: int` field alongside the existing `current_value: string`.
- The pane's `for entry[i] in Profiles.editor-fields` repeater branches on `entry.kind` to render the matching row component.
- The inherit-marker convention stays the same (`*` suffix on the displayed value when inheriting; reset glyph as the revert path).

## Why deferred

Today the only user-profile field needing this is `IdleTimeoutSeconds`. The other six user-side fields are all `SessionAction` enums. One field's worth of bespoke row type isn't worth the row-component dispatch machinery yet — the user-pane editor will stay at six rows for the foreseeable future. Revisit when a second numeric field surfaces (e.g. a future `IdleWarningSeconds` lead-up timer), or a user files a real need to surface the timeout in the GUI.

## Workaround in the meantime

CLI / `busctl` works:
```
busctl --user set-property org.tisvlay.Session /org/tisvlay/UserProfile/<UUID_> \
    org.tisvlay.UserProfile1 IdleTimeoutSeconds u 600
```

The daemon-side path is fully functional; only the GUI surface is missing.

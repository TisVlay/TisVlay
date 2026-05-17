# Profiles pane: revisit editor-wide save/cancel

The M3.5/M4 profile editor commits each field instantly: every ComboBox selection or reset glyph click fires a `Update*Profile(name, body)` round-trip. Reset-all is the only batch operation. There's no global Save/Cancel button.

The rationale (locked in early in the plan) was:

- Profile fields are almost all enum-typed; partial state can't be invalid.
- Drive-by edits are the common case — friction tax of a Save button doesn't earn its keep.
- Concurrent reload-vs-edit is unavoidable regardless of editor model.
- The daemon serializes writes under `disk_write_lock`, so per-field commits stay ordered.

## When this assumption breaks

The model holds while every field is enum-typed and instantly-validatable. Two scenarios force a rethink:

1. **A new free-text field lands.** Per `profile-description-free-text-field.md`, that single row gets `commit-on-blur` semantics — not the whole editor. As long as only the description is free-text, the model still holds for the rest.
2. **A field requires cross-field validation.** Example: a future `idle_timeout_seconds + on_idle_timeout` pair where setting one to a meaningful value without the other is semantically invalid. Today the daemon would accept the half-state and the user would see weird behavior. A save/cancel model would let the GUI validate the whole-body before committing.
3. **A field requires confirmation.** Example: setting `on_shutdown = power_off` on a system profile assigned to a critical workload-display TV. Per-field instant-commit means the user can fat-finger this; a save/cancel model would surface a "you sure?" confirm.

## What the revisit would look like

A workable shape:

1. Add a `dirty: bool` property to `Profiles` global, set Rust-side after each field edit (compare cached body against the body fetched at selection time).
2. Add a `Save` / `Discard` toolbar pair to the detail header (next to or replacing Reset-all). Disabled when `!dirty`.
3. Per-field edits update a *local* draft body inside `ProfilesEditorState` rather than calling `Update*Profile` directly. Save flushes the draft; Discard re-fetches the on-disk body.
4. Close-without-save: prompt if `dirty` when the user navigates away from the editor or closes the window.
5. Concurrent-edit handling: keep the silent-refresh path, but if the on-disk body diverges from the editor's *base* body (the snapshot at selection time), surface a "this profile was edited externally — your changes will overwrite" confirm. Same shape as VS Code's "file changed on disk" prompt.

## Why not now

- No concrete user complaint or use case.
- The instant-commit UX is more familiar to GUI users who expect macOS / GNOME settings semantics; save/cancel is a power-user model.
- Adds non-trivial state (drift detection, prompt-on-navigate, conflict resolution). Worth doing when one of the trigger scenarios above is real.

## See also

- `docs/plans/profile-crud-and-pane.md` § "What's already decided" / "What's deferred / out of scope" — both ends of the discussion are captured there.
- `profile-description-free-text-field.md` — the most-likely first trigger.

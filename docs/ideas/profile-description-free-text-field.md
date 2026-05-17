# Profiles pane: free-text description on a profile

`SystemProfileConfig` and `UserProfileConfig` are pure enum-set bags today — every field is either `Option<SessionAction>`, `Option<AutomateWhen>`, or `Option<u64>`. The Profiles pane editor assumes this: every shipped row uses `ProfileFieldRow`'s shared ComboBox + inherit-prefix model, and instant-commit is safe because partial selections can't be invalid.

A future `description: Option<String>` field (or any other free-text field) breaks both assumptions: free-text doesn't have a finite option set, and instant-commit on every keystroke would issue a `Update*Profile` per character.

## Why deferred

No current field is free-text, so the editor never has to confront the problem. Adding a description field is itself an open design question (do profiles need names beyond their slug? where would a description display in the TVs-pane ComboBox?) so deferring it is mostly a "no concrete use case yet" decision.

## Path forward when this matters

1. **Schema change**: add the field to `SystemProfileConfig` / `UserProfileConfig` in `tisvlay-core` with `Option<String>` shape, matching the existing per-field inheritance pattern.
2. **Slint `ProfileField` variant**: introduce a `kind` discriminator (probably the same one the numeric-spinner idea proposes — see `profile-user-idle-timeout-spinner.md`) and a `text-value: string` property alongside `options` / `current-value`.
3. **ProfileFieldRow**: render either the existing ComboBox or a `LineEdit` based on `kind`.
4. **Commit semantic**: the editor stays instant-commit for enum rows but the description row commits on blur or explicit Enter, not on every keystroke. The plan's "What's deferred" section flags this exact split as the natural carve-out — `commit-on-blur` lives on the row, the editor as a whole stays save/cancel-free.
5. **Validation**: probably none beyond length-cap; description fields don't have a "this string is invalid" surface unless we go down a markdown-rendering rabbit hole (let's not).

## See also

- `docs/plans/profile-crud-and-pane.md` § "What's deferred / out of scope" — calls this out alongside the numeric-spinner case.
- `profile-user-idle-timeout-spinner.md` — adjacent deferral with overlapping design footprint (the `kind` discriminator on the Slint row).
- `profile-editor-save-cancel-revisit.md` — sibling idea for the broader "what if commit-on-blur per row isn't enough" case.

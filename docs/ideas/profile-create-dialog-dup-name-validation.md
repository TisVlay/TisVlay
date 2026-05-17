# New-profile dialog: client-side duplicate-name validation

The Profiles-pane New-profile dialog (`NameInputDialog` invoked from `Profiles.create-system` / `create-user`) validates two cases inline as the user types:

- Empty name → Confirm disabled.
- `"default"` → "Reserved name" error inline.

Duplicate names (a system profile named `guest` already exists, user types `guest`) are **not** caught inline. The dialog accepts the name, fires `Profiles.confirm-create(kind, name)`, and the Rust handler calls `Manager1.CreateSystemProfile`. The daemon rejects with "profile 'guest' already exists"; Rust surfaces that as a `push_tvs_error` toast. The user has to reopen the dialog and try again.

## Why deferred

Slint pure functions can't iterate the existing-names list. The `validate-new-name(name) -> string` function on `ProfilesPane` can check string equality with literals (`name == "default"`), but it can't do `for r in Profiles.system: if r.name == name { return "duplicate"; }` — Slint's expression language forbids `for` inside a `function` body. So inline validation either needs:

1. Rust pushing back to a `Profiles.create-error` property after observing the keystroke (a callback round-trip per character).
2. A precomputed substring-contains trick on a sentinel-joined `|name1|name2|...` string, with Slint's `string.contains(...)` (if it exists; needs verification).
3. Promoting the list to a Slint `function` argument that the Slint runtime can iterate (it can't — same limitation).

Both (1) and (2) are workable; (1) trades clarity for latency, (2) trades robustness (sentinel collisions) for simplicity. Neither was worth a polish-pass commit before the rest of the pane was usable.

## Workaround

Server-side error surfaces as a toast. UX cost: user has to reopen the dialog after a failed create. Not great, not terrible — the conflict rate is bounded by how often users pick already-taken names, which is "rarely" in practice (profile names are typically descriptive: `guest`, `movies`, `office`).

## Path forward when this matters

Likely (1) is the cleanest:

1. Add `in property <string> create-error` on the `Profiles` global, Rust-pushed.
2. On every `name-changed(v)` Slint callback, also dispatch a `Profiles.validate-create-name(kind, v)` Rust callback. Rust reads its current cached profile-row list and pushes `create-error` accordingly.
3. The dialog's `error-message` becomes `error || create-error`.
4. Race-resistance: bump a counter on each validate request, check it on the push side; stale validations don't overwrite fresh state.

## See also

- `docs/plans/profile-crud-and-pane.md` § "Deviations from this plan" — flags this as a 9a-time deferral with a brief description of the trade-off.
- `crates/tisvlay-gui/ui/components/profiles_pane.slint::validate-new-name` — the Slint-side validation function. Currently checks only `""` and `"default"`.

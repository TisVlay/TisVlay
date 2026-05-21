# Per-TV automated tint in the Profiles-pane Used-by row

The profile detail header's "Used by:" row lists referencing TVs as `LinkText` spans separated by middots. Today every span renders at `Theme.info-text` regardless of whether the individual TV is currently automated. The aggregate "Active" badge in the same header conveys "at least one referencing TV is automated", but the per-TV detail is collapsed.

The pattern this should grow into matches the 3-state classification the master-list row dots already use:

- Unassigned → not in the list by construction (`references_of` filters)
- Assigned (referenced, not automated) → `Theme.info-text`
- Active (referenced, automated) → `Theme.success-text`

## Plumbing scope (small)

The fan-out helper `tv_meta` in `state.rs::convert_snapshot` already returns `(name, automated)` per TV (`HashMap<Uuid, (String, bool)>`). `build_profile_rows` already inspects the boolean for the row-level `active` aggregate; the per-TV value is discarded when pushing into `ProfileTvRefRaw`. Five-line change:

1. `ProfileTvRefRaw` (Rust, `state.rs`) grows `automated: bool`.
2. `ProfileTvRef` (Slint struct, `globals/profiles.slint`) grows `automated: bool`.
3. `build_profile_rows` populates the new field.
4. `state_watch.rs::raw_to_slint` mirrors the field.
5. Used-by-row `LinkText` binds `text-color: tv.automated ? Theme.success-text : Theme.info-text`.

## Accessibility — load-bearing followup

Color is the only differential between Assigned and Active in this row, which fails WCAG SC 1.4.1 (Use of Color). The master list pairs the same color with a textual status label ("Assigned" / "Active"); the Used-by row has no per-TV space for a label.

Mitigation: extend `LinkText` to take an optional `accessible-label` override (currently hardcoded to `root.text`). The Used-by call site binds `accessible-label: tv.automated ? @tr("{}, automated", tv.name) : tv.name`. Screen readers announce the state alongside the name. Sighted color-blind users still don't get the visual cue — acceptable mitigation given the textual badge is already visible at the header level.

Same accessible-label override pays forward to any future call site where a `LinkText` carries auxiliary state.

## Why not chips

Per-TV chips were tried earlier and rejected. Chip borders + padding pushed the profile-pane header ~5 px taller than the TVs-pane header, breaking visual parity. The mid-dot-separated spans match the TVs-pane descriptor row's height by construction. Color-tint preserves that constraint.

## Out of scope here

- Hover / pressed states on `LinkText`. Tracked separately — the current pointer-cursor convention conflicts with the user's "pointer = external link only" convention. See the LinkText hover state work.
- Keyboard navigation. `LinkText` is currently mouse-only; `TouchArea` doesn't grab focus. A separate keyboard-reach pass would wrap each `LinkText` in a `FocusScope` and surface a focus ring.

## Trigger

File as a follow-up when the next pass through the Profiles-pane detail header lands, or when a user reports the per-TV detail as missing. Implementation is small; the gate is the accessibility followup, which is what makes this a worth-doing item rather than a one-line color tweak.

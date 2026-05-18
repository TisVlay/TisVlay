# Profiles pane ↔ TVs pane visual parity

## Motivation

The Profiles pane silhouette already mirrors the TVs pane (left master + right detail), but the leaf widgets have drifted: master-list rows show a plain text name + `Nx` reference count instead of a tile + status line, and the detail header is a 2-row vertical stack instead of the TVs pane's 3-column horizontal layout. This pass aligns the leaf widgets so the two panes read as siblings, and adds first-class status indicators (Assigned/Active) that pull through from each profile's referencing TVs.

The tray already uses the same `im-user` / `system-users` icon distinction for user vs system profile submenus (`crates/tisvlay-tray/src/tray.rs::build_profile_submenu`); promoting that affordance into the GUI keeps the cross-surface idiom consistent.

## What's decided

1. **`ProfileTile` widget** parallels `VendorTile`. Takes `kind: ProfileKind` + `tile-size: length`; renders `im-user` for `ProfileKind.user`, `system-users` for `ProfileKind.system`. New entries on the `Icons` global (`Icons.user`, `Icons.system_users`) loaded Rust-side via `load_themed_icon`.

2. **Two status badges in the detail header**, on the right column, mirroring the TVs-pane shape:
   - **Assigned / Unassigned** — analogous to Connected/Disconnected.
     - Assigned = `referenced_by.len() > 0`. Tone `info`.
     - Unassigned = no references. Tone `neutral`.
   - **Active / Inactive** — analogous to Automated/Managed/Unmanaged.
     - Active = ∃ TV: `tv.profile == this AND tv.automated`. Tone `success`.
     - Inactive = otherwise (no references, or all referencing TVs are not automated). Tone `neutral`.
   - Both chips always render (mirrors TVs-pane unconditional pair).

3. **Master-list per-row status text** replaces the `Nx` trailing count.
   - 3 states, single label + dot:
     - `referenced_by.is_empty()` → `Unassigned`, tone `text-faint` / dim.
     - assigned, not active → `Assigned`, tone `info-text`.
     - active → `Active`, tone `success-text`.
   - Same line shape as `TvListRow`: dot (sized from `Design.size-status-dot`) + status text in `Design.font-mini`.
   - Reference count is implied by Assigned vs Unassigned; exact count is still visible in the detail header's "Used by" chip row.

4. **Detail header restructure** to a single-row, 3-column layout (matching TVs):
   - Col 1: `ProfileTile` at `Design.size-tile-md`.
   - Col 2 (flex, `horizontal-stretch: 1`): two stacked rows.
     - Row a: profile name + left-justified action icons (Rename / Duplicate / Delete in view mode; Save / Cancel in edit mode). Same affordance set as today.
     - Row b: "Used by" chips, wrapped in a **horizontal `ScrollView`** so a long list scroll-flickers instead of clipping into the badges column. Hidden when `referenced_by.is_empty()` to match current behavior. Falls under the name+actions row so the whole middle column gets the full row's height budget.
   - Col 3: two chips stacked (Assigned + Active), same `VerticalLayout` + `alignment: end` pattern as TVs-pane.
   - Padding values (`padding-top: 18px;`, `padding-bottom: gap-row;`) copied verbatim from TVs-pane so header heights match.

5. **`active: bool` lands on `ProfileRowRaw`** (Rust) and `ProfileRow` (Slint). Computed in `convert_snapshot` by joining each profile's `referenced_by` UUID set with the in-memory TV list (already in scope) and asking `any(|tv| tv.automated)`. Slint side derives the 3-state status label from `active` + `referenced_by.length`.

6. **`referenced_by_summary` survives** — still consumed by the delete-confirm dialog's "Cannot delete 'X': still used by Y, Z" message.

## Data-model deltas

```rust
// crates/tisvlay-gui/src/state.rs
pub struct ProfileRowRaw {
    pub uuid: SharedString,
    pub name: SharedString,
    pub is_default: bool,
    pub referenced_by: Vec<TvRefRaw>,
    pub referenced_by_summary: SharedString,
    pub active: bool,   // NEW — ≥1 referencing TV is automated.
}
```

```slint
// crates/tisvlay-gui/ui/globals/profiles.slint
export struct ProfileRow {
    uuid: string,
    name: string,
    is-default: bool,
    referenced-by: [ProfileTvRef],
    referenced-by-summary: string,
    active: bool,   // NEW.
}
```

No new wire-protocol changes — `active` is a Rust-side derivation from data the daemon already exposes (`ReferencedBy` + each TV's `Automated` property, both already on the snapshot).

## Visual sketches

**Master list row** (icon + name + status line, mirroring `TvListRow`):
```
┌──┬───────────────────────────────────┐
│👤│ evening                           │
│  │ ● Active                          │
└──┴───────────────────────────────────┘
```

**Detail header** (single row, 3 columns):
```
┌──────┬───────────────────────────────────┬──────────────┐
│      │ evening    [edit][copy][trash]    │  [Assigned]  │
│ 👤   │ Used by:  ← LR  TV  TV  TV  TV → │  [ Active  ]  │
└──────┴───────────────────────────────────┴──────────────┘
```

(The `← →` on the Used-by row indicates horizontal scroll.)

## Implementation outline

Branch off `main`. Each commit compiles and ships a self-contained slice; one PR.

### Commit 1 — `ProfileTile` widget + `Icons` plumbing
- Add `user` and `system_users` `image` properties on `Icons` global.
- Load them Rust-side in `register_callbacks` (or wherever the existing icons load) via `load_themed_icon(&["im-user"])` and `load_themed_icon(&["system-users"])`.
- New `ProfileTile` component in `widgets.slint` paralleling `VendorTile`. Inputs: `kind: ProfileKind`, `tile-size: length`. Picks the right icon per kind. Tone identical to `VendorTile` (rounded square w/ icon).
- No usage changes in this commit; tile is dead code until commit 3/4 wires it in.

### Commit 2 — `active: bool` + 3-state status derivation
- `ProfileRowRaw.active: bool` field.
- `convert_snapshot` populates `active` per profile by walking `referenced_by` UUIDs against the TV map. Requires `automated: bool` to be available on each TV in the snapshot — already is via `TvInfo`.
- `ProfileRow` Slint struct mirrors the field.
- Update `build_profile_rows` Rust unit tests to cover the 3 states + the "any automated" rollup.
- No UI changes in this commit; the field is unused until commit 3/4.

### Commit 3 — Master-list row redesign
- Replace `ProfileListRow`'s inner `HorizontalLayout` with the TVs-pane shape: `ProfileTile` (size-tile-sm) + name column (stretchy) where the row 2 shows the 3-state status label with the status dot.
- Drop the trailing `Nx` `Text`. `(default)` badge stays where it is.
- Drop `referenced_by_summary` from the master-list row visuals (still on the struct for the delete dialog).
- Row height grows from 40 px → 64 px? Or keep 40 px and pack tighter? Decision: match `TvListRow` at 64 px so the masters feel identical.

### Commit 4 — Detail header restructure
- Rewrite `ProfileDetailHeader`'s `header-layout` from `VerticalLayout` to `HorizontalLayout` with 3 columns: tile + middle (2 rows: name+actions, Used-by chip ticker) + status badges stack.
- Wrap the Used-by chip row in a horizontal `ScrollView` (`viewport-height: self.visible-height; vertical-scrollbar-policy: always-off;`) so overflow ticks sideways. Verify behavior with a synthetic profile that references 10+ TVs.
- Status badge column uses the same `VerticalLayout { alignment: center; spacing: gap-tight; }` shape as `TvDetailHeader`.
- Padding values copied from `TvDetailHeader` (`padding-top: 18px; padding-bottom: gap-row;`) — header heights should match by construction.

### Commit 5 — Visual polish + tests
- Side-by-side visual check vs TVs pane. Record any tweaks (probably: spacing between columns, vertical alignment of action icons against name).
- Add an integration check (Rust state-snapshot test): pseudo-profile with 0 / 1-non-automated / 1-automated referencing TVs produces the expected `active` values.
- Refresh `tisvlay-gui.pot` for new strings (`Assigned`, `Unassigned`, `Active`, `Inactive`).

## Open questions / risks

- **Master-list row height bump (40 → 64 px)** halves the number of rows visible per section. The 50/50 split between System and User sections (just landed) compounds this — a master column that fits 10 system profiles today would fit 5 after the row-height change, with scroll picking up the rest. Acceptable trade-off for the visual symmetry, but worth flagging.
- **Horizontal scroll on the Used-by row in Slint**: `ScrollView` works in Slint but the horizontal-only variant might want a `Flickable` instead. Need to experiment in commit 4; if `ScrollView`'s viewport-width binding doesn't quite get us the ticker behavior, fall back to a `Flickable` with a wide inner content rect.
- **`active` recomputation latency**: today every `convert_snapshot` re-derives it from the TV list. If a TV's `automated` flips, the next state refresh recomputes — same cadence as other derivations. No new event.
- **Default profile's status**: the reserved default has `referenced_by` populated for every TV that doesn't explicitly assign another profile. So the default will almost always be Assigned and often Active. That's correct — but the "(default)" badge stays as the identity marker (not a status marker), so both can coexist.

## Acceptance criteria

- [ ] `ProfileTile` renders `im-user` for user profiles, `system-users` for system profiles, at both list-row size and detail-header size.
- [ ] Master-list rows render: tile + name + (default badge if applicable) + status dot + 3-state status text. No `Nx` count.
- [ ] Detail header is a single horizontal row with tile + middle column (name + actions + Used-by chip ticker) + badges column. Header height visually matches the TVs-pane header.
- [ ] Used-by chip row scrolls horizontally when overflowed; never clips into the badges column.
- [ ] Assigned/Unassigned and Active/Inactive chips always render together. State combinations cover the 3 distinct profile states.
- [ ] Active correctly reflects "≥1 referencing TV with `automated: true`" — covered by a state-snapshot unit test.
- [ ] `just check` green; i18n catalog updated.

## Reference

- TVs-pane `TvListRow` (rows) and `TvDetailHeader` (detail) are the visual references.
- Tray uses `im-user` / `system-users` for the same distinction — `crates/tisvlay-tray/src/tray.rs::build_profile_submenu`.
- Profile-CRUD v2 plan: `docs/plans/profile-crud-v2.md` (closes out into design.md once these follow-up refactors land).

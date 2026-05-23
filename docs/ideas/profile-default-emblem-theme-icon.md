# Theme-icon upgrade for the default-profile indicator

`tisvlay-gui` ships the default-profile marker as a literal `\u{2713}` CHECK MARK codepoint suffixed onto the profile's display name — synthesized at `state.rs::display_profile_name` and rendered through plain `Text` widgets across the master list, detail header, and TVs-pane profile-selector ComboBoxes. The codepoint approach is functional and HIG-correct on a few axes (screen readers pronounce "check mark," text-presentation by default keeps the glyph in the system text font, no `@tr()` catalog pollution) but it's a pragmatic compromise — the textbook KDE answer is a theme icon via `freedesktop-icons` lookup of `emblem-default-symbolic`.

The relevant facts, verified locally:

- Breeze's `emblem-default-symbolic.svg` is **unambiguously a checkmark** (verified by dumping `share/icons/breeze/emblems/16/emblem-default-symbolic.svg`). The semantic glyph matches the codepoint we currently use — switching to the theme-icon path would render the same shape, just in the user's selected icon theme.
- KDE's canonical pattern is in [plasma-bigscreen's `AudioDelegate.qml`](https://github.com/KDE/plasma-bigscreen/blob/master/kcms/audio-device-chooser/ui/delegates/AudioDelegate.qml): trailing-slot `Kirigami.Icon { source: 'emblem-default-symbolic' }` next to each list-row text.
- [KDE HIG: Icons](https://develop.kde.org/hig/icons/) explicitly says: "Don't bundle custom icons … get them from the system's icon theme."

## Why deferred

Three things keep this from being a straightforward swap today:

1. **Slint's std-widget `ComboBox` can't host per-row icons.** Its model is `[string]`; the renderer paints text only. The TVs-pane System / User profile pickers are std `ComboBox`. Adding an icon-bearing row template means either extending the in-tree `ThemedSelect` widget (mentioned in [.claude/slint.md](../../.claude/slint.md)) with an optional trailing-icon slot, or accepting a hybrid where master list + detail header use the theme icon while ComboBoxes keep the suffix-text codepoint. Both shapes ship; the ThemedSelect extension is meaningfully bigger work than the current single-helper synthesis.

2. **Adwaita doesn't ship `emblem-default-symbolic`.** Breeze + hicolor cover it, but on a pure GNOME / Cosmic system the freedesktop icon cascade falls through to whatever the active theme provides — possibly a missing-icon placeholder. A real cross-DE smoke test (Adwaita-only, Cosmic-only) is needed before declaring victory. App-bundled SVG fallback under `crates/tisvlay-gui/assets/` is the conventional escape hatch, but adds maintenance.

3. **a11y label refactor on `LinkText` + `Image` widgets.** The current text-suffix carries the announcement implicitly (screen readers read the whole string). An `Image` widget needs `accessible-role: image; accessible-label: @tr("Default profile")` to give AT a meaningful label. The `LinkText` widget already grew an `accessible-label-override` for the per-TV automated-tint work; the same pattern extends here.

## Path forward when picked up

Single coherent commit if scoped tight:

- `crates/tisvlay-gui/src/main.rs::load_themed_icon` (or equivalent) gets an `emblem-default-symbolic` entry. The existing `freedesktop-icons` plumbing already handles the cascade.
- `ProfileListRow` in `profiles_pane.slint`: trailing `Image { source: ...; visible: entry.is-default; accessible-label: @tr("Default profile") }` next to the name. Drop the `\u{2713}` suffix from `display_name` for surfaces that get the widget. The detail-header read-only Text gets the same trailing image.
- `ThemedSelect` widget grows an optional per-row `image-source: image` slot, falling back to text-only rendering when unset. TVs-pane profile pickers switch from std `ComboBox` to `ThemedSelect` and bind `image-source: row.is-default ? @image-url(".../emblem-default-symbolic.svg") : @image-url("")`.
- `display_profile_name` helper in `state.rs` collapses to just the localized name (no suffix), since every render site now has the widget-level icon. The `name` / `display_name` two-field split on `ProfileRow` may also collapse back to a single `name` if no caller still wants the suffix-text form.
- Cross-DE smoke: hit each surface under Plasma (Breeze), GNOME (Adwaita), and Cosmic. File a bundled-asset fallback if any cascade misses.

Tooltips + accessible-labels are the harder-than-it-looks piece — make sure each surface announces "Default profile" identically so screen reader users hear consistent semantics. Worth a test-fixture pass through the headless `i-slint-backend-testing` accessibility query API to lock the announcements.

## What to leave alone

The editor's inherit-overlay marker on enum field rows (in `inherit_overlay()` in `profiles.rs`) is a different concern from the default-profile indicator. It happens to reuse the same `\u{2713}` codepoint for visual cohesion with the master-list marker, but its mechanism (defeat the `ComboBox.selected(v)` no-op by ensuring the displayed string differs from any wire token) requires a string-level differentiator. Replacing it with an image widget would break the no-op-defeating mechanism — keep the codepoint there.

## Triggers for picking this up

- Any user report about the indicator's appearance on Adwaita / Cosmic (visual regression vs Breeze).
- An adjacent UI surface needing per-row icons in ComboBoxes, which would force the ThemedSelect extension anyway and amortize that cost.
- A round of cross-DE polish before a 1.0 ship.

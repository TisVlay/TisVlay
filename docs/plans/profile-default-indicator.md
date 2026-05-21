# Default-profile indicator: consistent ✓ across every surface

## Motivation

After the v3 profile-CRUD work landed, the default profile was marked with `⚙️` (U+2699 gear) in two places — the master-list row's trailing badge and the editor's inherit overlay on inheriting enum fields. HIG research uncovered three problems with that choice that justified a rework:

1. **Wrong glyph semantically.** `⚙` is the universal "settings/configure" icon across desktop conventions (KDE Plasma, GNOME, macOS, Fluent). A user scanning the Profiles pane reads `default ⚙` as "click here to configure this row," not "this row is the default." KDE's actual emblem for "this is the default" is `emblem-default-symbolic`, which is a **checkmark** in Breeze, not a gear.

2. **Indicator missing from three surfaces.** The default profile is rendered in the master list, the detail header, the TVs-pane System-profile ComboBox, and the TVs-pane User-profile ComboBox. The gear only appeared in the master list. Inconsistent — a user could see "default" in the ComboBox value and have no visual cue that the assigned profile is the reserved default.

3. **i18n + a11y hygiene.** `@tr("⚙️")` puts an emoji codepoint into the gettext catalog as a `msgid` translators can't translate. Bare `⚙` codepoint also gets pronounced as "gear" by screen readers — semantically wrong for a default-indicator.

The fix is small but touches several files. The architectural decisions worth preserving (single helper, display/identity split, codepoint choice rationale, deferred theme-icon path) justify a checked-in plan file.

## Design decision: ✓ U+2713 across every display surface

The chosen glyph is **`✓` U+2713 CHECK MARK** (text presentation by default, no VS16 / VS15 needed). Rationale:

- **Semantic match.** Breeze's `emblem-default-symbolic` SVG is unambiguously a tick path. Picking the `✓` codepoint means the in-string indicator and the eventual theme-icon upgrade render the same shape. Visual consistency is preserved across any future Slint widget refactor.
- **Screen-reader friendly.** Orca, NVDA, JAWS, VoiceOver, and KDE's accessible bridge all pronounce `✓` as "check mark." Generic but semantically right for a default-indicator. Compare `⚙` → "gear" (wrong).
- **Width.** Single codepoint, ~10 px in the standard text font. No layout regression vs the prior gear; fits comfortably in the 180 px TVs-pane ComboBox value column.
- **Default Unicode presentation.** U+2713 has text presentation by default — renders in the system text font (Inter / system sans), shares baseline + weight with surrounding text. Unlike `⭐` U+2B50 or `⚙️ ` U+2699 U+FE0F which can flip to colored emoji rendering.
- **`@tr()` cleanup.** Codepoint is synthesized in Rust via `format!()` — never wrapped in `@tr()`. Next `just i18n-extract` removes the emoji `msgid` we currently carry.

The indicator appears on **every** display surface that renders a profile name. Native settings apps (KDE's `plasma-bigscreen/AudioDelegate.qml`, macOS' default-printer pickers) show the default emblem in every list, dropdown, and detail surface — not gated on "would surrounding text already convey this?" KDE convention: the indicator is the visible truth of `is_default`, displayed wherever the item is rendered.

## Wire vs display split

The default-ness is **wire identity** (the profile's UUID matches `DEFAULT_*_PROFILE_UUID`). The `✓` is **presentation**. The synthesis happens client-side at the display layer, **not** in `body.name` on disk or on the bus. Specifically:

- Daemon stores `body.name = "default"` (English literal) for the reserved-default profile. Unchanged.
- The daemon's inheritance predicate `body.name == "default"` keeps working. Unchanged.
- CLI / scripts / sysadmin TOML edits see normal names. Unchanged.
- The GUI's `convert_snapshot` synthesizes a `display_name` via a helper that wraps `gettext("default")` with the trailing `✓` when the UUID is the reserved-default.

`ProfileRow` (Slint) and `ProfileRowRaw` (Rust) grow a `display_name: SharedString` field alongside the existing `name: SharedString`. Per-surface choice:

| Surface | Binds to |
|---|---|
| Profiles master list row | `display_name` |
| Profile detail header read-only name | `display_name` |
| Rename-pencil `LineEdit` prefill | `name` (bare — the daemon validates it; ✓ is not a legal name character) |
| TVs-pane System-profile ComboBox model | `display_name` (synthesized for the default UUID's entry; bare for others) |
| TVs-pane User-profile ComboBox model | `display_name` (same) |
| Editor inherit overlay on enum field rows | `inherit_overlay()` swaps glyph from `⚙️` to `\u{00A0}\u{2713}` (NBSP + ✓) |

The rename-input split is the load-bearing piece: confusing `name` with `display_name` at the rename site would either send `✓` to the daemon (rejected as invalid name char) or silently strip on submit (surprise magic). Explicit two-field shape avoids both.

## Semantic collision (✓ means two related things)

`✓` carries two related-but-distinct meanings across the pane:

- **Master list / header / ComboBox**: "this profile **IS** the default."
- **Editor inherit overlay on field rows**: "this field **INHERITS FROM** the default profile."

HIG research (KDE, GNOME, Material, Fluent) doesn't address this exact "is-X / inherits-from-X" pairing. The closest prior art (KConfigXT's "differs from default" highlighting) handles only one side of the pair. Decision: **reuse the glyph with tooltip disambiguation.** Both meanings cluster around the default-profile concept, so visual cohesion wins over pedantic split. Tooltips / accessible-labels carry the disambiguating word.

Corollary worth flagging: when the user is viewing the default profile itself, every inheriting field row shows `… ✓` (Inheriting from baseline since this IS the default), AND the header / master list shows `default ✓` (this IS default). Two ✓s appear in the same pane. Reads as "default-y throughout" — matches user intuition even though the underlying semantics differ.

## Accessibility labels

Wrapping `@tr()` around the `✓` codepoint pollutes the gettext catalog. Instead, attach translatable `accessible-label` properties to the elements that render the indicator:

- Master list row → `accessible-label: @tr("Default profile")` on the row (or on a wrapping element AT can reach).
- Detail header → `accessible-label: @tr("Default profile")` on the name element.
- Editor field row with inherit overlay → `accessible-label: @tr("Inherits from default profile")` on the ComboBox row (if std-widget accessibility plumbing permits — may need to validate against Slint's a11y story).
- TVs-pane ComboBox: std-widget ComboBox renders row text verbatim and announces it as-is. Screen reader gets "Standard check mark" — semantically right, terse. Acceptable v1; theme-icon upgrade (see deferred follow-ups) gives the proper accessible-label per row.

## Implementation sketch

Single commit, scope kept tight. File touch list:

- `crates/tisvlay-gui/src/state.rs` — `ProfileRowRaw` grows `display_name: String`; helper `display_*_profile_name(uuid, body) -> String` synthesizes the suffix; `convert_snapshot` populates both `name` and `display_name`. TVs-pane ComboBox model strings switch to the helper.
- `crates/tisvlay-gui/src/profiles.rs` — `inherit_overlay()` literal swap (`⚙️` → `\u{00A0}\u{2713}`). Existing tests that compute the expected value via `inherit_overlay()` directly need no touch.
- `crates/tisvlay-gui/ui/globals/profiles.slint` — `ProfileRow` grows `display-name: string` field.
- `crates/tisvlay-gui/ui/components/profiles_pane.slint` — master-list row uses `display-name` for the visible Text and drops the `if root.entry.is-default: Text { @tr("⚙️") }` conditional. Detail-header read-only name uses `display-name`. Rename-pencil `LineEdit` binds `name`. Accessible-labels added.
- `crates/tisvlay-gui/ui/components/tvs_pane.slint` — TVs-pane ComboBoxes consume the synthesized display strings from the model.

No behavior change on the wire or on disk. Tests should remain green; one test currently asserts `inherit_overlay(spec, "screen_off")` which adapts to the new glyph automatically (glyph-agnostic by construction since the prior cleanup).

## Acceptance criteria

- `just check` end-to-end green on the merge commit.
- `prek run -a` green.
- All four display surfaces (master list, detail header, TVs-pane System ComboBox, TVs-pane User ComboBox) render the default profile with a trailing `✓`.
- Rename-pencil `LineEdit` prefills with the bare name (no `✓`).
- Editor's inheriting enum field rows render the ComboBox value with a trailing `✓`.
- `just i18n-extract` produces a `.pot` with no `⚙️` or `✓` `msgid` — the catalog gets the new `Default profile` / `Inherits from default profile` accessible-label entries instead.
- Screen-reader manual smoke: in Orca on KDE, navigating to the master-list default row announces "Default profile" (or similar; Slint's accessible-label flow through to AT-SPI).
- Visual smoke via the Slint MCP server (see `.claude/slint.md` MCP workflow): all four surfaces show the same `✓` placement; no width regressions in the TVs-pane ComboBox columns.

## Deferred follow-ups

- **Theme-icon vector upgrade.** Master list + detail header could upgrade from a `✓` codepoint in a `Text` to an `Image { source: load_themed_icon("emblem-default-symbolic"); accessible-label: @tr("Default profile") }` widget. Renders with theme-correct stroke weight + accent color; auto-flips for LTR/RTL via widget layout. TVs-pane ComboBoxes would need `ThemedSelect` extended to support trailing icons per row (the std `ComboBox` only takes a `[string]` model). Bigger refactor; ships as a clean additive change later. File to `docs/ideas/profile-default-emblem-theme-icon.md` when this plan migrates.
- **HIG-spec icon-cascade audit.** Adwaita doesn't ship `emblem-default-symbolic` directly — falls back through hicolor or app-bundled. Worth validating cross-DE coverage when the theme-icon upgrade lands. If the cascade fails for a real-world DE, ship a fallback SVG with the app under `crates/tisvlay-gui/assets/`.

## HIG research citations (anchor for future archaeology)

The choice of `✓` over alternatives (`⚙` rejected, `★` considered, `⭐` rejected for emoji presentation, `(default)` text-suffix rejected for width + redundancy) flowed from research against:

- [KDE HIG — Icons](https://develop.kde.org/hig/icons/) — "Don't bundle custom icons … get them from the system's icon theme."
- [KDE HIG — Emblem Icons](https://develop.kde.org/hig/icons/monochrome/emblem/) — emblem-default-symbolic is the canonical "is-default" emblem.
- [FreeDesktop Icon Naming Spec](https://specifications.freedesktop.org/icon-naming/latest/) — defines `emblem-default` semantically.
- [KDE plasma-bigscreen AudioDelegate.qml](https://github.com/KDE/plasma-bigscreen/blob/master/kcms/audio-device-chooser/ui/delegates/AudioDelegate.qml) — canonical KDE pattern for trailing default emblem.
- Breeze SVG source for `emblem-default-symbolic` (verified locally) — confirms checkmark shape, not star.

## Durable docs migration on merge

When this work merges, no design.md update is needed — the Profile catalog subsection already covers the wire surface and inheritance chain, and this is a GUI-presentation refinement that doesn't change wire shape. The plan file deletes per `docs/plans/README.md` convention. The two deferred follow-ups migrate to `docs/ideas/`.

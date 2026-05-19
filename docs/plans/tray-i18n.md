# Tray i18n expansion + Slint upstream watch

## Motivation

`tisvlay-gui` is i18n-ready from day one: Slint `@tr("…")` for UI literals + `gettextrs::gettext("…")` for Rust-side user-facing strings, both feeding a shared gettext catalog at `crates/tisvlay-gui/lang/`. `tisvlay-tray` ships English literals only — menu labels (`Open settings`, `Toggle backlight`, `Sleep`, …), tooltips, profile-submenu section headers. That gap means a French desktop with a localized GUI still gets an English tray menu sitting next to it, which reads as half-translated.

In parallel, "pure-Rust i18n" backends — Project Fluent via `fluent-rs` / `i18n-embed`, or ICU MessageFormat via the `icu` crates — are appealing on paper (no C runtime, modern feature set, gendered/variant terms, asymmetric translations). The blocker is Slint: `@tr("…")` reads gettext `.mo` catalogs and there is no alternative backend today. Splitting Rust-side and Slint-side to two backends doubles tooling and breaks the "translator catalog is source of truth" property.

This plan does the concrete tray expansion now and pins the upstream signal that would let us revisit the backend later.

## What's decided

1. **Expand gettext to `tisvlay-tray`** as part of this branch. Mechanics mirror `tisvlay-gui` so translators see a consistent layout across crates:
   - Add `gettext-rs` to `crates/tisvlay-tray/Cargo.toml`.
   - Wrap user-facing literals in `gettextrs::gettext("…")` — menu labels, tooltips, profile-submenu headers in `crates/tisvlay-tray/src/tray.rs`.
   - Add `crates/tisvlay-tray/lang/` mirroring the GUI directory layout.
   - Extend the `justfile` `i18n-extract` / `i18n-update` recipes to also drive the tray crate.
   - Initialize gettext in `main.rs` against the tray-specific text domain (`tisvlay-tray`).
   - Add a `build.rs` that compiles `.po` → `.mo` at build time, parallel to the GUI crate's `build.rs`.

2. **Per-crate `.po` domains.** Each crate ships its own translation domain (`tisvlay-gui`, `tisvlay-tray`, future per-vendor domains like `tisvlay-lg`). Matches GNOME / KDE precedent, gives translators coherent scope, prevents msgid collisions across modules with overlapping vocabulary.

3. **Stay on gettext as the backend.** Migrating Rust-side strings to Fluent today would force one of: (a) wait for Slint Fluent support; (b) maintain two parallel translation pipelines (`.po` + `.ftl`); or (c) push translated strings into Slint from Rust, which loses the `@tr` ergonomic and breaks the catalog-as-source-of-truth pattern. None of those is worth the lift until Slint moves.

4. **Track [slint#7949](https://github.com/slint-ui/slint/issues/7949) as the migration trigger.** *"Add support for translating text in Rust code in combination with translation bundling, without gettext as run-time library"* — open since March 2025, last activity June 2025. If Slint adds a way to translate Rust strings via the bundled-translations path (which compiles `.po` data at compile time instead of loading `.mo` at runtime), we can drop the `gettext-rs` runtime dependency without changing the catalog tooling. Slint shipped most of the bundled-translation infrastructure in 1.14 (issues #6793, #6804, #7022, #7154 — all closed late 2024); #7949 is the residual gap on the Rust-side API. Activity has slowed; subscribe so notifications fire if it picks up.

   Supporting upstream signals (worth watching but not gating):
   - [slint#9001](https://github.com/slint-ui/slint/issues/9001) — compile-time error for missing bundled translation (open, July 2025).
   - [slint#11248](https://github.com/slint-ui/slint/issues/11248) — `keys` translation/localization (open, April 2026).

5. **Project Fluent stays on the deferred shelf.** Even if #7949 lands, Fluent migration is a separate decision — would require parallel `.po` and `.ftl` catalogs during transition, plus translator-onboarding cost. Defer until either the residual gettext C dep becomes a packaging problem or a translator files a real plural/variant need that `ngettext` can't cover.

## What's not in scope here

- **Vendor-crate i18n** (`tisvlay-lg`, future `tisvlay-samsung` / `tisvlay-sony`). User-facing strings in vendor crates are pairing prompts and schema labels — small surface, will get their own domain when a translation request lands.
- **CLI i18n.** `tisvlay-cli` surfaces minimal user-facing text; the bulk is machine-readable. Not worth the domain overhead today.
- **Plural / variant handling.** No plural strings exist today. If one appears, use `ngettext` rather than reaching for Fluent.

## Tray migration steps

Ordered tasks for this branch:

1. Add `gettext-rs` to `crates/tisvlay-tray/Cargo.toml` (pin to the same version `tisvlay-gui` uses).
2. Initialize gettext in `crates/tisvlay-tray/src/main.rs` near the existing logging setup — `bindtextdomain` + `bind_textdomain_codeset` + `textdomain` against the `"tisvlay-tray"` domain.
3. Sweep `crates/tisvlay-tray/src/tray.rs` for user-facing literals and wrap each in `gettextrs::gettext("…")`. Targets include the static menu (`Open settings`, `Reconnect`, `Sleep`, `Wake`, `Toggle backlight`, …), tooltips, and the dynamic profile-submenu section headers.
4. Add `crates/tisvlay-tray/lang/` with a `.gitignore` for build artifacts (mirror the GUI crate's).
5. Add `crates/tisvlay-tray/build.rs` that compiles each `lang/<locale>/LC_MESSAGES/tisvlay-tray.po` → `.mo` at build time (lift the pattern from `crates/tisvlay-gui/build.rs`).
6. Extend the `justfile`:
   - `i18n-extract` runs `xgettext` against the tray crate's `src/` and writes `crates/tisvlay-tray/lang/tisvlay-tray.pot`.
   - `i18n-update` runs `msgmerge` for the tray crate alongside the GUI.
7. Commit the freshly-generated `tisvlay-tray.pot` so translators have a starting template. No locale-specific `.po` files in this branch — English baseline is the `msgid` literal itself; missing `.mo` falls back to the source string per gettext convention.
8. Update the i18n section of `.claude/CLAUDE.md` so the tray is listed alongside the GUI as i18n-ready.

## Migration scope when slint#7949 lands

If / when Slint adds a non-gettext Rust-side translation API:

1. Audit whether the `gettext-rs` runtime dep can be dropped entirely. Slint's bundled-translation path already covers Slint-side strings; #7949's resolution would cover Rust-side strings — same `.po` / `.mo` source files, no C runtime.
2. Decide separately whether to migrate the catalog format to Fluent. The two moves are independent: dropping the C dep is cheap and obviously valuable; Fluent migration is +medium effort with translator-onboarding cost.
3. Update `.claude/CLAUDE.md` and any `lang/README.md` files so the workflow docs match the new tooling.

## Migration path on durable docs after this plan ships

Per `docs/plans/README.md` convention, when this work merges:

- The tray-specific mechanics (per-crate domains, build.rs pattern) fold into the i18n section of `.claude/CLAUDE.md` as a "how to add i18n to a new crate" recipe.
- The Slint upstream watch list (issues #7949 / #9001 / #11248) migrates to `docs/ideas/slint-pure-rust-i18n.md` with an index entry in `docs/ideas.md`.
- This plan file is deleted; git history is the archive.

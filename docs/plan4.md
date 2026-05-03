# Plan 4 — KDE HIG cursor compliance + `UriBtn` rename + config-path "open in file manager" buttons

Bring the project's cursor language into compliance with KDE's [Human Interface Guidelines](https://develop.kde.org/hig/getting_input/) ("Only use the pointing finger cursor to signal interactivity for a clickable URL"), rename the existing `Btn` to `UriBtn` so the URL-opening intent is explicit at the call site, and add a small "open in file manager" icon button next to each peer's probed `ConfigPath` in the About-pane COMPONENTS table so a user who wants to edit the config externally can jump straight to the right folder.

## Why now

- Plan 3 surfaced a pre-existing project-wide deviation: every custom button widget (`DangerBtn`, `IconBtn`, `CopyBtn`, `Btn`) sets `mouse-cursor: pointer`, but KDE HIG says the pointer cursor is for clickable URLs *only*. Sidebar nav items (`SidebarItem`) already follow HIG; action buttons don't. The split is visible side-by-side in the sidebar today (Reload uses pointer; the nav rows above don't).
- The config-path text now flows from each peer's `ConfigPath` D-Bus property, so the user can *see* where each process reads config from — but there's no fast affordance for "let me edit that file." Adding a one-click "open in file manager" jump completes the loop.
- Both changes are visual-language hygiene; bundling them avoids two passes through the same call sites.

## Sources

- KDE HIG cursor rule: [`develop-kde-org/content/hig/getting_input.md:116`](https://develop.kde.org/hig/getting_input/) — "Only use the pointing finger cursor to signal interactivity for a clickable URL." Also line 117: "Only use underlined web-style links for clickable URLs, never for internal navigation within your app."
- Slint upstream alignment: std-widgets `Button` deliberately doesn't set pointer cursor in any of its five style implementations (Fluent / Material / Cupertino / Cosmic / Qt). Dropping pointer from our custom action widgets aligns us with both KDE HIG and Slint upstream defaults.
- Freedesktop spec for "show in file manager": [`org.freedesktop.FileManager1.ShowItems`](https://www.freedesktop.org/wiki/Specifications/file-manager-interface/) — implemented by Dolphin, Nautilus, Thunar, and most modern Linux file managers. Takes a list of URIs and opens the user's default manager with the items selected (not just the folder open).

## Cursor audit

| Surface | Cursor today | HIG says | Action |
|---|---|---|---|
| About brand area (opens repo URL) | pointer | pointer ✓ | keep |
| Version chip (interactive: opens release URL) | pointer | pointer ✓ | keep |
| `Btn` → `UriBtn` (Report issue, opens URL) | pointer | pointer ✓ | keep, rename |
| `AboutSlint` widget (opens slint.dev URL) | pointer (upstream) | pointer ✓ | keep |
| `Chip { interactive: true }` (currently URL-only) | pointer | pointer ✓ | keep |
| `SidebarItem` (nav) | default | default ✓ | already correct |
| `DangerBtn` (action) | pointer | default | **drop** |
| `IconBtn` (edit/save/cancel triplet, copy diagnostic) | pointer | default | **drop** |
| `CopyBtn` (clipboard write) | pointer | default | **drop** |
| Reload row (action) | pointer | default | **drop** |
| Remove TV button (DangerBtn) | pointer | default | **drop via DangerBtn** |
| Power-on / power-off / set-volume / set-backlight (std-widgets, fine already) | default | default ✓ | already correct |

## Decisions

1. **Rename `Btn` → `UriBtn`** in [`crates/tisvlay-gui/ui/widgets.slint`](../crates/tisvlay-gui/ui/widgets.slint). Makes the contract explicit: "this button is for opening a URL, hence the pointer cursor." Single import + call-site update in `about_pane.slint`.
2. **Drop `mouse-cursor: pointer`** from `DangerBtn`, `IconBtn`, `CopyBtn`, and any inline TouchAreas tied to non-URL actions (Reload row in `main.slint`). Default arrow takes over.
3. **Open-in-file-manager button**: small `IconBtn` (24px ghost, default cursor per HIG) next to each non-empty `ConfigPath` in the COMPONENTS table. Icon: `document-open-folder` from the freedesktop theme (Breeze / Adwaita both have it), with `folder-open` and `system-file-manager` as fallbacks.
4. **File-manager invocation**: D-Bus call to `org.freedesktop.FileManager1.ShowItems(URIs, StartupId)` on the user's session bus. Falls back to `webbrowser::open("file:///path/to/dir")` (which dispatches to xdg-open) if the FileManager1 service isn't reachable. The `ShowItems` path is preferred because it highlights the specific config file inside the folder rather than just opening the folder bare.
5. **Where the button lives in the row**: directly to the right of the `ConfigPath` text within the description column, before the version chip. Only renders when `entry.config_path != ""` — the CLI / GUI / pre-phase-2-tray rows don't get one.
6. **Single button per row, not per binary**: `tisvlay` (CLI) has no D-Bus surface and no probed config; the GUI's own config is empty (deferred); the tray's is empty (deferred). Only `tisvlayd` and `tisvlay-session` rows render the open-folder button after this plan lands.
7. **Tooltip-equivalent**: per-icon `accessible-label: @tr("Open config folder")`. Slint doesn't ship hover tooltips natively — the accessible label covers screen readers; the freedesktop convention is mostly self-explanatory glyph + folder.

## Phasing

Single commit if scope stays tight. Reasonable to split into two if review feedback runs long: Commit A = HIG cursor pass + `UriBtn` rename, Commit B = open-in-file-manager buttons. The HIG pass is mechanical; the open-folder buttons add a new D-Bus dependency (FileManager1) and a new icon load.

### Commit A — HIG cursor compliance + `UriBtn` rename

Touches:
- **[`crates/tisvlay-gui/ui/widgets.slint`](../crates/tisvlay-gui/ui/widgets.slint)**:
  - Drop `mouse-cursor: pointer` from `DangerBtn` (line 118), `IconBtn` (line 720 area), `CopyBtn` (line 660 area).
  - Rename `Btn` → `UriBtn` (no other change). The doc comment already explains it's for hyperlink-style buttons; tighten the language to KDE HIG terms ("URL openers only — do not use for in-app actions").
- **[`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint)**:
  - Update the import to bring in `UriBtn` instead of `Btn`.
  - Rename the call sites (currently just `Report issue`).
- **[`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint)**:
  - Drop `mouse-cursor: pointer` from the Reload row's TouchArea (line 233 area). Default arrow.
- Any other inline TouchAreas tied to actions (do a `grep -n 'mouse-cursor: pointer' crates/tisvlay-gui/ui/` sweep). Per-call review: keep only the ones that fire URL-open callbacks; drop the rest.

**Smoke test**: hover the Reload row → arrow cursor (was hand). Hover Remove TV → arrow. Hover the brand area in About → still hand. Hover the version chip → still hand. `just check` passes.

### Commit B — Config-path "open in file manager" icon button

Touches:
- **`crates/tisvlay-gui/src/`**:
  - New `file_manager.rs` module: helper that takes a `&Path` (the config file) and tries to open the user's file manager via `org.freedesktop.FileManager1.ShowItems` over the session bus. On failure (service not registered, D-Bus error), falls back to `webbrowser::open("file:///<parent_dir>")` so something happens regardless. Logs a warn on both fallback and total failure.
  - `main.rs`: add the icon load for `document-open-folder` / `folder-open` / `system-file-manager` and stash it on the `Icons` Slint global (or a similar slot).
- **`crates/tisvlay-gui/ui/components/about_pane.slint`**:
  - Extend `AboutComponentRow` with a `config_path` field (currently the description string already encodes it, but to drive the click we need the path on its own). Or: keep the structured description, parse the path back out on click. **First option is cleaner.**
  - In `ComponentRow`, when `entry.config_path != ""`, render a small `IconBtn` (default cursor per HIG, source = `Icons.open-folder`) between the description and the version chip. Click fires a new `open-config-folder(string)` callback up to the AppWindow with the config-path payload.
- **`crates/tisvlay-gui/ui/main.slint`**:
  - New `callback open-config-folder(string);` on AppWindow, forwarded to AboutPane.
- **`crates/tisvlay-gui/src/main.rs`**:
  - `app.on_open_config_folder(...)` invokes the new `file_manager::show_in_file_manager(&PathBuf::from(path))`.
  - `make_about_components` extends each row with `config_path` so the Slint side can pass it through to the callback.

**Smoke test**: in the About pane, daemon and session rows now render a small folder-icon button between their config-path text and the v0.1.0 chip. Click → file manager opens with the config file selected. Cursor on the icon button is the default arrow (HIG compliance).

## Open questions

1. **Icon choice for the open-folder button**: `document-open-folder` (Breeze name for "open the folder containing this") vs `folder-open` (generic open-folder glyph) vs `system-file-manager` (the file-manager app icon). My read: `document-open-folder` is the most semantically precise — "open the folder this document lives in" — and is the icon Dolphin uses for its own "open containing folder" action. Want me to default to that?
2. **Open-folder button on the GUI row**: when `Gui1.ConfigPath` becomes non-empty (M5 polish lands window-state persistence), should the GUI row get the same button? My default is yes — the affordance is uniform across peers; if the path exists, the button appears. No special-casing.
3. **CLI row**: `tisvlay` will never have a D-Bus surface. Stays without a button. Fine?
4. **Plan-doc step**: does plan4.md scope match what you had in mind, or want me to add/remove anything before we commit it?

## Out of scope

- **Editing the config from inside the GUI**: this plan only opens the file manager. An in-GUI editor for daemon/session config is a much larger feature (validation, atomic-write, schema-driven form, conflict resolution against running services). Deferred indefinitely.
- **Per-row "view logs" / "restart service" actions**: tempting symmetry but out of scope. Filed as future polish if the user asks.
- **HIG underline-on-hover for URL openers**: the version chip and brand-area title already have hover affordances (chip recolor, title brightens to accent). No additional underline treatment needed; KDE HIG only mandates underline for *clickable URLs in body text*, which we don't have.

## Done when

- `grep -rn 'mouse-cursor: pointer' crates/tisvlay-gui/ui/` returns only URL-opener call sites (UriBtn / brand area / Chip{interactive} / AboutSlint upstream / version chip).
- `Btn` is renamed to `UriBtn` everywhere; no `Btn` references remain in `widgets.slint` or `about_pane.slint`.
- Hovering Reload, Remove TV, the diagnostic-copy IconBtn, every CopyBtn in the TVs-pane INFO group, and the edit/save/cancel triplet all show the **default arrow**.
- Hovering the brand area, version chip, Report issue button, and AboutSlint badge all show the **pointer**.
- About pane COMPONENTS rows for daemon and session render a small folder-open IconBtn between the description and the version chip; click opens the file manager with the config file selected. Cursor on that button is **default arrow** (per HIG, this is an action, not a URL).
- `just check` passes.
- Plan 4 entry added to [`docs/plan.md`](plan.md)'s "Beyond the original roadmap" table once the work lands.

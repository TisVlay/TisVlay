# Plan 4 — KDE HIG cursor compliance + `UriBtn` rename + About-pane peer-table consolidation

Bring the project's cursor language into compliance with KDE's [Human Interface Guidelines](https://develop.kde.org/hig/getting_input/) ("Only use the pointing finger cursor to signal interactivity for a clickable URL"), rename the existing `Btn` to `UriBtn` so the URL-opening intent is explicit at the call site, fold the About-pane CONNECTIONS group into a richer COMPONENTS table (each peer appears once, with stacked status + version pills and an open-in-file-manager button next to its config path), and probe the CLI's version by shelling out to `tisvlay --version` so it joins the table as a first-class row.

## Why now

- Plan 3 surfaced a pre-existing project-wide deviation: every custom button widget (`DangerBtn`, `IconBtn`, `CopyBtn`, `Btn`) sets `mouse-cursor: pointer`, but KDE HIG says the pointer cursor is for clickable URLs *only*. Sidebar nav items (`SidebarItem`) already follow HIG; action buttons don't. The split is visible side-by-side in the sidebar today (Reload uses pointer; the nav rows above don't).
- The config-path text now flows from each peer's `ConfigPath` D-Bus property, so the user can *see* where each process reads config from — but there's no fast affordance for "let me edit that file." Adding a one-click "open in file manager" jump completes the loop.
- Plan 3 also exposed a structural redundancy: every peer is named twice in the About pane — once in CONNECTIONS (with bus address + verbose description + online chip) and once in COMPONENTS (with config path + version chip). Consolidating into a single per-peer table is denser, less repetitive, and lets the open-folder button live next to the path it acts on without forcing the eye to jump groups.
- The CLI is the only binary still without a version chip — purely because it has no D-Bus surface. A one-shot `tisvlay --version` shell-out closes that gap so all five binaries appear uniformly in the table.
- All four changes touch the same Slint surface; bundling avoids successive passes through the same call sites.

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
3. **Drop the CONNECTIONS group entirely** from `about_pane.slint`. The COMPONENTS table absorbs each peer's online/offline status as a stacked pill alongside the version pill. Bus addresses (`org.tisvlay.Daemon` etc.) and verbose CONNECTIONS descriptions ("Vendor-agnostic TV management, WoL at boot, per-TV D-Bus objects.") are dropped — the row name + concise type + config path is enough self-identification, and a power user who wants the bus name has `busctl --user list | grep tisvlay`.
4. **Stacked pills column** on the right of each row: status chip on top (online/offline tone), version chip below (info/warn/danger drift tone). Rows without a status (CLI, GUI — no D-Bus surface to probe) render only the version chip; rows without a version (pre-phase-2 fallback for any peer we can't reach) render no chip. Vertical alignment of the chip column stays consistent across rows.
5. **Open-in-file-manager button**: small `IconBtn` (24px ghost, default cursor per HIG) immediately to the right of the row's name (or as a leading icon next to it), only rendered when `entry.config_path != ""`. Icon: `document-open-folder` from the freedesktop theme (Breeze / Adwaita both have it), with `folder-open` and `system-file-manager` as fallbacks.
6. **File-manager invocation**: D-Bus call to `org.freedesktop.FileManager1.ShowItems(URIs, StartupId)` on the user's session bus. Falls back to `webbrowser::open("file:///path/to/dir")` (which dispatches to xdg-open) if the FileManager1 service isn't reachable. The `ShowItems` path is preferred because it highlights the specific config file inside the folder rather than just opening the folder bare.
7. **CLI version probing**: `tokio::process::Command::new("tisvlay").arg("--version").output().await` once on About-pane open, parse stdout (clap's default `--version` output is `tisvlay 0.1.0\n`). Failure → no version chip on the CLI row. Cached after the first probe; refreshed on the same trigger as the other peers' refreshes (cheap, no need to optimise).
8. **CLI row in the table**: name "tisvlay", description "command-line interface", no online pill (no D-Bus surface), version pill from the shell-out, no open-folder button (no config file). Symmetric with how the GUI row also lacks the online pill (we don't probe ourselves).
9. **Tooltip-equivalent**: per-icon `accessible-label: @tr("Open config folder")`. Slint doesn't ship hover tooltips natively — the accessible label covers screen readers; the freedesktop convention is mostly self-explanatory glyph + folder.
10. **Footer dots stay**: the sidebar's bottom-of-page Daemon / Session / Tray dots remain unchanged. Different surface — they're the at-a-glance "is the stack alive" indicator visible from any pane; the About-pane peer table is the detailed view. Two complementary affordances, not duplication.

## Phasing

Three commits, each with a coherent reason and a reviewable chunk that stands alone visually.

### Commit A — HIG cursor compliance + `UriBtn` rename

Mechanical pass. No new behaviour, no new dependencies, no UI structure changes.

Touches:
- **[`crates/tisvlay-gui/ui/widgets.slint`](../crates/tisvlay-gui/ui/widgets.slint)**:
  - Drop `mouse-cursor: pointer` from `DangerBtn` (line 118), `IconBtn` (line 720 area), `CopyBtn` (line 660 area).
  - Rename `Btn` → `UriBtn` (no other change). The doc comment already explains it's for hyperlink-style buttons; tighten the language to KDE HIG terms ("URL openers only — do not use for in-app actions").
- **[`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint)**: update the `Btn` → `UriBtn` import + call sites (currently just `Report issue`).
- **[`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint)**: drop `mouse-cursor: pointer` from the Reload row's TouchArea (line 233 area).
- Sweep: `grep -n 'mouse-cursor: pointer' crates/tisvlay-gui/ui/` and per-call-site review — keep only the ones tied to URL-open callbacks; drop the rest.

**Smoke test**: hover Reload, Remove TV, the diagnostic-copy IconBtn, every CopyBtn in the TVs-pane INFO group, the edit/save/cancel triplet → **default arrow**. Hover the brand area, version chip, Report issue button, AboutSlint badge → **pointer**. `just check` passes.

### Commit B — Drop CONNECTIONS, merge into COMPONENTS, stacked pills, CLI version probe

The structural About-pane restructure. Single visual jump from "two redundant groups" to "one richer table." Doesn't add D-Bus deps yet (open-folder button comes in Commit C); the CLI shell-out is a tiny cross-cutting addition that fits naturally here because it populates the same table.

Touches:
- **[`crates/tisvlay-gui/src/main.rs`](../crates/tisvlay-gui/src/main.rs)**:
  - New CLI version probe: `async fn probe_cli_version() -> Option<String>` that runs `tokio::process::Command::new("tisvlay").arg("--version").output().await` and parses stdout. Returns `None` on any failure (binary not on PATH, exit non-zero, unparseable output).
  - `refresh_about_components` calls it alongside the existing daemon/session/tray probes.
  - `AboutComponentRow` (Slint struct) gains an `online_known: bool` + `online: bool` pair so each row can render or skip the status pill independently of the version pill. CLI/GUI rows have `online_known: false` (no D-Bus surface to probe themselves); daemon/session/tray have `online_known: true` and the `online` value reflects current availability.
  - `make_about_components` populates online status from the same source the existing footer dots use (`daemon_available` etc.).
- **[`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint)**:
  - Delete the `ConnectionRow` component and the `CONNECTIONS` Group invocation entirely.
  - `ComponentRow` gains a stacked-pills column: `VerticalLayout { spacing: 4px; if entry.online_known: Chip { tone, label }; if entry.version != "": Chip { ... } }`. Status pill on top, version pill below; vertical alignment of the column stays consistent across rows.
  - Drop the `tray-available`, `daemon-available`, `session-available` properties from `AboutPane` if they're no longer referenced (rows now read status from the `entry` struct).

**Smoke test**: About pane has one COMPONENTS group with five rows; CONNECTIONS is gone. Every row has the same right-edge alignment (consolidated chip column lines up at x_right=682). Daemon / session / tray rows show online + version pills stacked. CLI row shows only a version pill (sourced from `tisvlay --version`). GUI row shows only the version pill. Killing the daemon flips daemon's online pill to "offline / danger" without touching the version pill (until the version is no longer reachable, then the version drops too).

### Commit C — Config-path "open in file manager" icon button

Adds the FileManager1 D-Bus call + new icon. Independently reviewable from the structural restructure in B.

Touches:
- **`crates/tisvlay-gui/src/file_manager.rs`** (new): helper that takes a `&Path` (the config file) and tries `org.freedesktop.FileManager1.ShowItems(URIs, StartupId)` over the session bus. On failure, falls back to `webbrowser::open("file:///<parent_dir>")` so something happens regardless. Warn-log on both fallback and total failure.
- **[`crates/tisvlay-gui/src/main.rs`](../crates/tisvlay-gui/src/main.rs)**: load `document-open-folder` (with `folder-open` + `system-file-manager` fallbacks) onto the `Icons` Slint global. Wire the new `app.on_open_config_folder(...)` callback to the helper.
- **[`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint)**: in `ComponentRow`, render an `IconBtn` (default cursor per HIG; placement: leading icon next to the row's name, or trailing the description — to be decided during implementation based on visual balance) when `entry.config_path != ""`. Click fires `open-config-folder(entry.config_path)`.
- **[`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint)**: new `callback open-config-folder(string);` on AppWindow, forwarded to AboutPane.

**Smoke test**: daemon and session rows render a small folder-icon button next to the row name. Click → file manager opens with the config file selected. Cursor on the icon is **default arrow** (per HIG, this is an action, not a URL). Tray and GUI rows have no folder button (config_path empty). CLI row has no folder button (no config file).

## Resolved during planning

- **Icon for the open-folder button**: `document-open-folder` (Breeze; with `folder-open` and `system-file-manager` as fallbacks). Most semantically precise — "open the folder this document lives in" — and matches Dolphin's own "open containing folder" affordance.
- **Editor-vs-file-manager**: file manager. Freedesktop has no default-text-editor convention; `xdg-open <file.toml>` relies on MIME associations that are usually unset for TOML. Opening the folder lets the user pick whatever editor they prefer.
- **CLI in the table**: yes, with a version probe via `tisvlay --version`. No D-Bus host on the CLI is silly for a one-shot client; shelling out is the right shape.
- **GUI row gets a folder button when its `ConfigPath` becomes non-empty** (M5 polish or whenever window-state persistence lands). No special-casing — the button's render condition is simply `entry.config_path != ""`.

## Out of scope

- **Editing the config from inside the GUI**: this plan only opens the file manager. An in-GUI editor for daemon/session config is a much larger feature (validation, atomic-write, schema-driven form, conflict resolution against running services). Deferred indefinitely.
- **Per-row "view logs" / "restart service" actions**: tempting symmetry but out of scope. Filed as future polish if the user asks.
- **HIG underline-on-hover for URL openers**: the version chip and brand-area title already have hover affordances (chip recolor, title brightens to accent). No additional underline treatment needed; KDE HIG only mandates underline for *clickable URLs in body text*, which we don't have.
- **Tray's "is offline" semantics deeper than name-presence**: today `Tray1` only exposes `Version` + `ConfigPath`; "online" means "the bus name is registered." If the tray ever exposes a richer health surface (idle / spinning / errored), the row chip can carry a richer tone — but for now it's binary online/offline, same as daemon and session.

## Done when

- `grep -rn 'mouse-cursor: pointer' crates/tisvlay-gui/ui/` returns only URL-opener call sites (UriBtn / brand area / Chip{interactive} / AboutSlint upstream).
- `Btn` is renamed to `UriBtn` everywhere; no `Btn` references remain in `widgets.slint` or `about_pane.slint`.
- Hovering Reload, Remove TV, the diagnostic-copy IconBtn, every CopyBtn in the TVs-pane INFO group, and the edit/save/cancel triplet all show the **default arrow**.
- Hovering the brand area, version chip, Report issue button, and AboutSlint badge all show the **pointer**.
- About pane has **one** group (COMPONENTS); CONNECTIONS is gone. Five rows: tisvlayd, tisvlay-session, tisvlay, tisvlay-tray, tisvlay-gui.
- Stacked-pills column on the right: status pill on top (online/offline) for daemon / session / tray; version pill below for whichever rows the version is reachable. CLI row has only a version pill (from `tisvlay --version`); GUI row has only a version pill (from compile-time `CARGO_PKG_VERSION`).
- Folder-open `IconBtn` next to the row name on every row whose `entry.config_path != ""` (today: daemon + session). Click opens the file manager with the config file selected via `FileManager1.ShowItems`. Cursor on that button is **default arrow** (per HIG).
- Killing the daemon flips its row's online pill to "offline / danger" tone; killing the tray does the same to its row. Footer dots in the sidebar mirror the same state (unchanged from Plan 3).
- `just check` passes.
- Plan 4 entry added to [`docs/plan.md`](plan.md)'s "Beyond the original roadmap" table once the work lands.

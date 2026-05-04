# Plan 2 — About pane parity + FOSS attribution

**Status: Commit A shipped + two follow-up plans (former `plan3.md`, `plan4.md`) absorbed below; Commit B redesigned to put third-party notices in their own pane.** Commits A through C of the absorbed work landed on `feat/gui-about` (peer-introspection symmetry, KDE HIG cursor compliance, COMPONENTS-table consolidation, file-manager folder buttons, source-side `absolutize`). Commit B (FOSS attribution + Notices pane) is the remaining work.

Bring the `tisvlay-gui` About pane in line with the design handoff, satisfy the Slint license requirement we're currently violating, and stand up a proper third-party-attribution pipeline so the redistributable `.deb`/`.rpm`/`.pkg.tar.zst` packages cite every FOSS dependency the way a proprietary build would.

## Why now

- The handoff describes an About pane the implementation only partly meets.
- Slint's Royalty-free 2.0 license **requires** the `AboutSlint` widget to appear in our About pane. We're not currently shipping it, which is a license-compliance gap regardless of redistribution.
- This work is independently valuable as a practice run for proper FOSS-attribution conventions even though `tisvlay` itself is Apache-2.0. The patterns transfer cleanly to any future commercial work built on the same dep graph.

## Sources

- Visual reference: [`.claude/ui/design_handoff_tisvlay_gui/screenshots/about-light.png`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/about-light.png), [`about-dark.png`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/about-dark.png).
- Component shape: [`.claude/ui/design_handoff_tisvlay_gui/other-panes.jsx`](../.claude/ui/design_handoff_tisvlay_gui/other-panes.jsx) `AboutPane` (lines 310–372).
- Attribution research summary lives in this plan; primary sources are [`cargo-about` docs](https://embarkstudios.github.io/cargo-about/), [Apache 2.0 §4(d)](https://www.apache.org/licenses/LICENSE-2.0), and the [Slint Royalty-free 2.0 license](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md).

## Gap analysis

| Area | Current ([about_pane.slint](../crates/tisvlay-gui/ui/components/about_pane.slint)) | Design handoff | FOSS-attribution scope |
|---|---|---|---|
| Header | Title + tagline (vertical) | Logo + title + tagline + version chip (horizontal row) | — |
| Connections | Bullet + label + status text (one-liner) | Status icon + title + address + description + online chip | — |
| Components | (none) | 5-row table: binary name + path/description | — |
| License | Footer text: `Apache-2.0` + repo URL | `License` group: copyright line + `View source` / `Report issue` buttons | — |
| Slint attribution | (none) | (none) | **`AboutSlint` widget — license-mandated** |
| Third-party notices | (none) | (none) | New "Open Source Notices" section: scrollable embed of `THIRD-PARTY-LICENSES.txt` |

## Findings worth flagging

- **Slint Royalty-free 2.0** requires the `AboutSlint` widget in our About pane (and a Slint badge on the download page if/when we have a marketing site). Not optional. Slint provides this as a prebuilt widget — integration is a single import + placement.
- **Dep graph is fully permissive.** A `cargo metadata` survey of `Cargo.lock` (~658 crates) shows: 279 `MIT OR Apache-2.0`, 174 `MIT`, 54 `Apache-2.0 OR MIT`, 31 legacy `MIT/Apache-2.0`, 29 `Apache-2.0`, 22 `Unicode-3.0`, 18 `Apache-2.0 WITH LLVM-exception …`, plus a handful of BSD-2/3-Clause, ISC, Zlib, BSL-1.0, CDLA-Permissive-2.0, Unlicense. **No GPL/LGPL/MPL.** A single generated `THIRD-PARTY-LICENSES.txt` blob covers everything; no source-redistribution disclosure obligations.
- **Slint itself** carries `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`. Picking the Royalty-free option is what triggers the `AboutSlint` widget requirement above.

## Tooling decision

**[`cargo-about`](https://github.com/EmbarkStudios/cargo-about)** (Embark Studios). Reasons:

- Re-resolves SPDX expressions per crate using a configurable priority list (so `MIT OR Apache-2.0` deterministically picks one option).
- Fetches missing license texts via clearlydefined.io.
- Supports per-crate clarifications for non-standard packaging.
- Emits arbitrary formats via Handlebars — we generate **two** outputs from the same data: a plain-text variant for `include_str!` embedding, and an HTML variant for `/usr/share/doc/tisvlay/`.

Rejected alternatives: `cargo-license` (lists only, no text), `cargo-bundle-licenses` (YAML output still needs rendering), `cargo-deny` (license-policy gate, useful as a complement, not a replacement), `cargo-3pl` (unmaintained).

## Layout target

```
crates/tisvlay-gui/
├── about.toml                      ← license accept-list + priority for OR-clauses
├── about.txt.hbs                   ← Handlebars template for the embedded plain-text blob
├── about.html.hbs                  ← Handlebars template for /usr/share/doc/...
├── THIRD-PARTY-LICENSES.txt        ← generated; checked in (so PRs show license diffs)
├── THIRD-PARTY-LICENSES.html       ← generated; checked in
└── ui/components/about_pane.slint  ← updated to surface them
NOTICE                              ← curated top-level (Apache-2.0 propagation)
```

`include_str!("../THIRD-PARTY-LICENSES.txt")` puts the text in the binary; Slint renders via `TextEdit { read-only: true }` inside a `ScrollView`. The HTML variant ships in the `.deb`/`.rpm` at `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` so distro audit tools find it. Stale-attribution detection mirrors the gettext flow: `just licenses` regenerates, `just licenses-check` is a prek hook that fails on diff.

## Shipped extensions (former `plan3.md` + `plan4.md`)

After Commit A landed, two follow-up plans extended the same About-pane surface in ways that need recording so subsequent work — including Commit B below — builds on a coherent baseline. The plan documents themselves have been retired into git history; this section captures the architectural decisions worth carrying forward. Implementation deltas relative to those plans are also folded in.

### Symmetric peer introspection (was: `plan3.md`)

- All four long-running peers — Daemon, Session, Tray, GUI — expose the same minimal D-Bus surface: const `Version` + `ConfigPath` properties on `Manager1` / `Session1` / `Tray1` / `Gui1`. Bus names: `org.tisvlay.{Daemon,Session,Tray,Gui}`.
- Matching client types in [`crates/tisvlay-core/src/dbus_client.rs`](../crates/tisvlay-core/src/dbus_client.rs); `StateWatcher` emits `TrayAppeared` / `TrayDisappeared` events.
- The sidebar footer carries three peer dots (Daemon / Session / Tray); the GUI is implicit (the calling process).
- The CLI is a one-shot client and stays out of the D-Bus surface; its version is probed by shelling out to `tisvlay --version`.

### About-pane consolidation + KDE HIG (was: `plan4.md`)

- Cursor language: `mouse-cursor: pointer` is now reserved exclusively for URL openers per [KDE HIG](https://develop.kde.org/hig/getting_input/). Action widgets (`DangerBtn`, `IconBtn`, `CopyBtn`, sidebar Reload) use the default arrow.
- `Btn` was renamed to `UriBtn` to make the URL-opener contract explicit at the call site.
- The originally-separate CONNECTIONS and COMPONENTS groups were folded into one five-row COMPONENTS table — Daemon / Session / Tray / GUI / CLI — with:
  - 28px ✓/✗ status circle on the left (green = surface answered, red = not),
  - friendly name + monospace bus address in parens (CLI has no parens, no D-Bus surface),
  - rich one-line description,
  - 1px hairline divider between rows,
  - stacked status + version chips on the right,
  - folder-open `IconBtn` next to the bus parens that fires `org.freedesktop.FileManager1.ShowItems` (with an `xdg-open` fallback on the parent directory) — helper at [`crates/tisvlay-gui/src/file_manager.rs`](../crates/tisvlay-gui/src/file_manager.rs). CLI omits the button; D-Bus peers without a config file render it disabled at 40% opacity.
- A single uniform `online` flag drives both the status circle and the presence chip; the CLI participates via the shell-out probe. The CLI row's presence chip is rendered with `opacity: 0` so its layout slot is preserved and version chips line up vertically across all rows.
- [`tisvlay_core::absolutize(PathBuf)`](../crates/tisvlay-core/src/config.rs) resolves any relative `--config` argument against the daemon/session CWD at startup so the `ConfigPath` D-Bus property is always absolute (consumers can't guess each peer's CWD otherwise). Called once in each binary's entry point.

## Phasing

Two commits, scoped per the "scaffolding + first usage in one commit" preference.

### Commit A — About pane visual fidelity + Slint license compliance + version-drift surface

**Goal**: Match the design handoff, stop violating the Slint license, and surface installed-version drift between the running GUI and the daemon/session it's talking to.

Touches:
- [`crates/tisvlayd/src/dbus/`](../crates/tisvlayd/src/dbus/) — add a read-only `Version` property on `Manager1` returning `env!("CARGO_PKG_VERSION")`.
- [`crates/tisvlay-session/src/session_dbus.rs`](../crates/tisvlay-session/src/session_dbus.rs) — same on `Session1`.
- [`crates/tisvlay-core/src/dbus_client.rs`](../crates/tisvlay-core/src/dbus_client.rs) — add `version()` accessors on `DaemonClient` + `SessionClient`.
- [`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint) — refactor to match the handoff:
  - Header row: logo + title + tagline (left), version chip (right; sourced from compile-time `CARGO_PKG_VERSION`).
  - CONNECTIONS group: replace `StatusLine` with a richer `ConnectionRow` (status icon + title + address + description + online chip, per [`other-panes.jsx:374`](../.claude/ui/design_handoff_tisvlay_gui/other-panes.jsx#L374)).
  - COMPONENTS group: data-driven 5-row table fed from a Rust array via a `Repeater`. Each row has a per-component version chip toned by comparison vs. the GUI's compile-time version — **info** if equal, **warn** if remote > local, **danger** if remote < local. Daemon and session rows pull their version over D-Bus; CLI and tray rows have no chip (no D-Bus surface to probe).
  - LICENSE group: copyright line + `View source` / `Report issue` buttons + **diagnostic copy-to-clipboard** button that emits the JSON blob `{ daemon_version, session_version, slint_version, gui_version }` (per [`docs/plan.md`](plan.md) M5) via `arboard` (already a dep).
  - **`AboutSlint` widget** at the bottom (Slint license compliance).
- `crates/tisvlay-gui/src/main.rs` — wire `CARGO_PKG_VERSION` through to Slint, fetch daemon/session versions on About-pane open, build the COMPONENTS row array, route the URL buttons through `webbrowser = "1"`, wire the diagnostic-copy callback.
- [`crates/tisvlay-gui/Cargo.toml`](../crates/tisvlay-gui/Cargo.toml) — add `webbrowser = "1"`.

**Visual smoke test**: launch `tisvlay-gui --user`, navigate to About, verify each section matches [`about-light.png`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/about-light.png) and `about-dark.png`. Sanity-check the version-drift logic by running an older `tisvlayd` binary against the new GUI (warn/danger chips render).

### Commit B — Third-party FOSS attribution pipeline + dedicated Notices pane

**Goal**: Generate, embed, and ship attribution data for all transitive deps. Surface it through a dedicated `Notices` pane reachable from a button in the About pane's LICENSE group, so the About pane itself doesn't grow a ~658-crate scrollable text dump alongside its now-dense peer table.

#### Pane navigation model

`Notices` is a "modal-ish" pane: it has an entry in the Slint `Pane` enum but no sidebar item. Reachable only via a `View third-party notices` button in the About pane's LICENSE group; the Notices pane carries its own header with a `← Back` button that sets `active-pane = Pane.about`. This keeps the sidebar uncluttered (it's reference material, not navigation) while still using the existing pane-switching plumbing — no overlay/popup framework needed. Sidebar nav remains visible during the Notices pane (so the user can still jump elsewhere); the back button is the canonical return path.

#### Touches

- [`flake.nix`](../flake.nix) — add `cargo-about` to the dev shell.
- `crates/tisvlay-gui/about.toml` — accept-list of allowed SPDX ids; priority order for `OR` expressions; clarifications for any non-standard crates.
- `crates/tisvlay-gui/about.txt.hbs` + `about.html.hbs` — Handlebars templates.
- `crates/tisvlay-gui/THIRD-PARTY-LICENSES.{txt,html}` — generated; checked in.
- `NOTICE` (top-level) — curated, lists only crates whose upstream `NOTICE` requires propagation (the Apache-2.0 ones surfaced by `cargo-about`).
- [`justfile`](../justfile) — `licenses` (regenerate) + `licenses-check` (CI-friendly diff check) recipes.
- [`prek.toml`](../prek.toml) — new hook calling `licenses-check` (priority 0, file-check tier).
- [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml) `[package.metadata.nfpm]` — add `THIRD-PARTY-LICENSES.html` → `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` to `contents`.
- `crates/tisvlay-gui/src/main.rs` — `const NOTICES: &str = include_str!("../THIRD-PARTY-LICENSES.txt");`; push as a Slint property.
- **New**: `crates/tisvlay-gui/ui/components/notices_pane.slint` — the dedicated pane. Header row with title + `← Back` button (calls `back-clicked`); body is a `ScrollView { TextEdit { read-only: true; text: root.notices; } }`.
- [`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint) — extend the `Pane` enum with `notices`; instantiate `NoticesPane` gated on `active-pane == Pane.notices`; wire `back-clicked => { root.active-pane = Pane.about; }`.
- [`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint) — add a `View third-party notices` button to the LICENSE group (a `UriBtn` is wrong here — it's not a URL opener; use a fresh in-app action button or extend an existing widget). Emits a `view-notices-clicked` callback that AppWindow translates into `active-pane = Pane.notices`.
- [`packaging/README.md`](../packaging/README.md) — short section on the attribution flow + how it's regenerated + where the HTML variant lives in the package.

#### Smoke tests

- `just licenses` regenerates without diff on a clean tree.
- `just package-rpm` produces a package whose `rpm -qpl` lists `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`.
- About pane LICENSE group shows the new `View third-party notices` button. Click → Notices pane displays the embedded text in a scrollable view. Back button → returns to About pane (sidebar `active-pane` flips to `tvs` if the user clicks a sidebar entry mid-flow; otherwise stays on About).

## Decisions

1. **Version chips**: compile-time `env!("CARGO_PKG_VERSION")` for the GUI's overall version chip in the header row. Per-row chips in the COMPONENTS table for the daemon and session reflect their probed runtime versions (read via the new `Version` property on `Manager1` and `Session1`). Tone the chip by comparison vs. the GUI's compile-time version: **info** if equal, **warn** if remote > local, **danger** if remote < local. CLI and tray rows: no chip — they're not D-Bus services we have a connection to, and shelling out to `--version` is awkward and brittle. (The drift case that matters in practice is daemon/session-vs-GUI after a partial upgrade where services haven't been restarted.)
2. **URL-open routing**: `webbrowser = "1"`. Slightly more dep weight than a hand-rolled `xdg-open` invocation, but cross-platform-ready and we may want more of its features later.
3. **Components table data source**: data-driven from Rust (forced by decision 1 — we need to push per-row version + tone). Slint renders via a `Repeater` over a property-bound array.
4. **Notices presentation**: dedicated `Notices` pane (no sidebar entry), reachable only via a button in the About pane's LICENSE group; the pane has its own back button. Originally specced as a sub-section inside About; promoted to its own pane after Plan 4 substantially densified the About pane (peer table + status circles + chips + folder buttons + LICENSE group + AboutSlint badge — adding a ~658-crate scrollable text dump on top of that would overwhelm the page). Per-license expandable sections still deferred as a stretch UX goal.
5. **OR-clause license priority** in `about.toml`: MIT first, Apache-2.0 second, others by `cargo-about`'s default order. Industry norm; minimizes NOTICE-propagation work.
6. **Diagnostic copy-to-clipboard button**: included even though not in the handoff. Lives in the LICENSE group alongside `View source` / `Report issue`, copies the JSON blob `{ daemon_version, session_version, slint_version, gui_version }` (from [`docs/plan.md`](plan.md) M5) via `arboard` (already a dep).

## Out of scope

- Authors / Contributors list (the handoff doesn't show one; can be added later if/when we have meaningful contributors beyond solo dev).
- Per-vendor backend attribution (LG/Samsung/Sony when those land) — once we have multi-vendor builds, vendor backend crates may want their own license sub-sections. Cross-link from this plan when relevant.
- A sidebar entry for the Notices pane. The pane exists, but it's reachable only via the About pane button — Notices is reference material, not navigation.

## Done when

- About pane visually matches both light and dark screenshots in [`screenshots/`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/).
- The `AboutSlint` widget is rendered in the About pane (Slint license satisfied).
- `THIRD-PARTY-LICENSES.{txt,html}` are generated, checked in, and refresh on `just licenses`.
- `prek run -a` fails if `Cargo.lock` changes without a corresponding regenerate.
- The `.rpm` smoke-tested on Bazzite still installs and the GUI launches cleanly with the new pane visible.
- About pane LICENSE group has a `View third-party notices` button. Click → Notices pane shows the embedded text in a scrollable view with a `← Back` button that returns to About. No sidebar entry for Notices.
- [`docs/plan.md`](plan.md) M5 entry is updated to mark the About-pane work complete; the deferred items above are filed in [`docs/ideas.md`](ideas.md).

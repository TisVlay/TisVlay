# Plan 2 — About pane parity + FOSS attribution

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

## Phasing

Two commits, scoped per the "scaffolding + first usage in one commit" preference.

### Commit A — About pane visual fidelity + Slint license compliance

**Goal**: Match the design handoff and stop violating the Slint license. No build-system or dep-graph changes.

Touches:
- [`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint) — refactor to match the handoff:
  - Header row: logo + title + tagline (left), version chip (right)
  - CONNECTIONS group: replace `StatusLine` with a richer `ConnectionRow` (status icon + title + address + description + online chip, per [`other-panes.jsx:374`](../.claude/ui/design_handoff_tisvlay_gui/other-panes.jsx#L374))
  - COMPONENTS group: 5-row table (`tisvlayd`, `tisvlay-session`, `tisvlay`, `tisvlay-tray`, `tisvlay-gui`)
  - LICENSE group: copyright line + `View source` / `Report issue` buttons
  - **`AboutSlint` widget** at the bottom (Slint license compliance)
- `crates/tisvlay-gui/src/main.rs` — pass `CARGO_PKG_VERSION` into the GUI as a Slint property; wire button callbacks for the two URLs.
- `crates/tisvlay-gui/Cargo.toml` — possibly add `webbrowser` dep if we go that route over a hand-rolled `xdg-open` invocation (see open question 2).

**Visual smoke test**: launch `tisvlay-gui --user`, navigate to About, verify each section matches [`about-light.png`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/about-light.png) and `about-dark.png`.

### Commit B — Third-party FOSS attribution pipeline

**Goal**: Generate, embed, and ship attribution data for all transitive deps. Wire it into the About pane.

Touches:
- [`flake.nix`](../flake.nix) — add `cargo-about` to the dev shell.
- `crates/tisvlay-gui/about.toml` — accept-list of allowed SPDX ids; priority order for `OR` expressions; clarifications for any non-standard crates.
- `crates/tisvlay-gui/about.txt.hbs` + `about.html.hbs` — Handlebars templates.
- `crates/tisvlay-gui/THIRD-PARTY-LICENSES.{txt,html}` — generated; checked in.
- `NOTICE` (top-level) — curated, lists only crates whose upstream `NOTICE` requires propagation (the Apache-2.0 ones surfaced by `cargo-about`).
- [`justfile`](../justfile) — `licenses` (regenerate) + `licenses-check` (CI-friendly diff check) recipes.
- [`prek.toml`](../prek.toml) — new hook calling `licenses-check` (priority 0, file-check tier).
- [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml) `[package.metadata.nfpm]` — add `THIRD-PARTY-LICENSES.html` → `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` to `contents`.
- `crates/tisvlay-gui/src/main.rs` — `const NOTICES: &str = include_str!("../THIRD-PARTY-LICENSES.txt");` and surface as a Slint property.
- [`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint) — new "Open Source Notices" sub-section with `TextEdit { read-only: true }` rendering the property.
- [`packaging/README.md`](../packaging/README.md) — short section on the attribution flow + how it's regenerated + where the HTML variant lives in the package.

**Smoke tests**:
- `just licenses` regenerates without diff on a clean tree.
- `just package-rpm` produces a package whose `rpm -qpl` lists `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`.
- `tisvlay-gui` About pane shows the embedded text and is scrollable.

## Open questions

1. **Version chip data source**: compile-time `env!("CARGO_PKG_VERSION")` (passed in via Slint property), or pull from a Slint global the way `daemon-available` is wired? Compile-time is simpler and the version doesn't change at runtime.
2. **URL-open routing for the View source / Report issue buttons**: `webbrowser` crate (≈1.5M downloads, cross-platform, well-maintained) or hand-rolled `std::process::Command::new("xdg-open")`? The crate is one extra dep but handles edge cases; the std::process route is one less dep.
3. **Components-table data source**: list only the binaries actually built, or stay statically authored as the handoff shows? Static is simpler and stable; build-time enumeration would only matter if we ever conditionally compile a binary out.
4. **Notices presentation**: single big scrollable `TextEdit`, or expandable per-license sections? The research and VS Code precedent say single blob; per-license sections add UI complexity for limited reader benefit.
5. **MIT-vs-Apache for `OR` clauses in `about.toml` priority**: picking MIT first minimizes NOTICE-propagation work (industry norm — what ripgrep/Alacritty do); picking Apache aligns with the project's own license. Default to MIT unless there's a project-values reason otherwise.

## Out of scope

- Authors / Contributors list (the handoff doesn't show one; can be added later if/when we have meaningful contributors beyond solo dev).
- Diagnostic copy-to-clipboard button (mentioned in `docs/plan.md` M5 description but visually not in the handoff for About; defer to M5 polish).
- Per-vendor backend attribution (LG/Samsung/Sony when those land) — once we have multi-vendor builds, vendor backend crates may want their own license sub-sections. Cross-link from this plan when relevant.
- A separate "Third-party Notices" pane in the sidebar (some apps do this; we follow the simpler pattern of keeping it inside About).

## Done when

- About pane visually matches both light and dark screenshots in [`screenshots/`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/).
- The `AboutSlint` widget is rendered in the About pane (Slint license satisfied).
- `THIRD-PARTY-LICENSES.{txt,html}` are generated, checked in, and refresh on `just licenses`.
- `prek run -a` fails if `Cargo.lock` changes without a corresponding regenerate.
- The `.rpm` smoke-tested on Bazzite still installs and the GUI launches cleanly with the new pane visible.
- [`docs/plan.md`](plan.md) M5 entry is updated to mark the About-pane work complete; the deferred items above are filed in [`docs/ideas.md`](ideas.md).

# Plan 2 — About pane parity + FOSS attribution

**Status: Commit A shipped + two follow-up plans (former `plan3.md`, `plan4.md`) absorbed below; Commit B redesigned twice (first for the dedicated Notices pane, then again post-research for the indie-Rust convention) and now split into B1 + B2 to keep the scaffolding/first-usage rule honest.** Commits A through C of the absorbed work landed on `feat/gui-about` (peer-introspection symmetry, KDE HIG cursor compliance, COMPONENTS-table consolidation, file-manager folder buttons, source-side `absolutize`). Commits B1 (workspace config + policy gate) and B2 (GUI Notices pane + packaging) are the remaining work.

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
- **Indie-Rust convention is "don't check in the generated bundle."** A survey of eleven well-regarded indie/single-maintainer Rust projects (ripgrep, fd, bat, alacritty, helix, zed, rustdesk, nushell, starship, gitui, lapce) found **none** ship a `THIRD-PARTY-LICENSES` file in their repo. The pattern is: small hand-written root `NOTICE` declaring the project's own copyright + license election, `cargo-deny` configured at repo root as a CI license-allow-list gate, and the bundled-deps artifact generated at package-build time only — gitignored, shipped under `/usr/share/doc/<pkg>/` in each binary distribution. Bat's `acknowledgements.bin` (built from `assets/create.sh`) and Materialize's `ci/deploy/devsite.sh` (calls `cargo about generate ci/deploy/licenses.hbs > licenses.html`) are the canonical examples. Checking the artifact in produces real diff churn (each transitive dep bump touches the file even when SPDX IDs don't change) without adding compliance value `Cargo.lock` doesn't already record.

## Tooling decision

Two complementary tools, each with a clear role:

**[`cargo-about`](https://github.com/EmbarkStudios/cargo-about)** generates the bundled-deps attribution artifact. Reasons:

- Re-resolves SPDX expressions per crate using a configurable priority list (so `MIT OR Apache-2.0` deterministically picks one option).
- Fetches missing license texts via clearlydefined.io.
- Supports per-crate clarifications for non-standard packaging.
- Emits arbitrary formats via Handlebars — we generate **two** outputs from the same data: a plain-text variant for `include_str!` embedding into the GUI binary, and an HTML variant for `/usr/share/doc/tisvlay/`.
- Defaults to single-package; we invoke with `--workspace --all-features` to cover all five binaries' transitive deps in one pass.

**[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)** runs as a CI policy gate. Reasons:

- Reads a checked-in `deny.toml` SPDX allow-list at repo root.
- Fails the prek pipeline if a new transitive dep introduces a forbidden license (e.g. GPL/LGPL/MPL slipping in via a future dep bump).
- Catches policy regressions at PR time without producing a diff-noisy generated artifact — replaces the original "stale-attribution prek hook" plan with a more useful "license policy enforcement" check.

Rejected alternatives: `cargo-license` (lists only, no text); `cargo-bundle-licenses` (YAML output still needs rendering); `cargo-3pl` (unmaintained).

## Layout target

Workspace-scoped sources at the repo root; generated artifacts gitignored. Mirrors what cargo-about expects (workspace-root config) and what the indie-Rust convention found above does (no checked-in bundled-deps blob).

```
about.toml                                  ← cargo-about: SPDX allow-list + OR-priority + clarifications (CHECKED IN)
about.txt.hbs                               ← Handlebars template for the embedded plain-text blob
about.html.hbs                              ← Handlebars template for /usr/share/doc/...
deny.toml                                   ← cargo-deny: license-policy allow-list (CHECKED IN)
NOTICE                                      ← hand-written project copyright + license election (CHECKED IN, ~4 lines)

crates/tisvlay-gui/
├── build.rs                                ← runs `cargo about generate ... about.txt.hbs > $OUT_DIR/THIRD-PARTY-LICENSES.txt`
├── ui/components/about_pane.slint          ← updated: `View third-party notices` button in LICENSE group
└── ui/components/notices_pane.slint        ← new: scrollable read-only TextEdit fed by build-time blob

(generated, gitignored — never committed)
target/.../THIRD-PARTY-LICENSES.txt         ← build.rs output, embedded via `include_str!(env!("..."))`
THIRD-PARTY-LICENSES.html (repo root)       ← built by `just licenses-html` for nfpm packaging
```

**`about.toml` content (informed by [cargo-about docs](../docs/cargo-about/) review):**

- `accepted = ["MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "BSL-1.0", "Unicode-3.0", "CDLA-Permissive-2.0", "Unlicense", "MIT-0"]` — priority-ordered. cargo-about picks the **first** match for `OR` expressions, so MIT wins on `MIT OR Apache-2.0`, minimizing NOTICE-propagation work.
- `targets = ["x86_64-unknown-linux-gnu"]` — Linux-only project; prunes Windows/macOS-only crates (`core-foundation`, `windows-sys`, etc.) from the bundle.
- `workarounds = ["rustls", "ring", "wasmtime", "rustix"]` — built-in cargo-about clarifications. `rustls` is a direct dep (LG WebSocket TLS), `ring` likely transitive, the LLVM-exception workarounds cover the 18 `Apache-2.0 WITH LLVM-exception` crates surfaced in the dep survey above. The full workaround catalogue lives at `docs/cargo-about/docs/src/cli/generate/workarounds.md` — review it whenever the dep graph changes.
- `private = { ignore = true }` — skip license checks on our own workspace crates (they're `publish = false`). Without this, cargo-about will either try to validate `tisvlay-test`/`tisvlay-mock`/`tisvlay-harness`'s license against the accept-list or noisily warn about them.
- `ignore-build-dependencies = false` — keep build-deps in the bundle: their generated output ships in our binaries.
- `ignore-dev-dependencies = true` — test-only crates don't ship; exclude.
- `filter-noassertion = true` — when clearlydefined.io conservatively tags a license expression with `NOASSERTION`, fall back to local scanning rather than requiring the user accept that pseudo-id.

Bootstrap: `cargo about init` generates a starter `about.toml` and `about.hbs`; we refine the toml per the above and write our own templates.

**Embedding strategy.** `tisvlay-gui/build.rs` shells out to `cargo about generate --workspace --all-features --locked --fail about.txt.hbs > $OUT_DIR/THIRD-PARTY-LICENSES.txt` and emits `cargo:rerun-if-changed=Cargo.lock` so it only re-runs on dep changes. `--locked` keeps the build reproducible (fails if `Cargo.lock` drifts or is missing); `--fail` exits non-zero if any crate's license can't be resolved (catches new deps with weird packaging at the build-time boundary). The Notices pane reads via `include_str!(concat!(env!("OUT_DIR"), "/THIRD-PARTY-LICENSES.txt"))`. Graceful degradation: if `cargo-about` isn't on `$PATH` (clean dev env without the Nix shell), the script writes a stub explaining how to install + falls through with a warning rather than failing the build — the GUI's Notices pane shows the stub text in that case.

**HTML for packaging.** A `just licenses-html` recipe regenerates `THIRD-PARTY-LICENSES.html` at the repo root before the nfpm `package-build` runs; nfpm contents map it to `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` so distro audit tools (rpm-sequoia, lintian, etc.) find it. The `package-{deb,rpm,archlinux}` recipes depend on `licenses-html` via `just`'s prerequisite mechanism so it always refreshes before bundling.

**Policy gate.** `cargo-deny check licenses` runs as a prek hook (priority 0, file-check tier — fast, no compilation). Catches a forbidden-SPDX dep at PR time without touching any artifact.

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

Three commits. A shipped; B1 + B2 remain. The split keeps each commit at one coherent layer — B1 is workspace tooling that no binary depends on yet, B2 is every binary-side and packaging consumer of B1's output landing together (scaffolding + first usage paired, per the documented preference).

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

### Commit B1 — Workspace config + license-policy gate

**Goal**: Stand up the cargo-about and cargo-deny scaffolding at the workspace root, so we can generate the bundled-deps blob on demand and enforce the SPDX allow-list at PR time. No binary, UI, or packaging changes — the next commit (B2) consumes this scaffolding.

The split is deliberate: B1 lands tooling that nothing depends on yet (a `just licenses-html` invocation produces output that gets gitignored; `just deny` is wired into prek but doesn't touch shipped code). It's a coherent, independently-reviewable foundation; B2 is then the user-facing payoff.

#### Touches

- [`flake.nix`](../flake.nix) — add `cargo-about` and `cargo-deny` to the dev shell.
- `about.toml` (repo root) — `cargo-about` SPDX accept-list (priority-ordered: MIT > Apache-2.0 > BSD-* > ISC > Zlib > BSL-1.0 > Unicode-3.0 > CDLA-Permissive-2.0 > Unlicense > MIT-0); `targets = ["x86_64-unknown-linux-gnu"]`; `workarounds = ["rustls", "ring", "wasmtime", "rustix"]`; `private = { ignore = true }`; `ignore-dev-dependencies = true`; `filter-noassertion = true`. Per Decisions 11 + 12 above.
- `about.html.hbs` (repo root) — Handlebars template for the packaged HTML variant; navigable layout with per-license anchors per the upstream `about.hbs` template (link in `docs/cargo-about/about.hbs`). The plain-text template lives with its consumer in B2.
- `deny.toml` (repo root) — `[licenses]` section only (skip `[bans]`/`[advisories]`/`[sources]` for now; out of scope). `allow = [...]` mirrors `about.toml`'s `accepted` list. `confidence-threshold = 0.92` (cargo-deny default).
- `NOTICE` (repo root) — hand-written, ~4 lines:
  ```
  TisVlay
  Copyright 2026 TisVlay contributors

  Licensed under the Apache License, Version 2.0
  (see LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0).
  ```
  Mirrors bat's pattern. Upstream `NOTICE` propagation lives inside the bundled blob (each License's `text` includes its NOTICE content if the upstream packaged one); this top-level `NOTICE` is just ours.
- `.gitignore` — add `/THIRD-PARTY-LICENSES.html` (repo root, the just-licenses-html output). Also ensure `target/` already covers the build.rs OUT_DIR file (it does — `target/` is workspace-gitignored).
- [`justfile`](../justfile) — two new recipes:
  - `licenses-html`: `cargo about generate --workspace --all-features --locked --fail about.html.hbs -o THIRD-PARTY-LICENSES.html`.
  - `deny`: `cargo deny check licenses`.
- [`prek.toml`](../prek.toml) — new hook `deny` calling the just recipe (priority 0, file-check tier — `cargo deny check licenses` is fast, no compilation needed).

#### Smoke tests

- `cargo about generate --workspace --all-features --locked --fail about.txt.hbs > /tmp/notices.txt` produces a non-empty file with no errors. Eyeball-verify it includes Slint's text, rustls's text, ring's text, and a sensible LLVM-exception entry.
- `just licenses-html` writes a non-empty `THIRD-PARTY-LICENSES.html` at the repo root; `git status` shows the workspace clean (file is gitignored). Open the HTML in a browser; sanity-check anchor navigation works.
- `just deny` exits 0 on the current workspace.
- Temporarily add a GPL-licensed crate to a fresh test branch (e.g. add `readline = "5"` or any GPL crate to a workspace member's deps), run `just deny` — it exits non-zero with a clear license-policy violation message. Roll the branch back.
- `prek run -a deny` runs the same check as part of the pre-commit pipeline and gates merges.

### Commit B2 — GUI Notices pane + packaged HTML

**Goal**: Consume B1's scaffolding from both binary surfaces — the GUI's About pane (via a Notices sub-pane fed by a build-time embed) and the distro packages (via `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`). Pairing them in one commit so the scaffolding gets its first usage in both places it needs to land.

#### Pane navigation model

`Notices` is a "modal-ish" pane: it has an entry in the Slint `Pane` enum but no sidebar item. Reachable only via a `View third-party notices` button in the About pane's LICENSE group; the Notices pane carries its own header with a `← Back` button that sets `active-pane = Pane.about`. This keeps the sidebar uncluttered (it's reference material, not navigation) while still using the existing pane-switching plumbing — no overlay/popup framework needed. Sidebar nav remains visible during the Notices pane (so the user can still jump elsewhere); the back button is the canonical return path.

#### Touches

**GUI build-time embed**

- `about.txt.hbs` (repo root) — Handlebars template for the embedded plain-text blob. Emits a flat `# {license name}\n{used by ...}\n{license text}\n---\n` per License, sized for Slint's `TextEdit { read-only: true }` rather than the navigable HTML variant from B1.
- `crates/tisvlay-gui/build.rs` (new) — invoke `cargo about generate --workspace --all-features --locked --fail about.txt.hbs > $OUT_DIR/THIRD-PARTY-LICENSES.txt`. Emit `cargo:rerun-if-changed=Cargo.lock` so it only re-runs on dep changes. If `cargo-about` isn't on `$PATH`, write a stub explaining how to install + emit a `cargo:warning=` rather than failing the build. Stub content: a short paragraph saying the embedded notices weren't generated because `cargo-about` wasn't available, with the install command and a link to the project README.
- `crates/tisvlay-gui/Cargo.toml` — no new deps; `cargo-about` is a dev-shell tool, not a `[build-dependencies]` crate.
- `crates/tisvlay-gui/src/main.rs` — `const NOTICES: &str = include_str!(concat!(env!("OUT_DIR"), "/THIRD-PARTY-LICENSES.txt"));` ; push as a Slint property.

**Notices pane**

- `crates/tisvlay-gui/ui/components/notices_pane.slint` (new) — the dedicated pane. Header row with title + `← Back` button (calls `back-clicked`); body is a `ScrollView { TextEdit { read-only: true; text: root.notices; } }`.
- [`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint) — extend the `Pane` enum with `notices`; instantiate `NoticesPane` gated on `active-pane == Pane.notices`; wire `back-clicked => { root.active-pane = Pane.about; }`.
- [`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint) — add a `View third-party notices` button to the LICENSE group. Note: `UriBtn` is wrong here (per Plan 4, it's reserved for URL openers; this is an in-app pane navigation). Default to std-widgets `Button` unless the visual treatment looks out of place; if it does, extend `widgets.slint` with a small in-app-action variant rather than reusing `UriBtn`.

**Packaging**

- [`justfile`](../justfile) — `package-{deb,rpm,archlinux}` recipes gain `licenses-html` as a prerequisite so the HTML always refreshes before bundling.
- [`crates/tisvlayd/Cargo.toml`](../crates/tisvlayd/Cargo.toml) `[package.metadata.nfpm]` — add `THIRD-PARTY-LICENSES.html` → `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html` to `contents`.
- [`packaging/README.md`](../packaging/README.md) — short section on the attribution flow: how `licenses-html` regenerates, where the HTML variant lives in the package, what `deny` enforces, and why nothing's checked in.

#### Smoke tests

- `cargo build -p tisvlay-gui` populates `target/.../build/.../out/THIRD-PARTY-LICENSES.txt` (verified via `find target -name THIRD-PARTY-LICENSES.txt`).
- `cargo build -p tisvlay-gui` with `cargo-about` removed from `$PATH` (or `PATH=/usr/bin:/bin cargo build`) succeeds with a `cargo:warning=` and the embedded blob is the install-instructions stub.
- About pane LICENSE group shows the new `View third-party notices` button. Click → Notices pane displays the embedded text in a scrollable view. Back button → returns to About pane (sidebar `active-pane` flips to `tvs` if the user clicks a sidebar entry mid-flow; otherwise stays on About).
- `just package-rpm` produces a package whose `rpm -qpl` lists `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`. `rpm -qp --info` shows the package size hasn't ballooned absurdly (the HTML is ~MB-scale, acceptable).

## Decisions

1. **Version chips**: compile-time `env!("CARGO_PKG_VERSION")` for the GUI's overall version chip in the header row. Per-row chips in the COMPONENTS table for the daemon and session reflect their probed runtime versions (read via the new `Version` property on `Manager1` and `Session1`). Tone the chip by comparison vs. the GUI's compile-time version: **info** if equal, **warn** if remote > local, **danger** if remote < local. CLI and tray rows: no chip — they're not D-Bus services we have a connection to, and shelling out to `--version` is awkward and brittle. (The drift case that matters in practice is daemon/session-vs-GUI after a partial upgrade where services haven't been restarted.)
2. **URL-open routing**: `webbrowser = "1"`. Slightly more dep weight than a hand-rolled `xdg-open` invocation, but cross-platform-ready and we may want more of its features later.
3. **Components table data source**: data-driven from Rust (forced by decision 1 — we need to push per-row version + tone). Slint renders via a `Repeater` over a property-bound array.
4. **Notices presentation**: dedicated `Notices` pane (no sidebar entry), reachable only via a button in the About pane's LICENSE group; the pane has its own back button. Originally specced as a sub-section inside About; promoted to its own pane after Plan 4 substantially densified the About pane (peer table + status circles + chips + folder buttons + LICENSE group + AboutSlint badge — adding a ~658-crate scrollable text dump on top of that would overwhelm the page). Per-license expandable sections still deferred as a stretch UX goal.
5. **OR-clause license priority** in `about.toml`: MIT first, Apache-2.0 second, others by `cargo-about`'s default order. Industry norm; minimizes NOTICE-propagation work.
6. **Diagnostic copy-to-clipboard button**: included even though not in the handoff. Lives in the LICENSE group alongside `View source` / `Report issue`, copies the JSON blob `{ daemon_version, session_version, slint_version, gui_version }` (from [`docs/plan.md`](plan.md) M5) via `arboard` (already a dep).
7. **Generated artifacts are gitignored, not checked in**: matches the indie-Rust convention surveyed under "Findings worth flagging." `Cargo.lock` already records dep changes with proper diff context; checking in `THIRD-PARTY-LICENSES.{txt,html}` would generate diff churn on every transitive bump even when SPDX IDs don't change. The DataDog/NewRelic-style "check-in + CI-diff-check" pattern is corporate-compliance overhead the project doesn't need at single-maintainer scale.
8. **`cargo-deny` instead of an artifact-staleness prek hook**: the originally-planned `licenses-check` (regenerate + diff) hook was a proxy for "did someone forget to refresh the bundle?" — meaningless once the bundle is gitignored. Replaced with `cargo deny check licenses`, which catches the *actual* compliance regression (a forbidden SPDX ID slipping into the dep graph) at PR time without producing any artifact.
9. **Repo-root layout** (rather than `crates/tisvlay-gui/`-scoped): the artifact covers all five binaries' transitive deps and ships product-wide at `/usr/share/doc/tisvlay/`. The location should match the truth (workspace-wide). `cargo-about` natively supports `--workspace`; no per-crate config is needed.
10. **GUI embed via `build.rs` + `OUT_DIR`** (option (a) of three considered): always-fresh embedded blob, no source-tree churn, single user-facing surface (the Notices pane works in dev and packaged builds alike). Tradeoff: requires `cargo-about` on `$PATH` at build time (graceful degradation: stub blob with install instructions if missing). Rejected: (b) "read `/usr/share/doc/...` at runtime" — broken in dev builds without a stub mechanism; (c) "open HTML in browser via `xdg-open`" — leaves the GUI without a self-contained Notices view, contradicts the plan's "dedicated pane" decision.
11. **`about.toml` config choices** (informed by upstream-docs review): `targets = ["x86_64-unknown-linux-gnu"]` (Linux-only project; prunes WIN/macOS-only deps from the bundle), `private = { ignore = true }` (skip license checks on our own `publish = false` workspace crates), `ignore-dev-dependencies = true` (test crates don't ship; build-deps stay in because their generated output is baked into the binaries we ship), `workarounds = ["rustls", "ring", "wasmtime", "rustix"]` (cargo-about's built-in clarifications cover our direct + transitive license-packaging quirks; review on every dep-graph shift), `filter-noassertion = true` (clearlydefined.io occasionally taints expressions with `NOASSERTION` — falling back to local scanning avoids the user having to accept a pseudo-SPDX id). The full set of upstream workarounds is enumerated at `docs/cargo-about/docs/src/cli/generate/workarounds.md`.
12. **`--locked --fail` on every `cargo about generate` invocation** — both in the GUI's `build.rs` and the `just licenses-html` recipe. `--locked` keeps the bundle reproducible (fails if `Cargo.lock` drifts or is missing); `--fail` exits non-zero if any crate's license can't be resolved, catching weird-packaging issues at PR time rather than silently emitting a partial bundle.

## Out of scope

- Authors / Contributors list (the handoff doesn't show one; can be added later if/when we have meaningful contributors beyond solo dev).
- Per-vendor backend attribution (LG/Samsung/Sony when those land) — once we have multi-vendor builds, vendor backend crates may want their own license sub-sections. Cross-link from this plan when relevant.
- A sidebar entry for the Notices pane. The pane exists, but it's reachable only via the About pane button — Notices is reference material, not navigation.

## Done when

- About pane visually matches both light and dark screenshots in [`screenshots/`](../.claude/ui/design_handoff_tisvlay_gui/screenshots/).
- The `AboutSlint` widget is rendered in the About pane (Slint license satisfied).
- `about.toml`, `about.{txt,html}.hbs`, `deny.toml`, and a hand-written `NOTICE` are checked in at the repo root. Generated `THIRD-PARTY-LICENSES.{txt,html}` are gitignored and only built on demand.
- `cargo build -p tisvlay-gui` populates `$OUT_DIR/THIRD-PARTY-LICENSES.txt` (verified via `find target -name THIRD-PARTY-LICENSES.txt`); the GUI's Notices pane embeds the generated blob via `include_str!`.
- `cargo build -p tisvlay-gui` without `cargo-about` on `$PATH` succeeds (graceful degradation): the embedded blob is a stub with install instructions, accompanied by a `cargo:warning=`.
- `prek run -a` runs `cargo-deny check licenses` and fails if a forbidden SPDX id (GPL/LGPL/MPL etc.) appears in the workspace dep graph.
- The `.rpm` smoke-tested on Bazzite still installs and the GUI launches cleanly with the new pane visible. `rpm -qpl tisvlay-*.rpm | grep THIRD-PARTY` lists `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`.
- About pane LICENSE group has a `View third-party notices` button. Click → Notices pane shows the embedded text in a scrollable view with a `← Back` button that returns to About. No sidebar entry for Notices.
- [`docs/plan.md`](plan.md) M5 entry is updated to mark the About-pane work complete; the deferred items above are filed in [`docs/ideas.md`](ideas.md).

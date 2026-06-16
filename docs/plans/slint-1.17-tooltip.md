# Slint 1.17 migration + reusable Tooltip pattern

## Context

This work will (1) migrate `tisvlay-gui` off the pinned Slint `=1.16.1`
(crates.io) onto the unreleased Slint **1.17**, then (2) start using 1.17's new
built-in `Tooltip` element to document the GUI's pane option controls.

1.17.0 is **not cut yet** — the latest tag is `v1.16.1`; master is `1.17.0-pre`
(local checkout `/home/ruffsl/ws/lgtv/libs/slint`, HEAD `a532df9e7`). So the
migration must pin a **git rev**, not a crates.io version. The `Tooltip`
element only exists in the 1.17 compiler, so the dependency migration is a hard
prerequisite for any tooltip work.

Decisions confirmed with the user:
- **Dependency strategy:** pinned git rev (CI/Nix-reproducible, committable).
- **Tooltip target:** in-pane option controls (the GUI has no menus — only
  sidebar nav + panes).
- **Tooltip scope:** reusable pattern only — land the reusable helper + 2-3
  proof controls, expand control-by-control in follow-ups.

This is two reviewable units, and they **must be two PRs in order** (not one):
Phase A (migration) lands and goes green on its own — it also carries a publish
policy change worth reviewing in isolation; Phase B (tooltips) only compiles
once A's 1.17 compiler is in. Per project convention the user pre-creates the
branch; baseline-commit the migration before layering tooltip changes.

**Forced consequence to surface:** depending on unreleased Slint via a git rev
means `tisvlay-gui` (currently `publish = true`) can no longer be published to
crates.io — `cargo publish` rejects any git/rev dependency. The release
workflow (`.github/workflows/release.yml` Phase 2 `publish-crates`) would fail
at the next release tag. So Phase A must set `tisvlay-gui` `publish = false` for
the duration of the pin and re-enable it when swapping back to a crates.io
`=1.17.0`. This is contained: only `tisvlay-harness` (a dev crate) depends on
`tisvlay-gui`.

---

## Phase A — Migrate Slint `=1.16.1` → git rev `a532df9e7` (1.17.0-pre)

Verified against the local checkout: crate names match ours verbatim (no
`package =` rename needed); all four crates share **one rev**; every feature we
use (`std`, `backend-winit-wayland`, `compat-1-2`, `renderer-femtovg`,
`gettext`, `unstable-winit-030`, `mcp`) exists on master; the `winit_030` +
`BackendSelector` APIs and **all** `i-slint-backend-testing` helpers we call are
unchanged; MSRV 1.92 ≤ our 1.95.

### A0. Pick the rev + publish/MSRV prerequisites

- **Choose a rev with confirmed-green upstream CI**, not today's `master` HEAD
  (`a532df9e7` is a dependabot npm bump — coincidental, low-signal). Check the
  GitHub Actions status for the candidate SHA (prefer the latest commit that
  touches the Rust/compiler crates and passed CI). Record *why this rev* in the
  Cargo.toml comment. Use the **full 40-char SHA**, identical byte-for-byte
  across every Slint dep in both Cargo.tomls (short vs long = distinct cargo
  sources → breaks GUI↔harness `AppWindow` type identity).
- **`crates/tisvlay-gui/Cargo.toml` L9: `publish = true` → `publish = false`**
  (release-breaker fix above). Add a brief comment that the GUI is unpublishable
  while pinned to a git Slint rev, with the re-enable trigger.
- **Bump workspace MSRV**: `Cargo.toml` L39 `rust-version = "1.85"` → `"1.92"`
  (Slint master is `rust-version = "1.92"`). Toolchain is stable 1.95 so builds
  are fine, but the declared floor must move to stay honest. No MSRV CI gate
  exists, so this is a truthfulness fix, not a build fix.

### A1. `crates/tisvlay-gui/Cargo.toml` — replace 5 specs with `{ git, rev }`

Use the full SHA chosen in A0, URL `https://github.com/slint-ui/slint`. Keep
`default-features`, `features`, `optional` exactly as-is — only the version spec
changes. **Do not identicalize features** across crates; only the rev must
match (the GUI and harness `slint` specs legitimately carry different features).

- L64 `slint` (features: std, backend-winit-wayland, compat-1-2,
  renderer-femtovg, gettext, unstable-winit-030)
- L132 `i-slint-backend-selector` (default-features=false, features=["mcp"], optional)
- L133 `i-slint-backend-testing` (features=["mcp"], optional)
- L147 `i-slint-backend-testing` (dev-dependency)
- L150 `slint-build` (build-dependency)

Update the nearby comments (L64/L132/L147 say "Pinned to the exact slint
version"/"`=1.16.1`"/"Resolved Slint version on crates.io is `1.16.1`") to
describe the git-rev pin instead. The rev pin is still exact, so the
"unstable-winit-030 / no-semver" rationale holds — only drop the crates.io
version claims. No plan-phase references in checked-in comments.

### A2. `crates/tisvlay-harness/Cargo.toml` — L35/L36

Same git+rev for `i-slint-backend-testing` and `slint` (features: std,
compat-1-2). **Both crates must use the identical rev string** or cargo treats
them as distinct sources and the harness↔GUI `AppWindow` type identity breaks.
Update the L33-34 comment ("paired with the GUI's pinned `=1.16.1`") to the
shared-rev wording.

### A3. Regenerate `Cargo.lock`

`nix develop -c cargo update slint slint-build i-slint-backend-selector
i-slint-backend-testing` (or let `just check` rewrite it). The ~14 `slint*` /
`i-slint-*` entries flip `source` to `git+https://github.com/slint-ui/slint?rev=…`,
lose `checksum`, and bump `version` to `1.17.0`.

### A4. Nix `outputHashes` — `packaging/nix/package.nix`

The file uses the **dotted** form `cargoLock.lockFile = ../../Cargo.lock;` (L18).
Convert it to the block form (you cannot mix `cargoLock.lockFile = …` with a
`cargoLock = { … }` block):
```nix
cargoLock = {
  lockFile = ../../Cargo.lock;
  outputHashes = { "slint-1.17.0" = "sha256-AAAA…="; };  # placeholder
};
```
nixpkgs `importCargoLock` keys `outputHashes` by `"<name>-<version>"`, resolves
it to the commit SHA, then maps SHA→hash — so one entry for `slint-1.17.0` (a
real workspace member at version 1.17.0) covers **all** sibling git crates from
that rev. Capture the real hash via the fake-hash flow: run `nix build .#tisvlay`
(or `nix flake check`), read `got: sha256-…` from the mismatch error, paste it
back. Confirm `flake.nix`'s `checks` derivation inherits this (it reuses the
package).

### A5. Docs

- `docs/concerns/slint-unstable-winit-030.md` — rewrite the `### Pin` section
  for the git rev; fix the `winit_030` lib.rs line reference; the "Slint 1.17
  released → audit" re-evaluation trigger is now actioned, reframe it. Make this
  doc the **durable home** for two interim reminders (the plan file is
  ephemeral; don't put version-anchored TODOs in code comments): (1) "when
  1.17.0 is tagged on crates.io, swap the git rev back to `slint = \"=1.17.0\"`",
  and (2) "re-enable `tisvlay-gui` `publish = true` at the same time". The
  `ActivationToken` audit note still holds. (Standalone concern docs are allowed
  to be version-specific — unlike code comments.)
- `.claude/slint.md` — update the "Setup gotchas" snippet (`slint = "1.16"` /
  `i-slint-backend-selector = "=1.16.1"` / "Resolved … on crates.io is `1.16.1`")
  to the git-rev form. MCP/`SLINT_EMIT_DEBUG_INFO`/accessibility notes unchanged.
- `.claude/CLAUDE.md` — grep for `1.16` / `crates.io` / `slint =`; edit only if
  a version mention surfaces (none found so far).

### A6. Verify Phase A

1. `nix develop -c just check` (full `--all-features`, includes `about_pane`
   tests + harness). Pipe once to a file and grep it.
2. Confirm no x11rb feature-drift:
   `nix develop -c cargo tree -p tisvlay-gui --all-features -i x11rb`
   (expect absent — `default-features = false` on the selector is load-bearing
   for wayland-only; the L134-143 comment explains).
3. GUI + MCP smoke:
   `nix develop -c env SLINT_EMIT_DEBUG_INFO=1 cargo build -p tisvlay-gui --features dev-mcp`
   then run with `SLINT_MCP_PORT=9315`; confirm window maps (cold-spawn
   activation-token hook fires) and MCP is reachable.
4. `nix build .#tisvlay` once the real hash is in.

Expected breakage: **none in source** (all APIs verified unchanged). The only
"failures" are the intentional fake-hash Nix iteration and a possible x11rb
re-check.

---

## Phase B — Reusable Tooltip pattern + 3 proof controls

Blocked on Phase A (the `Tooltip` element needs the 1.17 compiler).

### Verified Tooltip facts (from `internal/compiler/builtins.slint` + `passes/lower_tooltips.rs`)

- `Tooltip` has exactly one user property: `text: styled-text` (`@tr(...)` or
  `@markdown(...)`). Two modes: text mode (`text:` set, no children) or custom
  mode (one child, no `text:`); both/neither = compile error.
- **A `Tooltip` may NOT be a direct child of a layout element** — parent must be
  a non-layout item (Rectangle/TouchArea/Button). Hover region = parent's full
  box.
- At most one per parent; `for : Tooltip {}` rejected; `if cond : Tooltip {}`
  allowed; cannot `inherits Tooltip`. `delay`/`offset` are internal, not
  settable.

### B1. Add `InfoTip` to `ui/widgets.slint`

A 16px circled "?" glyph whose root is a `Rectangle` (so the `Tooltip` child is
legal). One `in property <string> text`; render the Tooltip as a direct child of
the root Rectangle:
```slint
if root.text != "": Tooltip { text: @markdown("\{root.text}"); }
```
**Critical syntax** (verified against the compiler): `@markdown(root.text)` is a
**parse error** — `@markdown` only takes string *literals*. And a plain `string`
does **not** coerce to `Tooltip.text` (which is `styled-text`). The only
single-property route for a dynamic string is the template form
`@markdown("\{root.text}")`. Consequence: the interpolated text is rendered as
markdown — fine for plain prose (our tips use only parens/periods/hyphens, all
markdown-inert), but keep tip copy free of `_ * # [` backticks or document that
they format. The translatable `@tr(...)` stays at the **call site** (B2), so it
extracts cleanly; never nest `@tr` inside the `@markdown` template.

Give it accessibility (convention across widgets.slint + lets AT announce the
tip, which the visual hover affordance can't): `accessible-role: text;`
`accessible-label: root.text;`. Style with existing tokens (`Theme.info*`,
`Theme.text-faint`, `Design.font-mini`, `Design.border-thin`); confirm those
`Theme.info*` tokens exist (Chip's info tone + InlineError use them — no new
tokens). Follow the house comment-banner format (`// InfoTip — <one-line>.`,
blank `//`, then rationale). Hover region is just the glyph. Export it.

Rationale for this idiom (over a generic wrapper or std-widget edits): it's the
only approach that works uniformly for the std-widgets we can't modify (Switch,
ComboBox, SpinBox, CheckBox), it sits exactly where the existing `helper:`
precedent lives, and the tight glyph hover-target sidesteps the
parent-box-hover rule.

### B2. Wire proof controls — both reuse shapes — `ui/components/tvs_pane.slint`

Import `InfoTip`. Add `in property <string> tip;` to the pane-local `FormRow`
(~L99-151). The label cell is a **`VerticalLayout`** (L108-122) holding a single
`Text` — to put the glyph *beside* the label (not stacked below it), wrap that
`Text` + `InfoTip { text: root.tip; }` in a `HorizontalLayout` inside the
existing label `VerticalLayout`. `InfoTip`'s own root is a Rectangle, so it's a
legal layout child while the Tooltip lives inside that Rectangle.

**Shape 1 — glyph next to label (std-widgets), 3 rows.** Set `tip:` on three
rows; the labels are actually `@tr("Screen")` / `@tr("Primary")` /
`@tr("Backlight")` (the longer names below are descriptive only):

- **Screen** Switch — `tip: @tr("Blanks the panel without powering off (OLED-safe, instant resume).")` (harvest the dead `// helper:` copy ~L739)
- **Primary** input ComboBox — `tip: @tr("Which input this PC uses; affects on-primary automation.")` (harvest the dead `// helper:` copy ~L795)
- **Backlight** OptimisticSlider — `tip: @tr("Panel brightness. Lower values reduce power draw and burn-in risk.")` (new copy ~L727)

**Delete the now-dead `// helper:` comment lines** (~L739, ~L795) whose copy
moved into live `tip:` props — leaving them is a stale duplicate (the project's
no-tombstone/no-duplicate-copy convention).

**Shape 2 — direct tooltip on our own custom widget, 1 proof.** Add the optional
`in property <string> tip;` to a custom widget (e.g. `DangerBtn` in
`ui/widgets.slint`) with a one-line `if root.tip != "": Tooltip { text:
@markdown("\{root.tip}"); }` as a **direct child of the widget's root
Rectangle** (sibling to its `HorizontalLayout`/`touch`, never inside the
layout). Wire it on the DANGER ZONE **Remove TV** `DangerBtn` (~L1003):
`tip: @tr("Removes this TV from the daemon's config. Pairing must be redone.")`.

Together these prove both follow-up shapes: glyph-beside-label for the
un-editable std-widgets, and a direct `tip` prop for our own widgets. Expanding
later = one `tip:` line per control. Note the `FormRow.tip` plumbing is
**pane-local** to tvs_pane (other panes have their own row helpers), so
cross-pane expansion re-adds the prop per pane — `InfoTip`/`DangerBtn.tip` are
the truly shared seams.

### B3. i18n

All tips are `@tr(...)` (translatable UI prose). Run `just i18n-extract` to
refresh `crates/tisvlay-gui/lang/tisvlay-gui.pot`, commit the `.pot` diff.

### B4. Verify Phase B

1. `nix develop -c just check` (compiles `.slint`, so a Tooltip misuse —
   layout-child violation, both/neither text+children — fails the build for
   free).
2. Add one cheap render test (mirror `tests/about_pane.rs`'s
   `init_no_event_loop` + element-query idiom): assert an `InfoTip` with non-empty
   `text` renders and announces its tip via `accessible-label`, and that an empty
   `text` gates out the glyph (the `if root.text != ""` guard). `about_pane.rs`
   itself needs **no** change (it's scoped to the About pane).
3. GUI MCP smoke: build with `dev-mcp`, hover the proof rows' `InfoTip` glyphs
   and the Remove-TV button, confirm the popup appears after the delay near the
   cursor and is input-transparent (doesn't steal slider/combo/button clicks).

---

## Critical files

- `Cargo.toml` (root) — workspace `rust-version` 1.85 → 1.92 (A0)
- `crates/tisvlay-gui/Cargo.toml` — `publish = false` + 5 Slint pins → git rev (A0/A1)
- `crates/tisvlay-harness/Cargo.toml` — 2 Slint pins → git rev (A2)
- `Cargo.lock` — regenerated (A3)
- `packaging/nix/package.nix` — dotted→block `cargoLock` + `outputHashes` (A4)
- `docs/concerns/slint-unstable-winit-030.md`, `.claude/slint.md` — docs +
  durable re-enable/swap-back triggers (A5)
- `crates/tisvlay-gui/ui/widgets.slint` — new `InfoTip` + `DangerBtn.tip` (B1/B2)
- `crates/tisvlay-gui/ui/components/tvs_pane.slint` — `FormRow.tip` (in a
  `HorizontalLayout`) + proof wires + dead-comment deletions (B2)
- `crates/tisvlay-gui/lang/tisvlay-gui.pot` — regenerated (B3)
- `crates/tisvlay-gui/tests/` — new InfoTip render test; `about_pane.rs` is the
  reference idiom, not edited (B4)
- `.github/workflows/release.yml` — not edited; the publish job is *why* A0 sets
  `publish = false` (context)

## Open follow-ups (out of scope, by design)

- Expand tooltips control-by-control across the other panes (re-adding the
  pane-local row `tip` plumbing per pane; `InfoTip` is the shared seam).
- Extend the `tip` prop to the remaining custom widgets (IconBtn/UriBtn) beyond
  the `DangerBtn` proof.
- **When 1.17.0 is tagged on crates.io**: swap the git rev for
  `slint = "=1.17.0"` and re-enable `tisvlay-gui` `publish = true` (durable
  reminder lives in `docs/concerns/slint-unstable-winit-030.md`, not here).
- Other 1.17 features noticed in passing: new `SystemTrayIcon` element (tray is
  ksni today), `cross-axis-alignment` on layouts, `PopupWindow.is-open`.

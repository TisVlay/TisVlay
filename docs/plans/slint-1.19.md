# Slint 1.19

## Goal

Move the GUI to Slint 1.19 as its release approaches, and use the bump to pay down what the release makes unnecessary: workarounds for upstream bugs 1.19 fixes, plumbing of our own that 1.19 offers an API for, and whatever the new compiler diagnostics point at. The UI opts into edition 1.19, the first edition, so the GUI follows the language where it's going rather than where it was.

The bump itself was mechanical: the three pins moved and the GUI compiled first try, with one new compiler warning, and the whole GUI test suite passed on the pre-release branch. The paydown is where the judgment is, and this plan is its record.

## What 1.19 changes for this GUI

From the [pre-release changelog](https://github.com/slint-ui/slint/blob/pre-release/1.19/CHANGELOG.md), the branch's source, and the upstream trackers the GUI's docs cite, checked on 2026-11-09 against `pre-release/1.19` at b4448d86.

**Fixed upstream.**

- Tab reaches a focusable under a clipping element while a scroll view hides it ([#13614](https://github.com/slint-ui/slint/pull/13614)). `LineEditField`'s reach scope exists in part for this.
- The interpreter's "accessing deleted parent" crash on a callback that destroys its own element ([#6426](https://github.com/slint-ui/slint/issues/6426), closed 2026-10-18). `docs/concerns/slint-live-preview-nav-crash.md` tracks it.

**Still open, so the workarounds stay.**

- Native winit popups ([#11740](https://github.com/slint-ui/slint/pull/11740)) are still a draft: `ThemedSelect` stays, and the ComboBox concern with it.
- The accesskit panic ([#10269](https://github.com/slint-ui/slint/issues/10269)) and the missing window-focus API ([#4382](https://github.com/slint-ui/slint/issues/4382)) are open: the accessibility idea and the `unstable-winit-030` concern stand.
- A read-only `TextInput` is a Tab stop ([#13761](https://github.com/slint-ui/slint/issues/13761)) and a `FocusScope` eats the press that focuses it ([#12044](https://github.com/slint-ui/slint/issues/12044)): the bare-input field and `focus-on-click: false` stay.
- The three findings from the remote-navigation train (`changed has-focus` skipped on creation, `ContextMenu` opening on the Menu key's release, a programmatic `focus()` walking into hidden items) reproduce on the branch, as checked with the standalone reproducers on 2026-11-07. Their workarounds stay; filing them is held for the maintainer's review and isn't part of this train.

**New, where the GUI has plumbing of its own.**

- `ensure-visible(behavior)` on every element scrolls the enclosing Flickables by the smallest amount that brings it into view, edge to edge and with no margin. `Nav.reveal` and `HelpScrollView.keep-in-view` exist for the margin, so they stay. `ThemedSelect`'s flyout scrolls to the keyboard cursor with arithmetic over the row height and the visible count; the cursor row can ask for itself instead.
- `Flickable.scroll-to()` and `ListView.ensure-row-visible()`: no caller here (the lists are a `ScrollView` over a `for`).
- `fontique_012::set_shared_collection()` (feature `unstable-fontique-012`) hands Slint a font collection before the platform initializes. `fonts.rs` builds a second collection for the monospace lookup.
- Editions and the project file: `slint-project.json` names the main file and the edition, and the LSP finds it by walking up from any `.slint` file. `CompilerConfiguration::with_edition` sets it from the build script instead.
- The testing backend gains `ElementQuery::match_accessible_label()`; the tests match on `accessible-id`, which still has no matcher, so `support::find_by_accessible_id` stays.
- `WinitWindowAccessor::dispatch_winit_window_event()`: the key remaps dispatch Slint keys, not winit events, so no caller.
- `@deprecated`, `TextInput.has-selection`, `StyledText.selectable`, `backdrop-blur`: no use here.

**Behavior changes to check.**

- Edition 1.19: an element created with `if` or `for` counts toward its parent's size, and a property set by only some states keeps its default in the others. The UI has no `states` blocks, so only the sizing applies, and the panes create rows and cards with `if` throughout.
- The winit backend applies a launcher's startup token from the environment and clears the variables ([#14360](https://github.com/slint-ui/slint/pull/14360)). The GUI applies the D-Bus `activation-token` through its own hook; the two paths now coexist.
- A new warning: an accessible text input nested in an element with an accessible role exposes the same text twice. `ThemedSpinBox`'s input tripped it; the bump sets `accessible-role: none` on it, as `LineEditField` already does.
- The compiler gains an info level (bindings to a property's default value, a value with no effect at the end of an `if` branch, and others) that `slint-build` doesn't print; the 1.19 viewer's `--check` and the LSP do.

**Dependencies.** fontique moves to 0.12 with Slint (Dependabot #240 closes with the bump). Slint's `gettext-rs` moves to 0.8, retiring the duplicate-version skip and the advisory note in `deny.toml`. `pin-utils` stays, through `spin_on` under `slint-build`, so its advisory ignore stays. Slint's clipboard (`smithay-clipboard` 0.7) is on SCTK 0.20 while winit 0.30 keeps SCTK 0.19, so `smithay-client-toolkit`, `calloop` and `calloop-wayland-source` are duplicated until the winit upgrade; `deny.toml` skips the 0.19 side.

**Gates while the release is pending.** The Nix package build needs an output hash per git source, so the Nix CI jobs stay red until 1.19.0 is on crates.io and the three `[patch.crates-io]` entries go; `deny.toml` allows Slint's repository as a git source for the same window, so `just check` and the audit job stay meaningful. The dev shell's `slint-viewer` and `slint-lsp` build from the same revision, since a 1.18 tool doesn't know the project file's edition. The 1.19 viewer's `--check` over the project reports 80 infos, every one "set to its default value" (`alignment`, `spacing`, `width` and `height` mostly; 27 in the profiles pane, 21 in the widgets), and no warnings.

## Decisions

1. **The edition lives in the project file, and the build script compiles the project file.** `crates/tisvlay-gui/slint-project.json` holds `edition` and `main`; `build.rs` passes its path. *Why:* the LSP and the viewer read the same file, so the editor's preview, the headless checks and the build agree on the edition and the main file, declared once.
2. **The pre-release branch now, the registry at release.** The pins read `=1.19.0` and `[patch.crates-io]` points the three Slint crates at `pre-release/1.19`; the dev shell's viewer and LSP build from that revision. The PR stays a draft until 1.19.0 is published and the patch section is gone. *Why:* the release is days away by Slint's history (its calls for testing precede releases by one to two weeks), so the work starts now and merges on the release, with CI's audit and Nix jobs as the gate.
3. **Only the reach scope's desktop branch goes.** `LineEditField`'s `reach` scope also carries couch mode's edit-on-Return, where the input is disabled until Return so the on-screen keyboard doesn't rise at every field a remote steps past. On a desktop the scope only forwards Tab to the clipped input, which 1.19 reaches itself. *Why:* the fix covers one of the scope's two reasons.
4. **The select flyout follows its cursor with `ensure-visible()`.** The cursor row asks for itself when it becomes the cursor, and the `content-y` arithmetic over `row-h` and `vis` goes. *Why:* the behavior is the API's exact purpose, and the arithmetic duplicates the row height the rows already have.
5. **The reveal margin stays as it is.** `ensure-visible()` brings an item edge to edge, which Slint's focus handling already does; the margin around the stop is the GUI's own and keeps its code. *Why:* no API change covers it.
6. **The shared font collection isn't adopted.** `fonts.rs` keeps its own collection for the monospace lookup. *Why:* sharing needs `unstable-fontique-012`, a second preliminary feature renamed at every fontique bump next to `unstable-winit-030`, to save one system font scan that already runs off the UI thread (and Slint's own runs in the background since 1.19). The saving doesn't buy the coupling.
7. **The compiler's diagnostics are acted on, warnings and infos alike.** The gates deny warnings, and an info the 1.19 tools report is fixed where its suggestion is right and recorded here where it isn't. *Why:* the new level is a review of the `.slint` files for free.
8. **Upstream issues aren't filed from this train.** The hold's condition (TisVlay tried on 1.19) is met by it; the drafts wait for the maintainer's review. *Why:* filing is the maintainer's call.

## Steps

Each step is a commit on `feat/slint-1.19`, passing `just check`. The KDE neon VM is down for this train, so the layout checks use the dev shell: the 1.19 `slint-viewer`'s `--check` and `--screenshot` against the project, and the GUI's own live preview (`GUI_LIVE=true`) on the desktop.

1. **The bump.** The three pins at 1.19.0 on the pre-release branch, the lock, fontique 0.12, the project file with the edition, the spin box's accessible role, the `deny.toml` entries, the `unstable-winit-030` audit paragraph, and the dev shell's viewer and LSP from the same revision. *Verified by:* `just check`; the two tools build and report 1.19.0; `slint-viewer --check` on the project compiles the UI with the edition.
2. **The layout under the edition.** Headless renders of each pane, the dialogs and the overlays, from the 1.18.1 viewer before the dev shell reloads and from the 1.19 viewer after, diffed; what moved is fixed or accepted, and the GUI's live preview confirms the panes on the desktop. *Verified by:* the renders match, or each difference is listed in Deviations as intended.
3. **The reach scope's desktop branch goes** (decision 3), with the `slint-gotchas.md` entry rewritten for what's left. *Verified by:* the navigation tests still Tab into a field below the fold and leave it backwards; a new test Tabs into a field under the fold with the scope's forwarding gone.
4. **The select flyout follows its cursor** (decision 4). *Verified by:* a test that opens a select longer than `max-visible`, steps to its last option and picks it.
5. **The compiler's infos** (decision 7), from the 1.19 viewer's `--check` of the project. *Verified by:* the check reports nothing.
6. **The gotchas re-checked on 1.19.** `Timer.restart()` inside an inlined component, `absolute-position` read in a geometry change handler, and the interpreter's deleted-parent crash through the live preview's Edit pencil; each entry in `slint-gotchas.md` and the live-preview concern is kept, rewritten or retired by the result. *Verified by:* the verdicts recorded in the docs.
7. **The registry swap**, when 1.19.0 publishes: the patch section goes, `slint-viewer` returns to `fetchCrate`, `slint-lsp` to nixpkgs' package or a `fetchCrate` pin until nixpkgs ships 1.19, Dependabot #240 closes. *Verified by:* the audit and Nix CI jobs green; `nix flake check`.
8. **Docs, and retire the plan.** The 1.19 findings worth keeping move into `docs/concerns/slint-gotchas.md` and the concerns they touch; the idea and concern indexes follow; the plan file is deleted.

## Deviations

None yet.

## Slint `ComboBox` dropdown clipped by the window

**Priority**: low
**Found by**: GUI Profiles/TVs pane review (July 2026)

### Context

Slint's winit backend has no real popup surface: winit itself never implemented one ([rust-windowing/winit#950](https://github.com/rust-windowing/winit/issues/950)), so Slint *simulates* a `PopupWindow` — the surface backing a `ComboBox` dropdown, and also the 1.17 `Tooltip` — by drawing it *within* the top-level window. A `ComboBox` near a window edge therefore risks a dropdown clipped to the window bounds instead of overflowing past them like a native menu. (Slint's Qt backend renders popups as true OS windows and doesn't have this limitation; `tisvlay-gui` uses the winit + skia/femtovg backend.)

A partial fix already landed upstream — [slint-ui/slint#6434](https://github.com/slint-ui/slint/pull/6434) (Nov 2024) repositions the popup to stay *inside* the window where it fits. So in a normally-sized, resizable window the dropdown shifts up/over to remain fully visible. The residual failure modes:

- The window is too short to contain the dropdown at all → it still clips, and stock `ComboBox` has **no scrollable popup** (scrolling is a separate, still-unimplemented request tracked in [slint#2375](https://github.com/slint-ui/slint/issues/2375) — the workaround is a hand-rolled custom widget), so clipped entries below the fold are genuinely unreachable, not merely cosmetic.
- The `Tooltip` surface, which the #6434 repositioning doesn't cover — [slint#12260](https://github.com/slint-ui/slint/issues/12260).

Where this surfaces in the GUI — the field-heavy panes where `ComboBox` rows stack in a scrollable column, so a low row's dropdown opens near the window's bottom edge:

- `crates/tisvlay-gui/ui/components/profiles_pane.slint` — the per-field enum `ComboBox` in the profile editor.
- `crates/tisvlay-gui/ui/components/tvs_pane.slint` — input / system-profile selectors.
- `crates/tisvlay-gui/ui/components/pairing_wizard.slint`, `schema_form.slint` — vendor / system-profile selectors.

### Upstream tracking

Root cause and the live fix path (verified by reading the full threads, July 2026):

- [slint#2375](https://github.com/slint-ui/slint/issues/2375) — *"PopupWindow is clipped by Window"* (open; root cause). Also the home of the separate ComboBox-popup-scrolling request.
- [slint#12260](https://github.com/slint-ui/slint/issues/12260) — *"Tooltip is clipped when the anchor widget is near the edge of the window"* (open; same limitation, tooltip surface).
- [slint#11740](https://github.com/slint-ui/slint/pull/11740) — *"Native winit popup"* (open **draft**). Depends on winit#4543, `Fixes` #12260, uses a ChildWindow fallback where native popups aren't supported. Wayland demo already working.
- [winit#4543](https://github.com/rust-windowing/winit/pull/4543) — *"Wayland, Windows, MacOS: Popup Implementation"* (open, **not draft**; actively developed, NLnet-sponsored). Adds `WindowAttributes::with_type()` for decorationless child popups. **Wayland (Mutter + KDE Plasma), Windows, macOS done; X11 pending.** This is the PR slint#11740 tracks.
- [winit#4444](https://github.com/rust-windowing/winit/pull/4444) — *"Another attempt at adding support for popups"* (open **draft**, GTK-style `create_popup` API). An **earlier, superseded** attempt, stale since March 2026 — winit#4543 is the one that carried forward.

Notably, winit#4543's tested platforms include **KDE Plasma on Wayland — `tisvlay-gui`'s primary target** — so our environment is first in line to benefit once the chain lands, X11 notwithstanding.

### Risk

Low in practice. With the #6434 repositioning, the settings window is resizable and normally tall enough that a dropdown shifts to stay visible, so options stay reachable. The genuine failure mode is a deliberately shrunk / very short window where a bottom-edge dropdown can neither fit nor scroll, hiding entries below the fold. No data or interaction correctness impact beyond that reachability edge.

### Mitigation options

- **Do nothing (current).** The real fix is the winit#4543 → slint#11740 chain; there's no clean downstream workaround, and #6434 already covers the common case. Growing the default window or keeping selectors off the extreme bottom edge only papers over it and fights the layout.
- **Qt backend.** Slint's Qt backend renders real popup windows and sidesteps this entirely — but adopting it swaps the whole rendering path and pulls a Qt dependency, disproportionate to a cosmetic edge case. Rejected.
- **Avoid, don't rebuild.** A hand-rolled in-window overlay (the toggletip overlay pattern already used for KDE-style contextual help) would dodge `PopupWindow` clipping but means re-implementing keyboard nav, focus, and dismiss for a stock widget. Not worth it here.

### Re-evaluation triggers

- winit#4543 merges → watch slint#11740 to consume it, then bump Slint (coordinate with the exact-version pin — see [Slint `unstable-winit-030` feature dependency](slint-unstable-winit-030.md)). Wayland/KDE is covered by #4543; X11 users would still wait.
- A Slint release closes [#2375](https://github.com/slint-ui/slint/issues/2375) or ships #11740 → drop this concern, re-verify dropdowns and tooltips at the window's bottom/right edge.
- A user reports an actually unreachable option (not just visual clipping in a shrunk window) → escalates priority; revisit the overlay workaround.

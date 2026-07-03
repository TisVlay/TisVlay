## Slint `ComboBox` dropdown clipped by the window

**Priority**: medium
**Found by**: GUI Profiles/TVs pane review (July 2026); reproduced live via the Slint MCP server (July 2026)

### Reproduced

Confirmed on the running GUI (KDE Plasma Wayland, display scale 2.0; window 1440×960 physical = 720×480 logical), driven through the Slint MCP server:

- Profiles pane → system profile `default` → the field editor lists 7 event→action `ComboBox` rows; **"On DPMS on"** sits at the bottom edge (logical y≈435 in a 480-tall window).
- Opening it drops a **208px-logical (416px-physical)** popup **downward**. ~160px of it falls below the window's bottom edge and is clipped — only the first option ("none") is visible; "power_on", "power_off", "screen_on"/"screen_off" are cut off.
- The popup is **not** repositioned upward, and the stock `ComboBox` popup's internal `ScrollView` is itself clipped to the visible sliver, so the hidden options are effectively **unreachable** without first scrolling the field list to move the row away from the bottom edge.

Screenshots + element-tree geometry captured under `scratchpad/` during the session.

### Context

Slint's winit backend has no real popup surface: winit itself never implemented one ([rust-windowing/winit#950](https://github.com/rust-windowing/winit/issues/950)), so Slint *simulates* a `PopupWindow` — the surface backing a `ComboBox` dropdown, and also the 1.17 `Tooltip` — by drawing it *within* the top-level window. A `ComboBox` near a window edge therefore clips to the window bounds instead of overflowing past them like a native menu. (Slint's Qt backend renders popups as true OS windows and doesn't have this limitation; `tisvlay-gui` uses the winit + skia/femtovg backend.)

There *is* an upstream clamp — [slint-ui/slint#6434](https://github.com/slint-ui/slint/pull/6434) (Nov 2024) added `place_popup()`, which is supposed to reposition the popup to stay inside the window when it wouldn't otherwise fit (`i-slint-core`'s `window.rs::show_popup` → `popup::place_popup`, with the window rect as the clip region). **But the live repro above shows it does not take effect for our winit-Wayland `ComboBox` popup** — the dropdown clips downward rather than shifting up. So on 1.17.0 this is still the #2375 behavior in practice, and the earlier "partial fix handles the common case" assumption was wrong. It is **not** a scale-factor artifact: the clip reproduces identically at KDE display scale 100% and 200% (layout uses the scale correctly either way). Why the clamp doesn't fire on the ChildWindow path is an open question — a focused upstream repro would be needed to pin it (candidates: the clamp clips size but not origin on this path, or the ChildWindow render ignores the clamped origin).

Where this surfaces in the GUI — the field-heavy panes where `ComboBox` rows stack in a scrollable column, so a low row's dropdown opens near the window's bottom edge:

- `crates/tisvlay-gui/ui/components/profiles_pane.slint` — the per-field enum `ComboBox` in the profile editor (the confirmed repro).
- `crates/tisvlay-gui/ui/components/tvs_pane.slint` — input / system-profile selectors.
- `crates/tisvlay-gui/ui/components/pairing_wizard.slint`, `schema_form.slint` — vendor / system-profile selectors.

### Upstream tracking

Root cause and the live fix path (verified by reading the full threads, July 2026):

- [slint#2375](https://github.com/slint-ui/slint/issues/2375) — *"PopupWindow is clipped by Window"* (open; root cause). Also the home of a separate ComboBox-popup-scrolling request.
- [slint#12260](https://github.com/slint-ui/slint/issues/12260) — *"Tooltip is clipped when the anchor widget is near the edge of the window"* (open; same limitation, tooltip surface).
- [slint#11740](https://github.com/slint-ui/slint/pull/11740) — *"Native winit popup"* (open **draft**). Depends on winit#4543, `Fixes` #12260, uses a ChildWindow fallback where native popups aren't supported. Wayland demo already working.
- [winit#4543](https://github.com/rust-windowing/winit/pull/4543) — *"Wayland, Windows, MacOS: Popup Implementation"* (open, **not draft**; actively developed, NLnet-sponsored). Adds `WindowAttributes::with_type()` for decorationless child popups. **Wayland (Mutter + KDE Plasma), Windows, macOS done; X11 pending.** This is the PR slint#11740 tracks.
- [winit#4444](https://github.com/rust-windowing/winit/pull/4444) — *"Another attempt at adding support for popups"* (open **draft**, GTK-style `create_popup` API). An **earlier, superseded** attempt, stale since March 2026 — winit#4543 is the one that carried forward.

Notably, winit#4543's tested platforms include **KDE Plasma on Wayland — `tisvlay-gui`'s primary target** — so our environment is first in line to benefit once the chain lands, X11 notwithstanding.

### Risk

Medium. Confirmed real reachability impact, not just cosmetic: a `ComboBox` sitting at the bottom of a scrollable field list drops a dropdown that clips off-window, and the hidden options can't be scrolled into view within the clipped popup. The user can still reach them by scrolling the field list up first (so the row isn't flush against the window edge) or by enlarging the window — but nothing signals that, so it reads as a bug. No data-corruption risk; the current selection and the visible options still commit correctly.

### Mitigation options

- **Bottom-space reservation (applied, but insufficient alone).** The profile field editor reserves one dropdown's height of empty scrollable slack below its fields card (`Design.combobox-popup-room`, used in `profiles_pane.slint`). Verified via the Slint MCP: scrolling the list down first *does* let the bottom row's dropdown open fully in-window. **But QA found it doesn't help in practice** — Slint blocks vertical scroll while a popup is open (an open `PopupWindow` is pointer-modal: `window.rs::process_mouse_input` swallows any pointer event outside the popup rect, scroll included — the same modality native Qt/KDE combos have). So a user who opens a bottom-edge dropdown and sees it cut off cannot scroll to fix it, and no scroll-based fix is possible. The reservation is only a substrate. The intended follow-up (next commit) is a **custom `ComboBox` built on `ComboBoxBase` that flips its popup upward when the row sits low in the window** — matching native toolkits, which keep the popup on-screen rather than clipping it. It also doesn't cover the other panes (`tvs_pane`, `pairing_wizard`, `schema_form`). Remove all of it when the upstream native-popup fix lands.
- **Qt backend.** Slint's Qt backend renders real popup windows and sidesteps this entirely — but adopting it swaps the whole rendering path and pulls a Qt dependency, disproportionate here. Rejected.
- **Avoid, don't rebuild.** A hand-rolled in-window overlay (the toggletip overlay pattern already used for KDE-style contextual help) would dodge `PopupWindow` clipping but means re-implementing keyboard nav, focus, and dismiss for a stock widget. Not worth it here.

### Re-evaluation triggers

- winit#4543 merges → watch slint#11740 to consume it, then bump Slint (coordinate with the exact-version pin — see [Slint `unstable-winit-030` feature dependency](slint-unstable-winit-030.md)). Wayland/KDE is covered by #4543; X11 users would still wait.
- A Slint release closes [#2375](https://github.com/slint-ui/slint/issues/2375) or ships #11740 → drop this concern, remove the `Design.combobox-popup-room` reservation from `profiles_pane.slint` (and the token), and re-verify the "On DPMS on" dropdown at the window's bottom edge.
- If it becomes worth chasing before then: file a focused upstream repro against #2375 showing #6434's `place_popup` clamp not repositioning a `ComboBox` popup on the winit-Wayland ChildWindow path (with the geometry captured here), and/or extend the bottom-space reservation to the other ComboBox panes (`tvs_pane`, `pairing_wizard`, `schema_form`).

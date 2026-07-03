## Slint `ComboBox` dropdown clipped by the window

**Priority**: low (profiles pane fixed; other panes still exposed)
**Found by**: GUI Profiles/TVs pane review (July 2026); reproduced live via the Slint MCP server (July 2026)
**Status**: mitigated in the profiles pane with a custom flip-up dropdown (`ThemedSelect`); the stock `ComboBox` panes (`tvs_pane`, `pairing_wizard`, `schema_form`) still clip.

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

Low, now that the field-dense profiles pane is fixed. The remaining exposure is the stock-`ComboBox` panes (`tvs_pane`, `pairing_wizard`, `schema_form`), whose selectors are fewer and rarely sit flush against the window's bottom edge. Where a stock `ComboBox` does clip, the hidden options can't be scrolled into view within the clipped popup (Slint blocks scroll while a popup is open — an open `PopupWindow` is pointer-modal, the same modality native Qt/KDE combos have), so the workaround is to enlarge the window. No data-corruption risk; the current selection and the visible options still commit correctly.

### Mitigation

- **Flip-up overlay (applied to the profiles pane).** `crates/tisvlay-gui/ui/globals/select.slint` adds `ThemedSelect` — a drop-in for `ComboBox` on the `model`/`current-value`/`selected` subset — whose dropdown is an in-window overlay (`SelectOverlay` at the AppWindow root), **not** a Slint `PopupWindow`. Mirroring the [Help overlay](../../crates/tisvlay-gui/ui/globals/help.slint), it escapes the detail's clipping ScrollView, reads the window size, and **flips upward when the row sits low in the window** — matching native Qt/KDE, which keep the dropdown on-screen. Verified via the Slint MCP: a bottom-edge row flips up fully in-window, a top row opens downward, click-outside dismisses, and selection commits both ways (`accessibleValue` round-trip). `ComboBoxBase` is not exported from std-widgets, so selection/keyboard is re-implemented (arrow/enter/escape via the overlay's FocusScope). Remove `select.slint` and revert the panes to stock `ComboBox` when the upstream native-popup fix lands.
- **Not yet migrated:** `tvs_pane`, `pairing_wizard`, `schema_form` still use stock `ComboBox`. Migrate them to `ThemedSelect` if their selectors start clipping, or leave them until the upstream fix.
- **Qt backend (rejected).** Slint's Qt backend renders real popup windows and sidesteps this entirely — but adopting it swaps the whole rendering path and pulls a Qt dependency, disproportionate here.

### Re-evaluation triggers

- winit#4543 merges → watch slint#11740 to consume it, then bump Slint (coordinate with the exact-version pin — see [Slint `unstable-winit-030` feature dependency](slint-unstable-winit-030.md)). Wayland/KDE is covered by #4543; X11 users would still wait.
- A Slint release closes [#2375](https://github.com/slint-ui/slint/issues/2375) or ships #11740 → drop this concern, delete `select.slint`, revert the profiles pane to stock `ComboBox`, and re-verify the "On DPMS on" dropdown at the window's bottom edge.
- A stock-`ComboBox` selector in `tvs_pane` / `pairing_wizard` / `schema_form` starts clipping → migrate that pane to `ThemedSelect`.
- If worth chasing upstream: file a focused repro against #2375 showing #6434's `place_popup` clamp not repositioning a `ComboBox` popup on the winit-Wayland ChildWindow path (with the geometry captured here).

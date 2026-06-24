# Contextual help (GUI) — KDE-style "i" toggletip + hover hints

`tisvlay-gui`'s in-window help system: an "i" **toggletip** (click/tap, plus hover-linger) for longer per-option help, and **hover tips** on icon-only buttons, links, and chips. All drawn by a single custom overlay at the window root — not Slint's built-in `Tooltip` (see [Why a custom overlay](#why-a-custom-overlay)).

## Model (KDE HIG)

KDE ranks help as: inline helper text → an **"i" `ContextualHelpButton`** (click/tap toggletip, used all over Plasma KCMs) → a hover tooltip as a last resort — *"don't put truly important text in a hover tooltip"* (touch-hostile). So the model is: inline helper text for short essentials (the Settings pane), an **"i" toggletip** for longer/per-option help, and **hover tips** on icon-only controls. Terminology: the click-"i"-reveals pattern is a **toggletip** (touch-accessible); a **tooltip** is hover/focus-only and transient.

## Architecture

The bubble can't be a child of its trigger: detail panes live in a clipping `ScrollView`, and placement is against the window/scroll bounds. So it renders **once at the window root**, fed by a global.

### `Help` global (`ui/globals/help.slint`)

Holds the single active request: `text`, `anchor` (window-space geometry), `clip` (the originating ScrollView's viewport rect), `owner` id, `pinned`, and hover bookkeeping (`requester-hovered`, `bubble-hovered`). `shown = text != "" && (pinned || requester-hovered || bubble-hovered)` — the KDE **two-source open model** (a click latch ORed with a transient hover, so a click never closes what a hover opened). `begin-request()` resets per-request state (incl. `clip`, and bumps `reposition-tick` so an in-scroll bubble re-clips even on a pin→pin handoff with no `shown`-transition); `hover-open`/`hover-close`/`toggle`/`dismiss` are the mutators; `set-clip-if-inside()` confines the bubble to a ScrollView only when the anchor lies inside it.

### `HelpOverlay` (rendered once in `main.slint`)

Fills the window. Places `HelpBubble` **centered over the anchor**, flipped below when there's no room above, clamped to the **clip region** (the ScrollView viewport when set, else the window) so a scrolled-away anchor pins to the scroll edge, not over the header/sidebar. **No scrim**: a Slint `TouchArea` can't catch clicks while passing hover through (see [slint-gotchas](../concerns/slint-gotchas.md)), so the overlay intercepts only the bubble itself and the cursor stays live everywhere.

### `HelpBubble`

**Shrink-to-fit width**: `min(col.preferred-width, max)` — Slint reports a word-wrap `Text`'s `preferred-width` as its NoWrap natural width, so the box hugs the widest line and only wraps at the cap (the native equivalent of Breeze's hidden 1-line measuring `Text`). Drop shadow via an outer inset rect.

### `HelpScrollView`

Drop-in `ScrollView` for a scrollable pane body: pins `viewport-width`, bumps `reposition-tick` on scroll, and pushes its post-layout viewport rect to `Help` (via `push-clip()`/`set-clip-if-inside`) so bubbles anchored inside it clamp to the viewport. Used by the TVs / Profiles detail bodies and the About pane.

### Triggers (`ui/widgets.slint`) — push model

A shared **`HelpTrigger`** carries the id / live-anchor / reposition-repush / hover-`Timer` + `set-hover(bool)`, embedded by every host so adding help is a one-liner:

- **`ContextualHelpButton`** — the "i" glyph (ghost `IconBtn` look; `Icons.info` = `help-contextual-symbolic` with fallbacks). Click pins + focuses; hover-linger (700 ms) opens transiently. a11y: `accessible-description` carries the help copy so screen-reader users who can't hover still hear it.
- **`tip` slots** on `IconBtn` / `UriBtn` / `Chip` / `LinkText` (hover-only) and the bespoke hosts (the Reload button, the pair/create "+" buttons, the About brand area). Each clears its tip on click **and on going non-interactive** — a destroyed (`if`-gated) or disabled trigger never emits `has-hover = false`, which would otherwise strand the bubble over a dead control.

### Dismissal — focus-out (KDE's `onActiveFocusChanged`), no scrim

A pin focuses a `FocusScope` on the button; losing that focus dismisses. Focus leaves on **Tab away**, **Escape**, **re-click**, **switch "i"**, **clicking another focusable control**, and — because the window's `forward-focus` targets a full-area `nav-scope` `FocusScope` (default `focus-on-click`) — **clicking empty background space anywhere in (or outside) the window**, which routes focus to `nav-scope` and off the button. So a click-anywhere dismiss falls out of the focus model with **no blocking scrim**. Plus a belt-and-suspenders **pane-change dismiss** (`changed active-pane → Help.dismiss()`) since navigating away leaves the owning button unable to fire its own dismiss.

### Anchor tracking on scroll/resize

Tracking can **not** observe `anchor` directly: a `changed anchor` handler reads `absolute-position` *during* layout and trips a reentrant layout-cache panic (see [slint-gotchas](../concerns/slint-gotchas.md)). Instead the ScrollView (`changed viewport-y`) and window (`changed width/height`) bump `Help.reposition-tick` (cheap, non-geometry); the owning trigger reads its `absolute-position` **only in that tick handler** (a post-layout event) and re-pushes `anchor`.

## i18n

`help:` / `tip:` strings are `@tr(...)`, with interpolation via `@tr("Edit {} config", name)` (template translated, data interpolated). A **`msgctxt` pass** disambiguates short/ambiguous labels (Slint's per-component default context + explicit `@tr("hint" => "…")` on polysemous atoms), and Rust-side profile-field labels use `pgettext`, so machine translation has per-component context. See [docs/i18n.md](../i18n.md) for the workflow.

## Why a custom overlay

Slint's built-in `Tooltip` and `PopupWindow` are both unfit for an anchored, wrapped, click-persistent bubble (pointer-jitter, no word-wrap, focus-grab lockup). That evaluation — and the other Slint footguns this system worked around (the `Timer.restart()` compiler ICE, the layout-cache panic, the `TouchArea` hover/click coupling) — lives in [slint-gotchas](../concerns/slint-gotchas.md).

## Slint dependency

The help overlay needs no specific Slint feature, but the branch landed it on Slint 1.17.0; the exact-pin rationale and the `system-tray`/`ksni` decoupling are in [slint-unstable-winit-030](../concerns/slint-unstable-winit-030.md).

## Rollout

Help / hover tips live on the non-obvious controls across all panes: TVs + Profiles detail rows ("i" toggletips), per-field reset/rename/delete/duplicate glyphs, the Reload + pair/create "+" buttons, the About pane (Report issue / View notices / version chip / Diagnostics / brand "View source"), and the Profiles "Used by" TV links. The controls themselves are the source of truth — grep `help:` / `tip:` in `ui/`.

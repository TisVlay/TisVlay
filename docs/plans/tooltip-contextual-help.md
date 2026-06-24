# Contextual help — KDE-style "i" toggletip + hover hints (as-built)

> **Status: as-built / merged.** Began as a plan to add Slint 1.17's native
> `Tooltip`; implementation + QA forced a pivot to a custom in-window **overlay**
> (a `Help` global + one `HelpOverlay` at the window root), then a full rollout
> across every pane and two `/code-review max` hardening passes. This documents
> what shipped and why. The native-`Tooltip` / `PopupWindow` dead-ends are kept
> in **Evolution** at the bottom — useful upstream feedback and the rationale for
> the overlay. This also consolidates the earlier Slint 1.17 migration plan (now
> obsolete — see Slint dependency below).

## Context

KDE HIG ranks help as: inline helper text → an **"i" `ContextualHelpButton`**
(click/tap toggletip, all over Plasma KCMs) → a hover tooltip as a last resort,
and *"don't put truly important text in a hover tooltip"* (touch-hostile).
Terminology: the click-"i"-reveals pattern is a **toggletip** (touch-accessible);
a **tooltip** is hover/focus-only and transient.

**Model:** inline helper text for short essentials (the Settings pane already did
this) + an **"i" toggletip** for longer / per-option help + **hover tips** on
icon-only buttons, links, and chips. All powered by a custom bubble (not Slint's
built-in `Tooltip`), drawn once at the window root.

## Slint dependency

The native `Tooltip` was the original reason to move to 1.17 — but the overlay
needs no 1.17 feature, so the help system itself is version-neutral. The branch
still lands on **Slint 1.17.0** (the project wanted it for other 1.17 work), via
the upstream fix that unblocked it: 1.17's `SystemTrayIcon` made `i-slint-core`
pull `ksni` on a hardcoded `async-io` backend, colliding (Cargo feature
unification) with this app's separate tokio `ksni` tray. We filed and landed
**slint-ui/slint#12141** — gating the tray/ksni backend behind a `system-tray`
Cargo feature — so `tisvlay-gui` builds `default-features = false` without it and
Slint pulls no `ksni`. The slint family is exact-pinned (`=1.17.0`) per the
no-semver / `unstable-winit-030` audit rationale in
[`../concerns/slint-unstable-winit-030.md`](../concerns/slint-unstable-winit-030.md);
`rust-version` is `1.92` (let-chains). MRE for the ksni coupling:
<https://github.com/ruffsl/slint-ksni-tray-mre>.

## As-built architecture

The bubble can't be a child of the trigger: detail panes live in a clipping
`ScrollView`, and placement is against the window/scroll bounds. So the bubble
renders **once at the window root**, fed by a global.

### `Help` global (`ui/globals/help.slint`)
Holds the single active request: `text`, `anchor` (window-space geometry), `clip`
(the originating ScrollView's viewport rect), `owner` id, `pinned`, and the hover
bookkeeping (`requester-hovered`, `bubble-hovered`). `shown = text != "" &&
(pinned || requester-hovered || bubble-hovered)` — the KDE **two-source open
model** (a click latch ORed with a transient hover, so a click never closes what
a hover opened). `begin-request()` resets per-request state (incl. `clip`, and
bumps `reposition-tick` so an in-scroll bubble re-clips even on a pin→pin handoff
with no `shown`-transition); `hover-open`/`hover-close`/`toggle`/`dismiss` are the
mutators; `set-clip-if-inside()` confines the bubble to a ScrollView only when the
anchor lies inside it.

### `HelpOverlay` (rendered once in `main.slint`)
Fills the window. Places `HelpBubble` **centered over the anchor**, flipped below
when there's no room above, clamped to the **clip region** (the ScrollView
viewport when set, else the window) so a scrolled-away anchor pins to the scroll
edge, not over the header/sidebar. **No scrim**: a Slint `TouchArea` can't catch
clicks while passing hover through, so the overlay intercepts only the bubble
itself; the cursor stays live everywhere.

### `HelpBubble`
**Shrink-to-fit width**: `min(col.preferred-width, max)` — Slint reports a
word-wrap `Text`'s `preferred-width` as its NoWrap natural width, so the box hugs
the widest line and only wraps at the cap (the native equivalent of Breeze's
hidden 1-line measuring `Text`). Drop shadow via an outer inset rect.

### `HelpScrollView`
Drop-in `ScrollView` for a scrollable pane body: pins `viewport-width`, bumps
`reposition-tick` on scroll, and pushes its post-layout viewport rect to `Help`
(via `push-clip()`/`set-clip-if-inside`) so bubbles anchored inside it clamp to
the viewport. Used by the TVs / Profiles detail bodies and the About pane.

### Triggers (`ui/widgets.slint`) — push model, self-contained
A shared **`HelpTrigger`** carries the id / live-anchor / reposition-repush /
hover-`Timer` + `set-hover(bool)`, written once and embedded by every host so
adding help is a one-liner:
- **`ContextualHelpButton`** — the "i" glyph (ghost `IconBtn` look; `Icons.info` =
  `help-contextual-symbolic` w/ fallbacks). Click pins + focuses; hover-linger
  (700 ms) opens transiently. a11y: `accessible-description` carries the help copy
  so SR users who can't hover still hear it.
- **`tip` slots** on `IconBtn` / `UriBtn` / `Chip` / `LinkText` (hover-only) and
  the bespoke hosts (the Reload button, the pair/create "+" buttons, the About
  brand area). Each clears its tip on click and on going non-interactive — a
  destroyed (`if`-gated) or disabled trigger never emits `has-hover = false`,
  which would otherwise strand the bubble over a dead control.

### Dismissal — focus-out (KDE's `onActiveFocusChanged`), no scrim
A pin focuses a `FocusScope` on the button; losing it dismisses: **Tab away**,
**click another focusable control**, **Escape**, **re-click**, **switch "i"**.
Plus a **pane-change dismiss** (`changed active-pane → Help.dismiss()`) since
navigating away leaves the owning button unable to fire its own dismiss. Gap:
clicking truly non-focusable empty space won't dismiss a pinned bubble — rare in
a dense form; full click-anywhere would need the blocking scrim (mutually
exclusive with a live cursor).

### Anchor tracking on scroll/resize
Tracking can **not** observe `anchor` directly: a `changed anchor` handler reads
`absolute-position` *during* layout and trips a reentrant layout-cache panic
(`layout.rs:895`). Instead the ScrollView (`changed viewport-y`) and window
(`changed width/height`) bump `Help.reposition-tick` (cheap, non-geometry); the
owning trigger reads its `absolute-position` **only in that tick handler** (a
post-layout event) and re-pushes `anchor`.

## i18n
`help:` / `tip:` strings are `@tr(...)`, with interpolation via
`@tr("Edit {} config", name)` (template translated, data interpolated). A
**`msgctxt` pass** disambiguates short/ambiguous labels (Slint's per-component
default context + explicit `@tr("hint" => "…")` on polysemous atoms), and Rust
profile-field labels use `pgettext` — so machine translation has per-component
context. Run `just i18n-extract`; commit the gui `.pot`; revert the tray `.pot`
if only its timestamp changed.

## Rollout (merged)
Help / hover tips on the non-obvious controls across **all** panes: TVs +
Profiles detail rows ("i" toggletips), per-field reset/rename/delete/duplicate
glyphs, the Reload + pair/create "+" buttons, the About pane (Report issue / View
notices / version chip / Diagnostics / brand "View source", with external-link
glyphs sized to the label), and the Profiles "Used by" TV links.

## Verification
- `just check` green on **1.17.0** (fmt / build / clippy `-D warnings` / 559
  workspace tests / doc tests).
- **Layer-B tests** (`tests/contextual_help.rs`, `init_no_event_loop`): the
  `ContextualHelpButton` renders with its accessible-label, and click pins /
  re-click dismisses via the testing backend (the `about_pane` tests caught the
  anchor-tracking layout panic).
- Two `/code-review max` passes fixed: a missing pencil tip, a `bubble-hovered`
  strand, tall-bubble placement, stale-`owner` reset (round 1); and a
  tip-strands-on-destroy/disable dismissal gap, a pin→pin clip-reset gap, the
  bespoke-forwarder DRY (`set-hover`), About→`HelpScrollView`, and removal of dead
  `bullets` plumbing (round 2).

## Open follow-ups
- Empty-space click-away dismiss (needs a non-blocking outside-press signal Slint
  doesn't expose).
- Reconsider the pencil's double-affordance (✎ + ⓘ) density per row.

## Evolution (why the overlay, not native `Tooltip` / `PopupWindow`) — upstream feedback
- **Native `Tooltip` (1.17)** is pointer-anchored + mouse-tracking (jitter),
  can't wrap (`StyledText`), and clamps position but not size (edge crop). Fine
  for short cursor-following hints, unfit for an anchored, wrapped,
  click-persistent bubble.
- **`PopupWindow`** (second attempt) captures focus on show → hover-steal →
  flicker; can't read the window size → no center/flip placement; the focus-grab
  + no working dismiss made a clicked-open bubble *lock up the UI*. Its winit
  backend renders popups into the parent surface and clamps to the window anyway,
  so it offered no advantage over an in-window overlay for a small bubble.
- A `Timer.restart()` **ICEs the compiler** when the `Timer` is in an inlined
  reusable component (`lower_restart_timer` can't find it post-inline) — we drive
  `running` directly. Worth a separate upstream bug.
- The `SystemTrayIcon → ksni(async-io)` coupling — filed and **fixed** upstream
  (slint-ui/slint#12141, released in 1.17.0).

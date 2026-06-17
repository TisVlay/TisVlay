# Contextual help — KDE-style inline help + "i" toggletip (as-built)

> **Status: as-built.** This started as a plan to build the help popover on
> Slint's `PopupWindow`; implementation + QA forced a pivot to an in-window
> **overlay** (a `Help` global + a single `HelpOverlay` at the window root). The
> sections below describe what actually shipped and why each piece is the way it
> is. The original PopupWindow design and its dead-ends are kept in
> **Evolution** at the bottom — useful as upstream feedback on Slint's native
> `Tooltip` and as the rationale for the overlay.

## Context

The first attempt used Slint 1.17's built-in `Tooltip` (a "?" glyph hover-tip).
Three problems, all confirmed against Slint source:

- **Jitter / cursor occlusion** — the built-in `Tooltip` hard-binds its popup
  `x/y` to `mouse-x/mouse-y` and tracks the pointer. No element-anchor mode;
  delay/offset/placement not settable.
- **Edge cropping / no wrap** — text-mode uses `StyledText` (no `wrap`) → grows
  to the unwrapped width; the popup position clamps but the body isn't resized.
- **Wrong affordance** — a hover "?" glyph isn't the KDE convention. The KDE HIG
  ranks help as: inline helper text → **`ContextualHelpButton`** (an "i" glyph,
  click/tap to reveal, all over Plasma KCMs) → hover tooltip as a last resort,
  and *"don't put truly important text in a hover tooltip"* (touch-hostile).

Terminology (researched): the click-"i"-reveals pattern is a **toggletip**
(touch-accessible); a **tooltip** is hover/focus-only, transient.

**Decisions (confirmed with the user):**
- **Hybrid model**: inline helper text for short essentials (KDE's first choice;
  the Settings pane already does this), plus an **"i" toggletip** for longer /
  optional / per-option help.
- A **custom help bubble** (not the built-in `Tooltip`) powers both the "i"
  toggletip (click/tap + hover-linger) and icon-button hover hints.

**Scope (this PR): the primitive + the TVs pane only.** The pencil `IconBtn`
rework is the canonical single-callsite test of the hover path. Profiles and the
About-pane `IconBtn` hover hints are deferred to a follow-up — landing 4 surfaces
+ a 15-callsite `IconBtn` change at once is too large a blast radius. This PR
also stays on Slint **1.16.1**: the overlay (below) needs no 1.17 feature, so we
reverted the brief 1.17-pre git pin (see `slint-1.17-tooltip.md`).

---

## Affordance model + per-pane mockups

Rule of thumb: **short + always-useful → inline `helper:`**; **longer / optional
/ per-option → "i" toggletip**; **icon-only buttons → hover hint**.

### TVs detail (this PR) — compact rows → "i" toggletips + pencil hover
```
TVs ▸ Living Room G4                                ● Connected

 PROFILES
   System   [ default            ▾ ]  ✎ ⓘ
     ✎  hover ▸ "Edit system profile"     (anchored below the pencil; transient)
     ⓘ  click/linger ▸ "Drives daemon lifecycle (boot / shutdown / sleep)."

 PICTURE / AUDIO / INPUT
   Backlight  [====o------] 80           ⓘ
   Screen     ( ●)                       ⓘ
   Current    [ HDMI 1           ▾ ]     ⓘ   (bulleted: Current vs Primary)
   Primary    [ HDMI 2           ▾ ]     ⓘ

 DANGER ZONE
   Removes this TV from the daemon's config.        [ Remove TV ]
   Pairing must be redone.       ← inline, always-visible (destructive: never hover)
```

### Settings pane — already inline (`helper:`); unchanged this PR.
### Profiles / About panes — DEFERRED (follow-up).

---

## As-built architecture

The bubble can't be a child of the trigger: the TV detail lives in a clipping
`ScrollView`, and we need placement against the window/scroll bounds. So the
bubble renders **once at the window root**, fed by a global.

### `Help` global (`ui/globals/help.slint`)
Holds the single active request: `text`, `bullets`, `anchor` (window-space
geometry), `clip` (the originating ScrollView's viewport rect), `owner` id,
`pinned`, and the hover bookkeeping (`requester-hovered`, `bubble-hovered`).
`shown` = `text != "" && (pinned || requester-hovered || bubble-hovered)` — the
KDE **two-source open model** (a click latch ORed with a transient hover, so a
click never closes what a hover opened). `claim-id()` hands each trigger a stable
id; `hover-open`/`hover-close`/`toggle`/`dismiss` are the mutators.
`reposition-tick` (a plain int) is bumped on scroll/resize.

### `HelpOverlay` (`ui/globals/help.slint`, rendered once in `main.slint`)
Fills the window. Places `HelpBubble` **centered over the anchor**, flipped below
when there's no room above, and clamped to the **clip region** (the ScrollView
viewport when set, else the window) so a scrolled-away anchor pins the bubble to
the scroll edge — not over the header/sidebar. **No scrim**: a Slint `TouchArea`
can't catch clicks while passing hover through, so the overlay intercepts only
the bubble itself; the cursor stays live everywhere.

### `HelpBubble` (`ui/globals/help.slint`)
The card. **Shrink-to-fit width**: `min(col.preferred-width, max)` — Slint reports
a word-wrap `Text`'s `preferred-width` as its NoWrap natural width, so the box
hugs the widest line and only wraps once it hits the cap (the native equivalent
of Breeze's hidden 1-line measuring `Text`). Drop shadow via an outer inset rect
(popups/overlays clip at their own bounds).

### Triggers (`ui/widgets.slint`) — self-contained
- **`ContextualHelpButton`** — the "i" glyph (ghost `IconBtn` look: squarish,
  no resting border, dim 14px glyph, blue `info` tint on hover/engage; `Icons.info`
  = `help-contextual-symbolic` w/ fallbacks). Click pins + focuses; hover-linger
  (700ms `Timer`) opens transiently. `accessible-role: button`, `accessible-label`
  the name, **`accessible-description` the help copy** (KDE binds this so SR users
  who can't hover still hear it).
- **`IconBtn.tip`** — hover-only hint; reports to `Help` on hover-linger, click
  stays the button's action.
Both report their `absolute-position` to `Help`; neither renders the bubble.

### Dismissal — focus-out (KDE's `onActiveFocusChanged`), no scrim
A pin focuses a `FocusScope` on the button; losing that focus dismisses:
**Tab away**, **clicking another focusable control**, **Escape**, **re-click**,
**switch to another "i"**. Plus a **pane-change dismiss** (`changed active-pane`
→ `Help.dismiss()`) because navigating away leaves the owning button unable to
fire its own dismiss (and sidebar items aren't focusable). Gap: clicking truly
non-focusable empty space doesn't clear focus, so it won't dismiss — rare in a
dense form, and full click-anywhere would need the blocking scrim (mutually
exclusive with a live cursor).

### Anchor tracking on scroll/resize
The bubble follows its anchor. Tracking can **not** observe `anchor` directly: a
`changed anchor` handler evaluates `absolute-position` *during* layout and trips
a reentrant layout-cache panic (`layout.rs:895`). Instead the ScrollView
(`changed viewport-y`) and window (`changed width/height`) bump `Help.reposition-tick`
(cheap, non-geometry); the owning button reads its `absolute-position` **only in
that tick handler** (a post-layout event) and re-pushes `anchor`. The ScrollView
likewise pushes its viewport rect to `Help.clip` on show + tick.

---

## i18n
`help:` / `tip:` / bullet strings are `@tr(...)`; one `@tr` per bullet. Run
`just i18n-extract`; commit the gui `.pot`; revert the tray `.pot` if only its
timestamp changed. No Slint-internal citations / version anchors in `.slint`
comments — behavior + rationale only.

## Verification
1. `just check` (compiles `.slint`; misuse fails the build) — 559 tests green
   on **1.16.1**.
2. **Layer-B tests** (`tests/contextual_help.rs`, `init_no_event_loop`): the
   `ContextualHelpButton` renders with its accessible-label, and click pins /
   re-click dismisses via the testing backend's accessible action. (The
   `about_pane` tests are what caught the anchor-tracking layout panic.)
3. Manual on KDE (`gui-start`): click + hover-linger open; re-click / Tab /
   Escape / click-another-control dismiss; cursor stays live over "i"s while
   pinned; bubble hugs text, wraps at the cap, follows on scroll/resize, and
   clamps to the ScrollView edge; pane-nav clears it.

## Open follow-ups
- Roll the hybrid across Profiles ("i" on inherit/reset) and the About-pane
  `IconBtn` hover hints — one `help:`/`tip:` per control. **Do the idiomatic /
  reuse cleanup (a `/code-review` pass) before this rollout.**
- Reconsider the pencil's double-affordance (✎ + ⓘ) density per row.
- Empty-space click-away dismiss (needs a non-blocking outside-press signal).

## Evolution (why the overlay, not `PopupWindow`) — upstream feedback
The native `Tooltip` and then `PopupWindow` were both dead-ends; the findings are
worth feeding back to Slint, and our overlay is a candidate to upstream as a
non-pointer-anchored, dismissable help-popover primitive:

- **Native `Tooltip`** is pointer-anchored + mouse-tracking (jitter), can't wrap
  (StyledText), and clamps position but not size (edge crop). Fine for short
  cursor-following hints, unfit for an anchored, wrapped, click-persistent bubble.
- **`PopupWindow`** (our second attempt) captures focus on show → hover-steal →
  flicker; can't read the window size → no center/flip placement; and the
  focus-grab + no working dismiss made a clicked-open bubble *lock up the UI*.
  Its winit backend renders popups into the parent surface and clamps to the
  window anyway, so it offered no advantage over an in-window overlay for a small
  bubble.
- A `Timer.restart()` **ICEs the compiler** when the `Timer` is in an inlined
  reusable component (`lower_restart_timer` can't find it post-inline) — we drive
  `running` directly instead. Worth a separate upstream bug.

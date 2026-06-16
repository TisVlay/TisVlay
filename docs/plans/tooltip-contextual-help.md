# Tooltip UX redesign — KDE-style inline help + "i" toggletip (anchored popover)

## Context

Phase B shipped Slint 1.17's built-in `Tooltip` (a "?" glyph hover-tip). Three
problems, all confirmed against Slint source:

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
- **Custom anchored popover** (not Slint's built-in `Tooltip`) powers both the
  "i" toggletip (click/tap + hover-linger) and icon-button hover hints. Anchored
  to the trigger → no jitter; bounded width + word-wrap → no cropping;
  `PopupWindow` edge-clamps → stays on-screen.

This reworks the Phase B proof (the "?" `InfoTip` and built-in-`Tooltip` `tip`
props are replaced).

**Scope (this PR): the primitive + the TVs pane only.** The pencil `IconBtn`
rework is the canonical single-callsite test of the hover path. Profiles and the
About-pane `IconBtn` hover hints are deferred to a follow-up (see end) — landing
4 surfaces + a 15-callsite `IconBtn` change at once is too large a blast radius.

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

 PICTURE
   Backlight  [====o------] 80           ⓘ
   Screen     ( ●)                       ⓘ
     ⓘ Screen ▸ "Blanks the panel without powering off (OLED-safe, instant
                 resume)."

 INPUT
   Current    [ HDMI 1           ▾ ]     ⓘ   ← net-new (row had no tip before)
   Primary    [ HDMI 2           ▾ ]     ⓘ
     ⓘ Current ▸  • HDMI 1 — set-top box
                  • HDMI 2 — this PC
                  • HDMI 3 — console

 DANGER ZONE
   Removes this TV from the daemon's config.        [ Remove TV ]
   Pairing must be redone.       ← inline, always-visible (destructive: never hover)
```

### Settings pane — already inline (`helper:`); unchanged this PR
```
 Notify me about
   TV connection      ( ●)
     Notify when a TV connects or disconnects.       ← inline helper (unchanged)
```

### Profiles pane — DEFERRED (follow-up): "i" on inherit/reset semantics.

---

## Primitive design (`crates/tisvlay-gui/ui/widgets.slint`)

House-style rule: **components are self-contained** (own their `TouchArea`,
expose `in` props + a `clicked()`), like `Chip`/`IconBtn`/`DangerBtn`. So the
hover-timer + dismiss + Escape logic is written **once, inside the widgets** —
never handed to call sites.

### `HelpPopover` — **private (non-exported)** anchored bubble
The shared internals; only `ContextualHelpButton` and `IconBtn` instantiate it.
- Inherits `Rectangle`, **sizes tightly to its trigger** (`@children`,
  `horizontal/vertical-stretch: 0`) so the popup anchor `y: root.height + 4px`
  lands just below the glyph, not below a larger wrapper.
- Owns a `PopupWindow`:
  - **Anchor**: `x: 0; y: root.height + 4px;` — PopupWindow x/y are an offset
    from the declaring element's origin (`core/window.rs:1726`); no mouse
    binding. Edge-clamp is automatic (`core/window/popup.rs:19-41`).
  - **Width/wrap**: the **`PopupWindow` root** carries the explicit
    `width: max-content-width` (~440px, just under KDE's 450px line-length cap)
    — the popup reads its *root* WindowItem width (`core/window.rs:1745-1763`),
    so the bound width must be on the PopupWindow, not only an inner rect. Inside:
    `Text { wrap: word-wrap; width: 100%; }`. Optional `in property <[string]>
    bullets` → a `for` of `•`-prefixed wrapped rows.
  - **Drop shadow**: outer transparent padding rect carries `drop-shadow-*`,
    inner bordered rect is the bubble (winit renders popups into the parent
    surface with redraw region == root geometry and no shadow padding, so an
    edge shadow leaves artifacts). Mirror the `Toast` chrome (pattern analogy;
    Toast is a Rectangle overlay, not a PopupWindow).
  - **close-policy: `no-auto-close`** (constexpr) + **manual dismiss** — a
    toggletip's "i" click is an *outside-popup* click that races
    `close-on-click-outside`; `no-auto-close` makes toggle deterministic. Under
    `no-auto-close`, **Escape is NOT automatic** → wire it ourselves via a
    `FocusScope` key handler (mandatory, not optional).

### `ContextualHelpButton` — exported, self-contained "i" toggletip
KDE-faithful **two-source open model** (verified against Kirigami
`ContextualHelpButton.qml`): a latched `pinned: bool` (click toggles, opens
**instantly**) ORed with a transient hover contribution (opens after the KDE
delay). The popup shows if `pinned || hovering`; the click handler toggles
`pinned` only and never consults hover state (this is what avoids the
"hover-opens-then-click-closes" bug). Keep-open-while-hovering-the-bubble is
gated on `!pinned`.
- 18px circle, `Icons.info` glyph, **button-like hover background** (so it reads
  as interactive, like a KDE ToolButton), `mouse-cursor: help`.
- Hover delay **700ms** (KDE `Units.toolTipDelay`); click **0ms** (instant).
- a11y: `accessible-role: button`; `accessible-label` = the *name* (e.g.
  "Show help"); **`accessible-description` = the help text** (KDE binds
  `Accessible.description` so a screen-reader user who can't hover still hears
  it). Keyboard: Enter/Space toggles, Escape closes. (The "don't use
  aria-describedby" guidance is about the popup's live-region semantics, not the
  trigger's description.)

### `IconBtn.tip` — exported, self-contained hover hint (rework)
`IconBtn` already exposes `in property <string> tip` (Phase B fed it a built-in
`Tooltip`). Replace that block with an internal `HelpPopover` in **hover-only**
mode (no `pinned`/click latch — click stays the button's own action). Hover
opens after 700ms below the button; pointer-leave closes. Keep the existing
`accessible-label` (so the action name exists for SR users without hover).

### Icons
- `globals/icons.slint`: add `in property <image> info;`.
- `src/lib.rs` (~L374): `icons.set_info(load_themed_icon(&["help-contextual-symbolic", "help-contextual", "documentinfo", "dialog-information"]));`
  (lead with KDE's own name; `dialog-information` is a *different, busier* glyph
  — last-resort only.)

### `FormRow` (tvs_pane.slint, ~L99) — hybrid slots
Keep `helper:` (inline). **Drop** the Phase-B `tip` prop (which rendered an
`InfoTip` by the label). Add `help:` (string) + optional `help-bullets:` →
renders a trailing `ContextualHelpButton` in the control row (right of the
control).

### Removed (prune fully)
- `InfoTip` component + its `tvs_pane.slint` L19 **import** + both built-in
  `Tooltip { }` blocks + all `@markdown("\{…}")` tip plumbing. Don't carry over
  `InfoTip`'s `AccessibleRole.text`-shadow comment.

---

## Known limitations (carry into implementation)
- **Focus steal during editing**: a user `PopupWindow` takes focus on open (only
  compiler-lowered Tooltips skip it). A hover-open over a pencil while a
  `LineEditField` is focused fires `FocusOut` on that input. **Gate hover
  popovers from opening while a text input is focused**, or accept the bounce —
  decide at impl; don't leave it implicit.
- **Timer first-fire**: `Timer { running: false }` then `restart()` on
  hover-enter — verify the *first* hover fires; if not, use `start()` (or set
  `running: true` in the handler). Read `is-open` directly, never a cached bool.
- **One popover at a time** (sibling auto-close) — matches KDE; desired.
- **Over a modal scrim**: `ConfirmDialog`/`NameInputDialog` are plain-rect
  scrims; a PopupWindow floats above them. Gate help triggers `enabled:
  !modal-open` in panes that can show a modal.

## Migration from committed Phase B
- Delete `InfoTip` + prune its import (`tvs_pane.slint:19`). Rework `IconBtn`
  (`tip` → internal hover `HelpPopover`) and `FormRow` (`tip` → `help:`). Re-map
  the proof rows per the mockup: Screen / Backlight → `ContextualHelpButton`
  `help:`; Primary keeps its copy; Current gains a *new* bulleted `help:`; the
  pencil keeps a hover `tip:`.
- Verify no `@markdown` or `Tooltip` remains anywhere after the edits.

## i18n
- `help:` / `tip:` / bullet strings are `@tr(...)`. Bulleted option help: one
  `@tr` per bullet (our choice, for independent translatability — not a
  CLAUDE.md rule). Run `just i18n-extract`; commit the gui `.pot` diff; revert
  the tray `.pot` if only its timestamp changed. Expect `i18n-extract` to
  **drop** any msgid whose call-site wording changed during the re-map — that's
  the intended diff, not churn.
- Do **not** leak Slint-internal citations / version anchors into `.slint`
  comments — those belong in the commit message. Banner comments describe
  behavior + rationale only (match `Toast`/`FormRow`).

## Verification
1. `nix develop -c just check` (compiles `.slint`).
2. **Layer-B test** (mirror `tests/about_pane.rs` `init_no_event_loop` +
   element-query): assert the `ContextualHelpButton` renders with
   `accessible-role: button` and its `accessible-label`/`description`. Static
   accessibility contract lands *with* the widget (don't defer again). The
   timer/Escape/open-close interaction is verified manually (no event loop in
   the test backend).
3. Manual on KDE (`gui-start` / MCP): "i" opens on click AND hover-linger;
   closes on re-click / Escape / pointer-leave; **no jitter**. Pencil hover
   hint below the button, dismiss on leave. Long/bulleted help **wraps**; an "i"
   near the window's right/bottom edge **stays fully visible**; shadow not
   clipped. Tap (touchpad) opens the toggletip. Focus/Escape behave; no bubble
   under a `ConfirmDialog` scrim.

## Open follow-ups (out of scope)
- Roll the hybrid across Profiles (the "i" on inherit/reset) and the About-pane
  `IconBtn` hover hints — one `help:`/`tip:` per control.
- Reconsider the pencil's double-affordance (✎ hover hint + adjacent ⓘ) density
  per row if it reads as cluttered.

## Critical files
- `crates/tisvlay-gui/ui/widgets.slint` — private `HelpPopover`; exported
  `ContextualHelpButton`; `IconBtn` rework; remove `InfoTip` + built-in `Tooltip`
- `crates/tisvlay-gui/ui/components/tvs_pane.slint` — `FormRow` `help:` slot,
  re-map proof rows, prune `InfoTip` import, Danger Zone inline kept
- `crates/tisvlay-gui/ui/globals/icons.slint` — `info` image slot
- `crates/tisvlay-gui/src/lib.rs` — load the "i" icon (~L374)
- `crates/tisvlay-gui/ui/globals/toast.slint` — drop-shadow-on-overlay reference
- `crates/tisvlay-gui/tests/` — Layer-B `ContextualHelpButton` a11y test (mirror
  `about_pane.rs`)
- `crates/tisvlay-gui/lang/tisvlay-gui.pot` — regenerated

# Pairing Wizard — Motion Specification

This document inventories every animation in the tisvlay pairing wizard and gives
each one a name, an intent, and the implementation values needed to reproduce it
faithfully in the Slint port (or any other UI toolkit).

The motion is grouped by **intent**, not by mechanic — animations that share an
intent should also share duration, easing, and amplitude so the wizard reads as
one coherent surface.

---

## Motion system at a glance

| Group | Members | Intent |
|---|---|---|
| Liveness signals | `tv-pulse` | "The system is awake and listening." |
| Indeterminate progress | `tv-spin`, `tv-scan` | "Work is happening, duration unknown." |
| Content choreography | `fadein-row` | "The data set just changed." |
| State feedback | `.15s` property transitions | "Your input was received." |

A handful of moments **deliberately don't move:** the stepper progress bar, the
success and failure icons on Step 3, and the Step 4 confirmation. Stillness on
resolution states is its own design choice — the absence of motion is what tells
the user "we've landed, you can read this now." Do not add a checkmark draw-on
or a confetti burst here.

---

## 1. `tv-pulse` — radar pulse

**Where**
- Step 1, the dot next to "Scanning via SSDP — found 2…"
- Step 3 (Pair Prompting), the dot next to "Waiting for response…"

**Pattern names**
*Radar ping* · *sonar pulse* · *ripple indicator*

**Category**
Ambient *liveness* / *heartbeat* signal — communicates "this process is ongoing"
without consuming attention. It should sit in the user's peripheral vision.

**Mechanics**

```css
@keyframes tv-pulse {
  0%   { transform: scale(0.6); opacity: 0.7; }
  100% { transform: scale(2.4); opacity: 0; }
}
/* applied to a colored disc behind the solid dot */
animation: tv-pulse 1.4s ease-out infinite;
```

| Property | Value |
|---|---|
| Duration | `1.4s` |
| Easing | `ease-out` |
| Iteration | `infinite` |
| Delay | `0s` |
| Loop type | Seamless infinite loop (no pause between cycles) |

**Easing rationale**
Ease-out, because the energy radiates *outward* and decays. Linear would feel
mechanical; ease-in would feel like the ring is collapsing inward.

---

## 2. `tv-spin` — indeterminate spinner

**Where**
- Step 1 empty state, around the TV glyph
- Step 3 "Registering…" card

**Pattern names**
*Indeterminate spinner* · *throbber* · *activity indicator*

**Category**
Indeterminate progress — used when duration is unknown and you need to occupy
the user's patience without lying about how long it'll take.

**Mechanics**

```css
@keyframes tv-spin {
  to { transform: rotate(360deg); }
}
/* applied to a circle with a transparent border and a single colored top border */
animation: tv-spin 0.9s linear infinite;
```

| Property | Value |
|---|---|
| Duration | `0.9s` per full revolution |
| Easing | `linear` (mandatory) |
| Iteration | `infinite` |

**Easing rationale**
Linear is mandatory — any easing creates a perceived "stutter" at the loop
seam. ~1s per revolution is the platform-standard cadence; faster reads as
"anxious," slower as "stalled."

---

## 3. `tv-scan` — sweeping shimmer bar

**Where**
- Step 1, under the discovery list
- Step 1, inside the empty-state card
- Step 3, inside the "Registering…" card

**Pattern names**
*Scan line* · *sweep bar* · *progress shimmer* (the broader category is
sometimes called *skeleton shimmer* when used over content placeholders).

**Category**
Indeterminate progress — but specifically the *spatial / linear* variant. The
spinner says "something is happening"; the scan bar says "something is being
**traversed**."

**Mechanics**

```css
@keyframes tv-scan {
  0%   { left: -40%; }
  100% { left: 100%; }
}
/* a 40%-wide gradient sliver that translates across a relatively-positioned track */
.scan-bar {
  position: absolute; top: 0; left: -40%;
  width: 40%; height: 100%;
  background: linear-gradient(90deg, transparent, var(--accent), transparent);
  animation: tv-scan 1.6s ease-in-out infinite;
}
```

| Property | Value |
|---|---|
| Duration | `1.6s` |
| Easing | `ease-in-out` |
| Iteration | `infinite` |
| Sliver width | `40%` of track |
| Travel | `left: -40%` → `left: 100%` |
| Gradient | `transparent → accent → transparent`, horizontal |

**Easing rationale**
Ease-in-out gives the bar a slight pause at each end of its travel, which reads
more like a deliberate sweep than a frantic loop.

**Visual reference**
Evokes a flatbed scanner, an oscilloscope sweep, or `nmap`-style port scans —
appropriate semantically because the system is actually scanning the network.

---

## 4. `fadein-row` — staged list entrance

**Where**
Step 1, the third TV in the discovery list ("Guest Bravia, fading in…").

**Pattern names**
*Fade-and-rise* · *slide-up entrance* · *enter transition*

**Category**
*List mutation* / *enter animation* — a member of the broader *choreographed
list* family (alongside reorder, exit, and crossfade transitions).

**Mechanics**

```css
@keyframes fadein-row {
  from { opacity: 0;    transform: translateY(4px); }
  to   { opacity: 0.55; transform: translateY(2px); }
}
```

| Property | Value |
|---|---|
| Opacity | `0` → `0.55` (intentionally not 1) |
| Transform | `translateY(4px)` → `translateY(2px)` |
| Easing | `ease-out` |

**Direction convention**
Upward motion + fade-in is the standard "this content just became available"
gesture. Downward motion would read as "dismissed."

**Why it stops at 55% opacity**
This is a *staged entrance* / *two-phase enter*. The row first announces itself
at low opacity, then resolves to full opacity once the user acknowledges it
(e.g. on hover, on selection, or after a settle delay). Do not "fix" this to
end at `opacity: 1` — the partial-opacity end state is the design.

---

## 5. State-change transitions (the unsung 90%)

The `transition: all .15s` rules are sub-200ms feedback that confirms an input
was received. They're easy to overlook, but they're what makes the wizard feel
like a native app rather than a stack of static frames.

| Where | What changes | Vocabulary |
|---|---|---|
| Stepper pills (1 → 2 → 3 → 4) | bg, text color, border color on becoming `active` / `done` | *state transition* / property tween on color tokens |
| Discovery rows | bg + border on hover/selection | *hover affordance* / *selection feedback* |
| "Add manually" disclosure caret | `rotate(0deg)` → `rotate(90deg)` | *disclosure rotation* / *chevron flip* |

**Conventions for this family**

| Property | Value |
|---|---|
| Duration | `150ms` (Breeze/Plasma de-facto standard) |
| Easing (color tweens) | `ease` (default) |
| Easing (transforms) | `ease-out` (specify explicitly) |

`≤100ms` reads as instant; `≥250ms` reads as sluggish for hover feedback. Stay
in the 120–180ms band for any new state-feedback transition.

---

## Porting notes for Slint

Slint 1.16's animation primitives map cleanly onto the above:

- `animate <property> { duration: 150ms; easing: ease-out; }` covers the entire
  "state feedback" group.
- For `tv-pulse`, `tv-spin`, and `tv-scan`, use a `Timer` driving an animated
  property bound to `transform` / `opacity` / `x`. Slint has no built-in
  `infinite` keyword on `animate {}` blocks; the canonical pattern is a Timer
  that flips a state property and lets `animate` interpolate between values.
- For `fadein-row`, gate the row's `opacity` and `y` on a `states {}` block
  triggered by `discovered := true`.

Whatever you do, keep the **durations and easings above as the source of
truth.** If a Slint constraint forces a change (e.g. you can't get
`ease-in-out` on a Timer-driven loop), document the deviation here rather than
silently drifting.

---

## Reduced-motion behavior

Respect `prefers-reduced-motion: reduce` (or the equivalent KDE/GNOME
accessibility flag):

| Animation | Reduced-motion fallback |
|---|---|
| `tv-pulse` | Replace with a static dot at full opacity. |
| `tv-spin` | Replace with a static "Loading…" label or a 3-dot ellipsis that fades the dots in sequence at ≥500ms intervals. |
| `tv-scan` | Hide entirely. The spinner already covers the indeterminate-progress role. |
| `fadein-row` | Set the row to its end state instantly, no transform. |
| `.15s` transitions | Keep — they're under the 200ms threshold most reduced-motion specs flag, and removing them makes the UI feel broken. |

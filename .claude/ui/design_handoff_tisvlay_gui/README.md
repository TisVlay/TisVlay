# Handoff: tisvlay GUI

## Overview

This bundle contains the design for **tisvlay-gui**, the Slint-based settings + management window for tisvlay (multi-vendor smart-TV control daemon for Linux desktops). It covers:

- **App shell** — sidebar navigation (TVs / Profiles / Session / About), titlebar, status footer (Daemon / Session presence indicators), light + dark themes.
- **TVs pane** — master/detail (TV list left, configuration detail right). Replaces the current placeholder/partial implementation in `crates/tisvlay-gui/ui/components/tvs_pane.slint`.
- **Pairing wizard** — modal dialog (~600×520) that takes over from the TVs pane when the `+` button in the list header is clicked. 4 steps (Discover → Configure → Pair → Done) with full state coverage (empty/streaming/error in Discover, four phases in Pair).
- **Profiles pane** — system + user profile editor with explicit inheritance from each type's `default` profile (M4 placeholder filled in).
- **Session pane** — left as M5 placeholder, polished message only (per user instruction).
- **About pane** — polished version of existing pane.

## About the Design Files

The HTML/JSX files in this bundle are **design references**, not production code. They are React + inline-styles prototypes I used to compose the look/feel/behavior. The real implementation target is **Slint 1.16** in `crates/tisvlay-gui/ui/`, using the data model and callbacks already wired in `crates/tisvlay-gui/src/main.rs` and `crates/tisvlay-gui/src/state.rs` (both included in `uploads/` for reference, and described below).

**Your job is to recreate these designs in Slint** using stock `std-widgets` (Button, ComboBox, Switch, Slider, CheckBox, ScrollView, ListView, GroupBox, LineEdit). No exotic animations, no custom shaders. The existing `main.slint` / `tvs_pane.slint` structure already maps cleanly to what's pictured — extend it, don't rewrite the Rust side.

## Fidelity

**High-fidelity.** Final colors, typography, spacing, hover/selected states, and copy are all production-ready. Pixel-perfect recreation in Slint is the goal.

The design is intentionally neutral — Breeze/Plasma-friendly but renders cleanly on GNOME/XFCE. No platform-specific chrome.

## How to view

Open these HTML files in a browser (or the project preview):

| File | What it is |
|------|-----------|
| `tisvlay GUI.html` | **Interactive prototype** — full clickable app with theme toggle, all panes, working pairing wizard takeover, mock state. The single most useful reference for behavior. |
| `tisvlay GUI Gallery.html` | **Static gallery** — every pane × every state × light/dark side-by-side on a design canvas. Use this for visual handoff and to compare states. |
| `Pairing Wizard.html` | The pairing wizard alone, all states + accessibility notes. |

---

## Design Tokens

Defined in `app-shell.jsx` as `APP_T.light` / `APP_T.dark`. Use these verbatim in Slint (Slint accepts `#rrggbb`).

### Color — Light

| Token | Hex | Usage |
|---|---|---|
| `bg` | `#fbfbfc` | Window background fallback |
| `surface` | `#ffffff` | Card/list/detail surface |
| `surface2` | `#f4f5f7` | Footer, group-box header band |
| `surfaceAlt` | `#f9fafb` | Sidebar-of-pane background (TV list) |
| `sidebar` | `#1b1e23` | Main app sidebar bg |
| `sidebarText` | `#cfd4dc` | Default sidebar text |
| `sidebarTextDim` | `#8a909a` | Secondary sidebar text |
| `sidebarSelected` | `#3daee9` | Active sidebar item bg |
| `sidebarHover` | `#262a33` | Hovered sidebar item bg |
| `border` | `#dcdfe4` | Default 1px borders |
| `borderStrong` | `#c2c7d0` | Inputs, selects, button outlines |
| `text` | `#1a1d22` | Primary text (16.4:1 on white) |
| `textDim` | `#5b626d` | Helper text (6.1:1) |
| `textFaint` | `#8a909a` | Timestamps, metadata |
| `accent` | `#3daee9` | Breeze blue — primary actions, focus, selection |
| `accentSoft` | `#e3f3fc` | Selection backgrounds |
| `success` / `successSoft` / `successText` | `#10b981` / `#d1fae5` / `#047857` | Connected, paired |
| `danger` / `dangerSoft` / `dangerText` | `#dc2626` / `#fee2e2` / `#b91c1c` | Disconnected, errors |
| `warn` / `warnSoft` / `warnText` | `#d97706` / `#fef3c7` / `#92400e` | Unmanaged, attention |
| `chip` / `chipText` / `chipBorder` | `#eef0f3` / `#374151` / `#dcdfe4` | Inline tags (vendor, profile, etc.) |

### Color — Dark

Mirror set; key swaps:

| Token | Hex |
|---|---|
| `bg` | `#1b1e23` |
| `surface` | `#232730` |
| `surface2` | `#2b2f39` |
| `border` | `#383d48` |
| `text` | `#eef0f3` |
| `textDim` | `#a4abb6` |
| `accent` | `#3daee9` (same — Breeze blue is theme-stable) |

### Typography

- **Family**: `system-ui, "Noto Sans", "Segoe UI", Roboto, "Helvetica Neue", sans-serif` (mock uses Inter from Google Fonts; the real app should use system stack).
- **Mono**: `ui-monospace, "JetBrains Mono", Menlo, monospace` (IPs, MACs, IDs).

| Role | Size | Weight |
|---|---|---|
| Page title (pane header H1) | 20px | 700 |
| Section H2 | 16px | 600 |
| Group-box title | 13px | 600 |
| Body | 13px | 400 |
| Label | 12px | 500 |
| Helper / dim | 11.5–12px | 400 |
| Sidebar item | 13px | 500 (600 active) |
| Status badge | 11px | 600 uppercase optional |

### Spacing

- Sidebar width: **180px**.
- TV-list (pane master) width: **260px**.
- Pane padding: **16–20px**.
- Group-box internal spacing: **8px** between rows, **12px** between groups.
- Form-row label column: **140px**.

### Radii / shadows

- Buttons / inputs: **4px**.
- Cards / GroupBox: **5–6px**.
- Window: **6px** (Slint native; no shadow needed in real app).
- Avatar tiles (vendor icons): **6px**.

---

## Component Map (Slint widget choices)

| Design element | Slint mapping |
|---|---|
| Sidebar item | `Rectangle` + `TouchArea` + `Text` (already in `main.slint`) |
| TV list row | `Rectangle` + `TouchArea`; bottom 1px `Rectangle` separator |
| Status badge / chip | `Rectangle` with `border-radius: 12px`, `border-width: 1px`, padded `Text` |
| GroupBox section | `std-widgets.GroupBox` (already used) |
| Slider / Switch / CheckBox / ComboBox / Button | std-widgets — already wired |
| Vendor logo tile | `Rectangle` + 2-letter `Text`, hardcoded vendor → color map |
| Stepper (wizard) | `HorizontalLayout` of `Rectangle` (number circle) + `Text` (label) + 1px line `Rectangle` between |
| Pulse / scanline | Slint `animate` on `opacity` / `x` — keep simple |
| Modal overlay | `Rectangle { background: #00000080; }` overlay (pattern already in `tvs_pane.slint` `ConfirmDialog`) |
| Inline error banner | `Rectangle` with `dangerSoft` bg, `danger` left border 3px optional, text + actions |
| Collapsible section | `Rectangle` containing a `TouchArea` header that toggles a property; show/hide body via `if` |

---

## Screens & Layouts

### App shell

```
┌──────────────────────────────────────────────────────────────┐
│ [titlebar — Plasma decoration takes over in real app]        │
├──────┬───────────────────────────────────────────────────────┤
│ tisvlay │                                                    │
│         │                                                    │
│  TVs    │              <active pane content>                 │
│  Profiles│                                                   │
│  Session │                                                   │
│  About   │                                                   │
│         │                                                    │
│  ─────  │                                                    │
│  ● Daemon │                                                  │
│  ● Session │                                                 │
└──────┴───────────────────────────────────────────────────────┘
```

- Sidebar **180px**, full-height, `sidebar` bg.
- Items: 36px tall, 4px radius, 12px left padding.
- Status footer at bottom: small bullet `●` (success / danger color) + label, 11–12px.
- Min window: 720×480 (matches existing `main.slint`).
- Preferred: 900×640.

### TVs pane (`tvs-pane.jsx`)

Master/detail layout, both columns inside a single `HorizontalBox`.

**Master (260px wide)**
- Header band: "N configured" (uppercase 11px) + circular `+` button (accent fill, white plus icon, 24×24).
- Scrollable `ListView` of `TvListRow`.
- Each row: 56px tall, hover bg `rowHover`, selected bg `rowSelected` with 2px left accent stripe.
  - 8×8 status dot (success / danger, vertically centered)
  - Vendor tile (28×28 letter avatar) — keep as decorative; not from design system (defined per vendor in `app-shell.jsx`)
  - Name (13px 600) + subtitle (vendor + IP, 11.5px dim mono for IP)
  - Trailing managed indicator: nothing if managed, small "unmanaged" chip in warn color if not.

**Detail**
- ScrollView. Padding 18–20px.
- Header row: TV name (20px 700) + 2 status badges (Connected/Disconnected, Managed/Unmanaged) + Remove button (right-aligned).
- Power group: Power On/Off button + live phase text (driven by `power_cycle_phase` view-model field, already in place):
  - `wol_sent` → "Sending wake-up…" (textDim)
  - `connecting` → "Connecting…" (textDim)
  - `connected` → "Connected" (success)
  - `failed` → "Wake failed" (danger)
- Screen toggle row: label + Switch + helper "(blanks the panel without powering off)".
- Audio group: Volume slider (0–100, disabled when muted) with live numeric, Mute switch.
- Picture group: Backlight slider (0–100) with live numeric.
- Input group: ComboBox bound to `available_input_ids`, disabled if empty.
- Configuration group: Managed switch with "(automated now)" hint, System profile combo, User profile combo (disabled when session offline), Primary input combo, TV ID readonly.
- Hardware identity group: Manufacturer / Model / Serial / DRM connector — labeled rows, dashes for empties.

**Empty state** (no TV selected): centered "Select a TV from the list" in `textFaint`.

**Empty state** (no TVs configured): pane shows centered illustration + headline "No TVs paired" + "Pair a TV" primary button (triggers same action as the `+`).

**Daemon offline state**: pane shows centered danger banner "Daemon offline — start `tisvlayd.service`" with a `Reload Config` button below.

### Pairing wizard (`Pairing Wizard.html`, integrated as in `tisvlay GUI.html`)

When `+` is clicked, the entire **content area** (right of the sidebar) is replaced by the wizard until cancelled or finished. **It is NOT a separate window** — the modal-dialog framing in the standalone `Pairing Wizard.html` was for the original spec; the user clarified it should take over the TVs pane in-place. (See `app-prototype.jsx` for the integration pattern.)

- 4 steps via `Stepper` component.
- Steps 1–3 have multiple sub-states; refer to the gallery for every variation.
- Cancel returns to the TVs pane with the previous selection restored.

### Profiles pane (`other-panes.jsx`)

Two-column layout: left list of profiles, right detail.

**Left list (220px)**:
- Two sections, each with a header strip and `+` add button:
  - "System profiles" (lifecycle)
  - "User profiles" (session)
- Items show profile name + small "default" chip on the default profile (cannot be deleted).

**Right detail**:
- Header: profile name (editable inline for non-default), kind chip (System / User), Duplicate / Delete buttons.
- Inherits-from line for non-default profiles: "Inherits unset fields from `default`" with a help-circle tooltip.
- Field rows. Each row shows:
  - Label (140px column)
  - Control (Select / Slider / Number)
  - **Inheritance state indicator** — one of:
    - "Set" (text, accent color)
    - "Inherited from default: <value>" (textDim, with a small "Override" link)
  - Helper line below (textDim, 11.5px)

**System profile fields** (one per row):
- `on_startup` — Select: power_on / power_off / screen_on / screen_off / none
- `on_shutdown` — Select (same enum)
- `on_reboot` — Select (same enum)
- `on_no_session` — Select (same enum)
- `on_sleep` — Select (same enum)
- `on_wake` — Select (same enum)
- `automate_when` — Select: always / on_primary / off_primary

**User profile fields**:
- `on_session_lock` — Select
- `on_session_unlock` — Select
- `on_greeter_wake` — Select (with footnote "Wayland only")
- `idle_timeout_seconds` — Number (0 = disabled), with helper text
- `on_idle_timeout` — Select
- `on_idle_resume` — Select

**Default-profile editing**: same UI but no inheritance line and no Delete button. The user knows they're setting the baseline; everything is "Set".

### Session pane

Centered placeholder per user instruction:
> "Session-service controls arrive in M5."

Keep current Slint as-is.

### About pane

Same content as today (`uploads/about_pane.slint`) restyled to match new tokens. Layout:
- Big title "tisvlay" 28px 700
- Tagline 14px textDim
- Connections group: two rows with bullet + label + status text
- Footer: license + repo URL in textFaint

---

## Interactions & Behavior

All callback names already exist in `main.slint` and `main.rs` — wire the new UI to them.

| UI action | Existing callback (in `main.slint`) |
|---|---|
| Volume slider | `set-volume(string, int)` |
| Mute switch | `set-mute(string, bool)` |
| Backlight slider | `set-backlight(string, int)` |
| Screen switch | `set-is-screen-on(string, bool)` |
| Input combo | `switch-input(string, string)` |
| Power On btn | `power-on(string)` (subscribes to `PowerCycleProgress` in Rust) |
| Power Off btn | `power-off(string)` |
| Managed switch | `set-managed(string, bool)` |
| System profile combo | `set-system-profile(string, string)` |
| User profile combo | `set-user-profile(string, string)` |
| Primary input combo | `set-primary-input(string, string)` |
| Remove btn | `remove-tv(string)` (after confirm dialog) |
| Reload Config btn | `reload-config()` |

**New callbacks needed for the additions in this design:**

- `request-pair()` → opens the pairing wizard. Pane shifts to wizard mode (in Slint: an `in-out property <bool> pairing-mode;` on `TvsPane`, plus the wizard component conditionally rendered).
- For wizard:
  - `discover-tvs(timeout: int)` → calls `Manager1.DiscoverTvs`. Already exists on `DaemonClient::discover_tvs`.
  - `pair-tv(name: string, vendor: string, ip: string, managed: bool, system-profile: string)` → calls `Manager1.AddTv` with vendor-specific JSON.
  - `cancel-pairing()` → returns to TVs pane.
- For Profiles pane:
  - `add-profile(kind: string)`, `duplicate-profile(kind: string, name: string)`, `delete-profile(kind: string, name: string)`, `rename-profile(kind: string, old-name: string, new-name: string)`, `set-profile-field(kind: string, profile: string, field: string, value: string)`, `clear-profile-field(kind: string, profile: string, field: string)`.

These will need the matching D-Bus methods on `Manager1` / `Session1` (currently profiles are read-only on D-Bus — addition/edit/delete is new daemon work, out of scope of this design but flagged here for the engineering plan).

### Power-cycle progress

Already wired. Just bind UI: button `enabled` is false while `power_cycle_phase` is non-empty + not `connected` / `failed`. Phase text uses the same string mapping as today's `tvs_pane.slint`.

### Theme switch

Add `in-out property <bool> dark-theme;` on `AppWindow`. All panes read it via property binding. The toggle is rendered in the sidebar footer as a small icon button (sun/moon). Persists via `slint::set_xdg_app_id` + an env file or KConfig if you want; not required for first cut.

### Animations

Keep minimal:
- Pulse dot in pairing/discovering: `animate opacity` 1.4s loop. Slint `animate` on a `Rectangle.opacity` does this.
- Scan line in discover: `animate x` linear loop.
- Stepper transitions: just hard color/state changes — no slide.

If `prefers-reduced-motion` is set (Slint's `Window::default-font-size` and friends don't expose this directly), have the `dark-theme` toggle live in an env-readable property that can also disable animations.

---

## State Management

The Rust side (`uploads/state.txt`, `uploads/main.txt`) already provides:

- `TvViewModel` (per-TV row, bound to `app.tvs`)
- `system_profiles: [string]`, `user_profiles: [string]`
- `daemon_available: bool`, `session_available: bool`
- `selected_tv_index: int`

**Additions needed:**

- `in-out property <bool> dark-theme;` on `AppWindow`.
- `in-out property <bool> pairing-mode;` on `TvsPane` (or a new top-level `Pane.pairing` enum value — but keeping it scoped to the TVs pane is cleaner since it returns to the TVs list).
- `[ProfileViewModel]` for both profile kinds (add a `ProfileViewModel` struct in Rust mirroring the field set; mark which fields are "set" vs "inherited").

---

## Files in this bundle

| File | Purpose |
|---|---|
| `tisvlay GUI.html` | Interactive prototype entrypoint |
| `tisvlay GUI Gallery.html` | Static gallery on a design canvas |
| `Pairing Wizard.html` | Standalone wizard view + accessibility notes |
| `app-shell.jsx` | Tokens (`APP_T`), shell components (sidebar, footer, theme toggle, primitives — Btn/IconBtn/Chip/StatusBadge/FormRow/etc.) |
| `tvs-pane.jsx` | TVs pane (master/detail) + sample data |
| `other-panes.jsx` | Profiles, Session, About panes |
| `wizard-components.jsx` / `wizard-frames.jsx` | Pairing wizard primitives + frames |
| `app-prototype.jsx` | Interactive shell + state wiring |
| `gallery.jsx` | Gallery composition |
| `design-canvas.jsx`, `tweaks-panel.jsx` | Tooling for the static gallery and the theme toggle (not for shipping) |

## Suggested implementation order

1. **Tokens & primitives** — port `APP_T` to Slint global (`global Theme`), build reusable components (`StatusBadge`, `Chip`, `IconBtn`, `FormRow`, `LabeledRow`).
2. **App shell + theme switch** — wire `dark-theme` property; light/dark token branches in the global.
3. **TVs pane** — refactor existing `tvs_pane.slint` into the new master/detail with the polished header / status badges / power-cycle indicator. (Mostly visual; Rust is unchanged.)
4. **Pairing wizard** — new `pairing_wizard.slint` component, mounted inside `TvsPane` when `pairing-mode = true`. Wire `discover-tvs` + `pair-tv` callbacks (Rust side: extend `DaemonClient` if needed).
5. **Profiles pane** — biggest new chunk. Requires backend D-Bus additions for write operations; if those slip, ship read-only first (matches the gallery's "default-only" state nicely).
6. **About pane** — token swap only.
7. **Session pane** — leave M5 placeholder; restyle text to match new tokens.

---

## Notes & gotchas

- **Vendor color tiles** (LG red, Samsung blue, Sony black) are decorative; do not derive these from theme tokens. Hard-coded map per vendor — define centrally so other surfaces (tray, future widgets) can reuse.
- **The existing `tvs_pane.slint`** uses ad-hoc colors (`#3b82f6`, `#10b981`, etc.). Replace all with the new tokens via `global Theme`.
- **`power_cycle_phase`** field is already in `TvViewRaw` and `TvViewModel`. Use it as the source of truth — don't add new state.
- **Confirm-remove dialog** is already implemented in `tvs_pane.slint`; restyle only.
- **Sidebar width** in current `main.slint` is 180px — keep that.
- The pairing wizard's "Manual fallback" collapsible should default-open whenever discovery has been running for >10s with zero results, OR on any discovery error.
- All buttons ≥30px tall. List rows ≥48px (Fitts-friendly across DEs).
- Plasma decoration takes over the titlebar in the real app — the mock's titlebar with traffic lights is decorative for desktop-environment-agnostic preview.
- WCAG AA contrast verified for all text/bg pairs; preserve when porting (especially `accent` on white needs ≥600 weight + white text on accent, never accent text on white as a link).

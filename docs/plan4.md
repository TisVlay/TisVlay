# Plan 4 — Slint architecture sweep

> **Status (in progress):** Phases 1, 2, and 3 shipped. Phase
> 3 went further than the original "cosmic vs fluent" framing:
> after a cosmic + qt experiment loop (cosmic was acceptable
> but not clearly better, qt added a hard Qt6 runtime dep —
> recorded in commit `f22e54b` + revert `dd6fb74`), fluent
> stayed the default. An empirical wrapper-by-wrapper test
> dropped `ThemedCheckBox` / `ThemedSelect` / `ThemedTextInput`
> in favor of std-widgets, replaced `Btn` (default + accent
> sites) with std-widgets `Button` and renamed the surviving
> red-only widget to `DangerBtn`, and added a thin
> `LineEditField` wrapper to fix std `LineEdit`'s
> low-contrast placeholder. The `theme.slint` audit pruned 3
> dead tokens; the broader Theme→Palette swap turned out to
> be a visual-identity choice rather than mechanical, so
> Theme stays.
>
> Still open: Phases 4-7.

A full pass through the GUI's Slint setup against Slint's own
documented patterns. Driven by the realization (during the
convergence-gating work) that several pieces of our initial setup
were defaulting to non-idiomatic patterns. Now that the D-Bus
surface is settled, we have a clean window to align the Slint side
before the GUI grows further (Profiles pane, Session pane, etc.).

## Motivation

Two concrete findings from the recent debugging arc:

1. The 100ms `signal_debounce` we shipped in `StateWatcher` was
   redundant — Slint already queries `current_monitor()
   .refresh_rate_millihertz()` and coalesces property updates between
   vsync frames. We were rate-limiting on top of a renderer that was
   already rate-limiting.
2. Our `OptimisticSlider`'s `changed model-value => { display = ... }`
   block writes to a property from a `changed` handler — the exact
   pattern the
   [Slint properties guide](../libs/slint/docs/astro/src/content/docs/guide/language/coding/properties.mdx)
   explicitly warns against ("compromises the purity of bindings …
   can cause loops").

Both shipped because we matched the surrounding code patterns instead
of cross-referencing Slint's documented guidance. With more sliders,
panes, and shared callbacks coming, locking in idiomatic patterns now
saves churn later.

## Scope

Seven phases, ranging from architectural (1, 2) to experimental (3)
to polish (4-7). Each is independent — partial completion is fine.

| # | Phase | Effort | Impact |
|---|-------|--------|--------|
| 1 | Move convergence gating from Slint to Rust | medium | high — real GUI/tray dedup |
| 2 | `global Daemon { ... }` for cross-pane callbacks | medium | high — kills prop drilling |
| 3 | Try `SLINT_STYLE=cosmic`, possibly drop Themed* wrappers | small (experiment) | possibly large |
| 4 | Computed bindings replace `changed` handlers where possible | small per site | medium |
| 5 | `on_ui(weak, |app| ...)` helper | small | small (boilerplate reduction) |
| 6 | `Palette.color-scheme = ...` direct, drop `AppearanceMode` enum | small | small |
| 7 | Animations (`animate background { duration: 150ms; }`) | small | polish |

## Decisions (locked)

- **Helper module layout**: split `dbus_client.rs` into per-helper
  modules. `RefreshTrigger`, `Coalescer<K, V>`, and
  `Reconciler<V>` + new `ReconcilerMap<K, V>` each get their own
  file. `dbus_client.rs` keeps just the actual D-Bus client. (Per
  the user's preference for smaller, single-purpose modules.)
- **Slint global split**: three separate files, one global per
  concern. `ui/globals/daemon.slint` (TV control callbacks),
  `ui/globals/pairing_wizard.slint`, `ui/globals/settings.slint`.
- **Style switch**: try `cosmic` first; if it renders cleanly on
  KDE Plasma in both light and dark mode, commit to it. The goal
  is broader than just "drop Themed* wrappers" — lean on the
  DE/Palette for theming so we maintain less of our own brushes.
- **Theme reduction**: after the style switch, audit `theme.slint`
  for tokens that can be replaced with std-widgets `Palette`
  values. Goal: minimize our custom theme surface.
- **Animation feel**: decide during execution; pure polish.

## Phase 1 — Convergence gating in Rust

> **Status (done):** Shipped. Helpers split into per-file
> modules (`refresh_trigger.rs`, `coalescer.rs`, `reconciler.rs`)
> in `tisvlay-core`. `ReconcilerMap<K, V>` factored. Tray
> migrated; GUI gained a symmetric `Reconcilers` struct threaded
> through `register_callbacks` / `initial_fetch_and_watch` /
> `watch_state` / `refresh`. `OptimisticSlider` simplified to a
> thin view (passthrough `changed model-value => display`).
> Commit `5a8724f`.

### Why

The GUI currently reimplements (in Slint) the same gating logic the
tray has in Rust (`Reconciler<V>` + `ScrollReconcilers`). The
duplication is concrete:

- Slint side (`OptimisticSlider` in `widgets.slint`):
  `last-sent`, `pending`, `display`, the `changed model-value`
  if-else block.
- Rust side (`ScrollReconcilers` in `tisvlay-tray/src/main.rs`):
  `volume`/`backlight` HashMaps of `Reconciler<u8>`,
  observe/dispatch helpers.

Same algorithm in two languages. When the user said earlier "I
don't want duplicate code", the obstacle was "different storage
layers (Slint properties vs Rust struct fields)". Slint's docs
themselves point at the resolution: keep Slint as a thin view,
push the state machine to Rust.

### Result

- Slint `OptimisticSlider` has no gating logic — just `display`
  property + `committed(int)` callback. ~30 lines of
  state/branches removed from `widgets.slint`.
- Rust holds per-`(tv_id, property)` `Reconciler<u8>` for each
  consumer (tray, GUI). Same shared helper.
- One implementation in `tisvlay-core`, two consumers.

### Steps

1. **Split helpers out of `dbus_client.rs`** (precursor — keeps the
   rest of Phase 1 less noisy and aligns with the locked decision
   on module layout):
   - `crates/tisvlay-core/src/refresh_trigger.rs` — `RefreshTrigger`.
   - `crates/tisvlay-core/src/coalescer.rs` — `Coalescer<K, V>`
     (move with the existing unit tests).
   - `crates/tisvlay-core/src/reconciler.rs` — `Reconciler<V>` + the
     new `ReconcilerMap<K, V>` (added below). Move the existing
     `reconciler_tests` mod with it.
   - `dbus_client.rs` retains only the actual D-Bus client surface
     (`DaemonClient`, `SessionClient`, proxy traits, `StateWatcher`,
     `StateEvent`, etc.).
   - `lib.rs` re-exports unchanged from the consumer's perspective.

2. **Generalize `ScrollReconcilers` into `ReconcilerMap<K, V>`** in
   the new `reconciler.rs`:

   ```rust
   pub struct ReconcilerMap<K, V> {
       inner: Arc<Mutex<HashMap<K, Reconciler<V>>>>,
   }

   impl<K: Hash + Eq + Clone, V: Copy + PartialEq + Send + 'static>
       ReconcilerMap<K, V>
   {
       pub fn new() -> Self;
       pub fn observe(&self, key: &K, value: V) -> V;
       pub fn current(&self, key: &K, fallback: V) -> V;
       pub fn dispatch(&self, key: &K, target: V);
   }
   ```

   Generic over key type (`String` for `tv_id`) and value type
   (`u8` for our properties; reusable for `bool`/`i32`/etc. later).
   Add unit tests covering:
   - Lazy creation on first use.
   - Per-key isolation.
   - Observation gating per key.

3. **Tray migration**: replace `ScrollReconcilers` struct in
   `tisvlay-tray/src/main.rs` with two `ReconcilerMap<String, u8>`
   instances (volume + backlight). Existing tray code mostly stays
   the same; just the helper struct is replaced. The `process_commands`
   ScrollBy match arm and the `refresh_state` observation pass are
   the call sites.

4. **GUI migration**: in `tisvlay-gui/src/main.rs`, add the same two
   `ReconcilerMap<String, u8>` instances. Wire them into:
   - `register_callbacks`: `on_set_volume` / `on_set_backlight`
     callbacks call `volume_recs.dispatch(tv_id, value)` BEFORE
     dispatching to the daemon (so observations during the daemon
     round-trip are correctly gated).
   - `refresh` (or `push_snapshot`): for each fetched TV, call
     `volume_recs.observe(tv_id, daemon_volume)` and
     `backlight_recs.observe(tv_id, daemon_backlight)`. Use the
     returned `current()` value when populating the per-row
     `TvViewModel` instead of the raw daemon value.

5. **Slint `OptimisticSlider` simplification**: in
   `tisvlay-gui/ui/widgets.slint`, remove `last-sent`, `pending`,
   the `changed model-value` if-else block. Keep:
   ```slint
   in property <int> model-value;
   in property <int> minimum: 0;
   in property <int> maximum: 100;
   in property <bool> enabled: true;
   in property <bool> value-visible: true;
   callback committed(int);

   in-out property <float> display: model-value;
   changed model-value => { display = model-value; }

   HorizontalLayout { ... Slider { value <=> root.display; ... } ... }
   ```

   The `changed model-value => { display = model-value; }` is a
   pure passthrough (one writer per property — Rust drives
   `model-value`, this just keeps `display` synced after a user
   drag breaks the initial binding). Not a violation of the "don't
   write in changed" rule because (a) display ≠ model-value, no
   loop; (b) it's the standard Slint pattern for re-establishing a
   broken `<=>` binding to a controlled value (the alternative is
   a pure binding which the Slider's two-way value can't sustain).

6. **Tests**:
   - `scroll_apply_under_ramp.rs`: should pass unchanged (uses
     `Reconciler<V>` directly).
   - Add a unit test for `ReconcilerMap<K, V>` covering the lazy
     create + per-key isolation behavior.
   - Manual QA: GUI volume/backlight slider drag forward+reverse
     during ramp; tray scroll forward-fast then reverse-small.
     Both should feel identical to today (the structural change is
     where the gate lives, not what it does).

### Risks

- **`changed model-value` loop hazard**: the simplified passthrough
  writes `display` from `changed model-value`. If `display` ever
  feeds back into `model-value` (it won't — `model-value` is `in`,
  driven by Rust only), we'd loop. Document the invariant in
  the slint comment.
- **Initial value timing**: `display: model-value` initializes
  `display` to whatever model-value is when the component first
  instantiates. If the model arrives later, the `changed` handler
  takes over. Should be fine for our load order (initial fetch
  happens before AppWindow.run).

### Suggested commit message

```
refactor(core): factor ReconcilerMap, gating in Rust not Slint

Pull the per-(key, value) reconciler-map pattern out of the tray's
`ScrollReconcilers` into a generic `ReconcilerMap<K, V>` in
`tisvlay-core`. Both the GUI and the tray now use it for
optimistic-state gating against ramping daemon properties.

The GUI's `OptimisticSlider` previously did the same gating in
Slint via `last-sent` / `pending` / a `changed model-value`
if-else block — exactly the write-from-changed pattern Slint's
properties guide warns against. Move it to Rust so Slint stays a
thin view (display + commit callback) and the algorithm has a
single implementation across both consumers.
```

## Phase 2 — `global Daemon { ... }` for cross-pane callbacks

> **Status (done):** Shipped across two commits — the global
> hoist (`c87211c`) plus an idiomatic-Rust follow-up (`4bf7820`)
> that bound `app.global::<X>()` once per scope rather than at
> every call site. Three globals files created
> (`ui/globals/daemon.slint`, `ui/globals/pairing_wizard.slint`,
> `ui/globals/settings.slint`); the visual `PairingWizard`
> Rectangle component renamed to `PairingWizardPane` to
> disambiguate from the same-named global. AppWindow shed ~30
> callbacks + 19 wizard-state properties; TvsPane / TvDetail /
> wizard pane shed all forwarders.

### Why

`AppWindow` declares ~12 callbacks (`set-volume`, `set-backlight`,
`set-mute`, `set-managed`, `set-system-profile`, `set-user-profile`,
`set-primary-input`, `switch-input`, `power-on`, `power-off`,
`remove-tv`, `request-pair`) and forwards each through every parent
component down to the leaf widget that fires it
(`AppWindow → TvsPane → TvDetail → OptimisticSlider`). Adding a new
callback means touching every layer.

Slint globals are designed for exactly this. The
[iot-dashboard](../libs/slint/examples/iot-dashboard/iot-dashboard.slint)
example uses the pattern verbatim.

### Result

- `AppWindow` callback section shrinks ~10×.
- Pane components don't re-expose daemon callbacks to children.
- Adding a new daemon write becomes: one global declaration + one
  Rust binding + the call site.

### Steps

1. **Create `ui/globals/` directory** with three per-concern files:
   - `ui/globals/daemon.slint` — TV control writes (volume, mute,
     backlight, screen, input, power on/off, managed, profiles,
     primary input, remove-tv, request-pair).
   - `ui/globals/pairing_wizard.slint` — pairing flow callbacks
     (cancel, step-next, step-back, retry-pair, back-to-discovery,
     finish, selected-changed, name-edited, profile-selected,
     managed-toggled, manual-vendor-edited, manual-ip-edited,
     manual-mac-edited, manual-add) plus the wizard's in-out state
     properties (selected-index, tv-name, etc.) if it makes sense
     to host them on the global.
   - `ui/globals/settings.slint` — appearance-mode plumbing,
     reload-config trigger.

   Each file declares one `export global X { ... }`. Imported into
   `main.slint` so they're registered with the AppWindow.

2. **Declare callbacks** on each global. Example for `Daemon`:
   ```slint
   export global Daemon {
       callback set-volume(string, int);
       callback set-mute(string, bool);
       callback set-backlight(string, int);
       callback set-screen(string, bool);
       callback switch-input(string, string);
       callback power-on(string);
       callback power-off(string);
       callback set-managed(string, bool);
       callback set-system-profile(string, string);
       callback set-user-profile(string, string);
       callback set-primary-input(string, string);
       callback remove-tv(string);
       callback request-pair();
   }
   ```

3. **Rust binding** (one site per global, replaces the ~12 separate
   `app.on_set_*` registrations in `register_callbacks`):
   ```rust
   let daemon = app.global::<Daemon>();
   daemon.on_set_volume(...);
   daemon.on_set_backlight(...);
   // ...

   let wizard = app.global::<PairingWizard>();
   wizard.on_cancel(...);
   // ...

   let settings = app.global::<Settings>();
   settings.on_appearance_mode_changed(...);
   // ...
   ```

4. **Migrate call sites**: in panes and components, replace
   `root.set-volume(tv.id, v)` with `Daemon.set-volume(tv.id, v)`,
   `root.wizard-cancel()` with `PairingWizard.cancel()`, etc.

5. **Delete callback re-exports**: the
   `callback set-volume(string, int);` declarations on `TvsPane`,
   `TvDetail`, etc. become dead and can be removed. Same for the
   `set-volume(v) => { root.set-volume(tv.id, v); }` forwarding
   handlers. Same for the wizard pane's ~15 callback re-exports.

### Risks

- **`tv.id` plumbing**: Each call site needs the `tv_id` to pass to
  `Daemon.set-volume(tv.id, v)`. Currently `TvDetail` has access to
  its `tv: TvViewModel`, so `tv.id` is available; `OptimisticSlider`
  doesn't. Either: (a) pass `tv_id: string` as an `in` property to
  `OptimisticSlider`, or (b) keep the `committed(int)` callback at
  the OptimisticSlider boundary and have `TvDetail` translate
  `committed(v)` → `Daemon.set-volume(tv.id, v)`. Recommend (b) —
  generic widget stays generic.

### Suggested commit message

```
refactor(gui): hoist daemon callbacks into global Daemon

Replace the ~12 per-component callback declarations and matching
prop-drilling forwarders with a single `global Daemon { ... }` in
`main.slint`. The AppWindow callback section, the per-pane
re-exports, and the per-component forwarding handlers all
collapse. Pane components call `Daemon.set-X(...)` directly.
Mirrors the pattern in slint/examples/iot-dashboard.

Wizard-specific callbacks split into `global PairingWizard {...}`
to keep concerns separated.
```

## Phase 3 — Lean on DE / `Palette`, drop custom theme

> **Status (partial):** Style-switch experiment didn't go as
> originally planned, but the underlying goal — drop the
> Themed* wrappers — was largely achieved on the existing
> default style.
>
> **Style outcome**: tried `cosmic` (looked acceptable on KDE
> Plasma but not clearly better than fluent), then tried `qt`
> (looked nice but adds a hard `qt6.qtbase` + `qt6.qtwayland`
> runtime dependency, breaks our musl-static GUI packaging
> path, and would require Qt6 declared as `Depends` in every
> distro package — tracked in commit `f22e54b` and revert
> `dd6fb74` for a future revisit). Settled on **fluent**
> (Slint's default; no `with_style` override in `build.rs`).
>
> **Wrapper outcome**: empirically tested wrapper-by-wrapper
> against current Slint 1.16 + fluent + KDE Plasma. The
> "fluent washes out on unfocused windows" rationale that
> motivated the original wrappers no longer holds. Dropped:
>
> - `ThemedCheckBox` → std-widgets `CheckBox` (1 site).
> - `ThemedSelect` → std-widgets `ComboBox` (7 sites). The
>   wrapper's `placeholder` property became a `current-value:
>   foo == "" ? "default" : foo` ternary at each call site.
> - `ThemedTextInput` → new `LineEditField` thin wrapper.
>   `LineEdit` exposes `placeholder-text` but not
>   `placeholder-color`; fluent's hard-coded `text-secondary`
>   placeholder (~60% black) reads as "same color" as typed
>   text. `LineEditField` suppresses fluent's placeholder
>   (passes empty `placeholder-text:` to inner `LineEdit`) and
>   overlays its own `Text` at `Theme.text-faint`. Also
>   forwards `font-family` so IP / MAC inputs stay monospace.
>
> **Btn outcome**: 8 of 10 sites (default + accent) swapped
> to std-widgets `Button`; the 2 danger sites (Remove TV,
> confirm-destroy in `ConfirmDialog`) kept on a renamed
> `DangerBtn` since std `Button` has no danger variant. The
> ConfirmDialog's std `Button` Cancel sitting next to a
> `DangerBtn` Remove looks consistent on KDE Plasma; both
> render at 32px height, 4px border-radius, with the danger
> button distinguishable by color alone (which is the
> intent). Variant logic in the wrapper collapsed: dropped
> `variant` / `is-accent` / `is-filled` branches.
>
> **theme.slint outcome**: audited. Pruned 3 dead tokens
> (`bg`, `accent-pressed`, `track`). The contemplated
> mechanical swap (`Theme.text` → `Palette.foreground`,
> `Theme.border` → `Palette.border`, `Theme.accent` →
> `Palette.accent-background`, `Theme.surface-alt` →
> `Palette.alternate-background`) turned out to be a
> visual-identity decision rather than a refactor: each
> mapping has materially different rendering (alpha vs
> solid borders, softer grey-blue text vs near-black,
> fluent's accent vs Breeze blue). Theme stays — the
> remaining tokens are domain-specific (3-tier surface
> stack, 3-tier text hierarchy, chip / status / sidebar /
> row state, Breeze accent) and not duplicates of Palette.

### Why

Two related problems:

1. `Btn`, `ThemedTextInput`, `ThemedCheckBox`, `ThemedSelect` exist
   because std-widgets at the default `fluent` style render poorly
   on KDE/Plasma + unfocused windows (low-contrast borders,
   washed-out disabled states). They reimplement std-widgets with
   explicit theme tokens.

2. Our `theme.slint` has ~30 named brushes (surface, surface-alt,
   border, border-strong, accent, accent-soft, accent-pressed,
   success/-soft/-text, warn/-soft/-text, danger/-soft/-text/
   -pressed, chip/-text/-border, row-hover/-selected/-text,
   text/-dim/-faint). Many of these duplicate concepts already in
   std-widgets `Palette` (background, foreground, accent-background,
   alternate-background, etc.). We maintain a parallel hierarchy.

The user's preference is to **lean on the DE/Palette for theming
and maintain less ourselves**. Slint ships built-in styles —
`fluent`, `material`, `cupertino`, `cosmic`, `qt`. `cosmic` is
closest to KDE Breeze; it's the most promising starting point for
"feels native on KDE".

If `cosmic` works:
- ~500 LOC of `widgets.slint` (the `Themed*` impls) goes away.
- A significant chunk of `theme.slint` collapses into `Palette`
  references (or just gets deleted in favor of using `Palette`
  directly at call sites).
- We follow the user's chosen color-scheme automatically (already
  partially via `Palette.color-scheme`; cosmic's brushes follow it
  natively).

### Steps

1. **Quick experiment** (15 minutes):
   ```bash
   SLINT_STYLE=cosmic cargo run -p tisvlay-gui
   ```
   Compare against current `fluent`. Specifically check:
   - Button hover/pressed states are visible.
   - Disabled buttons have readable text.
   - Switch/checkbox active state is visible.
   - ComboBox/dropdown styling.
   - LineEdit border in unfocused state.
   - Slider thumb/track contrast.
   - Light AND dark mode (toggle via Settings pane).

2. **If acceptable**: change build-time style in
   `tisvlay-gui/build.rs`:
   ```rust
   slint_build::compile_with_config(
       "ui/main.slint",
       slint_build::CompilerConfiguration::new().with_style("cosmic".into()),
   ).unwrap();
   ```

3. **Replace Themed* with std-widgets**: delete
   `ThemedTextInput`, `ThemedCheckBox`, `ThemedSelect`, `Btn` (if
   it was a theme workaround) from `widgets.slint`. Update call
   sites in `tvs_pane.slint`, `pairing_wizard.slint`, etc. to use
   std-widgets `LineEdit`, `CheckBox`, `ComboBox`, `Button`
   directly.

4. **Audit `theme.slint`**: for each brush, check whether
   `Palette` provides an equivalent. Replace usages with `Palette.*`
   at call sites and delete the local brush. Likely deletions:
   - `Theme.surface` → `Palette.background`.
   - `Theme.surface-alt` → `Palette.alternate-background`.
   - `Theme.text` → `Palette.foreground`.
   - `Theme.accent` → `Palette.accent-background` (or whatever
     `Palette` exposes — Slint 1.16's `Palette` reference at
     [palette.mdx](../libs/slint/docs/astro/src/content/docs/reference/std-widgets/globals/palette.mdx)).

   What probably stays: domain-specific tokens (chip backgrounds,
   status colors `success-text`/`warn-text`/`danger-text` for
   chips, `accent-soft` for selection backgrounds). Goal is to
   shrink `theme.slint` from ~30 brushes to whatever's genuinely
   project-specific.

5. **Audit `design.slint`**: not theme-related (sizes/spacing),
   but worth a sanity pass. Likely all of it stays; spacing/sizing
   isn't something `Palette` covers.

6. **If `cosmic` doesn't look right**: try `qt` (renders via Qt
   Quick if the feature is enabled — can match KDE more closely
   but adds a Qt build dep) or fall back to `fluent` + Themed
   wrappers. Document the outcome in a comment near `Btn` so we
   don't re-investigate. ("Tried cosmic 2026-04-26: still washes
   out on hover-state in dark mode. Reverted.")

### Future direction (out of scope here)

True DE-driven theming (accent color from KDE settings, custom
palette files) would require xdg-desktop-portal `Settings`
namespace queries for the user's accent color and writing it into
`Palette.accent-background` at runtime. Not in scope for this
plan; document as a future improvement if `cosmic` doesn't fully
satisfy.

### Risks

- **Visual regression on non-KDE platforms**: cosmic targets the
  COSMIC desktop. On GNOME/sway/macOS users may prefer
  fluent/cupertino. Address by leaving `SLINT_STYLE` overridable
  at build time; document in README.
- **`Btn`'s variant logic**: `Btn` has `variant: "default" |
  "accent" | "danger"` — std-widgets `Button` has `primary: bool`
  and no danger variant. May need to keep `Btn` for the danger
  case even if cosmic fixes the others.
- **Theme audit churn**: replacing every `Theme.surface` reference
  with `Palette.background` touches many files. Mostly mechanical;
  diff will be wide but shallow.

### Suggested commit message (if it works)

```
feat(gui): adopt cosmic style + Palette, drop custom theme layer

Replace fluent + custom Btn/ThemedTextInput/ThemedCheckBox/
ThemedSelect with stock std-widgets at the cosmic style — closer
to KDE Breeze rendering on the target platform — and collapse
most of theme.slint into Palette references at call sites. The
theme global retains only domain-specific tokens (chip palettes,
status colors).

Drops ~500 LOC of theme workarounds plus the parallel Theme/
Palette hierarchy. The GUI now follows the user's DE-chosen
color-scheme via Palette without our own per-brush light/dark
branching.
```

## Phase 4 — Computed bindings replace `changed` handlers

### Why

[Slint properties guide](../libs/slint/docs/astro/src/content/docs/guide/language/coding/properties.mdx)
explicitly recommends
`out property <T> derived: <expression>;` (computed) over
`changed X => { derived = ...; }` (imperative) — the binding form
is reactive, has fewer write paths, and removes loop hazards.

### Sites to audit

Run `rg -n "changed " crates/tisvlay-gui/ui/` and triage:

- Any `changed X => { local = ...; }` where the body is a pure
  function of inputs → convert to `out property local: <expr>;`.
- Any `changed X => { side_effect(); }` (firing a callback) →
  keep, that's the legitimate use of `changed`.

Likely candidates:
- The `changed model-value => { display = model-value; }`
  passthrough in `OptimisticSlider` (post-Phase-1) — keep, it's
  re-establishing a `<=>` binding which can't be done as a
  computed property.
- Any state-flag patterns like `changed tv => { interacting-X = ... }`
  if they survive Phase 1 — convert.

### Steps

1. Survey: `rg -n "changed " crates/tisvlay-gui/ui/`.
2. For each match, classify (pure-derive vs side-effect).
3. Convert pure-derives.
4. Document non-convertible cases inline.

### Risks

Low. Each conversion is local and behavior-preserving by
construction (computed binding has the same value as the imperative
assignment).

## Phase 5 — `on_ui` helper

### Why

`tisvlay-gui/src/main.rs` repeats this dance ~10 times:

```rust
let cb_weak = app_weak.clone();
slint::invoke_from_event_loop(move || {
    if let Some(app) = cb_weak.upgrade() {
        app.set_X(value);
    }
})
.ok();
```

A small helper:

```rust
fn on_ui<F>(weak: &slint::Weak<AppWindow>, f: F)
where
    F: FnOnce(AppWindow) + Send + 'static,
{
    let weak = weak.clone();
    slint::invoke_from_event_loop(move || {
        if let Some(app) = weak.upgrade() {
            f(app);
        }
    })
    .ok();
}
```

Use sites collapse to:

```rust
on_ui(&app_weak, |app| app.set_X(value));
```

### Steps

1. Add helper near top of `main.rs`.
2. Migrate every `slint::invoke_from_event_loop(... upgrade() ...)`
   to use it.
3. Verify build + behavior unchanged.

### Risks

None — pure refactor.

## Phase 6 — `Palette.color-scheme` direct write

### Why

We currently track `AppearanceMode` (System / Light / Dark) as an
enum on `AppWindow`, with Slint-side computation deriving a
`dark-mode` boolean from it and applying brushes accordingly.
Slint's `Palette.color-scheme` is itself a writable property — we
can write `Palette.color-scheme = ColorScheme.dark` directly and
let portal-driven `system` mode just leave it untouched.

### Steps

1. Drop `AppearanceMode` enum from `main.slint`.
2. Drop `set_appearance_mode` callback wiring in `main.rs`.
3. Settings pane writes `Palette.color-scheme` directly:
   ```slint
   if mode == "Light": Palette.color-scheme = ColorScheme.light;
   if mode == "Dark":  Palette.color-scheme = ColorScheme.dark;
   if mode == "System": /* leave untouched, portal drives it */;
   ```
4. `Theme` brushes already condition off `Palette.color-scheme`,
   so they cascade automatically.
5. Persist the choice in `gui.toml` as today.

### Risks

- **System mode after a forced toggle**: switching from System →
  Dark → System requires explicitly resetting to portal-driven.
  Verify `Palette.color-scheme` has a "default / unset" state we
  can return to; if not, may need to keep the enum for the
  three-way distinction. Test before committing.

## Phase 7 — Animations

### Why

Pure polish. None of our hovers, dialog opens, or wizard step
transitions are animated. Slint makes animations cheap.

### Steps

1. **Hover transitions** on `Btn`, `Chip`, `Btn`-variant
   backgrounds: add `animate background { duration: 150ms;
   easing: ease-in-out; }` to the relevant `Rectangle`s. Same for
   `border-color`.

2. **Wizard step transitions**: optional, larger. Use Slint
   `states [...]` with `in {...}` / `out {...}` blocks keyed off
   `wizard-step`. Skip unless we want the polish.

3. **Dialog open/close** (e.g. `ConfirmDialog`): fade-in via
   `opacity` animation triggered by the `open: bool` property.

4. **Slider thumb**: usually NOT animated when user is dragging —
   would conflict with direct manipulation. Skip.

### Risks

- Animations during user interaction can feel laggy. Apply only to
  passive UI states (hover, dialog open, etc.), never to controls
  the user is actively manipulating.
- Slow-motion debug: `SLINT_SLOW_ANIMATIONS=4` to verify nothing
  feels off.

## Cleanup checklist

Phase 1 — done:
- [x] Move `RefreshTrigger` from `dbus_client.rs` into
  `tisvlay-core/src/refresh_trigger.rs`.
- [x] Move `Coalescer<K, V>` (with its tests) into
  `tisvlay-core/src/coalescer.rs`.
- [x] Move `Reconciler<V>` (with its tests) into
  `tisvlay-core/src/reconciler.rs` and add `ReconcilerMap<K, V>`
  alongside.
- [x] Update `lib.rs` to declare the new modules + keep public
  re-exports unchanged.
- [x] Delete `OptimisticSlider`'s `last-sent`, `pending`, gating
  if-else block. Add a tight comment noting Rust owns the gate.
- [x] Replace `ScrollReconcilers` struct in the tray with two
  `ReconcilerMap<String, u8>` instances (volume + backlight). The
  named struct went to a small `Reconcilers` bundle struct that
  holds the two maps.

Phase 2 — done:
- [x] Create `ui/globals/daemon.slint`,
  `ui/globals/pairing_wizard.slint`, `ui/globals/settings.slint`.
- [x] Rename the visual `PairingWizard` Rectangle component to
  `PairingWizardPane` to free the name for the global.
- [x] Move struct types (`WizardDiscoveredTv`, `VendorOption`)
  into `ui/globals/pairing_wizard.slint` so the dependency
  direction stays one-way (component imports global, never
  reverse).
- [x] Delete the AppWindow callback declarations replaced by
  globals.
- [x] Delete the per-pane callback re-exports + forwarders
  (TvsPane / TvDetail / PairingWizardPane).
- [x] Idiomatic Rust: bind `app.global::<X>()` once per scope
  rather than at every call site (follow-up commit
  `4bf7820`).

Phase 3 — partial:
- [x] Try `SLINT_STYLE=cosmic`, decide whether to commit.
  Outcome: ok-but-not-clearly-better; stayed on fluent.
- [x] Try `qt` style. Outcome: looked nice but Qt6 runtime
  dep + musl-static break ruled it out for shipping (commit
  `f22e54b` + revert `dd6fb74` preserved as record).
- [x] Delete `ThemedCheckBox` from `widgets.slint`; migrate
  the 1 call site to std-widgets `CheckBox`.
- [x] Delete `ThemedSelect` from `widgets.slint`; migrate the
  7 call sites to std-widgets `ComboBox`. Placeholder via
  `current-value` ternary at each site.
- [x] Replace `ThemedTextInput` with `LineEditField` (thin
  wrapper around std `LineEdit` with overlaid high-contrast
  placeholder, supports `font-family` for IP/MAC monospace).
- [x] `Btn` swap experiment: 8 default/accent sites swapped to
  std-widgets `Button` (`primary: !connected` for the dynamic
  Power on/off site, `primary: true` for explicit accent).
  `Btn` renamed to `DangerBtn` and trimmed to a single-purpose
  red button (drops the `variant` property + `is-accent` /
  `is-filled` branches). 2 call sites: Remove TV + confirm
  destructive in `ConfirmDialog`. Side-by-side rendering
  in the ConfirmDialog (std `Button` Cancel + `DangerBtn`
  Remove) verified visually on KDE Plasma.
- [x] Audit `theme.slint` brush-by-brush. Dropped 3 dead
  tokens (`bg`, `accent-pressed`, `track`). The broader swap
  to `Palette.*` was evaluated and deliberately not done:
  `Palette.foreground` (`#000000E6` / `#FFFFFF`) vs
  `Theme.text` (softer grey-blue), `Palette.border` (alpha)
  vs `Theme.border` (solid grey), `Palette.accent-background`
  (fluent's blue) vs `Theme.accent` (hard-coded Breeze blue),
  and `Palette.alternate-background` vs `Theme.surface-alt`
  all differ enough to be visual-identity changes, not
  mechanical refactors. What remains in `Theme` is doing
  real work: 3-tier surface stack, 3-tier text hierarchy,
  domain tokens (chip / status / sidebar / row state), and
  the Breeze accent identity.

Phase 4:
- [ ] `rg -n "changed " crates/tisvlay-gui/ui/` and convert each
  pure-derive site to an `out property` computed binding.

Phase 5:
- [ ] Add `on_ui` helper near top of `main.rs`.
- [ ] Migrate every `slint::invoke_from_event_loop(... upgrade()
  ...)` site to use it.

Phase 6:
- [ ] Drop `AppearanceMode` enum + Rust mapper functions
  (`config_to_slint_mode`, `slint_to_config_mode`).
- [ ] Settings pane writes `Palette.color-scheme` directly.
- [ ] Verify System mode round-trip works after a forced toggle.

Phase 7:
- [ ] Hover transitions on Btn / Chip / row backgrounds.
- [ ] Dialog open/close fade.
- [ ] Smoke-test with `SLINT_SLOW_ANIMATIONS=4`.

Cross-cutting:
- [ ] Update `crates/tisvlay-gui/ui/main.slint` doc-comment
  describing the AppWindow surface — should reflect the new
  globals-callback model.
- [ ] Sweep `docs/design.md` for any GUI-architecture
  paragraphs (probably none, but check).
- [ ] Sweep `.claude/CLAUDE.md` for stale GUI patterns (callback
  examples, theme references).
- [ ] Close out `plan4.md` with a status header when done
  (matching the pattern of `plan2.md` / `plan3.md`).

## What to keep (rationale)

- **`Reconciler<V>`** in core — solves the gating problem; Phase 1
  builds on it, doesn't replace it.
- **`Coalescer<K, V>`** + **`RefreshTrigger`** — orthogonal to this
  work; just moving to their own files.
- **`Design` global** (sizes / spacing tokens) — `Palette` doesn't
  cover layout dimensions; this stays.
- **A reduced `Theme` global** — keeps domain-specific tokens that
  `Palette` doesn't cover (chip background palette, status semantic
  colors `success-text` / `warn-text` / `danger-text`, accent-soft
  for selection backgrounds). Goal: shrink, not delete.
- **`sync_vec_model` helper** in Rust — necessary for in-place
  `VecModel` mutation. There's no Slint built-in alternative that
  preserves widget state across server-driven row updates.
- **`if cond : Component {}` and `visible:` patterns** — both used
  correctly for their respective semantics (instantiate-on-demand
  vs hide-but-keep-state).

## Verification

After each phase commit, run the standard checks:

```bash
just check                # fmt + build + clippy + test
cargo nextest run         # all tests
just daemon-start &       # start daemon
cargo run -p tisvlay-gui  # smoke test the GUI
```

For Phase 1 specifically: replay the QA scenarios from `plan3.md`:

- GUI volume slider drag forward+reverse during a TV ramp (no
  thumb snap-back).
- GUI backlight slider drag forward+reverse.
- Tray scroll forward-fast then reverse-small (the original
  "lands at 10" bug).
- TV remote (or another GUI client) changes a value while local
  GUI is idle — the local GUI's display should track the external
  change.

## Commit strategy

Original plan was one commit per phase. Actual pattern diverged:

| Commit(s) | Phase | Notes |
|-----------|-------|-------|
| `5a8724f` | Phase 1 | `ReconcilerMap` + GUI gating in Rust |
| `c87211c`, `4bf7820` | Phase 2 | Globals hoist + idiomatic-Rust follow-up |
| `f22e54b`, `dd6fb74`, plus wrapper-drop commits | Phase 3 | Qt experiment + revert (kept for record), then incremental wrapper drops |
| TBD | Phases 4-7 | Polish bundle (computed bindings, on_ui, palette, animations) |

Phases 1 and 2 stayed close to the original "one commit per phase"
ideal. Phase 3 split into many small commits because the experiment
itself (cosmic → qt → revert → wrapper teardown) was iterative
rather than a single decision. Phases 4-7 are small and still
naturally bundle.

If a phase reveals unexpected cost or design questions, stop and
return to this plan rather than carry on into the next phase.

## Open questions (defer to execution)

- **Phase 3 leftovers**: `Btn` swap is not obviously a win — 5/6
  sites map cleanly to std `Button` via `primary: variant ==
  "accent"`, but the danger-variant Remove TV button has no
  std-widgets equivalent. Either keep `Btn` for all variants
  (consistency) or for danger alone (mostly-stock). Decide
  empirically when sitting in front of the GUI.

- **theme.slint audit**: mostly mechanical, but the rendered
  result depends on whether `Palette.background` etc. exactly
  match what we currently render. May reveal cases where the
  custom token was load-bearing (e.g. row-hover, accent-soft for
  selection backgrounds) and not just a duplicate of `Palette`.

- **Phase 7 timing**: which animations actually feel good?
  Subjective. Worth a quick playthrough with
  `SLINT_SLOW_ANIMATIONS=4` to make sure timings aren't off.

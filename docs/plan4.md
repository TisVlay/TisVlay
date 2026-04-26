# Plan 4 — Slint architecture sweep

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

## Phase 1 — Convergence gating in Rust

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

1. **Generalize `ScrollReconcilers` into `ReconcilerMap<K, V>`** in
   `tisvlay-core/src/dbus_client.rs` (next to `Reconciler` and
   `Coalescer`):

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

2. **Tray migration**: replace `ScrollReconcilers` struct in
   `tisvlay-tray/src/main.rs` with two `ReconcilerMap<String, u8>`
   instances (volume + backlight). Existing tray code mostly stays
   the same; just the helper struct is replaced. The `process_commands`
   ScrollBy match arm and the `refresh_state` observation pass are
   the call sites.

3. **GUI migration**: in `tisvlay-gui/src/main.rs`, add the same two
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

4. **Slint `OptimisticSlider` simplification**: in
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

5. **Tests**:
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

1. **Declare `global Daemon`** in `tisvlay-gui/ui/main.slint` (or a
   new `ui/globals.slint`):
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

2. **Rust binding** (one site, replaces the ~12 separate
   `app.on_set_*` registrations in `register_callbacks`):
   ```rust
   let daemon_global = app.global::<Daemon>();
   daemon_global.on_set_volume(...);
   daemon_global.on_set_backlight(...);
   // ...
   ```

3. **Migrate call sites**: in panes and components, replace
   `root.set-volume(tv.id, v)` with `Daemon.set-volume(tv.id, v)`.
   Drop the corresponding `callback set-volume(...)` declarations
   from each parent component.

4. **Delete callback re-exports**: the `callback set-volume(string, int);`
   declarations on `TvsPane`, `TvDetail`, etc. become dead and can be
   removed. Same for the `set-volume(v) => { root.set-volume(tv.id, v); }`
   forwarding handlers.

5. **Wizard callbacks decision**: the pairing wizard has its own
   set of callbacks (`wizard-cancel`, `wizard-step-next`, etc.).
   These are wizard-internal — they probably belong in a
   `global PairingWizard` rather than mixed with `Daemon`.
   Decide while implementing: separate global per concern, or one
   global for everything? Recommend separate globals (`Daemon`,
   `PairingWizard`, possibly `Settings`).

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

## Phase 3 — `SLINT_STYLE=cosmic` experiment

### Why

`Btn`, `ThemedTextInput`, `ThemedCheckBox`, `ThemedSelect` exist
because std-widgets default to `fluent` style which renders poorly
on KDE/Plasma + unfocused windows (low-contrast borders,
washed-out disabled states). They reimplement std-widgets with
explicit theme tokens.

Slint ships built-in styles: `fluent`, `material`, `cupertino`,
`cosmic`, `qt`. `cosmic` (System76's COSMIC desktop style) is
closest to KDE Breeze and may render acceptably without our
wrappers.

If it works: ~500 LOC of `widgets.slint` (the `Themed*` impls) goes
away, replaced by direct std-widget usage. Btn / Chip / Group /
FormRow stay (those aren't theme workarounds).

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

4. **If not acceptable**: keep wrappers, document the experiment
   outcome in a comment near the `Btn` definition so future-us
   doesn't re-investigate. ("Tried cosmic 2026-04: still washes out
   on hover. fluent + wrappers stays.")

### Risks

- **Visual regression on platforms other than KDE**: GNOME/sway
  users may prefer fluent. If we want per-DE styling, that's a
  bigger problem (Slint compiles against one style at build time).
  Address by leaving `SLINT_STYLE` overridable at build time;
  document in README.
- **`Btn`'s variant logic**: `Btn` has `variant: "default" |
  "accent" | "danger"` — std-widgets `Button` has `primary: bool`
  and no danger variant. May need to keep `Btn` for the danger
  case even if cosmic fixes the others.

### Suggested commit message (if it works)

```
feat(gui): switch slint style to cosmic, drop Themed* wrappers

Replace fluent + custom Btn/ThemedTextInput/ThemedCheckBox/
ThemedSelect with stock std-widgets at the cosmic style. Closer
to KDE Breeze rendering on the target platform; drops ~500 LOC of
theme workarounds.

Btn kept for the danger variant (std-widgets Button has primary
but not danger).
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

- [ ] Delete `OptimisticSlider`'s `last-sent`, `pending`, gating
  if-else block (Phase 1).
- [ ] Rename `ScrollReconcilers` → use `ReconcilerMap<K, V>` in
  tray (Phase 1). The tray's helper struct goes away as a named
  type; just two `ReconcilerMap` fields.
- [ ] Delete the AppWindow callback declarations replaced by
  `global Daemon` (Phase 2).
- [ ] Delete the per-pane callback re-exports + forwarders
  (Phase 2).
- [ ] If Phase 3 succeeds: delete `ThemedTextInput`,
  `ThemedCheckBox`, `ThemedSelect`, possibly `Btn` from
  `widgets.slint`.
- [ ] Drop `AppearanceMode` enum + Rust mapper functions
  (Phase 6).
- [ ] Update `crates/tisvlay-gui/ui/main.slint` doc-comment
  describing the AppWindow surface — should reflect the new
  global-callback model.
- [ ] Sweep `docs/design.md` for any GUI-architecture
  paragraphs (probably none, but check).

## What to keep (rationale)

- **`Theme` and `Design` globals** — already idiomatic. The
  iot-dashboard example does literally the same thing.
- **`Reconciler<V>`** in core — solves the gating problem; Phase 1
  builds on it, doesn't replace it.
- **`Coalescer<K, V>`** + **`RefreshTrigger`** — orthogonal to this
  work.
- **`sync_vec_model` helper** in Rust — necessary for in-place
  `VecModel` mutation. There's no Slint built-in alternative that
  preserves widget state across server-driven row updates.
- **`if cond : Component {}` and `visible:` patterns** — both used
  correctly for their respective semantics (instantiate-on-demand
  vs hide-but-keep-state).
- **`Palette.color-scheme`-conditioned brushes in `Theme`** —
  idiomatic; the iot-dashboard's Skin global does the same.

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

Suggested split (one phase ≈ one commit, with optional
consolidation):

| Commit | Phases | Notes |
|--------|--------|-------|
| 1 | Phase 1 | `ReconcilerMap` + GUI gating in Rust |
| 2 | Phase 2 | `global Daemon` + callback hoist |
| 3 | Phase 3 | Style experiment (conditional commit) |
| 4 | Phases 4-7 | Polish bundle (computed bindings, on_ui, palette, animations) |

Phases 1 and 2 have moderate blast radius and benefit from being
separate code-review surfaces. Phase 3 is conditional (commit only
if cosmic looks good). Phases 4-7 are small and naturally bundle.

If a phase reveals unexpected cost or design questions, stop and
return to this plan rather than carry on into the next phase.

## Open questions (to decide while implementing)

- **Phase 1**: should `ReconcilerMap` live next to `Reconciler` in
  `dbus_client.rs`, or in a separate `concurrency.rs` /
  `reconciler.rs` module? With `Reconciler` + `Coalescer` +
  `RefreshTrigger` + (now) `ReconcilerMap` all in `dbus_client.rs`,
  the file is getting concept-heavy. A `tisvlay-core/src/concurrency.rs`
  module pulling all four out is starting to look attractive.
  Decide with the implementer.

- **Phase 2**: one `global Daemon` for everything, or split into
  `Daemon` (TV control) + `Wizard` (pairing) + `Settings`
  (appearance / reload)? Recommend split — keeps each global's
  callback list short and the concerns visually separated. But
  one global is also defensible as the simplest possible.

- **Phase 3**: only worth doing if `cosmic` actually looks good on
  KDE. If `fluent` + Themed wrappers feel better visually, abandon
  the phase and document the rationale.

- **Phase 7**: which animations actually feel good? Subjective.
  Worth a quick playthrough with `SLINT_SLOW_ANIMATIONS=4` to make
  sure timings aren't off.

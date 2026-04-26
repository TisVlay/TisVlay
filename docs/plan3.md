# Plan 3 — D-Bus surface: read-only properties + Set methods

> **Status (closed):** Shipped across two commits — the code refactor
> (daemon impls, proxy traits, DaemonClient internals, test
> docstring) and this docs sweep (`docs/design.md`,
> `.claude/CLAUDE.md`, plan close-outs). All four writable TV-state
> properties are now read-only with paired `Set*` methods, method
> receivers are `&self`, and the "no pre-cache" workaround comments
> are gone. `state_forwarder` is the single authoritative emit path
> by construction. Reconciler / OptimisticSlider stay as documented.
>
> Kept as historical context for the design decisions.

## Motivation

Today, the daemon's writable TV-state properties (`Tv1.Volume.Volume`,
`Tv1.Volume.Muted`, `Tv1.Backlight.Backlight`, `Tv1.Screen.IsScreenOn`)
are exposed as `readwrite` properties. `Properties.Set` triggers
zbus's auto-emit of `PropertiesChanged` on setter return, which races
against `state_forwarder`'s explicit emit driven by the backend's
subscription path. Two emit sources for one logical state.

We've shipped a workaround (drop the optimistic pre-cache write in
each setter so the auto-emit fires with the *old* value, which
convergence-gating clients ignore as stale). That works but leaves
each setter with a long comment explaining why it intentionally
*doesn't* update the field it's supposed to set, and the bug is one
"helpful" pre-cache restoration away from regressing.

The conventional D-Bus pattern, used by zbus's own
[`screen-brightness.rs`](../libs/zbus/zbus/examples/screen-brightness.rs)
example (`org.gnome.SettingsDaemon.Power.Screen` → `StepUp`/`StepDown`
methods), UPower (`KbdBacklight.SetBrightness`), NetworkManager,
BlueZ, and most freedesktop services, is:

- **Properties = state** (read-only at the D-Bus level).
- **Methods = actions** (synchronous dispatch, async confirmation via
  the property's `PropertiesChanged` signal).

This eliminates the auto-emit race at its source: no setter, no
auto-emit. `state_forwarder` stays the single authoritative emit
path. Setter-level workaround comments go away. Behaviour aligns with
every other freedesktop service the user's tools (`busctl`, `d-feet`,
`gdbus`) already understand.

## Scope

**In scope** — convert these four `readwrite` properties to
`read`-only + Set method:

- `org.tisvlay.Tv1.Screen.IsScreenOn` → method `SetScreen(b)`
- `org.tisvlay.Tv1.Volume.Volume` → method `SetVolume(y)`
- `org.tisvlay.Tv1.Volume.Muted` → method `SetMuted(b)`
- `org.tisvlay.Tv1.Backlight.Backlight` → method `SetBacklight(y)`

The property name `IsScreenOn` stays (it's a boolean predicate —
clean to read); the method drops the `Is…On` suffix to read as a
verb-on-the-interface (`Tv1.Screen.SetScreen(true)`), matching the
shape of `SetVolume` / `SetMuted` / `SetBacklight`. The Rust-side
fn name on both daemon impl and proxy trait becomes `set_screen`.

**Out of scope** — config-driven properties stay `readwrite`. They're
synchronous on the daemon side (no TV roundtrip, no ramp, no race),
so `Properties.Set` is the right primitive:

- `org.tisvlay.Tv1.Name`
- `org.tisvlay.Tv1.Managed`
- `org.tisvlay.Tv1.SystemProfile`
- `org.tisvlay.Tv1.PrimaryInput`
- `org.tisvlay.Tv1.AutomateWhen` (already read-only)

**Already correct** — methods on TV-state interfaces, untouched:

- `org.tisvlay.Tv1.Power.PowerOn`/`PowerOff`
- `org.tisvlay.Tv1.Input.SwitchInput`

## What stays after the refactor

These solve a separate problem (delta-driven dispatch + thumb gating
during ramps) and remain irreducible client-side complexity:

- **`Reconciler<V>`** in `tisvlay-core` — used by the tray's
  `ScrollReconcilers`.
- **`OptimisticSlider`** in `tisvlay-gui/ui/widgets.slint` — Slint
  side of the same gating.
- **`ScrollReconcilers`** in `crates/tisvlay-tray/src/main.rs`.
- **Mock async ramp**
  (`backlight_ramp_intermediates` + `backlight_ramp_step_delay_ms`
  spawned task) — test infrastructure.
- **Coalescer** — orthogonal.
- **RefreshTrigger** — orthogonal.

## Migration

### Phase 1 — Daemon interface impls (`crates/tisvlayd/src/dbus/tv.rs`)

Pattern per property:

```rust
// BEFORE
#[zbus(property)]
fn backlight(&self) -> u8 { self.backlight }

#[zbus(property)]
async fn set_backlight(&mut self, level: u8) -> zbus::fdo::Result<()> {
    // ... validation ...
    info!(...);
    // [10-line comment about why we don't pre-cache]
    self.manager.set_backlight(&self.tv_id, level).await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
    Ok(())
}

// AFTER
#[zbus(property)]
fn backlight(&self) -> u8 { self.backlight }

async fn set_backlight(&mut self, level: u8) -> zbus::fdo::Result<()> {
    // ... validation ...
    info!(...);
    self.manager.set_backlight(&self.tv_id, level).await
        .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
}
```

Changes per setter:

- Drop `#[zbus(property)]` attribute → it becomes a regular method
  (zbus auto-converts `set_backlight` → `SetBacklight` D-Bus method).
- Drop the "no pre-cache" comment block.
- Trailing `Ok(())` collapses into `?` chain.

Specific sites in `crates/tisvlayd/src/dbus/tv.rs`:

| Rust fn | Current line | After: D-Bus method |
|---|---|---|
| `set_is_screen_on` → renamed to `set_screen` | ~287 | `Tv1.Screen.SetScreen(b)` |
| `set_volume` | ~329 | `Tv1.Volume.SetVolume(y)` |
| `set_muted` | ~350 | `Tv1.Volume.SetMuted(b)` |
| `set_backlight` | ~427 | `Tv1.Backlight.SetBacklight(y)` |

Also: change every method's receiver from `&mut self` to `&self`.
After the refactor there's no field mutation in any of these
methods (`state_forwarder` owns cache writes), so `&mut self` is
unjustified — and the [zbus FAQ on hangs](../libs/zbus/book/src/faq.md#im-experiencing-hangs-what-could-be-wrong)
is explicit that a slow `&mut self` interface method blocks all
other calls into the same interface for the duration. On a slow
ramping property (Luna roundtrip ~100-500ms for backlight) that's
visible if a second client tries to call into the interface during
the first client's `SetBacklight`. With `&self`, calls are
concurrent at the zbus layer; TvManager's per-TV mutex still
serializes the actual TV work, which is the right granularity.

The "Volume control interface" struct doc-comment ("Volume and mute
are writable properties. Setting them sends the command to the TV
backend and updates the cache on success.") needs updating to
describe the method+ro_property model.

### Phase 2 — Proxy traits (`crates/tisvlay-core/src/dbus_client.rs`)

Pattern per setter:

```rust
// BEFORE
#[zbus(property)]
fn set_backlight(&self, level: u8) -> zbus::Result<()>;

// AFTER
fn set_backlight(&self, level: u8) -> zbus::Result<()>;
```

Sites in the proxy traits:

- `TisvlayTvScreen::set_is_screen_on` (line ~247) → rename to
  `set_screen`. zbus auto-converts to `Tv1.Screen.SetScreen` D-Bus
  method, so naming on both sides aligns.
- `TisvlayTvVolume::set_volume` (line ~262).
- `TisvlayTvVolume::set_muted` (line ~270).
- `TisvlayTvBacklight::set_backlight` (line ~303).

`DaemonClient` wrapper methods (`set_volume`, `set_mute`,
`set_backlight`, `screen_on`, `screen_off`) — public Rust API
unchanged. Internally they call `p.X.set_Y(...)` on the proxy; that
call now invokes a method instead of `Properties.Set`. The two
internal `p.screen.set_is_screen_on(true|false)` calls inside
`DaemonClient::screen_on` and `screen_off` get renamed to
`p.screen.set_screen(true|false)` to match the proxy trait rename.

### Phase 3 — Cleanup

Stale comments and references to remove:

- `crates/tisvlayd/src/dbus/tv.rs`:
  - The "no pre-cache" comment blocks in all four setters become the
    method bodies' default behavior — delete entirely.
  - `set_backlight`'s long comment about "see
    `backlight_emit_ordering.rs`" — drop the rationale section, keep
    the test reference if useful (the test still guards the same
    invariant).
  - `TvVolumeService` struct doc comment ("Volume and mute are
    writable properties. Setting them sends the command...") →
    rewrite to "Volume and mute state. Reads via the cached
    properties; writes via `SetVolume` / `SetMuted` methods".
  - Same for `TvBacklightService` and `TvScreenService` doc comments.

- `crates/tisvlayd/tests/backlight_emit_ordering.rs`:
  - Module docstring mentions "the daemon's `Tv1.Backlight.Backlight`
    D-Bus property setter optimistically pre-caches" — rewrite to
    reference the new method-based model. The test itself passes
    unchanged (its invariant is "target value doesn't appear before
    ramp intermediates", which is now true by construction since
    there's no setter to race with `state_forwarder`).

- `docs/plan2.md`:
  - Item (A) recommended skip — leave note.
  - Item (C) coalescer factor-out — DONE, mark as such.
  - Add a closing note pointing to `plan3.md` for the next refactor.

- `docs/plan.md`:
  - Whatever it currently says about M3.5 / Profiles unchanged —
    that's the next milestone.

### Phase 4 — Documentation

`docs/design.md`:

- Lines ~254-260 (Feature Interfaces table). Change:
  > "Controllable state is exposed as writable properties — setting
  > them sends the command to the TV backend and updates the cache
  > on success. zbus auto-emits `PropertiesChanged` after each
  > successful set."

  to:
  > "Controllable state is exposed as read-only properties driven by
  > backend subscription pushes. Writes go through explicit Set
  > methods (`SetVolume`, `SetMuted`, `SetBacklight`,
  > `SetIsScreenOn`) — `state_forwarder` is the single emit path
  > for `PropertiesChanged`, removing the auto-emit-vs-subscription
  > race that affected ramping properties (notably backlight on LG
  > OLEDs)."

  And update each interface line:
  - `Tv1.Screen` — `IsScreenOn` (ro) property; `SetScreen(b)` method.
  - `Tv1.Volume` — `Volume` (ro), `Muted` (ro) properties;
    `SetVolume(y)`, `SetMuted(b)` methods.
  - `Tv1.Backlight` — `Backlight` (ro) property; `SetBacklight(y)`
    method.

- Line ~704 ("Write commands via writable property setters …
  auto-emit `PropertiesChanged` on success via zbus machinery"):
  rewrite to describe the new flow. Single emit path via
  state_forwarder. No auto-emit race.

`.claude/CLAUDE.md`:

- The "Writable properties (set via Properties.Set)" section under
  "Testing with busctl" needs each `set-property` line replaced with
  a `call` line:

  ```bash
  # OLD
  busctl --user set-property org.tisvlay.Daemon /org/tisvlay/Tv/X \
      org.tisvlay.Tv1.Volume Volume y 30

  # NEW
  busctl --user call org.tisvlay.Daemon /org/tisvlay/Tv/X \
      org.tisvlay.Tv1.Volume SetVolume y 30
  ```

  Same translation for `Muted` → `SetMuted` and `Backlight` →
  `SetBacklight`. For Screen, the property name (`IsScreenOn`) and
  the method name (`SetScreen`) differ — the read line stays
  `get-property … IsScreenOn`, the write line becomes
  `call … org.tisvlay.Tv1.Screen SetScreen b true`.

- Update the section heading from "Writable properties (set via
  `Properties.Set`)" to "Writable state (set via methods)".

`crates/tisvlayd/src/dbus/tv.rs` doc-block:

- The file-level doc-comment (top of file) — if it mentions writable
  properties, update.

## Test plan

### Existing tests that must still pass

- `crates/tisvlayd/tests/dbus_contract.rs::set_volume_round_trips_through_dbus` —
  client.set_volume(75) → settle → client.get_volume() == 75. The
  internal proxy call shape changes, the test pattern doesn't.
- `crates/tisvlayd/tests/dbus_contract.rs::set_backlight_round_trips_through_dbus` —
  same.
- `crates/tisvlayd/tests/dbus_contract.rs::set_mute_round_trips_through_dbus` —
  same.
- `crates/tisvlayd/tests/backlight_emit_ordering.rs` — was the
  regression test for the auto-emit race. After the refactor, the
  bug pattern is structurally impossible (no setter, no auto-emit).
  Test passes by construction. Keep the test; rewrite the docstring
  to frame it as "state_forwarder is the only emit path" rather than
  "no pre-cache leak".
- `crates/tisvlayd/tests/scroll_apply_under_ramp.rs` — exercises
  `Reconciler` pattern with the `Coalescer` apply. Unchanged.
- `tisvlay-core::dbus_client::reconciler_tests` — unit tests for
  Reconciler. Unchanged.
- All other dbus_contract tests (initial state, switch_input, etc.) —
  unchanged.

### Manual QA after the refactor

- GUI volume slider drag (forward and reverse during a TV ramp):
  same feel as today — no regression.
- GUI backlight slider drag (forward and reverse): same.
- Tray scroll wheel (forward fast then reverse small):
  back-scroll lands near intent (the post-Reconciler behavior we
  just shipped). Reverify on backlight (the LG ramp case).
- TV remote → GUI propagation: external changes still reach the
  slider after convergence.
- `busctl call ... SetVolume y 50`: works.
- `busctl set-property ... Volume y 50`: errors with
  `org.freedesktop.DBus.Error.PropertyReadOnly` (correct).

### Failure verification

Before committing, optionally re-introduce the pre-cache write on
one setter in a throwaway diff and confirm:

- `backlight_emit_ordering.rs` still passes (because there's no
  setter being annotated with `#[zbus(property)]` to auto-emit
  from). Confirms the architectural fix isn't dependent on the
  pre-cache hack.

## What to remove (cleanup checklist)

- [ ] `set_is_screen_on` "no pre-cache" comment block.
- [ ] `set_volume` "no pre-cache" comment.
- [ ] `set_muted` "no pre-cache" comment.
- [ ] `set_backlight` long "no pre-cache" rationale block.
- [ ] `TvVolumeService` struct doc-comment "writable properties"
  language.
- [ ] `TvBacklightService` struct doc-comment "writable property"
  language.
- [ ] `TvScreenService` struct doc-comment "writable" language.
- [ ] `backlight_emit_ordering.rs` module docstring rewrite.
- [ ] `docs/design.md` lines ~254-260 (interface table) and ~704
  (write commands paragraph).
- [ ] `.claude/CLAUDE.md` busctl examples for writable properties.
- [ ] `docs/plan2.md` close-out note.
- [ ] `docs/todo.md` / `docs/bugs.md` — sweep for any references to
  the auto-emit issue.

## What to keep (rationale)

- **`Reconciler<V>`** + unit tests — solves delta-dispatch intent
  gating during TV ramps; unrelated to the daemon's emit topology.
- **`OptimisticSlider`** in widgets.slint — slider thumb gating
  during ramps in the GUI. Same logic in Slint.
- **`ScrollReconcilers`** in tray — per-(tv_id, property) reconcilers
  for scroll dispatch.
- **Mock async ramp** (`backlight_ramp_step_delay_ms`,
  spawned-task push) — necessary for the `scroll_apply_under_ramp`
  regression test and any future tests that need to interleave
  commands during a ramp.
- **`Coalescer<K, V>`** — orthogonal. Sum-merge for tray, last-wins
  for GUI.
- **`RefreshTrigger`** — orthogonal. State-watcher consumer pattern.
- **`backlight_emit_ordering.rs`** — guards the invariant that
  `state_forwarder` is the only emit path. Still relevant; the test
  now passes structurally rather than by workaround.

## Decisions

- **External tools / scripts**: none in this workspace use
  `busctl set-property` against these interfaces today, and there
  are no external API consumers. Free to change the surface.
- **Method receiver**: `&self` (not `&mut self`). Avoids the zbus
  hang / serialize-on-mut behavior; concurrency floor is
  TvManager's per-TV mutex, which is the right granularity.
- **Screen method name**: `Tv1.Screen.SetScreen(b)` (not
  `SetIsScreenOn`). Drops the awkward "Is…On" suffix; reads as a
  verb on the interface, matches the shape of other Set* methods.
  The property name `IsScreenOn` stays — boolean predicate naming
  is fine for getters.

## Commit strategy

Two commits:

1. **Code refactor**: daemon impls + proxy traits + DaemonClient
   internals + comment cleanup in `dbus/tv.rs` and the test files.
   Verifies via `just check`. Single conceptual change ("D-Bus
   surface: writable state via methods").
2. **Doc sweep**: `docs/design.md`, `.claude/CLAUDE.md`,
   `docs/plan2.md` close-out, `docs/plan3.md` close-out (mark this
   plan as done). Pure docs.

Splitting keeps the code-review surface clean — the refactor
reviewer reads a code-only diff, the doc reviewer reads a
doc-only diff.

Suggested commit messages:

```
refactor(daemon): writable state via methods, properties read-only

Convert Tv1.Volume.Volume / Volume.Muted / Backlight.Backlight /
Screen.IsScreenOn from `readwrite` properties to read-only
properties + Set methods (SetVolume, SetMuted, SetBacklight,
SetScreen). Aligns with the conventional D-Bus pattern (UPower,
GNOME settings, BlueZ, NetworkManager) and zbus's own
screen-brightness example.

Eliminates the auto-emit-on-Set vs. state_forwarder race that
motivated the "drop pre-cache" workarounds — state_forwarder is
now the single authoritative emit path by construction. Method
receivers switch from `&mut self` to `&self` so concurrent calls
into the same interface no longer queue behind a slow setter
(notably backlight via Luna). Reconciler / OptimisticSlider stay
— those solve delta-dispatch gating during TV ramps, an
orthogonal client-side concern.
```

```
docs: writable state surface (methods, not Properties.Set)

Sweep follow-up to the D-Bus surface refactor. Updates
docs/design.md interface table + write-path narrative,
.claude/CLAUDE.md busctl examples, and closes out plans 2 and 3.
```

## Sequence (for execution)

1. Daemon: drop `#[zbus(property)]` from the four setters.
2. Daemon: change `&mut self` → `&self` on those methods.
3. Daemon: remove the "no pre-cache" comment blocks.
4. Daemon: update struct doc-comments on `TvVolumeService`,
   `TvBacklightService`, `TvScreenService`.
5. Proxy: drop `#[zbus(property)]` from the four `set_*` declarations
   in the proxy traits.
6. `cargo build --workspace --all-features` — verify compiles.
7. `cargo test -p tisvlay-core --lib reconciler_tests` — unit tests.
8. `cargo test -p tisvlayd --features mock --test dbus_contract` —
   round-trip tests.
9. `cargo test -p tisvlayd --features mock --test backlight_emit_ordering` —
   should still pass.
10. `cargo test -p tisvlayd --features mock --test scroll_apply_under_ramp` —
    should still pass.
11. `just check` — full lint/build/test sweep.
12. `docs/design.md` updates.
13. `.claude/CLAUDE.md` busctl examples.
14. `crates/tisvlayd/tests/backlight_emit_ordering.rs` docstring.
15. `docs/plan2.md` close-out note.
16. Manual QA: GUI sliders, tray scroll, busctl read+call, busctl
    set-property fails appropriately.
17. Single commit.

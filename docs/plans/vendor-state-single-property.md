# Vendor selection: collapse parallel id+name state to a single index

> **Status:** drafted 2026-05-09, not started. Replaces the `manual-vendor` (id) + `manual-vendor-name` (display) parallel-property pattern in the pairing wizard with a single `manual-vendor-index` int, using `current-index <=>` two-way binding on the std `ComboBox`. Eliminates the audit-table-as-invariant burden that the [`vendor-display-single-source.md`](./vendor-display-single-source.md) plan introduced and that this session's `reset_wizard` desync bug exposed.

Picked up after a state-tracking bug in `reset_wizard`: the function cleared `manual-vendor` (id) but left `manual-vendor-name` (display) stale. The ComboBox renders from the name, so it kept showing the user's prior pick while the wizard internally carried whatever id reset hardcoded. Audit of every `set_manual_vendor` / `set_selected_vendor` site found `reset_wizard` was the only unpaired site — but the *invariant itself* ("Rust must update both at every setter") is fragile, and the per-site audit is a load-bearing discipline rather than a structural property.

## Source-of-truth audit

| Site | Today | Should be |
|---|---|---|
| `pairing_wizard.slint:122` (global) | `in-out property <string> manual-vendor;` | (drop) |
| `pairing_wizard.slint:125` (global) | `in-out property <string> manual-vendor-name;` | (drop) |
| `pairing_wizard.slint:139` (global) | `callback manual-vendor-edited(string);` | (drop — two-way `<=>` replaces it) |
| `pairing_wizard.slint:237-244` (component) | `manual-vendor` + `manual-vendor-name` `in property` + `manual-vendor-edited` callback declarations | replaced by `in-out property <int> manual-vendor-index;` |
| `pairing_wizard.slint:434-435,438` (StepDiscover static-chip) | `text: ... ? ... : root.manual-vendor-name;` (display in 0/1-vendor case) | read from `supported-vendors[manual-vendor-index].name` (or empty if model is empty) |
| `pairing_wizard.slint:456-459` (StepDiscover ComboBox) | `current-value: root.manual-vendor-name;` + `selected(v) => { root.manual-vendor-edited(v); }` | `current-index <=> root.manual-vendor-index;` (no `selected()` handler needed) |
| `pairing_wizard.slint:952,956` (parent wiring) | `manual-vendor <=> ...` + callback forward | replaced by `manual-vendor-index <=> ...` |
| `lib.rs:965-984` (`on_manual_vendor_edited`) | Rust handler that fires on user select, resolves display→id, sets both properties | (drop — two-way binding propagates the index automatically) |
| `lib.rs:1233-1245` (`on_manual_add`) | `let mut vendor = wizard.get_manual_vendor().to_string(); ... if vendor.is_empty() { /* fall back to supported[0] */ }` | `let idx = wizard.get_manual_vendor_index(); supported.row_data(idx as usize)?.id.to_string()` — no fallback needed (index always valid for non-empty model) |
| `state_watch.rs:84-89` (initial seed) | `if get_manual_vendor().is_empty() { set_manual_vendor(first.id); set_manual_vendor_name(first.name); }` | `wizard.set_manual_vendor_index(0);` (after `supported-vendors` is published) |
| `wizard.rs:75-92` (reset_wizard) | Reset both id+name to first supported (the bug site we just fixed) | `wizard.set_manual_vendor_index(0);` — single line, no audit-table burden |

Net: from two parallel string properties + one callback + ~12 setter sites that have to update both → one int property + zero callback handlers + ~3 setter sites. The audit-table-as-invariant collapses into a structural guarantee from Slint's two-way binding semantics.

## The architectural complications

Three constraints surfaced in research that shaped the design:

**1. `current-value` is broken as a binding source on std `ComboBox`.** [`combobox-base.slint:10-11`](https://github.com/slint-ui/slint/blob/master/internal/compiler/widgets/common/combobox-base.slint) defines `current-value` as a derived expression off `current-index`/`model`, but `update-current-value()` *imperatively overwrites* it on user select — clobbering any binding like `current-value: vendor-display-for(id)`. After the first user click the binding is dead and `current-value` becomes plain state. **`current-index` is the canonical, two-way-safe binding point; `current-value` is a string-typed convenience.** This rules out the "id state + pure callback for display" pattern that the [`vendor-display-single-source.md`](./vendor-display-single-source.md) plan briefly considered.

**2. Slint's dependency tracker is shallow inside Rust-bound callbacks.** Bindings track properties accessed in the binding *expression*, not data the Rust closure captures. A pure callback `vendor-display-for(id)` whose closure scans `supported-vendors` would silently go stale if the model is mutated without `id` also changing. Workaround would be a "salt" property bumped on model republish — extra plumbing. (See [issue #9551](https://github.com/slint-ui/slint/issues/9551), open in 1.16.) Avoiding the pure-callback path sidesteps this.

**3. Slint's idiomatic pattern is `current-index` + struct model**, not pure-callback derivation. Upstream's [`examples/layouts/flexbox-interactive.slint`](https://github.com/slint-ui/slint/blob/master/examples/layouts/flexbox-interactive.slint) uses exactly this shape: array of structs as the model, single index as canonical state, `model[index].field` reads for derived values, no Rust callback for the lookup. We already publish `supported-vendors` as `[VendorOption { id, name }]`; this plan just picks index over id as the wizard-side state.

## Steps

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `in-out property <int> manual-vendor-index: 0;` to the `PairingWizard` global in `ui/globals/pairing_wizard.slint`. Drop `manual-vendor`, `manual-vendor-name`, and the `manual-vendor-edited` callback declaration. | todo | 0 is a safe default — even the empty-model case is handled by the static-chip branch (it doesn't read the index). |
| 2 | Mirror the property changes on the `PairingWizardPane` component in `ui/components/pairing_wizard.slint`. Replace the parent `<=>` wiring (`manual-vendor <=> ...`, `manual-vendor-name <=> ...`, callback forward) with a single `manual-vendor-index <=> PairingWizard.manual-vendor-index`. | todo | Per `slint.md`, missing prop forwards silently degrade to default-valued in props — grep ancestors after editing. |
| 3 | StepDiscover ComboBox: change `current-value: root.manual-vendor-name;` + `selected(v) => { root.manual-vendor-edited(v); }` to `current-index <=> root.manual-vendor-index;`. Drop the callback. | todo | Two-way binding propagates user pick back to the global without a Rust handler. |
| 4 | StepDiscover static-chip (≤1 vendor): replace the `: root.manual-vendor-name` text branch with `: root.supported-vendors[root.manual-vendor-index].name` for the 1-vendor case, empty text for the 0-vendor case. | todo | The 0-vendor case shouldn't render the wizard at all in practice, but keep it safe. |
| 5 | Drop `wizard.on_manual_vendor_edited(...)` handler in `lib.rs` (~lines 965-984). | todo | Callback declaration is gone; handler is dead. |
| 6 | Update `on_manual_add` in `lib.rs` (~line 1233): replace `get_manual_vendor()` + empty-id fallback with `get_manual_vendor_index()` + `supported.row_data(idx as usize)`. Read `id` and `name` from the row; use `id` for `pair_tv` dispatch, `name` for the paired `set_selected_vendor_name(...)` call. | todo | The `vendor.is_empty()` early-return becomes "if `row_data` returns None, log + return" — symmetric defensiveness. |
| 7 | Update the seed in `state_watch.rs::initial_fetch_and_watch` (~line 84): replace the `manual-vendor.is_empty()`-guarded `set_manual_vendor` + `set_manual_vendor_name` pair with a single `wizard.set_manual_vendor_index(0)`. | todo | The is_empty guard goes away — index 0 is the right answer whether this is first publish or a re-seed. |
| 8 | Update `reset_wizard` in `wizard.rs` (~lines 75-92): replace the `supported-vendors`-scanning id+name reset block with a single `wizard.set_manual_vendor_index(0);`. | todo | This is the file that motivated the whole refactor — it shrinks from ~17 lines to 1. |
| 9 | Audit grep: search the workspace for any remaining `manual-vendor\b` / `manual_vendor\b` / `manual-vendor-name` / `get_manual_vendor` / `set_manual_vendor` references. | todo | After steps 1-8 land, only `manual-vendor-index` should remain. |
| 10 | Smoke test on a Wayland session: pair flow from cold start (default index 0 selected), pick a non-default vendor, cancel, reopen (should reset to index 0), pair through manual flow with both `lg` and `mock`. | todo (user) | Verifies the bug we just fixed stays fixed and that the two-way binding round-trips correctly. |

## Risks / unknowns

- **`current-index` default value**: setting `in-out property <int> manual-vendor-index: 0;` means the ComboBox shows index 0 as selected even before `supported-vendors` is published. During the brief startup race (state_watch's first fetch), the model is empty and `supported-vendor-names[0]` would be empty. The ComboBox renders blank for that window; it's the same behavior as today's empty-`manual-vendor-name` startup window. Acceptable.
- **Two-way `<=>` binding direction on `current-index`**: per research, std `ComboBox`'s `current-index` is `in-out property <int>` and survives user-interaction-driven imperative writes (unlike `current-value`). Need to verify in practice that `current-index <=> manual-vendor-index` round-trips both directions: Rust setting the index updates the ComboBox display, and user select updates `manual-vendor-index`. Step 10's smoke test covers this.
- **Index stability under model re-publish**: if `supported-vendors` is ever re-published (today: never; future: dynamic plugins, sort changes, locale-driven re-order), the index silently points to a different vendor. Today's static-once-at-startup invariant means this is theoretical, but it's the asymmetry that "id-as-canonical-state" handled for free. **Out of scope for this refactor**; if dynamic plugins land, add a Rust sidecar `Option<String>` "intended id" and a re-resolve routine on model republish (one well-localized place, not an audit table).
- **`selected-vendor` is unchanged**: the discovery-picked TV's vendor still uses the `selected-vendor` (id) + `selected-vendor-name` (display) parallel-property pattern. See *Out of scope* below.

## Out of scope

- **`selected-vendor` parallel-property pattern**: same shape as `manual-vendor` (id + display, both set by Rust at every site) but materially less fragile — it's a read-only display in StepConfigure / StepDone, not a ComboBox-driven input, and the audit found zero unpaired sites. The cost/value of refactoring is low/low. Re-evaluate after this refactor lands; if the index pattern works cleanly in practice, applying it for uniformity is a small follow-up. If audit-table discipline holds, leave `selected-vendor` alone.
- **`tv-vendor` in StepDone**: bound from `selected-vendor-name` via parent wiring; would change in lockstep with the `selected-vendor` follow-up if/when we do it.
- **System-profile ComboBox** in StepConfigure (`pairing_wizard.slint:620`): also a single-string `current-value` binding, not a paired-state pattern. Different fragility profile, not covered here.
- **Dynamic plugins**: index-stability handling deferred until the daemon actually supports dynamic vendor backends. Today's `supported-vendors` is a static-once-at-startup model.

## References

- [`docs/plans/vendor-display-single-source.md`](./vendor-display-single-source.md) — the predecessor refactor that introduced `manual-vendor-name` as a parallel property; this plan finishes the job by collapsing the pair into a single index.
- [`crates/tisvlay-gui/src/wizard.rs::reset_wizard`](../../crates/tisvlay-gui/src/wizard.rs) — the function that motivated the refactor (id+name desync bug fixed earlier this session).
- [`crates/tisvlay-gui/src/state_watch.rs::initial_fetch_and_watch`](../../crates/tisvlay-gui/src/state_watch.rs) — where `supported-vendors` is published into the wizard global.
- Slint reactivity reference: [`reactivity.mdx`](https://github.com/slint-ui/slint/blob/master/docs/astro/src/content/docs/guide/language/concepts/reactivity.mdx) (purity rules, dependency tracking semantics).
- Slint std-widgets `ComboBox`: [`combobox-base.slint`](https://github.com/slint-ui/slint/blob/master/internal/compiler/widgets/common/combobox-base.slint) (the `update-current-value()` clobber rationale).
- Slint upstream example for `current-index` + struct model: [`examples/layouts/flexbox-interactive.slint`](https://github.com/slint-ui/slint/blob/master/examples/layouts/flexbox-interactive.slint).
- Open Slint issue [#9551](https://github.com/slint-ui/slint/issues/9551) — callback-rebind doesn't dirty existing bindings (one of the reasons we avoided the pure-callback path).

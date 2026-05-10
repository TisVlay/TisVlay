# Vendor display-name single source of truth

> **Status:** drafted 2026-05-09, not started. Three Slint sites in `pairing_wizard.slint` hardcode an `id → display-name` mapping that already exists upstream as backend `VENDOR_NAME` constants and is shipped to the GUI via `Manager1.GetSupportedVendors`. Replace the hardcoded mappings with parallel name properties driven by the `supported-vendors` model.

Picked up while reviewing the `feat/gui-mock-pair` branch. The cosmetic fix in StepConfigure (added the `mock` branch to one of the hardcoded mappings) exposed that the same mapping is duplicated three times in the Slint source. The right fix is to stop duplicating, not to keep adding branches every time a backend lands or gets renamed.

## Source-of-truth audit

| Site | Today | Should be |
|---|---|---|
| `tisvlay-mock/src/lib.rs:66` | `pub const VENDOR_NAME = "Mock (testing)"` | (canonical) |
| `tisvlay-lg/src/lib.rs:74` | `pub const VENDOR_NAME = "LG WebOS"` | (canonical) |
| `tisvlay-backends/src/lib.rs:381` | registers each backend with `tisvlay_<vendor>::VENDOR_NAME` | (canonical) |
| `Manager1.GetSupportedVendors` over D-Bus | returns `(id, name)` pairs from above | (canonical, shipped to GUI) |
| `pairing_wizard.slint:246-252` | `pure function vendor-display(id)` with hardcoded mapping | drop, use lookup |
| `pairing_wizard.slint:531-535` | `property <string> vendor-display:` ternary chain in StepConfigure | drop, use lookup |
| `pairing_wizard.slint:861` | inline `tv-vendor == "lg" ? "LG WebOS" : …` in StepPair | drop, use lookup |

Net: the GUI already has the data — the supported-vendors model is populated from `GetSupportedVendors` in `state_watch.rs::initial_fetch_and_watch` — but isn't using it for display, and the Slint duplicates have already drifted: the StepConfigure mapping was missing the `mock` branch entirely until the cosmetic fix that triggered this plan, and the busctl output captured during recent manual testing showed `"Mock (in-memory)"` (a stale daemon build) where the live const says `"Mock (testing)"`. Single-source-of-truth would have caught both.

## The architectural complication

Slint's pure-binding context can't iterate a model. You can write `model[i]` but there's no `find` / `position` helper to map id → index, so a `vendor-name-for(id)` pure function in Slint can't scan `supported-vendors`. The lookup has to happen in Rust, with the result stored in a property the binding can read.

Two options for surface shape:

| Option | Mechanism | Pros | Cons |
|---|---|---|---|
| Parallel name properties | Add `manual-vendor-name` + `selected-vendor-name` (`in-out property <string>`) alongside the existing id properties; Rust sets both at every setter site | Minimal Slint surface change; names stay in sync via Rust ownership | Two-property updates at every setter site (5 total) |
| `pure callback` from Slint | Declare `pure callback vendor-name-for(string) -> string`; bind it in Rust | One Slint declaration, no parallel state | `pure callback` semantics for non-trivial lookups against a model are awkward; binding-context safety needs care |

**Going with the parallel-property option** — it matches the pattern already used for other `(id, display)` pairs in the wizard (e.g., the supported-vendors model itself stores both fields), keeps Slint dumb, and the setter-site updates are mechanical.

## Steps

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `manual-vendor-name` + `selected-vendor-name` properties to the `PairingWizard` global in `ui/globals/pairing_wizard.slint` | done | Both added with comment-block rationale pointing at this plan. |
| 2 | Rust helper `vendor_display_for(model: &ModelRc<VendorOption>, id: &str) -> SharedString` | done | Lives next to `on_ui` in `lib.rs`. Empty-string short-circuit + id fallback keeps transient empty-model startup quiet. |
| 3 | Update each `set_manual_vendor(...)` site to also call `set_manual_vendor_name(...)` | done | Two sites: `state_watch.rs::initial_fetch_and_watch`, `lib.rs::on_manual_vendor_edited` (both the matched-row branch and the no-match fallback). |
| 4 | Update each `set_selected_vendor(...)` site to also call `set_selected_vendor_name(...)` | done | Three sites in `lib.rs`: discovery pick, on_selected_changed (reset + TV-from-list both), manual-add. |
| 5 | Slint: drop the `vendor-display` function in `PairingWizardPane` and the `vendor-display` projection in `StepConfigure`; drop the inline ternary in StepDone; replace each call site with the new name property | done | StepPair turned out not to have a hardcoded mapping — the duplicated mapping was actually in StepDone (the post-pair confirmation step). Re-reading the plan: that's the same logical "third site" the plan listed, just named correctly. Three call sites updated; three hardcoded mappings deleted. Wired the new properties through to PairingWizardPane / StepConfigure / StepDone via the parent's `<=>` and `:` bindings. |
| 6 | Smoke test on a Wayland session | todo (user) | Confirm wizard's vendor ComboBox + StepConfigure header chip + StepDone vendor row all render the daemon-supplied display name for both `mock` and `lg`. |
| 7 | Verify nothing else in the GUI hardcodes vendor display strings | done | Audit grep is clean: only matches are illustrative `e.g. "LG WebOS"` mentions in comments (one in `lib.rs`, one in the new property's doc comment). |

## Risks / unknowns

- **`set_*_name` ordering**: if the name property is set *before* `supported-vendors` is published, the lookup misses. The two properties want to be set in a single `on_ui` block where the model is already populated — `state_watch.rs::initial_fetch_and_watch` already does this for the seed; new setter sites need the same care.
- **Reset path** (`lib.rs:915` sets `selected_vendor` to default/empty): the name property should also reset to empty in lockstep.
- **Backend-side renames**: any future `VENDOR_NAME` change propagates through the daemon to the GUI without a recompile of slint files. Worth documenting in `docs/build.md` or `docs/design.md` so the convention sticks.

## Out of scope

- Refactoring the same pattern for other id→name mappings (e.g., `system-profile` → display label) — those are separate axes and don't have a backend-supplied name yet.
- Replacing `tv-vendor == "lg" ? …` patterns in `tvs_pane.slint` if any exist; those would extend the same fix once we confirm the wizard refactor pattern works.
- Adding a "vendor renamed" migration path — backend renames are a manual coordinated change and out of scope.

## References

- [`tisvlay-core/src/traits.rs::TvBackend::vendor_name`](../../crates/tisvlay-core/src/traits.rs) — trait method backends implement.
- [`tisvlay-backends/src/lib.rs`](../../crates/tisvlay-backends/src/lib.rs) — registry that ships the `(id, name)` pairs.
- [`crates/tisvlay-gui/src/state_watch.rs::initial_fetch_and_watch`](../../crates/tisvlay-gui/src/state_watch.rs) — where `supported-vendors` is published into the wizard global.
- Sibling docs: [`docs/build.md`](../build.md) (build/dep-graph posture).

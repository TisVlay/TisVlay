# Re-pair TV: refresh expired credentials without losing config

> **Status:** drafted 2026-05-10, not started. Sequenced *after* the [per-TV lifecycle correctness PR](./per-tv-lifecycle-correctness.md). Adds a `Tv1.RePair()` D-Bus method and a "Re-pair" GUI button (above Remove on the TV detail view) so users whose vendor credentials expire (LG WebOS client-key revocation, future Samsung token expiry, etc.) can refresh them without removing the TV and re-entering its name, profile, primary input, and managed flag from scratch.

## Context

Today, when a TV's vendor credentials become invalid — most commonly, an LG client_key revoked because the user clicked "Forget device" on the TV settings menu, or a TV firmware update that resets the WebOS auth store — the only recovery path through the GUI is:

1. `Tv1.Remove` the TV. Loses `name`, `profile.system`, `profile.user`, `managed`, `primary_input`, `automate_when` if overridden, `desktop_connector` mapping if matched.
2. `Manager1.AddTv` from scratch. User re-types the name, picks the profile from the dropdown, sets primary input, etc. Only then can they pair.

That's a ~30-second-of-clicking penalty for a transient credential-refresh that has nothing to do with the user-set config. The TV's identity (UUID, vendor, network address) and the operator's preferences (name, profile, etc.) all stay valid; only the credential field needs replacing.

**Re-pair** preserves every non-credentialing field and refreshes only what the backend's pairing handshake produces. UX: the user clicks a "Re-pair" button on the TV's detail view, accepts the prompt on the TV (LG shows a permission dialog with the controller name), the daemon stores the new credentials, and the TV reconnects. Existing pair-progress UI and signal infrastructure cover the live-feedback half.

## Design decisions

### DR1: D-Bus method placement → per-TV `Tv1.RePair()`

Re-pair targets a specific existing TV. Putting the method on `Tv1.*` (the per-TV interface) matches every other per-TV-targeted method (`Tv1.Reconnect`, `Tv1.UpdateConfig`, `Tv1.Power`, etc.). `Manager1.RePairTv(tv_id)` would force the client to look up the TV by id and would clutter the manager with per-instance methods.

### DR2: Async with PairingProgress signal (not synchronous)

Pair handshakes can take 10–30s in real conditions (TV asleep → WoL → boot → SSAP TLS handshake → user accepts on TV). A synchronous D-Bus call would block the client for that whole window — bad UX (frozen GUI) and risks D-Bus method timeout (default 25s).

Reuse the existing `PairingProgress { tv_id, phase, message }` signal that `Manager1.AddTv`'s flow already emits via `state_forwarder`. `Tv1.RePair()` returns immediately after kicking off the handshake; subsequent progress fires through the signal; success or failure surfaces as a final phase + a separate `PairingComplete { tv_id, success, error }` signal (or by the GUI watching `Connected` flip). Mirrors the wizard's existing async-pair UI, which already consumes these signals.

### DR3: Backend interface — reuse `start_pairing`, don't add a new trait method

The vendor backend trait's `start_pairing(vendor_config) -> vendor_config` is already idempotent against an existing config. LG's implementation submits the existing `client_key` first; if the TV recognizes it (most cases) the handshake completes immediately; if not, the TV prompts the user and the backend produces a fresh key. No backend-side changes needed for the LG case. Mock backend's `start_pairing` already accepts existing config + the `pairing` schema field's outcome — re-pair flows through the same path.

### DR4: Vendor-config merge → trust the backend's returned config

Backend's `start_pairing` returns a complete `vendor_config`. The daemon stores it as the TV's new vendor_config. Backends are expected to round-trip non-credentialing fields (address, port, user-typed values) unchanged through `start_pairing` — that's the existing implicit contract for AddTv.

If a future backend wants finer control over what re-pair preserves vs. replaces, we'd add a `PairingField::ephemeral: bool` flag to the schema (credentialing fields would be `ephemeral`; the daemon merges by overwriting only those). Out of scope for this plan; revisit if a backend requires it.

### DR5: WoL — re-use `pair_tv`-like wake-then-handshake flow

If the TV is in standby, re-pair should wake it before the SSAP handshake, same as AddTv does today via `pair_tv`. Implementation: extract the wake-then-pair logic from `pair_tv` into a private `wake_and_pair_with(vendor_config)` helper that both `pair_tv` (new-TV path) and `re_pair_tv` (existing-TV path) call.

### DR6: GUI surface — reuse the wizard's `StepPair` component

The pair-progress UI already lives in `crates/tisvlay-gui/ui/components/pairing_wizard.slint::StepPair` (live status, "press OK on TV" instructions, error display). For re-pair, render the same component but skip `StepDiscover` (we know the TV) and `StepConfigure` (we have the config) — the wizard opens directly to `StepPair` when invoked from a re-pair button. On success the wizard closes and a toast confirms. On failure the wizard shows the error and a Retry button.

The button itself: a small `IconBtn`-styled affordance in the TV detail edit-mode pane, immediately above the Remove button per the user's design intent. Icon: `view-refresh` (same family as the Reload sidebar icon — re-pair *is* a credential refresh).

### DR7: Cancel mid-re-pair — reuse the wizard's existing cancel flow

The wizard's cancel callback today aborts an in-flight `pair_tv`. For re-pair, cancel needs to stop the in-flight `start_pairing`. Backend trait already supports `cancel_pairing()`. Daemon-side, `Tv1.CancelPair()` is a per-TV method that maps to the backend's cancel; introduce it (was missing because AddTv's cancel was wizard-driven via `Manager1.CancelPair(tv_id)` — verify which exists today and reuse).

### DR8: Failure handling — leave existing config intact, propagate typed error

If the handshake fails (TV refuses, network unreachable, timeout), the existing `vendor_config` stays untouched on disk. `Tv1.RePair()` returns a typed error so the GUI can surface a specific message. Reuses the typed-error vocabulary from the per-TV lifecycle PR (`DaemonError::PairingFailed`, etc.) — no new variants needed unless re-pair surfaces a class of error AddTv doesn't.

After failure, the daemon attempts to reconnect with the *old* credentials — if the failure was transient (network blip, TV asleep), the connection comes back without user intervention; if the credentials are genuinely invalid, the next `is_connected` reads false and the user can try Re-pair again or fall back to Remove + Add.

### DR9: Concurrency — block re-pair if a pair is already in flight

If the user clicks Re-pair while a pair (initial or re-pair) is still running for the same TV, reject with `Tv1.RePair: already pairing`. Shared state lives on the TV's `ManagedTv` — already gated by the per-TV `Mutex<ManagedTv>` from the lifecycle plan; check a `pairing_in_flight: bool` field at the start of `re_pair_tv` and bail.

## Source-of-truth audit

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/tv_manager.rs::pair_tv` | Generates new UUID, creates TvConfig, calls backend's start_pairing, stores result | Extract a private `wake_and_pair_with(connector, vendor_config) -> vendor_config` helper that both `pair_tv` and the new `re_pair_tv` call. |
| `crates/tisvlayd/src/tv_manager.rs` | No `re_pair_tv` method | Add `pub async fn re_pair_tv(&self, tv_id: &str) -> Result<(), TvManagerError>`. Validates TV exists + not already pairing, closes existing connection, calls `wake_and_pair_with` against existing vendor_config, persists merged config, reconnects. |
| `crates/tisvlayd/src/dbus/tv.rs::TvService` | Tv1 interface — has `Reconnect`, `Remove`, `UpdateConfig`, `GetConfig` | Add `RePair() -> ()` D-Bus method. Async (returns after kicking off the handshake; progress via `Manager1.PairingProgress` signal). |
| `crates/tisvlayd/src/dbus/manager.rs` (or `tv.rs`) | `CancelPair(tv_id)` exists at Manager-level for AddTv | Verify whether to expose `Tv1.CancelPair()` as a per-TV alias or redirect re-pair cancel through the existing manager-level method. |
| `crates/tisvlay-gui/src/lib.rs` | Wizard is launched from sidebar's "Pair TV" button → `Daemon.request-pair` callback → `on_request_pair` opens the wizard at step 1 (Discover) | Add a `Tv.request-re-pair(tv_id)` callback. Handler resets the wizard, prefills `selected-vendor` / `selected-vendor-name` / `selected-address` / `tv-name` from the existing TV's config, jumps to `step = 3` (Pair), kicks off `Tv1.RePair(tv_id)`. |
| `crates/tisvlay-gui/ui/components/tvs_pane.slint` (TV detail edit-mode pane) | Has `Remove` button | Add `RePair` button immediately above Remove. Uses `Icons.reload` for now (same `view-refresh` icon used by the sidebar's Reload action — re-pair *is* a credential refresh). |
| `crates/tisvlay-gui/ui/globals/tv.slint` (or equivalent global) | No `request-re-pair(tv_id)` callback | Add it. Mirrors the existing `request-pair` callback shape. |
| `crates/tisvlay-mock/src/connector.rs::start_pairing` | Honors `pairing` schema field for new pairings | No change needed if the existing logic already accepts re-input vendor_config (verify in step 1). If not, extend so the same `accept` / `reject` / `timeout` / `manual` outcomes apply on the re-pair path. |

## Phase 1 steps — daemon side

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Verify `mock` and `lg` backends' `start_pairing` accept an already-credentialed vendor_config and round-trip it correctly. Add a unit test for each that confirms calling `start_pairing` twice in succession with the same input produces consistent output. | todo | If either backend assumes empty/default credentials on entry, that's a small fix to land before re-pair builds on it. |
| 2 | Extract `wake_and_pair_with(connector, vendor_config)` helper from `tv_manager::pair_tv`. Refactor `pair_tv` to call it. Verify by re-running the existing pair-flow tests. | todo | Pure refactor — no behavior change. Sets up the shared codepath for `re_pair_tv`. |
| 3 | Add `pairing_in_flight: bool` field to `ManagedTv`, set true around any pair call (existing `pair_tv` and the new re-pair), reset to false on completion or error. | todo | Concurrency gate per DR9. |
| 4 | Implement `tv_manager::re_pair_tv(tv_id)`. Validates state, closes connection, calls `wake_and_pair_with(existing_vendor_config)`, merges result into TvConfig, persists via `disk_write_lock`, triggers reconnect. | todo | Mirrors `pair_tv`'s structure but operates on existing TV instead of creating a new one. |
| 5 | Add `Tv1.RePair()` D-Bus method to `TvService` in `dbus/tv.rs`. Async-style: kicks off `re_pair_tv` in a tokio task, returns Ok immediately, progress goes through the existing `PairingProgress` signal. Failures route to `DaemonError::PairingFailed`. | todo | Symmetric with how `Manager1.AddTv` returns immediately and uses signals for progress. |
| 6 | Audit whether `Tv1.CancelPair()` should be a new per-TV alias or whether re-pair cancel routes through the existing `Manager1.CancelPair(tv_id)`. Pick one; document why. | todo | Lean toward per-TV alias for symmetry — Tv1.RePair lives on the per-TV path; pairing the cancel there too is consistent. Cheap to add. |
| 7 | Two regression tests in `crates/tisvlayd/tests/`: (a) `re_pair_preserves_top_level_config.rs` — set up a TV with name + profile + managed, drive re-pair through the mock's `pairing = "accept"` flow, verify the top-level TvConfig fields stay untouched while vendor_config is updated. (b) `re_pair_blocks_when_already_pairing.rs` — start an in-flight pair (mock's `pairing = "manual"`), call re-pair, expect `DaemonError::PairingFailed("...already pairing...")` or similar. | todo | Mock-driven, deterministic. |

## Phase 2 steps — GUI side

| # | Step | Status | Notes |
|---|---|---|---|
| 8 | Add `request-re-pair(string)` callback on the appropriate global (`Daemon` or a new per-TV global), with a Rust handler that prefills the wizard from existing TV state and kicks off `Tv1.RePair(tv_id)`. | todo | Reuse `WizardEntry`-shaped state. The wizard's existing pair-progress UI already consumes `PairingProgress` signals — no GUI-side signal-handling refactor needed. |
| 9 | Add `RePair` button (`IconBtn` style) to the TV detail edit-mode pane in `tvs_pane.slint`, immediately above Remove. Icon: `Icons.reload`. Accessible-label: "Re-pair this TV (refresh credentials)". Disabled while `connected = false` *and* the TV doesn't support WoL — re-pair needs the TV reachable, and we can't wake it without a MAC. | todo | The disabled-state condition mirrors the per-TV `power-on` button's gating logic. |
| 10 | Confirm dialog: re-pair disrupts the active connection and requires user interaction on the TV. Pop a `ThemedConfirm`-style dialog ("Re-pair Living Room? This will disconnect briefly and prompt you on the TV.") with Yes/Cancel before kicking off `Tv1.RePair`. | todo | Cheap UX safeguard — re-pair is destructive for the active connection. |
| 11 | Wizard flow: on `request-re-pair(tv_id)`, the wizard opens directly at `step = 3` (`StepPair`) with vendor / address / name / profile prefilled. `StepDiscover` and `StepConfigure` are skipped. Cancel during pair routes through the existing wizard cancel callback, which the new `Tv1.CancelPair` (or manager-level cancel) backs. | todo | Most of the work is wiring the prefill — the existing wizard already handles step=3 correctly. |
| 12 | Smoke test on a Wayland session: pair a real LG TV, click the TV's "Forget device" menu item, verify the GUI's connection drops, click Re-pair, confirm the dialog, verify the TV prompts for permission, accept on the TV, verify the GUI reconnects with new credentials. Also test against the mock backend with `pairing = "reject"` and `pairing = "timeout"` to verify the error surface. | todo (user) | Real-hardware test needs an LG; mock-backend tests cover the daemon-side surface deterministically via step 7. |

## Phase 3 steps — i18n + final polish

| # | Step | Status | Notes |
|---|---|---|---|
| 13 | `just i18n-extract` to refresh the gettext catalog with new strings ("Re-pair", confirm-dialog text, error messages). | todo | Per CLAUDE.md i18n workflow. |
| 14 | `prek run -a` and `just check` end-to-end gate. | todo | Per `feedback_lint_and_test_before_commit.md`. |
| 15 | Update `docs/design.md` — add a brief mention of `Tv1.RePair()` to the D-Bus interface section. | todo | Mirrors how `Tv1.Reconnect` and `Tv1.Remove` are documented. |

## Risks / unknowns

- **Backend's `start_pairing` invariants for re-input vendor_config** (step 1): if the LG backend assumes the input vendor_config is uncredentialed (pre-pair state) and overwrites the existing client_key with a fresh handshake, that's expected. If it errors on already-credentialed input, that's a backend fix to land first. Verify in step 1.
- **WoL latency makes re-pair a multi-second operation**: the user clicks Re-pair, sees a progress UI, waits up to ~30s for the TV to wake + handshake. The existing pair-progress UI handles this; no new mechanism. Worth a comment noting the expected duration so future readers don't assume re-pair is instant.
- **Persisted config rollback on cancel**: if the user cancels mid-re-pair, the daemon's `cancel_pairing` is best-effort; the backend may have already accepted the new credentials TV-side. In that case the daemon's `vendor_config` doesn't get updated (we cancel before persisting), so on next reconnect the daemon uses the *old* key, which the TV may now reject. Edge case; document as "if cancel-after-TV-accept happens, manual Re-pair recovers." Not worth special-casing.
- **Backend trait surface for `cancel_pairing` already exists** but we should verify it's wired through `tv_manager` for the AddTv flow today before relying on it for re-pair (step 6).
- **GUI button placement vs. edit-mode toggle**: the user specified "above Remove button". Remove sits inside the edit-mode pane today; re-pair would too. Worth confirming during step 9 that re-pair shouldn't *also* surface outside edit-mode (e.g., when the GUI shows "Connection failed" state) — that'd be a discoverability win but adds a second placement to maintain.

## Out of scope

- **Backend-driven "ephemeral field" schema flag** (DR4 alternative): defer until a backend requires it.
- **Re-pair via Tray menu**: re-pair is a low-frequency action with a multi-second progress UI; the tray's icon-and-tooltip surface isn't a great fit. Skipped; document the GUI as the canonical re-pair affordance.
- **Bulk re-pair across all TVs** (e.g., "Re-pair all" after a network move): out of scope; users can re-pair each TV individually if they need to.
- **Periodic auto-re-pair before credential expiry**: vendors don't reliably advertise expiry timestamps. Reactive re-pair (user clicks when broken) is the right model for v1.

## References

- [`docs/plans/per-tv-lifecycle-correctness.md`](./per-tv-lifecycle-correctness.md) — the prerequisite plan; provides the typed-error surface (`DaemonError::PairingFailed`, etc.) re-pair's failure path uses, and the `pairing_in_flight` concurrency gate.
- [`crates/tisvlayd/src/tv_manager.rs::pair_tv`](../../crates/tisvlayd/src/tv_manager.rs) — extracted `wake_and_pair_with` helper sets up shared codepath.
- [`crates/tisvlay-core/src/traits.rs::TvBackend::start_pairing`](../../crates/tisvlay-core/src/traits.rs) — backend-side pair entrypoint, reused unchanged.
- [`crates/tisvlay-gui/ui/components/pairing_wizard.slint::StepPair`](../../crates/tisvlay-gui/ui/components/pairing_wizard.slint) — pair-progress UI reused for re-pair without modification.
- [`crates/tisvlayd/src/state_forwarder.rs::PairingProgress`](../../crates/tisvlayd/src/state_forwarder.rs) — signal infrastructure that re-pair's progress feedback flows through.

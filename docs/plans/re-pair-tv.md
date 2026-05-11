# Re-pair TV: refresh expired credentials without losing config

> **Status:** delivered 2026-05-10; Wayland smoke test pending. Sequenced *after* the [per-TV lifecycle correctness PR](./per-tv-lifecycle-correctness.md) — landed in the same branch since both surfaces overlap (`pairing_in_flight` gate composes with the per-TV `Mutex<ManagedTv>` discipline that lifecycle plan documented). Adds a `Tv1.RePair()` D-Bus method and a "Re-pair" GUI button (above Remove on the TV detail view) so users whose vendor credentials expire (LG WebOS client-key revocation, future Samsung token expiry, etc.) can refresh them without removing the TV and re-entering its name, profile, primary input, and managed flag from scratch.

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

**Asymmetry with AddTv (acknowledged, not fixed here).** `Manager1.AddTv` today is *blocking*: its D-Bus handler awaits `pair_tv → session.wait_for_completion` for the full pair window, returning `(path, tv_id)` only after success. This is a latent design issue (frozen client surface during pair, D-Bus timeout risk) but it's the established contract and changing it would alter when ObjectManager publishes the new path — a behavior-changing refactor with its own risks. Re-pair gets the *better* shape (async + signals) because it's greenfield; AddTv stays blocking. A future cleanup could unify both onto the async-with-signals pattern, but it's out of scope here. The `Manager1.CancelPair(tv_id)` introduced in DR7 below works correctly against either shape — it just sends a Cancel command via the session's channel.

### DR3: Backend interface — reuse `start_pairing`, accept TV-prompt every re-pair

The vendor backend trait's `start_pairing(vendor_config) -> PairingSession` is called for re-pair with the existing credentialed vendor_config; the backend internally ignores existing credentials and performs a fresh handshake. For LG specifically, [`backend.rs:600`](../../crates/tisvlay-lg/src/backend.rs#L600) calls `WebOsClient::connect(&ip, None)` — the second arg (existing client_key) is hardcoded `None`. So **every re-pair triggers a fresh permission prompt on the TV regardless of the old key's validity**, and the old key is overwritten by whatever the handshake produces.

For v1 this is acceptable. The use case driving re-pair (creds expired or were revoked at the TV) implies the old key is dead anyway, so "always re-prompt" matches the user's mental model. If we ever want "try the existing key silently, fall back to prompt only on rejection", that's a backend-trait extension (`try_silent_pair(vendor_config)` or a flag on `start_pairing`) — out of scope for this plan, file as an idea if/when a backend supports it.

Mock backend's `start_pairing` already accepts existing config + the `pairing` schema field's outcome — re-pair flows through the same path for deterministic GUI testing.

### DR4: Vendor-config merge → trust the backend's returned config

Backend's `start_pairing` returns a complete `vendor_config`. The daemon stores it as the TV's new vendor_config. Backends are expected to round-trip non-credentialing fields (address, port, user-typed values) unchanged through `start_pairing` — that's the existing implicit contract for AddTv.

If a future backend wants finer control over what re-pair preserves vs. replaces, we'd add a `PairingField::ephemeral: bool` flag to the schema (credentialing fields would be `ephemeral`; the daemon merges by overwriting only those). Out of scope for this plan; revisit if a backend requires it.

### DR5: WoL — re-use `pair_tv`-like wake-then-handshake flow

If the TV is in standby, re-pair should wake it before the SSAP handshake, same as AddTv does today via `pair_tv`. Implementation: extract the wake-then-pair logic from `pair_tv` into a private `wake_and_pair_with(vendor_config)` helper that both `pair_tv` (new-TV path) and `re_pair_tv` (existing-TV path) call.

### DR6: GUI surface — reuse the wizard's `StepPair` component

The pair-progress UI already lives in `crates/tisvlay-gui/ui/components/pairing_wizard.slint::StepPair` (live status, "press OK on TV" instructions, error display). For re-pair, render the same component but skip `StepDiscover` (we know the TV) and `StepConfigure` (we have the config) — the wizard opens directly to `StepPair` when invoked from a re-pair button. On success the wizard closes and a toast confirms. On failure the wizard shows the error and a Retry button.

The button itself: a small `IconBtn`-styled affordance in the TV detail edit-mode pane, immediately above the Remove button per the user's design intent. Icon: `view-refresh` (same family as the Reload sidebar icon — re-pair *is* a credential refresh).

### DR7: Cancel mid-pair — add `Manager1.CancelPair(tv_id)` covering both AddTv and re-pair

Code reading revealed: **no `CancelPair` D-Bus method exists today**. The GUI wizard's `on_cancel` callback ([`lib.rs:776-781`](../../crates/tisvlay-gui/src/lib.rs#L776-L781)) only calls `reset_wizard(&app)` — purely local UI cleanup. The daemon-side `PairingSession::cancel()` exists ([`pairing.rs:457`](../../crates/tisvlay-core/src/pairing.rs#L457)) and backends respect it (LG races against cancel commands at [`lg/backend.rs:592`](../../crates/tisvlay-lg/src/backend.rs#L592)), but the cancel is never invoked from D-Bus. So today, when the user "cancels" a pair from the wizard, the daemon-side pair task keeps running until natural completion or timeout.

This reframes the question. It's not "match an existing convention" — it's "add the missing cancel surface, where?"

| Path | Per-TV `Tv1.CancelPair()` viable? | `Manager1.CancelPair(tv_id)` viable? |
|---|---|---|
| AddTv | **No** — the per-TV path isn't registered until after `pair_tv` returns (see [`dbus/manager.rs:256`](../../crates/tisvlayd/src/dbus/manager.rs#L256)) | **Yes** — UUID is generated upfront and shipped via `PairingProgress` signals before the path exists |
| Re-pair | Yes — TV already exists on the bus throughout | Yes |

AddTv can only use Manager-level. Re-pair can use either. Splitting placement across the two flows for one logical operation is duplicative, so: **single `Manager1.CancelPair(tv_id)` covers both**. The operation is "cancel a pair-in-flight identified by tv_id" — Manager scope fits the abstraction.

`Manager1.AddTv` today is *blocking* — its D-Bus handler awaits `pair_tv` for the whole 10–30s pair window. Adding `Manager1.CancelPair(tv_id)` thus does double duty: it gives re-pair its cancel mechanism, and it fixes the latent AddTv-cancel-doesn't-actually-cancel issue (the wizard's cancel becomes meaningful for the first time). Worth noting because the re-pair plan effectively pays down that debt as a side effect.

### DR7b: Cancel semantics on unknown tv_id — strict `Err(NotPairing)`

If `Manager1.CancelPair(tv_id)` is called when the tv_id isn't currently pairing (already completed, never started, wrong id), return `Err(DaemonError::NotPairing(tv_id))` rather than silently `Ok(())`. Matches the industry convention (BlueZ's `Adapter.RemoveDevice` on an unknown device returns `org.bluez.Error.DoesNotExist`; NetworkManager analogues likewise return typed errors). GUI clients only fire cancel when they know a pair is in flight (they're listening to `PairingProgress` signals); a typed error helps debugging if that invariant is ever broken.

### DR7c: Session tracking infrastructure — `pairing_sessions` map on `TvManager`

Today the `PairingSession` lives only inside `pair_tv`'s stack frame ([`tv_manager.rs:1582`](../../crates/tisvlayd/src/tv_manager.rs#L1582)) — there's no map of in-flight sessions for external dispatch to reach. To enable cancel from a D-Bus call, add `pairing_sessions: Arc<Mutex<HashMap<String, mpsc::Sender<PairingCommand>>>>` on `TvManager`. Register the session's command sender at pair start, remove on completion (success, failure, or cancel). Expose `pub async fn cancel_pair(&self, tv_id: &str) -> Result<(), TvManagerError>` that looks up the sender and sends `PairingCommand::Cancel` via the existing channel.

This infrastructure lands once (in Phase 1) and serves both AddTv cancel and re-pair cancel. The bookkeeping cost is the same regardless of API placement, which is another argument for the single-API approach.

### DR8: Failure handling — leave existing config intact, propagate typed error

If the handshake fails (TV refuses, network unreachable, timeout), the existing `vendor_config` stays untouched on disk. `Tv1.RePair()` returns a typed error so the GUI can surface a specific message. Reuses existing `DaemonError` variants (`PairingFailed`, etc.) — no new variants needed unless re-pair surfaces a class of error AddTv doesn't. (The lifecycle plan ended up not introducing new variants either, after the mid-implementation reframing.)

After failure, the daemon attempts to reconnect with the *old* credentials — if the failure was transient (network blip, TV asleep), the connection comes back without user intervention; if the credentials are genuinely invalid, the next `is_connected` reads false and the user can try Re-pair again or fall back to Remove + Add.

### DR9: Concurrency — block re-pair if a pair is already in flight

If the user clicks Re-pair while a pair (initial or re-pair) is still running for the same TV, reject with `Tv1.RePair: already pairing`. Shared state lives on the TV's `ManagedTv` — already gated by the per-TV `Mutex<ManagedTv>` from the lifecycle plan; check a `pairing_in_flight: bool` field at the start of `re_pair_tv` and bail.

## Source-of-truth audit

| Site | Today | Should be |
|---|---|---|
| `crates/tisvlayd/src/tv_manager.rs::pair_tv` | Generates new UUID, creates TvConfig, calls backend's start_pairing, stores result | Extract a private `wake_and_pair_with(connector, vendor_config) -> vendor_config` helper that both `pair_tv` and the new `re_pair_tv` call. |
| `crates/tisvlayd/src/tv_manager.rs` | No `re_pair_tv` method | Add `pub async fn re_pair_tv(&self, tv_id: &str) -> Result<(), TvManagerError>`. Validates TV exists + not already pairing, closes existing connection, calls `wake_and_pair_with` against existing vendor_config, persists merged config, reconnects. |
| `crates/tisvlayd/src/dbus/tv.rs::TvService` | Tv1 interface — has `Reconnect`, `Remove`, `UpdateConfig`, `GetConfig` | Add `RePair() -> ()` D-Bus method. Async (returns after kicking off the handshake; progress via `Manager1.PairingProgress` signal). |
| `crates/tisvlayd/src/dbus/manager.rs` | No `CancelPair` D-Bus method today (code reading confirmed). Wizard's `on_cancel` does only local UI cleanup; daemon-side pair task continues. | Add `Manager1.CancelPair(tv_id) -> ()` per DR7. Covers both AddTv-in-flight and re-pair-in-flight via a single API. Backed by the new `TvManager::cancel_pair` method (DR7c). |
| `crates/tisvlayd/src/tv_manager.rs::pairing_sessions` | No tracking of in-flight `PairingSession`s; session lives only inside `pair_tv`'s stack frame | Add `pairing_sessions: Arc<Mutex<HashMap<String, mpsc::Sender<PairingCommand>>>>`. Register at pair start, remove on completion. Expose `cancel_pair(tv_id)` that looks up + sends `PairingCommand::Cancel`. Per DR7c. |
| `crates/tisvlayd/src/dbus/mod.rs::DaemonError` | No `NotPairing` variant | Add `NotPairing(String)` typed variant for the cancel-against-unknown-tv_id case. Per DR7b. |
| `crates/tisvlay-gui/src/lib.rs` | Wizard is launched from sidebar's "Pair TV" button → `Daemon.request-pair` callback → `on_request_pair` opens the wizard at step 1 (Discover) | Add a `Tv.request-re-pair(tv_id)` callback. Handler resets the wizard, prefills `selected-vendor` / `selected-vendor-name` / `selected-address` / `tv-name` from the existing TV's config, jumps to `step = 3` (Pair), kicks off `Tv1.RePair(tv_id)`. |
| `crates/tisvlay-gui/ui/components/tvs_pane.slint` (TV detail edit-mode pane) | Has `Remove` button | Add `RePair` button immediately above Remove. Uses `Icons.reload` for now (same `view-refresh` icon used by the sidebar's Reload action — re-pair *is* a credential refresh). |
| `crates/tisvlay-gui/ui/globals/tv.slint` (or equivalent global) | No `request-re-pair(tv_id)` callback | Add it. Mirrors the existing `request-pair` callback shape. |
| `crates/tisvlay-mock/src/connector.rs::start_pairing` | Honors `pairing` schema field for new pairings | No change needed if the existing logic already accepts re-input vendor_config (verify in step 1). If not, extend so the same `accept` / `reject` / `timeout` / `manual` outcomes apply on the re-pair path. |

## Phase 1 steps — daemon side

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Verify `mock` and `lg` backends' `start_pairing` accept an already-credentialed vendor_config without erroring. Add a unit test for each that confirms calling `start_pairing` twice in succession with the same input produces a valid `PairingSession` both times. | done | Both backends use serde's lenient default (no `deny_unknown_fields`); credentialed extras parse fine. LG additionally has `client_key` as a first-class `Option<String>` field. Tests in `lg/backend.rs::tests` and `mock/connector.rs::tests`. |
| 2 | Extract a handshake helper from `tv_manager::pair_tv`. Refactor `pair_tv` to call it. Verify by re-running the existing pair-flow tests. | done | Named `run_pairing_handshake` (the plan's `wake_and_pair_with` name was speculative about a WoL step that doesn't exist today). Sets up the shared codepath for `re_pair_tv`. |
| 3 | Add `pairing_in_flight: bool` field to `ManagedTv`, set true around any pair call (existing `pair_tv` and the new re-pair), reset to false on completion or error. | done | Concurrency gate per DR9. Only re-pair currently reads/writes it; AddTv doesn't need the gate since the TV doesn't exist in `tvs` until pair succeeds. |
| 4 | Add session-tracking infrastructure on `TvManager`: `pairing_sessions: Arc<Mutex<HashMap<String, mpsc::Sender<PairingCommand>>>>`. Register the session's command sender at pair start (in `pair_tv` and the new `re_pair_tv`), remove on completion. Expose `pub async fn cancel_pair(&self, tv_id: &str) -> Result<(), TvManagerError>` that looks up + sends `PairingCommand::Cancel` via the channel. | done | Per DR7c. Lives in `run_pairing_handshake` (the shared helper), so both AddTv and re-pair register/cleanup automatically. New `PairingSession::command_sender()` accessor on `tisvlay-core` exposes the channel for the map. |
| 5 | Add `DaemonError::NotPairing(String)` typed variant to `dbus/mod.rs`. Map `TvManagerError::NotPairing(tv_id)` to it in the existing `From` block. | done | Per DR7b. |
| 6 | Implement `tv_manager::re_pair_tv(tv_id)`. Validates state (TV exists, `pairing_in_flight = false`), closes connection, calls handshake with the session sender registered in the map, merges result into TvConfig, persists via `disk_write_lock`, triggers reconnect. Cleans up the `pairing_sessions` entry on success / failure / cancel. | done | Mirrors `pair_tv`'s structure but operates on existing TV. The Mutex is released during the long handshake so other Tv1.* calls aren't blocked for 10–30s; they observe `NotConnected` (connection is closed pre-handshake) which is fine. |
| 7 | Add `Tv1.RePair()` D-Bus method to `TvService` in `dbus/tv.rs`. Routes to `manager.re_pair_tv(&self.tv_id)`. | done | Synchronous from the client's perspective (call returns when handshake completes); progress streams via `PairingProgress` signals. Per DR2's noted asymmetry with AddTv. |
| 8 | Add `Manager1.CancelPair(tv_id)` D-Bus method that calls `manager.cancel_pair(tv_id)`. Returns `Err(DaemonError::NotPairing(tv_id))` if the tv_id isn't currently pairing. | done | Per DR7. Covers both AddTv-in-flight and re-pair-in-flight cancel via a single API. |
| 9 | Three regression tests in `crates/tisvlayd/tests/`: (a) `re_pair_preserves_top_level_config.rs`, (b) `re_pair_blocks_when_already_pairing.rs`, (c) `cancel_pair_aborts_in_flight.rs` plus a bonus "CancelPair against unknown tv_id returns NotPairing" assertion in (c). | done | All four tests pass. Use `format!("{:#}", err)` to inspect the underlying error chain through the DaemonClient wrapper's contextual message. |

## Phase 2 steps — GUI side

| # | Step | Status | Notes |
|---|---|---|---|
| 10 | Add `request-re-pair(string)` callback on `Daemon` global with a Rust handler that prefills the wizard from existing TV state and kicks off `Tv1.RePair(tv_id)`. | done | Handler looks up the TV in `app.get_tvs()`, prefills `selected-vendor` / `selected-vendor-name` / `selected-address` / `tv-name`, sets `pair-tv-id` (for cancel routing), jumps to `step = 3`, spawns `run_wizard_re_pair`. |
| 11 | Add `RePair` button to the TV detail edit-mode pane in `tvs_pane.slint`, immediately above Remove. | done | Implementation used std `Button` (not `IconBtn`); icon deferred — the helper text under the button explains the action which makes the icon less critical. The disabled-state gate (`connected = false` + no MAC) wasn't added — re-pair semantically *requires* the TV reachable so the gate is implicit; daemon-side will return `ConnectionFailed` if unreachable. Revisit if the bare-button affordance proves unclear. |
| 12 | Confirm dialog before re-pair. | done | Mirrors the Remove confirm-dialog pattern: `pending-re-pair-id` / `pending-re-pair-name` properties on `TvsPane`, second `ConfirmDialog` instance with re-pair-specific message + "Re-pair" confirm label. |
| 13 | Wizard flow: on `request-re-pair(tv_id)`, wizard opens directly at `step = 3` (`StepPair`) with vendor / address / name / profile prefilled. | done | All wiring lives in `on_request_re_pair`. |
| 14 | Wire the wizard's existing `on_cancel` callback through to `Manager1.CancelPair(tv_id)`. | done | Uses a new `pair-tv-id` property on the `PairingWizard` global — set by `on_request_re_pair` for re-pair, set by `run_wizard_pair`'s drainer from the first `PairingProgress` signal for AddTv. Wizard's cancel handler reads the property and calls `daemon.cancel_pair(tv_id)` on a tokio task. Side benefit landed: AddTv cancel now actually aborts the daemon-side handshake. |
| 15 | Smoke test on a Wayland session: pair a real LG TV, click the TV's "Forget device" menu item, verify the GUI's connection drops, click Re-pair, confirm the dialog, verify the TV prompts for permission, accept on the TV, verify the GUI reconnects with new credentials. Also test against the mock backend with `pairing = "reject"` and `pairing = "timeout"` to verify the error surface, and exercise mid-pair cancel against `pairing = "manual"`. | todo (user) | Real-hardware test needs an LG; mock-backend tests cover the daemon-side surface deterministically via step 9. |

## Phase 3 steps — i18n + final polish

| # | Step | Status | Notes |
|---|---|---|---|
| 16 | `just i18n-extract` to refresh the gettext catalog with new strings ("Re-pair", confirm-dialog text, error messages). | done | Four new msgids landed in `tisvlay-gui.pot`. No translations in this commit — `.po` merges happen via `just i18n-update` per CLAUDE.md i18n workflow. |
| 17 | `prek run -a` and `just check` end-to-end gate. | done | Per `feedback_lint_and_test_before_commit.md`. |
| 18 | Update `docs/design.md` — add brief mentions of `Tv1.RePair()` and `Manager1.CancelPair(tv_id)` to the D-Bus interface section. | done | Tv1 lifecycle section + Manager1 methods list + ASCII service-overview diagram. |

## Risks / unknowns

- **WoL latency makes re-pair a multi-second operation**: the user clicks Re-pair, sees a progress UI, waits up to ~30s for the TV to wake + handshake. The existing pair-progress UI handles this; no new mechanism. Worth a comment noting the expected duration so future readers don't assume re-pair is instant.
- **Persisted config rollback on cancel**: if the user cancels mid-re-pair, the daemon's cancel is best-effort; the backend may have already accepted the new credentials TV-side before the cancel command arrives. In that case the daemon's `vendor_config` doesn't get updated (we cancel before persisting), so on next reconnect the daemon uses the *old* key, which the TV may now reject. Edge case; document as "if cancel-after-TV-accept happens, manual Re-pair recovers." Not worth special-casing.
- **AddTv blocking-shape unchanged** (per DR2): we're paying down AddTv's cancel-doesn't-actually-cancel debt as a side effect of step 14, but AddTv itself stays blocking. Future work could move AddTv to the async-with-signals shape that re-pair uses; tracked as out-of-scope.
- **`pairing_sessions` map race-on-completion** (step 4): if `pair_tv` removes its entry from the map after the cancel command was sent but before the pair task observed it, the cancel is wasted (pair completes naturally). Per `Tv1.Remove`'s pre-existing race semantics from the lifecycle plan: acceptable, since the observable result (pair succeeded *or* was cancelled) is coherent either way. Document the pattern alongside the map definition.
- **GUI button placement vs. edit-mode toggle**: the user specified "above Remove button". Remove sits inside the edit-mode pane today; re-pair would too. Worth confirming during step 11 that re-pair shouldn't *also* surface outside edit-mode (e.g., when the GUI shows "Connection failed" state) — that'd be a discoverability win but adds a second placement to maintain.

## Out of scope

- **`try_silent_pair` backend trait extension** (DR3 alternative): try the existing credential silently and only prompt the user on rejection. Would skip the always-re-prompt UX. Defer until we have evidence a backend supports it cleanly (LG's WebSocket library doesn't expose the right hook today).
- **Backend-driven "ephemeral field" schema flag** (DR4 alternative): defer until a backend requires it.
- **Moving `Manager1.AddTv` to async-with-signals shape** (DR2 asymmetry): a behavior-changing refactor with its own risks (changes when ObjectManager publishes the path, affects every client). Re-pair's `Manager1.CancelPair` works correctly against AddTv's blocking shape, so this PR doesn't strictly need the unification. File as a follow-up idea.
- **Re-pair via Tray menu**: re-pair is a low-frequency action with a multi-second progress UI; the tray's icon-and-tooltip surface isn't a great fit. Skipped; document the GUI as the canonical re-pair affordance.
- **Bulk re-pair across all TVs** (e.g., "Re-pair all" after a network move): out of scope; users can re-pair each TV individually if they need to.
- **Periodic auto-re-pair before credential expiry**: vendors don't reliably advertise expiry timestamps. Reactive re-pair (user clicks when broken) is the right model for v1.

## References

- [`docs/plans/per-tv-lifecycle-correctness.md`](./per-tv-lifecycle-correctness.md) — the prerequisite plan; documents the defensive-`unregister`-before-`register` pattern that re-pair's reconnect path inherits (via the shared `wake_and_pair_with` helper), plus the per-TV `Mutex<ManagedTv>` discipline the `pairing_in_flight` gate composes with.
- [`crates/tisvlayd/src/tv_manager.rs::pair_tv`](../../crates/tisvlayd/src/tv_manager.rs) — `wake_and_pair_with` helper extracted in step 2 sets up the shared codepath. Today the body's `let mut session = self.registry.start_pairing(...)` is the spot where `pairing_sessions` map registration (step 4) hooks in.
- [`crates/tisvlay-core/src/pairing.rs::PairingSession`](../../crates/tisvlay-core/src/pairing.rs) — the session that the new `cancel_pair` reaches through via `PairingCommand::Cancel`. Already supports cancel (line 457); we're wiring it to the D-Bus surface for the first time.
- [`crates/tisvlay-core/src/traits.rs::TvBackend::start_pairing`](../../crates/tisvlay-core/src/traits.rs) — backend-side pair entrypoint, reused unchanged. Per DR3, every re-pair triggers a fresh TV-side prompt.
- [`crates/tisvlay-lg/src/backend.rs:600`](../../crates/tisvlay-lg/src/backend.rs) — confirms `WebOsClient::connect(&ip, None)` always discards the existing client_key. The "always re-prompt" behavior in DR3 is established here.
- [`crates/tisvlay-gui/src/lib.rs::wizard.on_cancel`](../../crates/tisvlay-gui/src/lib.rs) — currently only calls `reset_wizard(&app)`; step 14 extends it to also call `daemon.cancel_pair(tv_id)`, fixing the latent AddTv-cancel-doesn't-actually-cancel issue as a side effect.
- [`crates/tisvlay-gui/ui/components/pairing_wizard.slint::StepPair`](../../crates/tisvlay-gui/ui/components/pairing_wizard.slint) — pair-progress UI reused for re-pair without modification.
- [`crates/tisvlayd/src/state_forwarder.rs::PairingProgress`](../../crates/tisvlayd/src/state_forwarder.rs) — signal infrastructure that re-pair's progress feedback flows through.

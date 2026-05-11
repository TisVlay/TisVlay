# Async pair API: AddTv/RePair return immediately, outcome via signals

> **Status:** drafted 2026-05-11, not started. Continuation of the [re-pair TV plan](../../docs/plan.md) follow-up work; addresses Copilot review feedback on PR #51 around tv_id correlation under concurrent pairings, wizard task cleanup, lost-update during re-pair, and `pairing_sessions` entry cleanup.

## Context

Today, `Manager1.AddTv(name, vendor, config, managed) -> tv_id` is a **blocking** D-Bus method that returns only after the entire pairing handshake completes — typically 10–30 seconds against a real LG TV. While that call is in flight, the daemon emits `Manager1.PairingProgress(tv_id, phase, message)` signals that the GUI wizard subscribes to.

The blocking model has a correlation problem under concurrent pairings: the GUI doesn't learn its own `tv_id` until `AddTv` returns, but `PairingProgress` signals are bus-wide. If another client (a second GUI, a CLI, busctl) starts an AddTv or RePair simultaneously, the GUI sees signals for both pairs interleaved and cannot tell them apart. Today's wizard uses a "first signal wins" heuristic to capture the tv_id of its pair from the signal stream — but the first signal that arrives might be from a foreign pair. Result: wrong phase/message displayed, and the Cancel button could send `CancelPair(other_clients_tv_id)`.

The fix isn't to forbid concurrent pairings — pairings are independent network operations and should run in parallel. The fix is to give each client a way to know its own `tv_id` *before* any signal arrives. The daemon already generates that UUID synchronously at the start of `pair_tv`; we just need to return it from the RPC immediately and run the handshake asynchronously underneath.

This refactor follows the canonical D-Bus pattern (BlueZ's `Adapter1.StartDiscovery`, `Adapter1.RemoveDevice`, NetworkManager's `Connection.Activate` all return immediately and report results via signals/properties). For tisvlay, the win is: (a) eliminates the correlation race by construction, (b) supports concurrent pairings from any number of clients, (c) simplifies the wizard (one source of truth: the filtered signal stream).

This plan also rolls in three independent reviewer concerns from PR #51 that are clean to address in the same refactor window:
- Copilot #2: wizard cancel doesn't stop in-flight tasks → generation counter on the wizard.
- Copilot #3: `Tv1.UpdateConfig` during re-pair handshake silently overwrites → reject UpdateConfig while `pairing_in_flight`.
- Copilot #4: `pairing_sessions` entry leak on early task drop → RAII scope guard.

## Design decisions

### DR1: `Manager1.AddTv` returns `tv_id` immediately; handshake runs async

`AddTv(name, vendor, config, managed) -> (path, tv_id)` keeps its signature but changes its semantics. The synchronous part now does:
1. Validate vendor exists.
2. Validate `config_json` against the vendor's pairing schema (schema-driven per-field validation).
3. Generate a fresh UUID v4 for the tv_id.
4. Spawn the actual pairing handshake into a tokio task.
5. Return `(path, tv_id)` immediately. The path is reserved but the per-TV D-Bus interfaces are not yet registered (they materialize on successful handshake).

Validation errors still surface as RPC errors (`UnknownVendor`, `ConfigInvalidVendorConfig`). Handshake errors (TV rejects, timeout, network unreachable) surface via `Manager1.PairingProgress(tv_id, "failed", reason)` — *not* as RPC errors, because the RPC already returned. Clients subscribe to PairingProgress filtered on the returned tv_id and wait for a terminal phase (`complete` or `failed`).

### DR2: `Tv1.RePair()` returns `()` immediately; same async pattern

Same shape as AddTv. The synchronous part claims the per-TV `pairing_in_flight` slot, validates state, then spawns the handshake. Outcome via PairingProgress filtered on the TV's existing `tv_id` (which the client already knows since it's the per-TV path).

**Lifecycle breakdown** — what's caller-visible (RPC return) vs. signal-driven:

```
Sync part (caller awaits, can return Err):
  1. get_tv(tv_id)                       → Err(TvNotFound) on miss
  2. lock tv, check pairing_in_flight    → Err(AlreadyPairing) if set
  3. set pairing_in_flight = true,
     capture was_connected,
     close existing connection,
     snapshot (vendor, vendor_config)
  4. release lock
  5. if was_connected:
       emit_connection_changed(tv_id, false)  → client sees Connected=false during handshake
  6. spawn run_re_pair_task(...)
  7. return Ok(())                       → RPC resolves; client subscribes/awaits signals

Async part (run_re_pair_task, signal-driven):
  run_pairing_handshake emits PairingProgress phases throughout the handshake.

  ├── Ok(new_inner_config):
  │     lock tv → clear pairing_in_flight, insert new_inner_config
  │     save_tv_config              (warn on fail; in-memory state still good)
  │     emit TvVendorConfigChanged  (refreshes Tv1.VendorConfig cache)
  │     reconnect(tv_id):
  │       ├── Ok:  reconnect emits ConnectionChanged{true} itself  → Connected flips to true
  │       └── Err: log warn; Connected stays false; property reflects truth
  │
  └── Err(handshake_err):                  ← PairingProgress::failed already emitted internally
        lock tv → clear pairing_in_flight
        if was_connected:
          best-effort reconnect with OLD vendor_config:
            ├── Ok:  reconnect emits ConnectionChanged{true} → session restored
            └── Err: log warn; Connected stays false
        (No further PairingProgress; already terminal at failed.)
```

**Observable terminal outcomes (client perspective):**

| Path                              | RPC return | PairingProgress terminal | Connected property |
|-----------------------------------|------------|--------------------------|--------------------|
| Success                           | `Ok(())`   | `complete`               | flips false → true (via reconnect) |
| Handshake failed, old credentials work | `Ok(())` | `failed` w/ reason | false → true (best-effort reconnect) |
| Handshake failed, old credentials dead | `Ok(())` | `failed` w/ reason | false → stays false |
| Sync validation error             | `Err(...)` | (no signal)              | unchanged |

The best-effort reconnect outcome is **not** surfaced through PairingProgress — that's about TV reachability, conceptually orthogonal to whether re-pair succeeded. Clients that care about reachability watch `Tv1.Connected`, which already reflects the truth.

### DR3: Eliminates the wizard's tv_id correlation heuristic

Both AddTv and RePair flows in the GUI now know their `tv_id` *before* any signal arrives — AddTv learns it from the RPC return, RePair learns it from the caller-provided argument. The wizard's signal drainer filters `PairingProgress` strictly on `tv_id == known_tv_id` and ignores everything else. No more "first signal wins" heuristic; no more captured-tv_id race window. Concurrent pairings from multiple clients are supported by construction.

The drain logic itself lives in the shared `DaemonClient::drain_pair_to_terminal` helper (DR14) — wizard, CLI, and tests all use the same implementation.

### DR4: Post-handshake work moves into the spawned task

Currently, `Manager1.AddTv` does the following *after* `pair_tv` returns: fetches initial live state, registers the per-TV D-Bus interfaces on the ObjectServer, returns the path/tv_id tuple. With the async refactor, the interface registration happens *inside* the spawned handshake task on success — that's when the TV becomes visible via `ObjectManager.GetManagedObjects` and the `InterfacesAdded` signal fires.

The `zbus::Connection` handle gets cloned and moved into the spawned task so it can call `register_tv_interfaces`. zbus's `Connection` is `Clone` (it's an Arc internally), so this is cheap.

### DR5: Persistence moves into the spawned task

The `save_tv_config` call (which writes the TV's TOML to `~/.config/tisvlay/...`) moves into the spawned task on the success path. Failure to persist after a successful handshake logs a warning but does not affect the in-memory state — same as today, just moved into the async task.

### DR6: Synchronous validation rejects bad input fast

Bad vendor name, malformed JSON, schema-validation failure on a required field: all return `Err` from the synchronous part before the spawn. This preserves the user-facing UX of "submit the form → immediate inline error on the bad field" for the wizard's StepConfigure → StepPair transition.

Anything that requires actually contacting the TV (DNS resolution, TCP connect, TLS handshake, WoL) is part of the async handshake. Those errors come via PairingProgress's terminal `failed` phase.

### DR7: Cancel: same `Manager1.CancelPair(tv_id)`, no change

`Manager1.CancelPair(tv_id)` still works the same way — sends a `PairingCommand::Cancel` to the in-flight session via the `pairing_sessions` map. The spawned handshake task observes the cancel, the pairing session resolves with `PairingError::cancelled`, the PairingProgress signal emits terminal phase `failed`. The wizard sees the terminal phase and surfaces the cancellation.

### DR8: `pair_tv` and `re_pair_tv` return type changes

- `TvManager::pair_tv(self: Arc<Self>, name, vendor, vendor_config, managed) -> Result<String, TvManagerError>`: returns the generated tv_id immediately. Errors are only from the synchronous validation part. The spawned task owns the rest.
- `TvManager::re_pair_tv(self: Arc<Self>, tv_id) -> Result<(), TvManagerError>`: returns immediately after spawning. Errors are only from "tv not found" or "already pairing" (synchronous-validate part).

The pairing-task spawning requires `Arc<Self>` to move into the spawned future. The D-Bus handler already holds `Arc<TvManager>`; cloning is cheap.

### DR9: ObjectManager visibility — TV appears only on successful handshake

Today, a TV appears in `ObjectManager.GetManagedObjects` only after `AddTv` returns successfully. With async, the per-TV interfaces are registered inside the spawned task's success path — same observable behavior. A failed pairing never creates the per-TV object.

This means: between `AddTv` returning the `tv_id` and the eventual success/failure terminal phase, the TV exists *in concept* (the path is reserved) but is not visible via ObjectManager. Clients that need progress visibility subscribe to PairingProgress; clients that just want "is this TV in the system" check ObjectManager.

### DR10: Test pattern — "drain to terminal"

Integration tests that previously did `let result = manager.pair_tv(...).await?;` now do:
1. Subscribe to `PairingProgress` filtered on the new tv_id.
2. Call `pair_tv` (returns tv_id immediately).
3. Drain the filtered stream until a terminal phase (`complete` or `failed`) arrives.
4. Assert on the terminal phase and any side effects (config persisted, interfaces registered, etc.).

Helper: `wait_for_pair_terminal(client: &DaemonClient, tv_id: &str) -> Result<TerminalPhase>`. Reusable across the four affected tests (`pair_round_trip`, `cancel_pair_aborts_in_flight`, `re_pair_blocks_when_already_pairing`, `re_pair_preserves_top_level_config`).

### DR11: Wizard refactor — generation counter for cancel/abort

Once `pair_tv` returns immediately, the wizard's lifetime model changes: the only long-running thing is the signal drainer, not an in-flight RPC. Adding a generation counter on the wizard module:

- `wizard_gen: Arc<AtomicU64>` lives next to `WizardEntries`. Incremented inside `reset_wizard`.
- Each `run_wizard_pair` / `run_wizard_re_pair` task captures the current value at start.
- Before any `on_ui(...)` callback or `publish_pair_outcome` call, check the captured value against the current. Skip if drifted.

This also resolves Copilot #2: a Cancel + re-open-fast scenario won't leak old-pair updates into the new wizard session.

### DR12: Block `Tv1.UpdateConfig` while `pairing_in_flight`

Add a check at the top of `update_tv_config`: if `tv.pairing_in_flight`, return `TvManagerError::OperationFailed("config edits are blocked during pairing")`. Symmetric with how AddTv can't UpdateConfig a TV that doesn't exist yet. Resolves Copilot #3.

### DR13: RAII scope guard for `pairing_sessions`

Add a `PairingSessionGuard` struct held in `run_pairing_handshake`. Its `Drop` impl removes the entry from `pairing_sessions`. Replaces the manual cleanup; survives every return path (Ok, Err, future-dropped, panic-unwind). Also: `cancel_pair` removes the entry on `sender.send(Cancel).await.is_err()` so a permanently-dead sender doesn't leak. Resolves Copilot #4.

### DR14: Shared `drain_pair_to_terminal` in `tisvlay-core::dbus_client`

Both the CLI (`tisvlay pair`, soon `tisvlay re-pair`), the GUI wizard, and integration tests all need the same "drain a pre-subscribed `pairing_progress_stream` filtering on tv_id, return on first terminal phase" loop. Extracting it into `tisvlay-core::dbus_client` collapses that duplication and gives the new clients (CLI re-pair, future automations) a one-call surface.

```rust
pub enum PairTerminal {
    Complete { tv_id: String, message: String },
    Failed   { tv_id: String, message: String },
}

impl DaemonClient {
    /// Drain a pre-subscribed `pairing_progress_stream` filtering on
    /// `target_tv_id`, calling `on_progress` for each non-terminal
    /// event, returning on the first terminal phase. Stream is
    /// consumed; caller subscribes before the originating
    /// `pair_tv`/`re_pair` call to avoid missing the burst.
    pub async fn drain_pair_to_terminal<S>(
        stream: S,
        target_tv_id: &str,
        mut on_progress: impl FnMut(&str /*phase*/, &str /*message*/),
    ) -> Result<PairTerminal>
    where S: Stream<Item = (String, String, String)> + Unpin;
}
```

CLI passes `|phase, msg| eprintln!("  {phase}: {msg}")`. GUI wizard passes an `on_ui(...)`-into-Slint closure plus a captured generation-counter check (DR11). Tests pass `|_, _| {}`. The helper itself doesn't know about Slint, tracing, or stdout — it's a generic filter+match-terminal loop.

Filter logic is centralized: `event_tv_id != target_tv_id` → ignore. Terminal phases (`complete`, `failed`) → return.

## Source-of-truth audit

| Site | Today | After this PR |
|---|---|---|
| `crates/tisvlayd/src/tv_manager.rs::pair_tv` | `pub async fn pair_tv(&self, ...) -> Result<String>`; runs handshake + persist + connect + register inline | `pub async fn pair_tv(self: Arc<Self>, ...) -> Result<String>`; validates synchronously, generates tv_id, spawns `run_pair_task`, returns immediately |
| `crates/tisvlayd/src/tv_manager.rs::re_pair_tv` | `pub async fn re_pair_tv(&self, tv_id) -> Result<()>`; runs handshake + persist + reconnect inline | `pub async fn re_pair_tv(self: Arc<Self>, tv_id) -> Result<()>`; validates synchronously, sets pairing_in_flight, spawns `run_re_pair_task`, returns immediately |
| `crates/tisvlayd/src/tv_manager.rs` | (no `run_pair_task` / `run_re_pair_task`) | New private async methods; own the handshake, persistence, reconnection, and ObjectServer registration |
| `crates/tisvlayd/src/tv_manager.rs::run_pairing_handshake` | Inserts into `pairing_sessions`, calls cleanup after `wait_for_completion` | Same, but cleanup moves to RAII guard so it's drop-safe |
| `crates/tisvlayd/src/tv_manager.rs::cancel_pair` | Returns NotPairing on send failure; leaves stale entry in map | Also removes the entry on send failure |
| `crates/tisvlayd/src/tv_manager.rs::update_tv_config` | Edits `tv.config` unconditionally under the per-TV lock | Returns `OperationFailed` if `tv.pairing_in_flight` is true |
| `crates/tisvlayd/src/dbus/manager.rs::add_tv` | Calls `manager.pair_tv(...)` blocking, then registers Tv1 interfaces after | Calls the new `pair_tv` (returns tv_id immediately), clones `connection` into the task; interface registration moves into the spawned task |
| `crates/tisvlayd/src/dbus/tv.rs::TvService::re_pair` | Calls `manager.re_pair_tv(tv_id).await?` blocking | Calls the new `re_pair_tv` (returns immediately); interface refresh on success happens inside the spawned task via existing `TvVendorConfigChanged` plumbing |
| `crates/tisvlay-gui/src/wizard.rs::run_wizard_pair` | Subscribes to stream, spawns drainer with "first signal wins" tv_id capture, awaits `pair_tv`, aborts drainer | Subscribes to stream, calls `pair_tv` to get tv_id, drains stream filtered on tv_id to terminal phase, publishes outcome from the terminal phase |
| `crates/tisvlay-gui/src/wizard.rs::run_wizard_re_pair` | Subscribes to stream, spawns drainer filtered on target tv_id, awaits `re_pair`, aborts drainer | Subscribes to stream, calls `re_pair`, drains stream filtered on tv_id to terminal phase, publishes outcome from the terminal phase |
| `crates/tisvlay-gui/src/wizard.rs` | (no generation counter) | Adds `wizard_gen: Arc<AtomicU64>` to module state; incremented on `reset_wizard`; captured + checked in tasks/drainers |
| `crates/tisvlay-gui/src/lib.rs::reset_wizard` call sites | Reset wizard state | Also increment `wizard_gen` |
| `crates/tisvlayd/tests/pair_round_trip.rs` | `manager.pair_tv(...).await?` returns final tv_id and persisted config | Subscribes to PairingProgress, calls `pair_tv` to get tv_id, drains to terminal, asserts on final state |
| `crates/tisvlayd/tests/cancel_pair_aborts_in_flight.rs` | Starts an in-flight pair, cancels, asserts on result | Subscribes to PairingProgress, kicks off pair (returns tv_id), waits for `awaiting_tv`, calls cancel, drains to terminal `failed`, asserts on cancellation reason |
| `crates/tisvlayd/tests/re_pair_preserves_top_level_config.rs` | Calls `manager.re_pair_tv(tv_id).await?` blocking, asserts config | Subscribes to PairingProgress, kicks off re-pair, drains to terminal `complete`, asserts config |
| `crates/tisvlayd/tests/re_pair_blocks_when_already_pairing.rs` | Starts first re-pair, attempts second, asserts AlreadyPairing | First re-pair returns Ok immediately (queued); second returns Err synchronously (`AlreadyPairing`); drain the in-flight pair's terminal phase to clean up |
| `crates/tisvlay-core/src/dbus_client.rs::pairing_progress_stream` | Returns `Stream<Item = (tv_id, phase, message)>` | No change. The filter-and-match-terminal loop on top of this stream moves into the new shared helper. |
| `crates/tisvlay-core/src/dbus_client.rs` | (no shared drain helper; wizard had inline drainer) | Add `PairTerminal` enum + `DaemonClient::drain_pair_to_terminal(stream, target_tv_id, on_progress)` per DR14. |
| `crates/tisvlay-cli/src/main.rs::Pair` handler | Calls `client.pair_tv(...).await?` blocking; prints success line | Subscribes to PairingProgress first, calls `pair_tv` to get tv_id, calls `drain_pair_to_terminal(stream, tv_id, |phase, msg| eprintln!("  {phase}: {msg}"))`, maps `PairTerminal::Complete` → success print, `Failed` → `anyhow::bail!`. |
| `crates/tisvlay-cli/src/main.rs` | (no `re-pair` subcommand) | Add `tisvlay re-pair <tv_id>` subcommand. Reuses the same drain helper. |
| `crates/tisvlay-harness/src/pairing.rs` | (does not exist) | New submodule. `wait_for_pair_phase(stream, tv_id, phase)` returns the matching event + the remaining stream so tests that need to trigger mid-flight cancellation can pause at e.g. `awaiting_tv` and resume the drain after. |

## Phase 0 — shared `drain_pair_to_terminal` helper

| # | Step | Status | Notes |
|---|---|---|---|
| 0 | Add `PairTerminal` enum + `DaemonClient::drain_pair_to_terminal(stream, target_tv_id, on_progress) -> Result<PairTerminal>` to `crates/tisvlay-core/src/dbus_client.rs` per DR14. Pure consumer of the existing `pairing_progress_stream`; no daemon-side changes yet. | todo | Lands before Phase 1 so Phases 2 (GUI), 3 (CLI), 4 (tests) can all call it. Unit-testable in isolation with a hand-built stream. |

## Phase 1 — daemon refactor (async pair_tv + re_pair_tv)

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `run_pair_task` private method on `TvManager`: takes ownership of `(uuid, tv_id, name, vendor, vendor_config, managed, connection)`, runs `run_pairing_handshake`, on Ok persists + adds to `tvs` + registers per-TV interfaces, on Err logs (PairingProgress already emitted `failed`). | todo | The connection clone is passed in so the task can call `register_tv_interfaces` itself. |
| 2 | Refactor `pair_tv` to: validate vendor, generate UUID, spawn `run_pair_task`, return Ok(tv_id). Change signature to `self: Arc<Self>`. | todo | Caller (D-Bus handler) gets `Arc<TvManager>` already; `Arc::clone` into the spawn. |
| 3 | Add `run_re_pair_task` private method: takes ownership of `(self_arc, tv_id, vendor, vendor_config, was_connected)`, runs `run_pairing_handshake`, persists, reconnects on success, best-effort reconnect on error (per the existing logic from the prior commit). | todo | Folds in the just-landed best-effort-reconnect from commit `55781a5`. |
| 4 | Refactor `re_pair_tv` to: validate state under lock + claim pairing_in_flight slot + capture vendor_config + emit ConnectionChanged{false}, then spawn `run_re_pair_task`, return Ok(()). Change signature to `self: Arc<Self>`. | todo | Keeps the sync-validation work (mutex + slot claim + ConnectionChanged emit) on the caller's side; the long handshake goes to the task. |
| 5 | Refactor `Manager1.AddTv` D-Bus handler: validate vendor + JSON + schema, call new `pair_tv` (gets tv_id immediately), build the path, return `(path, tv_id)`. Drop the post-pair interface-register block — that moves into `run_pair_task`. | todo | The handler clones `connection.clone()` and passes into `pair_tv` so the spawned task can register interfaces. |
| 6 | Refactor `Tv1.RePair` D-Bus handler: call new `re_pair_tv`, return `()`. No other changes — the task does everything else. | todo | Simpler than today. |
| 7 | Add RAII `PairingSessionGuard` in `tv_manager.rs`. Holds a reference to `pairing_sessions` and the `tv_id`. `Drop` removes the entry. Replace explicit cleanup in `run_pairing_handshake` with the guard. | todo | Survives panic-unwind, future-dropped, normal Ok/Err return. |
| 8 | Update `cancel_pair` to remove the map entry on `sender.send(Cancel).await.is_err()`. | todo | Belt-and-suspenders alongside the guard; defends against a stale entry whose receiver dies between guard-insert and guard-drop. |
| 9 | Update `update_tv_config`: at the top of the per-TV mutex section, if `tv.pairing_in_flight`, return `TvManagerError::OperationFailed("config edits blocked while pairing is in flight")`. | todo | Resolves Copilot #3. |

## Phase 2 — GUI wizard refactor (drain to terminal + generation counter)

| # | Step | Status | Notes |
|---|---|---|---|
| 10 | Add `wizard_gen: Arc<AtomicU64>` to the wizard module. Construct at `register_callbacks` time; pass into `reset_wizard` and into `run_wizard_pair` / `run_wizard_re_pair`. | todo | Each spawned task captures `.load(Ordering::SeqCst)` at start; `reset_wizard` does `.fetch_add(1, ...)`. |
| 11 | Refactor `run_wizard_pair`: subscribe to PairingProgress, call `daemon.pair_tv(...)` to get tv_id, set `wizard.pair_tv_id = tv_id`, call `DaemonClient::drain_pair_to_terminal(stream, tv_id, on_progress)` with an `on_progress` closure that does `on_ui(...)`-into-Slint with a captured-gen check. Map the returned `PairTerminal` to the wizard's existing outcome shape. | todo | DR14's helper does the filter + match-terminal loop. Drops ~30 lines of inline drainer code from wizard.rs. |
| 12 | Refactor `run_wizard_re_pair`: same shape as run_wizard_pair but tv_id is the function argument; same `drain_pair_to_terminal` call site, same closure pattern. | todo | Mirror structure; no duplication of the drain logic. |
| 13 | Update `publish_pair_outcome`: check generation; no-op if drifted. | todo | Already publishes via `on_ui`; the check goes there. |
| 14 | Smoke test: open wizard, click Cancel mid-pair, immediately re-open wizard for a new pair, verify the old task's late updates don't bleed into the new session. (Mock backend with `pairing = "manual"` makes this reproducible.) | todo | This is the canary for the gen counter. |

## Phase 3 — CLI catch-up

| # | Step | Status | Notes |
|---|---|---|---|
| 15 | Refactor `tisvlay pair` handler in `crates/tisvlay-cli/src/main.rs`: subscribe to PairingProgress first, call `client.pair_tv(...)` to get tv_id, call `DaemonClient::drain_pair_to_terminal(stream, &tv_id, |phase, msg| eprintln!("  {phase}: {msg}"))`, map `PairTerminal::Complete` to the existing success line + `Failed` to `anyhow::bail!`. | todo | Same shape as the wizard but stdout/stderr instead of Slint. |
| 16 | Add `tisvlay re-pair <tv_id>` subcommand: gated `Commands::RePair { tv_id }` enum variant, handler that calls `client.re_pair(tv_id)` then drains progress via the shared helper. | todo | ~15 lines. Mirrors the structure of `Pair`. Closes the CLI gap with the GUI. |

## Phase 4 — test refactor (drain to terminal pattern)

| # | Step | Status | Notes |
|---|---|---|---|
| 17 | Add a `tisvlay-harness::pairing` submodule with `wait_for_pair_phase(stream, tv_id, phase) -> (event, remaining_stream)`. Used by `cancel_pair_aborts_in_flight` to pause at `awaiting_tv` before triggering cancel, then resume draining via the shared `drain_pair_to_terminal` helper. | todo | Test-specific shape; production code uses the shared helper directly without a "wait-for-this-intermediate" step. |
| 18 | Update `pair_round_trip.rs`: subscribe stream first, call `pair_tv`, call `DaemonClient::drain_pair_to_terminal(stream, &tv_id, |_,_| {})`, assert on `PairTerminal::Complete`. | todo | |
| 19 | Update `cancel_pair_aborts_in_flight.rs`: subscribe stream first, call `pair_tv`, call `wait_for_pair_phase(..., "awaiting_tv")`, call `cancel_pair`, drain remainder to `Failed`, assert on cancellation reason. | todo | Mock's `pairing = "manual"` mode gives a deterministic "awaiting_tv" state. |
| 20 | Update `re_pair_preserves_top_level_config.rs`: subscribe stream first, call `re_pair_tv`, drain to `Complete`, assert config preservation. | todo | |
| 21 | Update `re_pair_blocks_when_already_pairing.rs`: kick off first re-pair (returns Ok), call second re-pair (returns Err `AlreadyPairing` synchronously), drain first's terminal phase to clean up. | todo | The "already pairing" check is now synchronous-only, which is cleaner. |
| 22 | Add a new test `update_config_blocked_during_pairing.rs` (or extend existing): verify `Tv1.UpdateConfig` returns an error while `pairing_in_flight` is true. | todo | Covers DR12. |

## Phase 5 — docs + i18n + final gate

| # | Step | Status | Notes |
|---|---|---|---|
| 23 | Update `docs/design.md` — note that `Manager1.AddTv` and `Tv1.RePair` are async (return immediately, outcome via signals). Update the signal section for `PairingProgress` to call out that clients should drain to a terminal phase. Mention the shared `drain_pair_to_terminal` helper for new client implementations. | todo | |
| 24 | Update `.claude/CLAUDE.md` "Known Quirks" with: AddTv/RePair return-immediately model + drain-to-terminal pattern for clients + `tisvlay re-pair` CLI subcommand. | todo | |
| 25 | `just i18n-extract` — refresh gettext catalog. (Wizard messages may have changed.) | todo | Per CLAUDE.md i18n workflow. |
| 26 | `prek run -a` + `just check` end-to-end gate. | todo | Per `feedback_lint_and_test_before_commit.md`. |

## Risks / unknowns

- **AddTv error model split**: validation errors return immediately as RPC errors; handshake errors come via signals. Clients that only check the RPC return won't know about handshake failures. The wizard already subscribes to PairingProgress, so it's fine; the CLI (`tisvlay add-tv`) needs to also subscribe-and-drain. Update CLI as part of step 21 if it currently relies on the blocking return.

- **Race: GUI restarts mid-handshake**: with blocking AddTv, the GUI dies → its RPC await dies → the daemon's pair task continues (it's not tied to the caller). Same here. But with async, if the GUI restarts, it has no record of the in-flight tv_id (the RPC return was in-process state). The pair completes silently; the user sees a new TV appear via ObjectManager after restart. Acceptable; document as known behavior.

- **`zbus::Connection::clone()` cost**: cheap (Arc clone internally). Verified by inspecting zbus 5 docs.

- **`run_pair_task` ownership**: takes `Arc<Self>`, `String` args, `toml::Value`, owned `zbus::Connection`. All `Send + 'static`. Standard tokio spawn pattern.

- **Test determinism with terminal-phase drain**: mock backend's `pairing = "accept"` mode emits `awaiting_tv` → `complete` synchronously via `yield_now`. Tests subscribe-then-call, so they don't miss the burst. Verify with a tiny ordering test if uncertain (step 15).

- **`PairingProgress` filter race**: client subscribes, then calls `AddTv`. If the daemon emits the first PairingProgress before the subscription is fully registered, the client misses it. zbus's `MessageStream` subscribes synchronously (before returning from the `receive_*` call), so this should be deterministic — but worth a smoke test in step 16.

- **Cancel semantics during the "spawned but not started" window**: between `pair_tv` returning tv_id and `run_pair_task` starting `start_pairing`, the pairing_sessions entry doesn't exist yet (insert happens inside `run_pairing_handshake`). If the GUI calls `CancelPair(tv_id)` in that gap, it gets `NotPairing`. Window is microseconds; the GUI's "Cancel" button isn't enabled until at least one PairingProgress signal arrives, which only happens after the entry is inserted. No fix needed; document if anyone hits it.

- **Test harness side effects**: `pair_round_trip` previously asserted on the persisted config via `tv_manager.get_config(&tv_id)` immediately after `pair_tv` returned. With async, persistence happens inside the spawned task; the assertion needs to wait for terminal phase first. Step 16.

## Out of scope

- **Streaming progress via D-Bus property** (e.g., `Manager1.PairingState` property that updates as pairing progresses): the signal-based approach is canonical; a property would be a duplicate channel. Skip.

- **Per-pair-session D-Bus object** (e.g., `/org/tisvlay/Pairing/<uuid>` with its own interface): more discoverable, but heavyweight for a 30-second operation. Skip unless a future need arises.

- **Caller-supplied request_id correlation token**: the bus-native tv_id from the RPC return is sufficient. Adding a client-side correlation token is redundant.

- **Backward compatibility with older clients**: this is an unpublished workspace; existing clients are all in-tree and update together. Don't bother with deprecation shims.

- **Cancel of `pair_tv` in the "spawned but not started" window**: documented as a known microsecond-scale gap (see Risks). A pre-emit insert into pairing_sessions would close it, but adds complexity for no real user benefit.

## References

- [`crates/tisvlayd/src/tv_manager.rs::pair_tv`](../../crates/tisvlayd/src/tv_manager.rs) — the function being refactored.
- [`crates/tisvlayd/src/tv_manager.rs::re_pair_tv`](../../crates/tisvlayd/src/tv_manager.rs) — sibling refactor.
- [`crates/tisvlayd/src/tv_manager.rs::run_pairing_handshake`](../../crates/tisvlayd/src/tv_manager.rs) — host for the RAII guard.
- [`crates/tisvlayd/src/dbus/manager.rs::add_tv`](../../crates/tisvlayd/src/dbus/manager.rs) — D-Bus handler that drops post-pair interface-register.
- [`crates/tisvlay-gui/src/wizard.rs::run_wizard_pair`](../../crates/tisvlay-gui/src/wizard.rs) — wizard flow being refactored.
- [`crates/tisvlay-core/src/dbus_client.rs::pairing_progress_stream`](../../crates/tisvlay-core/src/dbus_client.rs) — signal stream used by the wizard and tests.
- BlueZ async pattern precedent: [`Adapter1.StartDiscovery`](https://github.com/bluez/bluez/blob/master/doc/org.bluez.Adapter.rst) — same return-immediately + signal-for-outcome shape.

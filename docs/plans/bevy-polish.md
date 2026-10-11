# World ECS polish

Branch `feat/bevy-polish`, on top of the bevy_ecs 0.20 bump (#151). This came out of a review of the 0.20 changelog against how `tisvlay-world` uses the ECS. The release itself offered little we need. The review surfaced robustness gaps, three live bugs, and several places where the world hand-rolls what Bevy provides. The plan was reviewed by three agents (design, implementation correctness, maintainability), and their findings are folded in.

## Goals

1. Any dead daemon task (world, bridge, store) or a stop signal ends the process cleanly. The last store write is flushed, and systemd's restart policy takes over. No more "up but deaf".
2. Client commands run one at a time in arrival order with read-your-writes semantics, and a missing handler doesn't compile.
3. The uuid→entity maps can't drift from the entities they index, and removals can't forget their wire deletes or their route-monitor release.
4. The store holds exactly what the world holds. Saves come from comparing state, not from flags or wake lists kept by hand. Boot never exposes a partial table.

## Decisions

Owner's rulings, 2026-10-10:

- **Live bugs** found by the review get fix-first commits at the base of this branch, each with a regression test that fails first.
- **Supervision** is a full supervisor:
  - It watches the reactor, bridge and store tasks, `ctrl_c` and SIGTERM together.
  - Shutdown happens in order, flushing the store.
  - It exits non-zero when a task dies and zero on a signal.
  - A bevy panic-logging handler names the failing system.
  - No log-and-continue, no `panic = "abort"`.
- **Commands** run as one-shot systems (`run_system_cached_with`) from the reactor's exhaustive `match`.
  - A missing handler doesn't compile.
  - A handler returns its result, and the reactor answers in one place.
  - Observers were rejected: a missing observer drops a command silently, and only a test can catch that.
- **Maps** use immutable key components plus component hooks that maintain private maps and queue the wire deletes. The registry `SystemParam` alternative was rejected; the owner prefers the idiomatic design over the smaller diff.
- **Store saves:** rebuild and diff every step. `snapshot_store` builds the records every step and persists when they differ from the last saved snapshot. There's no wake list, because a hand-kept trigger list repeats the dirty flag's failure mode (live bug 1 is exactly a filter mistake).
- **Boot:** seed before spawn. The store and seed config apply to the world synchronously before the reactor task starts. The loaded snapshot becomes the diff baseline.
- **Derive pass:** dedup only. No change gating.
- **Not doing** (reasons under [Not doing](#not-doing)):
  - splitting `Meta`;
  - typed `automate_when` (ideas entry only);
  - the minor 0.20 additions.

## Work items

They're in landing order. Each is one commit unless noted, and each passes `just check` on its own.

### 0. Live-bug fixes

Each fix lands with a regression test that fails before the fix.

**0a. Re-pair drops the TV from the store.**
- *Cause.* `snapshot_store` filters `Without<PairVerifying>` (`systems.rs` `snapshot_store`). Re-pair inserts `PairVerifying { rollback: false }` on the existing TV and marks dirty in the same step, so that save leaves the TV out. A successful verify writes it back. A failed re-pair removes the marker with no dirty mark, so the TV stays off disk until some unrelated change. A restart in that window loses the TV.
- *Fix.* Exclude only verifying TVs with `rollback: true`, the fresh enrollments that must leave no trace.
- *Tests.* After a re-pair starts, and again after it fails, the persisted store still holds the TV.

**0b. A same-batch profile update is lost.**
- *Cause.* `AddProfile X` then `UpdateProfile X` in one batch: the update's `bodies.get_mut` misses the not-yet-spawned entity, and the reply is still ok.
- *Fix.* A minimal one in place: apply the update to the pending body, or refuse it. Item 5 makes this structurally impossible; the test keeps it fixed.
- *Test.* Assert the resulting body and its publish, not just two ok replies.
- *Helper.* Tests need a support helper that queues commands before the reactor task starts, so "one batch" is deterministic. `recv_many` takes at most 64.

**0c. Removal records grow without bound.**
- *Cause.* Every component removal is recorded in the world's removed-components buffer (bevy `bundle/remove.rs`), which only `World::clear_trackers` ages out. The reactor never calls it, and `Schedule::run` doesn't either. Marker components such as `PendingConnect`, `RetryAt` and `Reconnecting` come and go constantly, so this is a slow leak.
- *Fix.* Call `clear_trackers()` after `schedule.run`, next to the `Messages<InEvent>::update()`. It also advances `last_change_tick`; confirm no world-level `is_changed` read depends on the old value.
- *Test.* A unit test asserting the buffer stays bounded across steps, if bevy exposes enough to observe it. Otherwise the fix is covered by review.

### 1. Supervisor and panic logging

**Problem.**
- The daemon starts the reactor with `tokio::spawn(reactor.run())` and drops the handle. A panic in a system kills that task while the process and its queryables live on, refusing every rpc with `daemon_shutting_down`.
- The zenoh bridge task has the same shape: it logs its error and ends, but the liveliness token stays declared.
- `run` waits only on `ctrl_c`. SIGTERM, which is what systemd stop sends, kills the process without flushing the store's debounced write.
- On the `ctrl_c` path, the reactor still holds `persist_tx`, so the store never flushes either.

**Change.**
- The `select!` moves out of `run` into `Stack::run_until(shutdown: impl Future)`. `run` passes a future that resolves on `ctrl_c` or SIGTERM (`tokio::signal::unix`). Tests pass their own.
- `Stack` keeps `JoinHandle`s for the reactor, the bridge and the store task. `run_until` waits on the first of: the shutdown future, the reactor ending, or the bridge ending.
- **Ordered shutdown:**
  1. close the daemon (liveliness and queryables);
  2. abort the reactor task, which drops the world and its `persist_tx`;
  3. await the store task with a bounded timeout, so it flushes the pending snapshot;
  4. return `Ok` on a signal, or an error naming the dead task (with the `JoinError` for a panic).
  The process exits non-zero, and systemd's `Restart=on-failure` restarts it from the store.
- The reactor holds its own `inbox_tx`, so `run()` can only end by panicking, never by finishing cleanly. The supervisor treats any end as fatal.
- `Reactor::new` installs a `FallbackErrorHandler` that logs through `tracing::error!` with `ErrorContext::kind()` and `name()`, then defers to `match_severity` (bevy's default). A caught panic still resumes its unwind, and a future lower-severity error isn't turned into a crash.
- **What 0.20 catches:** panics in schedule systems, run conditions and applied commands, including commands flushed after a one-shot system. A one-shot system's own body isn't caught; it unwinds straight to the supervisor, and the std panic hook has already logged its file and line.

**Tests.**
- **Supervisor**, at daemon level through `serve()` and `Stack::run_until`:
  - A stub `VendorReg` connector panics in `display_config` (called synchronously in the reactor's effect drain). The test asserts that `run_until` returns an error naming the reactor.
  - A test asserts that a signal-path shutdown flushes a pending store write.
- **Handler**, as a world unit test: call it directly with `BevyError::panic(..)` and `ErrorContext::System`, and assert via `catch_unwind` that it resumes the unwind. No tracing capture is needed.

### 2. publish_greeter_timeout reads `meta.automated`

- `publish_greeter_timeout` runs after `derive_automated` in the tail chain, yet it recomputes `is_automated(resolve_automate_when(..))` for each TV. It should read `meta.automated`.
- `dispatch_system_events` keeps its recompute for now. Item 4 revisits it.
- Existing coverage: `greeter_timeout_key_and_idle_dispatch`.
- New: a world-level test where an input change flips `automated` in the same step as the timeout publish, which pins the tail-chain order.

### 3. Store snapshots: rebuild, diff, seed before spawn

**Change.**
- **Snapshot.**
  - Delete `DirtyStore` and its 14 marks.
  - `snapshot_store` builds the `TvRecord`/`ProfileRecord` lists every step, using 0a's filter.
  - It compares them to a `Local<Option<StoreSnapshot>>` and pushes `Persist` only on a difference.
  - With a handful of TVs, cloning their records a few times a second is trivial, and there's no wake list to get wrong.
- **Boot.**
  - `serve()` takes the loaded store snapshot and the seed records.
  - Before spawning the reactor, it runs one synchronous `reactor.step(..)` over the seed batch: the default `AddProfile`, the store profiles, the store TVs, then seed TVs not already stored.
  - It sets the diff baseline to the loaded store snapshot, through a `Reactor` method that writes the `Local` or a resource the system reads.
  - Replay therefore writes nothing. The first `Persist` is a real change, such as a new seed TV, matching today's "seed only adds" behavior.
  - The async replay in `run` goes away, and the default-profile send in `serve` folds into the seed batch.
- **New behavior to accept:** a blank TV name filled from the device model on an ordinary connect now persists, where today it never reaches the store.

**Tests.** They go through a `spawn_reactor_with_store` support helper that exposes the persist channel. The existing helper has no tap.
- *Exactly one `Persist` follows each mutation kind:*
  - set name, managed, system profile or primary input;
  - update config;
  - re-pair success;
  - fresh-pair verify;
  - profile update, duplicate and delete.
  Add-TV and profile round-trips are already covered by `restart_preserves_tvs_via_store` and `store_round_trips_profiles_and_assignment`; don't duplicate them.
- *No `Persist` follows a derived-only change:* an input switch flipping `automated`, or remote arm/disarm.
- *Boot:* a daemon booted from a store emits no `Persist` until something changes.

### 4. Client commands as one-shot systems

Two commits: a move-only module split, then the behavior change.

**4a. Module split, moves only.** `systems.rs` is 2,690 lines. Split it so that helpers shared by command handlers and fact systems don't strand:
- `systems/mod.rs`: `build_schedule`, plus `redial`, `spawn_tv`, `despawn_tv` and `upsert_payload`;
- `systems/dispatch.rs`: the screen/power tier (`dispatch_screen`, `power_on_tv`, the `arm_*` helpers), `verify_dispatch`, `reconcile_input` and `dispatch_system_events`;
- `systems/profiles.rs`: `profile_body`, `resolve_automate_when`, `resolve_action` and `is_automated`;
- `systems/facts.rs`: `apply_backend_events`, the transport, connector, carrier, route and pairing-update systems, and `reconcile_connections`;
- `systems/publish.rs`: the derive pass, `publish_changed`, `publish_greeter_timeout` and `snapshot_store`;
- `systems/commands.rs`: the three `handle_*_commands` systems and their private helpers (`send_backend`, `apply_config_patch`, `apply_managed`, `build_record`).

No logic changes, so it's reviewable with `git diff --color-moved`.

**4b. One-shot command handlers.**
- **Reactor.** `step` first applies the batch's `Tick`s (commands see the batch's clock, as today). It then takes each `InEvent::Command` in arrival order and runs it through one exhaustive `match`, before writing the remaining facts as messages:

  ```rust
  match cmd {
      Command::DeleteProfile(input, reply) => {
          let result = self.world.run_system_cached_with(delete_profile, input)
              .expect("command handlers are fn items registered on first use, never removed");
          self.answer(reply, result);
      }
      // …
  }
  ```
- **Flushing.** `run_system_cached_with` runs the handler and flushes its `Commands` on return (verified in 0.20, `World::run_system_with`). Hooks (item 5) fire before the next command.
- **`answer`.**
  - It pushes the reply into the `Outbox` after the handler's flush. Replies therefore follow the command's own publishes and the hooks' deletes, as today.
  - A `None` reply (boot seed) logs refusals at `warn` instead of dropping them silently.
- **Handler shapes.** Handlers are plain systems taking `In<Input>`.
  - *Immediate reply:* every command except the five below returns `Result<R::Response, WireError>`. That includes `PowerOn` and `SetScreen`, which reply once the action is sent or the wake dial is armed. `power_on_tv` and `dispatch_screen` return `Result<(), WireError>`, and `dispatch_system_events` logs their errors.
  - *Deferred reply:* `SetVolume`, `SetMuted`, `SwitchInput`, `SetBacklight` and `PowerOff` ack on `CommandDone`. They take their `Reply` (which is `Copy`) in the input and hand it to the backend. Validation failures return `Err`, and the reactor answers those.
  - *`GetConfig`:* the handler returns the `TvRecord`. The reactor arm redacts it with the connectors it owns, which deletes `Effect::RedactConfig`.
- **Command shape.** Each `Command` variant becomes a newtype around its input struct, with the reply alongside: `Command::X(XInput, Option<Reply<..>>)`. Input structs carrying a `VendorConfig` (`Pair`, `AddTv`, `UpdateConfig`) keep the key-names-only `Debug`. This touches about 80 construction sites in the bridge and the world tests, all in-repo, with no shims.
- **Deleted:**
  - the three `handle_*_commands` systems, so the ingest chain starts at `apply_backend_events`;
  - `command_reply`, `respond` and the `_ => {}` arms;
  - `Effect::RedactConfig`;
  - the `RemoveTv` route-monitor intercept in the fact loop (item 5 replaces it);
  - the 20-tuple chain-cap comment, if the chains no longer need splitting.
- **Kept:** `reply_ok`/`reply_err` survive for `CommandDone` acks in `apply_backend_events`.
- **Derive dedup, part two.** Profile edits now apply before the schedule, but input changes still arrive as facts in the ingest chain. So `derive_automated` moves to just before `dispatch_system_events`, which can then read `meta.automated` too. The comment on the order says why.
- **While touching `reactor.rs`:** drop its stale "(M2)" phase references.
- **Unchanged:** facts (backend events, carrier, routes, DPMS, pairing updates) stay messages in the schedule, because "latest wins per key" is the right batch meaning for facts. The two collapse-to-latest systems stay.

**Tests.**
- The end-to-end world tests through `WorldClient` pin behavior.
- New cross-family arrival-order tests use 0b's batch helper. Both fail today, because commands run per family, not in arrival order:
  - `[DeleteProfile X, AddTv { system_profile: X }]`: the delete succeeds and the add is refused with `profile_not_found`;
  - `[SetPrimaryInput Y, PowerOn { restore }]`: the restore targets Y.
- Plus `[AddTv X, AddTv X]`: the second is refused with `tv_exists`.

### 5. Immutable keys, hook-maintained maps

**Change.**
- **Keys.** `TvId`, `ProfileId` and a new `PairingFor(Uuid)` on pairing-session entities become `#[component(immutable)]`. A key can't change without the hooks seeing it.
- **Maps.** `TvIndex`, `ProfileIndex` and `PairingIndex` keep a private map and expose read-only `get`/`contains`.
  - `on_insert` adds the entry and logs a key collision at `error`.
  - `on_discard` (0.20's name; it runs on replace, remove and despawn) removes the entry, but only when the map still points at this entity.
- **Removal side effects** go in `on_remove`, never `on_discard`. Re-inserting the same key must not delete a live TV's keys.
  - `TvId` queues a wire `Delete` for every `TV_PATHS`/`TV_EXTRA_PATHS` key, plus a new `Effect::ForgetRoute`. That fixes a route-monitor leak on fresh-pair rollback, which today never sends `Forget`.
  - `ProfileId` queues `profile_system`.
  - Both are guarded on map identity, like `on_discard`.
  - Hooks reach `ResMut<Outbox>` and `Res<Site>` through `DeferredWorld`.
- **Documentation.** Every hook-carrying component's doc comment says what its hooks do.
- **Spawns and despawns.**
  - Spawn sites stop touching the maps. `despawn_tv` shrinks to the despawn. `DeleteProfile` no longer pushes its own delete.
  - `spawn_tv` returns the new `Entity`. `apply_pairing_updates` uses it to insert `PairVerifying`, or puts `PairVerifying` in the spawn bundle. Today it reads `tv_index` right after spawning, and with hooks that lookup would miss.
- **The maps lag inside schedule systems.** Within a single system, hooks run when its `Commands` flush, at system end. So inside one fact system the maps lag that system's own spawns and despawns:
  - Fact systems that despawn mid-loop (the rollback and pairing-terminal paths in `apply_backend_events` and `apply_pairing_updates`) track the uuids they despawned this run and skip later messages for them.
  - Or they use `try_insert`, since an `insert` on a despawned entity goes to the fallback handler as a panic.
  - "The map is current" holds between commands (item 4's flush per command), not within a fact system.
- **Teardown.** Settled in review: `World` has no `Drop` impl that runs hooks, and the `Outbox` only drains inside `step`. So dropping the world at shutdown or on a panic emits no deletes.

**Tests.**
- **Map consistency**, under `cfg(debug_assertions)` at the end of `step` (integration tests build the library without `cfg(test)`, so a test-only check wouldn't run there): each map equals its key query, both ways. The panic message states the invariant.
- **Existing coverage** of removal deletes: `remove_tv_is_boring` and `remove_emits_deletes_and_late_joiners_see_nothing`.
- **New:**
  - a fresh-pair rollback emits `ForgetRoute`;
  - a fresh pair still verifies before persisting, which pins the `spawn_tv` entity fix;
  - a batch with a pairing terminal followed by a late backend event for the same TV doesn't panic.

### 6. Docs and ideas

- **`docs/architecture.md` § The world:**
  - a "command flow" paragraph: each command runs as a one-shot system, then flushes, then the hooks run, then the next command, then the schedule over the facts;
  - why observers were rejected;
  - hook-maintained maps and the removal side effects;
  - the narrowed same-batch hazard note: facts only, and fact systems that despawn mid-loop;
  - rebuild-and-diff snapshots;
  - seed before spawn;
  - the supervisor.
- **§ Persistence** drops `DirtyStore` and describes the diff baseline.
- **`docs/ideas/typed-failed-reason-payload.md`** gains a bullet: `Meta.automate_when` is a `String` token; type it as `tisvlay_core::AutomateWhen` end to end, as with the stringly pairing phase.
- **`docs/ideas/supervised-task-helper.md`:** the daemon's supervisor now exists, using exit-on-death rather than restart-in-process. Note it as the model, and keep the in-process restart loops (tray link, idle adapter, netlink monitor) as the remaining idea.
- **Retire this plan** per `docs/plans/README.md`.

## Not doing

- **Gating the derive pass on change detection.** With a handful of entities it costs nothing, and recomputing can't go stale. Gating has traps: a new entity must be computed, a profile edit touches every TV, and the derive pass's own `Meta` write re-triggers it.
- **Splitting `Meta` into stored and derived components.** The snapshot diff makes it unnecessary for saves. A split would change the wire key or the payload shape for every client.
- **Log-and-continue on panic.** A panicked system has just broken an invariant, so crash-and-restart from the store is the safe recovery.
- **Minor 0.20 additions.** `chain_weak` gives nothing at our scale, and our ordering also depends on side channels bevy can't see. `shuffle_seed` has nothing to shuffle in a fully chained schedule. Error `.context()`, `despawn_all`, `resource_entity` and per-column change ticks have no use here.
- **Observers for commands.** See [Decisions](#decisions).

## Resolved in review

- **Commands from within the world.** There are none. Commands come only from the bridge and the daemon's boot seed. Systems push `Effect`s, and the reactor's self-sends are facts (`TransportFacts`, `PairingUpdate`, `Backend`).
- **Handler input types.** Each variant becomes a newtype around its input struct (item 4b).
- **Panic test seam.** A stub connector that panics, driven through `serve()` and `Stack::run_until`. No production seam.
- **Hooks at teardown.** None fire (item 5).
- **Executor.** A bare `Schedule::default()` runs single-threaded, because bevy_ecs's `multi_threaded` feature is off. Panics are caught by `handle_unwind` and reach the fallback handler with the system's name.

## Test plan

- `just check` after each commit.
- New tests per item, as listed above. Each fix in item 0 fails before its fix.
- Field: a week of normal use on the C3/G4, with a pair, a re-pair (including a deliberately failed one), profile edits, `systemctl restart` and `systemctl stop`. Check that state hydrates, the store matches the world, and nothing is deleted on restart.

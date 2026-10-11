# World ECS polish

Branch `feat/bevy-polish`, stacked on the bevy_ecs 0.20 bump (#151). This came out of a review of the 0.20 changelog against how `tisvlay-world` uses the ECS. The release itself offered little we need. The review did surface one robustness gap and several places where the world hand-rolls what Bevy provides.

## Goals

1. A panic in the world restarts the daemon instead of leaving it up but refusing every rpc.
2. Client commands run one at a time with read-your-writes semantics, and a missing handler doesn't compile.
3. The uuid→entity maps can't drift from the entities they index, and despawns can't forget their wire deletes.
4. Store saves are derived from what changed, not from a flag set by hand at 24 sites.

## Decisions

Owner's rulings, 2026-10-10:

- **Supervision:** exit when the reactor ends, plus a bevy panic-logging handler. No log-and-continue, and no `panic = "abort"`.
- **Commands:** run them as one-shot systems from the reactor's exhaustive `match`. Observers were rejected, because a missing observer drops a command silently and only a test can catch that.
- **Maps:** make the key components immutable, and maintain private maps with component hooks. Hooks also queue the wire deletes. The registry `SystemParam` alternative was rejected; the owner prefers the idiomatic design over the smaller diff.
- **Store saves:** change detection wakes `snapshot_store`, which diffs against the last saved snapshot. Each mutation kind gets a save test.
- **Derive pass:** dedup only. `publish_greeter_timeout` reads `meta.automated`. No change gating.
- **Not doing** (reasons under [Not doing](#not-doing)):
  - splitting `Meta`;
  - typed `automate_when` (ideas entry only);
  - the minor 0.20 additions.

## Work items

They're in landing order. Each item is one commit, and each passes `just check` on its own.

### 1. Reactor supervision and panic logging

**Problem.** The daemon starts the reactor with `tokio::spawn(reactor.run())` and drops the handle (`tisvlay-daemon/src/lib.rs`). The release profile unwinds on panic. So if a system panics, the reactor task dies, while the process and its queryables live on and refuse every rpc with `daemon_shutting_down`. systemd's `Restart=on-failure` never fires.

**Change.**
- `Stack` keeps the reactor's `JoinHandle`.
- `run` replaces its lone `ctrl_c().await` with a `select!` over `ctrl_c` and the handle.
- If the reactor ends first, `run` closes the daemon and returns an error naming the cause. A panic gives a `JoinError` and a clean end gives "world reactor stopped". The process exits non-zero, and systemd restarts it from the store.
- `Reactor::new` installs a `FallbackErrorHandler` that logs through `tracing::error!` with the failing system's name and kind (from `ErrorContext`). It then defers to bevy's `panic` handler, which resumes the unwind, so behavior stays crash-and-restart.
- bevy 0.20 catches panics in schedule systems, run conditions and commands, but not in one-shot systems (item 2). Those unwind straight to the supervisor, and the std panic hook still logs their file and line.

**Tests.**
- A daemon-level test injects a panicking system through a test-only hook (or panics a connector stub inside the world) and asserts that `run` returns an error.
- A world-level test asserts that the handler logs the system name. Use a tracing test subscriber, or assert on the error the handler receives.

### 2. publish_greeter_timeout reads `meta.automated`

`publish_greeter_timeout` runs after `derive_automated` in the tail chain, yet it recomputes `is_automated(resolve_automate_when(..))` for each TV. It should read `meta.automated`. `dispatch_system_events` keeps its own recompute, because it runs in the ingest chain before the derive pass and must see a profile edit that arrived in the same batch. A comment there should say so.

The existing greeter-timeout tests should pass unchanged. Add one where an input change flips `automated` in the same step, to pin the ordering.

### 3. Change-detected store snapshots

**Problem.** `DirtyStore(bool)` is set by hand at 24 sites, and a missed mark silently skips a save. Plain `Changed<Meta>` is too coarse, because `Meta` mixes stored facts (`managed`, `system_profile`, `primary_input`) with derived projections (`automate_when`, `automated`, `remote_armed`).

**Change.**
- Delete `DirtyStore`.
- `snapshot_store` wakes when any of the following fires:
  - `Changed<VendorRef>`, `Changed<payload::Identity>`, `Changed<payload::Meta>` or `Changed<ProfileBody>`;
  - `RemovedComponents<TvId>`, `<ProfileId>` or `<PairVerifying>`.
- When it wakes, it builds the `TvRecord`/`ProfileRecord` lists exactly as it does today and compares them to a `Local<Option<Snapshot>>`. It emits `Persist` only when they differ.
- Derived-field churn costs a rebuild, never a write.
- The first run always persists, which keeps today's boot behavior; confirm against the store task's debounce.
- Verify before relying on `RemovedComponents`: the reactor never calls `World::clear_trackers()`. Removal messages need it once per step, or they accumulate. Add it after `schedule.run`, next to the `Messages<InEvent>::update()`.

**Tests.** There's one test per mutation kind asserting that exactly one `Persist` follows, and one per derived-only change asserting that none does.
- *Mutations:*
  - add, remove and rename a TV;
  - set managed, system profile and primary input;
  - update config;
  - re-pair success;
  - fresh-pair verify (leaving `PairVerifying` makes the TV persistable);
  - add, update, duplicate and delete a profile.
- *Derived-only changes:* an input switch flipping `automated`, and remote arm/disarm.

These tests land before items 4 and 5 and pin save behavior across the refactor.

### 4. Client commands as one-shot systems

**Problem.**
- `handle_lifecycle_commands`, `handle_tv_commands` and `handle_profile_commands` loop over a batch with deferred `Commands`.
- Every later command in the batch reads state from before the batch's writes apply. That's why the maps are updated by hand mid-loop.
- The 24 `Command` variants are matched inside three systems that also own all the reply plumbing.

**Change.**
- The reactor's `step` takes each `InEvent::Command` out of the batch, in arrival order, before writing the remaining facts as messages. Each runs through one exhaustive `match`:

  ```rust
  match cmd {
      Command::DeleteProfile { id, reply } => {
          let result = world.run_system_cached_with(delete_profile, DeleteProfile { id });
          answer(world, reply, result);
      }
      // …
  }
  ```
- `run_system_cached_with` runs the handler immediately and flushes its `Commands` on return (verified in the 0.20 source, `World::run_system_with`). Hooks (item 5) fire before the next command, so a command always sees the previous one's results.
- Handlers are plain systems taking `In<Input>` and returning a result.
  - **Immediate-reply commands**, the profile family plus `AddTv`, `SetName`, `SetManaged`, `GetConfig`, `UpdateConfig` and the rest, return `Result<R::Response, WireError>`. The reactor answers the `Reply<R>` in one place.
  - **Deferred-reply commands**, the TV commands that ack on `CommandDone` (`SetVolume`, `SetMuted`, `SwitchInput`, `SetBacklight`, `SetScreen`, `PowerOn`, `PowerOff`), take their `Reply` in the input and hand it to the backend. Validation failures still return an `Err` the reactor answers.
- The `RemoveTv` route-monitor `Forget` intercept moves into that arm.
- The three `handle_*_commands` systems leave the schedule. The ingest chain starts at `apply_backend_events`.
- Handlers live in a new `systems/commands/` module split by family (lifecycle, tv, profiles), which also shrinks `systems.rs` (2,690 lines).
- `Command` stays the inbox transport. Its exhaustive match is the compile-time completeness check, the same pattern as the endpoint families' `Incoming`.
- **Unchanged:** facts (backend events, carrier, routes, DPMS, pairing updates, ticks) stay messages processed in the schedule. Their batch semantics, latest-wins per key, are correct for facts. The two collapse-to-latest systems and the architecture.md "same-batch hazard" note stay, but the note narrows to facts.

**Tests.**
- The existing end-to-end world tests through `WorldClient` pin behavior.
- New same-batch tests send these pairs in one batch:
  - two `AddTv` with the same uuid → the second gets `tv_exists`;
  - `DeleteProfile X` then `AddProfile X` → both succeed;
  - `AddProfile X` then `UpdateProfile X` → both succeed.
- Today the first and third cases are correct only because of the hand-updated maps. The new tests keep them correct after those updates go away.

### 5. Immutable keys, hook-maintained maps

**Change.**
- `TvId`, `ProfileId` and a new `PairingFor(Uuid)` on pairing-session entities become `#[component(immutable)]`. A key can't be changed without the hooks seeing it.
- `TvIndex`, `ProfileIndex` and `PairingIndex` keep a private map and expose read-only `get`/`contains`.
  - `on_insert` adds the entry.
  - `on_replace` removes the old entry, which covers re-insertion with a new key.
  - `on_remove`/despawn removes the entry.
- The wire deletes move into the `on_remove` hooks: `TvId` queues a `Delete` for every `TV_PATHS`/`TV_EXTRA_PATHS` key, and `ProfileId` queues `profile_system`. Hooks get a `DeferredWorld` with `ResMut<Outbox>` and `Res<Site>`.
- `despawn_tv` shrinks to the despawn plus any non-key teardown. The `DeleteProfile` arm no longer pushes its own delete.
- Every hook-carrying component's doc comment says what its hooks do.
- Spawn sites stop touching the maps. Combined with item 4's flush per command, the map is current before the next command reads it.
- **Check during implementation:** whether a hook fires on world teardown at shutdown, which would emit deletes for every TV. If it does, have the reactor drop the world without despawning, or gate the hook on a shutdown flag. Retained publishers must not be told to delete state on a daemon restart.

**Tests.**
- A test-only consistency check runs after every `step`: each map's entries equal its key query, both ways.
- The existing removal tests assert that the deletes still go out.
- A restart test asserts that no deletes go out on shutdown.

### 6. Docs and ideas

- `docs/architecture.md` § The world:
  - commands run as one-shot systems before the schedule, facts as messages within it;
  - hook-maintained maps and the despawn wire deletes;
  - change-detected snapshots replace the dirty flag;
  - reactor supervision.
- § Persistence drops `DirtyStore`.
- `docs/ideas/typed-failed-reason-payload.md` gains a bullet: `Meta.automate_when` is a `String` token; type it as `tisvlay_core::AutomateWhen` end to end, as with the stringly pairing phase.
- `docs/ideas/supervised-task-helper.md` gains the reactor as a fourth supervised shape: exit-on-end, not restart-in-process.
- Retire this plan per `docs/plans/README.md`.

## Not doing

- **Gating the derive pass on change detection.** With a handful of entities it costs nothing, and recomputing can't go stale. Gating has traps: a new entity must be computed, a profile edit touches every TV, and the derive pass's own `Meta` write re-triggers it.
- **Splitting `Meta` into stored and derived components.** The snapshot diff (item 3) makes it unnecessary for saves. A split would change the wire key or the payload shape for every client.
- **Log-and-continue on panic.** A panicked system has just broken an invariant, so crash-and-restart from the store is the safe recovery.
- **Minor 0.20 additions.** `chain_weak` gives nothing at our scale, and our ordering also depends on side channels bevy can't see. `shuffle_seed` has nothing to shuffle in a fully chained schedule. Error `.context()`, `despawn_all`, `resource_entity` and per-column change ticks have no use here.
- **Observers for commands.** See [Decisions](#decisions).

## Open questions

- **Handler input types.** Either reuse the `Command` variant's fields as a per-command input struct, or make each variant a newtype around its input struct (`Command::DeleteProfile(DeleteProfile, Option<Reply<..>>)`) so the match passes it straight through. The newtype is cleaner. It touches the bridge's construction sites, all in-repo.
- **Commands from within the world.** If any system queues a `Command` back into the inbox (seed replay goes through `WorldClient`, so it doesn't), it still arrives as an inbox event. Confirm there's no in-world shortcut that bypasses the reactor's match.
- **Test hook for the panic test.** Either a `#[cfg(test)]` system injected into the schedule, or a connector stub that panics inside a world-side callback. Choose whichever needs no production seam.

## Test plan

- `just check` after each item.
- Items 3–5 add the save, same-batch and map-consistency tests described above.
- Field: a week of normal use on the C3/G4, with a pair, a re-pair, profile edits and a daemon restart. Check that state hydrates and nothing is deleted on restart.

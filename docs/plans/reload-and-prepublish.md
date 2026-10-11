# Reload, typed wire strings, netlink restart, and pre-publish

A train of four cars, planned together because they share files and ordering constraints. Car 4 is split into three PRs, so the train ships as six PRs in all. It came out of revisiting the ideas filed by #150–#152 and preparing the `publish = true` crates for their first crates.io release. No crate is on crates.io yet, so every API change here is free; after the first publish, most of it would be a breaking release. The plan was reviewed by three agents (design, implementation correctness, maintainability), and their findings are folded in.

## Order

1. **Reload config + fsync.**
2. **Typed wire strings.**
3. **Netlink monitor restart.**
4. **Pre-publish**, in three PRs: (i) prune, move and unpublish; (ii) the type-level audit; (iii) checklist mechanics.

Cars 1 and 3 touch no published crate. runtime, wire, world, daemon, session, tray, the SDK and the GUI are all `publish = false`. Only car 2 gates car 4, because it reworks core's pairing and power vocabulary. Car 4 can proceed in parallel with cars 1 and 3. The real conflict points are:
- `tisvlay-runtime`'s `Cargo.toml` and lib (car 1's `write_atomic` vs 4(i)'s moved glue);
- the GUI's wizard and `compat.rs` (car 2 vs 4(i));
- `tisvlay/src/lib.rs` (cars 1 and 2 both touch the SDK, so land them in sequence).

## Decisions

Owner's rulings, 2026-10-11:

- **Reload re-reads the stores**, not the seed config. The daemon, session and tray are all covered, as the old button did. File watching was rejected: it's uncommon for daemons, and save-by-rename, half-written saves and the daemon's own writes make it fragile.
- **Netlink monitor:** restart in place, as its own car.
- **Pre-publish scope:** the API audit, the checklist mechanics, and core error pruning.
- **`tisvlay-mock` becomes `publish = false`.** #80 set `publish = true` to unblock the old `tisvlay-backends` crate (now gone) and to reserve the name. Neither reason justifies maintaining a public API for a test double.
- **App glue moves from core to `tisvlay-runtime`.** Only the GUI and tray use it, and both already depend on runtime.
- **Dependency types in public APIs:**
  - **Wrap** `linux_cec::PhysicalAddress` in a `tisvlay-cec` newtype. linux-cec is 0.x, seven months old, and has already broken once.
  - **Leave** core's `Config(#[from] toml::de::Error)` and `Json(#[from] serde_json::Error)`, `VendorConfig`'s `toml::Value`, and the toml serialize error `from_typed` returns. Both crates are 1.x, and the opaque-error wins were judged too small.
  - **Accept** tokio, uuid, bitflags, serde, toml and serde_json (core) and futures-core (ssdp) as public dependencies, and document them in each crate's docs.

  The grounding is the API guidelines' C-STABLE, the nightly-only status of cargo's `public = true` (RFC 3516), and how reqwest, axum and tokio-rustls handle the same question.
- **`tisvlay-lg` goes private** except the connector, backend, discovery, config and `VENDOR_NAME`. Making something public later isn't a breaking change; making it private later is.

Settled in review, where all three reviewers converged:
- **`ReloadReport`** is `{ changed: bool }`. Adding fields later costs nothing, since every client is in the repo.
- **SIGHUP is daemon-only.** The session and tray reload through their rpc and the CLI. Dev runs are foreground, so closing the terminal sends SIGHUP; a user-unit handler would turn that into a reload and leave an orphan holding the user socket.
- **Async trait style:** leave the mix and document it in `traits.rs`. The world holds `Arc<dyn TvConnector>`, which needs `#[async_trait]`; `TvDiscovery` is used generically and already has the `DynDiscovery` wrapper.
- **Car 4's split:** (i) 4a + 4b + 4d, the deletions, moves and unpublish, which share files; (ii) 4c, the judgment calls; (iii) 4e, last, so packaging checks run against the final API.
- **Wire enums new in car 2 are exhaustive.** Every client is in the repo, and exhaustive GUI matches are the point.

## Car 1: reload config + fsync

**State today.**
- The GUI's sidebar Reload button (`tisvlay-gui/ui/main.slint:263-328`, whose comment and tooltip name the stale `config.toml`) calls the SDK's `reload_config()` on the daemon, session and tray clients (`tisvlay/src/lib.rs:297`, `:776`, `:977`). All three are no-ops. A tray reload failure only `warn!`s and never reaches a banner (`tisvlay-gui-app/src/lib.rs:1684`).
- **Store writers:** three copies of tmp+rename (`tisvlay-daemon/src/store.rs`, `tisvlay-session/src/store.rs`, `tisvlay-tray/src/notify.rs`). None fsyncs, and the temp file is created under the process umask: a dev daemon without the unit's `UMask=0077` leaves client keys at 0644. The tray's writer ignores a failed temp write.
- **Loading:** `load_store` reads a missing file as an empty store.
- **Units:** none has `ExecReload`, and nothing handles SIGHUP.

**The race the design must avoid.**
- **`LastPersisted` isn't what's on disk.** It's the snapshot the world *sent*; the daemon's store task writes it after a 300 ms debounce. The session writes on every mutation, through the same debounce.
- **Effect:** a reload that compares the file with `LastPersisted` treats the daemon's own pending change as a hand edit and reverts it. A rename can be undone this way, and so can a re-pair's fresh client key. The stale snapshot still in the channel is then written over the reloaded state.
- **Rule:** so the store task, which knows what's actually on disk, owns reload.

**Design.**
- **Shared `write_atomic`** in `tisvlay-runtime` (a `fs` module behind a feature):
  - create `<path>.tmp` with mode 0600, write, `sync_all`, rename, then fsync the directory;
  - remove the temp file if any step fails, and log every failure;
  - the async store tasks call it through `spawn_blocking`.
- **The store task owns the on-disk baseline.** It keeps `base`, the snapshot it last wrote, or loaded at boot (`serve` hands it the loaded snapshot). Comparisons are between parsed snapshots canonicalized by sorting by id, never bytes.
- **Reload is a three-way merge:**
  - `base` is what we last wrote, `disk` is the file now (possibly hand-edited), and the world holds current state, including changes still pending a write;
  - the reload applies exactly the hand edit: the per-record, per-field changes from `base` to `disk`, onto the world;
  - the world's own pending changes survive, and a field changed on both sides takes the disk value (an explicit reload is the user's intent);
  - the merged world then persists normally.
- **Daemon flow.**
  - The bridge's `reload` arm asks the store task over a request channel.
  - The store task reads and parses the file. It **refuses**, naming the path, on: a missing file (never "empty", which would despawn every TV), a parse error, a permission error, or an ephemeral daemon with no store.
  - It computes the base→disk delta. An empty delta answers `changed: false`.
  - It sends `Command::ApplyStoreEdit { delta }`, then sets `base = disk` once the world accepts.
- **The world handler validates the whole delta first and refuses all of it**, naming the offending entry, on any of these:
  - a changed `vendor`;
  - removing or renaming the default profile;
  - deleting a profile a TV still uses;
  - an unknown vendor.

  It then applies through the same helpers the client commands use:
  - `spawn_tv` for additions;
  - the `remove_tv` logic for removals, so an in-flight re-pair session is cancelled too;
  - `apply_managed`, so the `Managed` marker and redial follow the flag;
  - `apply_config_patch`-style vendor merges, which redial;
  - `publish_profile` for profile bodies.

  It skips a TV with `PairVerifying` or a live pairing session, and logs the skip.
- **Unreloaded hand edits.** Before writing, the store task compares the file on disk with `base`. If they differ, a hand edit hasn't been reloaded. The task moves the file aside to a single `<path>.edited` (mode 0600, overwritten by the next set-aside, since the store holds LG client keys and timestamped copies would pile up credentials), logs a warning naming it, then writes. Because reload sets `base = disk`, an accepted edit never triggers this.
- **Session.**
  - Its store task gets the same `base` and three-way reload.
  - The `Core` task applies the user-profile and assignment delta.
  - It calls `delete_key` for removed profiles and assignments, and `recompute_idle` plus `recompute_remote_armed`/`publish_remote` as `delete_profile` does.
  - It keeps the default profile's pinned name, and refuses when there's no store.
- **Tray.**
  - It writes synchronously on each update, so `disk == last write` except after a hand edit, and reload can re-read `tray.toml` and apply it like `Update`.
  - Boot's `unwrap_or_default` stays for boot only: a reload refuses a parse error.
- **SIGHUP (daemon only).**
  - `main` installs the handler first thing, since SIGHUP's default action kills the process and a `Type=simple` unit counts as active at once.
  - It triggers the same store-task reload and logs the outcome, since `systemctl reload` reports success asynchronously.
  - `ExecReload=kill -HUP $MAINPID` goes in the systemd unit, and `${pkgs.util-linux}/bin/kill` (an absolute path) in the NixOS module.
  - `Type=notify-reload` is noted as the way to make reloads synchronous, and deferred.
- **Rpcs, SDK, CLI, GUI.**
  - `manager/rpc/reload`, `user/profile/rpc/reload` and `user/tray/rpc/reload`, each `(()) -> rpc::ReloadReport, write`.
  - `reload_config()` calls them and returns the report.
  - A `tisvlay reload` CLI verb, per the add-the-surface-by-hand convention.
  - The GUI fixes the stale comment and tooltip (then `just i18n-extract` and the `.pot` diff), and routes a tray reload failure to a banner.

**Commits** (each green; each retirement rides with the commit that makes it true):
1. `write_atomic` and the three writers. Retires `docs/ideas/atomic-config-writes.md` and its index entry.
2. The daemon store task's `base`, the reload request, the world's `ApplyStoreEdit` handler, the manager endpoint and its bridge arm (one commit: the compiler demands the arm), the SDK, and tests.
3. Session reload.
4. Tray reload.
5. SIGHUP, the unit, the NixOS module, and `packaging/README.md`.
6. GUI text and banner, the `.pot`, and the CLI verb. Retires `docs/ideas/reload-config.md` and its index entry, and updates `docs/architecture.md` § Persistence.
7. The unreloaded-edit set-aside.

**Tests.**
- **World:** the handler applies an add, a remove (which cancels an in-flight re-pair), a rename, a managed flip (marker and redial), a vendor-config change (redial) and a profile edit. Validation refuses a vendor change, the default profile's removal and a profile still in use, all-or-nothing. A verifying TV is skipped.
- **Daemon store task:**
  - the three-way merge keeps a pending world change while applying a disjoint hand edit;
  - an unchanged file reports `changed: false`;
  - a missing file, a parse error and an ephemeral daemon are refused;
  - the set-aside fires for an unreloaded edit, and not after a reload.
- **Daemon end to end:** through `serve`, a hand-edited store plus the reload rpc yields the new state on the wire. Nextest runs each test in its own process, so a SIGHUP test can send a real signal after the handler is registered.
- **Session and tray:** the reload round-trips, and the refusals.
- **`write_atomic`:** the result, the mode, and temp-file cleanup when a step fails.

## Car 2: typed wire strings

**State today.**
- **`PairingProgress.phase`** is a `String`: `awaiting_tv`, `awaiting_input`, `complete`, `failed`.
  - It's produced by core's `PairingState::to_dbus_state()` (used only by wire's `PairingProgress::from_state` and core's own tests) and by world literals.
  - It's matched by text in the SDK's `drain_pair_to_terminal`, the CLI, the tray, the daemon bridge's liveliness drop, and the GUI wizard. The Slint side compares strings.
  - The SDK already decodes a typed `PairingProgress`; only `phase` and `code` are strings.
- **`PairingProgress.code` and `DialFailure.code`** are `Option<String>`: `pairing.{rejected,timeout,connection-failed,invalid-input,cancelled,unknown}` and `device.permission-denied`, from core's `PairingErrorCode` and `DeviceErrorCode`. Neither derives serde.
- **`progress/power`** is an untyped `json!({"phase": &str})`. Its phases are `wol_sent`, `direct_connect`, `connecting`, `connected`, `failed` and `awaiting_network`. **Live bug:** core's `PowerCyclePhase` has no `awaiting_network`, and the SDK silently drops phases it doesn't recognize, so the GUI never sees it.
- **`Meta.automate_when`** is a `String` written from `AutomateWhen::to_string()` every step; no non-test client reads it. The GUI duplicates its token mapping for profile bodies.
- **`TvInfo.power_state`** is a `String`, which is the only reason core's `power_state_str_is_on` exists.

**Changes.** All of them live in `tisvlay-wire` unless noted, keeping serde forms out of core's published API.
- **Phase:** wire `PairingPhase` (exhaustive, `rename_all = "snake_case"`) with `From<&PairingState>`. `to_dbus_state` and its tests are deleted here (wire depends on core, not the reverse).
- **Codes:** wire `FailureCode` (exhaustive, a per-variant `rename` copying `ErrorCode`'s macro) with `From` impls from core's two code enums. `DialFailure.code` carries it.
- **Power phase:** a typed power-progress payload with a wire `PowerPhase` that includes `AwaitingNetwork`. The SDK, tray and GUI switch to it. Core's `PowerCyclePhase` goes in 4a if nothing else uses it.
- **`Meta.automate_when`** becomes `tisvlay_core::AutomateWhen`, with the same JSON. The GUI's duplicate token helpers switch to its `FromStr`/`Display`.
- **`TvInfo.power_state`** becomes typed (`PowerState`). `power_state_str_is_on` is then deleted in 4a rather than moved.
- **Consumers:**
  - the CLI and tray decode the typed payloads instead of `serde_json::Value`;
  - the bridge's liveliness check uses `serde_json::from_value` on the published effect's payload;
  - the GUI keeps strings only at the Slint boundary, converting from the enums there.
- **Docs:** prune the bullets this car implements from `docs/ideas/typed-failed-reason-payload.md`, along with its stale D-Bus/`a{sv}`/`tv_manager` prose.

**Commits:** (1) the pairing phase with all consumers; (2) failure codes; (3) the power payload, `automate_when`, `power_state`.

**Tests.**
- No byte-pinning goldens (owner rule): `AutomateWhen`'s existing round-trip test already covers its persisted form.
- Test the Slint-boundary conversions, and that the SDK surfaces `awaiting_network`.

## Car 3: netlink monitor restart

**State today.** `spawn_link_monitor` (`tisvlay-daemon/src/netlink.rs:44`) runs `watch_network` once.
- If the multicast connection fails to open, it logs one warning and stops. If the message stream ends, it exits silently.
- The likeliest stale path doesn't end the stream at all. On a NIC burst the kernel overruns the socket buffer (ENOBUFS), and netlink-proto 0.13 surfaces that as a `NetlinkPayload::Overrun` message (`framed.rs:89`). The loop drops it with `continue`, so lost link and route events are never resynced.
- When the monitor dies, the reactor's route requests go nowhere (`let _ =`), and a TV parked during a network blip never redials.

**Changes.**
- **Extract a pure `MonitorState`:** the monitored and resolved maps plus the carrier map, with `on_link(..)`, `resync(links) -> events` and a re-resolve step that returns `RouteResolved` changes. The I/O loop stays thin.
- **On `Overrun`:** log, then resync. Dump links and re-resolve every monitored TV.
- **A supervised loop:**
  - reopens the connection with capped exponential backoff, logging each failure and restart;
  - keeps serving `RouteRequest`s during backoff;
  - subscribes before dumping, so nothing falls between the dump and the subscription;
  - resets `carrier_state` per connection, then sends every dumped link (the world's carrier handling is idempotent) and re-resolves;
  - exits only when the request channel closes, meaning the world is gone.
- **The link dump uses its own short-lived `new_connection()`,** like route resolution. A dump on the multicast connection's handle could receive an `Overrun` on its *response* path, where netlink-proto 0.13 hits `unimplemented!()` (`connection.rs:291`).
- **Docs:** update `docs/watchers/network.md` and `docs/ideas/supervised-task-helper.md` (the netlink monitor now restarts in place), and drop netlink.rs's stale "ported from the root daemon" line.

**Commits:** (1) extract `MonitorState`, behavior unchanged; (2) overrun resync and the restart loop.

**Tests.**
- `MonitorState` unit tests: carrier transitions, a resync after missed messages, and re-resolution emitting only changes.
- The reconnect loop gets one opener closure; a test drives it with an opener that fails once, then succeeds.
- Field check: provoke an overrun with interface churn (e.g. creating and deleting a few hundred veth pairs) and confirm a parked TV still redials. Restarting NetworkManager doesn't close the kernel socket, so it doesn't exercise the restart path.

## Car 4: pre-publish

### PR (i): prune, move, unpublish (4a + 4b + 4d)

**4a. Prune core.**
- **Errors:** delete `errors::power` (`PowerError`, `PowerErrorCode`) and `ConfigErrorCode`, with `ConfigError::code()` and the unused `ConfigError` variants (`DuplicateTvId`, `InvalidProfileReference`, if still unused), and rewrite the `errors` module doc. `ConfigError` itself stays: the GUI's field validator uses it.
- **D-Bus leftovers:** delete `InputType::{from,to}_dbus_string` and `HardwareId::{to,from}_dbus_tuple`. Keep `Capabilities::to_strings`, which `Error::not_supported` uses, and drop only the D-Bus prose in `capabilities.rs`.
- **Wake:** make `wol::wake` and `WakeOptions` private. Only `wake_with_retry` and `wake_best_effort` are used outside core.
- **From car 2:** delete `power_state_str_is_on` and, if unused, `PowerCyclePhase`.
- **Ideas:** retire `docs/ideas/core-error-vocabulary-pruning.md` and its index entry.

**4b. Move app glue to `tisvlay-runtime`.**
- **What moves:** `Coalescer`, `ReconcilerMap` (with `Reconciler` going `pub(crate)` inside it), `RefreshTrigger`, `TvAssignment`, `references_of` and `user_references_of`, behind a runtime feature that pulls in `tisvlay-core`, uuid, parking_lot and tokio. runtime has no core dependency today, so there's no cycle.
- **What stays:** `SYSTEM_PROFILE_FIELDS`/`USER_PROFILE_FIELDS`, which `profile_plumbing!` generates alongside the merge code; moving them would duplicate the field list.
- **GUI:** delete `tisvlay-gui-app/src/compat.rs`; its fork-seam reason is gone.

**4d. Unpublish mock.**
- `publish = false`, and drop `version` from mock's `[workspace.dependencies]` entry. Unpublished crates are path-only by the root `Cargo.toml`'s convention.
- Fix `release-plz.toml`'s comment, which names a non-existent `tisvlay-harness`.
- Reword `docs/testing.md`'s "not test scaffolding" line, and point `docs/backends/implementing-a-vendor.md`'s "copy from mock" at the repo.
- `docs/build.md`'s count drops to four crates.

### PR (ii): type-level audit (4c)
- **lg first.** Make it private per Decisions, and move `tests/protocol_fixtures.rs` into in-crate unit tests. Update `docs/lg/testing.md`, which mentions the fixtures' location and `WebOsClient`. Then audit what remains.
- **`#[non_exhaustive]` policy:**
  - **Error enums and their code enums:** core's `Error`, `PairingErrorCode` and `DeviceErrorCode`, and ssdp's `Error`.
  - **Options structs:** ssdp's `SearchOptions`.
  - **Stay exhaustive:** enums the world and GUI match exhaustively (`AutomateWhen`, `SessionAction`, `EventAction`, `PairingState`, `PairingCommand`, `Transport`), because the "compiler demands the arm" property matters more than additive evolution at 0.x.
- **Vendor-constructed structs:** `TvState` and `Credentials` go `#[non_exhaustive]` (no struct literals outside core). `InputInfo`, `DiscoveredTv` and `PairingField` stay exhaustive; see Resolved after review.
- **Unit structs** (`LgBackend`, `LgConnector`, `CecBackend`): add a private field plus `new()`/`Default`, so adding state later isn't breaking.
- **`tisvlay-cec`:** a `PhysicalAddress` newtype with parse, display and a crate-private conversion. The test-link feature's `PowerStatus` re-export is documented as test-only.
- **Async trait style:** document the mix in `traits.rs`.

### PR (iii): checklist mechanics (4e, `docs/build.md` § First-publish checklist)
- **Packaging:**
  - Package all four crates in one invocation (`cargo package -p tisvlay-core -p tisvlay-ssdp -p tisvlay-lg -p tisvlay-cec`), so cargo's local-registry overlay resolves the path dependencies. Per-crate `cargo package -p tisvlay-lg` fails, because `tisvlay-core` isn't on crates.io yet.
  - Add it as a `just package-check` recipe, fix checklist step 1 to match, and fix whatever it flags.
  - Note the publish order: core, then ssdp, then lg and cec. Or use `cargo publish --workspace`.
- **Metadata:**
  - A README for core, lg and cec.
  - Per-crate `keywords` and `categories` replace the inherited workspace values: `lg`/`webos` don't fit core or cec, and `command-line-utilities` fits no library.
  - No `documentation` field: crates.io links docs.rs when it's unset.
- **Docs:**
  - A "Public dependencies" section in each crate's docs: tokio, uuid, bitflags, serde, toml and serde_json for core; serde for lg and cec; futures-core for ssdp.
  - Update the crate-structure lines in `.claude/CLAUDE.md` for core, runtime and mock.

## Resolved after review

Owner's rulings, 2026-10-11:
- **Vendor-constructed structs:** `InputInfo`, `DiscoveredTv` and `PairingField` stay exhaustive. At 0.x with lockstep versions, adding a field is a minor bump release-plz already handles. `TvState` and `Credentials`, which have no outside literals, go non-exhaustive.
- **Versioning:** the published libraries keep the shared lockstep `version.workspace`.
- **Reload conflicts:** a field changed both by a pending world change and by the hand edit takes the disk value. Pressing Reload means "use my edit".

## Retirement
When the last car merges:
- the reload and store-ownership design moves to `docs/architecture.md` § Persistence;
- the public-dependency and `#[non_exhaustive]` policy moves to `docs/build.md`, next to the first-publish checklist;
- the netlink resync notes move to `docs/watchers/network.md`;
- this file is deleted, and git keeps it.

## Test plan
- `just check` after every commit, and `just package-check` from car 4(iii) on.
- New tests per car, as listed.
- Field checks on the C3/G4:
  - **Car 1:** hand-edit `daemon.toml` while the daemon runs, then use the Reload button and `systemctl reload`. Also hand-edit, make a GUI rename without reloading, and confirm `daemon.toml.edited` appears.
  - **Car 3:** an interface-churn overrun with a parked TV.

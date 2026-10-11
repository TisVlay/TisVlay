# Reload, typed wire strings, netlink restart, and pre-publish

A train of four cars, each its own PR, planned together because they share files and ordering constraints. It came out of revisiting the ideas filed by #150–#152 and preparing the five `publish = true` crates for their first crates.io release. Nothing is published yet, so every API change here is free; after the first publish, most of it would be a breaking release.

## Order

1. **Reload config + fsync.** It's user-visible: the GUI's Reload button is dead today.
2. **Typed wire strings.** This goes before car 4 because it reworks core's pairing codes, which car 4 audits.
3. **Netlink monitor restart.** It's independent and small.
4. **Pre-publish.** It lands last, since cars 1–3 touch the public crates' surface. It's split into several PRs (4a–4e).

## Decisions

Owner's rulings, 2026-10-11:

- **Reload re-reads the stores**, not the seed config. To tell a deliberate edit apart from its own write, the daemon compares the file on disk with the snapshot it last wrote (`LastPersisted`). Reload covers the daemon, session and tray, as the old button did. SIGHUP (`systemctl reload`) triggers the same path. File watching was rejected: it's uncommon for daemons, and save-by-rename, half-written saves and the daemon's own writes make it fragile.
- **Netlink monitor:** restart in place, as its own car.
- **Pre-publish scope:** the API audit, the checklist mechanics, and core error pruning. The pruning stopped being a breaking release once it became clear nothing is published.
- **`tisvlay-mock` becomes `publish = false`.** #80 published it to unblock the old `tisvlay-backends` crate (now gone) and to reserve the name. Neither reason justifies maintaining a public API for a test double.
- **App glue moves from core to `tisvlay-runtime`:** `Coalescer`, `ReconcilerMap`, `RefreshTrigger`, `TvAssignment`, `references_of`, `power_state_str_is_on`, and the profile field lists. Only the GUI and tray use them, and both already depend on `tisvlay-runtime`.
- **Dependency types in public APIs:**
  - **Wrap** `linux_cec::PhysicalAddress` in a `tisvlay-cec` newtype. linux-cec is 0.x, seven months old, and has already broken once.
  - **Leave** core's `Config(#[from] toml::de::Error)` and `Json(#[from] serde_json::Error)`, `VendorConfig`'s `toml::Value`, and `from_typed`'s `toml::ser::Error`. Both crates are 1.x, and the owner judged the opaque-error wins too small.
  - **Accept** tokio, uuid, bitflags and futures-core as public dependencies, and document them in each crate's docs.

  The grounding is the API guidelines' C-STABLE, the nightly-only status of cargo's `public = true` (RFC 3516), and how reqwest, axum and tokio-rustls handle the same question.
- **`tisvlay-lg` goes private** except the connector, backend, discovery, config and `VENDOR_NAME`. `protocol`, `WebOsClient`, the endpoint modules and the port constants go private, and `tests/protocol_fixtures.rs` moves into in-crate unit tests. Making something public later isn't a breaking change; making it private later is.

## Car 1: reload config + fsync

**State today.**
- **The button:** in the sidebar (`tisvlay-gui/ui/main.slint:263-328`). Its handler (`tisvlay-gui-app/src/lib.rs:1645-1692`) calls `reload_config()` on the daemon, session and tray SDK clients, and all three are no-ops (`tisvlay/src/lib.rs:297`, `:776`, `:977`). The button's comment and tooltip still name `config.toml`.
- **Atomic writes:** three copies of tmp+rename, in `tisvlay-daemon/src/store.rs:44`, `tisvlay-session/src/store.rs:84` and `tisvlay-tray/src/notify.rs:158`. None fsyncs, and the tray's ignores a failed temp write.
- **Unit files:** none has `ExecReload`, and nothing handles SIGHUP.

**Changes.**
- **Shared `write_atomic`** in `tisvlay-runtime`: write `<path>.tmp`, `sync_all`, rename, then fsync the directory. All three stores use it, and every failure is logged. This retires `docs/ideas/atomic-config-writes.md`.
- **Rpcs**, declared in their endpoint families. Each family's `Incoming` match then demands its arm.
  - `manager/rpc/reload`, `user/profile/rpc/reload` and `user/tray/rpc/reload`, each `(()) -> rpc::ReloadReport, write`.
  - `ReloadReport { changed: bool }`. Counts of added, removed and changed entries are an open question.
- **Daemon:**
  - The bridge arm reads the store file (`load_store`) off the reactor and sends `Command::ReloadStore { snapshot }`.
  - The one-shot handler compares the snapshot with `LastPersisted`. If they're equal, it answers `changed: false`.
  - Otherwise it diffs the snapshot against the world by id:
    - it spawns new TVs and profiles;
    - it despawns removed ones; the hooks delete their keys and release routes;
    - for changed TVs, it writes changed fields onto `Identity`/`Meta`/`VendorRef`, and a changed vendor config redials, like `update-config`;
    - for changed profiles, it replaces the body.
  - Finally it sets `LastPersisted` to the snapshot, so the reload writes nothing back.
  - A fresh enrollment still verifying stays untouched. It isn't in the store yet.
- **Session:** the `Core` task re-reads `session.toml`, replaces `profiles` and `assignments` (keeping the default profile's pinned name, as at boot), republishes, and resets its persist baseline.
- **Tray:** `serve_settings` re-reads `tray.toml` and applies it like `Update`: `set_enabled`, then publish.
- **SIGHUP:** the daemon, session and tray treat SIGHUP as a reload. The units gain `ExecReload=kill -HUP $MAINPID`, in the systemd files and the NixOS module.
- **SDK and GUI:** `reload_config()` calls the rpc and returns the report. The GUI keeps its handler, fixes the stale comment and tooltip, and keeps showing refusals in the existing banner slots.
- **Unreloaded hand edits:** a daemon write can still clobber an edit made since its last write. The store writer compares the file on disk with its own last write before writing. If they differ, it moves the edited file aside to `<path>.edited-<unix-time>` and logs a warning naming it. The edit is never silently lost. This is an open question, and the alternatives are listed below.

**Tests.**
- **World:** the reload handler applies an add, a remove, a rename, a vendor-config change (which redials) and a profile edit. An unchanged file reports `changed: false` and writes nothing. A reload never persists back.
- **Daemon:** through `serve`, a hand-edited store file plus the reload rpc yields the new state on the wire. SIGHUP takes the same path, exercised through a `run_until`-style hook rather than a real signal.
- **Session and tray:** each reload rpc round-trips a file edit.
- **`write_atomic`:** a unit test for the temp-then-rename result.

## Car 2: typed wire strings

**State today.**
- **`PairingProgress.phase`** is a `String` with four values: `awaiting_tv`, `awaiting_input`, `complete`, `failed`.
  - It's produced by `PairingState::to_dbus_state()` and by world literals.
  - It's matched by text in the SDK's `drain_pair_to_terminal`, the CLI, the tray, the daemon bridge's liveliness drop, and the GUI wizard (whose Slint side compares strings).
- **`PairingProgress.code` and `DialFailure.code`** are `Option<String>` with seven values: `pairing.{rejected,timeout,connection-failed,invalid-input,cancelled,unknown}` and `device.permission-denied`.
  - They come from core's `PairingErrorCode` and `DeviceErrorCode`, neither of which derives serde.
  - The only consumer that matches on them is the GUI's `localize_pair_failure`.
- **`Meta.automate_when`** is a `String`, written from `AutomateWhen::to_string()` every step. No non-test client reads it. The GUI duplicates the token mapping for profile bodies (`profiles.rs:655`, `:1511`).

**Changes.**
- **Phase:** a wire `PairingPhase` enum with `rename_all = "snake_case"`. `PairingState` gains a `phase()` that returns it, replacing `to_dbus_state`.
- **Codes:** a wire `FailureCode` enum with a per-variant `rename`, copying `ErrorCode`'s macro. It's `#[non_exhaustive]` with no unknown fallback. core's two code enums map into it, and `DialFailure.code` carries it.
- **`Meta.automate_when`** becomes `tisvlay_core::AutomateWhen`. The JSON stays the same. The GUI's duplicate token helpers switch to `AutomateWhen`'s `FromStr`/`Display`.
- **Consumers:** the SDK, tray and bridge decode the typed payload instead of a `serde_json::Value`. The CLI does the same. The GUI keeps strings only at the Slint boundary, converting from the enum.

**Tests.** Serde round-trips that pin each enum's JSON form to today's strings. The existing pairing and wizard tests cover the consumers.

## Car 3: netlink monitor restart

**State today.** `spawn_link_monitor` (`tisvlay-daemon/src/netlink.rs:44`) runs `watch_network` once.
- If the multicast connection fails to open, it logs one warning and stops.
- If the message stream ends, the loop exits *silently*.
- Either way, route requests go nowhere (the reactor's sends are `let _ =`), and a TV parked during a network blip never redials.
- Link state is learned only from transitions, so missed messages leave the monitor's view stale.

**Changes.**
- The `monitored` and `resolved` maps and the request receiver move out of the connection's scope.
- A supervised loop reopens the connection with capped exponential backoff and logs each failure and restart.
- After every (re)connect it dumps current link state (`RTM_GETLINK`) to resync `carrier_state`, and re-resolves every monitored TV.
- The loop exits cleanly only when the request channel closes, which means the world is gone.

**Tests.** The reconnect loop is driven through a connection-factory seam: a fake stream that ends, then reopens. The tests assert that monitored TVs are re-resolved and that `Carrier` resyncs. Real-netlink tests stay as they are.

## Car 4: pre-publish

### 4a. Prune core
- Delete `errors::power` (`PowerError`, `PowerErrorCode`) and `ConfigErrorCode`, and rewrite the `errors` module doc.
- Delete the D-Bus leftovers:
  - `InputType::{from,to}_dbus_string`;
  - `HardwareId::{to,from}_dbus_tuple`;
  - `Capabilities::to_strings`, and the D-Bus prose in `capabilities.rs`;
  - `PairingState::to_dbus_state`, already replaced in car 2.
- Delete the unused `Reconciler`, `WakeOptions` and `wake_with_options` if they're still unused. `wol::wake_with_retry` and `wake_best_effort` stay.
- Retire `docs/ideas/core-error-vocabulary-pruning.md`.

### 4b. Move app glue to `tisvlay-runtime`
The items listed under Decisions move, behind a feature if they pull dependencies the binaries don't share. The GUI and tray imports change, and core's re-exports go.

### 4c. Type-level audit of the published crates
- **`#[non_exhaustive]` on enums:** the public enums in core (`Error`, `PairingErrorCode`, `DeviceErrorCode`, `PairingState`, `PairingCommand`, `Transport`, `PairingFieldType`, `PowerCyclePhase`, `SessionAction`, `EventAction`, `AutomateWhen`), lg (`WebOsScheme` before it goes private), ssdp (`Error`) and cec.
  - Exception: enums that third-party code must match exhaustively, decided case by case.
- **`#[non_exhaustive]` on structs:** options and config structs callers read but shouldn't build by literal, such as ssdp's `SearchOptions`.
  - Structs a vendor backend must construct, like `TvState`, `InputInfo`, `DiscoveredTv`, `PairingField` and `Credentials`, need a constructor or builder if they go non-exhaustive. Vendor crates are separate crates, so a struct literal would no longer compile. This is the audit's main judgment call. See the open questions.
- **Unit structs** (`LgBackend`, `LgConnector`, `CecBackend`): keep them constructible, or add a private field plus `new()`/`Default`.
- **`tisvlay-cec`:** `PhysicalAddress` newtype with parse, display and a crate-private conversion. The test-link feature's `PowerStatus` re-export is documented as test-only.
- **`tisvlay-lg`:** make it private per Decisions, and move `protocol_fixtures` in-crate.
- **Async trait style:** core's traits mix `#[async_trait]` (`TvBackend`, `TvConnection`, `TvConnector`) with native `impl Future` (`TvDiscovery`, which therefore can't be used as `dyn`). The audit records that or unifies it. See the open questions.

### 4d. `tisvlay-mock` unpublished
`publish = false`, and `docs/build.md`'s count drops to four crates.

### 4e. Checklist mechanics (`docs/build.md` § First-publish checklist)
- **Metadata:** a README for core, lg and cec. Per-crate `keywords` and `categories` replace the inherited workspace values: `lg` and `webos` don't fit core or cec, and `command-line-utilities` fits none of the libraries. Add a `documentation` field (docs.rs).
- **Packaging:** `cargo package -p <crate> --list`, then a full `cargo package` for each of the four, fixing whatever they flag.
- **Public dependencies:** a "Public dependencies" note in each crate's docs listing which dependency versions are part of its API: tokio, uuid, bitflags, toml and serde_json in core; futures-core in ssdp.

## Open questions
- **`ReloadReport` shape:** `changed: bool`, or per-kind counts for the GUI to show ("3 TVs updated")?
- **Unreloaded hand edits:** move the edited file aside and warn (proposed), refuse the write and keep the world dirty until a reload, or warn only?
- **SIGHUP on the session and tray:** both are user units, so `systemctl --user reload` works. Is that wanted, or are rpc reloads enough there?
- **Vendor-constructed structs** (4c): non-exhaustive plus constructors, or leave them exhaustive and accept that adding a field is a breaking release?
- **Async trait style** (4c): unify on native async fn in traits (dyn-incompatible without boxing), unify on `#[async_trait]`, or leave the mix and document it?
- **Car 4's PR split:** 4a–4e as five PRs, or 4a+4b (moves and deletions) and 4c+4d+4e (audit and metadata) as two?

## Test plan
- `just check` after every commit.
- New tests per car, as listed.
- Car 4 adds a `cargo package` run for each published crate. It could also become a `just` recipe.
- Field checks on the C3/G4:
  - **Car 1:** hand-edit the store, then use the Reload button and `systemctl reload`.
  - **Car 3:** pull a cable and restart NetworkManager while the daemon runs.

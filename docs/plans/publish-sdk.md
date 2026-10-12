# Publish the client SDK

Publish the `tisvlay` client SDK (`crates/tisvlay`) to crates.io, which also reserves the bare `tisvlay` name. The SDK re-exports wire types (`payload`, `rpc`, `error::ErrorCode`), so the protocol has to publish with it. Today both are `publish = false`, and wire was designed as in-repo vocabulary: no compatibility shims, every client in the repo, and an upgrade window of one re-login. This plan audits both crates the way #156–#158 audited the first four, and settles what a published SDK promises to clients outside the repo.

Nothing is on crates.io yet: the first four crates are prepared but unpublished. So the rename costs nothing there, and this train should land before the first manual publish, so one checklist session publishes all six crates.

## State today

- **SDK surface** (`crates/tisvlay`, about 1,600 lines): `DaemonClient` (system socket), `SessionClient` and `TrayClient` (user socket), `StateWatcher` and `StateWatcherConfig`, `TvInfo`, `TvStateChange`, `StateEvent`, `PairTerminal`, `NotificationCategory`, `DesktopMapping`, a `thiserror` `Error` with `Service`, and re-exports of wire's payload and rpc types and `ErrorCode`.
  - **Dependencies:** core, wire, zenoh, zenoh-ext (`unstable` feature, for the advanced subscriber), tokio, tokio-stream, futures-util, serde, serde_json, uuid, thiserror, tracing.
  - **In-repo users:** the GUI, and the tray as a dev-dependency only. The CLI talks to wire directly.
  - **Signatures:** no zenoh, zenoh-ext or tokio-stream type appears in an SDK signature. `Error` boxes its sources. Three streams return `impl futures_util::Stream`, `update_tv_config` takes a `serde_json::Map`, and core types appear throughout.
  - **Runtime:** `connect` spawns tokio tasks, and zenoh rejects the current-thread scheduler, so the SDK needs a multi-thread runtime.
- **Wire** (`crates/tisvlay-wire`, about 2,900 lines): modules `payload`, `keys`, `rpc`, `error`, `endpoint`, `caller`, `bootstrap` and `serve`. All except `endpoint` are inline in a 2,290-line `lib.rs`.
  - **Dependencies:** core, zenoh, bevy_ecs, anyhow, serde, serde_json, tokio, tracing, uuid.
  - **`endpoint` generates both halves.** Each family's table generates the client calls and the servers' `Incoming` dispatch, built on `serve::Responder`. That one declaration is what makes the compiler demand a server arm for every new rpc, so `serve` is part of the protocol, not daemon glue.
  - **`bootstrap`:** four zenoh config builders, two for connecting and two for listening (`peer_config`, `router_config`), plus the default socket paths. All four builders share a private `base_config` that turns scouting (network discovery) off. They return `anyhow` errors. It uses `std::os::unix`, so the crate is Unix-only.
  - **Daemon-only internals in the published surface:**
    - two untyped escape hatches, `Responder::into_query` and `serve::reply_json`, used only by the daemon bridge for one reply shape;
    - `manager::discover_scan`, which is daemon policy;
    - `keys::parse_*` and `Call::decode`, which the macro-generated server code needs.
  - **zenoh in protocol signatures:**
    - `Caller::new(Session)`;
    - `Incoming::receive(site, Query)`;
    - the `serve` helpers taking a `&Query`;
    - `RpcError::Transport { source: zenoh::Error }`;
    - the config builders returning `zenoh::Config`.
- **Policy conflicts** with `docs/build.md` § Public API policy:
  - **bevy_ecs:** eight payload structs (`Power`, `Volume`, `Link`, `Input`, `Backlight`, `Identity`, `VendorRows`, `Meta`) derive bevy's `Component`. `StatePayload` requires `Component`, and the world's generic `publish_changed::<T>` queries `&T, Changed<T>`. Uses are about 60 lines across six files in `tisvlay-world/src` (`facts.rs`, `mod.rs`, `dispatch.rs`, `commands.rs`, `publish.rs`, `profiles.rs`), plus the world tests' `get::<payload::X>` calls. No hooks sit on payload components.
  - **anyhow:** `bootstrap` returns `anyhow` errors.
- **Versions:** each service answers `info` with `ServiceInfo { version, state_path }`, stamping its own crate's `CARGO_PKG_VERSION`. All four crates sit on the workspace version, so they agree today. The `info` rpcs are `manager`, `user_profile` and `user_tray`.
- **The mirror:** the SDK's mirror silently drops samples it can't decode (`.ok()?` in `mirror.rs`). Under a version mismatch, mirror reads (`list_tvs`, `get_system_profile`, `remote_session`, `StateWatcher` events, the progress and discovery streams) would quietly go stale rather than fail.
- **Packaging:** the workspace entries for `tisvlay-wire` and `tisvlay` are path-only, which blocks `cargo package` for a published dependent (`Cargo.toml`, the internal-crates comment).

## Decisions

Owner's rulings, 2026-10-11:

- **Lockstep on semver, with a handshake.** SDK 0.x.y talks to services of the same `major.minor`, the rule users already know ("SDK 0.4.x works with daemon 0.4.x"), so a distro's daemon patch and the SDK's newest patch still work together.
  - **Release discipline:** `release-plz.toml` sets `features_always_increment_minor = true`, so every `feat` bumps the minor at 0.x. A new CLAUDE.md rule says a wire-visible change (a key, payload, rpc or decode rule) is committed as `feat` or breaking, never `fix`.
  - **What else was considered:**
    - exact version matching, like tmux, which forces clients to upgrade on every patch;
    - a hand-bumped protocol number, like the Nix daemon, where a forgotten bump is a silent bug;
    - negotiation or skew windows, like Docker and Kubernetes, which need tolerant decoding.
  - **No shims:** the no-compatibility rule stands.
- **Wire becomes `tisvlay-protocol`, published whole.**
  - **Contents:** keys, payload, rpc, error, endpoint, caller, serve, and all four config builders, which return a typed error.
  - **The builders stay together,** so the scouting-off rule lives in one place.
  - **Why the earlier split fell through:** it assumed `serve` was daemon-only. Splitting it from `endpoint` would break the declare-once guarantee, and publishing it lets a third party write a compatible mock service.
- **The daemon-only internals get the idiomatic treatment.**
  - `discover_scan` moves to the daemon.
  - A typed `Responder` method covers the bridge's one untyped reply, so `into_query` and `reply_json` go crate-private.
  - The helpers the macro needs (`Call::decode`, `keys::parse_*`) move to `#[doc(hidden)] pub mod __private`, documented as outside semver, the pattern serde and tokio use.
- **The bevy derives leave via one generic wrapper.** The world defines `#[derive(Component, Deref, DerefMut, PartialEq)] struct State<T: StatePayload>(pub T)`, and `StatePayload` drops its `Component` bound. Queries read `&State<Power>`, `publish_changed::<T>` stays one generic system, and the protocol crate has no bevy dependency.
- **All three clients publish:** `DaemonClient` and `StateWatcher`, plus `SessionClient` and `TrayClient`.
- **The CLI stays unpublished.** Distro packages and the Nix flake ship it.

Settled in review:

- **Where the check lives:** the SDK enforces it, and the CLI calls the protocol's `compatible()` in `status` and before its rpcs.
  - **Not in `Caller`:** the session's lock/idle screen-off and the tray go through `Caller`, and a check there would turn the accepted re-login window into hard automation failures.
  - **Not in liveliness:** zenoh liveliness tokens carry no payload, so the version would have to go into the key. Exact-key lookups (the CLI status probe, the GUI About page) would then see a mismatched service as absent.
- **One version, one rule.** `tisvlay_protocol::VERSION` and `compatible(ours, theirs) -> bool` hold the `major.minor` rule in one place. The services fill `ServiceInfo.version` from `VERSION`. An unparsable version is a mismatch, never a panic.
- **The handshake surface is frozen.** The three `info` keys and `ServiceInfo` never change incompatibly: fields may be added, and `deny_unknown_fields` is never used. An old SDK can then always read a new service's version and report a mismatch, instead of `Unavailable` or a decode failure.
- **The check is per service lifetime.** Clients keep their links across service restarts, and the common mismatch is a package upgrade restarting the daemon under a running GUI. So the cached verdict is cleared when the service's liveliness token goes away, and re-checked on the next call or when the token reappears.
- **A mismatch reaches every path.**
  - rpcs fail with `Error::VersionMismatch { service, ours, theirs }`;
  - writes wait for the verdict before sending, since a `#[serde(default)]` field can decode wrongly with no error;
  - `list_tvs` returns that error;
  - the `Option` mirror reads return `None`;
  - `StateWatcher` emits `StateEvent::VersionMismatch`;
  - `version()` and `config_path()` skip the gate, so they can still report what's running.
- **The SDK keeps its re-exports.** Owning copies of the protocol types adds nothing under lockstep. It re-exports the default socket paths, so external callers don't need the protocol's bootstrap module.

## Cars

Ordered so the conflict hotspot (wire's `lib.rs`) is split first and the later cars touch separate files.

1. **Protocol crate,** in three commits.
   - **1a, pure rename:**
     - `git mv crates/tisvlay-wire crates/tisvlay-protocol`, so history and rename detection follow;
     - the package name, the workspace member and dependency entries, and every `tisvlay_wire::` import;
     - commit scope `protocol` from here on.
     - Exit check: `grep -rn 'tisvlay-wire\|tisvlay_wire' docs .claude crates Cargo.toml` is empty. That covers CLAUDE.md, architecture.md, `docs/testing.md` and three `docs/ideas/` files.
     - The mdBook worktree follows at its next sync.
   - **1b, split `lib.rs`:** the inline modules move to their own files, with no content change.
   - **1c, bootstrap and internals:**
     - the typed `ConfigError` for the four builders;
     - `discover_scan` to the daemon;
     - the typed `Responder` method, so the escape hatches go private;
     - the `__private` module;
     - the bootstrap module doc updated.
2. **The `State<T>` wrapper.**
   - In `tisvlay-world`: `StatePayload` drops `Component`, the world defines `State<T>`, `publish_changed` and the spawn bundle use it, and the queries move to `&State<…>`. The world tests' `get::<payload::X>` move to `get::<State<payload::X>>`.
   - The protocol crate drops `bevy_ecs`.
   - Exit check: `cargo tree -p tisvlay-protocol -e normal -i bevy_ecs` is empty, and `just shear` is clean.
   - Docs: the "payloads double as bevy components" text goes (`docs/architecture.md`, `.claude/CLAUDE.md`'s crate map, the protocol module doc).
3. **Handshake.** Runs in parallel with car 2, since it touches the protocol's `caller`, `rpc` and `error` and the SDK.
   - **Protocol:** `VERSION` and `compatible()`, with the services filling `ServiceInfo.version` from `VERSION`.
   - **SDK:** a per-service verdict cache, cleared on a liveliness DELETE; the gates on rpcs, writes, `list_tvs` and the `Option` reads; `Error::VersionMismatch`; `StateEvent::VersionMismatch`.
   - **CLI:** the mismatch check in `status` and before its rpcs.
   - **GUI:** an app-level banner, beside the access and reload banners, naming the service and both versions ("TisVlay 0.4 can't talk to the daemon (0.3); restart the daemon or upgrade"). It shows above every pane until the versions match. Owner's ruling, 2026-10-11.
   - **Release:** `features_always_increment_minor = true` in `release-plz.toml`, and the wire-change commit rule in CLAUDE.md, next to "a new rpc is declared once".
4. **API audit** of `tisvlay-protocol` and `tisvlay`, the #157 checks:
   - private by default (`unreachable_pub`, unused public items);
   - the `#[non_exhaustive]` policy:
     - errors, codes and `StateEvent` (which gains a variant in car 3) get it;
     - `TvInfo` and `TvStateChange` are SDK-built records clients read but don't construct, so they get it;
     - `StateWatcherConfig` is an options struct, so it gets it and keeps its `Default`;
     - wire payload and rpc enums stay exhaustive, since under lockstep a new variant is a minor release anyway.
   - `#![warn(missing_docs)]` on both crates.
5. **Packaging.**
   - **Workspace entries:** `tisvlay-protocol` and `tisvlay` gain `version = "0.3.0"`, with the internal-crates comment updated. Without this, packaging fails.
   - **zenoh-ext:** the SDK pins `~1.10`, because zenoh-ext's `unstable` feature switches on zenoh's internal APIs and pins zenoh exactly, so a caret range could pull in a breaking 1.x.
   - **docs.rs:** `[package.metadata.docs.rs] targets` set to Linux.
   - **READMEs as crate docs:**
     - examples doctested as `no_run` with `#[tokio::main(flavor = "multi_thread")]`;
     - a § Compatibility section (the lockstep contract);
     - § Public dependencies: zenoh and core for the protocol; core, the protocol, uuid, serde_json and futures-core for the SDK;
     - the multi-thread runtime requirement;
     - the SDK README opening with "this is the library; the `tisvlay` command ships in distro packages and the Nix flake", since `cargo install tisvlay` would find no binary.
   - **Metadata:** per-crate keywords and categories, and `LICENSE` symlinks.
   - **`just package-check`:** both crates join its single `cargo package` invocation, so the SDK builds against the packaged protocol.
   - **`docs/build.md`:**
     - the checklist's crate count and publish order (core → ssdp → lg, cec → protocol → tisvlay);
     - the public-dependency list gains zenoh, for the protocol;
     - § Review triggers gains zenoh 2.x, and any change to zenoh-ext's `unstable` API.
   - **`release-plz.toml`:** a note that per-package tags (`tisvlay-v0.4.0`) sit next to the distro tag (`v0.4.0`).
   - **First release PR:** check that the CHANGELOGs for both new crates read as an initial release, since release-plz filters by package path across the rename.

*Decided during implementation (car 1c):*
- **`discover_scan` stays in the protocol.** It isn't daemon policy: a client's query window for a discover is `discover_scan(request) + DISCOVER_MARGIN`, so both sides must agree on it. Moving it would mean duplicating its constants, which would then drift. Its doc says so.
- **`keys::parse_*` stays public.** It's client API: the SDK's mirror, the CLI and the tray parse the keys they receive on wildcard subscriptions.
- **No `__private` module was needed.** `family!` isn't exported, so its expansions live in the protocol crate. Its helpers (`serve::payload`, `serve::reply_err`, the generated `Call::decode`) went `pub(crate)`.
- **The escape hatches became `ParkedReply`.** `Responder::park()` returns a type-erased reply the daemon bridge can keep in one map across rpcs and answer later with the world's JSON. It keeps a responder's guarantees: answering consumes it, and one dropped unanswered logs a warning. `into_query` and `reply_json` are gone.

*Decided during implementation (car 3):*
- **Mirror reads respect a known mismatch but don't trigger a check.** Otherwise `list_tvs` would fail whenever a service is merely offline, where it shows an empty list today. Each client checks as soon as its service's liveliness token appears, so a mismatch is known within moments of the service starting.
- **The check is `Handshake` in the SDK:** an async mutex around the verdict, so calls wait for a check in flight. A failure to ask (the service down) isn't remembered. `version()` and `config_path()` go around it. The CLI gates every daemon command except `status` (which reports the release) and `watch` (a passive view). The user-socket commands check the session service, and `reload` checks each user service it reloads.
- **The GUI's version banner isn't dismissible.** It clears itself when the mismatched service goes away, and its next lifetime gets a fresh check.
- **The stack runs linearly:** car 3 stacks on car 2 instead of running in parallel. It's simpler to merge, and costs nothing.

*Decided during implementation (car 4):*
- **`StateEvent` stays exhaustive,** against the car's first sketch. The policy keeps the attribute off enums whose every arm matters, and the GUI's exhaustive match is what demanded the version-mismatch arm. Under lockstep, a new variant is a minor release anyway.
- **What did get `#[non_exhaustive]`:** `RpcError` (the SDK's conversion keeps an unknown failure as a `Transport` source), `TvInfo`, `TvStateChange` and `StateWatcherConfig`. `ErrorCode` already had it.
- **`PowerPhase::DirectConnect` is gone.** Nothing published it; publishing it would promise clients a phase that never comes.
- **Writing the docs found two handshake gaps, both fixed:** the session client's `version()`/`config_path()` were gated, and `get_user_profile` skipped the mismatch check.
- **Pre-existing follow-ups the doc pass found, outside this train:**
  - the `profile_*` refusal messages say "system profile" when the session returns them for user profiles;
  - the session answers `BadRequest` for an internal failure ("the store writer is gone");
  - `tv::SetSystemProfile` doesn't check that the profile exists.

**PR split** (stacked): 1a+1b, then 1c, then 2 and 3 in parallel, then 4 (on both), then 5 (last, since its doctests need the final API).

## Test plan

- `just check` after every commit, and `just package-check` with the new crates from car 5 on. `just hack` covers each feature on its own.
- New tests per car:
  - **1c:** the typed config errors.
  - **2:** the existing world suites pass unchanged, plus the bevy exit check above.
  - **3:**
    - a handshake match;
    - a mismatch against a stub service built from `serve`, for the daemon, the session and the tray;
    - the cache cleared when a service restarts at a different version;
    - the mirror gates (`list_tvs` errors, `StateEvent::VersionMismatch` emitted);
    - a write waiting for the verdict;
    - `ServiceInfo`'s frozen shape decoding;
    - the CLI's mismatch check.
  - **5:** the README doctests (`no_run`), and `just package-check` building the SDK against the packaged protocol.

## Retirement

When the last car merges:
- the compatibility contract moves to a new architecture.md § Compatibility, and each README's § Compatibility states it for external users;
- the publish list, order and crate count move to `docs/build.md` § First-publish checklist, with the public-dependency list and the review triggers updated there;
- the plans/README entry is removed;
- this file is deleted, and git keeps it.

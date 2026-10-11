# Publish the client SDK

Publish the `tisvlay` client SDK (`crates/tisvlay`) to crates.io, which also reserves the bare `tisvlay` name. The SDK re-exports wire types (`payload`, `rpc`, `error::ErrorCode`), so the protocol has to publish with it. Today both are `publish = false`, and wire was designed as in-repo vocabulary: no compatibility shims, every client in the repo, and an upgrade window of one re-login. This plan audits both crates the way #156–#158 audited the first four, and settles what a published SDK promises to clients outside the repo.

## State today

- **SDK surface** (`crates/tisvlay`, about 1,600 lines): `DaemonClient` (system socket), `SessionClient` and `TrayClient` (user socket), `StateWatcher` and `StateWatcherConfig`, `TvInfo`, `TvStateChange`, `StateEvent`, `PairTerminal`, `NotificationCategory`, `DesktopMapping`, a `thiserror` `Error` with `Service`, and re-exports of wire's payload and rpc types and `ErrorCode`.
  - **Dependencies:** core, wire, zenoh, zenoh-ext (`unstable` feature, for the advanced subscriber), tokio, tokio-stream, futures-util, serde, serde_json, uuid, thiserror, tracing.
  - **In-repo user:** the GUI. The CLI talks to wire directly.
- **Wire** (`crates/tisvlay-wire`, about 2,900 lines): modules `payload`, `keys`, `rpc`, `error`, `endpoint`, `caller`, `bootstrap` and `serve`.
  - **Dependencies:** core, zenoh, bevy_ecs, anyhow, serde, serde_json, tokio, tracing, uuid.
  - **`endpoint` generates both halves.** Each family's table generates the client calls and the servers' `Incoming` dispatch, built on `serve::Responder`. That one declaration is what makes the compiler demand a server arm for every new rpc, so `serve` is part of the protocol, not daemon glue.
  - **`bootstrap`:** client connect configs and default socket paths, used by every binary and the GUI, plus the listening configs (`peer_config`, `router_config`), used only by the daemon and session. It returns `anyhow` errors.
- **Policy conflicts** with `docs/build.md` § Public API policy:
  - **bevy_ecs:** eight payload structs (`Power`, `Volume`, `Link`, `Input`, `Backlight`, `Identity`, `VendorRows`, `Meta`) derive bevy's `Component`, because the world stores them directly.
  - **anyhow:** `bootstrap` returns `anyhow` errors.
- **Compatibility:** requests decode strictly, wire enums are exhaustive, and the project rule is no fallback paths or skew tolerance. That rule held because every client was in-repo.

## Decisions

Owner's rulings, 2026-10-11:

- **Lockstep with a handshake.** A published SDK talks only to services of its own release line, the same `major.minor` (at 0.x, a minor bump is the breaking one). Each service advertises its version; the SDK checks it and fails a mismatch with a typed error that names both versions. There are no shims or tolerant decoding: the no-compatibility rule stands, and external clients upgrade with the daemon.
- **Wire becomes `tisvlay-protocol`, published whole.**
  - **Contents:** keys, payload, rpc, error, endpoint, caller, serve, and the client connect configs.
  - **Moves out:** the listening configs (`peer_config`, `router_config`) move to `tisvlay-runtime`, where `anyhow` is allowed. The client configs return a typed error.
  - **Why the earlier split fell through:** it assumed `serve` was daemon-only. It isn't, and splitting it from `endpoint` would break the declare-once guarantee. Publishing `serve` also lets a third party write a compatible mock service for their tests.
- **The bevy derives leave via newtypes.** The world wraps each of the eight payloads in its own `Component` newtype that derefs to the payload. The protocol crate has no bevy dependency.
- **All three clients publish:** `DaemonClient` and `StateWatcher`, plus `SessionClient` and `TrayClient`.
- **The CLI stays unpublished.** Distro packages and the Nix flake ship it.

## Cars

1. **Protocol crate.** Mechanical, behavior-preserving.
   - **Rename:** `crates/tisvlay-wire` → `crates/tisvlay-protocol` (`git mv`, so history follows), and update every `tisvlay_wire::` import.
   - **Move:** `peer_config` and `router_config` go to `tisvlay-runtime` (a `zenoh` feature, or the existing service features).
   - **Typed errors:** `client_config` and `client_config_resilient` return a typed `ConfigError`.
   - **Docs:** `CLAUDE.md`'s crate map and architecture.md's references follow the rename.
2. **Bevy newtypes.** The world defines `Component` newtypes for the eight payloads (`Deref`/`DerefMut` to the payload). Its systems and `publish_changed::<…>` read through them, and the protocol crate drops `bevy_ecs`. `StatePayload::PATH` stays on the payload type, so publishing still keys off the protocol type.
3. **Handshake.**
   - **Advertising:** the services already answer `info` with `ServiceInfo { version, state_path }`.
   - **Checking:** the SDK checks the `major.minor` once per client connection, the first time the service is reachable, and caches the result. A mismatch fails every call with `Error::VersionMismatch { service, ours, theirs }`.
   - **Open detail for review:** whether the check belongs in the SDK alone or in `caller`, so the CLI gets it too. If `caller` checks, the version travels with each service's liveliness, not an extra round trip.
   - **Tests:** a match, and a mismatch against a stub service built from `serve`.
4. **API audit** of `tisvlay-protocol` and `tisvlay`, the #157 checks:
   - private by default (`unreachable_pub`, unused public items);
   - the `#[non_exhaustive]` policy: errors and codes yes; `TvInfo`, `StateEvent`, `TvStateChange` and `StateWatcherConfig` per their construction; wire enums stay exhaustive, since lockstep means a new variant is a minor release anyway;
   - no dependency types beyond the documented list. zenoh 1.x is a public dependency of the protocol crate (`caller` takes a `zenoh::Session`, and `serve` takes a `zenoh::query::Query`). Check whether any SDK signature exposes zenoh or zenoh-ext;
   - `#![warn(missing_docs)]` on both;
   - decide whether the SDK's re-exports stay re-exports or become its own types.
5. **Packaging.**
   - READMEs as crate docs, doctested, with public dependencies;
   - per-crate keywords and categories;
   - `LICENSE` symlinks;
   - both crates added to `just package-check`;
   - publish order core → ssdp → lg, cec → protocol → tisvlay;
   - `docs/build.md`'s checklist and crate count;
   - release-plz picks them up.

## Test plan

- `just check` after every commit, and `just package-check` with the new crates from car 5 on.
- New tests per car:
  - car 1: the typed client-config errors;
  - car 2: the existing world suites, unchanged in behavior;
  - car 3: a handshake match and mismatch;
  - car 5: the README doctests.
- `just hack` for each feature on its own, since `tisvlay-runtime` gains the listening configs.

## Retirement

When the last car merges:
- the compatibility contract moves to architecture.md § Error model, next to the wire's typed-error model;
- the publish list and order move to `docs/build.md` § First-publish checklist;
- this file is deleted, and git keeps it.

# Publish the client SDK

Publish the `tisvlay` client SDK (`crates/tisvlay`) to crates.io, which also reserves the bare `tisvlay` name. The SDK re-exports wire types (`payload`, `rpc`, `error::ErrorCode`), so `tisvlay-wire` has to publish with it. Today both are `publish = false`, and wire was designed as in-repo vocabulary: no compatibility shims, every client in the repo, and an upgrade window of one re-login. This plan audits both crates the way #156–#158 audited the first four, and settles what a published SDK promises to clients outside the repo.

## State today

- **SDK surface** (`crates/tisvlay`, about 1,600 lines): `DaemonClient` (system socket), `SessionClient` and `TrayClient` (user socket), `StateWatcher` and `StateWatcherConfig`, `TvInfo`, `TvStateChange`, `StateEvent`, `PairTerminal`, `NotificationCategory`, `DesktopMapping`, a `thiserror` `Error` with `Service`, and re-exports of wire's payload and rpc types and `ErrorCode`.
  - **Dependencies:** core, wire, zenoh, zenoh-ext (`unstable` feature, for the advanced subscriber), tokio, tokio-stream, futures-util, serde, serde_json, uuid, thiserror, tracing.
  - **In-repo user:** the GUI. The CLI talks to wire directly.
- **Wire** (`crates/tisvlay-wire`, about 2,900 lines): modules `payload`, `keys`, `rpc`, `error`, `endpoint`, `caller`, `bootstrap` and `serve`.
  - **Dependencies:** core, zenoh, bevy_ecs, anyhow, serde, serde_json, tokio, tracing, uuid.
  - **Who uses what:**
    - `serve` is used only by the daemon.
    - `bootstrap` is used by every binary and by the GUI.
    - `caller` is used by the SDK, CLI, daemon, session and tray.
    - `payload`, `rpc`, `error` and `endpoint` are used across the services and the SDK.
- **Policy conflicts** with `docs/build.md` § Public API policy:
  - **bevy_ecs:** eight payload structs (`Power`, `Volume`, `Link`, `Input`, `Backlight`, `Identity`, `VendorRows`, `Meta`) derive bevy's `Component`, because the world stores them directly. bevy_ecs would become a public dependency, and it is 0.x with breaking releases every few months.
  - **anyhow:** `bootstrap` returns `anyhow` errors, which the policy keeps out of published APIs.
  - **Server glue:** `serve` is daemon-only glue that would ship to every SDK user.
- **Compatibility:** requests decode strictly, wire enums are exhaustive, and the project rule is no fallback paths or skew tolerance. That rule holds only while every client is in-repo; a published SDK puts clients outside it.

## Open questions

1. **What does a published SDK promise about daemon versions?**
   - **(a) Lockstep with a handshake.** SDK x.y talks only to a daemon x.y. The daemon advertises its protocol version, for example on the liveliness token or in `info`, and the SDK refuses a mismatch with a typed error. There are no shims, which keeps the current rule.
   - **(b) A semver protocol.** Compatible within a minor version, which takes tolerant decoding and versioned keys.
   - (a) looks right for 0.x.
2. **What shape should the wire boundary take?**
   - **(a) Feature-gate wire in place:** `serve` and the bevy derives behind features the daemon and world enable, and typed errors in `bootstrap`.
   - **(b) Split out a published protocol crate** (for example `tisvlay-protocol`, holding keys, payload, rpc, error, endpoint, caller and the client half of bootstrap), and keep `tisvlay-wire` unpublished for `serve` and the bevy glue.
3. **How do the bevy derives get out of the published API?** A `bevy` feature on the protocol crate, or newtype components in the world wrapping the payload structs.
4. **Which clients publish?** All three, or `DaemonClient` alone, with the session and tray clients kept in-repo because their user-socket protocol is younger.
5. **zenoh as a public dependency:** zenoh 1.x is stable semver. Does any SDK signature expose zenoh or zenoh-ext types? Audit item: the `StateWatcherConfig` fields and the advanced-subscriber setup.
6. **The CLI:** also publish `tisvlay-cli` so `cargo install tisvlay-cli` works, or leave the CLI to distro packages and the Nix flake.

## Proposed cars

These are provisional; the answers above reshape them.

1. **Wire boundary.**
   - Gate or split per Q2 and Q3.
   - Make `bootstrap`'s client half return typed errors, and move the anyhow-returning server half to `tisvlay-runtime`.
   - Run `unreachable_pub` and the private-by-default pass over what's left.
2. **SDK audit.**
   - Apply the same checks as #157: private by default, the `#[non_exhaustive]` policy (`TvInfo`, `StateEvent`, `TvStateChange` and the config structs), no dependency types beyond the documented list, and `#![warn(missing_docs)]`.
   - Decide what the re-exported wire types promise.
3. **Compatibility contract** per Q1:
   - the version advertisement and the SDK-side check with its typed error;
   - tests for match and mismatch;
   - architecture.md § Error model and the "no shims" rule restated for a published SDK.
4. **Packaging.**
   - READMEs as crate docs, public dependencies and keywords;
   - wire and the SDK added to `just package-check`;
   - publish order core → ssdp → lg, cec → wire (or protocol) → SDK;
   - `docs/build.md`'s checklist and crate count;
   - release-plz picking them up.

## Test plan

- `just check` after every commit, and `just package-check` with the new crates from car 4 on.
- New tests per car: the handshake (match and mismatch), the gated builds (the protocol crate without the `bevy` and `server` features builds and tests), and the README doctests.
- `cargo hack --each-feature` covers the new features (`just hack`).

## Retirement

When the last car merges:
- the compatibility contract moves to architecture.md § Error model, next to the wire's typed-error model;
- the publish list and order move to `docs/build.md` § First-publish checklist;
- this file is deleted, and git keeps it.

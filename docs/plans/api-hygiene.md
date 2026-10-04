# API hygiene: typed RPC endpoints, typed errors, secret types

Status: drafted 2026-10-04. Two review rounds are folded in, and every open question is ruled. **A0, A and B1 are implemented** (typed rpc endpoints; typed errors and the SDK API; `Secret`), and their durable content has moved out: the endpoint model, the "why no framework" decision and the error model to `docs/architecture.md` (§ Typed rpc endpoints, § Error model), the conventions to `CLAUDE.md`, the upgrade note to `packaging/README.md`, the secrecy 0.8 duplicate's reason to `packaging/licensing/deny.toml`, and the deferrals to `docs/ideas/`. B2 is pending.

Three public-API cleanups, landing as four PRs in a fixed order:

- **A0, typed endpoints (done):** each of the 46 rpcs is declared once (its key, request type and response type), and clients and servers derive from that declaration.
- **A, typed errors and the SDK API (done):** the rpc-reply codes become an enum, and the client SDK (`tisvlay`) gets typed errors, parameters and returns in one pass.
- **B1, `Secret` (done):** the LG client key becomes a type whose `Debug` can't leak it.
- **B2, `VendorConfig`:** the untyped vendor config becomes a type whose `Debug` prints key names only, threaded through the connector trait.

The live debug-log key leak has its own hotfix PR, #149 (open).

## 1. Rulings

1. **The SDK's error API is publish-grade, but the SDK stays unpublished.** No dependency's error type is named in a signature.
2. **The SDK gets one crate-wide `#[non_exhaustive]` thiserror enum,** with variants named for what the caller can act on.
3. **The rpc-reply code vocabulary is one enum in `tisvlay-wire`,** shared by servers and clients.
4. **Secrets use `secrecy` 0.10,** accepting a reasoned duplicate: zenoh pins 0.8 in its public config API.
5. **The untyped vendor config's `Debug` prints key names only.**
6. **B splits into B1 and B2, and the leak gets a hotfix first** (#149).
7. **Typed RPC endpoints go first, as A0,** on our own zenoh layer, not a framework (`docs/architecture.md` § Typed rpc endpoints).
8. **No compatibility code, on the wire included.** Every client is in this repo and ships together, and the shared Rust types are the contract.
   - Requests are strict: no lenient decoders, no null-or-object bodies, no aliases for old senders.
   - There's no unknown-code fallback.
   - The upgrade window (the daemon restarts on the new version while user units keep the old binaries) is accepted: the packaging docs say to log out and back in after upgrading.
9. **Nothing pins the wire's JSON shape.** There are no golden tests, and the daemon's raw-JSON integration tests move to typed calls. The shape may be cleaned up where it's awkward.
10. **World-answered rpcs use typed `Reply<R>` tokens,** so a mismatched reply doesn't compile.
11. **One plan, in a fixed order:** A0, A, B1, B2. Each PR still retires its own sections when it merges.

## 2. Research (verified dates)

**secrecy:**

- **0.10.0 (2024-09)** redesigned the crate around `SecretBox` and `SecretString`, and everything redacts. The last release is 0.10.3, from 2024-10.
- **0.10 withholds `Serialize`** unless the inner type opts in.
- **zenoh stays on 0.8:** [#1940](https://github.com/eclipse-zenoh/zenoh/pull/1940) deferred the bump, and `zenoh-config` exposes `SecretValue` publicly.
- **zeroize 1.x is shared.**

## 3. Current state

The rpc layer is typed since A0 (`docs/architecture.md` § Typed rpc endpoints), and its errors since A (§ Error model). Since B1, the client key is a `tisvlay_core::Secret` wherever it's typed (`Credentials`, the LG config, payloads and client). What follows is the state B2 changes.

**The client key** still travels untyped in:

- the untyped vendor config: a `toml::Value` that's always a table, held by about a dozen world and daemon types that derive `Debug`, and passed through the published `TvConnector` trait;
- the wire's request types (`rpc::AddTv.config`, and the update-config patch), which can carry a key from client to daemon;
- the plaintext store, by design.

Replies never carry the key, because `get-config` strips schema-`Secret` fields. Core's `TvConfig`/`SystemConfig` are dead code.

## 4. PR B2: `VendorConfig` (core, trait, world, daemon, wire requests)

- **A newtype over `toml::Table`:** serde-transparent, `Clone`, `PartialEq`, a key-names-only `Debug`, `parse::<T>()`, `get`/`get_mut`/`insert`, and no `Deref`.
- **It spreads through the code:**
  - `TvConnector` takes `&VendorConfig`, and `build_config_from_pairing` returns one.
  - Every world and daemon holder carries it, so a missed holder is a compile error.
  - The update-config merge and the MAC fill use its API.
  - It enters at the `json_to_toml` seam.
- **The wire's JSON request types holding vendor config** (`AddTv.config`, `UpdateConfig`) get the same key-names-only `Debug`, through one JSON-side helper.
- **`TvConfig`/`SystemConfig` are deleted** (dead code).
- **A store test:** load and re-save a `TvRecord` holding an LG table with `client_key`, asserting the key survives.
- **Docs:**
  - `CLAUDE.md`'s secrets convention adds that untyped vendor config is `VendorConfig`, and the "never log a `TvConfig`/`TvRecord` whole" caveat goes.
  - This plan retires: §2's secrecy notes go to `CLAUDE.md`'s rationale, the file is deleted, the "Active plans" entry is removed, and inbound references are swept.

## 5. Verification

- **`just check` on every commit and PR.**
- **Every new gate is proven red:** `{:?}` of a key-holding `Secret`, `VendorConfig` or request doesn't show the key.

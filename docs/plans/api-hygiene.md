# API hygiene: typed RPC endpoints, typed errors, secret types

Status: drafted 2026-10-04. Two review rounds are folded in, and every open question is ruled. **A0 is implemented** (typed rpc endpoints), and its durable content has moved out: the endpoint model and the "why no framework" decision to `docs/architecture.md` § Typed rpc endpoints, the convention to `CLAUDE.md`, the upgrade note to `packaging/README.md`, and its deferrals to `docs/ideas/`. A, B1 and B2 are pending.

Three public-API cleanups, landing as four PRs in a fixed order:

- **A0, typed endpoints (done):** each of the 46 rpcs is declared once (its key, request type and response type), and clients and servers derive from that declaration.
- **A, typed errors and the SDK API:** the rpc-reply codes become an enum, and the client SDK (`tisvlay`) gets typed errors, parameters and returns in one pass.
- **B1, `Secret`:** the LG client key becomes a type whose `Debug` can't leak it.
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

**Error design, 2025–26:**

- **Libraries expose concrete error types and never `anyhow`,** with dependency errors as boxed sources ([d34dl0ck, May 2025](https://d34dl0ck.me/rust-bites-designing-error-types-in-rust-libraries/index.html)).
- **Enum and opaque-struct shapes are both current** ([iroh, Aug 2025](https://www.iroh.computer/blog/error-handling-in-iroh); std::io and reqwest).
- **thiserror suffices** for IPC outcomes.

**secrecy:**

- **0.10.0 (2024-09)** redesigned the crate around `SecretBox` and `SecretString`, and everything redacts. The last release is 0.10.3, from 2024-10.
- **0.10 withholds `Serialize`** unless the inner type opts in.
- **zenoh stays on 0.8:** [#1940](https://github.com/eclipse-zenoh/zenoh/pull/1940) deferred the bump, and `zenoh-config` exposes `SecretValue` publicly.
- **zeroize 1.x is shared.**

## 3. Current state

The rpc layer is typed since A0 (`docs/architecture.md` § Typed rpc endpoints): every client calls `Caller::call::<R>()`, and every server dispatches from its family's `Call` enum. What follows is the state A and B change.

**SDK (`crates/tisvlay`):** 65 methods return `anyhow::Result`.

- **Error sites:** about 44.
- **Untyped parameters:** TV and profile ids as `&str`, configs and patches as JSON strings.
- **Untyped returns:** `re_pair -> String`, `get_tv_config -> String`, `get_effective_system_profile -> Value`.
- **Notification settings:** `NotificationSettingsBody` is the Option-field patch shape, and the GUI uses it for both get and update.
- **Client-side lookups:** three scan a fetched list (vendor, system profile, user profile).
- **The GUI is its only shipped caller;** the tray uses it in tests only.

**Codes and outcomes:**

- `WireError { code: String, message }`: 17 server codes, all kebab-case, all built by constructors.
- `RpcError` is already the call-outcome enum (A0): `Service(WireError)` plus the caller's own outcomes.
- **The GUI branches on two codes** through an anyhow-chain downcast in `edit_pane` that matches `RpcError::Service` and compares the string code. The `invalid-field` message format `"<field>: <reason>"` is load-bearing.
- **Out of scope:** pairing-progress and dial-failure codes (`pairing.*`, `device.*`).

**The client key** lives in:

- core's `Credentials.data` (its serde derives are vestigial);
- lg's `LgVendorConfig`, `RegisterPayload`/`RegisteredPayload` and `WebOsClient` (about 59 sites);
- the untyped vendor config: a `toml::Value` that's always a table, held by about a dozen world and daemon types that derive `Debug`, and passed through the published `TvConnector` trait;
- the wire's request types (`rpc::AddTv.config`, and the update-config patch), which can carry a key from client to daemon;
- the plaintext store, by design.

Replies never carry the key, because `get-config` strips schema-`Secret` fields. Core's `TvConfig`/`SystemConfig` are dead code.

## 4. PR A: typed errors and the SDK's public API

### A.1 `ErrorCode`

- **One `macro_rules!` table** generates the enum (with a serde `rename` per variant, all kebab-case), `as_str` and `ALL`, covering the 17 server codes. Adding a code is one line.
- **No unknown-code fallback** (ruling 8). An unknown code fails decoding into `RpcError::Undecodable`.
- **`WireError.code: ErrorCode`** (every code already comes from a constructor, since A0).
  - `WireError::as_invalid_field() -> Option<(&str, &str)>` sits next to the `invalid_field` constructor, so the message format lives in one crate.
- **Test:** `ALL` round-trips through serde and `as_str`.

### A.2 `tisvlay::Error`

```rust
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The service refused the request.
    #[error("{message} ({code})")]
    Rpc { code: ErrorCode, message: String },
    /// The service isn't running, or isn't reachable yet.
    #[error("{service} is unavailable")]
    Unavailable { service: Service },
    /// No answer (timed out or dropped). For a write, the outcome is unknown.
    #[error("{service} didn't answer")]
    Timeout { service: Service },
    /// The local link: session open, subscriptions, a stream that ended early.
    #[error("{what}")]
    Transport { what: &'static str, #[source] source: Option<BoxError> },
    /// A reply didn't decode as its rpc's response type.
    #[error("malformed reply from {service}")]
    Protocol { service: Service },
}
#[non_exhaustive] pub enum Service { Daemon, Session, Tray }
```

- **Mapping:** each client's `call` helper maps `RpcError`, filling in its `Service`. It's `pub(crate)`, never a public `From`.
  - `Unavailable` → `Unavailable`.
  - `Timeout` and `NoReply` → `Timeout`.
  - `Transport` → `Transport`.
  - `Undecodable` → `Protocol`.
  - `Service` → `Rpc`.
- **Helpers:** `rpc_code()` and `invalid_field()`.
- **What's gone:**
  - `InvalidInput`: parameters are typed (A.3).
  - `NotFound`: the three fetched-list lookups return `Option`.
- **Re-exports:** the SDK re-exports `ErrorCode`, not `RpcError`.
- **Anyhow at the edge:** wire's `bootstrap` keeps `anyhow` (binary glue), boxed into `Transport`.
- **Rendering:** causes live in `source()`, and the GUI renders the chain. A test pins that a connect failure names its cause.
- **Enum-level `#[non_exhaustive]` only,** so the GUI's tests can construct variants.

### A.3 The SDK's public API, retyped once

- **Parameters:**
  - TV and profile ids become `Uuid`.
  - Configs and patches become `serde_json::Map<String, Value>` (or the typed wire structs) instead of JSON strings.
- **Returns:**
  - Typed throughout: `re_pair` returns the attempt, `get_tv_config` returns `TvConfigView`, `get_effective_system_profile` returns `SystemProfileConfig`.
  - The SDK returns the wire's `PairStarted`, replacing its own.
- **Notification settings:**
  - `notification_settings()` returns the full `NotificationSettings`.
  - Updates take `NotificationSettingsPatch`.
  - The GUI's `NotificationCategory` helpers adapt.
- **Every SDK signature, and the GUI's call sites, change once,** here.

### A.4 GUI

- **The anyhow-chain downcast goes:** `find_rpc_error` is deleted.
  - `extract_invalid_vendor_config` and `is_not_pairing` take `&tisvlay::Error`.
  - `PairError` gains a `Local(String)` variant for its gettext messages and for parse failures of the Slint string ids that now become `Uuid`s at the GUI edge.
  - Their tests construct `tisvlay::Error`.
- **Name clash:** `compat.rs`'s glob re-exports of the SDK and core clash on `Error`/`Result`, so they become explicit.
- **Stale D-Bus-era comments on the touched paths** are rewritten.

### A.5 Docs (retired when A merges)

- **`CLAUDE.md` § Code conventions:** "published crates and the client SDK expose thiserror types; binaries and binary-glue libraries (runtime, the service crates' library halves, wire's bootstrap) may use `anyhow`."
- **`docs/architecture.md`:** the error model.
- **`docs/ideas/`:**
  - typed pairing-progress and dial-failure codes, folded into [typed-failed-reason-payload.md](../ideas/typed-failed-reason-payload.md);
  - deleting core's unused `PowerError`/`PowerErrorCode`/`ConfigErrorCode`;
  - the SDK's stringly pairing `phase` matching.

## 5. PR B1: `Secret` (core and lg)

- **Dependency:** `secrecy` 0.10 as a workspace dependency (`serde` feature), plus an exact skip in `packaging/licensing/deny.toml`: `{ crate = "secrecy@0.8.0", reason = "zenoh-config pins secrecy 0.8 (public SecretValue); the workspace is on 0.10" }`. `-D unnecessary-skip` retires it on its own.
- **`tisvlay_core::Secret`:**
  - a newtype over `secrecy::SecretString`, with transparent `Deserialize`, a hand-written `Serialize` through `expose_secret()`, `Clone`, no `PartialEq`, and `Debug` delegated (`[REDACTED]`);
  - one type for every typed secret, so secrecy stays out of the public signatures.
  - Zeroize on drop is incidental: the same key lives in the vendor config and the store.
- **Adopted by:**
  - `Credentials.data`, with serde dropped from `Credentials`, `PairingState` and `PairingError`. `Credentials`' `Debug` stays hand-written, with `extra` values plain.
  - `LgVendorConfig.client_key`, which loses `PartialEq, Eq` (one test adapts).
  - `RegisterPayload`, `RegisteredPayload` and `WebOsClient`'s API (published lg; free while unpublished).
- **Redaction tests** assert `[REDACTED]`.
- **Docs:** `CLAUDE.md`'s secrets convention says typed secrets are `tisvlay_core::Secret`.

## 6. PR B2: `VendorConfig` (core, trait, world, daemon, wire requests)

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

## 7. Verification

- **`just check` on every commit and PR.**
- **Every new gate is proven red:**
  - a typo'd error code doesn't compile;
  - `{:?}` of a key-holding `Secret`, `VendorConfig` or request doesn't show the key.

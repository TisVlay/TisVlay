# API hygiene: typed SDK errors and secret types

Status: drafted 2026-10-04, revised after three reviews (design, implementation correctness, maintainability), open questions ruled. Nothing is implemented yet.

Two public-API cleanups the code-quality pass deferred:

- **PR A, typed errors:** the client SDK (`tisvlay`) stops returning `anyhow::Result`. The wire's rpc-reply codes become one enum, and the wire `Caller`'s failures become an enum that wraps them.
- **PR B, secret types:** the LG client key becomes a type whose `Debug` can't leak it, and the untyped vendor config becomes a type that prints key names only. This closes a live leak (§3).

Three PRs, in order: A, then B1 (`Secret`) and B2 (`VendorConfig`). A carries this plan. The live leak (§3) gets a standalone hotfix first.

## 1. Rulings

Decided with the owner before drafting:

1. **The SDK's error API is publish-grade, but the SDK stays unpublished.** No dependency's error type is named in a signature, and the code is semver-minded. Publishing would also mean publishing `tisvlay-wire`; that's out of scope.
2. **One crate-wide `#[non_exhaustive]` thiserror enum,** with variants named for what the caller can act on.
3. **The rpc-reply code vocabulary is one enum in `tisvlay-wire`,** shared by the servers that send codes and the clients that match on them. The bytes on the wire don't change.
4. **Secrets use `secrecy` 0.10,** the current major, accepting a reasoned duplicate: zenoh pins 0.8 in its public config API.
5. **The untyped vendor config's `Debug` prints key names only** (confirmed by review). `Debug` can't consult the vendor schema, and guessing secret names (`*key*`, `*token*`) fails open.

## 2. Research (verified dates)

**Error design, 2025–26:**

- **Libraries expose concrete error types and never `anyhow`.** A dependency's error goes in as a boxed `source`, not as a named type ([d34dl0ck, May 2025](https://d34dl0ck.me/rust-bites-designing-error-types-in-rust-libraries/index.html); [Palmieri](https://www.lpalmieri.com/posts/error-handling-rust/)).
- **Two shapes are current:** a non_exhaustive enum with data (AWS's SDK; [iroh's snafu enums, Aug 2025](https://www.iroh.computer/blog/error-handling-in-iroh)), or an opaque struct plus `ErrorKind` (std::io, reqwest). We take the enum.
- **"Design errors, don't forward them":** name what the caller can do (iroh; Tison's `exn`).
- **iroh chose snafu for backtraces,** since std's generic member access is still unstable ([#99301](https://github.com/rust-lang/rust/issues/99301)). Our failures are IPC outcomes, so thiserror is enough.

**secrecy:**

- **0.8.0 (2021-07)** has the generic `Secret<T>` and opt-in `DebugSecret`.
- **0.10.0 (2024-09)** is a redesign: `SecretBox<T>`, `SecretString = SecretBox<str>`, everything redacts. 0.10.1–0.10.3 (through 2024-10) added `Clone` and `Deserialize` for `SecretString`. No release since.
- **0.10 withholds `Serialize`** unless the inner type implements `SerializableSecret`, which the orphan rule rules out for `str`/`String`.
- **zenoh stays on 0.8 until its own breaking release:** main pins `secrecy = "0.8.0"`. [eclipse-zenoh/zenoh#1940](https://github.com/eclipse-zenoh/zenoh/pull/1940) (May 2025, closed unmerged Oct 2025) deferred it as "big API change", and `zenoh-config` exposes `SecretValue` publicly.
- **zeroize 1.x is shared by both secrecy majors,** so the only duplicate is secrecy itself.

## 3. Current state

**SDK (`crates/tisvlay`):**

- **Signatures:** 65 public methods return `anyhow::Result` (10 more return `bool`, `Option` or `Self`).
- **Error sites:** 33 `anyhow!`/`bail!`/`context` sites, plus:
  - six `map_err(anyhow::Error::new)` (the three clients' `call`/`fetch` helpers);
  - three implicit `?` from wire's `bootstrap::client_config_resilient`, which returns `anyhow::Result`;
  - two implicit `?` from `serde_json::to_value` on request bodies.
- **Streams are already infallible:** item types carry no errors, and undecodable samples are skipped. Only the `*_stream()` constructors and `drain_pair_to_terminal` (its "stream ended before a terminal phase" `bail!`) return errors.

**The GUI is the only shipped SDK caller.** The tray uses the SDK only in tests; in production the tray and the CLI use wire's `Caller` directly.

**Rpc-reply codes:** `WireError { code: String, message }`.

- 17 server codes: 14 constructors, plus `daemon-shutting-down`, `discovery-not-found` and `unknown-rpc`, which the daemon's bridge builds as inline literals. (The bridge's `bad-request` duplicates the existing constructor.)
- 58 `WireError::` call sites: world 37, session 16, daemon 4, tray 1.
- About 22 tests assert on codes as raw JSON. 9 of the 17 codes have no such assertion.

**Caller outcomes:** `RpcError { code: String, message }` has six caller-minted codes:

- `rpc.transport`
- `rpc.unavailable`
- `rpc.no-reply` (`fetch` retries on it)
- `rpc.timeout`
- `rpc.encode` (unreachable: bodies are `serde_json::Value`)
- `unknown` (an error payload that doesn't decode)

They never cross the wire, and `RpcError` isn't `Serialize`.

**Who branches on a code:**

- **The GUI's `edit_pane::find_rpc_error`,** which walks an `anyhow` chain with `downcast_ref::<RpcError>()`. It's reached from three sites:
  - `update_tv_config` (`invalid-field`);
  - the pairing wizard (`invalid-field`, through `PairError::Rpc(anyhow::Error)`, which also carries GUI-made gettext messages);
  - `cancel_pair` (`pairing-not-found`).

  The `invalid-field` message shape `"<field>: <reason>"` is load-bearing; the GUI splits it.
- **The SDK's integration test,** which downcasts to `RpcError`.

**A second code vocabulary already crosses the wire** and is out of this plan's scope: pairing-progress and dial-failure codes. These are core's `PairingErrorCode` (`pairing.*`) and `DeviceErrorCode` (`device.*`), carried as `Option<String>`. The GUI compares one as a string (`wizard.rs`, `device.permission-denied`), and the world builds one literal by hand (`pairing.connection-failed`).

**The client key** lives in:

- **core's `Credentials.data`** (hand-written redacting `Debug`). It derives serde, but nothing serializes it, `PairingState` or `PairingError`; core's own docs say so (`pairing.rs`).
- **lg's types:**
  - `LgVendorConfig.client_key` (hand-written `Debug`; derives `PartialEq, Eq`, compared in one test);
  - `RegisterPayload` / `RegisteredPayload` (hand-written `Debug`; serialized to and from the TV);
  - `WebOsClient`, whose published API takes and returns the key as a plain string (`connect`, `connect_with_scheme`, `client_key()`).

  That's about 59 non-comment `client_key` sites in lg.
- **the untyped vendor config,** a `toml::Value` that is always a table. Every one of these holders derives `Debug`:
  - world: `TvRecord.config`, `VendorRef.config`, `PairingRef.params` (the existing config on re-pair), `Command::Pair.config`, `Command::UpdateConfig.patch`, `PairingUpdate::ConfigReady`, and `Effect::{SpawnConnect, StartPairing, CompletePairing, Facts}`;
  - daemon: `SeedTv.config`.

  It flows through the published core's `TvConnector` trait as `&toml::Value` (about ten methods, four implementations: lg, cec, mock, the world's test stub). It enters at the daemon's JSON-to-TOML seam (`json_to_toml`).
- **the TOML store on disk,** in plaintext. That's by design and out of scope.

**A live leak:** `world/src/systems.rs` (`apply_pairing_updates`) logs `?update` at debug level when a pairing update arrives for a session that's gone. A `ConfigReady` arriving after a cancel puts the client key in the journal.

**Dead code:** core's `TvConfig`/`SystemConfig` have no users outside core. The store is the world's `TvRecord`/`StoreSnapshot`.

The key never crosses the wire: the reactor strips `FieldVisibility::Secret` fields from `get-config` replies.

## 4. PR A: typed errors

One PR in two green commits: first the wire, then the SDK and GUI.

### A.1 Wire: `ErrorCode` (rpc-reply codes)

- **One `macro_rules!` table** (`TvNotFound => "tv-not-found", …`) generates the enum, `as_str`, `FromStr` and `const ALL: &[Self]`. Adding a code is then one line, and the two directions can't drift. This follows core's existing `*ErrorCode::as_str` pattern.
- **The 17 server codes only.** Caller outcomes are not codes (A.2).
- **The wire format is unchanged:**
  - `Serialize` by hand, as `as_str`.
  - `Deserialize` by hand: `visit_str` parses known strings and falls back to `Unknown`. Parsing is infallible: a fallible one would fail `WireError` decoding and lose the code.
- **`Unknown(UnknownCode)` has a private field,** built only by parsing a string that matches no known code. It's displayable, never constructible by servers.
  - This keeps the compile gate: `WireError` with a typo'd code doesn't build.
  - It keeps equality canonical: there's no `Other("tv-not-found")` that differs from `TvNotFound`.
  - It covers a real skew window, not a migration shim: the package postinstall restarts the daemon, but live user units (session, tray, GUI) keep the old binaries until re-login.
- **`WireError.code: ErrorCode`.**
  - The three inline literals get constructors, and the bridge's duplicate `bad_request` is deleted.
  - `WireError::invalid_field`'s format gets its parser next to it: `as_invalid_field() -> Option<(&str, &str)>`.
- **Tests:**
  - Round-trip every entry of `ALL` against literal strings copied from today's constructors, not strings derived from the new enum, since those strings are the contract.
  - No known string parses to `Unknown`, and an unknown one survives decode and display.
  - The existing raw-JSON code assertions stay as they are.

### A.2 Wire: `RpcError` becomes the call's outcome

```rust
pub enum RpcError {                 // exhaustive: wire is unpublished, so the SDK's mapping is compile-checked
    Service(WireError),             // the service answered with a typed refusal
    Unavailable { key: String },    // no queryable matched within the deadline
    Timeout { key: String },        // matched, but no reply in time
    NoReply { key: String },        // fetch retries this; outcome unknown
    Transport { key: String, source: zenoh::Error },
    Undecodable { key: String },    // an error payload that isn't a WireError
}
```

- **Merging with `WireError` was rejected:** `WireError` is a server's payload, `RpcError` is a call's outcome, which may never have reached a server.
- **`rpc.encode` disappears.** Encoding a `serde_json::Value` can't fail, so it becomes an `expect` stating that invariant.
- **`Caller::fetch` retries on `RpcError::NoReply`.**
- **`Display` matches today's CLI output closely.** The CLI prints `RpcError` through `Display` instead of formatting `code: message` itself, so caller-side lines change slightly.
- **The tray's `Caller` uses follow along.**

### A.3 SDK: `tisvlay::Error`

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
    /// No answer in time. For a write, the outcome is unknown.
    #[error("{service} didn't answer in time")]
    Timeout { service: Service },
    /// The local link: session open, subscriptions, a stream that ended early.
    #[error("{what}")]
    Transport { what: &'static str, #[source] source: Option<BoxError> },
    /// The caller passed something malformed (a TV id, JSON).
    #[error("invalid {what}")]
    InvalidInput { what: &'static str, #[source] source: Option<BoxError> },
    /// Something the caller named doesn't exist.
    #[error(transparent)]
    NotFound(Missing),
    /// A reply didn't have the expected shape.
    #[error("malformed reply: {what}")]
    Protocol { what: &'static str, #[source] source: Option<BoxError> },
}
#[non_exhaustive] pub enum Service { Daemon, Session, Tray }
#[non_exhaustive] pub enum Missing { Vendor(String), SystemProfile(Uuid), UserProfile(Uuid) }
pub type Result<T> = std::result::Result<T, Error>;
```

- **Static labels:** `&'static str`, not `String`, so nobody reaches for `format!`. The data a caller can act on is typed (`Service`, `Missing`).
- **Mapping:** each client's private `call`/`fetch` helper maps `RpcError`, filling in its own `Service`. The mapping is a `pub(crate)` function, never a public `From`.
  - `Unavailable` → `Unavailable`.
  - `Timeout` and `NoReply` → `Timeout`.
  - `Transport` → `Transport`.
  - `Undecodable` → `Protocol`.
  - `Service(e)` → `Rpc`.
- **Helpers:** `rpc_code() -> Option<&ErrorCode>` and `invalid_field() -> Option<(&str, &str)>`, so both GUI branches are one-liners.
- **wire's `bootstrap` keeps `anyhow`** (binary glue). The SDK boxes it into `Transport`'s source.
- **Re-exports:** the SDK stops re-exporting `RpcError` and re-exports `ErrorCode` instead.
- **Rendering:** causes live in `source()`, never repeated in `Display`. The GUI's user-facing error text renders the chain (anyhow's `{:#}`, or a small helper), so a connect failure still names its cause. A test pins that.
- **`#[non_exhaustive]` stays enum-level only,** so the GUI's tests can construct variants. Adding a field to a variant later is breaking, which is acceptable while the SDK is unpublished.

### A.4 GUI

- **`find_rpc_error` and its downcast are deleted.**
  - `extract_invalid_vendor_config` and `is_not_pairing` take `&tisvlay::Error`.
  - `PairError` becomes `{ Rpc(tisvlay::Error), Local(String), … }` for the GUI's own gettext messages.
  - Their tests construct `tisvlay::Error` directly. A bridged `anyhow` that silently stopped matching would otherwise keep the tests green.
- **Name clash:** `compat.rs` glob-re-exports both `tisvlay::*` and `tisvlay_core::*`, and both now export `Error`/`Result`. It gets explicit re-exports.
- **Stale D-Bus-era comments on the touched paths** are rewritten (`org.tisvlay.Error.NotPairing`, `zbus::Error::MethodError`, `ConfigInvalidVendorConfig`).
- **The SDK's integration test** matches `Error::Rpc` instead of downcasting.

### A.5 Docs (PR A retires its own sections)

- **`CLAUDE.md` § Code conventions:** "published crates and the client SDK expose thiserror types; binaries and binary-glue libraries (runtime, the service crates' library halves, wire's bootstrap) may use `anyhow`."
- **`docs/architecture.md`:** the error model, i.e. the rpc-reply codes, `RpcError` outcomes, and the SDK's mapping.
- **This plan:** A's sections and research move out when A merges, leaving a B-only plan.

## 5. PR B: secret types

### B.1 Dependency

- `secrecy` 0.10 as a workspace dependency, with the `serde` feature.
- An exact skip in `packaging/licensing/deny.toml`, worded per the bump routine: `{ crate = "secrecy@0.8.0", reason = "zenoh-config pins secrecy 0.8 (public SecretValue); the workspace is on 0.10" }`.
- No Review trigger: `just deny -D unnecessary-skip` fails on its own once zenoh catches up.

### B.2 `tisvlay_core::Secret`

- **A newtype over `secrecy::SecretString`:** transparent `Deserialize`, a hand-written `Serialize` through `expose_secret()`, `Clone`, no `PartialEq`, `Debug` delegated to secrecy's (`[REDACTED]`).
- **One type for every typed secret,** which keeps `secrecy` out of core's, lg's and the SDK's public signatures. A later secrecy major is then a private change. It also avoids `SecretBox<Newtype>` plus the `SerializableSecret`/`CloneableSecret` ceremony.
- **Zeroize on drop is incidental:** the same key lives in the vendor config and the store. The goal is `Debug` redaction.

### B.3 Adopt it

- **`Credentials.data: Secret`.**
  - Serde comes off `Credentials`, `PairingState` and `PairingError`; it's vestigial, and core is unpublished.
  - `Credentials`' hand-written `Debug` stays (it prints `extra` key names only, the same policy as ruling 5). `extra` values stay plain: `scheme` isn't a secret, and secret values would force `expose_secret()` at every read.
- **`LgVendorConfig.client_key: Option<Secret>`:** `Debug` becomes a derive, and `PartialEq, Eq` are dropped (one test adapts).
- **`RegisterPayload`, `RegisteredPayload` and `WebOsClient`** follow. That's a published-API change in lg (`connect`, `connect_with_scheme`, `client_key()`), free while lg is unpublished.
- **The redaction tests stay,** asserting `[REDACTED]` instead of `<redacted>`.

### B.4 `tisvlay_core::VendorConfig`

- **A newtype over `toml::Table`,** so "vendor config is a table" becomes a type:
  - serde-transparent, so the store format is unchanged;
  - derives `Clone, PartialEq` (`TvRecord` and `StoreSnapshot` derive `PartialEq`);
  - `Debug` prints sorted key names only;
  - no `Deref`; it provides `parse::<T>()`, `get`, `get_mut` and `insert`.
- **The type spreads instead of following a hand-kept list:**
  - `TvConnector`'s methods take `&VendorConfig`, and `build_config_from_pairing` returns one.
  - World and daemon code then carry it end to end, and a missed holder is a compile error.
  - This is free while core is unpublished. In the four implementations, most uses are `vendor_config.clone().try_into()`, which becomes `parse::<T>()`.
- **Every holder listed in §3 changes,** including the mutation sites (the MAC fill, the update-config merge) and the `json_to_toml` seam.
- **The leak is fixed as a consequence,** with a regression test asserting that a dropped `ConfigReady`'s debug output doesn't contain the key.
- **`TvConfig`/`SystemConfig` are deleted** (dead code; no compatibility requirement).

### B.5 Docs

- **`CLAUDE.md`'s secrets convention:** "typed secrets are `tisvlay_core::Secret`; untyped vendor config is `VendorConfig`". The "never log a `TvConfig`/`TvRecord` whole" caveat goes.

## 6. Verification

- **`just check` on every commit and PR.** Each new gate is proven red: a typo'd code doesn't compile, and `{:?}` of a key-holding `VendorConfig` or `Secret` doesn't contain the key.
- **Wire bytes:** the existing raw-JSON code assertions pass unchanged, and the A.1 round-trip covers the nine codes they don't.
- **Store bytes:** B's first commit, before any type change, adds a checked-in golden store (a `TvRecord` with an LG table including `client_key`), asserting load plus a byte-identical re-save. It then guards the newtype change. It's a regression guard for flatten and transparent serde, not a compatibility shim.

## 7. Deferred, to `docs/ideas/` on retirement

- **Typed pairing-progress and dial-failure codes** (`pairing.*`, `device.*`): today `Option<String>` on the wire and in the SDK. Fold into [typed-failed-reason-payload.md](../ideas/typed-failed-reason-payload.md), which covers this vocabulary and still names retired D-Bus types.
- **Delete core's unused `PowerError`/`PowerErrorCode`/`ConfigErrorCode`,** which are D-Bus-era leftovers with no users outside core.
- **SDK parameters as `Uuid` and `&Value`** instead of `&str` ids and JSON strings, which would remove most `InvalidInput`.
- **The SDK's stringly pairing `phase` matching** (`"complete"`/`"failed"`).

## 8. Owner rulings on the review round

1. **B splits into B1 and B2.** B1 is `Secret` across core and lg: small and self-contained. B2 is `VendorConfig`: the trait, the world and daemon holders, the dead `TvConfig` removal.
2. **The live leak gets a hotfix now,** as a standalone PR off main that logs the update's kind, not its payload, with a regression test. B2 then makes the class impossible.

## 9. Retirement

- **A merges:** A's durable content goes to `docs/architecture.md` and `CLAUDE.md`, and the plan keeps only B.
- **B merges:** the secret rules go to `CLAUDE.md`, the §7 items go to `docs/ideas/` (with index entries), this file is deleted, the "Active plans" entry is removed, and inbound references are swept.

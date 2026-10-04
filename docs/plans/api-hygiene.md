# API hygiene: typed SDK errors and secret types

Status: drafted 2026-10-04, before review. Nothing is implemented yet.

Two public-API cleanups the code-quality pass deferred, landing as two PRs:

- **PR A, typed errors:** the client SDK (`tisvlay`) stops returning `anyhow::Result`, and the wire's error codes become one shared enum.
- **PR B, secret types:** the LG client key becomes a type whose `Debug` can't leak it, and the untyped vendor-config bag gets a `Debug` that prints key names only.

They're independent; A goes first and carries this plan.

## 1. Rulings so far

Decided with the owner before drafting:

1. **The SDK's error API is publish-grade, but the SDK stays unpublished.** `#[non_exhaustive]`, no dependency's error type named in a signature, semver-minded. Flipping `publish = true` later is a checklist run, not a redesign. Publishing would also mean publishing `tisvlay-wire`, whose API carries zenoh types; that's out of scope.
2. **One crate-wide `#[non_exhaustive]` thiserror enum,** with variants named for what the caller can act on, not where the error came from. Dependency errors ride as boxed `source`s.
3. **The error-code vocabulary is one enum in `tisvlay-wire`,** shared by the servers that send codes and the clients that match on them. The bytes on the wire don't change.
4. **Secrets use `secrecy` 0.10,** the current major, accepting a reasoned duplicate: zenoh pins 0.8 in its public config API (§3).
5. **Assumed, not yet confirmed:** the untyped vendor-config bag's `Debug` prints key names only. `Debug` can't consult the vendor schema, which lives behind the connectors.

## 2. Research (verified dates)

**Error design, 2025–26:**

- **The consensus holds:** libraries expose concrete error types and never `anyhow`, and a dependency's error goes in as a boxed `source`, not as a named type in the API ([d34dl0ck, May 2025](https://d34dl0ck.me/rust-bites-designing-error-types-in-rust-libraries/index.html); [Palmieri](https://www.lpalmieri.com/posts/error-handling-rust/)).
- **Two shapes are both current:** a non_exhaustive enum with data (AWS's SDK `SdkError`; [iroh's snafu enums, Aug 2025](https://www.iroh.computer/blog/error-handling-in-iroh)), or an opaque struct plus a data-less `ErrorKind` (std::io, reqwest). We take the enum: two callers, one branch point, and boxed sources keep dependency types out of the API without the opaque ceremony.
- **"Design errors, don't forward them":** scope errors to what the caller can do, and write them for both machines and humans (iroh; Tison's `exn`).
- **Backtraces aren't needed here.** iroh chose snafu for automatic backtrace capture, since std's generic member access is still unstable ([#99301](https://github.com/rust-lang/rust/issues/99301)). Our failures are IPC outcomes, not deep stacks, so thiserror (already a dependency) is enough.

**secrecy:**

- **0.8.0 (2021-07)** has the generic `Secret<T>`, opt-in `DebugSecret`, and `SerializableSecret`.
- **0.10.0 (2024-09)** is a redesign: `SecretBox<T>`, `SecretString = SecretBox<str>`, `DebugSecret` removed (everything redacts), serde default features off. 0.10.1–0.10.3 (through 2024-10) added `Clone` and `Deserialize` for `SecretString`. No release since.
- **0.10 withholds `Serialize`** unless the inner type implements the `SerializableSecret` marker, which the orphan rule rules out for `String`. A serializable secret needs a local newtype. zenoh-config does exactly this (`SecretString(String)` under its 0.8 `Secret`).
- **zenoh stays on 0.8 until its own breaking release:** main still pins `secrecy = "0.8.0"`. A May 2025 dependency refresh ([eclipse-zenoh/zenoh#1940](https://github.com/eclipse-zenoh/zenoh/pull/1940), closed unmerged Oct 2025) deferred it as "big API change that may need discussion", and `zenoh-config` exposes `SecretValue = Secret<SecretString>` publicly.

## 3. Current state

**SDK (`crates/tisvlay`, about 1,700 lines):** 75 public methods return `anyhow::Result` through 33 `anyhow!`/`context` sites. They fall into these kinds:

| Kind | Example sites |
|---|---|
| Socket connect | "connect to daemon socket {path}", session and tray sockets |
| Subscribe | zenoh subscriber/liveliness declarations (about 10) |
| Rpc | anything through `Caller::call`, already a typed `RpcError` |
| Bad caller input | an unparsable TV id, config JSON or patch JSON |
| Not found locally | "no vendor '{v}'", "no system profile {p}" (mirror lookups) |
| Malformed reply | "reply carries no `{field}`", decode of a schema or defaults |

Only the GUI and the tray depend on the SDK. The CLI uses `tisvlay-wire`'s `Caller` directly.

**Wire codes:** `WireError { code: String, message }` and `RpcError { code: String, message }`.

- 14 `WireError` constructors cover the server codes.
- Four codes are inline literals in the daemon's bridge: `daemon-shutting-down`, `discovery-not-found`, `bad-request`, `unknown-rpc`.
- Five caller codes come from `RpcError::caller`: `rpc.transport`, `rpc.unavailable`, `rpc.no-reply`, `rpc.timeout`, `rpc.encode`.
- 58 `WireError::` call sites: world 37, session 16, daemon 4, tray 1.
- About 20 tests assert on code strings, mostly as raw JSON (`err["code"] == "tv-not-found"`), which stay valid because the bytes don't change.

**Callers that branch on a code:** only the GUI. `edit_pane.rs` walks an `anyhow` chain with `downcast_ref::<RpcError>()` to find `invalid-field` (to mark the offending form field) and `pairing-not-found`. The wire's own `Caller` retries on `rpc.no-reply`.

**The client key** lives in:

- core's `Credentials.data` (published API; hand-written redacting `Debug`; derives `Serialize` because `PairingState::Complete` carries it);
- lg's `LgVendorConfig.client_key` (hand-written `Debug`; derives `PartialEq, Eq`), `RegisterPayload` and `RegisteredPayload` (hand-written `Debug`), and `LgClient` (hand-written `Debug`);
- the untyped vendor-config bag: core's `TvConfig.vendor_config: HashMap<String, toml::Value>` (flattened), the world's `TvRecord.config`, `VendorRef.config` and `Effect::{SpawnConnect, StartPairing, CompletePairing, Facts}`, `InEvent::ConfigReady`, and the connect flow. All derive `Debug`.
- the TOML store on disk, in plaintext. That's by design and out of scope.

The client key never crosses the wire: the reactor strips `FieldVisibility::Secret` fields (per the vendor schema) from `get-config` replies.

## 4. PR A: typed errors

### A.1 `ErrorCode` in `tisvlay-wire`

- **A `#[non_exhaustive]` enum** with one variant per code: the 14 constructor codes, the four inline ones, and the five caller codes.
- **The wire format is unchanged.** Serialize and deserialize as the same strings through `#[serde(into = "String", try_from = "String")]` (or `from`) backed by `Display`/`FromStr`. Variants like `rpc.no-reply` can't be expressed by `rename_all`.
- **Unknown codes are preserved:** an `Other(String)` variant keeps a code from a newer server, so an old client still displays it. `#[serde(other)]` only allows unit variants and would drop the string.
- **`WireError.code` and `RpcError.code` become `ErrorCode`.** The four inline literals get constructors, so no server builds a code by hand.
- **Tests:** a round-trip test over every variant pins the strings, which are the real contract, and one test proves `Other` preserves an unknown code.

Open: should `RpcError` (`code + message`) and `WireError` (`code + message`) become one type, now that their codes share an enum? They differ only in who produces them.

### A.2 `tisvlay::Error`

A sketch, to be refined:

```rust
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The service answered with a typed refusal.
    #[error("{message} ({code})")]
    Rpc { code: ErrorCode, message: String },
    /// No service answered within the matching deadline (not running?).
    #[error("{service} is unavailable")]
    Unavailable { service: &'static str },
    /// The service took the request but didn't reply in time.
    #[error("{what} timed out")]
    Timeout { what: String },
    /// Couldn't open the socket.
    #[error("connect to {}", socket.display())]
    Connect { socket: PathBuf, #[source] source: BoxError },
    /// A zenoh subscription or declaration failed.
    #[error("subscribe {what}")]
    Subscribe { what: String, #[source] source: BoxError },
    /// The caller passed something malformed (an id, JSON).
    #[error("invalid {what}")]
    InvalidInput { what: String, #[source] source: Option<BoxError> },
    /// A lookup in the local mirror found nothing.
    #[error("no {what}")]
    NotFound { what: String },
    /// A reply didn't have the expected shape.
    #[error("malformed reply: {what}")]
    Protocol { what: String, #[source] source: Option<BoxError> },
}
pub type Result<T> = std::result::Result<T, Error>;
```

- **Caller codes map to variants.** `rpc.unavailable` → `Unavailable`, `rpc.timeout` → `Timeout`; transport, encode and no-reply → `Protocol` or a `Transport` variant (open). Server codes stay `Rpc { code, … }`.
- **`Error::rpc_code(&self) -> Option<&ErrorCode>`** for callers that only branch on a code.
- **Every public method** returns `tisvlay::Result`. The 33 sites become explicit constructions, and no `From` for dependency errors is public.
- **Watch streams:** reconcile any `anyhow` in item types too.

### A.3 Callers

- **GUI:** the paths that branch (save config, re-pair) keep `tisvlay::Error` up to the branch and `match` on `Error::Rpc { code: ErrorCode::InvalidField, .. }`. The anyhow-chain downcast in `edit_pane.rs` goes away. Elsewhere the GUI stays `anyhow`, since it's a binary, and `?` converts.
- **Tray:** mostly `?` into `anyhow`, unchanged.
- **CLI:** gains typed codes through `RpcError` for free; no behavior change.
- **Wire's `Caller` retry** matches `ErrorCode::RpcNoReply` instead of a string.

### A.4 Docs and conventions

- **`CLAUDE.md` § Code conventions:** the errors rule goes back to its strict form. Library crates, the SDK included, expose thiserror types; `anyhow` is for binaries only.
- **`docs/architecture.md`:** the error model, i.e. the code vocabulary and how the SDK maps it.
- **`docs/build.md` first-publish checklist:** note that the SDK's error API is publish-ready.

## 5. PR B: secret types

### B.1 Dependency

- **`secrecy` 0.10 as a workspace dependency,** with the `serde` feature.
- **`deny.toml` gets an exact skip:** `secrecy@0.8.0`, reason "zenoh-config pins secrecy 0.8 in its public config API (`SecretValue`)".
- **A Review trigger:** drop the skip when zenoh moves to 0.10.

### B.2 The secret type in core

- **The serializable case needs a local newtype,** something like `pub struct SecretText(String)` implementing `Zeroize`, `SerializableSecret` and `CloneableSecret`, used as `SecretBox<SecretText>`. `Credentials` needs `Serialize` through `PairingState`, and the LG config persists the key. Naming is open (`Secret`, `ClientSecret`, …).
- **Or drop `Serialize` from `PairingState`/`Credentials`,** if it turns out unused, and use plain `SecretString`. Verify first.

### B.3 Adopt it

- **`Credentials.data`** becomes the secret type, and its hand-written `Debug` reduces to a derive. `extra` values stay redacted, since its docs anticipate refresh tokens there; either keep the manual `Debug` for `extra` or make `extra` secret-valued (open).
- **`LgVendorConfig.client_key: Option<…secret…>`.**
  - Its hand-written `Debug` becomes a derive.
  - `PartialEq, Eq` must go or become manual, since `SecretBox` deliberately has no `PartialEq`. Check what compares configs.
- **`RegisterPayload`, `RegisteredPayload` and `LgClient`** follow, with reads through `expose_secret()`. About a dozen read sites.
- **The redaction tests stay,** now testing the wrapper end to end.

### B.4 The vendor-config bag

- **A `VendorConfig` newtype in core** over the vendor table: serde-transparent, so the store format is unchanged, with a `Debug` that prints only key names (ruling 5).
- **Applied at every holder that derives `Debug`:** `TvConfig.vendor_config` (inside the flattened map; check that flatten plus a transparent newtype round-trips), `TvRecord.config`, `VendorRef.config`, the `Effect` variants, `InEvent::ConfigReady`, and the connect flow.
- **Open, the biggest scoping question:** the published core's `TvConnector` trait takes `&toml::Value` in about ten methods (`connect`, `wake`, `supports_wake`, `display_config`, …). Should those take `&VendorConfig`?
  - Changing them extends the protection into every connector implementation, which could otherwise log `?vendor_config`. The cost is churn in a published trait and all three backends.
  - Leaving them protects the holders while the connector boundary hands over the inner value.

### B.5 Docs and conventions

- **`CLAUDE.md`'s secrets convention:** a secret is a `secrecy` type, and untyped vendor config is `VendorConfig`. The "never log a `TvConfig` whole" caveat goes away.
- **`docs/build.md` Review triggers:** the zenoh-on-0.10 trigger from B.1.

## 6. Verification

- `just check` at each PR, and each gate proven red where it's new: for example, a `WireError` built with a typo'd code no longer compiles, and `format!("{:?}")` of a `VendorConfig` holding a key doesn't contain it.
- **Wire compatibility:** the existing raw-JSON code assertions in the daemon, session and world tests pass unchanged. That's the proof the bytes didn't move.
- **Store compatibility:** a store written before PR B loads after it, and re-saves byte-identically.

## 7. Open questions

1. Merge `RpcError` and `WireError` into one type (A.1)?
2. A `Transport` variant, or fold `rpc.transport`/`rpc.encode`/`rpc.no-reply` into `Protocol` (A.2)?
3. `TvConnector` methods take `&VendorConfig`, or keep `&toml::Value` (B.4)?
4. Name and shape of the core secret type, and whether `Credentials.extra` values become secrets (B.2, B.3)?
5. Confirm ruling 5, key-names-only `Debug` for the bag.

## 8. Retirement

When B merges:

- The error model goes to `docs/architecture.md`.
- The secret-type rules go to `CLAUDE.md`'s conventions.
- The secrecy trigger goes to `docs/build.md`.
- Deferred items go to `docs/ideas/`.
- This file is deleted.

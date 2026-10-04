# API hygiene: typed RPC endpoints, typed errors, secret types

Status: drafted 2026-10-04. Two review rounds (design, implementation correctness, maintainability) are folded in, and every open question is ruled. Nothing is implemented yet.

Three public-API cleanups, landing as four PRs in a fixed order:

- **A0, typed endpoints:** each of the 46 rpcs is declared once (its key, request type and response type), and clients and servers derive from that declaration. No more `json!` bodies, `Value` replies picked apart by field name, or string verb matches. A wire refactor; the SDK's public API doesn't change.
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
7. **Typed RPC endpoints go first, as A0,** on our own zenoh layer, not a framework (§2).
8. **No compatibility code, on the wire included.** Every client is in this repo and ships together, and the shared Rust types are the contract.
   - Requests are strict: no lenient decoders, no null-or-object bodies, no aliases for old senders.
   - There's no unknown-code fallback.
   - The upgrade window (the daemon restarts on the new version while user units keep the old binaries) is accepted: the packaging docs say to log out and back in after upgrading.
9. **Nothing pins the wire's JSON shape.** There are no golden tests, and the daemon's raw-JSON integration tests move to typed calls. The shape may be cleaned up where it's awkward.
10. **World-answered rpcs use typed `Reply<R>` tokens,** so a mismatched reply doesn't compile.
11. **One plan, in a fixed order:** A0, A, B1, B2. Each PR still retires its own sections when it merges.

## 2. Research (verified dates)

**Typed RPC, and why no framework:**

- **The idiomatic pattern is the typed endpoint:** a key plus its request and response types, declared once, with compile-time pairing. Examples: [postcard-rpc's `Endpoint`](https://docs.rs/postcard-rpc/), Bitwarden's [`RpcRequest`](https://sdk-api-docs.bitwarden.com/bitwarden_ipc/trait.RpcRequest.html), Fedimint's [`TypedApiEndpoint`](https://docs.fedimint.org/fedimint_core/module/trait.TypedApiEndpoint.html). On the server side, tarpc's generated request enum (one variant per call, carrying its decoded arguments) is the model for A0.3, minus tarpc's transport.
- **zenoh's [RFC #2510](https://github.com/eclipse-zenoh/zenoh/issues/2510)** (opened 2026-03-22, still open) proposes `TypedQueryable<Req, Resp>` in zenoh-ext. If it lands, our layer can sit on it.
- **[zrpc / Zenoh-RPC](https://github.com/ZettaScaleLabs/zenoh-rpc)** (ZettaScale, last pushed 2026-07-27) says "alpha… should not be used in production". It brings its own encoding and key layout, which would conflict with our empirical zenoh laws (matching-gated queriers, servers as client-mode links).
- **[tarpc](https://docs.rs/tarpc)** needs a connection-shaped transport; zenoh query/reply isn't one.
- **tonic/gRPC** would replace zenoh, undoing the v0.3.0 architecture.

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

**RPC surface:** 46 rpcs on 8 queryables (7 family wildcards, plus an exact `manager/vendors` that the `vendors/**` wildcard already covers).

| Family | Key shape | Server | Rpcs |
|---|---|---|---|
| manager | `manager/rpc/<verb>` | daemon bridge | `info`, `enrich`, `discover` (streams), `discover-start`, `discover-stop`, `add-tv`, `pair`, `cancel-pair` |
| vendors | `manager/vendors`, `manager/vendors/<v>/schema` | daemon bridge | catalog, schema |
| system profiles | `profile/rpc/<verb>` | daemon, then the world | `create`, `update`, `duplicate`, `delete`, `defaults`, `effective` |
| per-TV | `tv/<uuid>/rpc/<verb>` | daemon, then the world | 13 setters and actions, plus `get-config`, `update-config`, `re-pair` |
| user profiles | `user/profile/rpc/<verb>` | session | `create`, `update`, `duplicate`, `delete`, `info`, `defaults`, `effective`, `assign` |
| remote session | `user/remote/rpc/<verb>` | session | `set`, `status` |
| tray settings | `user/tray/rpc/<verb>` | tray | `get`, `update`, `test`, `info` |

- **Dispatch:** each server pushes queries into one task, extracts the verb with `keys::parse_*`, and matches on strings. The daemon's manager family is split: `info`, `enrich` and `discover*` are exact-key `if`s in `handle_query`, and the rest falls through to `route()`. World-routed verbs reply later through `WireEffect::Reply { result: Result<Value, WireError> }`, keyed by a bare `ReplyTo` id. The bridge overrides five replies with a pre-minted `ok_payload` (add-tv, pair, re-pair, profile create, profile duplicate).
- **Requests:** 21 `rpc::*` structs. `enrich`, `update-config`, remote `set` and tray `update` are ad hoc. Clients mostly hand-write `json!`: the SDK uses the structs for 3 calls, the CLI for 3, the tray for 2.
- **Replies:** 9 come from typed structs, 36 are `json!` literals, and 1 is an ad-hoc map.
- **Latent bug:** `Caller` turns a success payload that fails to decode into `Value::Null`, silently.
- **Inconsistencies:**
  - An unknown verb gets `unknown-rpc` from the daemon but `bad-request` from the session and tray.
  - The session's remote handler and the tray treat bad JSON as null.
- **`call` vs `fetch`:** every read uses `fetch` (re-issue on no-reply) and every write uses `call`. `call_many` serves `discover` only.

**SDK (`crates/tisvlay`):** 65 methods return `anyhow::Result`.

- **Error sites:** about 44.
- **Untyped parameters:** TV and profile ids as `&str`, configs and patches as JSON strings.
- **Untyped returns:** `re_pair -> String`, `get_tv_config -> String`, `get_effective_system_profile -> Value`.
- **Notification settings:** `NotificationSettingsBody` is the Option-field patch shape, and the GUI uses it for both get and update.
- **Client-side lookups:** three scan a fetched list (vendor, system profile, user profile).
- **The GUI is its only shipped caller;** the tray uses it in tests only.

**Codes and outcomes:**

- `WireError { code: String, message }`: 17 server codes, all kebab-case. 14 come from constructors, and 3 are inline literals in the bridge, which also duplicates `bad_request`.
- `RpcError { code: String, message }` adds 6 caller-minted outcomes that never cross the wire.
- **The GUI branches on two codes** through an anyhow-chain downcast in `edit_pane`. The `invalid-field` message format `"<field>: <reason>"` is load-bearing.
- **Out of scope:** pairing-progress and dial-failure codes (`pairing.*`, `device.*`).

**The client key** lives in:

- core's `Credentials.data` (its serde derives are vestigial);
- lg's `LgVendorConfig`, `RegisterPayload`/`RegisteredPayload` and `WebOsClient` (about 59 sites);
- the untyped vendor config: a `toml::Value` that's always a table, held by about a dozen world and daemon types that derive `Debug`, and passed through the published `TvConnector` trait;
- the wire's request types (`rpc::AddTv.config`, and the update-config patch), which can carry a key from client to daemon;
- the plaintext store, by design.

Replies never carry the key, because `get-config` strips schema-`Secret` fields. Core's `TvConfig`/`SystemConfig` are dead code.

## 4. PR A0: typed endpoints (wire refactor; SDK public API unchanged)

### A0.1 Families and endpoints in `tisvlay-wire`

```rust
/// A family of rpcs served by one wildcard queryable.
pub trait Family {
    /// What, besides the site, addresses a call: `()`, a TV `Uuid`, a vendor `str`.
    type Target: ?Sized;
    /// The decoded request, one variant per rpc (A0.3).
    type Call;
    fn key(site: &str, target: &Self::Target, verb: &str) -> String;
    fn wild(site: &str) -> String;
}

pub trait Rpc {
    type Family: Family;
    const VERB: &'static str;
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;
}
/// One reply. `REISSUE`: safe to re-issue on no-reply (reads; no minting, no relative effect).
pub trait Unary: Rpc { const REISSUE: bool; }
/// A reply per finding, collected until the window closes.
pub trait Streaming: Rpc { fn window(request: &Self::Request) -> Duration; }
```

- **One small `macro_rules!` invocation per verb-shaped family** (6 families, 44 rpcs). Each generates:
  - the family type;
  - a zero-sized marker per rpc implementing `Rpc` plus `Unary` or `Streaming`, with `$(#[doc])*` passed through for rustdoc;
  - the family's request enum (A0.3).

  The request and response structs stay hand-written outside the macro, and go-to-definition lands on the family's table rows.
- **The vendors family** (catalog and schema; no verb, and the vendor sits mid-key) is written by hand against the same traits.
- **Requests:**
  - Body-less rpcs take `()`.
  - `power-on` takes `PowerOn`; clients send `restore_input: false` instead of null.
  - The four ad-hoc bodies get structs: `Enrich { vendor, ip }`, an object-typed `UpdateConfig` patch, `RemoteSet { source: Option<String>, active }`, and `NotificationSettingsPatch` (Option fields with `skip_serializing_if`).
  - Optional fields remain only where optional is the API (remote `set`'s `source`).
- **Responses:**
  - **Existing types are reused:** `PairingField`, `DiscoveredTv`, `SystemProfileConfig`, `UserProfileConfig`, `RemoteSessionState`.
  - **New wire types:**
    - `Ack`, a braced empty struct that serializes as `{}`;
    - `Created { id }`, `PairStarted { id, attempt }`, `RePairStarted { attempt }`, `DiscoverySession { session }`;
    - `ServiceInfo { version, state_path }`, `VendorEntry { id, name, capabilities }`, `EnrichReply { mac: Option<…> }`;
    - `TvConfigView`, the redacted `get-config` record.
  - **Core changes:** `ResolvedUserProfile` gains `Serialize` and `Deserialize`.
  - **Notification settings move to wire:** the tray's `NotificationSettings` moves into wire with its default-on `Default`, `bits()` and `merged_with()`.
  - **The SDK keeps its own `PairStarted`** (string fields) until A retypes the SDK API.
- **Shape cleanup is allowed** (ruling 9). For example, if `tv_id`/`id` naming is inconsistent across replies, A0 fixes it rather than preserving it.

### A0.2 The typed `Caller`, and `RpcError` as the call's outcome

- **`RpcClient { caller, site }`** holds the site:
  - `call::<R: Unary>(&target, &req) -> Result<R::Response, RpcError>` re-issues on no-reply only when `R::REISSUE`;
  - `call_many::<R: Streaming>(…) -> Result<Vec<R::Response>, RpcError>` takes its window from `R::window`, moving the window logic out of the CLI.
- **Decoding happens once, in the client:** a reply that doesn't decode as `R::Response` is an error, never a silent `Null`.
- **The error is an outcome enum,** introduced here because strict decoding needs it:

  ```rust
  pub enum RpcError {                       // exhaustive: wire is unpublished
      Service(WireError),                   // the service refused
      Unavailable { key: String },          // no queryable matched within the deadline
      Timeout { key: String },              // matched, no reply in time
      NoReply { key: String },              // re-issued once if REISSUE; outcome unknown
      Transport { key: String, source: zenoh::Error },
      Undecodable { key: String },          // a payload that isn't the expected type
  }
  ```

  `WireError` and `RpcError` stay separate types: a server's payload versus a call's outcome. Request encoding becomes an `expect`, since serializing our request types (structs and string-keyed maps) can't fail. The CLI prints `RpcError` through `Display`.
- **The untyped `call`/`fetch`/`call_many` are deleted in this PR.** Main never carries two APIs.

### A0.3 Servers dispatch from generated request enums

- **Each family's generated request enum carries the decoded body,** e.g. `TvCall::SetVolume(rpc::SetVolume)`, `TvCall::PowerOff(())`.
  - `TvCall::decode(verb, payload) -> Result<TvCall, WireError>` gives one decode site and the same errors on every server: `unknown-rpc` for an unknown verb, `bad-request` for a malformed body.
  - Servers `match` the enum exhaustively, so a family member without a handler doesn't compile, and neither does a handler fed the wrong request type.
  - The daemon's split manager dispatch becomes one exhaustive match per family.
- **Servers that answer straight away** (session, tray, the daemon's own verbs) reply through `reply::<R>(&query, &R::Response)`.
- **World-answered rpcs carry a typed `Reply<R>` token** (a `Copy` newtype over the reply id, with `PhantomData<R>`) in their `Command` variant, with `ok(&R::Response)` and `err(WireError)`.
  - `WireEffect::Reply` stays `Value`, so the bridge stays generic.
  - The world already knows every minted id (`record.id`, the attempt, the new profile id), so it replies `Created`/`PairStarted`/`RePairStarted` itself, and **`ok_payload` is deleted**.
  - Two seams need an erased, error-only view: `command_reply()`, which extracts the reply from many variants, and the backend's `CommandDone`, all of whose replies are `Ack`s, via `Reply<R: Rpc<Response = Ack>>::erase()`.
  - The reactor's `redacted_config` returns `TvConfigView`.
- **Queryables are declared from `Family::wild`.** The redundant exact `manager/vendors` queryable is dropped.
- **Servers and the typed `Caller` never `?`-log requests,** since request types can carry vendor config (B2 redacts them structurally).

### A0.4 Clients (internal only)

- **The SDK's method bodies move to typed calls; their signatures don't change** (A retypes them). The GUI is untouched.
- **The CLI moves to typed calls** and keeps printing through `serde_json::to_value`, so its output stays sorted and deterministic (`DiscoveredTv.vendor_data` is a `HashMap`).
- **The tray (production) and the session's `call_tv`** move too.

### A0.5 Tests

- **No goldens** (ruling 9).
- **These suites move to typed calls:**
  - the daemon's raw-JSON integration tests;
  - the session test that uses the untyped `Caller`;
  - the world tests' about 41 `reply: Some(..)` constructions, which become `Reply::new(..)`;
  - the reactor redaction test.
- **Gates proven red:**
  - a client calling `call::<R>` with the wrong request type;
  - a server arm handling the wrong body type, or missing a family member;
  - `call::<Discover>` (it's streaming, not unary).
- **Coverage:** keep coverage at least where it is. The suites move rather than vanish, and rpcs no test reaches today (the three `info`s, `enrich`, catalog/schema, `set-muted`, `reconnect`, user `update`/`duplicate`, the tray's four) get a typed case each.

### A0.6 Commits

1. The traits, the family tables, the request enums, the new request and response types, `RpcClient` and `RpcError`.
2. The servers move: request enums, `Reply<R>` in the world, `ok_payload` gone, the consistency fixes, queryables from `Family::wild`.
3. The clients move (SDK internals, CLI, tray, session), the untyped `Caller` goes, and the tests migrate.

### A0.7 Docs (retired when A0 merges)

- **`docs/architecture.md`:**
  - the endpoint model (families, the request enums, `Reply<R>`, `RpcError`), plus "why no framework" from §2 as a decision note;
  - the tray settings family, which its key-space table lacks.
- **`CLAUDE.md`:** "a new rpc is declared once in its family's table; the compiler then demands the server arm. Add the SDK or CLI surface by hand."
- **`packaging/README.md`:** after upgrading, log out and back in, since user units keep the old binaries until then.
- **`docs/ideas/`:**
  - adopting zenoh-ext's typed queryables if RFC #2510 lands;
  - SDK methods for the rpcs only the CLI reaches (`add-tv`, `discover`, user `effective`).

## 5. PR A: typed errors and the SDK's public API

### A.1 `ErrorCode`

- **One `macro_rules!` table** generates the enum (with a serde `rename` per variant, all kebab-case), `as_str` and `ALL`, covering the 17 server codes. Adding a code is one line.
- **No unknown-code fallback** (ruling 8). An unknown code fails decoding into `RpcError::Undecodable`.
- **`WireError.code: ErrorCode`.**
  - The three inline literals get constructors, and the bridge's duplicate `bad_request` goes.
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

## 6. PR B1: `Secret` (core and lg)

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

## 7. PR B2: `VendorConfig` (core, trait, world, daemon, wire requests)

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

## 8. Verification

- **`just check` on every commit and PR.**
- **Every new gate is proven red:**
  - a mismatched request type doesn't compile, on either side;
  - a server missing a family member doesn't compile;
  - a world reply of the wrong type doesn't compile;
  - a typo'd error code doesn't compile;
  - `{:?}` of a key-holding `Secret`, `VendorConfig` or request doesn't show the key.

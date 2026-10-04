# API hygiene: typed RPC endpoints, typed errors, secret types

Status: drafted 2026-10-04, revised after three reviews. The owner then widened the scope to typed RPC endpoints (A0), and this revision awaits a second review round. Nothing is implemented yet.

Three public-API cleanups, landing as four PRs in order:

- **A0, typed endpoints:** each rpc is declared once (its key, request type and response type), and clients and servers derive from that one declaration. No more `json!` bodies or `Value` replies picked apart by field name.
- **A, typed errors:** the wire's rpc-reply codes become one enum, the wire `Caller`'s failures become an enum that wraps them, and the client SDK (`tisvlay`) gets a typed `Error` instead of `anyhow`.
- **B1, `Secret`:** the LG client key becomes a type whose `Debug` can't leak it.
- **B2, `VendorConfig`:** the untyped vendor config becomes a type whose `Debug` prints key names only, threaded through the connector trait.

A0 carries this plan. The live debug-log key leak was hotfixed separately (#149).

## 1. Rulings

1. **The SDK's error API is publish-grade, but the SDK stays unpublished.** No dependency's error type is named in a signature. Publishing would also mean publishing `tisvlay-wire`; that's out of scope.
2. **One crate-wide `#[non_exhaustive]` thiserror enum** for the SDK, with variants named for what the caller can act on.
3. **The rpc-reply code vocabulary is one enum in `tisvlay-wire`,** shared by servers and clients.
4. **Secrets use `secrecy` 0.10,** accepting a reasoned duplicate: zenoh pins 0.8 in its public config API.
5. **The untyped vendor config's `Debug` prints key names only.**
6. **B splits into B1 and B2, and the leak gets a hotfix first** (#149).
7. **Typed RPC endpoints go first, as A0,** built on our existing zenoh layer, not a framework (§2).

## 2. Research (verified dates)

**Typed RPC, and why no framework:**

- **The idiomatic pattern is the typed endpoint:** a key plus its request and response types, declared once, with compile-time pairing. Examples: [postcard-rpc's `Endpoint`](https://docs.rs/postcard-rpc/), Bitwarden's [`RpcRequest`](https://sdk-api-docs.bitwarden.com/bitwarden_ipc/trait.RpcRequest.html) (`type Response`, `const NAME`), Fedimint's [`TypedApiEndpoint`](https://docs.fedimint.org/fedimint_core/module/trait.TypedApiEndpoint.html).
- **zenoh's own [RFC #2510](https://github.com/eclipse-zenoh/zenoh/issues/2510)** (opened 2026-03, still open) proposes `TypedQueryable<Req, Resp>` in zenoh-ext for exactly this boilerplate. It isn't merged; if it lands, our layer can sit on it.
- **[zrpc / Zenoh-RPC](https://github.com/ZettaScaleLabs/zenoh-rpc)** (ZettaScale, last pushed 2026-07): its README says "alpha… should not be used in production". It brings its own encoding (bincode/CBOR) and key layout, which would change our wire bytes and conflict with our empirical zenoh laws (matching-gated queriers, servers as client-mode links).
- **[tarpc](https://docs.rs/tarpc)** needs a connection-shaped transport (`Stream + Sink`); zenoh query/reply isn't one.
- **tonic/gRPC** would replace zenoh, undoing the v0.3.0 architecture (cached publishers, liveliness).

**Error design, 2025–26:**

- **Libraries expose concrete error types and never `anyhow`,** with dependency errors as boxed sources ([d34dl0ck, May 2025](https://d34dl0ck.me/rust-bites-designing-error-types-in-rust-libraries/index.html)).
- **Enum or opaque-struct shapes are both current** (AWS's SDK and [iroh, Aug 2025](https://www.iroh.computer/blog/error-handling-in-iroh) vs std::io/reqwest).
- **"Design errors, don't forward them."**
- **thiserror suffices:** our failures are IPC outcomes, not deep stacks.

**secrecy:**

- **0.10.0 (2024-09)** redesigned the crate: `SecretBox`, `SecretString = SecretBox<str>`, everything redacts. Last release 0.10.3, 2024-10.
- **0.10 withholds `Serialize`** unless the inner type opts in.
- **zenoh stays on 0.8 until its own breaking release:** [#1940](https://github.com/eclipse-zenoh/zenoh/pull/1940) deferred the bump, and `zenoh-config` exposes `SecretValue` publicly.
- **zeroize 1.x is shared,** so the only duplicate is secrecy.

## 3. Current state

**RPC surface:** 46 rpcs on 8 wildcard queryables, across 7 families.

| Family | Server | Count |
|---|---|---|
| manager (`info`, `enrich`, `discover`, `discover-start`/`-stop`, `add-tv`, `pair`, `cancel-pair`) | daemon bridge | 8 |
| vendors (catalog, `<v>/schema`) | daemon bridge | 2 |
| system profiles (`create`, `update`, `duplicate`, `delete`, `defaults`, `effective`) | daemon, into the world | 6 |
| per-TV (14 setters and actions, plus `get-config`, `update-config`, `re-pair`) | daemon, into the world | 16 |
| user profiles (`create` … `assign`, `info`, `effective`) | session | 8 |
| remote session (`set`, `status`) | session | 2 |
| tray settings (`get`, `update`, `test`, `info`) | tray | 4 |

- **Dispatch:** every server pushes queries into one task, extracts the verb with a `keys::parse_*` function, and matches on the string. The daemon routes most verbs into world `Command`s and answers later through `WireEffect::Reply`.
- **Requests:** every body a server parses has a `tisvlay_wire::rpc::*` struct (21), except `enrich`, `update-config`, remote `set` and tray `update`, which are ad hoc. Clients mostly hand-write `json!` literals instead: the SDK uses the structs for 3 calls, the CLI for 3, the tray for 2.
- **Replies:**
  - 9 are serialized from typed structs.
  - 36 are `json!` literals: 22 `{}` acks, 5 `{id}`, `{id, attempt}`, `{attempt}`, `{session}`, 3 `{version, state_path}`, the vendors array, the redacted `get-config` record, and user `effective`.
  - 1 is an ad-hoc map (`enrich`).
- **Streaming:** only `discover` streams replies (`call_many`, `ConsolidationMode::None`).
- **A latent bug:** `Caller` turns a success payload that fails to decode into `Value::Null`, silently.
- **Server inconsistencies:**
  - An unknown verb gets `unknown-rpc` from the daemon but `bad-request` from the session and tray.
  - The session's remote handler and the tray treat bad JSON as null instead of refusing it.
  - The daemon also serves the vendor catalog from both an exact and a `**` queryable, which is redundant: `**` matches zero chunks.
- **Client wrappers:** `Caller` is wrapped by the SDK's three clients, by the CLI, by the tray (production, not through the SDK) and by the session's `call_tv`. The GUI reaches rpcs only through the SDK.

**SDK (`crates/tisvlay`):** 65 methods return `anyhow::Result`.

- **Error sites:** 33 `anyhow!`/`bail!`/`context` sites, plus six `map_err(anyhow::Error::new)`, three implicit `?` from wire's `bootstrap`, and two implicit `?` from `serde_json::to_value`.
- **Untyped parameters:** TV and profile ids are taken as `&str`, and vendor configs and patches as JSON strings.
- **Streams are already infallible.**

**Codes and outcomes:**

- `WireError { code: String, message }` has 17 server codes (14 constructors, plus 3 inline literals in the bridge).
- `RpcError { code: String, message }` adds six caller-minted outcomes (`rpc.transport`, `.unavailable`, `.no-reply`, `.timeout`, `.encode`, `unknown`) that never cross the wire.
- The GUI branches on `invalid-field` and `pairing-not-found` through an anyhow-chain downcast in `edit_pane`. The `invalid-field` message format `"<field>: <reason>"` is load-bearing.
- **Out of scope:** pairing-progress and dial-failure codes (core's `pairing.*`/`device.*`, `Option<String>` on the wire).

**The client key** lives in:

- core's `Credentials.data` (its serde derives are vestigial);
- lg's `LgVendorConfig`, `RegisterPayload`/`RegisteredPayload` and `WebOsClient` (about 59 sites);
- the untyped vendor config: a `toml::Value` that's always a table, held by about a dozen world and daemon types that all derive `Debug`, and passed through the published `TvConnector` trait as `&toml::Value`;
- the plaintext store, which is by design.

It never crosses the wire, because `get-config` strips schema-`Secret` fields. Core's `TvConfig`/`SystemConfig` are dead code.

## 4. PR A0: typed endpoints

### A0.1 The endpoint table in `tisvlay-wire`

```rust
pub trait Rpc {
    /// Where the rpc lives: `Site`, or `(Site, Uuid)` for per-TV rpcs.
    type Address;
    type Request: Serialize + DeserializeOwned;
    type Response: Serialize + DeserializeOwned;
    /// Safe to re-issue on no-reply (reads); writes are issued once.
    const IDEMPOTENT: bool;
    fn key(address: &Self::Address) -> String;
}
```

- **One `macro_rules!` table declares all 46 rpcs:** family, verb, request, response, idempotence.
  - It generates a zero-sized marker type per rpc, implementing `Rpc`.
  - It also generates a per-family verb enum (`TvVerb::parse(&str)`) that servers `match` on exhaustively. A server that doesn't handle a new rpc then fails to compile.
- **Keys keep their current shape,** built from the existing `keys` functions, and the bytes stay the same JSON.
- **Request types:** the existing 21 `rpc::*` structs, plus types for the four ad-hoc bodies (`Enrich`, a JSON-object `UpdateConfig` patch, `RemoteSet`, a `NotificationSettingsPatch`).
- **Response types:**
  - **Existing ones are reused** where they're already serialized: `PairingField`, `DiscoveredTv`, `SystemProfileConfig`, `UserProfileConfig`, `RemoteSessionState`.
  - **New wire types for the `json!` shapes:**
    - `Ack`, a braced empty struct that serializes as `{}` (a unit struct would serialize as `null`);
    - `Created { id }`, `PairStarted { id, attempt }`, `RePairStarted { attempt }`, `DiscoverySession { session }`;
    - `ServiceInfo { version, state_path }`, `VendorEntry { id, name, capabilities }`;
    - a redacted `TvConfigView` for `get-config`;
    - `Serialize` on core's `ResolvedUserProfile`;
    - the tray's `NotificationSettings`, which moves into wire, replacing the SDK's parallel `NotificationSettingsBody`.
- **JSON shape, not byte order:** serde_json here doesn't preserve map order, so today's `json!` replies emit sorted keys, while structs emit declaration order. Parsers don't care. Golden tests pin each reply's JSON value, not its bytes. Anything that prints raw replies (the CLI's JSON output) may reorder keys.

### A0.2 The typed `Caller`

- **`call::<R>(&R::Address, &R::Request) -> Result<R::Response, RpcError>`.** It re-issues on no-reply only when `R::IDEMPOTENT`, replacing today's call/fetch split by convention.
- **`call_many::<R>(…) -> Result<Vec<R::Response>, RpcError>`** for `discover`.
- **Decoding happens once, here.** A reply that doesn't decode as `R::Response` is an error, never a silent `Null`.
- **The untyped methods are removed** once every caller has moved (no compatibility layer).

### A0.3 Servers dispatch from the table

- **Each server parses the verb into its family enum, decodes the body as `R::Request`, and replies with `R::Response`,** through small helpers (`decode::<R>(&Query)`, `reply::<R>(&Query, &R::Response)`). The daemon's world-routed replies are built from typed values at the world seam.
- **Consistency fixes fall out:**
  - An unknown verb gets `unknown-rpc` from every server.
  - Malformed JSON gets `bad-request` from every server, so the session's remote handler and the tray stop treating it as null.
- **The redundant exact vendor-catalog queryable is dropped.**

### A0.4 Clients

- **SDK:**
  - The methods become thin typed calls.
  - TV and profile ids become `Uuid` parameters, and configs and patches become typed values instead of JSON strings.
  - The GUI adapts at its call sites.
- **CLI:** its `json!` bodies and field-by-field reply parsing move to the typed calls. JSON output may reorder keys (A0.1).
- **The tray (production) and the session's `call_tv`** move as well.

### A0.5 Verification

- **Golden tests:** one per rpc family, pinning each request's and reply's JSON value against literals copied from today's code, before the switch.
- **The existing wire, session and world tests pass unchanged.**
- **A drift guard:** a test asserts that the table's keys match what each server's queryable serves, catching a family with an rpc but no handler.

### A0.6 Commits

1. The table, the trait, the new request and response types, the typed `Caller`, and the golden tests (green against today's servers).
2. The servers move onto the table, with the consistency fixes.
3. The clients move (SDK with the GUI, CLI, tray, session), and the untyped `Caller` methods go.

## 5. PR A: typed errors (on top of A0)

### A.1 `ErrorCode` (rpc-reply codes)

- **One `macro_rules!` table** generates the enum, `as_str`, `FromStr` and `ALL`, covering the 17 server codes. The three inline literals get constructors, and the bridge's duplicate `bad_request` goes.
- **Hand-written serde through `as_str`.** Decoding is infallible: an unknown code becomes `Unknown(UnknownCode)`, whose field is private, so servers can't build one. That keeps "a typo'd code doesn't compile" true, keeps equality canonical, and covers the real skew window: postinstall restarts the daemon, but user units keep the old binaries until re-login.
- **`WireError.code: ErrorCode`,** with `as_invalid_field()` next to the `invalid_field` constructor.
- **Tests:** round-trip `ALL` against literal strings copied from today's constructors. No known string parses to `Unknown`, and an unknown one survives decode and display.

### A.2 `RpcError` is the call's outcome

`enum RpcError { Service(WireError), Unavailable { key }, Timeout { key }, NoReply { key }, Transport { key, source }, Undecodable { key } }`

- **Exhaustive,** since wire is unpublished, so the SDK's mapping is compile-checked.
- **`Undecodable` covers both** an error payload that isn't a `WireError` and, after A0, a success payload that isn't `R::Response`.
- **`rpc.encode` becomes an `expect`:** encoding a typed request into JSON can't fail.
- **The CLI prints `RpcError` through `Display`.**

### A.3 `tisvlay::Error`

```rust
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{message} ({code})")]
    Rpc { code: ErrorCode, message: String },
    #[error("{service} is unavailable")]
    Unavailable { service: Service },
    /// No answer in time. For a write, the outcome is unknown.
    #[error("{service} didn't answer in time")]
    Timeout { service: Service },
    /// The local link: session open, subscriptions, a stream that ended early.
    #[error("{what}")]
    Transport { what: &'static str, #[source] source: Option<BoxError> },
    #[error(transparent)]
    NotFound(Missing),
    /// A reply didn't decode as its rpc's response type.
    #[error("malformed reply from {service}")]
    Protocol { service: Service },
}
#[non_exhaustive] pub enum Service { Daemon, Session, Tray }
#[non_exhaustive] pub enum Missing { Vendor(String), SystemProfile(Uuid), UserProfile(Uuid) }
```

- **A0 removes `InvalidInput` entirely,** because ids and configs are typed parameters, and shrinks `Protocol` to the one decode site.
- **Mapping:** each client's `call` helper maps `RpcError`, filling in its `Service`. It's `pub(crate)`, never a public `From`.
  - `Unavailable` → `Unavailable`.
  - `Timeout` and `NoReply` → `Timeout`.
  - `Transport` → `Transport`.
  - `Undecodable` → `Protocol`.
  - `Service` → `Rpc`.
- **Helpers:** `rpc_code()` and `invalid_field()`.
- **Re-exports:** the SDK re-exports `ErrorCode`, not `RpcError`. wire's `bootstrap` keeps `anyhow` (binary glue), boxed into `Transport`.
- **Rendering:** causes live in `source()`. The GUI renders the chain, and a test pins that a connect failure names its cause.
- **`#[non_exhaustive]` stays enum-level only,** so the GUI's tests can construct variants.

### A.4 GUI

- **`find_rpc_error` and its downcast go.**
  - `extract_invalid_vendor_config` and `is_not_pairing` take `&tisvlay::Error`.
  - `PairError` gains a `Local(String)` variant for its gettext messages.
  - Their tests construct `tisvlay::Error`.
- **Name clash:** `compat.rs`'s glob re-exports of the SDK and core clash on `Error`/`Result`, so they become explicit.
- **Stale D-Bus-era comments on the touched paths** are rewritten.

### A.5 Docs (A0 and A retire their own sections)

- **`CLAUDE.md` § Code conventions:** "published crates and the client SDK expose thiserror types; binaries and binary-glue libraries (runtime, the service crates' library halves, wire's bootstrap) may use `anyhow`." A0 adds a convention: a new rpc is one line in the endpoint table.
- **`docs/architecture.md`:**
  - the endpoint table and the error model;
  - the tray settings family, which the key-space table is missing today.

## 6. PR B: secret types

### B1 `Secret` (core and lg)

- **Dependency:** `secrecy` 0.10 as a workspace dependency (`serde` feature), plus an exact skip in `packaging/licensing/deny.toml`: `{ crate = "secrecy@0.8.0", reason = "zenoh-config pins secrecy 0.8 (public SecretValue); the workspace is on 0.10" }`. `-D unnecessary-skip` retires it on its own.
- **`tisvlay_core::Secret`:**
  - a newtype over `secrecy::SecretString`, with transparent `Deserialize`, a hand-written `Serialize` through `expose_secret()`, `Clone`, no `PartialEq`, and `Debug` delegated (`[REDACTED]`);
  - one type for every typed secret, so secrecy stays out of the public signatures.
- **Adopted by:**
  - `Credentials.data`, with serde dropped from `Credentials`, `PairingState` and `PairingError`. `Credentials`' `Debug` stays hand-written, with `extra` values plain.
  - `LgVendorConfig.client_key`, which loses `PartialEq, Eq`.
  - `RegisterPayload`, `RegisteredPayload` and `WebOsClient`'s API (published lg; free while unpublished).
- **The redaction tests** assert `[REDACTED]`.

### B2 `VendorConfig` (core, trait, world, daemon)

- **A newtype over `toml::Table`:** serde-transparent, `Clone`, `PartialEq`, a key-names-only `Debug`, `parse::<T>()`, `get`/`get_mut`/`insert`, and no `Deref`.
- **It spreads through the code:**
  - `TvConnector` takes `&VendorConfig`, and `build_config_from_pairing` returns one.
  - Every world and daemon holder carries it, so a missed holder is a compile error.
  - The update-config merge and the MAC fill use its API.
  - It enters at the `json_to_toml` seam.
- **`TvConfig`/`SystemConfig` are deleted** (dead code).
- **A store regression guard comes first:** before any type change, a checked-in golden store (a `TvRecord` with an LG table including `client_key`) asserts load plus a byte-identical re-save.
- **Docs:** `CLAUDE.md`'s secrets convention says typed secrets are `Secret` and untyped vendor config is `VendorConfig`.

## 7. Verification

- **`just check` on every commit and PR.**
- **Every new gate is proven red:**
  - a mismatched request type doesn't compile;
  - a server missing a verb doesn't compile;
  - a typo'd code doesn't compile;
  - `{:?}` of a key-holding `Secret` or `VendorConfig` doesn't show the key.
- **JSON shapes are pinned:**
  - A0's golden tests;
  - the existing raw-JSON code assertions;
  - A.1's literal round-trip for the codes those assertions miss.

## 8. Deferred, to `docs/ideas/` on retirement

- **Typed pairing-progress and dial-failure codes** (`pairing.*`, `device.*`), folded into [typed-failed-reason-payload.md](../ideas/typed-failed-reason-payload.md).
- **Delete core's unused `PowerError`/`PowerErrorCode`/`ConfigErrorCode`** (D-Bus-era).
- **The SDK's stringly pairing `phase` matching.**
- **Adopting zenoh-ext's typed queryables** if RFC #2510 lands.
- **SDK methods for the rpcs that only the CLI reaches:** `add-tv`, `discover`, user `effective`.

## 9. Open questions for the second review round

1. **Is A0 one PR in three commits, or two PRs** (A0a: the table, `Caller` and servers; A0b: the clients)?
2. **Should the SDK's id and config parameter typing** (`Uuid`, typed configs) ride in A0, or wait for its own PR, given the GUI churn?

## 10. Retirement

- **A0 and A merge:** their durable content goes to `docs/architecture.md` and `CLAUDE.md`, and the plan keeps only B.
- **B2 merges:**
  - the secret rules go to `CLAUDE.md`, and §8 goes to `docs/ideas/` with index entries;
  - this file is deleted, the "Active plans" entry removed, and inbound references swept.

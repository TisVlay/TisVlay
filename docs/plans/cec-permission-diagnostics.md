# CEC permission failures: typed reason payload end-to-end + discovery warning badge

## Context

When `tisvlayd` runs as a user that can *see* but not *open* `/dev/cecN` (device is `root:tisvlay 0660`; the daemon's user isn't in the `tisvlay` group, or `services.tisvlay.cec.enable` / the udev rule isn't applied), two failures surface, both opaque:

1. **Pairing fails uninformatively.** CEC `start_pairing` completes instantly (no auth); the real failure is the daemon's post-handshake `connect()` → `AsyncDevice::open("/dev/cec0")` → `EACCES`. `map_cec_err` (link.rs) flattens every unrecognized `linux_cec::Error` to `Error::backend_with_source(VENDOR_ID, "CEC error", errno)`, whose `Display` shows only `"CEC error"`. Verified live: `F_ERROR: "Connection failed for TV …: Backend error (cec): CEC error"`.
2. **Discovery hides the cause.** `CecDiscovery::discover` lists the node from `read_dir("/dev")` (no open), then best-effort opens each to probe identity. On `EACCES` the error is silently dropped (`discovery.rs:76-82` `let Ok(link) = … else { continue }`), leaving a bare `HDMI CEC (/dev/cec0)` row indistinguishable from "TV off" / "device busy".

The user chose the **most complete and rigorous** approach (architecture correctness over implementation time): a typed failure-reason payload that survives daemon→every client, plus a permission-only warning badge on the discovery row. This realizes the deferred [typed `Failed { reason }` payload](../ideas/typed-failed-reason-payload.md), of which the permission case is the first consumer.

**Guiding principle (transport-agnostic).** The failure reason is a **typed, serde-serializable value in `tisvlay-core`**; each transport owns only its *projection*. Today that projection is D-Bus `a{sv}`; a future Varlink (JSON) or Zenoh (CDR) transport re-projects the same core type without touching the vocabulary or structure — mirroring how `DiscoveredTv` (core struct) is already projected to a positional D-Bus tuple at the boundary. The reason **code** is a namespaced `domain.kebab` string (`device.permission-denied`), a transport-neutral semantic token (Varlink error names and Zenoh key-expressions are both dotted/hierarchical).

Enabling facts from exploration + review: `PairingState::Failed` is already typed (`Failed { error: PairingError { code: PairingErrorCode, message } }`), but the code is discarded at the `Box<dyn Error>` boxing sites and the terminal emit `("failed", e.to_string())` (tv_manager.rs:3491), and the wire is `sss`. The permission case specifically travels as `TvManagerError::ConnectionFailed { source: core::Error }` (a plain enum match, no downcast). In-repo precedents to mirror: `errors/power.rs` + `errors/config.rs` (Code enum + `as_str()` `domain.kebab` + `code()` bridge), the `PowerError`/`ConfigError` → `DaemonError` mapping (tisvlayd/src/dbus/mod.rs), the GUI `code → localized label` match (profiles.rs:246-267), and `DiscoveredTv`→tuple projection (dbus/manager.rs:274-287). No backwards-compat shims — all crates rebuild together; land as **one cohesive commit**.

## Architecture decisions (settled)

- **Wire = a typed `a{sv}` detail dict.** `PairingProgress(tv_id: s, phase: s, detail: a{sv})`. `detail` is a `tisvlay-core` struct deriving zbus `SerializeDict`/`DeserializeDict`/`Type` **and** serde `Serialize`/`Deserialize`. `Option` fields absent from the dict when unset → no empty-string sentinel; single migration to the idea-doc destination.
- **Namespaced code vocabulary.** `device.permission-denied`, `pairing.rejected`, etc. Retrofit `PairingErrorCode` to the `as_str()` → `pairing.<kebab>` shape (Display delegates; keep serde and `as_str()` consistent). Add `errors/device.rs` (`DeviceErrorCode::PermissionDenied` → `"device.permission-denied"`) to match the power/config domain pattern.
- **Badge = permission only** (`EACCES`/`EPERM`); other probe failures stay bare-but-logged.

## Implementation

### A. Core vocabulary + typed detail — `crates/tisvlay-core`

- `src/error.rs`: add `Error::PermissionDenied { path: String }`, `#[error("Permission denied accessing {path}")]` (sentence-case to match siblings).
- `src/errors/device.rs` (new, mirrors power.rs/config.rs): `DeviceErrorCode::PermissionDenied` with `as_str() -> "device.permission-denied"` + `Display`. Re-export from `errors/mod.rs`.
- `src/pairing.rs`: retrofit `PairingErrorCode` to `as_str() -> "pairing.<kebab>"` (`pairing.rejected`, `pairing.connection-failed`, …) with `Display` delegating; **keep serde `rename_all` consistent with `as_str()`** (the enum's serde form and its wire code must not diverge). `PairingErrorCode::PermissionDenied` is **not** added here — permission is a device concern; it projects to `device.permission-denied` (see C).
- **Typed detail struct** (new, in `pairing.rs`, feature-gated behind `dbus` since zvariant provides the derives): `PairingProgressDetail { message: String, code: Option<String>, path: Option<String> }` deriving `zbus::zvariant::SerializeDict + DeserializeDict + Type` (`signature = "a{sv}"`). `code`/`path` are `Some` only on the `failed` phase; `Option` fields are omitted from the dict when `None` (`skip_serializing_if`). This is the transport-agnostic value both D-Bus (`a{sv}`) and any future transport project.

### B. CEC backend — `crates/tisvlay-cec`

- `Cargo.toml`: add `nix = { version = "0.31", default-features = false, features = ["errno"] }` — **version-locked to linux-cec's `nix 0.31`** (the workspace also has `nix 0.30`; a mismatch makes `Errno` a different type and the match won't compile). Verify with `cargo tree -p tisvlay-cec -i nix` → only 0.31.x.
- `src/link.rs`: at `LinuxCecLink::open` (link.rs:175) and `LinuxCecMonitor::open` (link.rs:124), wrap the `AsyncDevice::open(...)` result specifically — on `linux_cec::Error::SystemError(Errno::EACCES | Errno::EPERM)` return `Error::PermissionDenied { path: device.to_string() }`; else `map_cec_err`. (`device` is in scope; don't touch the shared, path-less `map_cec_err` for the later ioctls.) Separately improve `map_cec_err`'s catch-all so `SystemError(errno)` renders the errno instead of the bare `"CEC error"` (log quality; keep `#[source]`).
- `src/discovery.rs` (discovery.rs:76-82): bind the probe-open `Err` instead of dropping it; `debug!(path, error)` on **every** failure arm (closes the observability gap); on `Err(Error::PermissionDenied { .. })` additionally `vendor_data.insert("access".to_string(), "permission_denied".to_string())` (free-form channel, no wire-signature change — same as `brand`). Other failures stay bare.
- `src/link.rs` test seam: extend `FakeCecLinkOpener` with a permission-denied `open` mode (mirrors `with_identity`/`with_monitor_disconnect`) for discovery + connect-failure tests.

### C. Daemon — `crates/tisvlayd`

- `src/tv_manager.rs`: add a `failure_reason(&TvManagerError) -> PairingProgressDetail`-style projection returning the namespaced `code` + optional `path` + `message`:
  - `ConnectionFailed { source: Error::PermissionDenied { path } }` → `code="device.permission-denied"`, `path=Some(path)` (plain enum match, no downcast).
  - `PairingFailed(boxed)` → `boxed.downcast_ref::<PairingError>().map(|pe| pe.code.as_str())` else `pairing.unknown` (the box holds heterogeneous types — direct `pe.code` won't compile).
  - `ConfigError(_)` / others → their namespaced code / `pairing.unknown`.
  Change `StateChangeEvent::PairingProgress` (tv_manager.rs:260-264) to carry the typed `detail` (or its fields) instead of `message: String`. Populate at the five emit constructors (tv_manager.rs:3493, 3702, 3713, 3908, 3957) — non-terminal phases set only `message` (from `PairingState::message()`), terminal `failed` sets `code`+`path`.
- `crates/tisvlay-cec/src/connector.rs` (wake path) — teach `wake_and_connect`'s retry loop (connector.rs:155) to short-circuit on `Error::PermissionDenied` the way it already does for `Error::ConnectionClosed`, so reconnect/wake/re-pair surface the permission reason instead of the generic "did not report power-on".
- `src/dbus/manager.rs` (manager.rs:888-893): change the `pairing_progress` signal to `(tv_id: &str, phase: &str, detail: PairingProgressDetail)` (encodes `a{sv}`).
- `src/state_forwarder.rs` (state_forwarder.rs:201-206): add `ref detail` to the destructure; pass through.
- `src/dbus/mod.rs`: add `DaemonError::PermissionDenied(String)` (`org.tisvlay.Error.PermissionDenied`) and a **match arm** in the existing `From<tisvlay_core::Error>` impl (mod.rs:185-205, before the `_ => OperationFailed` at :202) — the impl already exists; only the arm+variant are new. Covers the sync-RPC path.

### D. Client — `crates/tisvlay-core/src/dbus_client.rs`

- Proxy `pairing_progress` signal (dbus_client.rs:311): `detail: PairingProgressDetail`.
- `pairing_progress_stream` (dbus_client.rs:1487): yield `(tv_id, phase, detail)`.
- **Both** drain fns: `drain_pair_to_terminal` bound `S: Stream<Item=(String,String,PairingProgressDetail)>` (dbus_client.rs:1533) **and** `drain_pair_to_terminal_pinned` (1543-1565).
- `PairTerminal::Failed { tv_id, detail }` (carry the typed detail, not a bare message) (dbus_client.rs:88-96).
- Unit-test helpers (dbus_client.rs:3320-3326 `stream_from` + call sites) and the `PairTerminal::Failed { tv_id, message }` assertion (dbus_client.rs:3374) update to the new item/struct shape.

### E. Other stream consumers (all break on the shape change)

- `crates/tisvlay-tray/src/pairing.rs:40`: `while let Some((tv_id, phase, detail)) = stream.next()` — render `detail.message`; optionally bump the `device.permission-denied` notification to actionable/critical (nicety).
- `crates/tisvlay-harness/src/pairing.rs:40-42`: `drain_to_phase` bound + destructure to the 3-tuple with `detail`.
- `crates/tisvlay-cli/src/main.rs:319,345`: `Failed { detail, .. }` → print `detail.message` (already carries the English detail). Update `tests/pair_snapshot.rs` only if drained output changes.

### F. GUI — `crates/tisvlay-gui`

- **Pairing failure localization** (`src/wizard.rs`): `terminal_to_result` (wizard.rs:581-589) currently collapses `Failed { message, .. }` into an opaque `anyhow` string — change its contract so the typed `detail` (code+path+message) reaches `publish_pair_outcome` (wizard.rs:603-641). Add a `code → localized string` map (plain `gettext`, modeled on the `match` at profiles.rs:246-267 which uses `pgettext` — pairing needs no context, so plain `gettext`); render the localized primary text into `PairingWizard.pair-message` (globals/pairing_wizard.slint:99-100) with `pair-phase="failed"`, falling back to `detail.message` for unknown/empty codes. Keep the code-map lookup independent of and ahead of the existing `extract_invalid_vendor_config` field-routing branch (they don't collide — handshake failures don't carry the MethodError shape, wizard.rs:577-580). StepPair (pairing_wizard.slint:689-802) renders unchanged.
- **Discovery badge** (`src/wizard.rs` + `ui/`): in `WizardEntry::to_slint` (wizard.rs:42-59) read `vendor_data["access"]` alongside `brand` and set a new **mandatory** scalar on `WizardDiscoveredTv` (globals/pairing_wizard.slint:14-24) — e.g. `access-warning: bool` (the generated struct literal won't compile without initializing it). In `DiscoveryRow` (pairing_wizard.slint:181-202) append a trailing ⚠ affordance gated on the flag — a Unicode `⚠` `Text` (no `Icons` warning glyph exists) with a localized `@tr(...)` tooltip. **Tooltip trigger caveat:** the row's full-bleed `touch := TouchArea` (pairing_wizard.slint:206-208) is topmost, so a nested `HelpTrigger` gets no hover of its own — drive the tooltip from the row's own `touch.has-hover`, or add a dedicated overlay TouchArea declared *after* `touch`. Verify the chosen approach with the Slint MCP before finalizing.
- **i18n**: new strings — the `gettext` code→message map (Rust pass) and the `@tr` badge tooltip (Slint pass); run `just i18n-extract`, commit the refreshed `.pot`.

### G. Docs (required for consistency)

- `docs/design.md:283` (+ mentions ~210/245): update the `PairingProgress` signature to `(tv_id, phase, detail: a{sv})`.
- `.claude/CLAUDE.md:233` (Known Quirks): update the `Manager1.PairingProgress(tv_id, phase, message)` reference.
- `docs/cec/protocol.md` (Discovery ~82, Device identity ~76): the "falls back to the bare entry" sentences now split into permission-denied (badged, `vendor_data["access"]`) vs. everything-else (bare).
- `docs/ideas/typed-failed-reason-payload.md`: note the typed `a{sv}` detail landed here (first consumer); the remaining idea scope is the broader per-domain reason vocabulary if extended further.
- No code comment may reference this plan, a "Plan N", or the idea doc by number.

## Verification

- **Unit**: EACCES open → `Error::PermissionDenied` (link.rs, via permission `FakeCecLinkOpener`); `map_cec_err` errno-in-message; discovery sets `vendor_data["access"]="permission_denied"` on permission error and leaves+logs other failures bare; `failure_reason(ConnectionFailed→PermissionDenied)` → `device.permission-denied` with `path`; `PairingErrorCode::as_str()` namespaced strings; `PairingProgressDetail` round-trips through `a{sv}` and serde JSON (guards the transport-agnostic contract); drain surfaces the typed `detail`.
- **Daemon/harness**: a mock-backed / `tisvlay-harness` drain test asserting the `failed` phase carries `code="device.permission-denied"` end-to-end over the real signal (the layer the first draft omitted).
- **Repro (end-to-end)**: daemon reachable but `/dev/cec0` unopenable by its user → GUI wizard shows (a) the ⚠ badge + tooltip on the discovery row and (b) the localized permission message on StepPair (not `Backend error (cec): CEC error`); grant access → badge gone, pairing proceeds. Drive/inspect via Slint MCP + headless screenshot. `busctl --user monitor` shows `PairingProgress` carrying the `a{sv}` detail with `code`.
- **Suite**: `nix develop -c just check` + `prek run -a` green.

## Out of scope / noted extensions

- Recovery-action buttons gated on reason code (e.g. "Open CEC docs") — the typed payload enables it; not built now.
- Extending the namespaced reason vocabulary to every other `TvManagerError` beyond what pairing already surfaces (handshake-native `pairing.*`, `config.*`) — the projection is in place; broadening is incremental.

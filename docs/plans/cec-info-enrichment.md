# CEC INFO enrichment (live device identity)

Enrich the TVs-pane **INFO** group for a CEC-backed TV with the identity the set answers on the bus — OSD name, vendor, CEC version — and reflect the resolved vendor in the **VendorTile** branding. Unlike LG's static pair-time `scheme` diagnostic, CEC identity is **live**: re-plug the dongle into a different TV and INFO should follow. Grounded in a real `cec-ctl` dump of the C3 (`OSD Name 'OLED42C3PU'`, `Vendor ID 0x00e091 (LG)`, `CEC Version 1.4`) and confirmed against `linux-cec` 0.2 (`get_osd_name`, `get_vendor_id`, `GetCecVersion`).

> Status: **v2 built** on `feat/cec-backend` (PR [#94](https://github.com/ruffsl/TisVlay/pull/94)). Green: build · clippy `-D warnings` · 614 tests. CEC identity is folded into `HardwareId` (Make/Model) with a `Tv1.Brand` signal for the tile; discovery probes each node; identity queries are bounded. All four v2 slices landed as § Reconciliation v2 describes; the v1 `DeviceIdentity`/`VendorInfo` parallel path is fully removed. Durable content is in [docs/cec/protocol.md](../cec/protocol.md) § Device identity.

## What CEC exposes (and the verdict)

| Query (`linux-cec`) | Example | Use |
|---|---|---|
| `get_osd_name()` → `OsString` | `OLED42C3PU` | ✅ INFO row + pair-time name autofill (it's the model) |
| `get_vendor_id()` → `Option<VendorId>` | `00-e0-91` → LG | ✅ INFO row (name + raw) + VendorTile branding |
| `GetCecVersion` → `CecVersion` | `1.4` | ✅ INFO row (feature-level diagnostic) |
| Physical Address / Device Type | `0.0.0.0` / `TV` | ❌ low value — a TV is always bus root |

`linux-cec`'s `VendorId` is **only** the raw OUI (`Display` = `"00-e0-91"`); it has **no name table** (the `(LG)` in `cec-ctl` is v4l-utils' own map). So we own a small OUI→name map.

## Design

### Live identity, not a config snapshot

The `Diagnostic` model (LG `scheme`) bakes the value into `[tv.<vendor>]` at pair time — wrong here, since a re-plugged dongle would show a stale TV. Instead the identity **rides the live state pipeline**:

- **`TvState` gains an optional identity block** (typed: `osd_name`, `vendor` (raw OUI + resolved name + canonical id for the tile), `cec_version`) — merge-preserved like `inputs`, so power-only monitor deltas keep it.
- **The CEC connection queries it once per connection**, in its *initial* `subscribe_state` snapshot (not per push — the values are static for a given link). A **reconnect to a different TV** (unplug → disconnect → reconnect) brings a fresh initial snapshot → new identity → live INFO update.
- **The forwarder caches it** on `ManagedTv` and drives two D-Bus surfaces:
  - `Tv1.VendorInfo` — gains the identity as live `(label, value)` rows (INFO group renders it already; **~zero GUI change**). CEC thus shows *live* rows where LG's `scheme` is a static pair-time one — a nice demo that `VendorInfo` carries both.
  - A **brand signal** for the VendorTile (see below).

### OUI → name map (ours)

A small static table — `00-e0-91`→(`"LG"`, `"lg"`), plus Samsung / Sony / Panasonic / Vizio / TCL / Hisense / Philips … — yielding a **display name** and a **canonical id** (matching the VendorTile's `"lg"`/`"samsung"`/`"sony"` keys). Unknown OUIs resolve to name-absent. The INFO vendor row shows **both**: `LG (00-e0-91)`, or just `00-e0-91` when unresolved.

### VendorTile branding

`VendorTile` today keys on the *backend* vendor id (`root.tv.vendor`), so a CEC TV — vendor `"cec"` — falls through to the hardcoded `"MK"` fallback (reads like "mock"). Two changes:

- **Feed the tile the resolved *device* brand for CEC**: a new `brand-vendor` on the GUI `TvView` = the CEC-resolved canonical vendor (`"lg"`) when known, else the backend vendor id. So a CEC-connected LG set shows the **LG** tile (it *is* an LG TV, just driven over CEC).
- **Fix the fallback code**: `"cec"` → `"CE"`, keep `"mock"` → `"MK"`; unknown brands derive initials rather than always `"MK"`. So an *unresolved* CEC TV reads `CE`, not `MK`.

### Name autofill (pair-time)

In `build_config_from_pairing`, prefill the friendly-name field from the OSD name (`OLED42C3PU`) — captured once at pair time (the user can rename later, so it's deliberately *not* live).

## Slices

1. **CEC queries + OUI map.** `CecLink` gains `query_osd_name` / `query_vendor_id` / `query_cec_version` (+ `FakeCecLink` scripted returns); the static OUI→name map lives in the CEC crate.
2. **Live identity in state.** Typed identity block on `TvState` (merge-preserved); CEC populates it in the initial subscription snapshot; the forwarder caches it and folds it into `Tv1.VendorInfo` (live rows) + a `brand-vendor` surface. Mock parity for daemon-level tests.
3. **GUI.** INFO rows arrive via `VendorInfo` (no Slint change); wire `brand-vendor` into the two `VendorTile` usages; add the `"cec"`→`"CE"` fallback (and initials-derivation) to `VendorTile`.
4. **Name autofill.** `build_config_from_pairing` prefills the friendly name from the OSD name.

Plus tests (CEC unit: query round-trips, OUI resolution incl. unknown; daemon: identity flows to `VendorInfo` live and refreshes on reconnect to a different mock identity) and docs ([docs/cec/protocol.md](../cec/protocol.md) INFO/identity section; CLAUDE.md).

## Decisions

- **Identity typing — typed `DeviceIdentity`** (approved). A typed struct with optional fields (`osd_name`, `vendor` = raw OUI + resolved name + canonical id, `cec_version`) on `TvState`, not a loose `Vec<(String,String)>` — the VendorTile needs the canonical vendor id *separately* from the display rows, and typing keeps the merge/serialize surface explicit. The daemon derives the `VendorInfo` display rows from it.
- **Brand surface — small dedicated signal** (approved). Expose the resolved canonical vendor for the tile as its own field the GUI reads (`TvView.brand-vendor`, backed by a daemon-side value), rather than reverse-parsing a `VendorInfo` display row.
- **Query reliability.** Best-effort: a failed/absent query yields no row (elided) and no brand (tile falls back to `CE`). The C3 answered these *in Standby*, so pair-time and on-connect capture both work regardless of power.

## Reconciliation v2 — fold identity into `HardwareId` (planned)

Hardware QA of v1 exposed a design flaw: the INFO group already renders **Make** (`hw-manufacturer`), **Model** (`hw-model`), **Serial** from the cross-vendor [`HardwareId`](../../crates/tisvlay-core/src/hardware_id.rs) (LG populates it, refreshed on reconnect; the header also shows `hw-model` next to the descriptor). v1's parallel `DeviceIdentity` put a **Device name** (OSD name ≈ Model) and **Vendor** (≈ Make) row *right next to* the empty Make/Model rows — pure duplication. Also, v1 crashed the daemon on real hardware (`get_osd_name`/`get_vendor_id` read the *local adapter*, and the local OSD buffer is 14-byte NUL-padded → invalid D-Bus string → the bus dropped the daemon's name). The query-the-TV + sanitize fix landed (`a8a75be`); this section is the model reconciliation. Decisions (approved): **fold into `HardwareId`**, **drop the CEC version**, **probe during discovery**.

### Fold CEC identity into `HardwareId`

- CEC's `hardware_id()` returns `HardwareId { manufacturer: <resolved vendor name>, model: <OSD name>, serial: None }`, cached from a connect-time bus query (mirrors LG's `Arc<RwLock<Option<HardwareId>>>` refresh pattern — query async on connect, return sync). It flows through the *existing* `Tv1.HardwareId` → Make/Model rows + header `hw-model` (+ the deferred EDID-connector dedup). Refreshes on a dongle-swap via the GUI's `ConnectionChanged`-triggered re-fetch, like LG — no per-push liveness needed (identity only changes on reconnect).
- **Remove** the v1 parallel plumbing: `DeviceIdentity` on `TvState` (+ `display_rows`/`is_empty`/exports), `TvConnection::device_info()`, the `Tv1.VendorInfo` composition (revert to config-diagnostics only, undo the `vendor_diagnostics` rename + `device_identity` field + forwarder emit), the daemon pair-time blank→OSD autofill, and the mock `set_device_identity` override (tests use the existing `set_hardware_id`).
- **Drop the CEC version** — remove `query_cec_version` + `cec_version_str`; it doesn't fit Make/Model/Serial and was the least useful.

### Keep the vendor-tile branding

- The tile needs the canonical vendor (`lg`), which `HardwareId` doesn't carry. Keep `Tv1.Brand` + the GUI `brand` field + the `CE`/monogram fallback. Source it from a small **sync `TvConnection::brand() -> Option<String>`** (default `None`; CEC returns the cached canonical from the OUI map), so the daemon's `Brand` = `conn.brand()` else the backend vendor id — no `DeviceIdentity` needed.
- Keep the OUI→(name, canonical) map in `identity.rs`; it feeds both `manufacturer` (name) and `brand` (canonical). Keep the fixed, TV-querying `query_osd_name`/`query_vendor_id` (used by both the connection and discovery).

### Probe during discovery

- `CecDiscovery` opens each `/dev/cecN` and queries OSD name + vendor, populating `DiscoveredTv.name` (OSD name) + its `HardwareId`. So the wizard's discovery list shows real TV names/models and the name field prefills from the OSD name — the enrichment finally reaches [`pairing_wizard.slint`](../../crates/tisvlay-gui/ui/globals/pairing_wizard.slint). A non-answering node falls back to the bare `/dev/cecN` entry.
- **Wizard tile brand** (follow-up, `478433a`): the discovery tile keyed on `DiscoveredTv.vendor` (the backend id `cec` → `CE`), so a resolved LG set still showed the generic tile. Carry the canonical through `DiscoveredTv.vendor_data["brand"]` — the free-form map that already crosses D-Bus for autofill, so no `DiscoveredTv` wire-signature change — and key the wizard tile on it (falling back to the backend id). Mirrors the TVs-pane `Tv1.Brand`. `WizardDiscoveredTv` gains a `brand` field; `vendor` stays the backend id for pairing.

### Stall fix

- Bound the CEC identity queries with a short `IDENTITY_QUERY_TIMEOUT` (best-effort), **not** `Timeout::MAX` — so a TV that ignores `Give OSD Name` can't hang connect or discovery. (Power keeps `Timeout::MAX`; it's not best-effort.)

### v2 slices

1. **CEC → HardwareId + brand.** Connection queries on connect (bounded), caches `HardwareId` + canonical; `hardware_id()` + new `brand()`. Remove `device_info`, version.
2. **Unwind v1 core/daemon/mock.** Remove `DeviceIdentity`/`TvState` field, revert `VendorInfo`, drop pair-autofill + mock identity override. Daemon `Brand` from `conn.brand()`.
3. **Discovery probe.** `CecDiscovery` OSD/vendor probe → `DiscoveredTv`; verify the wizard shows/prefills.
4. **Tests + docs.** Daemon HardwareId via `set_hardware_id`; CEC-level brand + discovery-probe + timeout; refresh [docs/cec/protocol.md](../cec/protocol.md) (identity = HardwareId, not a separate block).

## Migration on merge

Fold the CEC-identity-is-`HardwareId` note into [docs/cec/protocol.md](../cec/protocol.md); delete this plan with the CEC PR. Per [docs/plans/README.md](README.md).

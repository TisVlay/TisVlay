# CEC INFO enrichment (live device identity)

Enrich the TVs-pane **INFO** group for a CEC-backed TV with the identity the set answers on the bus — OSD name, vendor, CEC version — and reflect the resolved vendor in the **VendorTile** branding. Unlike LG's static pair-time `scheme` diagnostic, CEC identity is **live**: re-plug the dongle into a different TV and INFO should follow. Grounded in a real `cec-ctl` dump of the C3 (`OSD Name 'OLED42C3PU'`, `Vendor ID 0x00e091 (LG)`, `CEC Version 1.4`) and confirmed against `linux-cec` 0.2 (`get_osd_name`, `get_vendor_id`, `GetCecVersion`).

> Status: **planned**, on `feat/cec-backend` (PR [#94](https://github.com/ruffsl/TisVlay/pull/94)). Baseline before implementing.

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

## Open questions / decisions

- **Identity typing.** A typed `DeviceIdentity` (optional fields) on `TvState` beats a loose `Vec<(String,String)>` because the VendorTile needs the canonical vendor id *separately* from the display rows. Wire cost is a few extra fields on the state property.
- **Brand surface.** Prefer a small dedicated signal (e.g. a `brand`/`device_vendor` the GUI reads) over parsing a `VendorInfo` display row for the tile.
- **Query reliability.** Best-effort: a failed/absent query yields no row (elided) and no brand (tile falls back to `CE`). The C3 answered these *in Standby*, so pair-time and on-connect capture both work regardless of power.

## Migration on merge

Fold the identity/`VendorInfo`-is-live-or-snapshot note into [docs/cec/protocol.md](../cec/protocol.md); delete this plan with the CEC PR. Per [docs/plans/README.md](README.md).

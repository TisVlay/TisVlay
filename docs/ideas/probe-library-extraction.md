# Probe library extraction (when GUI lands)

`crates/tisvlayd/src/desktop_probe.rs` walks `/sys/class/drm/`, parses each connector's EDID via `edidkit`, and matches managed TVs to physical connectors by `HardwareId`. The daemon owns the probe internally — it triggers on every `ConnectionChanged(true)` event (when a freshly-connected TV's `HardwareId` first becomes available) and on `Manager1.ReloadConfig`. The matching outcome is exposed via `Tv1.Desktop.Connector` on D-Bus.

When `tisvlay-gui` lands and wants richer EDID information (capability tables, manufacture date, vendor-specific extension blocks, HDR static metadata, supported modes, etc.) for use cases like "show the right vendor icon", "enable HDR-only setting choices", or "render a per-TV capabilities pane", the right move is a library extraction rather than growing the D-Bus surface:

- **Extract the probe primitives into a reusable library crate** (e.g. `tisvlay-probe`, or fold into `tisvlay-core` behind a feature flag if its deps stay narrow). Expose pure functions: `enumerate_connectors() -> Vec<ProbedConnector>`, `read_edid(connector_path) -> Result<Vec<u8>>`, `parse_edid(bytes) -> ParsedEdid` (full field surface — capabilities, manufacture date, vendor blocks), `parse_hardware_id(bytes) -> HardwareId` (the subset already used for matching).
- **`tisvlayd` keeps owning the matching responsibility** via the library. No behavioral change.
- **`tisvlay-gui` consumes the library directly** for on-demand introspection. Reads `Tv1.Desktop.Connector` from D-Bus to know which connector to point at, then calls the library locally to render rich detail.
- **D-Bus surface stays minimal**: matching outcome + identity essentials. Capabilities, mode lists, vendor blocks, manufacture metadata never flow through zbus.

### Why not push everything through D-Bus

D-Bus is a synchronization protocol for cross-process state — its contract is stable, hard to evolve, and every field needs a serialization decision. EDID parsing is data extraction over inputs every process can read independently from sysfs (no privilege required); pushing that through D-Bus duplicates information for no consistency benefit. The single source of truth that genuinely needs D-Bus is the *matching decision* — which TV is bound to which connector, given the live TV-side `HardwareId` from the daemon's connection. The daemon owns both halves; its result is published on `Tv1.Desktop.Connector` for other tooling. Raw EDID detail doesn't have the same single-source-of-truth need.

### Why not move probe into the library entirely

The matching decision is shared state with multiple consumers (tray, future GUI, future power-management adapters). Without a single source of truth, two consumers re-running the probe could disagree on the binding (especially in same-model multi-TV setups where iteration order matters). Keep the matching outcome on D-Bus, served by the daemon; let the library serve raw EDID parsing to whoever needs it.

### What this means concretely

- `Tv1.Desktop` interface stays minimal: `Connector` only.
- `Tv1.HardwareId` (already there) carries vendor-side identity for icons / pretty names.
- The library extraction is mechanical when GUI work starts (~250 LOC moved); no D-Bus API churn needed at that point.
- An EDID hash field on `DesktopMapping` was considered and rejected — see the discussion in [PR #34](https://github.com/TisVlay/TisVlay/pull/34) and the cull that followed.

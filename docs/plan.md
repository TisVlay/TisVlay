# tisvlay-gui plan

> **Status:** M1–M4 shipped; the per-TV editor is feature-complete with edit-mode (pencil → ✓/✗) wired through `Tv1.UpdateConfig` / `Tv1.GetConfig`, and the Profiles pane is feature-complete with property-bag-body CRUD over `Manager1` / `Session1` (see [docs/design.md § Profile catalog](design.md#profile-catalog)). M3.5's daemon surface ended up reshaped into the v3 property-bag wire surface rather than the name-keyed JSON sketch originally proposed. M5 (Session/About panes) is partial — About pane is built; Session pane is a stub. M6 (packaging + NixOS module flag) is partial — binary packaged; NixOS `enableGui` flag + `.desktop` entry remain.
>
> Several plans not on the original M1–M6 list landed alongside the milestones and shaped the resulting surface — see "Beyond the original roadmap" below for pointers to git-history of those (plans 2–6, all archived after their completion). The Profiles pane shipped via three plan iterations of its own — v1 (PR #58), v2 (PR #59), v3 (current); design notes archived in git history under `docs/plans/profile-crud-v3.md`.
>
> | Milestone | Status |
> |---|---|
> | M1 — Skeleton + read-only TV list | ✅ shipped |
> | M1.5 — `Tv1.Power.PowerCycleProgress` signal | ✅ shipped |
> | M2 — Per-TV editor + control surface | ✅ shipped + extended through Plan 6's edit-mode flow |
> | M3 — Pairing wizard + discovery streaming | ✅ shipped |
> | M3.5 — Profile CRUD + `ConfigReloadStatus` (daemon) | ✅ shipped as the v3 property-bag CRUD surface (see [design.md § Profile catalog](design.md#profile-catalog)) |
> | M4 — Profiles pane | ✅ shipped |
> | M5 — Session pane + About pane + polish | partial — About pane built; Session pane stub |
> | M6 — Packaging + distribution | partial — binary packaged; NixOS `enableGui` flag + `.desktop` entry pending |

Slint-based settings + management window for tisvlay. Sibling to `tisvlay-tray`. Pure-Rust, Linux-only, talks to the existing `tisvlayd` (system bus) and `tisvlay-session` (user bus) over D-Bus. No direct config-file writes — every mutation goes through the daemon, which owns its config dir and applies changes on the user's behalf (same pattern `AddTv` already uses).

## Crate layout

New crate at `crates/tisvlay-gui/`:

```
crates/tisvlay-gui/
├── Cargo.toml
├── build.rs                  # slint_build::compile("ui/main.slint")
├── src/
│   ├── main.rs               # tokio runtime + Slint event loop bridge
│   ├── daemon_state.rs       # background task: subscribes to D-Bus signals,
│   │                         # pushes mutations into Slint models
│   ├── pairing.rs            # pairing wizard logic (state machine over signals)
│   └── profiles.rs           # profile CRUD wrappers around D-Bus
├── ui/
│   ├── main.slint            # window root, sidebar nav
│   └── components/
│       ├── tvs_pane.slint
│       ├── tv_detail.slint
│       ├── profiles_pane.slint
│       ├── session_pane.slint
│       ├── about_pane.slint
│       └── pairing_wizard.slint
└── packaging/
    └── tisvlay-gui.desktop   # XDG desktop entry
```

Cargo.toml dependencies:

```toml
[dependencies]
slint = "1.16"
tisvlay-core = { workspace = true, features = ["dbus"] }
tokio = { workspace = true }
zbus = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
clap = { workspace = true }
i-slint-backend-selector = { version = "=1.16.1", features = ["mcp"], optional = true }

[features]
dev-mcp = ["dep:i-slint-backend-selector"]

[build-dependencies]
slint-build = "1.16"
```

`slint = "1.16"` is caret-implicit `^1.16` — catches `1.16.x` patches. `i-slint-backend-selector` is pinned to `=1.16.1` (does not follow semver per the slint skill, must match the resolved `slint` version exactly). Behind `dev-mcp` so production builds don't pull it.

## Bus model

Same dual-bus pattern as `tisvlay-cli` and `tisvlay-tray`. `--user` flag selects session bus for dev; default is system bus for production. The GUI holds two clients in parallel: a `tisvlay_core::DaemonClient` (Manager1 + per-TV Tv1.* interfaces on system bus) and a `SessionClient` (Session1 on user bus). Disconnect from either is non-fatal — the GUI shows a "daemon offline" / "session offline" badge in the About pane and disables the affected panes.

## Style

Default `fluent` (closest to KDE Breeze visually). Switchable at build time via `SLINT_STYLE` env var. Standard widget library only — no style-specific widgets, so `fluent` → `material` / `cosmic` / `cupertino` is a one-flag swap if we want to revisit later.

### Design tokens & theme global

The Claude Design agent produced a high-fidelity handoff at [.claude/ui/design_handoff_tisvlay_gui/](../.claude/ui/design_handoff_tisvlay_gui/) — interactive HTML prototype, static gallery, and reusable JSX primitives. The README in that folder is the source of truth for visual decisions; key tokens (`bg`, `surface`, `text`, `accent`, `success`/`danger`/`warn` palettes, `chip`, `row-hover` / `row-selected`, etc.) are ported to Slint as a single global at [crates/tisvlay-gui/ui/theme.slint](../crates/tisvlay-gui/ui/theme.slint).

`Theme` exposes one `dark-mode: bool` switch that branches every color property between the light and dark palettes. Components reference `Theme.text`, `Theme.surface`, `Theme.accent`, etc. directly — no inline colors. Flipping `Theme.dark-mode` re-tints the whole UI without component rebuilds.

**Vendor color tiles** (LG red `#a50034`, Samsung blue `#1428a0`, Sony black, mock chip-gray) are intentionally outside the theme system — they're decorative brand identity, not theme-derived. Defined inline in the `VendorTile` component for reuse from the tray and any future widgets.

The handoff ships its design as React/JSX prototypes; treat them as visual references, not implementation. Stick to stock `std-widgets` (Button, Switch, Slider, ComboBox, ScrollView, ListView) plus a small set of bespoke primitives (`Chip`, `VendorTile`, `Group`, `FormRow`, `LabeledRow`) — no exotic animations, no custom shaders.

## D-Bus surface

### Already exists (consumed by the GUI today)

`Manager1` (system bus):
- `DiscoverTvs(timeout_ms) -> Vec<(id, name, address, (manufacturer, model, serial), vendor, vendor_data)>` — `vendor_data` is the kernel-resolved MAC + any other autofill keys.
- `GetSupportedVendors() -> Vec<(vendor_id, vendor_name)>`
- `GetPairingSchema(vendor) -> json: s` — JSON-serialized `Vec<PairingField>` declaring the form fields the wizard renders dynamically.
- `AddTv(name, vendor, config_json, managed) -> (path, tv_id)` — schema-validated server-side via `PairingField::validate`. `tv_id` is a hyphenated UUID v4. Returns immediately after sync validation; the handshake runs in a daemon-side spawned task and outcome surfaces via `PairingProgress` (drain to terminal).
- `ReloadConfig()`
- `GetUserProfileDefault() -> json: s` — admin baseline user profile.
- `ListSystemProfiles() -> Vec<(name, json)>`
- `ListUserProfiles() -> Vec<(name, json)>` — admin baseline list.
- Signals: `PairingProgress(tv_id, phase, message)`, `DiscoveryFound(...)`.

Per-TV (`Tv1.*`):
- Read-only properties: `Uuid`, `Vendor`, `Connected`, `Capabilities`, `HardwareId`, `VendorConfig`, `VendorInfo`, `AutomateWhen`, `Automated`, plus the live state mirrors (`Power.PowerState`, `Power.SupportsWake`, `Volume.Volume`, `Volume.Muted`, `Backlight.Backlight`, `Screen.IsScreenOn`, `Input.CurrentInput`, `Input.AvailableInputs`, `Desktop.Connector`).
- Writable properties (single-field flips via `Properties.Set`): `Tv1.{Name, Managed, SystemProfile, PrimaryInput}`, plus the live-state writables (`Volume.Volume`, `Volume.Muted`, `Screen.IsScreenOn`, `Backlight.Backlight`).
- Methods: `Tv1.Reconnect`, `Tv1.Remove`, `Tv1.GetConfig`, `Tv1.UpdateConfig(json_patch)`, `Tv1.Power.PowerOn` / `PowerOff` (with `PowerCycleProgress(phase)` signal), `Tv1.Input.SwitchInput`.

`Session1` (user bus):
- `ListUserProfiles()` — merged admin + user.
- `GetTvUserProfile(tv_id) -> profile_name: s`.
- `SetTvUserProfile(tv_id, profile_name)`.
- `GetEffectiveUserProfile(tv_id) -> json: s` — fully resolved.
- `ReloadConfig()`.
- Signal: `UserProfileChanged(tv_id, profile_name)`.

The session service also reactively prunes orphan `[[tv]]` user-profile assignments from its on-disk user config when the daemon's `ObjectManager.InterfacesRemoved` fires (i.e. after a TV is removed via `Tv1.Remove`). No GUI work needed for that surface.

### Profile CRUD surface (shipped in v3)

The Manager1 + Session1 profile-CRUD surface shipped through three iterations (PR #58 / #59 / current). Final shape is documented in [docs/design.md § Profile catalog](design.md#profile-catalog):

- Identity is UUID, not name. Eight CRUD methods per bus (`List` / `Get` / `Update` / `Create` / `Duplicate` / `Delete` / `GetResolvedDefaults` / `GetEffective`).
- Bodies ride as `a{sv}` property bags via `#[derive(SerializeDict, DeserializeDict)]`, not JSON strings. `Option<T>::None` ⇔ absent key ⇔ "inherits from the chain below".
- Topology signals split into `*Added(uuid)` + `*Removed(uuid)`; body mutations fire `*Changed(uuid)`. Three signals per bus.
- Rename folds into `Update` with a new `name` in the body — no dedicated method.
- `Duplicate` is a daemon method (mints a new UUID under one write lock), not a client-side `Get` + `Create`.
- `ReloadConfig` emits the same per-UUID signals an external client would, so config-file edits are observationally identical to client mutations.

The original M3.5 sketch (name-keyed identity, JSON-string bodies, `ConfigReloadStatus` signal) is preserved here as the v1 attempt's shape. v3's prose iteration history lives in git under the deleted `docs/plans/profile-crud-v3.md` plan.

## Screens

Single window, sidebar-nav layout. Nav items: TVs, Profiles, Session, About.

### TVs pane (default landing)

Master-detail layout. Left: list of TVs with status icon (connected / disconnected / pairing-in-progress). Right: detail for the selected TV.

Detail tab includes:

- **Header:** TV name (also editable in the CONFIG group), connection status chip, vendor tile, primary descriptor (first `Tv1.VendorConfig` entry — LG: IP).
- **Live state controls:** Power button (uses `PowerCycleProgress` for staged feedback), screen on/off toggle, volume slider + mute toggle, backlight slider, current input dropdown populated from `Input.AvailableInputs`.
- **Inline configuration (single-field flips via `Properties.Set`):**
  - Managed checkbox → `Tv1.Managed`
  - System profile dropdown → `Tv1.SystemProfile`
  - User profile dropdown → `Session1.SetTvUserProfile(tv_id, name)`
  - Primary input dropdown → `Tv1.PrimaryInput`
- **CONFIG group (batch edit via `Tv1.UpdateConfig`):** address-book pencil-toggle UX. View mode shows name + `Tv1.VendorConfig` rows as read-only labels; click pencil → friendly-name input + schema-driven form for the vendor `UserInput` fields (LG: IP, MAC). ✓/✗ icon-buttons commit or discard; ✓ swaps in a `SpinnerArc` while the save is in flight.
- **INFO group (read-only):** TV ID (UUID), hardware identity (manufacturer / model / serial), DRM connector, vendor diagnostic rows from `Tv1.VendorInfo` (LG: WebSocket scheme — refreshed only on re-pair).

Action buttons in the TVs pane footer: **Add TV** (opens pairing wizard), **Remove TV** (confirm dialog → `Tv1.Remove`), **Reload Config** (calls both `Manager1.ReloadConfig` and `Session1.ReloadConfig`).

### Profiles pane

Two sub-tabs: **System Profiles** and **User Profiles**. Both sub-tabs have the same shape: list on the left, editor on the right.

List actions: **New** (opens editor with empty form), **Duplicate** (Get-then-Create on the daemon, prefills editor with copy named `<orig>-copy`), **Delete** (confirm dialog, blocked if any TV references it).

Editor surfaces every field on `SystemProfileConfig` / `UserProfileConfig`. Strongly-typed inputs:

- Enum-valued fields (`on_disconnect`, `on_session_lock`, etc.) → comboboxes populated from a hard-coded list of valid actions (`poweroff`, `screen_off`, `screen_on`, `none`, plus any others enumerated in config.rs).
- Optional fields → checkbox-gated input ("Inherit from default" unchecked → input enabled).
- Numeric fields (`idle_timeout_seconds`) → number spinner with min/max validation.

Save button serializes form state to JSON (preserving `null` for unchecked inheritable fields), calls `UpdateSystemProfile` or `UpdateUserProfile`. Validation errors surface inline.

### Session pane

User-bus Session1 client status (running / disconnected / restart button). Default user-profile selector (calls a future `Session1.SetUserProfileDefault` — flag in M5 if not already present, otherwise via existing surface). Future home for the auto-start-flow controls when that work lands.

### About pane

Version, license, links to project page and bug tracker, daemon connection status (system bus), session connection status (user bus). Diagnostic copy-to-clipboard button that emits a JSON blob of `{ daemon_version, session_version, slint_version, gui_version }` for bug reports.

## Pairing wizard

Modal flow, four steps:

1. **Discover.** Calls `Manager1.DiscoverTvs(timeout_ms=5000)` and concurrently subscribes to `Manager1.DiscoveryFound` signals. Results stream into a list as the daemon emits them. Manual-entry fallback at the bottom: vendor dropdown + IP textbox for users on networks where SSDP is blocked.
2. **Configure.** Selected TV pre-fills name (from discovery) and vendor; user can override name, set managed checkbox, choose initial system profile.
3. **Pair.** Subscribes to `Manager1.PairingProgress(tv_id, phase)` first, calls `Manager1.AddTv(name, vendor, config_json, managed)` (returns the reserved `tv_id` immediately after sync validation), then drains the filtered stream to a terminal phase via `DaemonClient::drain_pair_to_terminal`. UI shows phase-by-phase: `awaiting_tv` ("Confirm prompt on TV…") → `awaiting_input` (when the vendor needs a PIN, etc.) → `complete` / `failed`. Cancel button calls `Manager1.CancelPair(tv_id)` to abort the daemon-side handshake.
4. **Done.** Wizard closes; new TV is selected in the TVs list.

## State sync model

Initial load: `ObjectManager.GetManagedObjects` for the full TV tree (single round-trip), `Manager1.ListSystemProfiles`, `Session1.ListUserProfiles`, `Session1.GetEffectiveUserProfile` per TV.

Live updates: a tokio task subscribes to `org.freedesktop.DBus.Properties.PropertiesChanged` signals and the per-pane signals (`TvAdded`/`TvRemoved`/`PairingProgress`/`DiscoveryFound`/`PowerCycleProgress`/`ConfigReloadStatus`/`UserProfileChanged`/`UserProfileListChanged`/`SystemProfileChanged`).

Slint side: the GUI holds Slint `Model<T>`s for the TV list, profile list, and per-TV detail. Mutations from the tokio side land via `slint::invoke_from_event_loop` so all UI mutations execute on the Slint event-loop thread. This is the canonical pattern; the slint skill confirms it.

## Slint dev workflow

```sh
SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315 cargo run -p tisvlay-gui --features dev-mcp --user
```

While the GUI is running with `dev-mcp`, an MCP client at `http://localhost:9315/mcp` can inspect the live UI tree, take screenshots, click elements, and verify layout. Production builds drop the `dev-mcp` feature so no MCP server ships in the binary.

`SLINT_BACKEND=winit-skia` forces software rendering for screenshot determinism. `SLINT_DEBUG_PERFORMANCE=refresh_lazy,console` for perf diagnostics.

## Milestones

Each milestone is one PR. Sequencing reflects dependency order (M1.5 must land before M2 wires Power-on UI; M3.5 before M4 has anything to consume).

### ✅ M1 — Skeleton + read-only TV list

Crate scaffolded: `tisvlay-gui` builds, launches a window with the four-pane sidebar. TVs pane shows the TV list from `GetManagedObjects` and renders a static detail view from cached state. No edits, no pairing.

Live updates: subscribe to `PropertiesChanged` and per-TV state mirrors update in real time.

`dev-mcp` feature wired so the UI can be iterated with MCP introspection. No new daemon surface.

### ✅ M1.5 — `Tv1.Power.PowerCycleProgress` signal (daemon)

Daemon-side only. Signal emitted from the `PowerOn` handler around the WoL → connect path. `dbus_client.rs` proxy + `tisvlay-cli` log integration shipped alongside.

### ✅ M2 — Per-TV editor + control surface

Wired writable property setters: volume / backlight / mute / screen / input switcher. Per-property setters for managed / name / system-profile / primary-input via `Tv1.{Managed, Name, SystemProfile, PrimaryInput}` (the original `SetTvOption` dispatcher was retired in favor of these — see Plan 6 phase 8.4). User-profile assignment via `Session1.SetTvUserProfile`. Power button uses M1.5's `PowerCycleProgress`. Reload Config button calls both daemon and session reloads. Remove TV button → `Tv1.Remove` (per-object, post-Plan-6 architectural pivot).

Beyond the original M2 scope: the **edit-mode pencil toggle** in the CONFIG group landed during Plan 6's Phase 8.5, batching name + vendor `UserInput` fields through `Tv1.UpdateConfig` for atomic edits.

### ✅ M3 — Pairing wizard + discovery streaming

Daemon-side: `Manager1.DiscoveryFound` + `Manager1.PairingProgress` signals shipped. ARP-resolved MAC enrichment of discovery results landed alongside (so the wizard's MAC field auto-fills from a kernel neighbor lookup, not a separate user input step).

GUI-side: four-step pairing wizard component (Discover → Configure → Pair → Done) with schema-driven step-2 form. The schema-driven path (Plan 5 Phase 2) supplanted the hard-coded LG-only fields originally sketched here — every field comes from `Manager1.GetPairingSchema(vendor)`, and the wizard stays vendor-agnostic.

### ✅ M3.5 — Profile CRUD (daemon)

Shipped as the v3 property-bag CRUD surface — see [docs/design.md § Profile catalog](design.md#profile-catalog) for the durable architecture. Two prior attempts (PR #58 whole-body JSON, PR #59 per-property typed properties) shaped the final design; v3's iteration history lives in git under the deleted `docs/plans/profile-crud-v3.md`. The `ConfigReloadStatus` signal sketch was superseded by per-profile `*Added` / `*Changed` / `*Removed` signals emitted from `ReloadConfig` — a config-file edit now looks identical on the wire to an external client mutation.

### ✅ M4 — Profiles pane

Shipped. Master/detail layout with stacked System / User sections (single pane, not sub-tabs as originally sketched). Create / Rename / Delete / Duplicate flows; per-field inheritance editor with locally-set indicator (accent gutter) + per-field reset glyph + reset-all confirm; IdleTimeoutSeconds spinner; Used-by chip ticker with click-through to TVs pane; instant-commit edits (one D-Bus round-trip per field) instead of the originally-sketched Save button. The editor's three-layer `ProfileLayers` cache + signal-driven row refresh is the perf-critical piece v1 (PR #58) lacked.

### M5 — Session pane + About pane + polish (partial — About done)

About pane shipped end-to-end on `feat/gui-about` + `feat/gui-about-2` (former plan 2): peer-introspection (Daemon/Session/Tray/GUI uniformly expose `Version` + `ConfigPath`), KDE HIG cursor compliance + `UriBtn` rename, COMPONENTS table consolidation with file-manager folder buttons, version-drift surface, AboutSlint widget for license compliance, third-party-attribution pipeline (cargo-about + cargo-deny + browser handoff for the bundle). Session pane is still a placeholder ("session-service status arrives in M5"). Settings pane (a fifth nav item that emerged after the original roadmap) is also a placeholder. Polish items (toasts, keyboard nav, window-state persistence) are incremental as the panes get fleshed out.

### M6 — Packaging + distribution (partial)

The `tisvlay-gui` binary is now packaged — see [packaging/README.md](../packaging/README.md). Remaining: NixOS module's `services.tisvlay.enableGui` flag and the XDG `.desktop` entry.

### Beyond the original roadmap

Several plans not on the M1–M6 list shipped alongside the milestones above. Each had its own plan doc that was archived after completion; follow the commit messages for the implementation arc.

| Topic | Driving work | Key commits |
|---|---|---|
| Coalescer factor-out for scroll dispatch | (former plan, archived in git history) | `Coalescer<K, V>` lives in `tisvlay-core` and is reused by both the tray's volume/backlight scroll and the GUI's optimistic slider |
| D-Bus refactor: read-only properties + typed Set methods for live state | (former plan 3) | commit `87714ad` ("`refactor(daemon): writable state via methods, properties read-only`"); rationale captured in Plan 6's "SetTvOption retirement" research |
| Slint architecture sweep: Daemon global, theme tokens, computed bindings, animations | (former plan 4) | spans many commits; established the per-pane component layout, the `Theme` global, the address-book contact pattern that Plan 6's edit pencil reuses |
| Power-cycle robustness, MAC capture, vendor-agnostic config schema | (former plan 5) | `7566f0b` (declarative config schema with `FieldVisibility` metadata), `f50df5a` (schema-driven pairing wizard), `da9f2a6` (ARP-resolved MAC enrichment), `f18cd40` (smart Power button + connect-first PowerOn) |
| Edit TV properties: `Tv1.UpdateConfig` / `GetConfig`, edit-mode pencil toggle, `VendorConfig`/`VendorInfo` split | (former plan 6) | `b55d8d9` (Tv1.UpdateConfig + Tv1.GetConfig), `7ae48c6` (edit-mode toggle), `a0f9f26` (VendorConfig/VendorInfo split); per-object `Tv1.Remove` (`2837098`) replaced `Manager1.RemoveTv` along the way |
| About pane parity + peer introspection + KDE HIG + FOSS attribution | (former plan 2, retired in this commit) | shipped on `feat/gui-about` (Commit A: design-handoff parity, AboutSlint widget, peer-introspection symmetry, COMPONENTS-table consolidation, file-manager folder buttons) and `feat/gui-about-2` (B1: cargo-about + cargo-deny scaffolding under `packaging/licensing/`; B2: build-time attribution embed + browser handoff). Maintenance reference: [docs/licensing.md](licensing.md) |

## Open follow-ups (post v1)

These are explicitly out of scope for v1 GUI but worth tracking:

- **Auto-start UX research.** When user installs tisvlay and runs the GUI for the first time, should the GUI auto-start the session service + tray? See [docs/ideas.md "User service auto-start after `nixos-rebuild switch`"](ideas.md). Reference: NordVPN's desktop client. Decision blocked on understanding the systemd-user / D-Bus-activation idiom; revisit after v1 ships.
- **EDID detail panel.** Per [docs/ideas.md "Probe library extraction (when GUI lands)"](ideas.md), the GUI may want richer EDID detail (capability tables, manufacture date, vendor blocks). Requires extracting `tisvlayd::desktop_probe` into a `tisvlay-probe` library crate and consuming it from the GUI directly (not via D-Bus). Defer until v1 ships and we know what the panes actually need.
- **Polkit gating.** Current trust model is "any local user with D-Bus access can mutate config." Fine for single-user desktops (the typical tisvlay deployment). For multi-user shared workstations, polkit is the canonical Linux gate. When this becomes a concern, add `polkit::AuthorityProxy::check_authorization` to each privileged Manager1 method handler and ship `org.tisvlay.policy.xml` declaring action IDs.
- **Profile rename.** Skipped in v1 because it requires walking every TV's profile assignment to rewrite atomically. If user feedback wants it, the simplest path is a daemon-side `RenameSystemProfile(old, new)` / `RenameUserProfile(old, new)` that does the assignment-rewrite under an internal lock.
- **Per-DE event-driven power management.** Tracked in [docs/ideas.md](ideas.md). Independent of the GUI, but the Session pane is a natural future home for any user-facing toggles that work surfaces.

## Trust model note

The plan operates under the current "open D-Bus + daemon-owned config" model. Any local user can call any Manager1 method via the system bus; the daemon does the privileged work as the dedicated `tisvlay` system user. This matches the existing AddTv / SetTvOption pattern. If multi-user gating becomes necessary later, polkit adds it incrementally without GUI-side changes.

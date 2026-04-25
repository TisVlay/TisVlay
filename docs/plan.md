# tisvlay-gui plan

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

### Already exists (consumed unchanged)

`Manager1` (system bus):
- `DiscoverTvs(timeout_ms) -> Vec<(id, name, address, (manufacturer, model, serial), vendor)>`
- `GetSupportedVendors() -> Vec<(vendor_id, vendor_name)>`
- `AddTv(name, vendor, config_json, managed) -> (path, tv_id)`
- `RemoveTv(tv_id)`
- `ReloadConfig()`
- `SetTvOption(tv_id, key, value)` — for `managed`, `name`, `profile.system`, `primary_input`
- `GetUserProfileDefault() -> name`
- `ListSystemProfiles() -> Vec<(name, json)>`
- `ListUserProfiles() -> Vec<(name, json)>`  (admin baseline)
- Signals: `TvAdded(path)`, `TvRemoved(path)`, `ConfigReloaded`

Per-TV (`Tv1.*`):
- All current properties (`Connected`, `HardwareId`, `Power.PowerState`, `Volume.Volume`/`Muted`, `Backlight.Backlight`, `Input.{CurrentInput, AvailableInputs}`, `Screen.IsScreenOn`, `Desktop.Connector`, etc.)
- All current methods (`Power.PowerOff`/`PowerOn`, `Input.SwitchInput`)
- All current writeable properties (`Volume.Volume`, `Muted`, `Screen.IsScreenOn`, `Backlight.Backlight`)

`Session1` (user bus):
- `ListUserProfiles()` — merged admin + user
- `GetTvUserProfile(tv_id)`
- `SetTvUserProfile(tv_id, profile_name)`
- `GetEffectiveUserProfile(tv_id)` — fully resolved JSON
- `ReloadConfig()`
- Signal: `UserProfileChanged(tv_id, profile_name)`

### New surface added by this work

**M1.5 daemon-side additions:**

- `Tv1.Power.PowerCycleProgress(phase: s)` signal. Phases: `wol_sent`, `connecting`, `connected`, `failed`. Emitted during `PowerOn` so the GUI can show staged progress.

**M3 daemon-side additions:**

- `Manager1.PairingProgress(tv_id: s, phase: s)` signal. Phases: `prompting`, `registering`, `complete`, `failed`. Emitted during `AddTv`.
- `Manager1.DiscoveryFound(tv_info: (sssss(sss)))` signal. Emitted per-TV during `DiscoverTvs` so the GUI streams results instead of waiting for the timeout. Existing `DiscoverTvs` RPC still returns the final list for non-streaming consumers (CLI).

**M3.5 daemon-side CRUD additions:**

`Manager1` (system bus):
- `CreateSystemProfile(name: s, json: s) -> ()` — fails if name exists
- `UpdateSystemProfile(name: s, json: s) -> ()` — fails if name doesn't exist
- `DeleteSystemProfile(name: s) -> ()` — fails if `name == "default"` or any TV references it
- `GetSystemProfile(name: s) -> json: s` — returns the unmerged `SystemProfileConfig` (with `Option<T>` fields preserved as JSON `null`s) so the GUI edits the profile as written, not as resolved
- `ConfigReloadStatus(success: b, error_msg: s)` signal — fired after `ReloadConfig` completes

`Session1` (user bus):
- `CreateUserProfile(name: s, json: s) -> ()`
- `UpdateUserProfile(name: s, json: s) -> ()`
- `DeleteUserProfile(name: s) -> ()` — fails if `name == "default"` (the merged default) or any TV assigns it
- `GetUserProfile(name: s) -> json: s` — returns the user-config profile (unmerged), or the admin baseline if not in user config
- `UserProfileListChanged(name: s, kind: s)` signal — kind ∈ `created`, `updated`, `deleted`. Distinct from existing per-TV `UserProfileChanged`.

Clone is **not** a daemon method — the GUI does `Get` + `Create` client-side. Rename is **not** in v1.

## Screens

Single window, sidebar-nav layout. Nav items: TVs, Profiles, Session, About.

### TVs pane (default landing)

Master-detail layout. Left: list of TVs with status icon (connected / disconnected / pairing-in-progress). Right: detail for the selected TV.

Detail tab includes:

- **Header:** TV name (editable), connection status badge, vendor, address.
- **Live state controls:** Power button (uses `PowerCycleProgress` for staged feedback), screen on/off toggle, volume slider + mute toggle, backlight slider, current input dropdown populated from `Input.AvailableInputs`.
- **Configuration:**
  - Managed checkbox → `SetTvOption(tv_id, "managed", "true"/"false")`
  - System profile dropdown → `SetTvOption(tv_id, "profile.system", name)`
  - User profile dropdown → `Session1.SetTvUserProfile(tv_id, name)`
  - Primary input dropdown → `SetTvOption(tv_id, "primary_input", id)`
- **Hardware identity:** read-only display of `HardwareId` (manufacturer / model / serial) and `Desktop.Connector`.

Action buttons in the TVs pane footer: **Add TV** (opens pairing wizard), **Remove TV** (confirm dialog), **Reload Config** (calls both `Manager1.ReloadConfig` and `Session1.ReloadConfig`).

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
3. **Pair.** Calls `Manager1.AddTv(name, vendor, config_json, managed)`, subscribes to `Manager1.PairingProgress(tv_id, phase)`. UI shows phase-by-phase: "Confirm prompt on TV…" (`prompting`) → "Registering…" (`registering`) → "Complete" or "Failed: <reason>". Cancel button drops the call (best-effort; the daemon may already be mid-handshake).
4. **Done.** Wizard closes; new TV is selected in the TVs list.

## State sync model

Initial load: `ObjectManager.GetManagedObjects` for the full TV tree (single round-trip), `Manager1.ListSystemProfiles`, `Session1.ListUserProfiles`, `Session1.GetEffectiveUserProfile` per TV.

Live updates: a tokio task subscribes to `org.freedesktop.DBus.Properties.PropertiesChanged` signals and the per-pane signals (`TvAdded`/`TvRemoved`/`ConfigReloaded`/`PairingProgress`/`DiscoveryFound`/`PowerCycleProgress`/`ConfigReloadStatus`/`UserProfileChanged`/`UserProfileListChanged`/`SystemProfileChanged`).

Slint side: the GUI holds Slint `Model<T>`s for the TV list, profile list, and per-TV detail. Mutations from the tokio side land via `slint::invoke_from_event_loop` so all UI mutations execute on the Slint event-loop thread. This is the canonical pattern; the slint skill confirms it.

## Slint dev workflow

```sh
SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315 cargo run -p tisvlay-gui --features dev-mcp --user
```

While the GUI is running with `dev-mcp`, an MCP client at `http://localhost:9315/mcp` can inspect the live UI tree, take screenshots, click elements, and verify layout. Production builds drop the `dev-mcp` feature so no MCP server ships in the binary.

`SLINT_BACKEND=winit-skia` forces software rendering for screenshot determinism. `SLINT_DEBUG_PERFORMANCE=refresh_lazy,console` for perf diagnostics.

## Milestones

Each milestone is one PR. Sequencing reflects dependency order (M1.5 must land before M2 wires Power-on UI; M3.5 before M4 has anything to consume).

### M1 — Skeleton + read-only TV list

Crate scaffolded: `tisvlay-gui` builds, launches a window with the four-pane sidebar. TVs pane shows the TV list from `GetManagedObjects` and renders a static detail view from cached state. No edits, no pairing.

Live updates: subscribe to `PropertiesChanged` and per-TV state mirrors update in real time.

`dev-mcp` feature wired so I can iterate UI with MCP introspection. No new daemon surface.

Acceptance: launches against a running `tisvlayd`, shows current TVs with live volume/input/power-state. Builds pass `just check`.

### M1.5 — `Tv1.Power.PowerCycleProgress` signal (daemon)

Daemon-side only, no GUI work. Add the signal and emit it from the existing `PowerOn` handler around the WoL → connect path. Update the `dbus_client.rs` proxy. Extends `tisvlay-cli` `power on` to log the phases for verification. PR-sized: ~50-100 LOC.

Acceptance: `busctl --system monitor` shows phase transitions during `PowerOn` on a sleeping TV. CLI logs match.

### M2 — Per-TV editor + control surface

Wire writeable property setters in the TVs pane: volume slider, backlight slider, mute toggle, screen-on toggle, input switcher. Wire `SetTvOption` for managed / name / system-profile assignment. Wire `Session1.SetTvUserProfile` for user-profile assignment. Power button uses M1.5 `PowerCycleProgress` for staged feedback. Reload Config button calls both daemon and session reloads. Remove TV button with confirm dialog.

Acceptance: every TV-detail control round-trips through the daemon. Live state stays in sync with externally-driven changes (CLI changes are visible in the GUI without reload).

### M3 — Pairing wizard + discovery streaming

Daemon-side: add `Manager1.DiscoveryFound` and `Manager1.PairingProgress` signals. Wire emission into existing `DiscoverTvs` and `AddTv` paths.

GUI-side: pairing wizard component implementing the four-step flow above. Streams discovery results, shows pairing phase progress.

Acceptance: pairing a fresh LG TV via the GUI succeeds end-to-end and matches the CLI experience for time-to-pair. Discovery results visibly stream rather than appearing all at once.

### M3.5 — Profile CRUD + ConfigReloadStatus (daemon)

Daemon-side only, no GUI work. Add the eight new D-Bus methods (`Create/Update/Delete/Get` for system + user profiles), the two new signals (`SystemProfileChanged`, `UserProfileListChanged`, `ConfigReloadStatus`), and the corresponding proxies in `dbus_client.rs`. Validation: profile names match `[a-z0-9-]+`; "default" is undeleteable; deletion blocked when any TV references the profile.

Tests: unit tests for the validation rules; integration tests with a running daemon for the round-trip.

Acceptance: `tisvlay-cli profile create foo --json '...'` and `tisvlay-cli profile delete foo` work via the new D-Bus methods. CLI gains `profile {create,update,delete,get}` subcommands as a side benefit.

### M4 — Profiles pane

Consumes M3.5. Two sub-tabs (System / User), list + editor layout. Strongly-typed form inputs as described above. Duplicate button is client-side compose. Delete with confirm dialog. New profile creation. Editor changes the active profile in real time when `SystemProfileChanged` / `UserProfileListChanged` signals arrive (warn if the form has unsaved local edits).

Acceptance: can create, edit, duplicate, delete profiles end-to-end. Every field on `SystemProfileConfig` / `UserProfileConfig` is reachable in the form.

### M5 — Session pane + About pane + polish

Session pane: user-bus connection indicator, session-service status, restart button (calls `systemctl --user restart tisvlay-session` via `org.freedesktop.systemd1`). Default user-profile selector.

About pane: version info, links, license, diagnostic-copy button.

Polish: error toasts for failed D-Bus calls, keyboard navigation (Tab order, Enter-to-confirm in dialogs), window-state persistence (size + last-selected TV via XDG state dir).

### M6 — Packaging + distribution

NixOS module: `services.tisvlay.enableGui` option (default `false` initially; flip to `true` after a stabilization period). Adds the GUI binary and desktop entry to system PATH.

`cargo-nfpm` `[package.metadata.nfpm.contents]` adds the GUI binary, desktop entry, icon. Same package as `tisvlay` (no separate `tisvlay-gui` package).

XDG desktop entry: `tisvlay-gui.desktop` with `Exec=tisvlay-gui`, category `Settings;HardwareSettings;`, icon. Auto-discovered by KDE / GNOME / XFCE app menus.

Optional systemd user service: not auto-started; user launches from app menu or shell. Auto-start-after-install flow is explicitly **out of scope for v1** — see follow-ups.

Acceptance: installs cleanly via `nix flake check` VM test. Desktop entry shows up in app menu. `which tisvlay-gui` returns `/usr/bin/tisvlay-gui` after install.

## Open follow-ups (post v1)

These are explicitly out of scope for v1 GUI but worth tracking:

- **Auto-start UX research.** When user installs tisvlay and runs the GUI for the first time, should the GUI auto-start the session service + tray? See [docs/ideas.md "User service auto-start after `nixos-rebuild switch`"](ideas.md). Reference: NordVPN's desktop client. Decision blocked on understanding the systemd-user / D-Bus-activation idiom; revisit after v1 ships.
- **EDID detail panel.** Per [docs/ideas.md "Probe library extraction (when GUI lands)"](ideas.md), the GUI may want richer EDID detail (capability tables, manufacture date, vendor blocks). Requires extracting `tisvlayd::desktop_probe` into a `tisvlay-probe` library crate and consuming it from the GUI directly (not via D-Bus). Defer until v1 ships and we know what the panes actually need.
- **Polkit gating.** Current trust model is "any local user with D-Bus access can mutate config." Fine for single-user desktops (the typical tisvlay deployment). For multi-user shared workstations, polkit is the canonical Linux gate. When this becomes a concern, add `polkit::AuthorityProxy::check_authorization` to each privileged Manager1 method handler and ship `org.tisvlay.policy.xml` declaring action IDs.
- **Profile rename.** Skipped in v1 because it requires walking every TV's profile assignment to rewrite atomically. If user feedback wants it, the simplest path is a daemon-side `RenameSystemProfile(old, new)` / `RenameUserProfile(old, new)` that does the assignment-rewrite under an internal lock.
- **Per-DE event-driven power management.** Tracked in [docs/ideas.md](ideas.md). Independent of the GUI, but the Session pane is a natural future home for any user-facing toggles that work surfaces.

## Trust model note

The plan operates under the current "open D-Bus + daemon-owned config" model. Any local user can call any Manager1 method via the system bus; the daemon does the privileged work as the dedicated `tisvlay` system user. This matches the existing AddTv / SetTvOption pattern. If multi-user gating becomes necessary later, polkit adds it incrementally without GUI-side changes.

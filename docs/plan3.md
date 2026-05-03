# Plan 3 — Tray as a first-class GUI peer + symmetric peer-introspection D-Bus surface

**Status: shipped.** Implementation landed in a single commit on `feat/gui-about`; smoke-tested via the Slint MCP server end-to-end (kill/restart of `tisvlay-tray` flips the GUI's row + footer dot in real time). All "Done when" criteria below are satisfied.

Bring `tisvlay-tray` into the GUI's status surfaces (footer dot, About pane CONNECTIONS row, COMPONENTS chip) and generalise the per-peer D-Bus introspection so every long-running process in the stack — daemon, session, tray, and the GUI itself — can be probed for version and config-path the same way. Replaces the static, hardcoded per-component descriptions in the GUI's COMPONENTS catalogue with values queried from each peer's own `ConfigPath` property, so a `--config /some/override` invocation is reflected accurately.

## Why now

- Tray has no D-Bus presence today; its online/offline state and version aren't surfaced anywhere in the GUI even though daemon and session both are. Visible asymmetry.
- The COMPONENTS catalogue in [`make_about_components`](../crates/tisvlay-gui/src/main.rs) hardcodes each peer's "system daemon · /var/lib/tisvlay/" string. If a deployment overrides config paths, the GUI lies.
- External tooling (busctl, monitoring scripts, future packaging tests) can already query `Manager1.Version` and `Session1.Version`. Adding `Tray1` and `Gui1` lets the same probe pattern walk the whole stack.

## Sources

- Existing pattern reference: [`crates/tisvlayd/src/dbus/manager.rs`](../crates/tisvlayd/src/dbus/manager.rs) `#[zbus(property(emits_changed_signal = "const"))] fn version()`, [`crates/tisvlay-session/src/session_dbus.rs`](../crates/tisvlay-session/src/session_dbus.rs) same shape.
- Client-side mirror: [`crates/tisvlay-core/src/dbus_client.rs`](../crates/tisvlay-core/src/dbus_client.rs) — `DaemonClient::version()`, `SessionClient::version()`.
- Watcher events to extend: same file, `StateWatcher` + `StateEvent::SessionAppeared` / `SessionDisappeared`.
- Consumer: [`crates/tisvlay-gui/src/main.rs`](../crates/tisvlay-gui/src/main.rs) `refresh_about_components` + `make_about_components`.

## Gap analysis

| Surface | Daemon | Session | Tray | GUI |
|---|---|---|---|---|
| D-Bus name registered | ✅ `org.tisvlay.Daemon` | ✅ `org.tisvlay.Session` | ❌ | ❌ |
| `Version` property | ✅ `Manager1.Version` | ✅ `Session1.Version` | ❌ | ❌ |
| `ConfigPath` property | ❌ | ❌ | ❌ | ❌ |
| Client in tisvlay-core | ✅ `DaemonClient` | ✅ `SessionClient` | ❌ | ❌ |
| Watcher appear/disappear | ✅ daemon / `Manager1` | ✅ session / `Session1` | ❌ | n/a (self) |
| Footer dot in GUI | ✅ | ✅ | ❌ | n/a (self) |
| About-pane CONNECTIONS row | ✅ | ✅ | ❌ | n/a (self) |
| About-pane COMPONENTS chip | ✅ probed `Version` | ✅ probed `Version` | ❌ static (no chip) | ✅ via `env!("CARGO_PKG_VERSION")` |
| About-pane COMPONENTS description | ❌ hardcoded path | ❌ hardcoded path | ❌ hardcoded | ❌ hardcoded |

## Decisions

1. **D-Bus bus names**: `org.tisvlay.Tray` and `org.tisvlay.Gui` — mirrors `org.tisvlay.{Daemon,Session}`.
2. **D-Bus interface names**: `org.tisvlay.Tray1` and `org.tisvlay.Gui1` — mirrors `Manager1` / `Session1` (the `1` suffix is a stable-API marker for future breaking changes).
3. **Footer ordering**: Daemon → Session → Tray. System bus first, then user-bus services in their natural pairing order.
4. **Tray1 / Gui1 surface**: just `Version` and `ConfigPath` for now. No methods (no consumer yet). Future tray notification driver and any other tray-side functionality lands in subsequent commits.
5. **`ConfigPath` semantics**: returns the canonical resolved path (post `--config` flag resolution, post default-path fallback). Empty string when the peer has no config file (the GUI today; the tray until phase 2's notification settings land). Picked canonical-resolved over raw-arg because that's what the user actually wants to know — "where is this thing reading config from."
6. **GUI symmetry**: add `Gui1.Version` + `Gui1.ConfigPath` even though the GUI is the calling process and we don't display itself in CONNECTIONS. The interface is for external probes (busctl, monitoring); GUI's own values are still surfaced via the existing `env!("CARGO_PKG_VERSION")` constant in the COMPONENTS row.
7. **Tray description copy** in CONNECTIONS row: `"Tray icon, menu actions, real-time TV state."` — matches the daemon/session "what is this and what does it do" tone.
8. **Symmetric pre-stage**: this plan adds the D-Bus surfaces and consumer wiring. The tray's actual desktop-notification driver + config schema is a follow-up plan (out of scope here).

## Phasing

Single commit if scope stays tight; split as needed during implementation.

### Commit A — `Tray1` + `Gui1` surfaces, `ConfigPath` everywhere, dynamic COMPONENTS catalogue

**Goal**: Every peer in the stack exposes the same minimal `(Version, ConfigPath)` introspection surface; the GUI consumes them all the same way.

Touches:

- **[`crates/tisvlayd/src/dbus/manager.rs`](../crates/tisvlayd/src/dbus/manager.rs)** — add `ConfigPath` const property on `Manager1` returning the resolved daemon config path. Pull from wherever the daemon currently parses `--config`.
- **[`crates/tisvlay-session/src/session_dbus.rs`](../crates/tisvlay-session/src/session_dbus.rs)** — same property on `Session1`.
- **[`crates/tisvlay-tray/src/`](../crates/tisvlay-tray/src/)** — new `dbus.rs` module that registers `org.tisvlay.Tray` on the user bus and serves `org.tisvlay.Tray1` with `Version` + `ConfigPath` (empty for now until the notification config lands). Wire from `main.rs`.
- **[`crates/tisvlay-gui/src/`](../crates/tisvlay-gui/src/)** — same shape: register `org.tisvlay.Gui` + serve `Gui1.Version` + `Gui1.ConfigPath` (empty). Probably a small `dbus.rs` module mirroring the tray's.
- **[`crates/tisvlay-core/src/dbus_client.rs`](../crates/tisvlay-core/src/dbus_client.rs)**:
  - Add `ConfigPath` to existing `TisvlayManagerProxy` and `TisvlaySessionProxy` traits.
  - Add `DaemonClient::config_path()` + `SessionClient::config_path()` accessors.
  - Add `TisvlayTrayProxy` trait + `TrayClient` (mirror of `SessionClient` shape: `new()`, `is_available()`, `version()`, `config_path()`).
  - Add `TisvlayGuiProxy` + `GuiClient` (same).
  - Extend `StateWatcher` with `TrayAppeared` / `TrayDisappeared` events driven by `NameOwnerChanged` for `org.tisvlay.Tray`. (`Gui` doesn't need watcher coverage — the calling GUI process is itself.)
- **[`crates/tisvlay-gui/src/main.rs`](../crates/tisvlay-gui/src/main.rs)**:
  - Add `tray: Arc<RwLock<Option<TrayClient>>>` alongside the existing session client.
  - `refresh_about_components` takes the tray client too; probes `version` + `config_path` for daemon, session, tray; passes them through to a refactored `make_about_components`.
  - `make_about_components` becomes data-driven: each peer entry now provides probed `(version, config_path)` and a description-template that interpolates the path. Falls back to static text when offline.
  - Wire `app.set_tray_available(...)` from watcher events.
- **[`crates/tisvlay-gui/ui/main.slint`](../crates/tisvlay-gui/ui/main.slint)**:
  - Add `in property <bool> tray-available: false;`
  - Add a third `Text` row in the footer alongside `● Daemon` / `● Session`: `● Tray`.
- **[`crates/tisvlay-gui/ui/components/about_pane.slint`](../crates/tisvlay-gui/ui/components/about_pane.slint)**:
  - Add a third `ConnectionRow` for the tray to the CONNECTIONS group:
    - `title: @tr("Tray (user bus)")`, `address: "org.tisvlay.Tray"`, `description: @tr("Tray icon, menu actions, real-time TV state.")`
    - `ok: root.tray-available`
  - No COMPONENTS-pane Slint changes — the table is data-driven; new chip arrives automatically once `make_about_components` populates the row.

**Smoke test**: `busctl --user introspect org.tisvlay.Tray /org/tisvlay/Tray` shows `Tray1` with `Version` + `ConfigPath`. Same for `org.tisvlay.Gui` / `Gui1`. Same new properties on `Manager1` / `Session1`. About pane shows three CONNECTIONS rows + tray chip in COMPONENTS with version + drift tone. Footer has three dots. Killing the tray flips its chip + footer dot to offline; restarting flips back.

## Out of scope

- **Tray's desktop-notification driver** (libnotify or xdg-desktop-portal `Notification` interface). Filed for a follow-up plan: tray-side notification dispatch, plus its config schema (per-TV notify toggles, quiet hours, etc.). When that lands, `Tray1.ConfigPath` becomes non-empty and the COMPONENTS row will include the actual config path automatically.
- **GUI window-state config**: with `Gui1.ConfigPath` empty for now, no config file is read. If we ever add window-state persistence (M5 polish), `ConfigPath` activates without a Slint change.
- **CLI**: `tisvlay` is a one-shot client, no D-Bus surface. Stays static in the COMPONENTS catalogue with no chip.
- **Test harness coverage**: `TestTray` is a placeholder today. Wiring a `TrayClient` test against a fake `Tray1` is straightforward but not load-bearing for this work; defer until a real test consumer appears.

## Done when

- ✅ `busctl --user introspect` confirms `Tray1` and `Gui1` interfaces with `Version` + `ConfigPath` properties.
- ✅ `Manager1` and `Session1` both expose `ConfigPath` const properties; verified with the same probe.
- ✅ About pane renders three CONNECTIONS rows (daemon, session, tray); footer renders three dots; COMPONENTS table chip appears for `tisvlay-tray` with the same drift-tone logic as the other peers.
- ✅ COMPONENTS-row descriptions for daemon, session, and tray reflect their actual probed `ConfigPath` (not the previously-hardcoded paths). Verified in the dev shell: `tisvlayd` reads `docs/configs/daemon.toml` and the row text says so.
- ✅ Killing or restarting `tisvlay-tray` flips the GUI's tray dot + chip in real time, parallel to how session events already work.
- ✅ `just check` passes (build + clippy + tests); `just package-rpm` still produces an installable artifact (no nfpm config changes — both new binaries are already shipped).
- ⏳ Plan 3 entry to add to [`docs/plan.md`](plan.md)'s "Beyond the original roadmap" table — pending follow-up commit when the branch lands on `origin/main`.

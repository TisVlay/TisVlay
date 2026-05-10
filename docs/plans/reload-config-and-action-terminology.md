# Reload-config: register/unregister D-Bus paths + unify the user-facing action term

> **Status:** delivered 2026-05-10; smoke test confirmed. Two intertwined fixes landing in one branch: (a) `Manager1.ReloadConfig` doesn't register/unregister D-Bus object paths when TVs are added or removed via direct config-file edits, leaving ghost TVs visible in clients (or invisible new TVs); (b) the GUI labels its sidebar action "Reload" while the Tray menu labels the equivalent action "Refresh" — same D-Bus call, two terms. TDD-driven: write a failing regression test, watch it fail, implement the fix, watch it pass.

User-reported reproduction: pair a mock device, manually remove its `[[tv]]` block from `config.toml` with a text editor, click "Reload" in the GUI sidebar (or "Refresh" in the Tray menu). Expected: the device disappears from both UIs. Actual: the device persists in both UIs until the daemon is restarted.

## Source-of-truth audit

### Bug surface

| Site | Today | Should be |
|---|---|---|
| [`tv_manager.rs::reload_config`](../../crates/tisvlayd/src/tv_manager.rs) (~L2117) | Updates internal `IndexMap`, closes connections on removed TVs, emits `StateChangeEvent::ConfigReloaded`. Returns `Result<(), TvManagerError>`. | Same, but also returns the `(added, removed)` ID diff so the dbus layer can register/unregister object paths. |
| [`dbus/manager.rs::reload_config`](../../crates/tisvlayd/src/dbus/manager.rs) (~L272) | Calls `manager.reload_config().await` and returns. No D-Bus path manipulation. | Diff in hand, loop `register_tv_interfaces` for `added`, loop `unregister_tv_interfaces` for `removed`. |
| [`state_forwarder.rs::ConfigReloaded`](../../crates/tisvlayd/src/state_forwarder.rs) (~L193) | Emits `Manager1.ConfigReloaded` signal, refreshes cached property values for *currently-known* TVs, re-runs desktop probe. | Unchanged. The state forwarder reacts to the `ConfigReloaded` event but isn't responsible for path lifecycle (the dbus handler is now). |

### Terminology surface

| Site | Today | Should be |
|---|---|---|
| [`crates/tisvlay-gui/ui/main.slint`](../../crates/tisvlay-gui/ui/main.slint) sidebar item | Label `"Reload"`, icon `"↻"`, a11y label `"Reload daemon config"` | Unchanged — keep "Reload" as the canonical user-facing term. |
| [`crates/tisvlay-tray/src/tray.rs`](../../crates/tisvlay-tray/src/tray.rs) (~L322) | Label `"Refresh"`, icon `"view-refresh"` | Label `"Reload"`, icon `"view-refresh"` (or `document-revert` if the StatusNotifierItem theme renders something more reload-shaped — verify in practice). |
| [`crates/tisvlay-tray/src/tray.rs`](../../crates/tisvlay-tray/src/tray.rs) variant `TrayCommand::Refresh` | Variant name `Refresh` | Variant name `Reload` (internal symbol, no user-visible impact, but matches the new label). |

Why "Reload" wins:

- It matches the underlying API name (`Manager1.ReloadConfig`).
- The action *is* a config-file re-read with side effects (per-TV connection close on removed TVs); "Refresh" implies a UI-only state update and is misleading in this context.
- "Refresh" is the conventional UI term for *state* fetch, which both clients already do continuously via signals — there's no separate user-facing affordance for it. Keeping "Refresh" on the Tray's reload button conflates two concepts.

## Fix design

### Daemon side (the bug)

`tv_manager.reload_config` currently returns `Result<(), TvManagerError>`. Change to return `Result<ReloadDiff, TvManagerError>` where `ReloadDiff` carries:

```rust
pub struct ReloadDiff {
    pub added:   Vec<String>,  // TV ids that appeared in the new config
    pub removed: Vec<String>,  // TV ids that vanished from the new config
}
```

The diff is computed naturally inside the existing reload body — Phase 1 (~L2129-2173) already iterates old and new id sets to do its remove/add bookkeeping; collect the ids into the `ReloadDiff` while it's at it. No structural change to the reload logic, just a return-value enrichment.

Then `dbus/manager.rs::reload_config` becomes:

```rust
async fn reload_config(&self) -> Result<(), DaemonError> {
    info!("D-Bus: ReloadConfig");
    let diff = self.manager.reload_config().await?;

    let connection = /* zbus Connection from #[zbus(connection)] handler arg */;
    for tv_id in &diff.removed {
        let path = tv_path(tv_id);
        if let Err(e) = unregister_tv_interfaces(connection, &path).await {
            warn!(tv_id = %tv_id, error = %e, "ReloadConfig: failed to unregister TV interfaces");
        }
    }
    for tv_id in &diff.added {
        let path = tv_path(tv_id);
        let state = self.manager.get_current_state(tv_id).await;
        if let Err(e) = register_tv_interfaces(
            connection, &path, tv_id.clone(), self.manager.clone(), state.as_ref(),
        ).await {
            warn!(tv_id = %tv_id, error = %e, "ReloadConfig: failed to register TV interfaces");
        }
    }
    Ok(())
}
```

Mirrors the `Manager1.AddTv` (registers on add) and `Tv1.Remove` (unregisters on remove) patterns that already exist — symmetry restored.

`tv_manager` stays decoupled from zbus. The dbus handler is the layer that already knows about both, so it's the right home for the wiring.

### Client side (terminology)

Tray:

- [`tray.rs`](../../crates/tisvlay-tray/src/tray.rs): rename `TrayCommand::Refresh` → `TrayCommand::Reload`. Update the menu item label `"Refresh"` → `"Reload"`. Icon stays as `view-refresh` for now (StatusNotifierItem icon themes vary; revisit if a better fit surfaces).
- [`main.rs`](../../crates/tisvlay-tray/src/main.rs): rename the match arm and surrounding log messages (`"Refresh: …"` → `"Reload: …"`). The internal `refresh_state()` helper name is fine — that's a state-fetch helper used by multiple paths, not the user-facing action.

GUI: no changes — already uses "Reload".

i18n: the gettext catalog has a `Refresh` msgid via `@tr("Refresh")` in tray.rs. Renaming triggers `just i18n-extract` to update `tisvlay-gui.pot` (note: tray strings currently aren't covered by the gui-bound catalog; verify whether the rename actually flows through any extraction path or if the tray's strings are bare literals without gettext wrapping today).

## Failing regression test

New file [`crates/tisvlayd/tests/reload_registers_unregisters_dbus_paths.rs`](../../crates/tisvlayd/tests/reload_registers_unregisters_dbus_paths.rs), modeled on the existing [`reload_resolves_system_profile.rs`](../../crates/tisvlayd/tests/reload_resolves_system_profile.rs).

Two test cases — both should fail on `main` and pass after the fix lands.

### Test 1: removal via file edit + reload unregisters the D-Bus path

```rust
#[tokio::test]
async fn reload_unregisters_dbus_path_for_removed_tv() {
    // Initial: 2 mock TVs configured.
    let initial = mock_config_with_tvs(&["living_room", "bedroom"]);
    let daemon = TestDaemon::with_config(initial).await.expect("daemon");
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    let client = DaemonClient::from_connection(bus_conn).await.expect("client");

    // Sanity: both TVs visible via ObjectManager.
    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(tvs.len(), 2, "sanity: both TVs visible after startup");

    // Edit config: drop "bedroom", keep "living_room".
    let updated = mock_config_with_tvs(&["living_room"]);
    updated.save_to(&daemon.config_path()).expect("write config");

    daemon.manager().reload_config().await.expect("reload");

    // Settle window — state_forwarder consumes ConfigReloaded
    // asynchronously; D-Bus path unregistration happens in the
    // dbus::manager::reload_config handler synchronously, so this
    // sleep mirrors `reload_resolves_system_profile.rs`'s pattern.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(tvs.len(), 1, "removed TV should disappear from ObjectManager after reload");
    assert_eq!(tvs[0].name, "living_room", "remaining TV should be living_room");
}
```

### Test 2: addition via file edit + reload registers the D-Bus path

```rust
#[tokio::test]
async fn reload_registers_dbus_path_for_added_tv() {
    let initial = mock_config_with_tvs(&["living_room"]);
    let daemon = TestDaemon::with_config(initial).await.expect("daemon");
    let bus_conn = PrivateBus::shared().connect().await.expect("client conn");
    let client = DaemonClient::from_connection(bus_conn).await.expect("client");

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(tvs.len(), 1, "sanity: one TV visible after startup");

    let updated = mock_config_with_tvs(&["living_room", "bedroom"]);
    updated.save_to(&daemon.config_path()).expect("write config");

    daemon.manager().reload_config().await.expect("reload");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let tvs = client.list_tvs().await.expect("list_tvs");
    assert_eq!(tvs.len(), 2, "added TV should appear in ObjectManager after reload");
    assert!(tvs.iter().any(|t| t.name == "bedroom"), "bedroom should be among the visible TVs");
}
```

A `mock_config_with_tvs(&[&str]) -> SystemConfig` helper either lands in `tisvlay-harness::daemon` (next to `mock_tv_config`, `test_tv_uuid`) for cross-test reuse, or inline at the top of the test file. Cross-test reuse is the better default.

Pre-fix expected behavior: test 1 returns 2 TVs after reload (the removed one persists); test 2 returns 1 TV after reload (the added one is invisible). Both assertions fail. After the fix, both pass.

## Steps

| # | Step | Status | Notes |
|---|---|---|---|
| 1 | Add `mock_config_with_tvs(&[&str]) -> SystemConfig` helper to `tisvlay-harness::daemon` | done | Builds on top of existing `mock_tv_config`. |
| 2 | Write the two failing regression tests in `reload_registers_unregisters_dbus_paths.rs` | done | Confirms the bug exists pre-fix; both tests fail. Run `cargo nextest run -p tisvlayd reload_registers_unregisters` to verify. |
| 3 | Define `ReloadDiff { added: Vec<String>, removed: Vec<String> }` in `tv_manager.rs` and change `reload_config`'s signature to return it | done | Public API. Used only by the dbus layer today. |
| 4 | Wire the diff collection into the existing reload body | done | Phase 1 already iterates the right id sets — collect into `ReloadDiff` rather than just dropping the info. |
| 5 | Update `dbus/manager.rs::reload_config` to consume the diff and call `register_tv_interfaces` / `unregister_tv_interfaces` | done | Mirrors `add_tv` and `Tv1.Remove`'s patterns. Extracted to an `apply_reload_diff` helper in `dbus/setup.rs` so the SIGHUP handler in `main.rs` can share it — that path had the same bug. |
| 6 | Re-run the regression tests; both pass | done | Verifies the fix. |
| 7 | Rename `TrayCommand::Refresh` → `Reload`, update label and log strings in `tray.rs` + `main.rs` | done | Pure search-and-replace; no behavior change. Followed up with a small commit swapping the GUI sidebar's `↻` Unicode glyph for `Icons.reload` (resolving `view-refresh` from the user's freedesktop theme) so both clients render the same icon. |
| 8 | Run `prek run -a` and `just check` end-to-end | done | Lint + tests + clippy gate. Per `feedback_lint_and_test_before_commit.md`. |
| 9 | Smoke test on a Wayland session: pair a mock TV, edit config to remove it, click "Reload" in GUI / "Reload" in Tray, verify it disappears in both | done | The actual user flow that motivated the fix. |

## Risks / unknowns

- **`ReloadDiff` callers**: `tv_manager.reload_config()` is currently called in two places — `dbus/manager.rs::reload_config` (the D-Bus handler) and direct test invocations (`reload_resolves_system_profile.rs`, possibly others). Tests don't care about the new return value but will need a `let _ = ` or an `expect(...).into_diff()`-style throwaway. Cheap.
- **Unregister race**: if a Tv1.* method is in flight when reload removes the TV, the call could see an unregistered path mid-call. zbus's `object_server.remove` is synchronous from the caller's perspective but the in-flight call may already be unwinding. Same race exists in `Tv1.Remove` today — existing pattern, not a new exposure. Worth a comment but not a blocker.
- **Tray icon**: `view-refresh` is the system icon name for "refresh"-shaped iconography. Most icon themes render it as a circular arrow which still reads as "reload" reasonably well; if a particular theme renders it as a literal "refresh" pictogram (e.g., download arrows), revisit. Out of scope for this branch — lock the *label* now; revisit the icon as a follow-up if it stands out in practice.
- **i18n update**: gettext extraction may or may not pick up the tray's bare-literal strings depending on whether they're wrapped in `gettext(...)` today. Verify during step 7; if extraction misses it, the rename's i18n delta is just a `.po` cleanup of the now-orphaned `Refresh` msgid, deferrable.

## Out of scope

- **Manager1.ReloadConfig D-Bus signature change**: the public method signature stays `() -> ()`. The diff is internal between tv_manager and the dbus handler, not a wire-format change.
- **Per-TV `OptionChanged` events on reload**: the existing state_forwarder behavior (refreshing cached properties for currently-known TVs) is correct for the *update* case and unchanged here. Only the path lifecycle is being fixed.
- **Tray icon redesign**: see Risks above.
- **Tray "open settings" link / GUI refresh shortcut**: the user's broader API-shape concern was about the Reload/Refresh inconsistency, not other sidebar/menu items. Audit can come later if a similar inconsistency surfaces.
- **Generalized "config drift detection"**: the daemon could `inotify`-watch the config file and auto-reload, but that's a separate UX choice (auto-reload is invasive; explicit reload is the current contract). Out of scope.

## References

- [`crates/tisvlayd/src/tv_manager.rs::reload_config`](../../crates/tisvlayd/src/tv_manager.rs) — internal-state reload body that gains the diff return value.
- [`crates/tisvlayd/src/dbus/manager.rs::reload_config`](../../crates/tisvlayd/src/dbus/manager.rs) — D-Bus handler that gains the register/unregister loops.
- [`crates/tisvlayd/src/dbus/tv.rs::unregister_tv_interfaces`](../../crates/tisvlayd/src/dbus/tv.rs) (~L599) and `register_tv_interfaces` — the helpers we wire in.
- [`crates/tisvlayd/tests/reload_resolves_system_profile.rs`](../../crates/tisvlayd/tests/reload_resolves_system_profile.rs) — sibling test that establishes the reload-test pattern (config-on-disk + `manager.reload_config()` + D-Bus assertion).
- [`crates/tisvlay-harness/src/daemon.rs`](../../crates/tisvlay-harness/src/daemon.rs) — where `mock_config_with_tvs` lands.
- [`crates/tisvlay-tray/src/tray.rs`](../../crates/tisvlay-tray/src/tray.rs) and [`main.rs`](../../crates/tisvlay-tray/src/main.rs) — terminology rename surface.
- [`crates/tisvlay-gui/ui/main.slint`](../../crates/tisvlay-gui/ui/main.slint) — GUI sidebar Reload affordance (unchanged, already canonical).

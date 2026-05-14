# GUI desktop integration: single-instance, tray launch, .desktop entry

> **Status:** drafted 2026-05-11, revised same day after a 2026 best-practices audit, **implemented and validated 2026-05-11/12 on live KDE Plasma 6 Wayland**. The original three-commit design landed plus five follow-up fix commits resolving live-test bugs (Slint `show()` kill-on-mapped-surface, multi-spawn cascade on rapid clicks, KIO queue-drop race during interface registration, activation-token propagation, and citation corrections). See [Post-implementation findings](#post-implementation-findings) for what we learned vs the original plan, and [Cleanup follow-ups](#cleanup-follow-ups) for churn-debt the implementation accumulated.

## Context

`tisvlay-gui` exists as a binary but isn't an "app" on the desktop: no `.desktop` entry, no icon in the launcher, no single-instance handling (running it twice opens two windows talking to the same daemon), and the only way to reach it from a running session is to type the binary name in a terminal. Meanwhile `tisvlay-tray`'s menu offers `Quit` — useful in development, wrong for a service-managed binary in production where ksni is supposed to run for the life of the session.

Three coordinated changes close the gap:

1. **Quit gating.** Hide the tray's `Quit` item in release builds where the tray is managed as a systemd user service. Keep it for `cargo run` so developers can still exit cleanly without `systemctl --user stop`.
2. **Tray-driven GUI launch with single-instance.** Add an "Open Settings" tray item. Clicking it should raise the existing GUI window if one is open, and spawn a new GUI otherwise — never duplicate. Independently, launching the GUI from the app menu, KRunner, or `gdbus call` should go through the same code path so behavior is uniform across entry points.
3. **`.desktop` entry.** Make the GUI appear in the system app menu under Settings, with a freedesktop icon-theme icon (`video-display`, matching the tray's choice) until custom artwork lands.

The unifying mechanism for (2) and (3) is the **freedesktop `org.freedesktop.Application` D-Bus interface** plus a `DBusActivatable=true` desktop entry. This is the same pattern GTK's `Gtk.Application` and Qt's `QGuiApplication` use under the hood; KDE Plasma, GNOME Shell, KRunner, and xdg-desktop-portal all route launches through it when the entry opts in. Reusing the standard means the tray's "Open Settings" button hits the same activation path as the app menu, with no parallel forking logic.

Under Plasma 6.5+ (Aug 2025+) KWin tightened focus-stealing prevention: `Activate(platform_data)` with empty `platform_data` reliably *spawns* a new instance but may *silently fail to raise* an already-running window. The plan therefore also threads `xdg-activation-v1` tokens from the tray-side caller through to the GUI-side handler. Details in DR4 and DR5.

## Design decisions

### DR1: `cfg(debug_assertions)` gates the tray's Quit menu item

The simplest mechanism that auto-aligns with what we already do for release packaging. `rustPlatform.buildRustPackage` (Nix) and `cargo-zigbuild` (nfpm `.deb` / `.rpm`) both compile with `--release`, so `debug_assertions = false` falls out for free — Quit disappears in shipped packages and stays for `cargo run`. Zero `Cargo.toml` plumbing, no `--no-default-features` propagation across the three packaging surfaces.

The `TrayCommand::Quit` enum variant and its dispatch in `main.rs` stay regardless — SIGINT / SIGTERM handlers still drive them. Only the menu item construction in `tray.rs` is gated.

Alternative considered: a `feature = "dev-menu"` mirroring the existing `dev-mcp` pattern. Rejected because it costs `--no-default-features` plumbing on three packaging paths for no benefit unless someone wants a "production menu" debug build for QA — not a current requirement.

### DR2: `org.freedesktop.Application` for single-instance and activation

Pattern: the GUI claims the bus name `org.tisvlay.Gui` on the session bus at startup. If the request succeeds, it's the authoritative instance and registers `org.freedesktop.Application` at `/org/tisvlay/Gui` with one method, `Activate(platform_data)`. If the request returns `Exists` (another process already owns the name), the new process gets a proxy to the existing owner, calls `Activate({})`, and exits cleanly. The existing instance receives the call and raises its window.

This pattern is mature: GTK's `Gtk.Application(flags=Gio.ApplicationFlags.HANDLES_OPEN)`, Qt's `QtSingleApplication`, and freedesktop's `xdg-desktop-portal` all use it. Picking the standard interface (not a custom `org.tisvlay.Gui1.Activate`) means any launcher that supports DBusActivatable — Plasma activities, GNOME Shell, KRunner, anything routed through xdg-desktop-portal — hits the same code as the tray button.

Alternatives considered and rejected:

- **Custom D-Bus interface.** Functionally equivalent but only the tray would know to call it; app menu launches would fall back to `fork+exec`, racing the bus-name claim.
- **Lock-file plus signal.** No clean way to raise the window from a signal handler inside Slint's event loop; we'd end up rebuilding a bespoke version of the D-Bus pattern with worse ergonomics.

### DR3: `.desktop` filename and keys

When `DBusActivatable=true` is set, the freedesktop spec requires the desktop entry's *basename* (without `.desktop`) to be a valid D-Bus bus name and to match the bus name the app claims. So the file is `org.tisvlay.Gui.desktop`, not `tisvlay-gui.desktop`. The bus name is `org.tisvlay.Gui` and the object path is the bus name with dots replaced by slashes, `/org/tisvlay/Gui`. All three follow the existing `org.tisvlay.Daemon` / `org.tisvlay.Session` conventions.

Beyond the basics, four Desktop Entry spec keys (additions across the 2024–2025 spec revisions) are worth setting up front:

- **`SingleMainWindow=true`** — exact match for our one-settings-window model; hides launcher "Open New Window" affordances on Plasma 6 and GNOME 47+. Pair with `X-GNOME-SingleWindow=true` for older GNOME.
- **`StartupWMClass=org.tisvlay.Gui`** — KWin and GNOME Shell use this to associate the live window with the .desktop entry. Without it: wrong taskbar icon, broken task-grouping, fallback heuristics based on process name. Must match the Wayland `app_id` Slint sets on its surface.
- **`Implements=org.freedesktop.Application;`** — spec-recommended when implementing an optional D-Bus interface; surfaces capability to launchers.
- **`Keywords=tv;television;remote;hdmi;webos;lg;`** — improves KRunner / GNOME-Shell search hit rate. Cheap addition.

Slint 1.16 ships `slint::set_xdg_app_id(app_id) -> Result<(), PlatformError>` (PR slint-ui/slint#6851, merged Nov 2024, fixed #1332). Call it in `tisvlay_gui::run()` before constructing `AppWindow`; the winit backend folds it into `WindowAttributes::with_name()` for Wayland `app_id` and X11 `WM_CLASS`. With this in place the `.desktop` entry's `StartupWMClass=org.tisvlay.Gui` matches the live window, and combined with `SingleMainWindow=true` KWin / GNOME Shell can raise the existing window from the taskbar / app menu / KRunner without going through our D-Bus `Activate` handler.

### DR4: `Activate` handler bridges into Slint via `slint::invoke_from_event_loop`

The zbus interface handler runs on a Tokio task. The Slint window only accepts mutation from inside its event loop. The existing GUI code already uses `slint::invoke_from_event_loop(move || ...)` at [crates/tisvlay-gui/src/lib.rs:92](../../crates/tisvlay-gui/src/lib.rs#L92) to shuttle daemon push events into the UI — same pattern applies to `Activate`. The handler captures a `slint::Weak<MainWindow>`, posts a closure that calls `window.upgrade().unwrap().window().show()` (plus whatever raise/focus call Slint exposes; verify during implementation), and returns immediately. No new threading primitives.

**Activation-token propagation.** Under Plasma 6.5+ and KWin's "Extreme" focus-stealing-prevention setting, a bare `window.show()` will silently *fail to raise* an already-mapped window on Wayland — the compositor denies the activation without a valid `xdg-activation-v1` token. The `Activate` handler must read `platform_data["activation-token"]` (a `String` value packed into the `a{sv}` dict), set `$XDG_ACTIVATION_TOKEN` in the process environment before the `slint::invoke_from_event_loop` call, and let Slint's underlying winit pick it up via the standard Wayland activation flow. If the dict is missing the key (older callers, `gdbus` CLI smoke tests, X11 sessions), the handler proceeds without a token and accepts the best-effort raise — matches today's behavior on X11 and pre-Plasma-6.5 systems.

The token is single-use and short-lived (seconds). The tray-side acquisition is covered in DR5.

### DR5: Tray acquires an `xdg-activation-v1` token before calling `Activate`

The caller half of the activation-token plumbing. Before the tray's "Open Settings" dispatch calls `ApplicationProxy::activate(platform_data)`, it must obtain an activation token from the compositor and pack it into `platform_data` under the key `activation-token`.

The token-acquisition flow on Wayland is: bind `xdg_activation_v1` from the global registry, call `get_activation_token`, set the requesting surface (the SNI tray icon's surface, exposed by ksni's underlying StatusNotifierItem surface), `commit`, and listen for the `done` event carrying the token string. KDE's KDBusService wrapper does this in C++/Qt apps; the Rust equivalent is direct `wayland-protocols` crate usage (or via `wayland-client` if we want finer control).

This pulls a small Wayland-protocols dep into `tisvlay-tray` (the crate doesn't have one today; ksni itself encapsulates its Wayland surface internally). On X11 sessions and on Wayland sessions where `xdg_activation_v1` isn't advertised, the helper returns `None` and the dispatch falls back to calling `Activate` with empty `platform_data` — the cold-spawn path still works via D-Bus auto-activation, and the raise-existing path degrades to today's "may or may not raise" behavior. That degradation matches the pre-Plasma-6.5 baseline, so it's acceptable for first cut.

Alternative considered and rejected: requesting the token via `xdg-desktop-portal`'s `org.freedesktop.portal.Request` interface (the `ashpd` Rust crate). The portal route exists for sandboxed/Flatpak apps that don't have direct Wayland-protocol access; we have direct access via ksni's surface, so the portal is unnecessary indirection.

### DR6: D-Bus session service file mirrors `org.tisvlay.Daemon.service`

Four lines, installed at `share/dbus-1/services/org.tisvlay.Gui.service`:

```ini
[D-BUS Service]
Name=org.tisvlay.Gui
Exec=/usr/bin/tisvlay-gui
```

(`User=` and `SystemdService=` from the Daemon file don't apply — the GUI runs as the calling user, no systemd unit.)

With this file in place, `gdbus call --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui --method org.freedesktop.Application.Activate '{}'` auto-spawns the binary if it isn't running. The tray's "Open Settings" button calls the same method (with an activation token added per DR5); whether the GUI is already up or not, the user sees one window.

**Smoke-test caveat.** The bare `gdbus` command above with empty `platform_data` is fine for verifying the *spawn* path under D-Bus auto-activation. It is **not** a valid smoke test for the *raise-existing-window* path under Plasma 6.5+ — that requires a token, which `gdbus` from a non-Wayland-surface shell can't easily obtain. For end-to-end "raise an existing window" verification, use the tray's "Open Settings" button (which has its own activation surface) or write a small Wayland-client test harness that performs the token-acquisition handshake before the D-Bus call.

### DR7: nfpm `contents` migration scope

The current nfpm config in [crates/tisvlayd/Cargo.toml](../../crates/tisvlayd/Cargo.toml) has no `package.metadata.nfpm.contents` block — it relies on cargo-nfpm's default-content discovery, which picks up `/usr/bin/<binary>` for every workspace binary plus systemd units it finds. Adding the two new packaging files (`.desktop`, D-Bus service) means we either rely on cargo-nfpm's heuristics finding them under `packaging/freedesktop/` (unlikely — it doesn't know that directory exists) or we switch to explicit `contents` enumeration.

Switching to explicit `contents` means listing **all** installed files, not just the two new ones — a small migration risk: drop a binary or systemd unit and a release ships incomplete. Mitigation: before the migration, run `dpkg-deb -c target/nfpm/tisvlay_*.deb | sort` against today's build, copy that list into the `contents` array, then add the two new entries. Diff after a rebuild confirms parity.

If the migration feels too risky to bundle, the alternative is to ship commits 1 and 2 as one PR and commit 3 (packaging) as a follow-up — keeps blast radius small. Recommendation is still to bundle so the GUI is end-to-end usable from app-menu launch in one PR, but it's a defensible split.

### DR8: `request_name` is synchronous, before Slint spinup

The bus-name claim happens early in `tisvlay_gui::run()` before any Slint code runs. If the request returns `Exists`, the process never spins up a Slint event loop — it just proxies `Activate` and exits. This avoids the user-visible flash of a half-built window appearing for 50ms before exiting.

`request_name` uses the `DO_NOT_QUEUE` flag — we never want to wait for the existing instance to release the name; if it's there, we hand off and quit.

## Implementation order

### Commit 1 — GUI single-instance + `org.freedesktop.Application`

Files:

- **New** `crates/tisvlay-gui/src/single_instance.rs` — encapsulates the `request_name` decision and the `Application` interface impl. The interface's `Activate(a{sv})` handler reads `platform_data["activation-token"]`, sets `$XDG_ACTIVATION_TOKEN` in the process environment if present, then posts the window-raise closure via `slint::invoke_from_event_loop`. Keeps `lib.rs` from growing further.
- `crates/tisvlay-gui/src/lib.rs` — call the new module's entry from `run()` before Slint spinup. On `Exists` → return `Ok(())` after the proxy `Activate` call. On `PrimaryOwner` → register the interface, store the `slint::Weak<MainWindow>` in the interface struct, continue with normal startup.

Verification:

- Spawn race: `tisvlay-gui &; tisvlay-gui &` — second exits cleanly, first stays.
- Cold-spawn via D-Bus: kill any running instance, then `gdbus call --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui --method org.freedesktop.Application.Activate '{}'` — a new GUI window appears.
- Raise-existing under Plasma 6 Wayland: with an instance already running, the gdbus call above succeeds at the D-Bus level but may or may not raise (no token — expected); the *real* raise-existing verification is via commit 2's tray button.

### Commit 2 — Tray "Open Settings" + Quit gating

Files:

- `crates/tisvlay-tray/Cargo.toml` — add the minimum Wayland-protocols dep to access `xdg_activation_v1` (likely `wayland-protocols` with the `staging` or `unstable` feature plus `wayland-client`; finalize during implementation based on what crate exposes the protocol today).
- **New** `crates/tisvlay-tray/src/activation_token.rs` — small helper that requests an `xdg-activation-v1` token from the compositor against ksni's tray surface and returns `Option<String>`. Returns `None` on X11 sessions or if the protocol isn't advertised.
- `crates/tisvlay-tray/src/tray.rs` — add `TrayCommand::OpenGui` variant near the existing variants; insert an "Open Settings" `tv_item` with icon `preferences-system` between the TV-list section and the Reload separator; wrap the Quit `tv_item` in `#[cfg(debug_assertions)]`.
- `crates/tisvlay-tray/src/main.rs` — add a dispatch arm: acquire a token via the helper, build `HashMap<String, Value>` containing it under `activation-token` (or empty if `None`), then call `ApplicationProxy::activate(platform_data)` on the session bus. Errors log a warning; user-visible failure is rare given D-Bus auto-spawn covers the "GUI not running" case.

Verification: `cargo run -p tisvlay-tray` shows Quit and "Open Settings"; release build (`cargo build --release && ./target/release/tisvlay-tray`) shows only "Open Settings". Click "Open Settings" once → GUI spawns. Click again → existing window raises (the activation-token path; this is the verification that's hard to do via gdbus per DR6).

### Commit 3 — Packaging fanout

Files:

- **New** `packaging/systemd/org.tisvlay.Gui.service` — the four-line D-Bus session service file.
- **New** `packaging/freedesktop/org.tisvlay.Gui.desktop` — the desktop entry, including `DBusActivatable`, `SingleMainWindow`, `StartupWMClass`, `Implements`, `Keywords`, `Icon=video-display`, `Categories=Settings;HardwareSettings;`. New subdirectory keeps freedesktop-specific files separate from systemd units. (`packaging/desktop/` would also work; `freedesktop/` is more accurate since the directory may grow to hold MIME types, icon variants, etc.)
- `packaging/nix/package.nix` — `postInstall` adds two `install -Dm0644` lines into `$out/share/applications/` and `$out/share/dbus-1/services/`.
- `crates/tisvlayd/Cargo.toml` — add `[package.metadata.nfpm.contents]` enumerating today's installed files plus the two new ones (see DR7 for migration approach).
- `packaging/scripts/postinstall.sh` — append `update-desktop-database /usr/share/applications 2>/dev/null || true`. Redundant on dpkg/rpm (those run triggers automatically) but harmless and defensive.

Verification:

- `nix build .#tisvlay && find result/share -name '*.desktop' -o -name '*.service' | sort` lists both new files at the expected paths.
- `just package-deb && dpkg-deb -c target/nfpm/tisvlay_*.deb | grep -E 'applications|dbus-1'` shows both installed.
- `just package-deb && diff <(dpkg-deb -c target/nfpm/tisvlay_*.deb | sort) docs/baselines/deb-contents.txt` (if we capture a baseline) confirms no regression from the `contents` migration.

## Out of scope for this plan

- Custom app icon (PNG/SVG artwork). Using `video-display` from the freedesktop icon theme until artwork lands. The icon name in the `.desktop` entry can be swapped without touching code.
- MIME-type handlers (e.g. `tisvlay-gui://` URL scheme). The user mused about URI handling; D-Bus activation covers the single-instance need without a custom scheme. URI handling can be added later by extending `org.freedesktop.Application` with `Open(uris, platform_data)`.
- A NixOS module flag for opt-out of GUI install (`enableGui`). Already tracked in [docs/plan.md](../plan.md) M6.

## Open questions

- **Slint window raise / focus API.** Slint 1.16 has *no* public method to bring an existing window to the foreground (tracked at slint-ui/slint#4382, open). The naive workaround — `window.show()` from inside `slint::invoke_from_event_loop` on an already-mapped surface — destroys the Wayland surface and exits the event loop, killing the GUI process (confirmed in live testing on Plasma 6 Wayland). Our `Activate` handler therefore no-ops on already-running GUIs; users rely on the `.desktop` entry's `StartupWMClass` match + `SingleMainWindow=true` to let KWin raise the window from the taskbar / app menu / KRunner. When #4382 lands, the handler can re-introduce `slint::invoke_from_event_loop` + the activation-token consumption from DR4 against the proper raise API.
- **Window-on-close behavior.** Today `tisvlay_gui::run()` returns when the Slint window closes, ending the process. With D-Bus activation, this is fine: subsequent activations auto-spawn. Worth a quick check that an `Activate` arriving microseconds before window close doesn't drop the window-raise instruction.
- **Tray activation surface availability.** Ksni's underlying StatusNotifierItem surface is what we'd use to scope the `xdg-activation-v1` request. Verify during implementation that ksni exposes the `wayland-client` `Surface` handle (or equivalent) to consumers — if not, we may need to upstream a tiny accessor or fall back to a "best-effort no-token" path on systems where the surface isn't reachable.
- **Whether to capture the baseline `.deb` contents (DR7 verification step) into a file in the repo, or do it ad-hoc during PR review.** Leaning ad-hoc — the file would decay.

## Post-implementation findings

Five substantive bugs surfaced during live testing on KDE Plasma 6 Wayland that the original plan didn't anticipate. Each is documented here as a permanent reference because some intersect with non-obvious upstream behavior (zbus, Slint, KIO, KWin); without the source-of-truth pointers below, the fixes look arbitrary.

### F1: zbus drops queued calls if interfaces aren't registered before claiming the bus name

Confirmed root cause of the "Did not receive a reply" KIO dialog and the "Remote peer disconnected" Plasma notification on close. KDE's KIO launcher (`KIO::ApplicationLauncherJob` → `DBusActivationRunner::startProcess`) issues an `org.freedesktop.Application.Activate` call on the well-known bus name *during* cold-spawn auto-activation. `dbus-daemon` queues the call waiting for the name to be claimed; once we claim it, the daemon resolves the destination to our unique connection name and delivers the queued call.

If we claimed the name *first* and registered the `Application` interface *later* (the original plan's order — `try_claim()` early, `serve()` inside the daemon-client `block_on` ~100ms later), zbus dropped the queued call during the gap — per its own warning:

> Requesting name `X` before setting up the object server. Method calls arriving before interfaces are registered may be lost.

KIO waited 25 s for the reply, timed out, and surfaced both notifications. We mistakenly ignored that warning as cosmetic; it was the root cause.

**Fix:** merge `try_claim` + `serve` into `try_claim_and_serve` with order: connect → register interfaces → claim name. By the time we own the name, the object server is ready; queued calls hit the handler immediately.

**Source-of-truth pointers** (for re-verification when KDE versions move):

- KIO launcher: [`~/ws/lgtv/libs/kio/src/gui/dbusactivationrunner.cpp`](../../../libs/kio/src/gui/dbusactivationrunner.cpp) — see `startProcess()`. Object-path derivation, interface name, the three methods called (`Activate` / `Open` / `ActivateAction`), and the `activation-token` packing from the spawn-process env all live here.
- zbus warning emitter: [`~/ws/lgtv/libs/zbus/zbus/src/connection/mod.rs:633`](../../../libs/zbus/zbus/src/connection/mod.rs#L633).
- zbus's atomic alternative: `Builder::session().name().serve_at().build()` — rejected because the builder forces `DoNotQueue` and errors on conflict, which kills our graceful-proxy-on-Exists path. The manual register-before-claim approach preserves both.

### F2: Slint 1.16 `window.show()` on an already-mapped Wayland surface destroys the surface and exits the event loop

**Resolved by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md).** The inert-on-re-activation `raise()` handler was superseded by a `winit::Window::request_user_attention` call (slint's `unstable-winit-030` feature) — the spec-compliant xdg-activation-v1 attention path. KWin maps `UserAttentionType::Informational` to `demands_attention`, which pulses the taskbar entry under default Focus Stealing Prevention without ever calling `window.show()`. The body below is preserved as the historical bug record.

Discovered when the first version of the `Activate` handler called `window.show()` via `slint::invoke_from_event_loop` to raise the existing window. Observed cascade in journal: every Activate killed the process (`tisvlay-gui shutting down` within 1 s of receiving Activate), and the next click respawned via D-Bus auto-activation — repeating until the user closed every zombie window. The tracking upstream issue is [slint-ui/slint#4382](https://github.com/slint-ui/slint/issues/4382) (open) for a proper raise/focus API.

**Fix:** the `Activate` handler is intentionally inert on already-mapped windows — it only consumes the `activation-token` from `platform_data` for the *cold-spawn* path (via a `oneshot::channel` to the main thread; main thread sets `$XDG_ACTIVATION_TOKEN` before `app.run()`). For re-activation while running, users rely on KWin's `StartupWMClass` match + `SingleMainWindow=true` to raise from the taskbar / KRunner / app menu without going through our handler at all.

### F3: Multi-spawn cascade from rapid tray clicks with slow dispatch

The `OpenGui` tray dispatch arm took ~500 ms per click (mostly the `xdg-activation-v1` token request inside `spawn_blocking`). Rapid clicks queued in the mpsc; if the user closed the GUI mid-queue, each remaining `Activate` respawned a fresh GUI via D-Bus auto-activation. Observed cascade: 5 rapid clicks → 5 GUIs to close in sequence.

**Fix:** `Arc<AtomicBool>` shared between the menu callback (sender, check-and-set) and the dispatch arm (clears flag once `activate_gui` returns) — drops bursts at the source. Plus tightened `activation_token::TIMEOUT` from 500 ms to 100 ms (Plasma either grants in <1 ms or never; the previous bound was wasteful).

### F4: KIO *does* pass `activation-token` in `platform_data` — but only when KDE has one to issue

`DBusActivationRunner::startProcess` reads `XDG_ACTIVATION_TOKEN` from the *spawn process's* environment and includes it in `platform_data`. KDE sets that env var on the spawn when the launch context has a token (e.g., Plasma kickoff has a surface with seat focus). Confirmed from `/proc/$PID/environ` capture: sometimes the var is unset → KIO passes empty string → our handler captures empty string → winit's mapping doesn't see a useful token → the launch handshake degrades to "best effort" (same as no token).

**Implication:** the activation-token plumbing is correct-by-construction now (F1 unblocked delivery), but its *value* depends on whether KDE had a token to issue. We log "Propagating launcher activation token to window mapping" on every non-empty token. On empty tokens, the call still succeeds and KIO's `processStarted` still fires — which is what stops the "Remote peer disconnected" tracking on close. So even with an empty token, F1's fix alone resolves the user-visible symptom.

### F5: Slint `set_xdg_app_id` requires the backend to be initialized

When we added `slint::set_xdg_app_id("org.tisvlay.Gui")` early in `run()`, it failed with `PlatformError::NoPlatform`. The implementation at [`i-slint-core/api.rs:1374`](../../../libs/slint/internal/core/api.rs#L1374) passes `|| Err(NoPlatform)` as the lazy-init factory — it deliberately requires the backend to be already-set.

**Fix:** call `slint::BackendSelector::new().select()?` first (the documented public init path). Subsequent `AppWindow::new()` reuses the same context via `with_platform`.

## Reconciliation with original DRs

Two prior DRs are now stale or partial; flagging for future readers:

- **DR4** ("Activate handler bridges into Slint via `slint::invoke_from_event_loop`") describes `window.upgrade().unwrap().window().show()` as the handler body. **Superseded by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**: the handler now bridges via `WinitWindowAccessor::with_winit_window` and calls `request_user_attention(Some(UserAttentionType::Informational))` for re-activation, plus routes the cold-spawn token through `BackendSelector::with_winit_window_attributes_hook` instead of the env var.
- **DR8** ("`request_name` is synchronous, before Slint spinup") implied claim-then-serve. **Revised by F1**: it's now register-then-claim, still synchronous and still before Slint spinup. The `Proxied` early-exit semantics are unchanged.

DR5 (tray-side `xdg-activation-v1` token acquisition) shipped, but **was deleted by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)** once the GUI took over the user-attention path. Tray-side tokens were empty most of the time anyway (tray is surfaceless), and KIO's launcher already passes the `activation-token` key with empty value when the spawn-env var is unset — so the round-trip was load-bearing only for "make the call look complete," which is no longer needed.

## Cleanup follow-ups

PR churn-debt to be addressed in a small follow-up PR after this lands:

| Item | What | Where | Effort |
|---|---|---|---|
| ~~Diagnostic logging downgrade~~ | ~~The `info!(method=…, keys=…, token_present=…)` fields in `Activate` / `Open` / `ActivateAction` were added for live debugging; useful but verbose in production.~~ **DONE**: trimmed to `method=` + `token_present=` (kept at info level — one line per cold-spawn is useful production signal), removed the redundant `debug!` in `raise()`. Net: ~5 fewer lines, cleaner journal output. | n/a | done |
| ~~Stale plan refs in code~~ | ~~A few `// per docs/plans/...` references mention "DR4/DR5" with the pre-F2 semantics.~~ **DONE**: updated `dbus.rs`'s `try_claim_and_serve` docstring to cite F1 + F4 directly. Stale references to the old `serve()` / `try_claim` function names also updated to `try_claim_and_serve`. The "(unchanged from the pre-single-instance version)" section header was rewritten — that history note is no longer relevant. | n/a | done |
| ~~50 ms timeout tuning~~ | ~~The `tokio::time::timeout(50ms, activation_token_rx)` in `lib.rs` is now a safety net — F1's fix means the Activate is delivered almost atomically with name claim, so the receiver wakes in <5 ms typically. Could lower to 20 ms for snappier startup, or even remove the timeout if we can guarantee the Activate is always delivered (it isn't — direct `cargo run` has no inbound Activate). 50 ms is a reasonable default; only tune if startup latency becomes a complaint.~~ **DONE by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**: the entire `block_on(timeout(activation_token_rx))` block is gone — the cold-spawn token now flows through `Arc<Mutex<Option<String>>>` + `BackendSelector::with_winit_window_attributes_hook`, so there's nothing left to wait on. | n/a | done |
| Open / ActivateAction simplification | Both methods delegate to `raise(extract_activation_token(...))` — they're spec-required. The current implementation is fine; flagged only because the boilerplate triplicates. Could fold into a single dispatch helper if it grows further. | `crates/tisvlay-gui/src/dbus.rs` | skip unless KIO adds more methods |
| ~~zbus claim-before-serve warning~~ | ~~Still fires from zbus because `request_name` is called after the bus connection exists.~~ **RESOLVED by F1**: post-fix journal traces no longer emit the warning. zbus's warning condition checks `is_bus() && object_server.get().is_none()`; we now touch the object server (via `at(...)`) before `request_name_with_flags`, so the condition is false. No cleanup needed. | n/a | done |
| ~~Replace `unsafe { std::env::set_var }` with winit helper~~ | ~~winit 0.30.13 ships `winit::platform::startup_notify::set_activation_token_env(ActivationToken)` that wraps the env-var write in a typed API. Adopting saves the `unsafe` block + SAFETY comment (~3 LOC). Cost: requires either Slint's `unstable-winit-030` feature flag (Slint reserves the right to break it) OR pulling `winit = "0.30"` as a direct dev-dep pinned to Slint's vendored version. Both have small maintenance costs for cosmetic gain. **Verdict: skip unless we adopt `unstable-winit-030` for the raise/focus workaround anyway**.~~ **DONE by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**: adopted `unstable-winit-030` (pinned to `=1.16.1`) and routed the token through `WindowAttributesExtStartupNotify::with_activation_token(ActivationToken::from_raw(token))`. The `unsafe { set_var }` block and its SAFETY comment are gone. | n/a | done |
| ~~`xdg-activation-v1` plumbing via WindowAttributes vs env var~~ | ~~The architecturally correct path is `BackendSelector::with_winit_window_attributes_hook(\|attrs\| attrs.with_activation_token(token))`. Today the env-var approach works only because KWin/Mutter read the var on child-spawn (compositor-side fallback) — winit 0.30 does NOT auto-read it. On a compositor without that convention, our handshake silently produces no activation request. The fix gates on `unstable-winit-030`. **Defer**: track Slint's promotion of `with_winit_window_attributes_hook` to stable. The current path is fine on KWin and Mutter, which is what we ship for.~~ **DONE by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md)**: switched to `BackendSelector::with_winit_window_attributes_hook` (gated on `unstable-winit-030`, which we adopted for the pulse-attention path anyway). The handshake now works on any winit-0.30 compositor, not just KWin/Mutter. | n/a | done |
| Plan doc condensation | This doc is now ~250 lines with both forward-looking design and historical findings. Consider splitting into `docs/plans/gui-desktop-integration.md` (design) and `docs/findings/gui-desktop-integration.md` (post-impl learnings), or condensing after the PR merges. | `docs/plans/` reorg | 30 min |
| Custom icon | `Icon=video-display` is a freedesktop-theme placeholder. Swap for bundled artwork once available. The `.desktop` line and `module.nix`'s reference both update without code changes. | `packaging/freedesktop/org.tisvlay.Gui.desktop`, plan/PR-ready when art lands | trivial once art exists |

## Open questions — resolution status

- **Slint Wayland `app_id` support.** RESOLVED — `slint::set_xdg_app_id` exists in 1.16 (PR slint-ui/slint#6851). Used in `tisvlay_gui::run()`. Combined with `StartupWMClass=org.tisvlay.Gui` in the `.desktop`, KWin correctly associates the live window with the launcher entry.
- **Slint window raise / focus API.** PARTIAL — [slint-ui/slint#4382](https://github.com/slint-ui/slint/issues/4382) is still open, but the user-experience need is now met via `winit::Window::request_user_attention(Some(UserAttentionType::Informational))` on slint's `unstable-winit-030` feature — see [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md). KWin pulses the taskbar entry under default Focus Stealing Prevention. Literal "raise to front" still depends on the compositor's FSP policy (Extreme/High = pulse only, Low/None = raise), which is compositor configuration rather than a Slint gap.
- **Window-on-close behavior.** RESOLVED indirectly via F1 — the Activate is delivered atomically with name claim, so the "Activate arriving microseconds before window close" race no longer exists in practice (the Activate has always already been handled before the user can interact with the window).
- **Tray activation surface availability.** RESOLVED by [gui-pulse-on-reactivate.md](gui-pulse-on-reactivate.md) — the tray no longer attempts to acquire its own `xdg-activation-v1` token (the dedicated `wayland-client` connection was deleted along with the helper). User-attention on re-activation now flows through the GUI's own surface via `winit::Window::request_user_attention`, which is the spec-compliant path and doesn't depend on whether the tray itself can obtain a token.
- **`.deb` contents baseline file.** Decided: ad-hoc verification at PR review, no committed baseline file.

# feat(gui): single-instance + tray launcher + .desktop integration

## Summary

Wires `tisvlay-gui` into the Linux desktop as a real app rather than a binary you type at a shell. The originally-planned three coordinated changes shipped, followed by **five post-implementation fix commits** that addressed bugs surfaced by live testing on KDE Plasma 6 Wayland — all archived as findings F1-F5 in [`docs/plans/gui-desktop-integration.md`](docs/plans/gui-desktop-integration.md).

The three core changes:

1. **Single-instance GUI** via the freedesktop `org.freedesktop.Application` D-Bus interface. Running `tisvlay-gui` while one is already running gracefully proxies the activation to the existing process and exits — no duplicate windows.
2. **Tray "Open Settings" menu item** that activates the GUI through the same D-Bus path, with an `Arc<AtomicBool>` coalescer that drops bursts of rapid clicks before they queue. `Quit` menu item moved behind `cfg(debug_assertions)` so it's only visible in `cargo run` builds — release builds rely on `systemctl --user stop` since the tray is a managed user service.
3. **`.desktop` entry + D-Bus session activation file** so launchers (KRunner, Plasma app menu, GNOME Shell) hit the same activation path. Sets Wayland `app_id` to match `StartupWMClass=org.tisvlay.Gui` so KWin associates the live window with the launcher entry and can raise it from the taskbar.

## What ships

### Code

- **[`crates/tisvlay-gui/src/dbus.rs`](crates/tisvlay-gui/src/dbus.rs)** — `try_claim_and_serve(config_path)` is the single atomic flow that connects to the user bus, **registers `Gui1` + `org.freedesktop.Application` interfaces FIRST**, then requests `org.tisvlay.Gui` with `DO_NOT_QUEUE`. On `Exists`, proxies `Activate` to the existing owner and signals the caller to return cleanly. On `PrimaryOwner`, the connection is returned alongside an `oneshot::Receiver<String>` carrying the first inbound Activate's `activation-token` (consumed before `app.run()` to drive winit's startup-notification handshake). Order matters — see F1 in the plan doc for the KDE-source-of-truth pointer to `kio/src/gui/dbusactivationrunner.cpp`.
- **[`crates/tisvlay-gui/src/lib.rs`](crates/tisvlay-gui/src/lib.rs)** — `slint::BackendSelector::new().select()?` then `slint::set_xdg_app_id("org.tisvlay.Gui")?` early in `run()`, before `AppWindow::new()`. The single-instance claim runs before window construction to avoid a half-built-window flash on conflict. After daemon-client setup, a 50 ms timeout on the activation-token receiver, then `$XDG_ACTIVATION_TOKEN` is set in the process env if a token arrived — winit consumes it during first surface mapping.
- **[`crates/tisvlay-tray/src/activation_token.rs`](crates/tisvlay-tray/src/activation_token.rs)** (new) — synchronous `xdg-activation-v1` token request for the tray's `Activate` calls. Best-effort: tray icons are surfaceless (rendered by the panel), so the request goes out without `set_surface()` and the compositor decides by policy. Returns `None` on X11, missing protocol, or 100 ms timeout.
- **[`crates/tisvlay-tray/src/{tray,main}.rs`](crates/tisvlay-tray)** — `TrayCommand::OpenGui` variant; `"Open Settings"` menu item with a coalescing callback (`Arc<AtomicBool>` shared with the dispatch arm). `Quit` menu item wrapped in `#[cfg(debug_assertions)]`; the `TrayCommand::Quit` variant and SIGINT/SIGTERM dispatch stay — only the menu construction is gated.

### Packaging

- **[`packaging/systemd/org.tisvlay.Gui.service`](packaging/systemd/org.tisvlay.Gui.service)** (new) — D-Bus session activation file. Auto-spawns `tisvlay-gui` on `Activate()` if not running.
- **[`packaging/freedesktop/org.tisvlay.Gui.desktop`](packaging/freedesktop/org.tisvlay.Gui.desktop)** (new) — desktop entry with `DBusActivatable=true`, `SingleMainWindow=true`, `StartupWMClass=org.tisvlay.Gui`, `Implements=org.freedesktop.Application;`, `Categories=Settings;HardwareSettings;`, `Icon=video-display` (freedesktop theme placeholder until custom artwork lands), `Keywords` for launcher search.
- **[`packaging/nix/package.nix`](packaging/nix/package.nix)** — `makeDesktopItem` + `copyDesktopItems` for the `.desktop` (matches the 622-package nixpkgs idiom). `postInstall` installs the `.service` template + `substituteInPlace --replace-fail` rewrites its `Exec=` to the Nix-store path (`dbus-daemon` execs the literal path without `$PATH` lookup; `--replace-fail` ensures the substitution actually matches so a future template-format drift fails the build loudly).
- **[`crates/tisvlayd/Cargo.toml`](crates/tisvlayd/Cargo.toml)** — appends two entries to the existing `package.metadata.nfpm.contents` for the `.deb` / `.rpm` install paths.
- **[`packaging/scripts/postinstall.sh`](packaging/scripts/postinstall.sh)** — `update-desktop-database /usr/share/applications` for minimal distros without dpkg/rpm trigger.

### Docs

- **[`docs/plans/gui-desktop-integration.md`](docs/plans/gui-desktop-integration.md)** (new, ~270 lines) — design rationale (8 DRs), 2026-best-practices audit revision, **post-implementation findings F1-F5** (each pinned to source-of-truth file paths in upstream KDE/zbus/Slint), reconciliation notes flagging the prior DRs that were revised, **cleanup follow-ups table** with rationale per item (most marked DONE; the remaining are explicit skip/defer decisions backed by Rust-ecosystem research).

## Known limitations and architectural watch items

- **Raise-on-Activate from the tray is a deliberate no-op.** Slint 1.16 has no public raise/focus API ([slint-ui/slint#4382](https://github.com/slint-ui/slint/issues/4382) — open, dormant since Jan 2024). The obvious implementation (`slint::invoke_from_event_loop` posting `window.show()` on an already-mapped Wayland surface) destroys the surface and exits the event loop, killing the GUI process (finding F2). Cold-spawn from the tray still works (D-Bus session activation opens a fresh GUI); for raising an already-running GUI, users alt-tab or use the **taskbar / app menu / KRunner**, which now work because the Wayland `app_id` set via `slint::set_xdg_app_id` ([slint-ui/slint#6851](https://github.com/slint-ui/slint/pull/6851), shipped Slint 1.16) matches the `.desktop` entry's `StartupWMClass`.
- **Activation-token propagation relies on KWin/Mutter convention.** winit 0.30 does NOT auto-read `$XDG_ACTIVATION_TOKEN`; our env-var approach works only because KWin and Mutter read it on child-spawn. On a compositor that doesn't follow that convention, the xdg-activation-v1 handshake silently no-ops (the user-visible Activate reply still succeeds, so no notification — but the launcher won't see proper startup completion). The architecturally clean fix routes the token through `BackendSelector::with_winit_window_attributes_hook`, which gates on Slint's `unstable-winit-030` feature flag. Documented in the plan's cleanup-table; deferred until Slint promotes the hook to stable.
- **Custom icon.** `video-display` from the freedesktop theme is a placeholder; the `Icon=` line in the `.desktop` swaps without touching code.

## Verification

Run on a Plasma 6 Wayland session after `nixos-rebuild switch`:

```bash
# Single-instance: second tisvlay-gui exits cleanly, leaving one process
tisvlay-gui &; sleep 1; tisvlay-gui      # second exits 0
pgrep -afc tisvlay-gui                   # 1

# D-Bus surface
gdbus introspect --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui

# Cold-spawn via D-Bus auto-activation
pkill -TERM tisvlay-gui; sleep 2
gdbus call --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui \
  --method org.freedesktop.Application.Activate '{}'
pgrep -afc tisvlay-gui                   # 1

# KDE app-menu launch end-to-end (F1 + F4 paths verified)
# After launching via Kickoff / KRunner, journal shows:
journalctl --user --since "-1 min" | grep -iE 'Application call received|Propagating launcher'
# Expected:
#   "Application call received method=Activate token_present=true"
#   "Propagating launcher activation token to window mapping"
# AND no "Did not receive a reply" / "Remote peer disconnected" notifications.

# Coalescer: 5 rapid Events spawn exactly 1 GUI
TRAY=org.kde.StatusNotifierItem-$(pgrep tisvlay-tray)-1
for i in 1 2 3 4 5; do
  gdbus call --session --dest "$TRAY" --object-path /MenuBar \
    --method com.canonical.dbusmenu.Event -- 24 clicked "<0>" 0
done
sleep 2; pgrep -afc tisvlay-gui          # 1 (was: up to 5 pre-coalescer)

# Quit gating
cargo run -p tisvlay-tray                # menu shows "Quit"
cargo build --release -p tisvlay-tray \
  && ./target/release/tisvlay-tray       # menu hides "Quit"
```

## Test plan

- [ ] `just check` passes.
- [ ] `nix build .#tisvlay` succeeds.
- [ ] `find result/share -name '*.desktop' -o -name '*.service' | sort` lists both new files at the conventional XDG paths.
- [ ] After `nixos-rebuild switch`: KDE app launcher / KRunner shows "TisVlay" with the `video-display` icon.
- [ ] Clicking "TisVlay" in the app launcher with no GUI running → window appears; journal shows `Application call received`. **No** `Did not receive a reply` KIO dialog.
- [ ] Close the GUI window. **No** Plasma "Remote peer disconnected" notification.
- [ ] With GUI running in the background, clicking the **taskbar entry** → window raises (KWin handles via `StartupWMClass` match).
- [ ] Tray "Open Settings" with no GUI running → window spawns once.
- [ ] Tray "Open Settings" 5 rapid clicks → exactly one GUI window appears; no zombie respawn cascade on subsequent close.
- [ ] `pkill -TERM tisvlay-gui` after a burst of tray clicks → 0 GUI processes within 10 s.
- [ ] `just package-deb` succeeds; `dpkg-deb -c target/nfpm/tisvlay_*.deb | grep -E 'applications|dbus-1'` lists both new files at `/usr/share/applications/` and `/usr/share/dbus-1/services/`.

## Commit breakdown

15 commits, grouped by purpose. The design + initial implementation landed first; the five fix commits in the middle each address a distinct finding (F1-F5) surfaced by live KDE Plasma 6 Wayland testing; the trailing cleanup pass trims churn-debt and records research-validated decisions.

**Design / plan**:

| Commit | Purpose |
|---|---|
| [`d6d9fce`](../../commit/d6d9fce) | docs(plans): initial GUI desktop-integration plan |
| [`f5a7c43`](../../commit/f5a7c43) | docs(plans): 2026-best-practices audit revision |

**Initial implementation (matching the plan)**:

| Commit | Purpose |
|---|---|
| [`a8016e1`](../../commit/a8016e1) | feat(gui): single-instance via `org.freedesktop.Application` |
| [`7de65a9`](../../commit/7de65a9) | feat(tray): Open Settings menu + `cfg(debug_assertions)` Quit gate |
| [`6585337`](../../commit/6585337) | feat(packaging): GUI `.desktop` + D-Bus session service |
| [`6334bdd`](../../commit/6334bdd) | fix(packaging): drop redundant `.desktop` keys per nixpkgs audit |

**Live-test fix commits (each addresses a finding archived in F1-F5)**:

| Commit | Finding | Purpose |
|---|---|---|
| [`d0eb8e8`](../../commit/d0eb8e8) / [`b822780`](../../commit/b822780) | F2 | fix(gui): no-op Activate handler + set Wayland app_id — work around Slint's kill-on-`show()` on mapped surface |
| [`caa2cab`](../../commit/caa2cab) | F3 | fix(tray): coalesce OpenGui clicks; tighten activation-token timeout — kills the multi-spawn cascade |
| [`50b9d83`](../../commit/50b9d83) | F4 | fix(gui): consume Activate's activation-token to complete launcher handshake |
| [`9fa8b07`](../../commit/9fa8b07) | F1 diagnostic | fix(gui): enhance logging for freedesktop.Application activation methods (added the info-level keys+token diagnostic that ID'd F1's root cause) |
| [`f826c6f`](../../commit/f826c6f) | **F1** | fix(gui): register D-Bus interfaces before claiming name — the actual fix; merges `try_claim` + `serve` to eliminate the zbus queue-drop race that caused "Did not receive a reply" and "Remote peer disconnected" notifications |

**Post-implementation findings + cleanup**:

| Commit | Purpose |
|---|---|
| [`028618c`](../../commit/028618c) | docs(plans): archive post-implementation findings + cleanup TODO list (F1-F5 documentation, source-of-truth pointers, reconciliation with prior DRs, cleanup-table) |
| [`b1d05a8`](../../commit/b1d05a8) | docs(plans): integrate research findings — minimal cleanup wins (Rust-ecosystem research confirming our impl is state-of-the-art; resolves zbus-warning cleanup item; documents winit/Slint feature-flag trade-offs) |
| [`d0f46d6`](../../commit/d0f46d6) | chore(gui): trim Activate handler logging + update stale code refs |

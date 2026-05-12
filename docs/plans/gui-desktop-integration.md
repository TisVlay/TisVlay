# GUI desktop integration: single-instance, tray launch, .desktop entry

> **Status:** drafted 2026-05-11, not started. One PR, three commits. Lands on top of PR #47's Wayland-only GUI and the runtime-deps fixes in ce8c2d2 / ddda197 / the in-flight session rpath patch.

## Context

`tisvlay-gui` exists as a binary but isn't an "app" on the desktop: no `.desktop` entry, no icon in the launcher, no single-instance handling (running it twice opens two windows talking to the same daemon), and the only way to reach it from a running session is to type the binary name in a terminal. Meanwhile `tisvlay-tray`'s menu offers `Quit` — useful in development, wrong for a service-managed binary in production where ksni is supposed to run for the life of the session.

Three coordinated changes close the gap:

1. **Quit gating.** Hide the tray's `Quit` item in release builds where the tray is managed as a systemd user service. Keep it for `cargo run` so developers can still exit cleanly without `systemctl --user stop`.
2. **Tray-driven GUI launch with single-instance.** Add an "Open Settings" tray item. Clicking it should raise the existing GUI window if one is open, and spawn a new GUI otherwise — never duplicate. Independently, launching the GUI from the app menu, KRunner, or `gdbus call` should go through the same code path so behavior is uniform across entry points.
3. **`.desktop` entry.** Make the GUI appear in the system app menu under Settings, with a freedesktop icon-theme icon (`video-display`, matching the tray's choice) until custom artwork lands.

The unifying mechanism for (2) and (3) is the **freedesktop `org.freedesktop.Application` D-Bus interface** plus a `DBusActivatable=true` desktop entry. This is the same pattern GTK's `Gtk.Application` and Qt's `QGuiApplication` use under the hood; KDE Plasma, GNOME Shell, KRunner, and xdg-desktop-portal all route launches through it when the entry opts in. Reusing the standard means the tray's "Open Settings" button hits the same activation path as the app menu, with no parallel forking logic.

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

### DR3: `.desktop` filename follows reverse-DNS to match the bus name

When `DBusActivatable=true` is set, the freedesktop spec requires the desktop entry's *basename* (without `.desktop`) to be a valid D-Bus bus name and to match the bus name the app claims. So the file is `org.tisvlay.Gui.desktop`, not `tisvlay-gui.desktop`. The bus name is `org.tisvlay.Gui` and the object path is the bus name with dots replaced by slashes, `/org/tisvlay/Gui`. All three follow the existing `org.tisvlay.Daemon` / `org.tisvlay.Session` conventions.

### DR4: `Activate` handler bridges into Slint via `slint::invoke_from_event_loop`

The zbus interface handler runs on a Tokio task. The Slint window only accepts mutation from inside its event loop. The existing GUI code already uses `slint::invoke_from_event_loop(move || ...)` at [crates/tisvlay-gui/src/lib.rs:92](../../crates/tisvlay-gui/src/lib.rs#L92) to shuttle daemon push events into the UI — same pattern applies to `Activate`. The handler captures a `slint::Weak<MainWindow>`, posts a closure that calls `window.upgrade().unwrap().window().show()` (plus whatever raise/focus call Slint exposes; verify during implementation), and returns immediately. No new threading primitives.

### DR5: D-Bus session service file mirrors `org.tisvlay.Daemon.service`

Four lines, installed at `share/dbus-1/services/org.tisvlay.Gui.service`:

```ini
[D-BUS Service]
Name=org.tisvlay.Gui
Exec=/usr/bin/tisvlay-gui
```

(`User=` and `SystemdService=` from the Daemon file don't apply — the GUI runs as the calling user, no systemd unit.)

With this file in place, `gdbus call --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui --method org.freedesktop.Application.Activate '{}'` auto-spawns the binary if it isn't running. The tray's "Open Settings" button calls the same method; whether the GUI is already up or not, the user sees one window.

### DR6: nfpm `contents` migration scope

The current nfpm config in [crates/tisvlayd/Cargo.toml](../../crates/tisvlayd/Cargo.toml) has no `package.metadata.nfpm.contents` block — it relies on cargo-nfpm's default-content discovery, which picks up `/usr/bin/<binary>` for every workspace binary plus systemd units it finds. Adding the two new packaging files (`.desktop`, D-Bus service) means we either rely on cargo-nfpm's heuristics finding them under `packaging/freedesktop/` (unlikely — it doesn't know that directory exists) or we switch to explicit `contents` enumeration.

Switching to explicit `contents` means listing **all** installed files, not just the two new ones — a small migration risk: drop a binary or systemd unit and a release ships incomplete. Mitigation: before the migration, run `dpkg-deb -c target/nfpm/tisvlay_*.deb | sort` against today's build, copy that list into the `contents` array, then add the two new entries. Diff after a rebuild confirms parity.

If the migration feels too risky to bundle, the alternative is to ship commits 1 and 2 as one PR and commit 3 (packaging) as a follow-up — keeps blast radius small. Recommendation is still to bundle so the GUI is end-to-end usable from app-menu launch in one PR, but it's a defensible split.

### DR7: `request_name` is synchronous, before Slint spinup

The bus-name claim happens early in `tisvlay_gui::run()` before any Slint code runs. If the request returns `Exists`, the process never spins up a Slint event loop — it just proxies `Activate` and exits. This avoids the user-visible flash of a half-built window appearing for 50ms before exiting.

`request_name` uses the `DO_NOT_QUEUE` flag — we never want to wait for the existing instance to release the name; if it's there, we hand off and quit.

## Implementation order

### Commit 1 — GUI single-instance + `org.freedesktop.Application`

Files:

- **New** `crates/tisvlay-gui/src/single_instance.rs` — encapsulates the `request_name` decision and the `Application` interface impl. Keeps `lib.rs` from growing further.
- `crates/tisvlay-gui/src/lib.rs` — call the new module's entry from `run()` before Slint spinup. On `Exists` → return `Ok(())` after the proxy `Activate` call. On `PrimaryOwner` → register the interface, store the `slint::Weak<MainWindow>` in the interface struct, continue with normal startup.

Verification: `tisvlay-gui &; tisvlay-gui &` — second exits cleanly, first stays. `gdbus call --session --dest org.tisvlay.Gui --object-path /org/tisvlay/Gui --method org.freedesktop.Application.Activate '{}'` raises the window.

### Commit 2 — Tray "Open Settings" + Quit gating

Files:

- `crates/tisvlay-tray/src/tray.rs` — add `TrayCommand::OpenGui` variant near the existing variants; insert an "Open Settings" `tv_item` with icon `preferences-system` between the TV-list section and the Reload separator; wrap the Quit `tv_item` in `#[cfg(debug_assertions)]`.
- `crates/tisvlay-tray/src/main.rs` — add a dispatch arm calling `ApplicationProxy::activate(HashMap::new())` on the session bus. Errors log a warning; user-visible failure is rare given D-Bus auto-spawn covers the "GUI not running" case.

Verification: `cargo run -p tisvlay-tray` shows Quit and "Open Settings"; release build (`cargo build --release && ./target/release/tisvlay-tray`) shows only "Open Settings". Clicking "Open Settings" once spawns the GUI; clicking again raises the existing window with no second process.

### Commit 3 — Packaging fanout

Files:

- **New** `packaging/systemd/org.tisvlay.Gui.service` — the four-line D-Bus session service file.
- **New** `packaging/freedesktop/org.tisvlay.Gui.desktop` — the desktop entry. New subdirectory keeps freedesktop-specific files separate from systemd units. (`packaging/desktop/` would also work; `freedesktop/` is more accurate since the directory may grow to hold MIME types, icon variants, etc.)
- `packaging/nix/package.nix` — `postInstall` adds two `install -Dm0644` lines into `$out/share/applications/` and `$out/share/dbus-1/services/`.
- `crates/tisvlayd/Cargo.toml` — add `[package.metadata.nfpm.contents]` enumerating today's installed files plus the two new ones (see DR6 for migration approach).
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

- Window-on-close behavior. Today `tisvlay_gui::run()` returns when the Slint window closes, ending the process. With D-Bus activation, this is fine: subsequent activations auto-spawn. Worth a quick check that an `Activate` arriving microseconds before window close doesn't drop the window-raise instruction.
- Whether to capture the baseline `.deb` contents (DR6 verification step) into a file in the repo, or do it ad-hoc during PR review. Leaning ad-hoc — the file would decay.

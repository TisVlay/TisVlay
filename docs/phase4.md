# Plan: Phase 4b — tisvlay-tray System Tray App

## Context

All daemon, session, and CLI components are complete and stable. The next step (from docs/design.md Phase 4) is a system tray icon for quick TV control, using the `ksni` crate (KDE/freedesktop StatusNotifierItem spec).

The tray app communicates with `tisvlayd` via `DaemonClient` (same pattern as CLI and session service). It is vendor-agnostic — no vendor crate dependencies.

## Architecture

```
ksni event loop (runs TisvlayTray, calls menu()/scroll()/activate())
    |
    | menu callbacks send TrayCommand via mpsc channel
    v
command processor task (receives TrayCommand, calls DaemonClient)
    |
    | after each command, refreshes state via handle.update()
    v
polling task (every 5s, calls list_tvs + get_state, updates tray via handle)
```

**Key constraint**: ksni callbacks are sync (`Fn(&mut T) + Send`), but `DaemonClient` is async. Solution: callbacks send `TrayCommand` on an unbounded mpsc channel; a dedicated async task processes them.

ksni 0.3.3 uses `zbus` v5 + tokio — same stack as tisvlay. No compatibility issues.

## New Files

| File | Purpose |
|------|---------|
| `crates/tisvlay-tray/Cargo.toml` | Crate config (deps: tisvlay-core[dbus], ksni, tokio, clap, tracing, anyhow) |
| `crates/tisvlay-tray/src/main.rs` | Entry point, arg parsing, task orchestration |
| `crates/tisvlay-tray/src/tray.rs` | `TisvlayTray` struct, `ksni::Tray` impl, `TrayCommand` enum, menu building |
| `crates/tisvlay-tray/src/state.rs` | `CachedTvState`, `fetch_all_state()` helper |
| `systemd/tisvlay-tray.service` | Systemd user service |

## Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` (workspace root) | Add `"crates/tisvlay-tray"` to `members` |
| `CLAUDE.md` | Add Phase 4b as complete, add tray to project structure |

## Menu Structure

Per-TV submenu (connected):
```
▶ Living Room G4  [connected]
  ├─ Power Off
  ├─ Screen On / Screen Off
  ├─ Input ▶  (RadioGroup: HDMI 1, HDMI 2 ✓, HDMI 3)
  ├─ Volume: 60
  │  ├─ Volume Up (+5)
  │  ├─ Volume Down (-5)
  │  └─ Mute / Unmute
  └─ Backlight: 80
     ├─ Backlight Up (+10)
     └─ Backlight Down (-10)
```

Per-TV submenu (disconnected/off):
```
▶ Office C3  [disconnected]
  └─ Power On (Wake)
```

Bottom:
```
──────────
Refresh
──────────
Quit
```

Scroll on tray icon → volume adjust on first connected TV.
**Known v1 limitation**: multi-TV scroll always targets the first connected TV.
Future: add a "default TV" concept or use last-interacted TV.

## TrayCommand Enum

```
PowerOn(tv_id), PowerOff(tv_id)
ScreenOn(tv_id), ScreenOff(tv_id)
SwitchInput(tv_id, input_id)
VolumeUp(tv_id), VolumeDown(tv_id), ToggleMute(tv_id)
BacklightUp(tv_id), BacklightDown(tv_id)
Refresh, Quit
```

## Implementation Steps

1. **Scaffold crate** — Cargo.toml, workspace member, basic main.rs with arg parsing
2. **Tray module with mock data** — TisvlayTray struct, TrayCommand enum, ksni::Tray impl with hardcoded menu to verify tray icon appears and menu renders
3. **State module** — CachedTvState struct, fetch_all_state() calling list_tvs + get_state
4. **Wire main.rs** — DaemonClient, spawn tray, polling task, command processor, replace mock data with live state
5. **Systemd service** — tisvlay-tray.service user service file
6. **Test** — with tisvlayd --session + tisvlay-tray --user

## Key Patterns to Follow

- Arg parsing: match `crates/tisvlay-session/src/main.rs` (--user flag, tracing init)
- DaemonClient creation: match session service pattern (system vs session bus)
- Channel architecture: match ksni `examples/realworld.rs` pattern
- Cargo.toml: match `crates/tisvlay-session/Cargo.toml` for workspace inheritance

## Design Decisions

- **No slider widget** in ksni → use disabled label + up/down items + tray scroll
- **Polling not signals** for initial impl → fixed 5s interval (no backoff — UI should stay responsive regardless of past failures). Immediate refresh after commands.
- **Icon**: `"video-display"` freedesktop theme icon (custom icon later)
- **Tray status**: `Active` = at least one TV connected. `Passive` = daemon unavailable or all TVs disconnected. (NeedsAttention reserved for future alerts.)
- **Daemon unavailable**: show tray with Passive status, "Daemon unavailable" menu, continue polling at same interval
- **SIGHUP**: reload config (to pick up new TVs) — match session service pattern

## Verification

1. `cargo check` and `prek run -a` pass
2. Run `tisvlayd --session` in one terminal
3. Run `tisvlay-tray --user` — tray icon appears
4. Lock/unlock desktop — verify screen blank/unblank still works (session service)
5. Use tray menu: power off, screen toggle, input switch, volume up/down
6. Kill tisvlayd — verify tray degrades gracefully ("Daemon unavailable")
7. Restart tisvlayd — verify tray recovers on next poll

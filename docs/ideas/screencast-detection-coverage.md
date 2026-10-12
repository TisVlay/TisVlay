# Screencast detection beyond KWin

The PipeWire screencast detector matches one producer: KWin, by its capture stream's `media.name` prefix `kwin-screencast-` (see [remote/detection.md](../remote/detection.md)). Research in October 2026 found no blessed, desktop-agnostic way for a user daemon to learn that the screen is being shared. This collects what exists, so the detector can grow when a non-KDE user or an upstream change makes it worth it.

## What exists today

- **No portal API.** xdg-desktop-portal has no interface, and no proposal, for listing or watching active ScreenCast/RemoteDesktop sessions. The closest precedent is `org.freedesktop.background.Monitor` (a `BackgroundApps` property the shell reads).
- **No Wayland protocol.** `ext-image-copy-capture-v1` serves the capturing client only. The open `ext-surface-capture-control` proposal (wayland-protocols !450) tells a surface it is itself being captured, which doesn't fit a daemon.
- **Each desktop's indicator uses internal state.** GNOME Shell reads Mutter's in-process remote-access controller; xdg-desktop-portal-kde raises a per-session status-notifier item, and only for portal sessions; Hyprland is the one compositor with a public event (`screencast>>` on its IPC socket2).
- **PipeWire is the de facto common signal, without a shared convention.** Each producer names its stream differently, and two use the webcam class:

  | Producer | `media.class` | Identifying name |
  |---|---|---|
  | KWin (5.20+) | `Stream/Output/Video` | `media.name` `kwin-screencast-…` |
  | Mutter ≤ 50 / 51+ | `Stream/Output/Video` | `media.name` `meta-screen-cast-src` / `meta-stream-source-source` |
  | xdg-desktop-portal-wlr | `Video/Source` | `xdpw-stream-…` |
  | xdg-desktop-portal-hyprland | `Video/Source` | `xdph-streaming-…` |
  | xdg-desktop-portal-cosmic | `Video/Source` | `node.name` `cosmic-screencast` |

  `media.role = "Screen"` is documented in PipeWire's keys but no producer sets it, and Mutter's rename in 51 shows name matching breaks.

## Options

- **Per-producer predicates.** Add Mutter's names (both generations) and the portal backends' prefixes alongside KWin's. Cheap and precise, but each producer is a name to maintain.
- **Class-and-producer predicate.** Match any `Stream/Output/Video` node, or a `Video/Source` node without `device.api`/`device.id` (to exclude cameras). Covers every producer above without names; the camera exclusion needs checking against libcamera and virtual cameras.
- **Compositor-PID match.** A capture node's owning client carries `pipewire.sec.pid` (set by the server from the socket's credentials, so it can't be forged); compare it with the compositor's PID from `SO_PEERCRED` on `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`. Name-free and covers KWin and Mutter, but misses the portal backends, which capture from their own processes.
- **Hyprland IPC.** Subscribe to socket2's `screencast` events: a first-party signal, Hyprland only.

## Upstream changes worth proposing

Either would make this standard:

- Capture producers setting the already-documented `media.role = "Screen"` (a small patch to KWin, Mutter and the portal backends).
- An xdg-desktop-portal ScreenCast monitor interface, modeled on `org.freedesktop.background.Monitor`.

## Testing

A NixOS VM test (`packaging/nix/test.nix`) running PipeWire and KWin, checking the session's detector connects and reports a capture, would catch packaging regressions such as a missing build input or runtime tool, which unit tests can't.

## Trigger

A GNOME, wlroots, Hyprland or COSMIC user who wants screen-share awareness, either upstream proposal landing, or a packaging regression the VM test would have caught.

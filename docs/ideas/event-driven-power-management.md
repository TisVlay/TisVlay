# Event-driven power-management redesign

Today's `tisvlay-session` listener path (D-Bus screensaver, Wayland idle, logind, idle timer) drives `Tv1.Power.SetIsScreenOn` / `PowerOff` per session events. It works but couples each new event source to a session-bus dependency, and the lock/unlock/idle policy is split between TOML config and `event_handler` code. A redesign should preserve the current UX exactly while opening the door to per-DE adapters with cleaner event semantics.

### Hard requirements

- **Sub-100 ms lockscreen-snap-back.** Mouse jiggle on lockscreen → TV unblanks before the user perceives lag. Today's Wayland idle path delivers this; any replacement must too.
- **Distinguish fast blank from slow power-off.** `Tv1.Power.SetIsScreenOn(false)` keeps the TV powered (~100 ms LG round-trip on resume); `PowerOff` deep-sleeps (5-10 s LG OLED cold boot, requires WoL on wake). Map: lockscreen blank / idle blank → `IsScreenOn(false)`; system suspend / explicit user action / long-idle escalation → `PowerOff`.
- **Stay multi-TV.** Per-TV instances keyed by EDID-matched DRM connector (the existing `Tv1.Desktop.Connector` surface).
- **Handle detached TVs (HDMI splitter, network-only) via per-TV idle timer in the daemon, resets on user-initiated activity.**

### Candidate event sources, ranked by latency

| Source | Latency | Coverage |
|---|---|---|
| KWin Wayland idle (`org_kde_kwin_idle`) | ~10 ms | KWin only |
| wlroots output management (`wlr_output_management_v1`) | ~10 ms | sway, hyprland, river |
| KWin DPMS (`org_kde_kwin_dpms`) | ~10 ms | KWin only |
| logind `LockedHint` / `IdleHint` PropertiesChanged | ~50 ms | KDE; GNOME inconsistent |
| DRM atomic API (non-master client) | polling-only | any compositor on Linux |
| Sysfs `/sys/class/drm/*/dpms` poll | 1 / poll-rate | any compositor on Linux |
| udev DRM uevents | event-driven for hotplug, NOT for DPMS prop changes | any compositor on Linux |

### Likely architecture

A small per-DE adapter pattern:

```
tisvlayd
 ├── PowerObserver trait
 │    ├── KdeWaylandObserver (kde_kwin_idle + dpms)
 │    ├── WlrootsObserver (wlr_output_management)
 │    ├── X11Observer (XScreenSaver extension)
 │    └── LogindFallbackObserver (LockedHint + DPMS sysfs poll)
 │         — fallback when no specific adapter matches
 └── Each observer emits ScreenBlank / ScreenWake / Sleep / Wake
    events on a single channel; daemon dispatches to Tv1.Power.*
```

The session crate either gets folded into `tisvlayd` (its session-bus connection becomes the `KdeWaylandObserver`'s home) or one of the adapters runs as a small user-context helper. Depends on whether system-bus tisvlayd can hold a Wayland connection — needs investigation.

### What NOT to do

- **Don't ship sysfs-DPMS polling as the *primary* (lockscreen-snap-back) path.** DPMS doesn't distinguish blank from off in modern compositors — KDE/X11 collapse to On/Off in practice. Mapping DPMS_OFF to `Tv1.Power.PowerOff` loses the fast-blank UX (panel toggle, ~100 ms) and replaces it with a TV cold-boot (5-10 s). Mapping DPMS_OFF to `IsScreenOn(false)` works correctness-wise, but the wake-from-lockscreen latency is capped by the polling rate (~1 s at 1 Hz vs ~100 ms today). This prohibition is about the *primary* low-latency path. A *secondary, opt-in* sysfs-DPMS poller does ship — see `on_dpms_off`/`on_dpms_on` (system profile), driven by `tisvlayd/src/dpms.rs`. It's `none` by default, scoped to the EDID-matched connector, and explicitly accepts the ~1 s latency; it complements, not replaces, the session lock/idle path.
- **Don't unify "blank" and "off" semantics.** Two distinct LG SSAP code paths with different physics. Pick per-event.
- **Don't drop the per-DE adapter pattern in favor of "one universal observer".** Each DE exposes session state differently and there's no Linux-wide standard event.

### Lineage

Volume sync via PipeWire and brightness sync via KWin Wayland + D-Bus + an out-of-tree kernel module were explored in [#34](https://github.com/ruffsl/TisVlay/pull/34) and scrapped before merging to main. Reasons worth preserving so this isn't re-discovered:

- **Brightness:** PowerDevil patches required (BacklightDetector EDID extension + udev hotplug + KWinDisplayDetector eviction). Distributing a binary patch against Plasma's release cadence is an ongoing maintenance burden. KWin's `kde_external_brightness_v1` isn't truly bidirectional — `set_observed_brightness` is one-shot per drag (commit-gated by `m_done`), so a Wayland-only path can't reflect TV-remote changes back onto the slider. HDR mode bypasses the protocol, switching to a D-Bus path with different semantics. D-Bus reverse loops require fragile race-free guards (echo-suppress, ramp-debounce) that produce visible UI bouncing under stress.
- **Volume:** PipeWire mirror chains two gain stages, and the result depends on which side moved last. Per-app audio gets clipped or attenuated unpredictably.

The full sync history (granular commits, plan docs, removed kernel module) is preserved in [PR #34](https://github.com/ruffsl/TisVlay/pull/34); the cull that retired it is preserved in the squash-merge that brought this stub onto main.

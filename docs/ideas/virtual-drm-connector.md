# Virtual DRM connector for detached TVs

For users with TVs not on a PC GPU (HDMI splitter, network-only, multi-room), the long-term desire is for the compositor to manage them as if they were physical displays — i.e. the compositor sees a virtual display, and the daemon observes DPMS / wake events on it the same way it observes physical connectors.

Three paths for future research:

1. **Out-of-tree minimal virtual DRM driver.** A small kernel module that registers a `drm_device` with one or more `drm_connector` entries, no CRTC / encoder / framebuffer scan-out machinery. Connectors expose `dpms`, `edid`, `status` like real connectors. Compositor manages DPMS state on them. Daemon observes via the same path as a physical connector. Each managed-but-detached TV gets its own connector. EDID is synthesized from TV identity (or the user pins one). Cost: significant. DRM is complex; even a "minimal" connector driver needs to interact correctly with DRM master semantics, modesetting (compositors will at least probe modes), and the userspace API surface enough for compositors not to choke. `vkms` (in-tree virtual KMS) is the reference — too feature-heavy to fork wholesale, but illustrates the integration shape. Rust-for-Linux DRM bindings are still emerging (the nova-driver work in 6.13+ is the leading edge); a C-only or mostly-C driver is the realistic path today.

2. **Userspace virtual-connector bridge.** If a future kernel grows a "virtual connector userspace driver" interface (analogous to `uinput` for input devices), userspace could synthesize a DRM connector without kernel module work. No such interface exists today.

3. **Extend X11 / Wayland protocols.** Lobby for a Wayland protocol that lets non-master clients receive output state notifications across all connectors. Strictly upstream effort, no in-tree work for tisvlay.

The expectation is that someone hits the detached-TV use case in practice, gauges the cost-benefit, and either builds option 1 or accepts a per-TV idle-timer fallback in the event-driven redesign above.

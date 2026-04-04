# Wake/Reconnect Benchmarks

Measured on LG OLED evo C3 42" (OLED42C3PUA) connected via Ethernet
(2.5Gbps, eno2). Desktop: NixOS, Intel/Nvidia, KDE Plasma Wayland.

## Test methodology

Sleep cycles measured via journald timestamps (`-o short-precise`).
D-Bus power cycles via `busctl` calls. Network blips via `nmcli`.
Benchmark script: `scripts/bench_network.py`.

## Key metrics

- **Sleep → disconnect**: time from `PrepareForSleep` to `Connected=false`
- **Wake → subscribed**: time from `System resumed` to `Subscribed to backend`
- **Wake → login**: time from `System resumed` to `ScreenSaver` (session unlock)
- **NM restored → subscribed**: time from NM `ConnectedSite` to subscription

## Variant comparison (sleep/wake cycle)

Tested 6 variants to find the optimal NM watcher + WoL interaction:

| Variant | Wake→login | Wake→sub | Sleep disconnect | Racing | NM clean |
|---------|------------|----------|-----------------|--------|----------|
| Baseline (no NM, 3s retries) | 16.5s | 18.6s | never | no | n/a |
| V1 (NM, no debounce) | — | 16.1s | 8ms | **yes** | no |
| V2 (NM, 500ms debounce) | — | 35.3s | 504ms | **yes** | no |
| V3B (wake window suppression) | — | 18.4s | 9ms | no | yes |
| VA (reconnecting flag, reconnect only) | — | 14.8s | 8ms | no | no |
| **VA+ (reconnecting flag, both guards)** | **10-13s** | **14-18s** | **3ms** | **no** | **yes** |
| VC (deferred wake via NM) | — | 18.0s | 4ms | **yes** | no |

### Variant details

**V1 (NM, no debounce):** NM oscillated during wake (restored→lost→restored
in ~50ms). First NM reconnect killed by second NM lost signal.

**V2 (NM, 500ms debounce):** Absorbed NM oscillation but added 500ms latency.
logind and NM paths still raced. Worst result (35.3s).

**V3B (wake window suppression):** Suppressed all NM events for 30s after
logind `PrepareForSleep(false)`. Worked but anchored to wake timestamp,
so NM blip recovery was also suppressed during the window.

**VA (reconnecting flag):** Per-TV `reconnecting: bool` set during WoL,
checked by `reconnect_disconnected`. NM reconnect suppressed but NM
`disconnect_network_tvs` still fired (no-op since connection held by backend).

**VA+ (both guards):** Extended VA to also skip `disconnect_network_tvs`
for TVs with `reconnecting=true`. Fully clean: no NM interference at all.

**VC (deferred wake):** logind emitted `SystemWake` instead of executing
`on_wake` directly. Forwarder deferred actions until NM restored. NM
oscillation killed in-flight connections. A late NM flap at +18s killed
an already-working subscription. Fragile — rejected.

## WoL retry interval

| Interval | D-Bus power cycle | Sleep wake |
|----------|-------------------|------------|
| 3s (old, 10 max) | 6.9s | 14-16s |
| **500ms (new, 60 max)** | **6.8s** | **10-13s** |

Each retry attempt costs ~50ms (TCP connect + WebSocket close frame).
TV SSAP ready ~5.2s after WoL from short power-off, ~8-12s from deep sleep.
500ms polling catches it within one cycle of SSAP becoming ready.

## Network blip recovery (nmcli off/on, no sleep)

| Metric | Value |
|--------|-------|
| NM lost → Connected=false | <1ms |
| NM restored → subscribed | 136ms |

## TV stays on (automate_when=on_primary, non-primary input)

| Metric | Value |
|--------|-------|
| Wake → subscribed | 8.1s |
| Notes | TV not powered off during sleep (Automated=false). WebSocket reconnect only. |

## Static IP experiment

Hypothesis: static IP eliminates DHCP, saving 2-3s.

| Sample | NM restore | Wake→login |
|--------|-----------|------------|
| 1 (warm interface) | 3.6s | 10.1s |
| 2 (DHCP profiles present) | 7.7s | 10.4s |
| 3 (static only) | 8.1s | 13.4s |

**Conclusion:** Not consistently faster. Bottleneck is Ethernet link
negotiation (~3-4s), not DHCP handshake. Sample 1 was an outlier
(interface was warm from manual activation).

## Hardware observations

- TV displays HDMI input before SSAP accepts WebSocket — greeter
  visible ~8s after WoL, SSAP ready at ~10-12s
- TV accepts WebSocket during boot but sends immediate close frame
  (SSAP service not ready); backend retry loop handles this
- WoL → TV NIC online is near-instant; the wait is for SSAP firmware boot
- NM oscillates during wake: ConnectedGlobal → Disconnected → Connecting
  → ConnectedSite within ~100ms, settles after ~3-8s
- Ethernet link negotiation after sleep: ~3-4s for 2.5Gbps auto-negotiate
- eno1 vs eno2: different NICs have different wake times (eno1: 19s, eno2: 3-4s)

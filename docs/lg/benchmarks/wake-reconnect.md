# Wake and reconnect timings

How long a TV takes to come back after a TV power cycle, a PC sleep and a network blip, measured end to end on the installed stack: from the moment something acts to the moment the daemon holds a live connection and the CLI reads the TV as on.

## Bench

- **TV:** LG OLED evo C3 42" (OLED42C3PUA), on Wi-Fi in a routed IoT VLAN. The PC reaches it through its gateway, and Wake-on-LAN crosses into the VLAN through a broadcast reflector ([isolated networks](../../book/src/operations/isolated-networks.md)). The same panel is also enrolled through a CEC adapter, for the CEC cycles.
- **PC:** NixOS, KDE Plasma on Wayland, wired over 2.5 GbE (an Intel `igc` NIC), with NetworkManager doing DHCP.
- **Stack:** the 0.5.0 release, installed, measured 2026-11-01.

## Method

[`scripts/bench_wake.py`](../../../scripts/bench_wake.py) runs the cycles as root and records when it acted and when the TV read on again; its `report` lines those times up with the daemon's and the kernel's journal. Its docstring has the commands and what to do before a run. It stops the user's `tisvlay-session` for the run, so only system-profile actions fire: the TV powers off on PC sleep and on again on wake, and the screen goes off with DPMS.

The anchors:

- **on command:** the CLI's `power <tv> on` going out.
- **resume:** the kernel's `PM: suspend exit`.
- **LG online:** the LG connector's registration completing after a wake (`LG TV online after wake`).
- **connected:** the world applying the new connection with its first power state (`connected event applying`). From here the power getter can read on.
- **reads on:** the CLI's power getter first reading `on`, polled every 250 ms. It trails *connected* by the poll and the publish.

## Results

Medians, with the range in brackets.

### TV power cycle

`power off`, a wait, then `power on`, which for LG is a WoL and a dial.

| | 20 s off (n=3) | 10 min off (n=1) |
|---|---|---|
| on command → WoL sent | 0.02 s | 0.02 s |
| on command → LG online | 1.07 s (0.35–1.19) | 6.95 s |
| on command → connected | 5.00 s (4.90–6.02) | 10.16 s |
| on command → reads on | 5.21 s (4.94–6.30) | 10.37 s |

The TV's network stays up in standby: within half a second of the WoL it accepts the WebSocket and closes it at once, until its SSAP service is ready. It registers about 1 s after a short standby and about 4 s after the long one, which then spent another 2.7 s answering the hardware-identity query. Between registering and *connected* the daemon waits for the TV's first power-state report, which comes once the TV reports `Active`: 3–4 s after registering, in both cases.

A CEC power cycle, on the same panel, reads on in 0.59 s (0.58–0.59, n=3). A CEC link stays connected through standby, so power-on is an Image View On over the live link, and the getter reads on as soon as the TV reports its new power status. When the panel itself lit wasn't measured. These runs predate the in-place wake's re-send-and-poll loop, which polls from 300 ms, so a TV that takes the first frame should read on about as fast.

### PC sleep

`systemctl suspend` with a 45 s RTC alarm (n=3, over two runs). The TV powers off on the way down and is woken on the way up.

| | |
|---|---|
| suspend → daemon saw sleep | 0.02 s |
| resume → daemon saw resume | 0.31 s (0.31–0.32) |
| resume → WoL sent | 0.31 s (0.31–0.32) |
| resume → last carrier up | 13.98 s (13.88–14.26) |
| resume → route back | 18.55 s (18.17–19.27) |
| resume → LG online | 22.95 s (22.46–22.97) |
| resume → connected | 23.59 s (23.05–25.76) |
| resume → reads on | 26.00 s (n=1) |

The PC's own network sets the pace, not the TV or the daemon. The NIC links up before the kernel finishes resuming, then drops and links up again about 14 s after the resume; the route to the TV's VLAN, through the gateway, is back about 5 s after that. The daemon sends its WoL the moment it hears of the resume, while there's no route to the TV, so that packet is lost. The LG connector resends a WoL before every connect attempt, and the TV registers about 4 s after the route returns, as from the 10-minute standby above. The wake dial came through on attempts 32, 35 and 33. The slowest *connected* is the TV again: its first power report came 2.8 s after registering, where the other two took 1.1 s and 0.1 s.

Only the second run times the getter: the first polled only after the alarm plus 20 s of awake time, long after the TV was back, and the script now polls from the resume.

### Network blip

`ip link set <iface> down`, 5 s, then `up` (n=2). The TV stays on.

| | |
|---|---|
| down → carrier lost | 0.05 s (0.05–0.06) |
| up → carrier up | 3.63 s (3.50–3.76) |
| up → route back | 10.36 s (10.35–10.36) |
| up → connected | 10.52 s (10.51–10.52) |
| up → reads on | 10.60 s (10.57–10.63) |

The daemon parks the connection within 50 ms of the carrier dropping, and reconnects about 150 ms after the route to the TV returns. The rest is the PC: 2.5 GbE autonegotiation takes about 3.5 s, and NetworkManager restores the route to the gateway about 7 s after that. A route that appears for a moment as the link goes up, before carrier, sets off one early dial, which fails after 5 s without delaying the real one.

## Observations

- After the second PC sleep's resume the display went DPMS-off, so the daemon screened the TV off, and it reported `Screen Off` through both blips. That doesn't change the connect timings. It did show the script's final `power on` to the connected, screen-off TV failing with `Operation not supported: power_on (requires capability: power)`. Power-on now routes by transport, and treats a connected, awake network TV as already on ([architecture](../../architecture.md#the-world)).
- On this bench a faster wake is the PC's network and the TV's to win, not the daemon's. The daemon's own share is the WoL after resume (0.3 s) and the reconnect once the route returns (0.15 s); everything else is waiting on the network or the TV.

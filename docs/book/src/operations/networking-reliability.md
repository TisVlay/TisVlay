# Networking & Reliability

Most of the time you never think about this chapter — it's the machinery that makes power and screen automation land reliably across suspends, reboots, cable pulls, and Wi-Fi drops, without you scripting the difference. This page explains what it does, so the behavior isn't surprising.

## Network-aware connections

tisvlay tracks network state per TV, straight from the kernel (netlink) — no NetworkManager dependency:

- **Cable pulls and software disconnects** are noticed in milliseconds, not on a timeout.
- **Each TV is scoped to the interface that actually routes to it.** A Wi-Fi drop won't disconnect a TV reachable over Ethernet, and vice-versa; failover between Wi-Fi and wired reconnects on its own. This holds through VLANs, VPNs, and bridges.
- A TV that goes down **stays down until asked** or until its network route returns — tisvlay doesn't hammer a TV that's genuinely gone, but it does resume the moment the path is back.

## Wake-on-LAN only when the network is up

Wake-on-LAN is a one-shot magic packet with a limited retry budget, and a boot or resume is exactly when the network is *least* ready — the NIC may still be negotiating. tisvlay therefore **holds a wake until a route to the TV actually exists**, instead of burning its retries firing into an interface that hasn't come up yet. When the route appears, the wake resumes automatically.

The upshot: "power the TV on when I resume the machine" works even if your NIC takes several seconds to come up, or if you resume onto Wi-Fi. You don't need to add delays or retries of your own.

## Reliable screen and power actions

Screen commands are **verify-and-retry**, not fire-and-forget:

- After sending `screen_on`/`screen_off`, tisvlay confirms the panel actually changed and retries if it didn't.
- A `screen_on` that finds the TV in standby **escalates to a full power-on** (Wake-on-LAN for network TVs, a bus wake for CEC) — capped at one wake, so a TV that reports itself dark can't trigger an endless wake loop.
- The **desired screen state is latched**, so it survives a TV reconnect or the login-screen→session handoff: if the connection drops mid-command, the reconnect re-asserts what you asked for rather than losing it.

Together these mean automation reaches the state you asked for from any starting point — TV off, TV on the wrong input, network still coming up — without per-case scripting.

## When something's off

If a wake or screen action doesn't land, look at the text beside **Power on** on the TV's page, which shows how far the wake got, and walk through [Troubleshooting → The TV won't power on](./troubleshooting.md#the-tv-wont-power-on). To watch connection and power state change live from a terminal, use `tisvlay watch`.

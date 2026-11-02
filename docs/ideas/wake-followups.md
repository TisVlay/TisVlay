# Wake follow-ups

Small items left after a wake was made to report success only once the TV has acted on it (#233). How a wake works now is in the `TvConnector::wake_and_connect` and `TvConnection::power_on` rustdocs and [architecture.md](../architecture.md#the-world). Trigger: a user reporting one of them, or work nearby.

## A power-off during a dialing wake isn't cancelled

`cancel_held_wake` cancels a wake dial that's held on a missing route or device, but leaves one that's already dialing: its outcome is on its way. LG's confirm loop can then go on resending Wake-on-LAN for a while after an off, up to its 8 s confirm window, so a TV powered off mid-wake may turn back on. An awake-to-off edge seen mid-loop could also stop it. Cancelling the dial itself would need the connection task to take commands while it dials, as an in-place wake does.

## A screen intent's in-place wake never reaches `dispatch-failed`

When a screen-on's verify budget is spent on a standby-reachable TV, `verify_dispatch` sends an in-place `PowerOn` and drops the latch, so a wake that then fails reports `failed` on `progress/power` but never `event/dispatch-failed` for the screen action. No current backend is standby-reachable without emulating screen through power, where the screen-on already took the power path, so it hasn't mattered.

## LG's dial phase resends Wake-on-LAN twice as often as its confirm window

Before the TV accepts a connection, the loop resends WoL before every connect attempt, every 0.5 s. Once connected, the confirm window resends every 1 s, to spare the broadcast reflector a routed TV's packets cross. The dial phase could resend on the same 1 s clock. It runs only until the TV's network answers (about 0.5 s after a short standby, measured in [the wake benchmark](../lg/benchmarks/wake-reconnect.md)), so the extra packets are few.

## An in-place wake's failure toasts without a "Waking…" before it

An in-place power-on publishes only `failed` on `progress/power`, so the tray's "Waking X failed" arrives with no "Waking X…" toast first. That's on purpose: a landed in-place wake (every CEC screen-on, which runs through power) mustn't announce itself. A wording that reads well on its own, such as "X didn't turn on", would fit both paths.

## A connected LG waits for its first state push after the TV reports on

LG's confirm loop reads the TV's power state until it reports on, and then the dial still waits for the TV's first state subscription push before the world gets the connection. On the bench that's about 4 s, most of a wake's 5 to 7 s. The dial could hand over the reading the loop already has as the connection's first state, instead of waiting for the push. Check on hardware that the push that follows doesn't contradict it.

## A power-on sent just as an in-place wake ends

`WakingInPlace` is set when an in-place power-on is sent, and removed when the connection task reports a wake's end. A second power-on sent after the first wake ended, but before the world has heard so, starts a new wake whose marker the first wake's `PowerOnDone` then clears. Its screen verify and input restore stop waiting for it, so a switch can go out mid-wake again. A sequence number on the marker and on `PowerOnDone` would close it.

## Pushes wait out an in-place wake

While an in-place wake runs, the connection task takes commands but not the backend's state subscription or events, so a push arriving then waits for the wake to end, at most its 4 s window. CEC is poll-only, so only a remote-power event is late. Taking both streams in the wake's `select!` closes it for a backend that pushes.

## Not checked on hardware

- A power-on right after a real remote-off. A CEC standby leaves an LG in the same `ActiveStandby` state and ran the same code path, with 8 of 8 wakes landing.
- CEC's dial-path wake, with the adapter dropped and brought back before the power-on.

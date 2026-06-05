# Adaptive poll rate during power transitions

A poll-only backend (no `subscribe_state()` push channel — the [Epson projector](../epson/protocol.md#no-push--event-channel) is the first) relies on the daemon's periodic state poll to notice power changes. That poll runs at a **single fixed cadence** regardless of what the device is doing. During a multi-second `WarmingUp` / `CoolingDown` transition, that fixed cadence is a poor fit at both ends: too slow to give a responsive "warming → active" UI update, yet too fast to run all the time once the device is steady.

A device that pushes (LG) doesn't have this problem — it gets a transition update the instant the substate flips. Poll-only backends would benefit from **polling faster while a transition is in flight and backing off once steady**.

## Where this is, and isn't, already handled

The Epson connector already polls quickly during its *own* `wake_and_connect` warmup — `WARMUP_POLL_INTERVAL` (1 s) until the projector reaches `Active`, in [`connector.rs`](../../crates/tisvlay-epson/src/connector.rs). That covers the **daemon-driven** power-on path. The gap is the **passive** path: a projector warmed up or cooled down *by its own remote* transitions through `WarmingUp` / `CoolingDown` with no daemon-driven loop watching it — only the steady-state periodic poll, which may be seconds-coarse and won't tighten up just because the device happens to be mid-transition.

The reliable-dispatch verify loops are *not* the place for this either: they are event-driven on a [`watch::Sender<PowerState>`](../../crates/tisvlayd/src/tv_manager.rs) and already return on the first matching transition. This idea is specifically about the **steady-state refresh cadence** that feeds that channel for poll-only devices.

## Path forward

When picked up:

- Give the per-device poll task two cadences — a fast interval (sub-second to ~1 s) and a slow steady-state interval — and switch to fast whenever the last read was `WarmingUp` / `CoolingDown` (or `Unknown` during a suspected transition), reverting to slow on a steady `Active` / `ScreenOff` / `Standby`.
- Source both intervals from the backend, alongside the existing per-backend [`verify_retry_config()`](../../crates/tisvlay-epson/src/backend.rs) tuning, so a projector's warmup-poll cadence and a faster CEC device's can differ without daemon changes. (`verify_retry_config` is the precedent for "transport-latency tuning is a backend knob.")
- Keep the slow interval the default so push backends and steady devices don't pay for the fast path.

The baseline ships a single fixed interval deliberately — get the poll-only path correct first, then optimize the cadence once hardware timings (warmup ~30–40 s, cooldown ~10 s on the LS12000, both TBD) say what "fast enough" actually means.

## Trade-offs

- Faster polling means more ESC/VP21 round-trips during a transition — fine for a wired LAN device, but the fast interval shouldn't be so tight it hammers a slow projector NIC. Tune from the measured warmup/cooldown durations (a [hardware-validation item](../epson/firmware-quirks.md)).
- Adds a small state machine to the poll task (which cadence am I in). Only worth it once a poll-only backend with real transition latency is on hardware and the fixed cadence is demonstrably too coarse or too chatty.

## Trigger condition

Pick this up when one of:

- The fixed steady-state poll feels laggy for passive (remote-initiated) projector warmup/cooldown in real use.
- A second poll-only or projector-class backend lands and wants its own cadence.
- Hardware-validation timings for the LS12000 warmup/cooldown are measured and the right fast/slow split becomes concrete.

## Related

- Epson "no push channel" finding (why poll-only is the only path): [docs/epson/protocol.md](../epson/protocol.md#no-push--event-channel).
- Why a poll-only backend still satisfies the reliability contract: [docs/backends/implementing-a-vendor.md](../backends/implementing-a-vendor.md) § Decision rationale.
- The warmup poll the connector already runs on the daemon-driven path: [crates/tisvlay-epson/src/connector.rs](../../crates/tisvlay-epson/src/connector.rs).
- Per-backend tuning precedent: `verify_retry_config()` in [docs/design.md](../design.md) § Reliable screen-state actions.

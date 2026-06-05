# Projector lamp / laser-hours diagnostic surface

A projector's lamp or laser has a finite life, and ESC/VP21 (and PJLink) can report the hours used. There is **no place to surface it today**: [`TvState`](../../crates/tisvlay-core/src/state.rs) has slots for power, screen, volume, mute, input, and backlight — all things a TV has — but nothing for a cumulative diagnostic counter like lamp-hours. The [Epson backend](../epson/protocol.md) reads the four control commands that map onto existing capabilities and ignores the diagnostic vocabulary.

This is a projector-shaped gap: lamp-hours is not a control, it is a read-only diagnostic. Folding it into the control-oriented `TvState` would be a poor fit; it wants a diagnostics channel.

## Path forward

Three options, roughly in increasing weight:

1. **A `Diagnostic`-visibility schema field.** The vendor config schema already has a `Diagnostic` visibility tag (the Epson `model` field uses it) that surfaces read-only values in the INFO pane. A `lamp_hours` row would ride the existing schema → INFO-pane projection with no D-Bus or GUI rendering changes. The catch: schema fields are persisted *config*, and lamp-hours is *live* device state — it would need a refresh path that writes back into the config-shaped value, which the schema model isn't built for. Workable only if treated as "last-read snapshot," which is misleading for a monotonically-increasing counter.

2. **A new live `TvState` field.** Add `lamp_hours: Option<u32>` to `TvState`, populated by the backend's `get_state()` and forwarded like any other live property. Honest about being live state, and a second projector vendor would reuse it. The cost is a core-type change that every backend ignores except projectors — the kind of vendor-shaped widening of a vendor-agnostic type the design deliberately avoids (cf. why volume isn't special-cased for speakerless devices). Justified only once *two* projector-class backends want it.

3. **A vendor-info projection.** Surface it through the existing vendor-info / diagnostics display path (`display_diagnostics` on the connector) as a computed, read-at-display-time value rather than a cached property. Keeps it out of `TvState`, keeps it honestly live, and stays entirely within the vendor crate — no core change. The weakest discoverability (it is a diagnostics readout, not a first-class property) but the cleanest blast radius.

Option 3 is the natural first step; option 2 becomes right when a second projector vendor makes the field pay for its core footprint.

## Trade-offs

- A future **PJLink transport** ([protocol.md § Transport landscape](../epson/protocol.md#transport-landscape-dated)) would expose lamp-hours natively as part of its standard query set — adding that transport would make this idea nearly free on the read side, so it may be worth waiting for PJLink rather than building a bespoke ESC/VP21 lamp-hours path now. The [`pjlink`](https://lib.rs/crates/pjlink) Rust crate is the candidate dependency for that transport.
- Whatever the surface, the *units* and query availability are unverified on the LS12000 (see [firmware-quirks.md](../epson/firmware-quirks.md)); the diagnostic can't be designed concretely until a probe confirms the response shape.

## Trigger condition

Pick this up when one of:

- A user wants lamp / laser-life visibility for maintenance planning.
- A second projector-class vendor backend lands and the field would be reused (strengthens the case for option 2).
- A PJLink transport is added (lamp-hours comes nearly for free, shifting the cost calculus).

## Related

- Epson protocol reference (the unused diagnostic vocabulary): [docs/epson/protocol.md](../epson/protocol.md).
- The hardware-validation open question on lamp-hours availability: [docs/epson/firmware-quirks.md](../epson/firmware-quirks.md).
- `TvState` definition: [crates/tisvlay-core/src/state.rs](../../crates/tisvlay-core/src/state.rs).
- Why vendor-shaped fields don't get special-cased into core types: [docs/backends/implementing-a-vendor.md](../backends/implementing-a-vendor.md) § Decision rationale.

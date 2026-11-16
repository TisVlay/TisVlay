# IR remote control

A user asked for TisVlay to drive AV equipment over infrared: TVs, soundbars and receivers with no network or CEC control. The research from 2026-11-16 is in [docs/research/](../research/README.md#ir-remote-control): what hardware can send IR from a Linux desktop, how the kernel exposes it, the codes for the target TVs, and which Rust crates to use. A Flirc USB gen 2 is on order as the first bench device.

Nothing is decided yet. Trigger: the review fixes now in flight have landed and the Flirc has been on the bench.

## What the research settled

- **The Flirc gen 2 can send, weakly.** Its on-board LED sends at 38 kHz only. Flirc's staff put the range at about 2 m line of sight; the product page now says 35 ft. Linux sees it as a USB keyboard on `/dev/hidraw`, not as a kernel IR device.
- **Flirc's SDK can't ship.** It's proprietary and binary-only, licensed for use with Flirc hardware only. `flirc-rs` (MIT) documents the USB protocol from a clean-room capture; its four transmit commands are small enough to reimplement.
- **Encode with `irp`** (MIT, by the kernel's rc-core maintainer). It turns NEC, Samsung32, Sony, RC5/6 and Pronto into pulse timings. Write the `/dev/lircN` transmit path in-house on `libc` rather than depending on `cir`.
- **LG has discrete codes**: NEC address `0x04`, power on `0xC4`, power off `0xC5`, HDMI 1–4 `0xCE`/`0xCC`/`0xE9`/`0xDA`. Mute is a toggle and volume only steps. Some LG factory codes can damage the TV, so a backend sends from a fixed allowlist and never an arbitrary code.
- **Code data:** Flipper-IRDB is CC0 and Home Assistant's `infrared-protocols` is MIT, so either can be vendored. IRDB has no clear license.

## Open questions

### The backend model

IR can't read anything back. A wake is done when the TV reports it's on (#233), and the screen verify loop needs a state to read, so an IR backend that only sends would fail both. The options:

1. **An actuator on a TV that has a state source**: IR sends, while CEC, the LG network backend or the DRM connector status reads. The trait set has no notion of a state source apart from the command path today.
2. **A standalone backend with assumed state**: power reads as assumed once the code is sent, and the confirmation treats that as done. That's a new marker on the wire, so a `feat`.
3. **A standalone backend with no read-back**: it declares power and input only and reports power as unknown. The simplest, but screen verification always fails.

Mute and volume are absolute in `TvConnection`, and IR offers only a toggle and steps. They need assumed state or a relative-step capability, which [CEC's relative volume](cec-followups.md#relative-volume-cross-cutting-capability) needs too.

### Who it's for

The LG TVs already have network and CEC control, so IR adds little for them except as a test bed with known codes. Equipment with no other control path (soundbars, receivers, older TVs) is where IR earns its place, and it changes which protocols matter: Samsung32 needs raw timings because the kernel can't encode it, Sony SIRC runs at 40 kHz, and RC5/6 at 36 kHz.

### The first transport

- **Flirc over hidraw**: the hardware on order. 38 kHz only and short range.
- **Kernel LIRC (`/dev/lircN`)**: one path for every in-kernel transceiver, and the `rc-loopback` driver is a free fake for CI. Hardware to buy is thin: mceusb clones, IguanaWorks closed in 2025, Irdroid's store is paused.
- **An ESPHome network emitter**: any carrier, placed at the TV, $11–26. ESPHome's IR proxy is still experimental.
- **The seam first**: an `IrEmitter` trait with a code table and a fake (the research sketches one after CEC's `CecLink`), transports after the bench tests.

### Bench tests that decide it

These run unattended and log to a file, since the test TV can be the bench's only display.

- How far the Flirc reaches a C3 or G4 with the LG power-on code.
- Whether the C3 and G4 honor the discrete power and HDMI codes from deep standby, and how many repeats they need.
- Whether a C3 or G4 drops HDMI hotplug or changes its EDID in standby on this GPU. If it does, the DRM connector status could serve as IR's power read-back.
- Whether Flirc's library needs libusb on gen 2 or only hidraw. `strace` a `flirc_util sendir`. The answer decides the unit's `DeviceAllow` lines.

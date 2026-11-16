# Rust IR ecosystem survey for a `tisvlay-ir` backend

Surveyed 2026-11-16. Crate figures are from the crates.io API on that date: "dl" is all-time downloads, "90d" is recent downloads. Repo activity is from the GitHub and GitLab APIs. The license column is the crate's SPDX expression. TisVlay's allow-list (`packaging/licensing/deny.toml`) is MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unicode-3.0, CDLA-Permissive-2.0 and Unlicense, with per-crate exceptions (MPL-2.0 for `option-ext`, LGPL for `linux-cec`). `[bans]` fails on duplicate majors unless a reason is given in the skip list.

## TL;DR

- **Encoding: use `irp`.** It is MIT, pure Rust, by Sean Young (the kernel rc-core maintainer). It encodes any protocol written in IRP notation (NEC1/LG, NECx/Samsung, Sony SIRC, RC5/6, Kaseikyo…) to µs mark/space, parses and encodes Pronto hex, and decodes, which helps tests. Leave the `bpf` feature off, because it pulls LLVM through `inkwell`.
- **LIRC: write the `/dev/lircN` transmit path in-house, about 150 lines on `libc`.** Don't depend on `cir`. It is sync, would add a third `toml` major, carries `assert!` panics in `send`, and can't send scancodes.
- **Flirc: use neither the SDK nor FFI.** The flirc/sdk is proprietary: prebuilt `.so`/`.a` only, licensed solely for use "with Flirc manufactured hardware in commercial environments". It can't pass cargo-deny, isn't free for Nix, and can't ship in a .deb. A clean-room, MIT-licensed protocol description exists in `flirc-rs` (2026-09). Reimplement its 4 transmit commands over hidraw in-house, or with `async-hid` (pure Rust, tokio).
- **Network blasters:** `esphome-client` (MIT, tokio) already carries ESPHome's new `InfraredRFTransmitRawTimingsRequest`. This is the best-placed network path now that Home Assistant has an infrared entity platform (2026). Broadlink and Tasmota have no usable crate. Both are small in-house protocols.
- **Code tables:** don't ship IRDB (custom non-OSI license with obligations). Flipper-IRDB is CC0, and HA's `infrared-protocols` is MIT. HA's library has LG's discrete codes (NEC, address 0x04, POWER_ON 0xC4, POWER_OFF 0xC5, HDMI_1..4). A small cited in-house table is fine.

---

## 1. Linux LIRC / rc-core from Rust

| Crate | Version (released) | dl / 90d | License | Repo | Notes |
|---|---|---|---|---|---|
| `cir` | 0.1.4 (2025-03-20) | 8,293 / 54 | MIT | gitlab.freedesktop.org/linux-media/tools/cir | Library behind the new Rust `ir-ctl`. Modules: `lirc`, `rcdev`, `keymap`, `lircd_conf`, `rc_maps`. Repo active (last commit 2026-10-20, e.g. a "lircd.conf: Fix RC-MM toggle bit transmit" fix), but nothing released for 20 months. |
| `irp` | 0.3.4 (2025-03-20) | 14,651 / 150 | MIT | same repo | Encoder/decoder. See §2. |
| `lirc-sys` | 0.0.1 (2020-06-11) | 3,259 / 2 | — | gitlab.com/Russel/rust-lirc-sys | Abandoned stub. |
| `rust-lirc-client-sys` | 0.2.0 (2022-10-01) | 3,532 / 40 | — | github.com/tmayoff/rust-lirc-client-sys | FFI to the C `liblirc_client`, which talks to the lircd daemon. Needs lircd and a C library. Avoid. |
| `evdev` | 0.13.2 (2025-09-15) | 2.47M | Apache-2.0 OR MIT | github.com/cmr/evdev | Has a `tokio` feature (EventStream). Relevant only to receive: rc-core's input device, or Flirc's keyboard output. |
| `rc-core` | — | — | — | — | The name is taken by an unrelated S3 CLI crate. There is no rc-core crate. |
| `linux-infrared`, `lirc` | — | — | — | — | Not on crates.io. |

### `cir::lirc::Lirc` in detail (read from the source on `main`)

- **API:** `Lirc::open(path)` → `send(&mut self, &[u32])`, `set_send_carrier`, `set_send_duty_cycle`, `num_transmitters`/`set_transmitter_mask`, feature probes (`can_send`, …), raw and scancode receive, and optional BPF attach behind the `bpf` feature, which uses `aya`. It implements `AsFd`/`AsRawFd`. It is sync and blocking: the source notes that "the write syscall on a lirc chardev waits until the IR is transmitted".
- **No scancode transmit.** `LIRC_MODE_SCANCODE` is used only for receive. The kernel does support it for transmit: write a `struct lirc_scancode` with `rc_proto` and `scancode` set (kernel lirc-dev-intro). The in-kernel encoders do the timing.
- **Policy friction:** `write()` panics through `assert_eq!` on a short write, and `send()` asserts a non-empty buffer. Both cut against `docs/conventions.md` (panics must state their invariant; no panics in a daemon path).
- **Dependencies:** `nix 0.29` (already in TisVlay's lock through zenoh), `evdev 0.13`, `bitflags 2`, `toml 0.8`, `itertools 0.13`, `peg`, `irp`. `toml 0.8` would be a **third `toml` major** next to 0.9 and 1.2, which needs a `[bans]` skip entry.
- **No C library needed** without `bpf`. With `bpf`, `irp/bpf` pulls `inkwell` and LLVM 15.

**Verdict:** the code is a good reference, but a poor dependency. The transmit path TisVlay needs is small: open, `LIRC_GET_FEATURES`, `LIRC_SET_SEND_CARRIER`, `LIRC_SET_SEND_DUTY_CYCLE`, `LIRC_SET_TRANSMITTER_MASK`, and `write()` of an odd-length `u32` array. Optionally add `LIRC_SET_SEND_MODE(LIRC_MODE_SCANCODE)` plus a `lirc_scancode` write. Write it in-house on `libc`, which tisvlay-cec already depends on, in a file with `#![expect(unsafe_code)]`. Run the blocking `write` in `spawn_blocking`.

**Async:** none of these crates is async. LIRC transmit is a blocking write that lasts the IR burst (around 70–110 ms for one NEC frame plus repeats), so `spawn_blocking` is right. For receive, `tokio::io::unix::AsyncFd` on the lirc fd works, since `poll` is supported.

**Prior art, not a dependency:** `tv-rs-ir` 0.7.1 (crate created 2026-10-28, MIT, 1 star, github.com/Zakarialabib/tv-rs). Its `LircEmitter` shells out to `ir-ctl`, it has hand-written NEC/RC5/SIRC encoders, and it ships a `MockIrEmitter`. It is a young multi-protocol TV workspace and too immature to depend on.

## 2. IR protocol encoding and decoding

| Crate | Version (released) | dl / 90d | License | MSRV | Protocols | Fit |
|---|---|---|---|---|---|---|
| **`irp`** | 0.3.4 (2025-03-20). Repo active to 2026-10 | 14,651 / 150 | MIT | 1.85 (ed. 2024) | **Any protocol written in IRP notation:** NEC1/NECx/NEC32, Samsung32/36, Sony SIRC 12/15/20, RC5/RC5x/RC6 (all modes), Kaseikyo/Panasonic, JVC, Sharp, RC-MM, XMP, … Plus Pronto hex parse/encode (`Pronto::parse` → `encode(repeats)`), raw/mode2 parsing, and an NFA/DFA `Decoder`. | **Best fit.** `Irp::parse(irp_str)?.encode_raw(vars, repeats)` → `Message { carrier, duty_cycle, raw: Vec<u32> }`. Pure Rust: `num`, `bitvec`, `peg`, `xml-rs`, `serde`, `itertools 0.13` (`xml-rs`, `itertools 0.13`, `num-*` are already in TisVlay's lock). All MIT/Apache. `Protocol::parse(path)` reads IrpTransmogrifier's `IrpProtocols.xml`, but that file is **GPL-3.0** and must not be vendored. Embed the handful of IRP strings needed instead. |
| `infrared` | 0.14.2 (2023-01-22). Repo **revived 2026-11-14** (0.15.0 bump, ed. 2024, MSRV 1.85, unreleased) | 48,430 / 1,793 | MIT OR Apache-2.0 | — | NEC (+ Samsung, NEC16, Apple), RC5, RC6, Denon, SBP (Samsung Blu-ray). **No Sony SIRC, no Kaseikyo, no Pronto.** | Built for no_std/embedded. `sender::ProtocolEncoder<const F: u32>` fills a tick buffer at sample rate F (F = 1 MHz gives µs). The encoder emits one frame with a leading 0, no repeat frames and no trailing gap. Fine as a cross-check, too narrow as the main encoder. |
| `oxstack-ir` | 0.2.1 (2026-10-25) | 53 | MIT | — | NEC only | New, no_std, M5Stack-oriented. |
| `pronto` | 0.0.0 (2021-02-02) | 1,321 | MIT/Apache | — | Pronto | Placeholder. `irp` covers Pronto. |
| `infrared-rs` (smarthome-go) | 0.3.1 (2023-09-06) | 5,173 | **GPL-2.0** | — | Receive only | Disallowed license. |
| `gree-ir`, `brickbeam` | — | — | — | — | Air-conditioner / LEGO PF | Irrelevant. |

### LG and discrete codes

- **LG TVs use plain NEC1 with device 4** (subdevice = 255−4 = 0xFB), at 38 kHz. IRDB's `codes/LG/TV/4,-1.csv` lists `POWER,NEC1,4,-1,8`. Flipper-IRDB's file for the C3 (`TVs/LG/LG_OLED48C37LA (LG_OLED C3 models).ir`) has `protocol: NEC, address: 04, command: 08` for Power.
- **Discrete codes** come from HA's MIT-licensed `infrared-protocols` (`codes/lg/tv.py`, `LG_ADDRESS = 0xFB04`): `POWER_ON = 0xC4`, `POWER_OFF = 0xC5`, `HDMI_1 = 0xCE`, `HDMI_2 = 0xCC`, `HDMI_3 = 0xE9`, `HDMI_4 = 0xDA`, `VOLUME_UP = 0x02`, `VOLUME_DOWN = 0x03`, `MUTE = 0x09`, `INPUT = 0x0B`.
- `irp` handles all of these directly through the NEC1 IRP: `{38.4k,564}<1,-1|1,-3>(16,-8,D:8,S:8,F:8,~F:8,1,^108m,(16,-4,1,^108m)*)[D:0..255,S:0..255=255-D,F:0..255]` with D=4, F=0xC4. In the kernel's scancode mode the same code is `RC_PROTO_NEC`, scancode `0x04C4`.
- `infrared`'s `Nec` encoder handles them too.
- Whether the C3 and G4 honor C4/C5 is a field check (open questions).

## 3. Flirc from Rust

### flirc/sdk (github.com/flirc/sdk)

- **Last commit:** 2024-09-23 (v3.27.16). 72 stars.
- **Contents:** headers (`flirc.h`, `ir.h`) and prebuilt `libflirc`/`libir` binaries (`.so`/`.a` for Linux x86_64, aarch64 and arm, plus macOS and Windows). Library source is **not** included. The `cli/` (flirc_util) and `ir/` example sources are.
- **License (added in 3.27.16):** "Copyright (c) 2024 Flirc Inc. All rights reserved … to use the Software solely in conjunction with Flirc manufactured hardware in commercial environments … may not be used, copied, modified, merged, published, distributed … except as permitted". **Proprietary, with no SPDX id.** It fails cargo-deny, is `unfree` in Nix, and can't be redistributed in TisVlay's packages. Linking it, statically or dynamically, would also taint the open-source packages. **FFI is ruled out.**
- **Transmit API:**
  - `fl_transmit_raw(uint16_t *buf, uint16_t len /* ≤100 markers */, uint16_t ik /* gap: µs below fw 4.10.0, ms from 4.10.0 */, uint8_t repeat)`
  - `flirc_send_pronto(...)`
  - `fl_ir_transmit_kill()`
  - `fl_ir_packet_poll()` for raw capture
  - `libir` adds `ir_encode`/`fl_transmit_prot(enum rc_proto, scancode, repeat)`, whose `rc_proto` enum mirrors the kernel's.
  - The headers mention libusb error codes. libflirc uses libusb for Gen 1 and HID for Gen 2.
- **Transmit range:** a user report (gist by arfoll, 2022-06-25, Flirc 2.0 "dori", fw v4.9.3) sends an LG on/off code with `flirc_util sendir --pattern=…` and says "Range is terrible as advertised, but it does work". Flirc's 2017 blog lists IR transmit as a Gen2 feature. Hardware suitability is the other agent's question.

### `flirc-rs` 0.1.0 (released 2026-09-24)

- **Stats:** 15 downloads, 0 stars, MIT, MSRV 1.88, github.com/EriksRemess/flirc-rs. Lib name `flirc`.
- **What it is:** a pure-Rust protocol reimplementation (no libflirc) plus a CLI. Its `docs/protocol.md` is an explicit clean-room description ("observed … on firmware 4.10.5 … not a copy of the proprietary library implementation").
- **Command interface:** interface 0 carries 33-byte HID reports (`0x02` + 12-byte ASCII command + payload; response `0x01`). Transmit uses 65-byte reports (response ID `0x05`).
- **Commands:** `irtranset` (u16 count, u16 gap, u8 repeat), then `irtransraw` 52-byte chunks of u16 µs with a leading artificial 0, at most 99 user timings. `irtranskil` stops a transmission, and `getversion` reads the firmware version.
- **Raw capture:** interface 2, enabled through `usb_if`, emits report ID `0x07`.
- **Caveats as a dependency:** single author, 0.1, sync API on `hidapi` 2.6.7 with default features (bundles and compiles the C hidapi, needs `libudev-dev`). `ctrlc` is a non-optional dependency of the library. **Use it as the protocol reference, not as a dependency.** If it were adopted, TisVlay would need `spawn_blocking` around every call anyway.

### HID/USB transport crates

| Crate | Version (released) | dl | License | C library? | Fit for Flirc |
|---|---|---|---|---|---|
| `hidapi` | 2.6.7 (2026-08-27) | 10.5M | MIT | **Yes** by default (bundled C hidapi through `cc` + libudev). `linux-native` is Rust but needs the `udev` crate (libudev C) | Works, but adds C and stays sync. |
| `async-hid` | 0.5.3 (2026-06-11). Master at 0.6.0 (nix 0.31) | 66K / 25K | MIT | **No.** The Linux backend is pure Rust: hidraw ioctls and netlink uevents through `nix` | **Good fit:** `tokio` feature, async read/write of reports, enumeration. Deps: `nix 0.29` (in lock), `futures-lite`, `log`. |
| `nusb` | 0.2.9 (2026-10-24) | 2.26M | Apache-2.0 OR MIT | No (pure Rust usbfs) | Flirc's interfaces are HID class and bound by `usbhid`. nusb would have to detach the kernel driver, which kills Flirc's keyboard function. **Prefer hidraw.** |
| `rusb` | 0.9.4 (2024-04-27) | 10.3M | MIT | **Yes** (libusb) | No. |

**Practical choice:** Flirc Gen2 exposes `/dev/hidrawN` per interface. The transmit protocol is write report → read report. An in-house transport is very small:

- find the node through sysfs (`20a0:0006`, `bInterfaceNumber == 00`);
- open it O_RDWR (O_NONBLOCK if `AsyncFd` is used);
- `write` 65 bytes, then `read` the response.

It can sit on `tokio::fs::File`/`AsyncFd`, or on `async-hid` if hotplug and enumeration are wanted. Either way it is pure Rust with no C dependency.

## 4. Network IR blasters from Rust

| Target | Crate | Version (released) | License | Async | Notes / fit |
|---|---|---|---|---|---|
| **ESPHome** (native API, TCP 6053, Noise) | **`esphome-client`** | 0.2.1 (2026-09-04), repo active | MIT | tokio | Features gate the protocol: `api-1-14` (ESPHome 2026.1) and later include `rpc infrared_rf_transmit_raw_timings(InfraredRFTransmitRawTimingsRequest)`, with fields `key`, `carrier_frequency` (Hz), `repeat_count`, and `repeated sint32 timings` (µs, + = mark, − = space). Also `ListEntitiesInfraredResponse` and `InfraredRFReceiveEvent`. Generic `try_write(message)`. Deps: `prost 0.14`, `snow` (Noise), `tokio`, `tracing`, `thiserror`, optional `mdns-sd` discovery. **Clean fit.** |
| ESPHome | `esphome-native-api` | 3.1.0 (2026-11-06) | MIT | tokio | Server and client (UbiHome). Each ESPHome version is a feature, it uses `log` rather than `tracing`, and `prost 0.13`. Second choice. |
| ESPHome | `esphome` | 0.1.0 (2021-08-22) | MIT | — | Dead. |
| ESPHome | `esphome-proto` | 0.1.1 (2025-08-17) | **LGPL-3.0** | — | Disallowed license. |
| **Broadlink RM** (python-broadlink protocol) | `rbroadlink` | 0.4.2 (2024-06-18). Repo idle since | MIT | **No** (std `UdpSocket`, `String` errors) | Old crypto crates (`aes 0.7`, deprecated `block-modes 0.8`), `get_if_addrs 0.5`. **Don't depend on it.** The protocol is small: UDP/80 discovery, AES-128-CBC with a fixed key and IV, an auth handshake that returns a session key, and IR payload `0x26`, repeat, LE length, then ticks of 269/8192 ms (about 32.84 µs), with `0x00`+u16-BE escapes for long durations. Writing it in-house on `tokio::net::UdpSocket` + `aes 0.8` (already in TisVlay's lock) + `cbc` is roughly 300 lines. python-broadlink (MIT, last push 2024-08) is the reference. |
| **Tasmota** (IRsend over HTTP/MQTT) | `tasmor_lib` | 0.10.0 (2026-06-05) | **MPL-2.0** | tokio | Optional `paho-mqtt` (C). Not worth a license exception. |
| Tasmota | `tasmota-core` | 0.2.3 (2026-10-28) | MIT | — | HTTP status and discovery for a CLI. No IR focus. |
| Tasmota | `tasmota-mqtt-client` | 0.1.0 | — | — | **Archived.** |

**Tasmota in-house:** it is one HTTP GET, `http://<ip>/cm?cmnd=IRsend%20{"Protocol":"NEC","Bits":32,"Data":"0x20DF23DC"}`, or raw `IRsend 0,<µs>,…`. TisVlay has no HTTP-client crate in its lock today (only `http`/`httparse` through tungstenite). Reuse whatever the session's Sunshine `serverinfo` poll uses, or hand-roll HTTP/1.0 on `tokio::net::TcpStream`. MQTT would bring in `rumqttc` (Apache-2.0, pure Rust, tokio), which is overkill for this.

**Fit with the daemon:** all three are `Transport::Network` backends with stateless request-per-command semantics, so `survives_source_ip_change() = true`. They are also one-way: power state can't be read back, except where a Broadlink unit or an ESPHome node also has a receiver. ESPHome is the strategic option: HA's new `infrared` entity platform (contributors blog 2026-03-30) makes ESPHome nodes the standard network IR emitter, with protocol encoding left to the client. That is the same split `irp` gives TisVlay.

## 5. Code databases

| Source | Format | License | Ship in TisVlay? | Rust parser |
|---|---|---|---|---|
| **IRDB** (probonopd/irdb, 1,374★, last push 2026-11-11) | CSV `functionname,protocol,device,subdevice,function`, one file per `<mfr>/<type>/<D>,<S>.csv`. LG TV is `LG/TV/4,-1.csv` (NEC1) | **Custom, non-OSI:** you must open an issue announcing your product, include a notice, and hand over up to 3 free units on request. Permission is revoked on non-compliance, even for network access | **No**, neither bundled nor fetched at runtime | None. Trivial CSV, and `irp` renders each row once the IRP string is known. |
| **Flipper-IRDB** (Lucaslhm, 4,492★, last commit 2026-04-12) | Flipper `.ir`: `Filetype: IR signals file`, `Version: 1`, blocks of `name/type: parsed/protocol/address/command` (4 LE bytes each) or `type: raw` with `frequency/duty_cycle/data`. Parsed protocols: NEC, NECext, NEC42(ext), Samsung32, RC5(X), RC6, SIRC/SIRC15/SIRC20, Kaseikyo, RCA, Pioneer | **CC0-1.0** | Yes, license-wise. Coverage is crowd-sourced: the LG files mostly carry the power **toggle** (0x08), not discrete codes. A good **user-import** format | None on crates.io (`flipper-rpc` and friends talk to the device, not `.ir` files). Easy to write: a line-oriented `key: value` parser, about 100 lines. |
| **HA `infrared-protocols`** (home-assistant-libs, created 2026-02-17, last push 2026-11-14) | Python enums per device (`codes/lg/tv.py`, `samsung/tv.py`, `sony/tv.py`, `philips/tv.py`) plus encoders (`commands/nec.py`, `samsung.py`, `sony.py`, …) | **MIT** | Yes, as a cited source for a hand-curated table. Has **LG discrete power on/off and HDMI 1–4** | n/a (Python) |
| LIRC remotes DB (sourceforge lirc-remotes) | `lircd.conf` | Not clearly stated. LIRC itself is GPL-2.0 | No, unless the license is clarified. A user-supplied file is fine | `cir::lircd_conf` (MIT) parses and encodes `lircd.conf`, but it comes with cir's dependency tree. |
| Kernel rc keymaps (v4l-utils `rc_keymaps/*.toml`, mirrored in cir) | TOML `[[protocols]] name/protocol/variant` + `[protocols.scancodes] 0x… = "KEY_…"` | Generated from kernel C (GPL-2.0) | Not needed. These are **PC-side receiver** keymaps (capture cards, TV boxes), not TV codesets. Relevant only if TisVlay ever receives IR through rc-core | `cir::keymap`. |
| IrpTransmogrifier `IrpProtocols.xml` | XML of IRP definitions | **GPL-3.0** | **No.** Embed the few IRP strings TisVlay uses (short protocol definitions) | `irp::Protocol::parse` |

**Can TisVlay ship a code table?** Yes. Keep it small, hand-curated and cited: discrete power on/off, the HDMI inputs, and volume/mute per brand, as `(irp_protocol, D, S, F)` tuples in a `codes.rs`. Use HA's MIT library and Flipper's CC0 data as sources. Treat it as a fact table, with each entry's provenance in a comment. Code values are facts, but taking them from permissive sources avoids any argument. Let users override or extend it from config with Pronto hex, raw timings or a Flipper `.ir` file. That follows the "device config = facts" split: the codeset is a device fact.

## Recommended stack for a `tisvlay-ir` backend crate

### Depend on

- **`irp`** (MIT, default features; *not* `bpf`) for all encoding (IRP strings for NEC1, NECx1/Samsung32, SIRC12/15/20, RC5, RC6, Kaseikyo) and for Pronto import. Its `Decoder` also gives tests a semantic round-trip check. New transitive crates are `peg`, `bitvec` (+`funty`/`radium`/`tap`/`wyz`) and `num`, all MIT/Apache. Releases are infrequent (last 2025-03), but the repo is maintained and the author is the kernel's rc-core maintainer.
- **`esphome-client`** (MIT, tokio), behind an `esphome` feature, for the network-emitter leg when one is wanted.
- **`libc`** (already a workspace dependency) for the LIRC ioctls and hidraw.

### Write in-house

- **LIRC transmit** (`lirc.rs`, `#![expect(unsafe_code)]`): feature probe, carrier, duty cycle, transmitter mask, and a blocking `write` through `spawn_blocking`. Optionally add scancode-mode transmit so the kernel's encoders can be used. Treat `EINVAL` on long buffers the way cir does: split at long spaces.
- **Flirc Gen2 hidraw transport** (`flirc.rs`): sysfs lookup of `20a0:0006` interface 0; `getversion`, which matters because the gap units changed at fw 4.10; `irtranset`, `irtransraw`, `irtranskil`. Credit flirc-rs's `docs/protocol.md` (MIT). Enforce the device limits: at most 99 timings, a 16–64 ms gap, 1–10 repeats. If hotplug enumeration is wanted, `async-hid` (tokio feature, pure Rust) is the dependency to reach for instead of hand-rolled sysfs.
- **Code table + Flipper `.ir` / Pronto import** (`codes.rs`, `import.rs`).
- **Later:** Broadlink (UDP + `aes`/`cbc`) and Tasmota (one HTTP GET), each about 100–300 lines.

### Avoid

- The flirc SDK, which is proprietary.
- `hidapi` (C), `rusb` (libusb C), and `nusb` (it would have to detach usbhid).
- `cir`: dependency drift, sync, panics.
- `rbroadlink`: sync, stale crypto.
- `tasmor_lib` (MPL + paho C), `esphome-proto` (LGPL-3), `infrared-rs` (GPL-2).
- `flirc-rs` as a dependency (single-author 0.1; use it as a reference).
- IRDB data and `IrpProtocols.xml`.

### Test seam, mirroring `CecLink` / `CecLinkOpener`

```rust
/// One encoded transmission: what every emitter ultimately consumes.
pub(crate) struct IrFrame {
    pub carrier_hz: Option<u32>,   // None = emitter default (Flirc is fixed ~38 kHz)
    pub duty_pct: Option<u8>,
    pub timings_us: Vec<u32>,      // odd length: mark, space, …, mark
    pub repeats: u8,
    pub gap_us: u32,
}

#[async_trait]
pub(crate) trait IrEmitterOpener: Send + Sync + std::fmt::Debug {
    /// Open the configured emitter (`/dev/lircN`, a Flirc serial, an ESPHome key@host).
    async fn open(&self, target: &EmitterTarget) -> Result<Box<dyn IrEmitter>>;
}

#[async_trait]
pub(crate) trait IrEmitter: Send + Sync + std::fmt::Debug {
    /// Device limits the encoder must respect (max timings, settable carrier,
    /// firmware-side repeats).
    fn limits(&self) -> EmitterLimits;
    /// Transmit and return once the burst has left the LED (LIRC's write
    /// blocks until then; Flirc/ESPHome return on the device's ack).
    async fn transmit(&self, frame: &IrFrame) -> Result<()>;
}
```

- Keep the **encoding layer above the seam as pure functions**: `codes` → `irp` → `IrFrame`. The connection depends only on `IrEmitter`.
- **`FakeIrEmitter`** (`#[cfg(any(test, feature = "test-emitter"))]`, `parking_lot` for interior mutability, the same pattern as `FakeCecLink`): it records every `IrFrame`, can fail on scripted calls (`ENODEV` for an unplug, `EBUSY`), and can enforce fake `limits()`.
- **Assert on meaning, not bytes.** Tests decode the recorded frames back with `irp`'s `Decoder` to `(protocol, D, F)`, so `power_on()` is asserted as "sent NEC1 D=4 F=0xC4 with ≥1 repeat" rather than pinning timing goldens. That matches the no-byte-pinning-goldens preference.
- **One layer lower, for the Flirc wire code:** packet building and parsing are pure functions with unit tests. A `FakeFlircHid` on `tokio::io::duplex` plays scripted firmware responses, like LG's `FakeSsapServer`, so the real transport runs without hardware. For LIRC, a fake isn't worth much beyond `FakeIrEmitter`. An optional `loopback-tests`-style hardware test (cir has one) can stay out of CI.

### Open questions

1. **Open loop versus the wake contract.** IR has no power read-back, but `wake_and_connect` is "done when the device has acted on it", and the daemon judges wakes from the connection's power state (#233). Options:
   - declare power `Unknown` and opt out of the confirmation;
   - pair IR with a state source: CEC `query_power` where an adapter exists, an LG SSAP probe, or ESPHome/Broadlink receive;
   - model IR as an extra *actuator* transport on an existing TV rather than a standalone vendor.

   This is the main design decision.
2. **Do the C3 and G4 honor the discrete codes** POWER_ON 0xC4, POWER_OFF 0xC5 and HDMI_n from deep standby, and how many repeats do they need? This is an unattended field script (the TV is the bench's only display).
3. **Flirc range and placement** (anecdotally "terrible"), the gap-unit change at firmware 4.10, and whether driving the transmitter disturbs the receiver/keyboard function. [linux-ir-hardware.md](linux-ir-hardware.md) covers the hardware side.
4. **Device access:** `/dev/lircN` and Flirc's `/dev/hidrawN` need udev rules granting the `tisvlay` system user (the daemon runs as `tisvlay`), not `uaccess`. Packaging gets new udev rules next to the existing TV-remote keymap rule.
5. **`Transport` modeling:** reuse `Usb`, or add an `Ir` variant? `local_device()` returning the lirc/hidraw node fits the existing "wait for the node" machinery.
6. **Encoding location:** kernel scancode-mode transmit versus userspace `irp`. Recommendation: always encode with `irp`, because Flirc and ESPHome need raw timings anyway and one path keeps tests uniform.
7. **Receive side, later:** Flirc maps learned buttons to HID keys through evdev, while LIRC/rc-core devices decode in the kernel. Either could feed `REMOTE_KEYS` and `RemotePower`, using `evdev` with the `tokio` feature, or `irp` decoding on raw capture.

## Sources

- crates.io API: `https://crates.io/api/v1/crates/<name>` for irp, cir, infrared, flirc-rs, hidapi, async-hid, nusb, rusb, evdev, rbroadlink, esphome-client, esphome-native-api, esphome, esphome-proto, tasmor_lib, tasmota-core, tasmota-mqtt-client, tv-rs-ir, oxstack-ir, pronto, lirc-sys, rust-lirc-client-sys, infrared-rs, rumqttc (queried 2026-11-16)
- [cir / irp repo (freedesktop GitLab)](https://gitlab.freedesktop.org/linux-media/tools/cir): `cir/src/lirc.rs`, `cir/Cargo.toml`, `irp/Cargo.toml`, `irp/README.md`, `irp/src/protocols.rs`, `rc_keymaps/`
- [jkristell/infrared](https://github.com/jkristell/infrared) and [docs.rs infrared 0.14.2](https://docs.rs/infrared/0.14.2/infrared/)
- [flirc/sdk](https://github.com/flirc/sdk): `License`, `README.md`, `Changelog.md`, `libs/include/flirc/flirc.h`, `ir/README.md`
- [EriksRemess/flirc-rs](https://github.com/EriksRemess/flirc-rs): `README.md`, `docs/protocol.md`, `src/transport.rs`, `Cargo.toml`
- [arfoll's flirc_util LG TV on/off gist](https://gist.github.com/arfoll/f7c55907fb224a5887e892d2bf58b4a7) and [Flirc blog post 664](https://blog.flirc.tv/index.php/wp-json/wp/v2/posts/664)
- [ruabmbua/hidapi-rs Cargo.toml](https://github.com/ruabmbua/hidapi-rs), [sidit77/async-hid Cargo.toml](https://github.com/sidit77/async-hid), [kevinmehall/nusb](https://github.com/kevinmehall/nusb)
- [nicholascioli/rbroadlink](https://github.com/nicholascioli/rbroadlink), [mjg59/python-broadlink](https://github.com/mjg59/python-broadlink)
- [daanoz/esphome-client](https://github.com/daanoz/esphome-client): `src/proto/api_1_16/api.proto` infrared messages; [UbiHome/esphome-native-api](https://github.com/UbiHome/esphome-native-api)
- [ESPHome Infrared component](https://esphome.io/components/infrared/), [ESPHome IR/RF Proxy](https://esphome.io/components/ir_rf_proxy/), [HA infrared entity platform blog](https://contributors.home-assistant.io/blog/2026/03/30/infrared-entity-platform)
- [home-assistant-libs/infrared-protocols](https://github.com/home-assistant-libs/infrared-protocols): `infrared_protocols/codes/lg/tv.py`
- [probonopd/irdb](https://github.com/probonopd/irdb): `LICENSE.md`, `codes/LG/TV/4,-1.csv`
- [Lucaslhm/Flipper-IRDB](https://github.com/Lucaslhm/Flipper-IRDB): `LICENSE`, `TVs/LG/`
- [bengtmartensson/IrpTransmogrifier](https://github.com/bengtmartensson/IrpTransmogrifier) (GPL-3.0)
- [lirc-remotes on SourceForge](https://sourceforge.net/projects/lirc-remotes/)
- [Kernel LIRC device interface](https://www.kernel.org/doc/html/latest/userspace-api/media/rc/lirc-dev-intro.html)
- [Zakarialabib/tv-rs](https://github.com/Zakarialabib/tv-rs): `crates/drivers/src/ir/emitter.rs`

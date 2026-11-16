# IR emitting from a Linux desktop: hardware, kernel stack, codes, and closing the loop

Research date: 2026-11-16. Each claim carries its source and the date I could verify for it. "Verified locally" means I read the artifact myself: a clone of `flirc/sdk`, a sparse checkout of the kernel `drivers/media/rc` at `aebb727` (Linux 7.4-rc3, 2026-11-15), the v4l-utils `ir-ctl.1.in` man page source, and GitHub API reads of IRDB, Flipper-IRDB and Home Assistant.

---

## 1. Flirc USB (gen 2)

### Can it transmit? Yes, weakly, at 38 kHz only, with an emitter that is already on the board

- **The SDK has a transmit API.** `libs/include/flirc/flirc.h` in `flirc/sdk` (head `280cccb`, 2024-09-22, SDK 3.27.16; verified locally) declares:
  - `int fl_transmit_raw(uint16_t *buf, uint16_t len, uint16_t ik, uint8_t repeat)`: "Sends a raw IR packet for Flirc Gen2 hardware". The buffer holds µs timings, up to **100 timing markers**. `ik` is the inter-packet delay, in µs on firmware < 4.10.0 and in ms on ≥ 4.10.0; the library detects the version and converts. "Repeat count, typically 3. Zero for infinite, currently not supported."
  - `int flirc_send_pronto(uint16_t *buf, uint32_t length, uint8_t rep)`: Pronto hex, "only supported on gen2 hardware".
  - `int fl_ir_transmit_kill(void)` stops a running transmission. `fl_unit_test()` is a "closed loop test" that sends a signal and checks it is received identically, which shows the board carries both an emitter and a receiver.
  - Raw capture: `fl_open_device_alt()` opens the "alternate interface used for ir raw capturing", and `fl_ir_packet_poll(struct ir_packet *)` returns `buf[256]` µs timings plus `elapsed`.
- **`libir`** (`libs/include/ir/ir.h`) is a decoder and encoder for kernel-style `enum rc_proto` values: RC5, RC5X, NEC, NECX, NEC32, NEC48, RC6 variants, Sony, and others. It has `ir_encode()` and `ir_tx(protocol, scancode, repeat)`. Its README describes the helper `fl_transmit_prot(enum rc_proto, scancode, repeat)` and says `ir_encode` should run before every send because RC5 has a toggle bit. The changelog lists encoders added in 3.27.15 and 3.27.16 for NEC48, RC6, Nokia, GAP25 and Denon-K. libir also powers https://ir.flirc.io, Flirc's web decoder.
- **CLI.** `cli/src/cmds/ir_transmit.c` implements `flirc_util sendir --raw="+8840 -4394 …" | --csv=… | --pronto="0000 006D …" [--repeat=N≤10] [--ik=15000..65000 µs] [--kill]`. Verified locally.
- **Firmware.** The changelog embedded in `libflirc.so` (strings, verified locally) has gen2 firmware **4.2.1 "ir transmitter support"**, then "fixes for IR transmitter", "api improvements for IR transmitter", "ir transmission accuracy fixes", "ability to detect ongoing transmission" and "ignore receiver while transmitting". Flirc's blog post "SDK Release, Linux, RPi, IR Transmit Support" (2017-08-06, https://blog.flirc.tv/index.php/wp-json/wp/v2/posts/664) says transmit requires firmware ≥ v4.2, is raw-only with no protocol knowledge, and uses a "very small transmitter" whose power "isn't the best".
- **Range and carrier.** Flirc staff (user "jason") wrote on the forum thread "Transmit IR signal on gen 2" (posts dated 2020-07-30, https://forum.flirc.tv/index.php?/topic/3980-transmit-ir-signal-on-gen-2/): "I'm not pleased with the transmitter", "line of site, 6 feet max", "**Only the 38khz frequency is supported**", and a higher-power transmitter is planned "in the next generation". On 2021-02-13 he recommended "a daemon that keeps the device open and you send it a command". The current product page (https://flirc.tv/products/flirc-usb-receiver, fetched 2026-11-16, $24.95) lists "IR Transmitter: 35ft (line of sight)", which contradicts the staff figure. A user gist "flirc_util LG tv on/off" (arfoll, 2022-06-25, https://gist.github.com/arfoll/f7c55907fb224a5887e892d2bf58b4a7) used flirc_util 3.25.3 with firmware v4.9.3 and reports "Range is terrible as advertised, but it does work" on an LG TV. **No external LED is needed**, but plan for roughly arm's-length range: put it on a USB extension cable near the TV's IR window. I found no evidence of a gen 3 with a stronger emitter; retailers still sell "Gen2" in 2026.
- **Learning codes.** Yes. Raw capture works through the SDK (`fl_ir_packet_poll`) and through the GUI's IR debug log, which exports to IrScrutinizer ("updating IR Logging for direct IR Scrutinizer Import" in the firmware changelog), and libir decodes the captures. The receiver is a demodulating 38 kHz part, so it cannot measure carrier frequency. That doesn't matter for LG NEC, which uses 38 kHz.

### USB identity and how Linux sees it

- **VID 0x20A0** (Clay Logic / pid.codes shared VID; `fl_open_device(0x20A0, "flirc.tv")` in `cli/src/main.c`, verified locally). The nixpkgs `flirc` package's `99-flirc.rules` (package 3.27.19, upstream tarball archived 2026-08-26, license `unfree`; read via GitHub API 2026-11-16) gives:
  - gen2 application **20a0:0006** (match on `SUBSYSTEM=="hidraw"`); gen2 bootloader 20a0:0005 (hidraw).
  - gen1 application 20a0:0001 and 0004, bootloader 0000 and 0002 (matched as `usb_device`, i.e. libusb).
  - `lsusb` shows "20a0:0006 Clay Logic flirc" (LibreELEC forum).
- **It enumerates as a standard USB HID keyboard.** It decodes on the device and emits keystrokes, so **rc-core and LIRC never see it: no `/dev/lircN` and no `rc` sysfs node.** The configuration, transmit and raw-capture traffic goes over HID reports on `/dev/hidrawN`.
- **Permissions.** Flirc's official rules use `MODE="0666"`. For TisVlay, use a `GROUP="tisvlay", MODE="0660"` rule on `SUBSYSTEM=="hidraw", ATTRS{idVendor}=="20a0", ATTRS{idProduct}=="0006"`. In the systemd unit that means `DeviceAllow=char-hidraw rw`, plus `char-usb_device` if libusb turns out to be needed (open question below).

### SDK form, license, and the Rust options

- **Form.** Prebuilt binaries only: `libflirc.{a,so.3.27.16}` and `libir.{a,so}` for Linux x86_64, i686 (static only), arm and aarch64, plus macOS and Windows. They come with C headers. The Linux `.so` is **not stripped and carries debug_info**, and its `NEEDED` entries are `libusb-1.0.so.0` and `libhidapi-hidraw.so.0` (verified locally). The flirc_util CLI source is included.
- **License** (`License`, added 3.27.16, 2024-09-22): "All rights reserved". Use is permitted "solely in conjunction with Flirc manufactured hardware in commercial environments", and the Software "may not be used, copied, modified, merged, published, distributed, sublicensed, or sold except as permitted". It is proprietary, nixpkgs marks it unfree, and it is incompatible with shipping inside TisVlay's packages. GitHub reports it as NOASSERTION.
- **Rust paths:**
  1. **FFI to `libflirc.so`, loaded with `dlopen` behind a feature, with the user supplying the library.** Small to write: about four functions (`fl_open_device`, `fl_transmit_raw`/`flirc_send_pronto`, `fl_close_device`, `fl_ir_packet_poll`). But it is a global-state C library: `fl_open_device` takes no handle, so there is one device per process. It is unfree and C-only, so TisVlay could not ship it.
  2. **Reimplement the HID reports over `/dev/hidraw`** using the `hidapi` crate or raw `read`/`write`. No open documentation or reimplementation exists; I searched and found none. The report format would have to come from a usbmon capture of `flirc_util sendir`, which is clean-room observation of the device. Whether that is fine under the license's "may not be used … except" terms is a judgment for the maintainer. This is the only path that fits TisVlay's packaging.
  3. **Shell out to `flirc_util`.** Fastest for a spike, unshippable for the same license reason.
- **Software freshness.** The SDK repo was last pushed 2024-09-23. The Flirc desktop software is at 3.27.19 (nixpkgs, 2026-08). A German Raspberry Pi forum post (2023-12-04) reports that Flirc tools failed on kernel 6 while the device itself worked.

---

## 2. The Linux kernel IR stack (rc-core and LIRC)

Sources: kernel docs `lirc-dev-intro`, `lirc-write` and `lirc-get-features` (https://docs.kernel.org/userspace-api/media/rc/, fetched 2026-11-16), and the source at `aebb727` (7.4-rc3, 2026-11-15).

- **rc-core** registers every IR device as `/sys/class/rc/rcN` and, when the driver supports raw IR, as a LIRC chardev **`/dev/lircN`**. In `lirc_dev.c`, the chrdev region and class are both named `"lirc"` and the nodes are `lirc%d`. That gives `DeviceAllow=char-lirc rw` and a udev match on `SUBSYSTEM=="lirc"` / `KERNEL=="lirc[0-9]*"`.
- **TX modes:**
  - `LIRC_MODE_PULSE` is send-only. You write alternating pulse and space values in µs, starting and ending with a pulse, so the count is odd. **`write()` blocks until the hardware has transmitted.** Too much data returns `EINVAL`.
  - `LIRC_MODE_SCANCODE` works for send and receive. You write exactly one `struct lirc_scancode {timestamp, flags, rc_proto, keycode, scancode}` with only `rc_proto` and `scancode` set, and **the kernel encodes it**. It returns `EINVAL` if there is no encoder for the protocol.
  - `LIRC_MODE_MODE2` is receive-only raw.
- **In-kernel encoders**, i.e. decoders with an `.encode` op (verified locally): NEC (nec, necx, nec32), RC5, RC6, Sony (12/15/20), JVC, Sanyo, Sharp, MCE keyboard, RC-MM and iMON. **There is no Samsung32 encoder.** The NEC encoder always emits the 9 ms header (`ir-nec-decoder.c:210`), and Samsung32 uses a 4.5 ms header, so Samsung codes must be sent as raw PULSE timings. Default carriers come from the protocol definitions: NEC 38 kHz, Sony 40 kHz, RC5 36 kHz.
- **Ioctls.** The `LIRC_GET_FEATURES` flags are `LIRC_CAN_SEND_PULSE` (which implies scancode TX), `LIRC_CAN_SET_SEND_CARRIER` (`LIRC_SET_SEND_CARRIER`), `LIRC_CAN_SET_SEND_DUTY_CYCLE` and `LIRC_CAN_SET_TRANSMITTER_MASK` (choosing which emitter jack).
- **BPF decoders.** `BPF_PROG_TYPE_LIRC_MODE2` programs attach to a LIRC device with raw receive, up to 64 per device, and report through `bpf_rc_keydown()` and related calls. They cover receive only. `ir-keytable` loads keymaps and BPF protocols.
- **v4l-utils `ir-ctl`** (man source, last changed 2026-03-17): `-S proto:scancode` sends a scancode, using the protocols rc5, rc5x_20, rc5_sz, jvc, sony12/15/20, nec, necx, nec32, sanyo, rc6_*, sharp, imon and rc_mm_*. `-s file` sends raw pulse/space or mode2 text, which may include `carrier N` and `scancode proto:val` lines. `-K`/`-k` sends a keycode from a TOML keymap. Other options are `-c` carrier, `-D` duty cycle, `-e` emitters, `-g` gap (default 125 ms), and `-r -w -m` to receive in wideband "learning mode" with carrier measurement. For example, `ir-ctl -d /dev/lirc0 -S nec:0x04c4` should send LG POWER ON (scancode derived below).
- **Drivers with TX** (`tx_ir` set in the 7.4-rc3 tree, verified locally):

  | Driver | Hardware | Carrier / duty / mask ops |
  |---|---|---|
  | `mceusb` | Windows MCE eHome transceivers (Philips/SMK/Topseed/Formosa/Fintek…), Hauppauge, Twisted Melon Manta | carrier + tx mask (2 emitter jacks) |
  | `iguanair` | IguanaWorks USB IR Transceiver | carrier + tx mask |
  | `ir_toy` | Dangerous Prototypes IR Toy (04d8:fd08), Irdroid USB (04d8:f58b) | carrier |
  | `redrat3` | RedRat3 | carrier |
  | `gpio-ir-tx`, `pwm-ir-tx`, `ir-spi`, `meson-ir-tx` | SBC GPIO/PWM/SPI LEDs (DT overlays) | carrier + duty |
  | `ite-cir`, `ene_ir`, `winbond-cir`, `serial_ir` | laptop/HTPC built-in CIR, homebrew serial | various |
  | `rc-loopback` | software test device | all, which makes it a free CI fake |

  There is **no in-kernel driver for USB-UIRT or Flirc**. `ttusbir`, `streamzap`, `fintek-cir` and `nuvoton-cir` are receive-only here.
- **Maintainer.** Sean Young maintains rc-core/LIRC, gpio-ir-tx, pwm-ir-tx and iguanair (MAINTAINERS, master 2026-11). He also publishes the Rust crates **`cir` 0.1.4** (MIT, 2025-03-20; modules `lirc` for chardev access, `rcdev` for sysfs enumeration of IR *and CEC* devices, `keymap`, `lircd_conf`, `rc_maps`) and **`irp` 0.3.4** (MIT, 2025-03-20; an IRP-notation encoder and decoder over IrpTransmogrifier's `IrpProtocols.xml`, Pronto hex parse and generate, optional BPF decoder generation), both from gitlab.freedesktop.org/linux-media/tools/cir. Together they cover the Rust side: `irp` turns any protocol (Samsung32 included) into timings or Pronto, and `cir::lirc` sends them.
- **Unprivileged daemon access** mirrors `/dev/cecN` exactly. The comment block in `packaging/systemd/tisvlay-daemon.service` (lines 45–55) describes the pattern: drop `PrivateDevices=true`, add `DeviceAllow=char-lirc rw`, and add the udev rule `SUBSYSTEM=="lirc", GROUP="tisvlay", MODE="0660"`. It carries the same caveat: DeviceAllow resolves the major from `/proc/devices` at unit start, so `rc_core` must be loaded first. The CEC stack usually loads rc_core already through CEC RC passthrough, but that should be checked. For Flirc it would be `DeviceAllow=char-hidraw rw` with a hidraw udev rule. Hotplug is the same as CEC: `PrivateDevices` hides late nodes, and `DeviceAllow` plus the cgroup device policy admits them.

---

## 3. Hardware options for TX on Linux

Prices were fetched 2026-11-16 unless noted. "In-kernel" means it appears as `/dev/lircN` with no vendor SDK.

| Option | Kernel path | Price / availability (date) | Notes |
|---|---|---|---|
| **Flirc USB gen2** (on order) | none (hidraw + proprietary SDK) | $24.95, in production (flirc.tv, 2026-11-16) | 38 kHz only, ~2 m range by staff account; also a learner. |
| **MCE eHome transceiver** (mceusb) | `mceusb`, TX on units with emitter jacks | Secondhand, plus no-name clones on marketplaces; **current price not verified** (historically ~$20, 2007 NextPVR post) | Needs separate 3.5 mm stick-on emitter cables; a Dec 2025 Arch forum post confirms a 2-jack unit had no built-in LED. Stick-on emitters solve the line-of-sight problem for a PC behind the TV. Two emitters can be addressed through `LIRC_SET_TRANSMITTER_MASK`. Clone quality varies, so check `ir-ctl -f` for `send` before trusting a listing. |
| **IguanaWorks USB IR Transceiver** | `iguanair` | **Discontinued**: "Iguanaworks has closed and our products are no longer sold", shutdown 2025-12-31 (iguanaworks.net) | Only secondhand. |
| **USB-UIRT** | none in-kernel (LIRC userspace driver only) | **$55, "IN STOCK", build-to-order** (usbuirt.com/order.htm, fetched 2026-11-16); a 2026 notice says Windows drivers won't be re-signed | Learns any carrier. A poor fit for TisVlay without kernel support. |
| **Irdroid USB Infrared Transmitter v3 / transceiver** | `ir_toy` (04d8:f58b) | $29 on Tindie, but **seller "on a break"** (fetched 2026-11-16) | The IR Toy (04d8:fd08) by Dangerous Prototypes is legacy. |
| **RedRat** | `redrat3` | Commercial; price not checked | Pro test gear, overkill. |
| **GPIO / PWM LED on an SBC** | `gpio-ir-tx`, `pwm-ir-tx` (DT overlay) | One IR LED and a transistor, about $1; the Pi itself extra | Not on the desktop. It would be a separate networked node, which is the ESP32 option below with more effort. |
| **ESP32 / ESP8266 + ESPHome** | network | Seeed XIAO IR Mate about $10.90 direct; M5Stack AtomS3 Lite about $15; Athom IR/RF $20–26 (computingforgeeks.com, updated 2026-11-11) | ESPHome `remote_transmitter` (RMT, any carrier, duty 30–50%). The new **IR/RF Proxy** (`infrared:` / `ir_rf_proxy`, marked **EXPERIMENTAL**, esphome.io fetched 2026-11-16, ESPHome 2026.10.2 current) accepts raw mark/space timings, carrier and repeat over the native API. Rust: `esphome-native-api` 3.1.0 (2026-11-06) or `esphome-client` 0.2.1. |
| **Tasmota IR** | network | Same ESP hardware | HTTP/MQTT `IRSend` with protocol plus hex or raw; Tasmota publishes a code list (tasmota.github.io/docs/Codes-for-IR-Remotes). |
| **Broadlink RM4 mini / RM4 Pro** | network (UDP, AES-128 local protocol) | About $26 / $45 (computingforgeeks, 2026-11-11) | Local control through python-broadlink (MIT, last release 0.19.0, 2024-04-17) or the Rust crate `rbroadlink` 0.4.2 (2024-06). **Firmware risk**: if the device reaches Broadlink's cloud, local control can stop working, and RM4 Pro firmware 62093 reportedly breaks local integrations (Homey app page, forum reports, undated). The device must be "unlocked" in the app. |
| **Sound-card DAC hack** | ALSA | Two IR LEDs and a resistor | LIRC's `audio` driver documents TX: two 950 nm LEDs anti-parallel across L/R with the right channel inverted, driven by a sine at **half the carrier** (19 kHz for 38 kHz) so rectification doubles it. "48 kHz is about the bare minimum", 96 kHz recommended, about 3 m range without an amplifier (lirc.org/html/audio.html, undated; it recommends a 2007 PortAudio). Android "IR Blaster" apps still use the same audio-jack trick. **Reachable carriers:** with the half-frequency trick the sine must stay below Nyquist and in practice below the codec's reconstruction filter (~0.45·fs). At 48 kHz that is ~21.6 kHz sine, so ≤ ~43 kHz carrier (covers 36/38/40 kHz with little margin). At 96 kHz, ≤ ~86 kHz (adds 56 kHz). At 192 kHz, ≤ ~170 kHz, or 38 kHz direct without the trick. **No maintained desktop project** besides the legacy LIRC plugin. It is a bad fit anyway: a system daemon has no access to the user's PipeWire, needs an exclusive ALSA `hw:` device, and fights desktop audio. |

---

## 4. Protocols and codes for the target TVs

### LG (C3 / G4): NEC, address 0x04, with discrete codes available

All LG TV codes are standard NEC (38 kHz) with **address 0x04** (32-bit LSB-first form `0x20DF….`). Kernel `RC_PROTO_NEC` scancode = `(0x04 << 8) | cmd`.

| Function | NEC cmd | Kernel scancode | 32-bit form | Discrete? |
|---|---|---|---|---|
| Power (toggle) | 0x08 | `nec:0x0408` | 20DF10EF | toggle |
| **Power ON** | **0xC4** | `nec:0x04c4` | 20DF23DC | **discrete** |
| **Power OFF** | **0xC5** | `nec:0x04c5` | 20DFA35C | **discrete** |
| **HDMI 1** | 0xCE | `nec:0x04ce` | 20DF738C | discrete |
| **HDMI 2** | 0xCC | `nec:0x04cc` | 20DF33CC | discrete |
| **HDMI 3** | 0xE9 | `nec:0x04e9` | 20DF9768 | discrete |
| **HDMI 4** | 0xDA | `nec:0x04da` | 20DF5BA4 | discrete |
| Mute | 0x09 | `nec:0x0409` | 20DF906F | **toggle only** (no discrete mute found) |
| Vol + / − | 0x02 / 0x03 | `nec:0x0402/03` | 20DF40BF / 20DFC03F | relative steps only |

Sources, which agree with each other:
- **IRDB** `codes/LG/TV/4,-1.csv` (probonopd/irdb, repo pushed 2026-11-11): POWER ON NEC1 4 196, POWER OFF 197, HDMI 1 206, HDMI 2 204, HDMI 3 233, HDMI 4 218, MUTE 9. An older row "INPUT - HDMI 1 … 198" is a different, older input code.
- **ledoge gist "All LG TV IR codes (as of webOS 25)"** (2026-02-11, https://gist.github.com/ledoge/ab78723efdaa00e6815e30d1792790fd) lists `DSC_IR_KEY_PWRON` 20DF23DC, `PWROFF` 20DFA35C and `HDMI1..4` as above. **It warns that some factory/service codes (e.g. `IR_KEY_IN_STOP`, `IR_KEY_POWERONLY`, `DSC_IR_KEY_SUBSTRATE`) can damage the TV.** The author calls PWRON/PWROFF and HDMI1–4 safe. Allowlist the codes; never send arbitrary ones.
- **Flipper-IRDB** `TVs/LG/LG_OLED48C37LA (LG_OLED C3 models).ir` (committed 2024-10-30; CC0): Power 0x08, Mute 0x09, Vol 0x02/0x03, and address 0x04 for a real C3 remote.
- **Home Assistant `infrared-protocols`** (`codes/lg/tv.py`, MIT, main pushed 2026-11-14; HA pins 10.7.0): `LG_ADDRESS = 0xFB04`, `POWER_ON = 0xC4`, `POWER_OFF = 0xC5`, `HDMI_1..4 = 0xCE/0xCC/0xE9/0xDA`. Its `media_player.async_turn_on/off` send `POWER_ON`/`POWER_OFF`. The HA docs page for LG Infrared says power on and off send "the same IR power toggle", which contradicts the code; the code is authoritative.
- An LG service manual for 2009–2010 sets lists different discrete values for older models, so confirm on the C3 and G4 hardware.

### Samsung (Samsung32 protocol: NEC-like, 4.5 ms header, address 0x07, 38 kHz)
- Flipper-IRDB `TVs/Samsung/*.ir` (CC0): **Power OFF discrete 0x98** in several files (`Samsung_TV_Full.ir` "Power_off_only"), toggle 0x02, HDMI1 0xE9, HDMI2 0xBE, HDMI3 0xC2, HDMI4 0xC5. IRDB `Samsung/TV/7,7.csv` agrees on HDMI (NECx2 7.7: 233/190/194/197).
- **Discrete Power ON is not confirmed.** Neither IRDB nor Flipper-IRDB has a verified one; files labeled "Power on" carry the 0x02 toggle. A Remote Central file states "Power On is not a discrete (couldn't find one)". The usual workaround is to send "off" and then toggle.
- The kernel can't encode Samsung32 (see §2), so send raw timings, which the `irp` crate can generate.

### Sony (SIRC, 40 kHz)
- IRDB `Sony/TV/1,-1.csv`: Sony12 device 1 **POWER ON function 46, POWER OFF 47**, toggle 21, mute 20 (toggle). That gives kernel `sony12:0x01002e` and `0x01002f` (the scancode is `device<<16 | subdevice<<8 | function`, from `ir-sony-decoder.c:148`).
- IRDB `Sony/TV/26,-1.csv`: **HDMI 1–5 = Sony15 device 26 functions 90–94**, with Sony12 variants for 2011-era sets.
- Sony community threads (undated) report that some Bravia models expose no discrete inputs. Sony's Pro Bravia docs steer integrators to IP or serial control.

### Where codes come from
- **IRDB** (probonopd/irdb): CSV rows of `protocol,device,subdevice,function`, active (pushed 2026-11-11). It has no SPDX license (GitHub reports NOASSERTION), so treat the data as uncleared before vendoring.
- **Flipper-IRDB** (Lucaslhm): **CC0**, 13.5k files, last commit 2026-04-12. Parsed NEC/Samsung32/SIRC entries or raw timings.
- **Kernel keymaps** `drivers/media/rc/keymaps/`: these map scancodes to keycodes for *receive* only. There is **no LG or Samsung TV keymap**, so they are not a TX code source.
- **lircd.conf remote database** (lirc-remotes on SourceForge): the `cir::lircd_conf` module parses these and generates IRP.
- **IrScrutinizer / IrpTransmogrifier** (bengtmartensson, GPL-3.0, pushed 2026-10-30 and 2026-08-15): the reference toolchain for converting between Pronto, raw and IRP, and the origin of `IrpProtocols.xml`, which the `irp` crate consumes.
- **Remote Central** Pronto files, Tasmota's code page, and `infrared-protocols` (MIT, typed Python enums per brand: a clean, permissively licensed reference for LG, Samsung and others).

---

## 5. Closing the loop on an open-loop transport

What existing projects do:
- **Home Assistant** added an `infrared` entity platform in **2026.4**, with **LG Infrared** as the first brand integration. The commits are 2026-03-24 and 25 (`Add LG Infrared integration (#162359)`), and the manifest has `"iot_class": "assumed_state"`. Entities are `_attr_assumed_state = True` and show the last command sent. **Volume is step-only**, and **an optional IR receiver feeds an event entity**, so presses on the physical remote can update the assumed state. Emitters come from ESPHome, Broadlink, ZHA (Tuya TS1201) or SMLIGHT, per computingforgeeks (updated 2026-11-11) and the HA docs page fetched 2026-11-16.
- **Logitech Harmony** keeps per-device assumed state across activities. When it drifts ("out of sync": the remote thinks a device is on when it is off), the **Help** button re-sends the activity's commands. Logitech's support advice is to **switch the device to discrete PowerOn/PowerOff and discrete inputs** and to raise the inter-device delay (Harmony 1000/1100 manuals; support.logi.com "Adjusting how a Harmony remote turns a device on or off"). In short: discrete codes plus idempotent re-send.
- **LIRC users** script `irsend` and pair it with a sensor (current-sensing plugs, or a light sensor on the standby LED) or with CEC.

Techniques available to TisVlay:
1. **Make every command idempotent with discrete codes.** For LG, power on/off and HDMI1–4 are fully idempotent. Re-sending them (Harmony's Help) is safe, so drift in those values can't accumulate.
2. **Use the DRM connector TisVlay already reads.** `dpms` and `status` show whether the PC's output is on, not whether the TV is. Whether `status` or EDID changes when the TV enters standby varies by TV. The kernel CEC admin guide notes "some displays in standby mode have no HDMI Hotplug Detect signal". A 2017 amd-gfx thread reported LG displays reading `disconnected` when powered off while NEC and Samsung did not. **Measure this on the C3 and G4** by watching `/sys/class/drm/*/status` and udev `change` events across a standby cycle before relying on it.
3. **CEC hybrid.** CEC `Give Device Power Status` is real read-back, and TisVlay's CEC backend already does it, but it needs the same CEC adapter that, if present, makes IR unnecessary for power. IR still adds value next to CEC where CEC is unreliable or where a device lacks it.
4. **Network presence** (ping, SSDP). This is weak for LG: with "TV On with Wi-Fi" / Quick Start+ the network stays up in standby, and if the TV is on the network the LG SSAP backend is strictly better than IR.
5. **Listen to the user's remote.** A receiver (Flirc gen2 raw capture, an MCE transceiver, an ESPHome receiver) can watch for LG NEC address-0x04 frames from the user's own remote and update assumed state, as HA's event entity does. This is the only practical fix for mute and volume drift. Flirc's firmware "ignore receiver while transmitting" means it won't hear itself.
6. **Audio/ARC state.** No practical read-back without CEC. Volume steps can be "homed" by sending N× Vol− and then counting up. That is slow: NEC frames take about 108 ms, so a full sweep takes seconds, and it is only acceptable as an explicit "set" emulation.

---

## What this means for a TisVlay IR backend

How it maps onto the backend model (`docs/backends/implementing-a-vendor.md`, `crates/tisvlay-core/src/traits.rs`):
- **Capabilities that are honest open-loop**, given the contract "Declare a bit only when the matching `TvConnection` method really works": `POWER` (discrete on/off), `INPUT` (discrete HDMI1–4) and probably `SCREEN` as power-emulated. `MUTE` and `VOLUME` are not honest: the trait's `set_mute(bool)` and `set_volume(level)` are absolute, and LG IR offers only a mute toggle and volume steps. They would need assumed state (the "Emulating a capability" pattern) or a new relative-step capability.
- **No push and no `get_state`.** As `docs/concerns/poll-only-screen-verify.md` and § "What the verify loop reads" describe, screen verification on a poll-only, not-standby-reachable backend always fails: `screen_on` escalates and `screen_off` ends in `event/dispatch-failed`. An IR backend needs either a **state source** (CEC or a DRM probe, i.e. a composite or companion backend) or an explicit "assumed" `PowerState` the verify loop treats as satisfied once sent. It is closest to CEC in being `reachable_in_standby() == true` (the emitter is always "connected"), with `power_on` as wake-in-place.
- **Transport.** The existing `Transport::Usb` fits Flirc. `/dev/lircN` is a local node like `/dev/cecN` and would use `local_device()`. An ESPHome or Broadlink blaster is `Network`.

Feasible designs, cheapest first:
1. **Kernel LIRC backend** (`cir` + `irp` crates, MIT): one code path for every in-kernel transceiver (mceusb with stick-on emitters, ir_toy/Irdroid, GPIO), the `rc-loopback` driver as a free CI fake (`modprobe rc-loopback` gives a `/dev/lircN` that loops TX into RX), packaging that mirrors CEC exactly (`DeviceAllow=char-lirc rw` plus a udev group rule), and Samsung/Sony through `irp` raw encoding. The weakness is purchasable hardware: mceusb clones are secondhand or no-name, IguanaWorks closed 2025-12-31, and Irdroid's store is paused.
2. **Flirc backend over hidraw**: works with the hardware already on order and needs no kernel driver. It means reverse-engineering the HID reports with usbmon against `flirc_util`. It is limited to 38 kHz (fine for LG, Samsung and NEC; Sony's 40 kHz probably works at reduced range) and has short range. FFI to the proprietary `libflirc` is fine for a spike but can't ship.
3. **ESPHome IR proxy (network)**: about $11–26 of hardware, placeable at the TV, any carrier, a Rust client crate exists. It is experimental upstream (API may change) and adds a networked device per room.
4. **Broadlink**: cheap and common, but the firmware and cloud lock risk makes it a poor default.

Skip the sound-card hack, USB-UIRT (no kernel driver) and RedRat.

Open questions for the maintainer:
1. **Does the Flirc's transmitter reach the C3 or G4 from the desk?** This is the first bench test: `flirc_util sendir --csv=0,<NEC 0x04C4 timings>` at increasing distances. If it fails beyond about 2 m, Flirc is a code-learning tool rather than the emitter.
2. **Is reverse-engineering Flirc's HID reports acceptable** given its "all rights reserved, Flirc-hardware-only" license, or does the Flirc path stay a dev-only FFI spike?
3. **Does libflirc need libusb on gen2**, or only hidraw? `strace` a `sendir` to find out. The answer decides whether the unit needs `char-usb_device` as well as `char-hidraw`.
4. **Does the C3 or G4 drop HPD or change EDID in standby** on this GPU? This decides whether DRM status can act as the IR backend's power read-back.
5. **Should IR be a standalone vendor backend, or an actuator under a composite** (IR for power and inputs, CEC or DRM for state)? The current trait set has no notion of a "state source" separate from the command path.
6. **How should open-loop state appear on the wire:** a new "assumed" marker on `State<T>` (wire-visible, so a `feat`) versus claiming NotSupported for mute and volume? The latter is simplest and honest, but loses the most-wanted soundbar use case.
7. **Code data licensing:** Flipper-IRDB is CC0 and `infrared-protocols` is MIT, so either can be vendored. IRDB has no clear license. The code set must be an allowlist, never arbitrary codes, because of the LG factory-code damage warning.
8. **Target devices:** the LG TVs already have SSAP and CEC paths. Is the IR user's real target a non-networked TV, soundbar or AVR? That changes which protocols (Samsung32 raw, Sony SIRC 40 kHz, RC5/RC6 36 kHz) matter.

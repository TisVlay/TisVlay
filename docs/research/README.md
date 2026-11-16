# Research

Dated surveys kept so a later design can start from them instead of searching again. Each one records what was true on its date, with its sources. They aren't maintained: a fact here may have gone stale, so check it again before you build on it. The idea or plan that uses a survey links to it, and says what's still undecided.

## IR remote control

Surveyed 2026-11-16 for [IR remote control](../ideas/ir-remote-control.md).

- [IR emitting from a Linux desktop](ir/linux-ir-hardware.md): the Flirc gen 2's transmitter and its SDK, the kernel's rc-core and LIRC stack, the hardware that can send, the LG, Samsung and Sony codes, and how other projects live with no read-back.
- [The Rust IR ecosystem](ir/rust-ir-ecosystem.md): crates for encoding, LIRC, Flirc, network emitters and code tables, a recommended stack for a backend crate, and a test seam that mirrors CEC's.

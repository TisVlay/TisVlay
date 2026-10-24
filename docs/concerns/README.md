# Design Concerns & Technical Debt

Items that aren't bugs but represent fragility, suboptimal patterns, or deferred decisions worth revisiting. Each entry is a self-contained per-concern doc with priority, context, risk, mitigation options, and re-evaluation triggers — pick one when its trigger condition fires (a related bug surfaces, an upstream change lands, a sprint has slack).

## Architecture

- [Zenoh idiom audit against zenoh 1.9.0 (2026-07-12 triage list)](zenoh-idiom-audit.md)

## Testing

- [Session event tests use real-time sleeps](session-event-tests-real-time-sleeps.md)
- [Remote-session detectors not yet field-validated (krdp, RustDesk, PipeWire, logind)](remote-session-detector-validation.md)
- [An LG C3 may ignore CEC `<Standby>` just after waking (unconfirmed)](cec-standby-ignored-after-wake.md)

## Build & CI

- [CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround](test-ci-cargo-llvm-cov-show-env.md)
- [Stray `.profraw` files outside `target/` from instrumented binaries](profraw-stray-files-outside-target.md)

## CLI

- [CLI `pair` gets no vendor schema defaults](cli-pair-vendor-fields.md)

## GUI

- [Slint `unstable-winit-030` feature dependency](slint-unstable-winit-030.md)
- [Slint `ComboBox` dropdown clipped by the window](slint-combobox-popup-clipped.md)
- [Slint live-preview interpreter panics on a navigation callback (dev-only; #6426)](slint-live-preview-nav-crash.md)
- [Slint compiler/runtime gotchas hit building the GUI (all worked around)](slint-gotchas.md)

## Packaging & deployment

- [Packaged greeter socket access — solved via a daemon group ACL (Fedora + Ubuntu validated; NixOS wiring pending)](greeter-packaged-socket-access.md)

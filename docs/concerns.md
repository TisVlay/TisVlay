# Design Concerns & Technical Debt

Items that aren't bugs but represent fragility, suboptimal patterns, or deferred decisions worth revisiting. Each entry is a self-contained per-concern doc with priority, context, risk, mitigation options, and re-evaluation triggers — pick one when its trigger condition fires (a related bug surfaces, an upstream change lands, a sprint has slack).

## Testing

- [Session event tests use real-time sleeps](concerns/session-event-tests-real-time-sleeps.md)
- [`TestDaemon` / `TestSession` name ownership is bus-global](concerns/test-name-ownership-bus-global.md)

## Build & CI

- [CI test-ci pipeline depends on `cargo llvm-cov show-env` workaround](concerns/test-ci-cargo-llvm-cov-show-env.md)
- [Stray `.profraw` files outside `target/` from instrumented binaries](concerns/profraw-stray-files-outside-target.md)

## CLI

- [CLI `pair` can't carry vendor-specific fields](concerns/cli-pair-vendor-fields.md)

## GUI

- [Slint `unstable-winit-030` feature dependency](concerns/slint-unstable-winit-030.md)
- [Slint `ComboBox` dropdown clipped by the window](concerns/slint-combobox-popup-clipped.md)
- [Slint live-preview interpreter panics on a navigation callback (dev-only; #6426)](concerns/slint-live-preview-nav-crash.md)

## Packaging & deployment

- [Packaged greeter socket access — solved via a daemon group ACL (Fedora + Ubuntu validated; NixOS wiring pending)](concerns/greeter-packaged-socket-access.md)

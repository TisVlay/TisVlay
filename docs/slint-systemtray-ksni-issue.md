# Upstream issue draft — Slint `SystemTrayIcon` forces a hardcoded ksni `async-io` backend on all downstream ksni users

Draft to file at <https://github.com/slint-ui/slint/issues>. Captures the
coupling that forced `tisvlay-tray` onto ksni's `async-io` backend when we
briefly migrated to Slint 1.17-pre (since reverted to 1.16.1 — see
[`docs/concerns/slint-unstable-winit-030.md`](concerns/slint-unstable-winit-030.md)).

---

## Title

`i-slint-core` unconditionally pulls `ksni` with a hardcoded `async-io` backend under `std`, breaking apps that use ksni with the default tokio backend

## Summary

Slint 1.17's new `SystemTrayIcon` element makes `i-slint-core` depend on
[`ksni`](https://crates.io/crates/ksni). The dependency is:

1. **Bundled into the `std` feature** — not opt-in, so any real application
   pulls it whether or not it instantiates `SystemTrayIcon`.
2. **Hardcoded to ksni's `async-io` backend.**

Because Cargo unifies a dependency's features across the whole graph, this
forces `async-io` onto *every* ksni consumer in the workspace. An application
that already uses ksni directly with its **default `tokio` backend** then fails
to compile: ksni 0.3.5 added a compile-time guard that errors when both `tokio`
and `async-io` are enabled.

## Reproduction

A Cargo workspace with:

- a system-tray binary depending on `ksni = "0.3"` (default features → `tokio`,
  the common choice for a tokio app), and
- a GUI binary depending on `slint` 1.17 with `std` (i.e. any real GUI),

fails to build:

```
error: Features "tokio" and "async-io" cannot be enabled at the same time.
  --> ksni-0.3.5/src/compat.rs
```

`cargo tree -i ksni` shows the two consumers pulling incompatible feature sets:
`i-slint-core` (async-io, via `std`) and the app's tray crate (tokio, default).

## Source

`internal/core/Cargo.toml`:

```toml
[features]
std = [
    # ...
    "dep:ksni",          # ← ksni pulled in by `std`, not an opt-in feature
    "dep:async-channel",
]

[target.'cfg(all(target_family = "unix", not(target_vendor = "apple"), not(target_os = "android")))'.dependencies]
ksni = { version = "0.3.3", optional = true, default-features = false, features = ["async-io", "blocking"] }
#                                                                                  ^^^^^^^^^^ hardcoded backend
```

## Impact

- Any downstream that uses ksni with the default `tokio` backend can't build
  alongside Slint 1.17 on Linux — even if it never uses `SystemTrayIcon`.
- The consumer has no escape hatch: `std` is non-negotiable, and the backend is
  fixed, so they're forced to switch their own ksni usage (and transitively
  their zbus runtime expectations) to `async-io` to match Slint.

## Requested change

**Primary — make the tray/ksni dependency opt-in.** Move `dep:ksni` (and the
`SystemTrayIcon` support) out of `std` into a dedicated feature, e.g.
`system-tray`, off by default. Apps that don't use Slint's tray then don't pull
ksni at all and keep full control over their own ksni configuration. This fully
decouples the two.

**Secondary — let consumers choose the ksni backend.** For apps that *do* use
`SystemTrayIcon`, expose the async-runtime choice (e.g. `system-tray-tokio` /
`system-tray-async-io`, forwarding to ksni's `tokio` / `async-io`) instead of
hardcoding `async-io`, so Slint's tray can run on the same runtime as the rest
of the app (tokio is the common case, and matches zbus's typical setup).

Either change on its own unblocks us; together they cover both "I don't want
Slint's tray" and "I want Slint's tray, on my runtime."

## Environment

- Slint: 1.17.0 (master, e.g. rev `bd7fbece0`)
- ksni: 0.3.5 (the release that added the tokio+async-io compile guard)
- Target: Linux (the ksni dep is `cfg(unix, !apple, !android)`-gated)

## Workaround

Until this is addressed we stay on Slint 1.16.1 (no `SystemTrayIcon`, no ksni
in `i-slint-core`), which lets `tisvlay-tray` keep ksni's default tokio backend.

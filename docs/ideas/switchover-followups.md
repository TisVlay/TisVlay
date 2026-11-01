# Switchover follow-ups (v0.3.0 zenoh stack)

Small items deferred at the v0.3.0 switchover, grouped here because each is too small for its own spec. Trigger: any of them blocking a release polish pass, or slack time.

## Tray gettext re-wiring

The v0.2 tray owned a gettext domain (`tisvlay-tray.pot`, build.rs-compiled `.mo`); the zenoh-era tray shipped without it, so its menu items and notification copy are English-only string literals. Re-wire `gettext()` around user-facing strings, restore the build.rs catalog compile, and re-add the tray extraction pass to `just i18n-extract` (the recipe went GUI-only at the switchover). [docs/i18n.md](../i18n.md) covers the GUI side; the tray would mirror it:

- **Catalogs** under `crates/tisvlay-tray/lang/`: a committed `tisvlay-tray.pot` and per-locale `<locale>/LC_MESSAGES/tisvlay-tray.{po,mo}`.
- **A `build.rs` running `msgfmt`**, since the tray has no Slint to compile `.mo` files as a side effect. It soft-fails with `cargo:warning=` (the runtime falls back to source strings), the pattern `tisvlay-gui-app/build.rs` uses for `cargo-about`. Checking in `.mo` files was the alternative: no build step, but two diffs per `.po` edit and binary blobs in git.
- **`bindtextdomain` + `textdomain` directly**, against `CARGO_MANIFEST_DIR/lang`, rather than `gettextrs::TextDomain`, whose lookup walks XDG paths only and misses the source tree in dev runs. Installed builds then need the catalogs installed under `/usr/share/locale` and the binary taught to look there.

## Mock backend ships unconditionally

The v0.2 daemon gated the mock vendor behind a `mock` cargo feature that packaging enabled deliberately ("Mock (testing)" for field debugging). The zenoh daemon registers `tisvlay-mock` unconditionally — same end state as the packages shipped anyway, but it is no longer a choice. Decide whether that stays deliberate (document it) or gets a feature gate back for minimal builds.

## IPC performance harness

The switchover carried no numbers — the enqueue-ack volume-lag bug on the C3 *felt* like transport latency until it was traced to daemon-side queue depth, and eyeballs can't tell transport QoS from a daemon flaw. A small harness can. The original "zenoh vs the old D-Bus stack" framing is now unrunnable (that stack is deleted), so the value is intra-zenoh: settings-rpc round-trip latency under scroll bursts (the coalescer's pacing), state-fanout latency (daemon publish → client mirror), and cold-start hydration time (open → caches drained). Trigger: a perceived-latency report, or a change to the coalescer / cache config that wants a before/after.

## Fresh consolidated design doc

docs/design.md is banner-marked as the retired D-Bus architecture. The topology/key-space/persistence consolidation this item originally asked for now lives in [docs/architecture.md](../architecture.md), which is the durable architecture reference. What remains open is folding the **stock-profile and pairing flows** (still only in code + the retired design.md) into a durable home — either a section in architecture.md or a fresh design.md — if a reader ever needs them narrated.

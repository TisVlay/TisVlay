# Switchover follow-ups (v0.3.0 zenoh stack)

Small items deferred at the v0.3.0 switchover, grouped here because each is too small for its own spec. Trigger: any of them blocking a release polish pass, or slack time.

## Tray gettext re-wiring

The v0.2 tray owned a gettext domain (`tisvlay-tray.pot`, build.rs-compiled `.mo`); the zenoh-era tray shipped without it, so its menu items and notification copy are English-only string literals. Re-wire `gettext()` around user-facing strings, restore the build.rs catalog compile, and re-add the tray extraction pass to `just i18n-extract` (the recipe went GUI-only at the switchover). The i18n architecture doc (docs/i18n.md) still describes the two-domain layout to return to.

## Mock backend ships unconditionally

The v0.2 daemon gated the mock vendor behind a `mock` cargo feature that packaging enabled deliberately ("Mock (testing)" for field debugging). The zenoh daemon registers `tisvlay-mock` unconditionally — same end state as the packages shipped anyway, but it is no longer a choice. Decide whether that stays deliberate (document it) or gets a feature gate back for minimal builds.

## Coverage baseline re-measurement

`cov_baseline_lines` in the justfile still carries the pre-switchover baseline. The measured population changed completely (old D-Bus crates deleted, wire tests added), so the ratchet number is stale in an unknown direction. Run `just cov`, set the baseline to the measured value, and let the ratchet resume from there.

## Fresh consolidated design doc

docs/design.md is banner-marked as the retired D-Bus architecture. The parity plan (docs/plans/parity-zenoh-ecs.md) is the accurate architecture reference but is written as a plan, not a design doc. Fold the plan's topology/key-space/persistence sections plus the stock-profile and pairing flows into a fresh docs/design.md once the stack has survived its duty week.

## Idea-file name sweep

Several pre-switchover idea specs reference the deleted crate names (`tisvlayd`, the harness) in prose and estimates. Their concepts survive the transport swap; sweep them for naming and re-validate any build-time numbers against the merged workspace when one of them is picked up.

# Move the pre-publish artifact smoke-test into CI

The release flow keeps a **manual verify-before-publish gate**: a `v*` tag builds the distro packages into a *draft* GitHub release, the maintainer downloads and tests the `.deb`/`.rpm`/`.pkg`, then promotes the draft to a full release — and only that human promote fires `release: released`, which publishes the crates to crates.io ([`.github/workflows/release.yml`](../../.github/workflows/release.yml), rationale under "Packaging" in [`docs/design.md`](../design.md)). crates.io is irreversible (yank only rolls forward), and we ship *libraries*, so a bad publish strands downstream pinners — the gate is cheap insurance.

A release-flow survey of comparable Rust projects found that even the strongest documented human-checkpoint project (ripgrep) gates only on the binary **build** succeeding, not on anyone functionally exercising the artifact — the prevailing norm is "CI-green + `cargo yank` as the escape hatch." So our manual artifact test is already *more* conservative than the norm. This idea keeps that conservative posture but removes the manual toil: have CI install the freshly built package and smoke-test it, so "verified" means a machine actually ran the artifact rather than the maintainer downloading it by hand.

## Where it would live

A step in [`release.yml`](../../.github/workflows/release.yml)'s Phase 1 `build` job, after `just package-all` and before attaching to the draft (fail the job — and thus the draft — if the smoke check fails). This sits entirely on the tag-build side; it does **not** touch the human promote → `release: released` → crates.io chain, which stays as the gate trigger. It only upgrades what the maintainer is promoting on top of.

## How deep to smoke-test

Two levels, pick based on appetite for CI plumbing:

- **Level 0 — install + `--version`.** In a clean container per format (`debian` for the `.deb`, `fedora`/`rockylinux` for the `.rpm`, `archlinux` for the `.pkg.tar.zst`), install the package and run `tisvlay --version` / `tisvlayd --version`. Cheap, and catches the gross failures that a build-only gate misses: missing shared libraries (fontconfig et al.), wrong install paths, broken `postinstall.sh`/`preremove.sh` maintainer scripts, packaging metadata errors.
- **Level 1 — end-to-end via the mock backend.** Under `dbus-run-session`, start the *installed* `tisvlayd --session`, wait for the bus name, then `busctl`/`tisvlay` the `Manager1` interface, add a Mock TV, toggle power/volume/screen, and assert the state round-trips. This exercises the full D-Bus + backend path on the actual shipped binary with zero hardware.

Level 1 is the natural payoff of shipping the mock backend (Part E of the publishing plan — the daemon now registers the `"Mock (testing)"` vendor in distro builds, [`crates/tisvlay-mock`](../../crates/tisvlay-mock/)). That backend exists so a field user — or a bug report — can confirm the daemon / D-Bus / desktop stack works without a real TV; **CI just becomes the first such "field" user.** The same install-and-add-a-mock-TV script doubles as the artifact smoke-test and as living documentation of the field-debug procedure.

## Trade-offs to budget for

- **CI time per format.** One container spin-up + install per package format. Level 0 is seconds; Level 1 adds a session-bus dance. Could smoke only the `.deb` (the primary target) and treat `.rpm`/`.pkg` as build-only, since nfpm builds all three from the same binary.
- **Cross-distro install rough edges.** Dependency names differ across `apt`/`dnf`/`pacman`; a smoke that installs on all three is more surface to maintain than one that installs on Debian only.
- **Doesn't make the manual gate redundant by itself.** Even with CI smoke, the maintainer may still want eyes on a real desktop (tray/GUI/KDE quirks a headless container can't see). The win is that CI now covers the *mechanical* "does the package install and run" layer, narrowing the manual step to genuinely human-judgment checks — or letting the maintainer trust CI and skip the download entirely on routine releases.

## Out of scope

- The crates.io publish gate itself — unchanged; this only strengthens the signal feeding it.
- GUI/tray visual verification — a headless container can launch the binaries but can't meaningfully exercise the Slint window or the KDE tray; that stays manual (or a separate Wayland-headless effort).

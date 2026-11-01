# SBOM precision: unified features and Slint's elected license

The release SBOMs come from cargo-cyclonedx and are checked against what cargo-auditable embeds in the binaries ([docs/licensing.md](../licensing.md) § SBOMs). Two gaps are known and not yet worth closing.

## Feature unification across binaries

The release build compiles all `default-members` in one cargo invocation, so Cargo unifies features across them, while cargo-cyclonedx resolves each binary's crate on its own. That made no difference when the pipeline was built, but a future dependency could change it: a crate enabled only by the GUI's feature set would land in the CLI binary without the CLI's SBOM listing it.

`just sbom-check` would catch it (crates the binary records that the SBOM doesn't). The options then: build `tisvlay-cli` and `tisvlay-gui-app` in separate invocations (some shared crates compile twice), or pass the unified features to cargo-cyclonedx.

## Slint's elected license

The SBOM records Slint's declared expression, `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`, not the royalty-free option the project elected ([docs/licensing.md](../licensing.md) § License elections). CycloneDX 1.6 can mark a license as declared or concluded, but cargo-cyclonedx stops at 1.5.

## Trigger

`just sbom-check` failing on a crate only the binary records (the first), or cargo-cyclonedx gaining CycloneDX 1.6 output (the second).

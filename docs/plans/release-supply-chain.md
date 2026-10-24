# Release supply chain: immutable releases, provenance, SBOMs

A user who downloads a TisVlay package today can check it against `checksums-sha256.txt`, but nothing ties either file to this repo's CI: the checksums sit next to the packages they describe, and both can be replaced together. The SBOM that CI produces (`just sbom`) describes the whole workspace rather than what ships: 809 crates, 86 of them Windows/macOS/wasm-only, plus test-only crates. It has no checksums, and it's a 14-day CI artifact that no release carries.

This plan makes releases verifiable end to end:

- GitHub immutable releases lock each published tag and its assets.
- SLSA Build Level 3 provenance, signed through Sigstore, ties every package and binary to the commit and workflow that built it.
- One SBOM per shipped binary per architecture, with licenses and checksums, is attached to the release and signed against the packages.

v0.4.0 is held until all of it lands, so it's the first release with the full set.

## Decisions

- **Hold v0.4.0 for the whole plan.** The current `v0.4.0` draft is mislabeled: the tag points at `ad96bf7` while the workspace version is still `0.3.0`, so every asset is named `0.3.0`. The draft and its tag are deleted (step 0) and re-cut after the version bump, once steps 1–5 have merged.
- **Immutable releases, as a repository setting.** The release flow already follows GitHub's recommended order: the tag push builds a draft with every asset attached, and the maintainer's promote publishes it. Immutability locks the tag and the assets at that promote, and GitHub then issues a release attestation (`gh release verify`). Everything before the promote stays mutable, so re-running the tag build or moving the tag still works on a draft.
- **SLSA Build Level 3 through a reusable workflow in this repo.** `.github/workflows/build-packages.yml` (`on: workflow_call`) builds and signs, and `release.yml` calls it. The signing certificate then names `build-packages.yml` as the signer, and users verify with `--signer-workflow` pointing at it. A builder in a separate org repo would isolate the build further, but it's another repo to maintain and a ref to bump for every build change, and the same maintainer reviews both either way.
- **Signing in its own job, inside the reusable workflow.** Every process in a job with `id-token: write` can request that job's OIDC token, including the build scripts and proc macros of the roughly 640 crates the build compiles. GitHub's Level 3 example signs in the build job. Here the build jobs get no `id-token`; a separate `attest` job, which only downloads the build output and runs `actions/attest`, holds it. No dependency code runs where the signing token is available.
- **Verification pins the signer.** `gh attestation verify -R TisVlay/TisVlay` alone accepts a signature from any workflow in the repo, and `ci.yml`'s test job holds `id-token: write` (for Codecov) while running every dependency's code on each PR. So every documented command and check passes `--signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml` and `--source-ref refs/tags/vX.Y.Z`. With the signer pinned, `ci.yml`'s token can stay.
- **The six packages and the four binaries are all signed.** nfpm copies files into the packages unchanged, so an installed `/usr/bin/tisvlay` has the digest that was signed, and `gh attestation verify /usr/bin/tisvlay …` works on a machine where TisVlay is already installed.
- **SBOMs from cargo-cyclonedx, checked against cargo-auditable.**
  - One CycloneDX 1.5 JSON SBOM per shipped binary (`tisvlay`, `tisvlay-gui`) per architecture, limited to that binary, its target and its features, with licenses and checksums. A probe on x86_64 listed 456 crates for `tisvlay` and 608 for `tisvlay-gui`, every one with a license.
  - cargo-cyclonedx works the dependency set out from cargo metadata, not from the build. So the packages are built with cargo-auditable, which embeds the set that was actually compiled, and a release check fails if the two disagree.
  - The embedded data also lets Trivy, grype, syft and `cargo audit bin` scan installed binaries.
  - The survey below has the alternatives.
- **SBOMs as release assets and as attestations.**
  - Assets serve anyone who wants the file (Dependency-Track, `grype sbom:…`). They're listed in `checksums-sha256.txt` and, once published, covered by the release attestation.
  - SBOM attestations bind each SBOM to its binary and to its architecture's three packages, which an asset alone can't do.
  - Not inside the packages: no convention exists, and scanners read the embedded data instead ([docs/ideas/in-package-sbom.md](../ideas/in-package-sbom.md)).
- **Two pre-publish checks.** `release.yml` verifies every package and binary before it creates the draft, so a signing mistake means no draft. `just verify-release vX.Y.Z` downloads the draft the way a user would and runs the install docs' exact commands, which keeps those commands from drifting. After the promote it also runs `gh release verify`, since the release attestation only exists then.
- **One SBOM recipe.** `just sbom` becomes the release SBOM generator for the host architecture, and the release build runs it. Switching to Cargo's own SBOM precursor files once they're stable means changing only that recipe. `ci.yml`'s workspace-SBOM step and its artifact go.

## SBOM tooling survey (2026-10-23)

Downloads are crates.io's last-90-day counts on that date. The first two rows were measured on this repo; the rest come from each tool's docs.

| Tool | Maintainer | Limited to what ships? | Licenses | Checksums | Formats | Last release | Downloads (90 d) |
|---|---|---|---|---|---|---|---|
| cargo-sbom (current) | one individual | No: whole workspace, or one package | Yes, 809/809 | None | SPDX 2.3, CycloneDX 1.4–1.6 | 0.10.0, 2025-06-17 | 69k |
| **cargo-cyclonedx** (chosen) | CycloneDX project (OWASP) | Yes: one binary, target and feature set | Yes, 456/456 for `tisvlay` | Yes, except workspace crates | CycloneDX 1.3–1.5 | 0.5.9, 2026-03-19 | 867k |
| cargo-auditable, read by syft or Trivy | Rust Secure Code WG | Exact: read from the compiled binary | No | No | SPDX or CycloneDX, via the reader | 0.7.7, 2026-10-02 | 429k |
| syft on `Cargo.lock` | Anchore | No: every target | No | not checked | SPDX, CycloneDX | 1.55.0, 2026-10-19 | Go tool |
| Trivy on `Cargo.lock` | Aqua | No | No ([coverage](https://trivy.dev/latest/docs/coverage/language/rust/)) | not checked | SPDX, CycloneDX | 0.76.1, 2026-10-21 | Go tool |
| Cargo's `-Z sbom` | Cargo team | Exact: written by the build | No | not checked | precursor JSON; needs a converter | nightly only | n/a |

Today's SBOM also rewrites Slint's `GPL-3.0-only` as the deprecated SPDX id `GPL-3.0`.

Related tools that aren't SBOM generators, or don't fit:

- **dist** (cargo-dist): a complete release pipeline whose supply-chain options are cargo-auditable, cargo-cyclonedx, GitHub attestations and OmniBOR IDs. It shows which combination the Rust release tooling has standardized on.
- **cargo-about** (312k): the license bundle; it keeps that job. **cargo-license** (244k) and **cargo-bom** (688) produce license listings, not SBOMs.
- **rust-audit-info** (43k): extracts the embedded dependency list from a binary. Used for the check in step 3.
- **auditable2cdx** (113): an experimental converter from the embedded data to CycloneDX. **cargo-spdx**: abandoned since 2022.
- **GitHub's dependency-graph SBOM export**: repo-wide SPDX built from `Cargo.lock`, so it over-includes the same way today's SBOM does.

Trends:

- cargo-cyclonedx is downloaded about 12× as often as cargo-sbom, and cargo-sbom hasn't released in 16 months.
- cargo-auditable's embedded data is the distro convention for Rust: Alpine, NixOS, openSUSE, Void, Chimera and Wolfi build with it, and Ubuntu 26.04 does for some packages. Every major scanner reads it.
- Cargo's own SBOM support ([RFC 3553](https://github.com/rust-lang/rfcs/pull/3553)) is the long-term answer: precursor files written by the build itself, with resolved features.
  - It's a [2026 Rust project goal](https://github.com/rust-lang/goals/issues/649), but the [tracking issue](https://github.com/rust-lang/cargo/issues/16565) is still open, waiting on fingerprinting and an end-to-end demo in a standard SBOM format.
  - The precursor files don't carry licenses either, so licenses will still be joined in from cargo metadata.
- Part of what's pushing the tooling is the EU Cyber Resilience Act, which requires SBOMs for commercial products from December 2027. Non-commercial open source is mostly exempt.

## Implementation

### 0. Before starting (maintainer)

- [ ] Delete the `v0.4.0` draft and the `v0.4.0` tag. The cachix pins `v0.4.0-x86_64-linux` and `v0.4.0-aarch64-linux` point at the mislabeled build; the re-cut's push moves them (`--keep-revisions 1`).

### 1. Workflow hardening (its own PR)

This goes first because steps 3 and 4 rewrite `release.yml`.

- [ ] Pin every action to a commit SHA, with the tag in a comment; Dependabot's github-actions updates keep the pins current.
- [ ] `persist-credentials: false` on the remaining checkouts (`ci.yml`'s ci, msrv, features and docs jobs; `audit.yml`; `release.yml`'s build).
- [ ] Move `release.yml`'s top-level `contents: write` down to the jobs that need it.
- [ ] Replace `softprops/action-gh-release` with `gh release create --draft --verify-tag --generate-notes` plus `gh release upload`. CI only ever creates and fills drafts; publishing stays the maintainer's promote.
- [ ] actionlint and zizmor as prek hooks, so CI runs them too.
- [ ] Move `dorny/test-reporter` into a `workflow_run` workflow: PRs from forks get a read-only token and can't create check runs.

### 2. Immutable releases (maintainer, settings)

- [ ] Enable release immutability for the repo (Settings → General → Releases, or `gh api -X PUT repos/TisVlay/TisVlay/immutable-releases`), before the v0.4.0 promote. `GET` on that endpoint currently returns `{"enabled":false}`.
- [ ] Fix the comments that assume a tag can move after publishing: the `nix-cache` job's Push step in `release.yml` ("a re-pushed tag moves the pin") and the "Release pins" bullet in [nix-binary-cache.md](nix-binary-cache.md). A tag can now only move before the promote.
- No other changes. release-plz's per-package tags (`tisvlay-core-v0.4.0` and so on) aren't release tags, and `publish-crates` never modifies the GitHub release (`git_release_enable = false`).

### 3. Auditable builds and SBOM generation

- [ ] **Spike first:** build with `cargo auditable zigbuild --profile dist --target x86_64-unknown-linux-gnu.<glibc>` and check the result with `rust-audit-info target/x86_64-unknown-linux-gnu/dist/tisvlay`. cargo-auditable's README says it "works with any Cargo command", but nobody has tried it with zigbuild here. Open questions has the fallback.
- [ ] `package-build` builds through `cargo auditable zigbuild`.
- [ ] Rewrite `just sbom`:
  - Run cargo-cyclonedx for the package triple: `--describe binaries --target <triple> -f json --spec-version 1.5`, with `SOURCE_DATE_EPOCH` set from the commit time so reruns produce identical files.
  - Move the two shipped binaries' SBOMs to `target/sbom/<triple>/{tisvlay,tisvlay-gui}.cdx.json` and delete the rest. In the probe, cargo-cyclonedx wrote one file per workspace binary, next to each crate's `Cargo.toml` (including `gen-docs`), whatever `--manifest-path` named.
  - Add `*.cdx.json` to `.gitignore` as a backstop.
- [ ] Add `just sbom-check`: for each shipped binary, compare the SBOM's `name@version` set against what `rust-audit-info` reports for the built binary, and fail with the difference. The build job runs it after `package-build` and `sbom`.
- [ ] Tools: add cargo-auditable, cargo-cyclonedx and rust-audit-info to the flake's dev shell. CI installs the same versions through `taiki-e/install-action`, pinned like the other version-sensitive tools. cargo-sbom leaves both.
- [ ] `ci.yml`: drop the `SBOM` and `Upload SBOM` steps, and `cargo-sbom` from the Setup tool list.
- Nix needs nothing: `buildRustPackage` already builds through `auditable-cargo`.

### 4. SLSA Level 3 build and signing (`build-packages.yml`)

- [ ] A new reusable workflow, `on: workflow_call`, with two jobs:
  - `build` (matrix: `ubuntu-latest`, `ubuntu-26.04-arm`), `contents: read` only. Today's build steps plus step 3's `sbom` and `sbom-check`. Uploads the packages, the two binaries and the two SBOMs for each architecture.
  - `attest`, which needs `build` and has `id-token: write`, `attestations: write` and `contents: read`. It downloads the build output and runs only `actions/attest`:
    - provenance for the six packages and four binaries, in one call with `subject-checksums`. The binary subjects are named by architecture (e.g. `x86_64/tisvlay`), since both architectures' files share a name;
    - one SBOM attestation per binary per architecture (`sbom-path`), whose subjects are that binary and its architecture's three packages.
- [ ] `release.yml`'s tag-push `build` job becomes a call to `./.github/workflows/build-packages.yml`. The calling job grants `id-token: write`, `attestations: write` and `contents: read`; GitHub requires the caller and the reusable workflow to both have them.
- [ ] `release.yml`'s `release` job (`contents: write`, `attestations: read`) does the following before it creates the draft:
  - runs `gh attestation verify` on each package and binary with `--signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml --source-ref "$GITHUB_REF" --deny-self-hosted-runners`, then again with `--predicate-type https://cyclonedx.org/bom` for the SBOMs (confirm that predicate URI against `actions/attest` while implementing);
  - extracts the binaries from each `.deb`, `.rpm` and `.pkg.tar.zst` and verifies those as well, which proves the installed-binary claim for every format;
  - writes `checksums-sha256.txt` over the packages and SBOMs, then creates the draft and uploads.
- [ ] The signer path appears in `release.yml`, the `verify-release` recipe and the book. A comment at each points to the others.

### 5. Verification tooling and docs

- [ ] Add `just verify-release <tag>`:
  - `gh release download <tag>` into `target/verify/<tag>/` (drafts included, with the maintainer's auth), then `sha256sum -c checksums-sha256.txt`;
  - the provenance and SBOM checks from step 4 on each package, and on the binaries extracted from the host's package format;
  - once the release is published, `gh release verify <tag>`, plus `gh release verify-asset <tag> <file>` for each asset.
- [ ] `docs/build.md` release checklist: run `just verify-release vX.Y.Z` on the draft before promoting, and again after.
- [ ] Book, `getting-started/installation.md`: a "Verify your download" section covering:
  - a downloaded package: `gh attestation verify <file> -R TisVlay/TisVlay --signer-workflow TisVlay/TisVlay/.github/workflows/build-packages.yml --source-ref refs/tags/vX.Y.Z`;
  - an installed binary: the same command on `/usr/bin/tisvlay`;
  - the SBOM: add `--predicate-type https://cyclonedx.org/bom --format json --jq '.[].verificationResult.statement.predicate'`;
  - the release itself: `gh release verify vX.Y.Z` and `gh release verify-asset vX.Y.Z <file>`;
  - Nix: the cachix signing key already covers prebuilt store paths (`packaging/nix/README.md`).
- [ ] `packaging/README.md`: the release pipeline's new shape (build → attest → verify → draft → promote).

### 6. The v0.4.0 re-cut

- [ ] Bump the workspace to 0.4.0, tag it, and watch the run: the draft should appear only after verification passes. Run `just verify-release v0.4.0` on the draft.
- [ ] Promote it, then run `just verify-release v0.4.0` again, now including `gh release verify`. Check that the release page shows "Immutable" and that the attestations appear in the repo's Actions tab.
- [ ] On a machine with the `.deb` installed, run `gh attestation verify /usr/bin/tisvlay …`.

### Related fix (separate PR, not blocking)

The flake pins cargo-about 0.9.1 only for the dev shell (`cargoAbout` in `flake.nix`). `packaging/nix/package.nix` gets nixpkgs' 0.9.0, which, according to `about.toml`'s comment, silently drops the Slint crates' royalty-free license text from `THIRD-PARTY-LICENSES.html`. Move `cargoAbout` out of the devShell's `let`, pass it to both `callPackage ./packaging/nix/package.nix` sites (the overlay and `packages`), and confirm with a `nix build` that the bundle contains the Slint text.

## Open questions

- **cargo-auditable with zigbuild.** If the spike fails, try composing them the other way round, through cargo-auditable's documented setup for replacing `cargo` (`REPLACING_CARGO.md`). If nothing works, `sbom-check` has nothing to compare against, and the plan falls back to cargo-cyclonedx alone, with that gap documented.
- **Feature unification.** The release build compiles all `default-members` in one cargo invocation, so Cargo unifies features across them, while cargo-cyclonedx resolves each binary's crate on its own. If `sbom-check` finds crates that only the binary records, that's the likely cause. Options: build `tisvlay-cli` and `tisvlay-gui-app` in separate invocations (some shared crates then compile twice), or pass the unified features to cargo-cyclonedx. The same check settles whether the 11 platform-specific-looking crate names in the GUI's x86_64 SBOM are really compiled in.
- **The elected Slint license.** The SBOM records Slint's declared expression, `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`, not the royalty-free option the project elected (`docs/licensing.md`). CycloneDX 1.6 can mark a license as declared or concluded, but cargo-cyclonedx stops at 1.5. Revisit when it supports 1.6.

## Alternatives considered

- **SLSA Build Level 2.** This is what `actions/attest` gives on its own. Signing in a separate job in `release.yml` would already keep the token away from dependency code; Level 3 adds the reusable-workflow signer identity that GitHub requires before you can claim it. Signing inside the build jobs was rejected outright, because every dependency's build script could then sign.
- **A builder in a separate org repo:** stronger isolation, but a second repo to maintain and a ref to bump for every build change (see Decisions).
- **OpenSSF's slsa-github-generator:** the older Level 3 route, which publishes in-toto provenance files as release assets. GitHub's `attest` is first-party, and it's verified with the same `gh` the docs already use.
- **syft on the auditable binaries:** an exact crate set, but cargo-auditable records no licenses or checksums, and syft (1.54) can't add licenses for Rust. A script to merge them in from cargo metadata would be ours to maintain.
- **cargo-sbom (today's tool), filtered down to the binaries:** it has no target, feature or binary options, records no checksums, and rewrites `GPL-3.0-only` as `GPL-3.0`.
- **Cargo's `-Z sbom` precursor files:** the long-term answer, but nightly-only. Revisit once it's stable; `just sbom` is the one place to change.
- **dist (cargo-dist):** bundles cargo-auditable, cargo-cyclonedx and GitHub attestations, but replacing the nfpm and zigbuild pipeline to get them isn't worth it.
- **An SBOM inside the packages:** deferred to [docs/ideas/in-package-sbom.md](../ideas/in-package-sbom.md).
- **Attesting the Nix outputs:** cachix already signs them, Nix users verify with the cache key, and nobody would run `gh attestation verify` on a store path.
- **Attesting the `.crate` files:** no crates.io user would check a GitHub attestation for them, and trusted publishing already ties each publish to `release.yml`.

## Migration on merge

Per [README.md](README.md): the decisions and the survey move to `packaging/README.md` (release pipeline) and `docs/licensing.md` (SBOM tooling), and any open questions still open move to `docs/ideas/`.

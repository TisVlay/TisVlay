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
- **Sigstore bundles as release assets too.** `gh attestation verify` looks attestations up through the GitHub API, so the person verifying needs gh signed in. The signing job's bundles also go into the draft as `.sigstore.json` assets, so anyone can verify offline or without a GitHub account, and the bundles are locked in with the release. They're also what OpenSSF Scorecard's Signed-Releases check looks for, since it only checks release assets.
- **Publishing needs the maintainer's approval, not just a `v*` tag.** A second account has write access, and write access can push tags and publish releases, while the `crates-io` and `cachix` environments only check the tag's name. So both environments require the maintainer's approval, and a tag ruleset limits creating, moving and deleting `v*` tags to repository admins, as the `main` ruleset does for `main`. release-plz's per-crate tags (`tisvlay-core-v…`) don't match `v*`.
- **A security policy.** `SECURITY.md` says how to report a vulnerability, and GitHub's private vulnerability reporting is turned on, so reports don't have to be public issues.
- **The 0.4.0 bump comes from `release-plz update`, run locally.** It picks the version from the conventional-commit history, updates the workspace version and the internal dependency requirements, and writes each crate's first `CHANGELOG.md`; the result is reviewed and merged as a normal PR. The release-PR workflow (`release-plz.yml`) stays off: a PR opened with the workflow's own token doesn't trigger CI, so it could never pass the `CI result` check `main` requires. Turning it on later means giving it a GitHub App token.
- **Rehearse with `v0.4.0-rc.1` before v0.4.0.** The signing and verification only run on a tag push, so a throwaway rc tag on the bump commit runs the whole pipeline against a draft that's never published, then gets deleted along with its tag. The cost is one cachix push (pinned as `v0.4.0-rc.1-<system>`), and the rc's signatures stay in the public transparency log.
- **Two PRs for steps 3–5:** step 3 on its own, since it can be tested locally; then steps 4 and 5 together, so the signer path the release job, the recipe and the book share is reviewed once.

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

- [x] Delete the `v0.4.0` draft and the `v0.4.0` tag (2026-10-23). The cachix pins `v0.4.0-x86_64-linux` and `v0.4.0-aarch64-linux` still point at the mislabeled build; the re-cut's push moves them (`--keep-revisions 1`).

### 1. Workflow hardening (its own PR)

This goes first because steps 3 and 4 rewrite `release.yml`.

- [x] Pin every action to a commit SHA, with the release in a comment (pinact); Dependabot's github-actions updates keep the pins current.
- [x] `persist-credentials: false` on the remaining checkouts (`ci.yml`'s ci, msrv, features and docs jobs; `audit.yml`; `release.yml`'s build).
- [x] No workflow-level grants: `release.yml`'s top-level `contents: write`, and `release-plz.yml`'s default token, became `permissions: {}`, since every job declares its own.
- [x] Replace `softprops/action-gh-release` with `gh release create --draft --verify-tag --generate-notes` plus `gh release upload`. CI only ever creates and fills drafts; publishing stays the maintainer's promote. A rerun refreshes the tag's draft and deletes assets the new build didn't produce; a published release fails the job instead.
- [x] actionlint and zizmor as prek hooks (`just actionlint`, `just zizmor`), run by CI's `checks` job on every change, where zizmor also runs its online audits.
- [x] Move `dorny/test-reporter` into a `workflow_run` workflow (`test-report.yml`): PRs from forks get a read-only token and can't create check runs.
- [x] A one-week Dependabot cooldown for both ecosystems, which zizmor's Dependabot audit asked for.

### 2. Repository settings and security policy

Settings the maintainer applies (an admin token is needed), any time before the v0.4.0 tag:

- [x] **Immutable releases** (enabled 2026-10-24).

  ```bash
  gh api -X PUT repos/TisVlay/TisVlay/immutable-releases
  ```

- [x] **Required reviewers** (2026-10-24) on the `crates-io` and `cachix` environments: Settings → Environments → each one → Required reviewers → `ruffsl`, with "Prevent self-review" left off so you can approve your own release. Keep the `v*` tag rule. Afterwards, check that each still admits only `v*` tags and now lists a reviewer:

  ```bash
  for e in crates-io cachix; do
    gh api "repos/TisVlay/TisVlay/environments/$e" --jq '{name, rules: [.protection_rules[].type]}'
    gh api "repos/TisVlay/TisVlay/environments/$e/deployment-branch-policies" --jq '[.branch_policies[] | "\(.type):\(.name)"]'
  done
  ```

  Each tag build's `nix-cache` job, and each `publish-crates` run after a promote, then waits for one approval.
- [x] **Tag ruleset** ("release tags", 2026-10-24): only repository admins may create, move or delete `v*` tags, with the same bypass as the `main` ruleset.

  ```bash
  gh api -X POST repos/TisVlay/TisVlay/rulesets --input - <<'EOF'
  {
    "name": "release tags",
    "target": "tag",
    "enforcement": "active",
    "conditions": { "ref_name": { "include": ["refs/tags/v*"], "exclude": [] } },
    "rules": [{ "type": "creation" }, { "type": "update" }, { "type": "deletion" }],
    "bypass_actors": [{ "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }]
  }
  EOF
  ```

- [x] **Private vulnerability reporting** (2026-10-24):

  ```bash
  gh api -X PUT repos/TisVlay/TisVlay/private-vulnerability-reporting
  ```

- [x] **`SECURITY.md`** ([#200](https://github.com/TisVlay/TisVlay/pull/200)): which versions get fixes, how to report (the private reporting form), and what to expect after reporting.

With immutability on:

- [x] Fix the comments that assume a tag can move after publishing: the `nix-cache` job's Push step in `release.yml` ("a re-pushed tag moves the pin") and the "Release pins" bullet in [nix-binary-cache.md](nix-binary-cache.md). A tag can now only move before the promote, and only by an admin.
- Nothing else changes. release-plz's per-package tags (`tisvlay-core-v0.4.0` and so on) aren't release tags, and `publish-crates` never modifies the GitHub release (`git_release_enable = false`).

### 3. Auditable builds and SBOM generation

- [x] **Spike (2026-10-23, x86_64):** `cargo auditable zigbuild --profile dist --target x86_64-unknown-linux-gnu.2.34`, with nixpkgs' cargo-auditable 0.7.6, built as fast as usual (98 s warm).
  - Both binaries carry a `.dep-v0` section: 457 crates for `tisvlay`, 609 for `tisvlay-gui`. Their newest glibc symbol is still `GLIBC_2.34`.
  - cargo-cyclonedx's per-binary SBOMs list exactly the same `name@version` set, apart from the root crate: the SBOM describes it in `metadata.component`, under the binary's name. So the single cargo invocation's feature unification adds nothing, and the 11 platform-specific-looking crates in the GUI's SBOM really are compiled in.
- [x] `package-build` builds through `cargo auditable zigbuild`.
- [x] Rewrite `just sbom`:
  - Run cargo-cyclonedx for the package triple: `--describe binaries --target <triple> -f json --spec-version 1.5`, with `SOURCE_DATE_EPOCH` set from the commit time so reruns produce identical files.
  - Move the two shipped binaries' SBOMs to `target/sbom/<binary>-<version>-<arch>.cdx.json` (for example `tisvlay-gui-0.4.0-x86_64.cdx.json`, the release asset names) and delete the rest. In the probe, cargo-cyclonedx wrote one file per workspace binary, next to each crate's `Cargo.toml` (including `gen-docs`), whatever `--manifest-path` named.
  - Add `/crates/*/*.cdx.json` to `.gitignore` as a backstop.
- [x] Add `just sbom-check`: for each shipped binary, compare the SBOM's `name@version` set against what `rust-audit-info` reports for the built binary, leaving out the root crate (see the spike), and fail with the difference. `release.yml`'s build job runs it after `just package-all` and attaches the SBOMs to the draft (listed in `checksums-sha256.txt`); step 4 signs them.
- [x] Tools: add cargo-auditable, cargo-cyclonedx and rust-audit-info to the flake's dev shell. `release.yml` installs the same versions through `taiki-e/install-action`, and now pins its other tools (cargo-about, cargo-nfpm, cargo-zigbuild) to the flake's versions too; until now it installed whatever was newest. cargo-sbom leaves both the dev shell and CI, and jq joins the dev shell.
- [x] `ci.yml`: drop the `SBOM` and `Upload SBOM` steps, and `cargo-sbom` from the Setup tool list.
- Nix needs nothing: `buildRustPackage` already builds through `auditable-cargo`.

### 4. SLSA Level 3 build and signing (`build-packages.yml`)

- [x] A new reusable workflow, `on: workflow_call`:
  - `build` (matrix: `ubuntu-latest`, `ubuntu-26.04-arm`), `contents: read` only. Today's build steps plus `just sbom-check`. It stages one flat artifact per architecture: `assets/` (the packages and SBOMs) and `attest/` (checksum files naming what to sign, so the attest job needs no binaries). One flat directory, because `upload-artifact` roots a multi-path upload at the paths' common ancestor; step 3's upload mixed `target/nfpm-dist/` and `target/sbom/`, which would have put the release job's files in subfolders.
  - `attest` (one leg per architecture), with `id-token: write`, `attestations: write` and `contents: read`. It downloads its architecture's artifact and runs only `actions/attest` (v4.2.2), plus a few lines of shell:
    - one provenance attestation over the three packages and two binaries (`subject-checksums`; binaries named `<arch>/<binary>`);
    - one SBOM attestation per binary (`sbom-path`, predicate `https://cyclonedx.org/bom`), over the binary and the three packages;
    - the three Sigstore bundles, uploaded as `tisvlay-<version>-<arch>.provenance.sigstore.json` and `<binary>-<version>-<arch>.cdx.sigstore.json`.
- [x] `release.yml`'s tag-push `build` job calls it, granting `id-token: write`, `attestations: write` and `contents: read`.
- [x] A new `verify` job (`contents: read`, `attestations: read`) runs `just verify-assets` on the downloaded assets and bundles before any draft exists. So the check that gates the draft is the one the maintainer runs.
- [x] The `release` job writes `checksums-sha256.txt` over the packages, SBOMs and bundles, and attaches them all to the draft.
- [x] The signer path appears in `build-packages.yml`'s header, `just verify-assets` and the book, each pointing to the others.

### 5. Verification tooling and docs

- [x] `just verify-assets <dir> <ref>`:
  - the checksums;
  - every package and every binary unpacked from them (with bsdtar; an architecture's three packages must carry identical binaries): provenance, then the SBOM attestation, looked up on GitHub and checked from the bundles alone;
  - that each SBOM asset is the SBOM that was signed.

  A dry run with a stand-in `gh` covered its control flow (56 verifications, 30 of them from bundles) and its rejection of an SBOM that doesn't match.
- [x] `just verify-release <tag>`: downloads the release (drafts included) into `target/verify/<tag>/`, runs `just verify-assets`, then the book's cosign command on the x86_64 `.deb`. Once the release is published, it also runs `gh release verify` and `gh release verify-asset` on every asset.
- [x] Where the release steps are written down: `release.yml`'s header and `packaging/README.md`'s new "Release signing and verification" section, rather than `docs/build.md`, which has no release checklist.
- [x] Book: a new Reference page, [Security & Supply Chain](../book/src/reference/security-supply-chain.md), linked from the install page's opening paragraph. It covers checksums; signatures, with gh for a package or an installed binary and cosign for people without a GitHub account; the SBOMs; scanning an installed system with `cargo audit bin`, Trivy and grype; and the cache signing key for Nix users.
- [x] The dev shell gains gh, libarchive (bsdtar) and cosign.
- [ ] **Confirm at the rehearsal:** the cosign command (the flags of cosign 3.x against a multi-subject GitHub bundle), and whether `gh attestation verify --bundle` works without a GitHub login. If it does, the book can offer it as the offline option.

### 6. The v0.4.0 re-cut

- [x] Add release-plz and cargo-semver-checks to the dev shell. On a branch, run `release-plz update`, review the version bump and the new `CHANGELOG.md` files, and merge it as a normal PR.
  - release-plz 0.3.172 picked up `.config/release-plz.toml` and bumped all three published crates to 0.4.0.
  - cargo-semver-checks found `tisvlay-core` API-compatible, and one break in the SDK: `StateEvent::ReleaseChecked`, a variant added to an exhaustive enum. That's a breaking change at 0.x, which the minor bump covers.
  - `tisvlay-protocol` has no commits since 0.3.0, so release-plz wrote it no entry. Its CHANGELOG gets one by hand, saying it moves in lockstep.
  - The Nix package now reads its version from `Cargo.toml`, as the flake does for the MSRV, so a bump needs no edit there.
  - `just package-check` packages and builds all three crates at 0.4.0, CHANGELOGs included.
  - The CHANGELOGs' compare links use per-crate tags (`tisvlay-core-v0.3.0…`). v0.3.0 was published by hand, so those tags don't exist; all three crates were published from `c8809db`, the `v0.3.0` commit, which is where they'd go. release-plz creates the 0.4.0 ones when it publishes.
- [ ] **Rehearsal:** tag the merge commit `v0.4.0-rc.1` and watch the run. The draft should appear only after verification passes, and the `nix-cache` job waits for your approval. Run `just verify-release v0.4.0-rc.1` on the draft. Fix anything that fails and repeat with `-rc.2`. Then delete the rc draft and its tag, never publishing it.
  - `rc.1` (2026-10-24): both architectures packaged, and arm64 passed `sbom-check` on its first run. Then Stage failed: the glob `tisvlay-*-<arch>.cdx.json` also matched `tisvlay-gui-…`. Fixed in #208, which also gives each architecture's attest files their own directory.
  - `rc.2`: the build and the provenance attestation passed. The first SBOM attestation failed: `actions/attest` only accepts a CycloneDX SBOM that has a `serialNumber`, which cargo-cyclonedx leaves out under `SOURCE_DATE_EPOCH`. Fixed by having `just sbom` add a name-based UUID for the commit, binary and architecture, which keeps reruns byte-identical.
- [ ] Tag the same commit `v0.4.0`, watch the run, and run `just verify-release v0.4.0` on the draft.
- [ ] Promote it and approve the `publish-crates` deployment, then run `just verify-release v0.4.0` again, now including `gh release verify`. Check that the release page shows "Immutable" and that the attestations appear in the repo's Actions tab.
- [ ] On a machine with the `.deb` installed, run `gh attestation verify /usr/bin/tisvlay …`.

### Related fix (separate PR, not blocking)

- [x] [#201](https://github.com/TisVlay/TisVlay/pull/201): the Nix package built with nixpkgs' cargo-about 0.9.0, because the flake pinned 0.9.1 for the dev shell only. So the cached build of `main` ships no Slint license text, in either the installed `THIRD-PARTY-LICENSES.html` or the bundle the GUI embeds.
  - `packaging/nix/cargo-about.nix` now provides at least 0.9.1 inside `package.nix`, which covers the flake's package, the overlay and the module's fallback; the dev shell uses the same file.
  - 0.9.1 rejected the `no-clearly-defined` key the Nix build used to insert, so that `sed` is gone.

## Open questions

- **aarch64.** The spike covered x86_64 only; `sbom-check` covers the arm leg on its first run.
- **Feature unification, later.** The release build compiles all `default-members` in one cargo invocation, so Cargo unifies features across them, while cargo-cyclonedx resolves each binary's crate on its own. That made no difference in the spike, but a future dependency could change that. If `sbom-check` then finds crates that only the binary records, the options are building `tisvlay-cli` and `tisvlay-gui-app` in separate invocations (some shared crates then compile twice), or passing the unified features to cargo-cyclonedx.
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
- **OpenSSF Scorecard:** deferred to [docs/ideas/openssf-scorecard.md](../ideas/openssf-scorecard.md). zizmor already gates the workflow checks it would score.
- **The release-PR workflow with the default token:** its PRs don't trigger CI, so they can't pass `main`'s required checks; see Decisions.
- **Attesting the Nix outputs:** cachix already signs them, Nix users verify with the cache key, and nobody would run `gh attestation verify` on a store path.
- **Attesting the `.crate` files:** no crates.io user would check a GitHub attestation for them, and trusted publishing already ties each publish to `release.yml`.

## Migration on merge

Per [README.md](README.md): the decisions and the survey move to `packaging/README.md` (release pipeline) and `docs/licensing.md` (SBOM tooling), and any open questions still open move to `docs/ideas/`.

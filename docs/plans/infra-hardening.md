# Workspace infrastructure hardening

Status: plan / baseline. A batch of build, release, and supply-chain infrastructure improvements, each justified on its own merits. No new shell scripts — every change is TOML, YAML, Nix, a `just` recipe (tool invocation only), a prek hook, or a `uses:` GitHub Action.

## Motivation

Four soft spots in the workspace scaffolding, closed together because they share a toolchain/flake bump:

1. **Reproducibility** — `rust-toolchain.toml` floats on `channel = "stable"`, so a build is not reproducible from a git SHA alone.
2. **Supply-chain coverage** — `deny.toml` gates licenses + duplication but scopes out source provenance and RustSec advisories, so an off-registry dep passes silently and a vuln disclosed against a pinned dep is never surfaced.
3. **Commit-subject discipline** — release-plz derives versions/CHANGELOGs from commit subjects, but nothing enforces Conventional Commits, so a malformed subject that reaches `main` is a latent release bug.
4. **Attribution completeness** — the cargo-about `LicenseRef-*` clarify blocks (Slint family) are configured but inert on the pinned cargo-about, so `THIRD-PARTY-LICENSES.html` ships without the Slint license text.

## In scope

### 1. Pin the Rust toolchain (+ flake input bump, LLVM alignment)

The pinned `rust-overlay` only resolves up to the old stable, so pinning current stable requires bumping the flake inputs; that same bump supplies the newer cargo-about for item 2.

- `flake.lock`: `nix flake update nixpkgs rust-overlay` (one bump; verify the whole `just check` + `nix flake check` after).
- `rust-toolchain.toml`: `channel = "stable"` → the full three-component current stable (e.g. `1.97.0` — pin the exact string for reproducibility). Keep `profile = "default"` (it already includes rustfmt+clippy on both the Nix and rustup sides — no `components` edit needed). No new target.
- `flake.nix`: align the LLVM bintools to the pinned rustc's LLVM major (verify with `rustc -vV` on the pinned toolchain; ensure `llvmPackages_NN` exists in the bumped nixpkgs). **Bind once** — `llvmPkgs = pkgs.llvmPackages_NN` in the `let`, referenced at the three sites (`bintools-unwrapped`, `LLVM_COV`, `LLVM_PROFDATA`) — and drop the LLVM-major from the two nearby comments so the value lives in exactly one place.
- `Cargo.toml` `rust-version = "1.92"` stays as the MSRV floor (distinct from the pinned dev/CI channel).
- Maintenance note: Dependabot does not bump `rust-toolchain.toml`; the pin is a deliberate, reviewed bump.

Files: `flake.lock`, `rust-toolchain.toml`, `flake.nix`.

### 2. Bump cargo-about (complete the Slint attribution) + simplify CI install

- The bumped nixpkgs supplies a cargo-about that honors `license_files` for `LicenseRef-*` clarify blocks (verify the version is at/after the release that carries that fix; override in the flake only if nixpkgs lags).
- `.github/workflows/ci.yml`: drop the manual `cargo install cargo-about --locked --version 0.9.1 --features cli` step; add `cargo-about` to the existing `taiki-e/install-action` tool list (the pinned `@v2` floats to a release that carries it). Verify the prebuilt exposes the `cargo about` subcommand (i.e. built with the `cli` feature) — keep the manual install as fallback only if it does not.
- `packaging/licensing/about.toml`: the Slint clarify blocks now surface text — update the comment that says the fix is "not yet shipped," and remove any "known gap / warnings persist" note that no longer holds.
- Regenerate `packaging/licensing/THIRD-PARTY-LICENSES.html` via `just licenses-html` and commit it (the Slint license text now lands).
- `docs/build.md`: correct the "CI omits cargo-about" note if the swap changes it.

Files: `flake.nix`/`flake.lock` (shared with item 1), `.github/workflows/ci.yml`, `packaging/licensing/about.toml`, `packaging/licensing/THIRD-PARTY-LICENSES.html`, `docs/build.md`.

### 3. Conventional-commit linting

History is squash-merge (`(#NNN)` subjects, no merge commits), so the PR title is what lands on `main` and reaches release-plz. Therefore:

- **CI (authoritative):** add a `commit-lint` job to `ci.yml` using `amannn/action-semantic-pull-request` (a `uses:` action — no shell) to lint the **PR title** on the uncustomized Conventional-Commits default type set.
- **Local (advisory aid):** add `compilerla/conventional-pre-commit` as a prek **remote** hook, pinned `rev`, `stages: [commit-msg]`, default type set (no `args` customization, so it can't diverge from the CI check). Add `default_install_hook_types = ["pre-commit", "commit-msg"]` — placed **above the first `[[repos]]`** in `prek.toml` (top-level key; appended at the bottom it mis-scopes into the last repo table). prek runs `language: python` remote hooks in an isolated uv venv and supports the commit-msg stage. `prek run --all-files` in CI runs the pre-commit stage, so this commit-msg hook is correctly skipped there.
- **Dependabot prefix fix:** `dependabot.yml` currently uses `prefix: "choir: 🛠️ "` / `"choir: 📦 "` — not a Conventional type. Switch to `prefix: "ci: "` (github-actions) / `"chore: "` (cargo) — colon+space, since dependabot concatenates prefix+summary verbatim (a bare `"ci"` yields `ci bump …` and fails the regex).
- No `just commit-lint` recipe (a shell-free range lint isn't cleanly expressible; the prek hook + CI action cover it).

Files: `.github/workflows/ci.yml`, `prek.toml`, `.github/dependabot.yml`.

### 4. cargo-deny: `[sources]` (blocking) + `[advisories]` (weekly, non-blocking)

The current lockfile already carries several live, mostly-transitive, mostly-unfixable advisories, so advisories must not gate PRs/commits.

- `packaging/licensing/deny.toml`:
  - Add `[sources]`: `unknown-registry = "deny"`, `unknown-git = "deny"`, `allow-registry = ["https://github.com/rust-lang/crates.io-index"]`, `allow-git = []`. Verified safe — zero git/off-registry deps.
  - Add `[advisories]`: `db-urls = ["https://github.com/rustsec/advisory-db"]`, `yanked = "deny"`, and an `ignore = [...]` seeded with the current known advisories (get the exact RUSTSEC IDs by running `cargo deny check advisories`), each with a one-line reason (transitive + no-upstream-fix, or unmaintained-accepted) so the weekly job is green-until-a-new-disclosure rather than perpetually red.
  - Update the file header comment that currently says advisories/sources are "out of scope."
- `justfile`: extend the `deny` recipe's check list to `licenses bans sources` (blocking; **not** advisories — keeps the prek `deny` hook offline-friendly, no advisory-db fetch on lockfile commits).
- New `.github/workflows/audit.yml`: a weekly `schedule:` (+ `workflow_dispatch`) job running `EmbarkStudios/cargo-deny-action` with `command: check advisories` (a `uses:` action — no shell). Non-blocking on PRs by construction.
- `docs/licensing.md`: update in place — document `[sources]` (blocking) and `[advisories]` (weekly scheduled, non-blocking), and fix the "two checks" wording.

Files: `packaging/licensing/deny.toml`, `justfile`, `.github/workflows/audit.yml`, `docs/licensing.md`.

### 5. SBOM generation (SPDX, CI artifact)

- `flake.nix`: add `cargo-sbom` to the dev-shell package list (present in the bumped nixpkgs).
- `justfile`: `sbom_dir := "target/sbom"` + an `sbom` recipe (grouped `check`): `mkdir -p {{ sbom_dir }}` then `cargo sbom --output-format spdx_json_2_3 > {{ sbom_dir }}/tisvlay.spdx.json`. SPDX only by default (CycloneDX is one extra `--output-format cyclone_dx_json_1_6` line if a consumer ever needs it). Tool invocations only — the existing recipe idiom.
- `.github/workflows/ci.yml`: an `SBOM` step (`if: ${{ !cancelled() }}`) + `upload-artifact`, mirroring the lcov artifact.
- Not added to the blocking `check` gate (a generated artifact, not a policy gate). `target/sbom/` is already gitignored (bare `target`).

Files: `flake.nix`, `justfile`, `.github/workflows/ci.yml`.

### 6. Low-risk cleanups

- `justfile`: lift the shared scope into `check_flags := "--profile test --workspace --all-targets --all-features"` so `build` and `clippy` can't drift (matches the existing `cov_exclude` variable idiom; `test`/`test-ci` intentionally differ and keep their own strings).
- `.github/workflows/ci.yml`: guard the `dorny/test-reporter` step with `hashFiles('target/nextest/ci/junit.xml') != ''` — log hygiene (the reporter is already `fail-on-error: false`, so this only keeps a pre-nextest failure from emitting a confusing "no report files" line).
- `.github/dependabot.yml`: group updates — all github-actions bumps into one PR, all routine cargo updates into one PR. Folds in the prefix fix from item 3.

Files: `justfile`, `.github/workflows/ci.yml`, `.github/dependabot.yml`.

## Not in scope

- **Per-file SPDX headers** — declined (large mechanical change, low value for a single Apache-2.0 license).
- **Opt-in nightly `.#fast` dev lane** — deferred; the measured win is a cold-build-only, few-percent gain that does not apply to this workload, against real nightly-pin upkeep. The existing idea note ([docs/ideas/ci-nightly-build-knobs.md](../ideas/ci-nightly-build-knobs.md)) already tracks the re-open trigger (CI turnaround becoming a bottleneck); left as-is.
- **MSRV verification job** — pinning the channel above the `rust-version = "1.92"` floor widens the untested MSRV gap; a cheap `cargo check` on 1.92 (or `cargo-msrv`) is a reasonable *follow-up*, not part of this batch.
- **No new shell scripts** — enforcement is declarative config + tool-invoking recipes + `uses:` actions.

## Guardrails

- No bash/sh scripts; `just` recipes are tool invocations (existing idiom).
- American spelling; no hard-wrapped markdown; no plan-phase or version-anchored facts in checked-in code comments.
- Each commit leaves `just check` green.

## Delivery (staged commits, off a fresh branch from `main`)

- **C0** — this plan doc → `docs/plans/`.
- **C1** — flake input bump + toolchain pin + LLVM bind-once alignment + `check_flags` + the `hashFiles` guard (the build-config chunk; the flake bump is the riskiest step — verify `just check`, `just cov`, and `nix flake check`).
- **C2** — cargo-about: CI install-action swap + `about.toml` comment fix + regenerated `THIRD-PARTY-LICENSES.html`.
- **C3** — deny `[sources]` (blocking) + `[advisories]` weekly `audit.yml` + ignore-list triage + `docs/licensing.md`.
- **C4** — SBOM: flake pkg + `just sbom` + CI step.
- **C5** — commit-lint: prek remote hook + `default_install_hook_types` + dependabot prefix fix + dependabot grouping + CI PR-title job.

## Implementation-time verifications

- Exact LLVM major of the pinned rustc (`rustc -vV`) and that `llvmPackages_NN` resolves in the bumped nixpkgs.
- The bumped nixpkgs' cargo-about version carries the `LicenseRef-*` `license_files` fix.
- The install-action prebuilt cargo-about exposes the `cargo about` subcommand.
- The exact RUSTSEC IDs for the `[advisories].ignore` seed (`cargo deny check advisories`).

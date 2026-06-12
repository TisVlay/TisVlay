Title: feat(packaging): publish the workspace to crates.io (default-members, rename, release-plz, mock backend)

## Summary

Enables full crates.io presence for the workspace — both a real distribution channel and namespace defense (none of the `tisvlay*` names were registered, so this claims them and blocks their `-`/`_`/case variants). Along the way it centralizes the "what ships" list, renames the CLI crate to the prime `tisvlay` name, automates publishing with release-plz, and ships the in-memory mock backend in the packages as a field-debugging aid.

Distribution stays as it is today — Nix flake + `.deb`/`.rpm`/`.pkg` via the existing `release.yml` on a `v*` tag — with automated crates.io publishing added alongside, not replacing it.

## What's in it

Focused commits, following Parts A–E of [docs/plans/crates-io-publishing.md](docs/plans/crates-io-publishing.md):

- **A — `default-members`** (`build: centralize the shipping set`): hoists the five shipping binary crates into `workspace.default-members` so bare `cargo build` / `cargo zigbuild` scope to the shipping set from one place, dropping the `-p` lists that the justfile and `package.nix` each duplicated. Tests opt back into the full workspace via `--workspace`.
- **B + C — publish flags + rename** (`build: publish production crates to crates.io; rename tisvlay-cli -> tisvlay`): flips `publish = true` on the nine production crates plus `tisvlay-mock` (10 total); `tisvlay-test` / `tisvlay-harness` stay unpublished. Renames the `tisvlay-cli` crate to `tisvlay` — it already builds the `tisvlay` binary, so only the package id moves — which gives `cargo install tisvlay` and claims the prime name with real content.
- **D — release-plz** (`ci: automate crates.io publishing with release-plz`): on push to `main`, release-plz opens a release PR (shared version bump + per-crate CHANGELOGs) and, on merge, publishes the publishable crates to crates.io in dependency order. Decoupled from the distro release (see below).
- **E — shippable mock backend** (`feat(packaging): ship the mock backend as a field-debug aid`): enables `tisvlayd`'s existing `mock` feature in the distro + Nix builds, so the shipped daemon registers the "Mock (testing)" vendor. A field user — or a bug report — can add a mock TV to confirm the daemon / D-Bus / desktop stack works without real hardware, isolating a tisvlay issue from a TV / network one.

## Decisions and findings worth a look

- **A dev-dependency publish blocker, beyond the known mock optional-dep.** Every production crate that dev-deps on `tisvlay-test` / `tisvlay-harness` couldn't publish: those `workspace.dependencies` entries carried a `version`, so `cargo publish` keeps the dev-dep and demands the (unpublished) crate from crates.io (`tisvlay-core` fails `cargo publish --dry-run` on exactly this). Fix: make `tisvlay-test` / `tisvlay-harness` path-only — a versionless dev-dep is dropped from published manifests.
- **release-plz is decoupled from `release.yml`, not tag-coupled.** release-plz creates one tag *per package* (`{{ package }}-v{{ version }}`); a single shared `v{{ version }}` tag "may create conflicts" on a shared-version workspace. So release-plz owns crates.io with `git_release_enable = false` (it creates no GitHub release), and `release.yml` keeps owning the distro packages + GitHub release on a manually-pushed `v*` tag. The two tag namespaces don't collide.
- **The mock backend was already publish- and ship-ready.** `register_mock_production` exists, the display name is already `"Mock (testing)"`, and the crate's panics are all `#[cfg(test)]` — the production path (`new_production_parts` → connector / discovery, config via serde) has none, and its discovery returns an empty list so it never surfaces fake TVs. Part E was just flipping the feature on in the build recipes plus a README / description label.

## Before this publishes anything

The release automation is inert until these are set up, so the branch can merge safely first:

1. Add a **`CARGO_REGISTRY_TOKEN`** repo secret — a crates.io API token scoped `publish-new` + `publish-update`.
2. Enable the repo setting **"Allow GitHub Actions to create and approve pull requests"** so the release-PR job can open its PR.
3. ⚠️ Once the token is set, the **next push to `main` publishes all 10 crates at `0.1.0`** (release-plz publishes any manifest version not already on the registry). Set the token only when you're ready for that initial publish.

Distro releases are unchanged: push a `v0.x.y` tag and `release.yml` builds the `.deb`/`.rpm`/`.pkg` + draft GitHub release.

## Verification

- `just check` green across the branch — 538 tests, clippy `-D warnings`, fmt.
- `cargo publish --dry-run` packages `tisvlay-core` cleanly; the rest fail only on topological ordering (internal deps not yet on crates.io), which release-plz resolves by publishing in order. `tisvlay-test` / `tisvlay-harness` are correctly rejected as `publish = false`.
- `cargo build --features tisvlayd/mock` compiles the mock into the daemon; `default-members` keeps a bare `cargo build` scoped to the five shipping binaries (zero dev crates compiled).
- `release-plz.toml` and the workflow pass the repo's `prek` TOML/YAML checks.

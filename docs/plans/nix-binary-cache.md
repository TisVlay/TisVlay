# Nix binary cache for releases (Cachix)

NixOS users build `tisvlay` from source today: the flake has no binary cache, so every install and every update compiles the whole workspace locally. This plan adds a public Cachix cache that CI fills on each release tag, plus install docs that let users opt in to the prebuilt package.

## Decisions

- **Cachix free tier, not FlakeHub Cache.** FlakeHub Cache is paid-only, and reading from it requires authentication (`determinate-nixd login`), so only logged-in Determinate Nix users would get hits. Cachix's open-source plan gives 5 GB with anonymous reads and least-recently-used eviction past the limit. Paths already on cache.nixos.org aren't stored again, so the budget goes to our own outputs.
- **Push on release tags only, for x86_64 and aarch64 Linux.** The cache serves users who pin a release (`github:TisVlay/TisVlay/vX.Y.Z`). Users following `main` keep building locally, and PR churn never touches the 5 GB budget.
- **Opt-in package; the module default stays as it is.** A cache only hits when the user's derivation hashes to what CI built, which means the package from *our* `flake.lock`. The module's default (`pkgs.tisvlay or (pkgs.callPackage ./package.nix { })`) builds against the host's nixpkgs and never matches, and it stays that way:
  - The package shares the system's libraries, and the host's nixpkgs security updates reach it.
  - The GUI's femtovg renderer uses OpenGL, so pairing it with a Mesa from a different nixpkgs than the system driver risks breaking GL.

  Users who want prebuilt binaries set `services.tisvlay.package` to the flake's package and accept a second set of runtime libraries in their closure.

## Implementation

### 1. Create the cache (maintainer, by hand)

- [x] A public `tisvlay` cache, owned by the TisVlay organization on Cachix rather than a personal account, so it outlives one maintainer: <https://app.cachix.org/organization/tisvlay/cache/tisvlay>. Cachix-managed signing, so an auth token alone can push. Public key: `tisvlay.cachix.org-1:pa9AnLlywwXfm4cwUzRD1afQh2VbGTbYF6a9iQgzb/g=`.
- [x] A write auth token for that cache, `CACHIX_AUTH_TOKEN`.
- [x] The token lives in a `cachix` environment (Settings → Environments) whose deployment rule only admits `v*` tags, the same restriction `crates-io` has; there's no repository-level copy. A repository secret would be readable by any branch's workflow run; the environment lets only tag runs push, even if the workflow's triggers change.

### 2. CI: push the release build (`release.yml`)

- [x] A `nix-cache` job in phase 1 (tag push), next to `build`, so the cache is warm while the draft release is verified. The maintainer can then test the cached install before promoting the release. A tag moved to a later commit before promotion just pushes the new build alongside the old one; the cache keys on store paths, not tags.
- [x] A runner matrix of `ubuntu-26.04` and `ubuntu-26.04-arm`, covering x86_64-linux and aarch64-linux.
- [x] No cache poisoning: `release.yml` restores no caches, and the job keeps that rule. With `skipAddingSubstituter` it never substitutes from our own cache, so it only writes to it, and only tag runs in the `cachix` environment hold the token.
- [x] **Closure-only push.** cachix-action's automatic push would upload every path the job built, including about 580 MiB per architecture of vendored crate sources that cache.nixos.org lacks; its `pushFilter` is a deny-list regex that can't name them. Instead the action only installs the CLI (`skipPush`), and a step pushes the output's runtime closure with `cachix push` (measurements in [docs/ideas/nix-cache-main-builds.md](../ideas/nix-cache-main-builds.md)).
- [x] **Release pins.** Each build is pinned as `<tag>-<system>` with `--keep-revisions 1`: eviction can't drop a release, and a tag re-pushed to a new commit moves its pin to the new build, leaving the old one to normal eviction. Since immutable releases and the `v*` tag ruleset (docs/plans/release-supply-chain.md), only an admin can move a tag, and only before its release is published.
- **Action versions** follow the workflows' major-tag convention (`cachix/install-nix-action@v31`, `cachix/cachix-action@v17`); the pending zizmor hardening covers SHA pinning for all workflows at once.

### 3. Docs

- [x] **Book, `getting-started/installation.md`, NixOS section:** a "Prebuilt binaries (optional)" block covering:
  - pinning the input to a release tag;
  - adding the substituter and key through `nix.settings.substituters` / `trusted-public-keys`, not the flake's `nixConfig`, which `nixos-rebuild` of a consuming flake doesn't apply;
  - setting `services.tisvlay.package = tisvlay.packages.${pkgs.stdenv.hostPlatform.system}.default`;
  - the trade-offs above (duplicate libraries, security fixes arriving with our releases, the GL caveat);
  - not setting `inputs.tisvlay.inputs.nixpkgs.follows`, which also changes the hash.
- [x] **`module.nix`, `package` option description:** mentions the cached alternative.
- [x] **`packaging/nix/README.md`:** a "Binary cache" section saying what's pushed, when, and from where. The stale "no binary cache" comments in `package.nix` and `flake.nix` are fixed (the `doCheck = false` rationale still holds for users who build locally).

### 4. Verify on the first release after merge

- [ ] On the draft release, check the `nix-cache` job's upload list and size against the 5 GB budget, and that the dashboard shows both `<tag>-<system>` pins.
- [ ] On a NixOS host with the documented config, run `nixos-rebuild build` and confirm `tisvlay` is fetched from tisvlay.cachix.org, not built. Check with `nix path-info --store https://tisvlay.cachix.org <path>`.

## Open questions

- **aarch64 build.** Nobody has built the flake on aarch64-linux yet; the first tag run is the test. If it fails, drop the ARM matrix entry rather than block the release.

## Alternatives considered

- **FlakeHub Cache:** paid, with authenticated reads; see Decisions. Publishing to FlakeHub (free for public flakes) would add versioned flake URLs (`flakehub.com/f/TisVlay/TisVlay/0.3`) but no binaries. That could be a separate follow-up.
- **Magic Nix Cache:** discontinued on 2025-02-01.
- **Self-hosted (Attic, S3):** a server to run and pay for, for a project this size.
- **Defaulting the module to our package:** cache hits for everyone, at the cost of duplicate runtime libraries, security fixes tied to our release cadence, and the GL mismatch risk for the GUI. Rejected in favor of opt-in.

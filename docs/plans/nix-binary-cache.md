# Nix binary cache for releases (Cachix)

NixOS users build `tisvlay` from source today: the flake has no binary cache, so every install and every update compiles the whole workspace locally. This plan adds a public Cachix cache that CI fills on each release tag, plus install docs that let users opt in to the prebuilt package.

## Decisions

- **Cachix free tier, not FlakeHub Cache.** FlakeHub Cache is paid-only, and reading from it requires authentication (`determinate-nixd login`), so only logged-in Determinate Nix users would get hits. Cachix's open-source plan gives 5 GB with anonymous reads and least-recently-used eviction past the limit. Paths already on cache.nixos.org aren't stored again, so the budget goes to our own outputs.
- **Push on release tags only.** The cache serves users who pin a release (`github:TisVlay/TisVlay/vX.Y.Z`). Users following `main` keep building locally, and PR churn never touches the 5 GB budget.
- **Opt-in package; the module default stays as it is.** A cache only hits when the user's derivation hashes to what CI built, which means the package from *our* `flake.lock`. The module's default (`pkgs.tisvlay or (pkgs.callPackage ./package.nix { })`) builds against the host's nixpkgs and never matches, and it stays that way:
  - The package shares the system's libraries, and the host's nixpkgs security updates reach it.
  - The GUI's femtovg renderer uses OpenGL, so pairing it with a Mesa from a different nixpkgs than the system driver risks breaking GL.

  Users who want prebuilt binaries set `services.tisvlay.package` to the flake's package and accept a second set of runtime libraries in their closure.

## Implementation

### 1. Create the cache (maintainer, by hand)

- [ ] Create a public `tisvlay` cache on cachix.org, owned by a TisVlay organization rather than a personal account, so the cache outlives one maintainer. Keep Cachix-managed signing (an auth token alone can push).
- [ ] Generate a write-only auth token scoped to that cache.
- [ ] Add a `cachix` environment in repo Settings → Environments that only admits `v*` tags, the same restriction `crates-io` has, and store the token there as `CACHIX_AUTH_TOKEN`. A run from a branch can't push even if the workflow's triggers change.
- [ ] Record the cache's public key; the docs below need it.

### 2. CI: push the release build (`release.yml`)

A new job in phase 1 (tag push), next to `build`, so the cache is warm while the draft release is being verified. The maintainer can then test the cached install before promoting the release.

```yaml
nix-cache:
  if: github.event_name == 'push'
  runs-on: ubuntu-latest
  environment: cachix
  permissions:
    contents: read
  steps:
    - uses: actions/checkout@v7
    - uses: cachix/install-nix-action@<pinned sha>
    - uses: cachix/cachix-action@<pinned sha>
      with:
        name: tisvlay
        authToken: ${{ secrets.CACHIX_AUTH_TOKEN }}
        # Build from cache.nixos.org only; see the cache-poisoning note.
        skipAddingSubstituter: true
    - run: nix build .#default
```

- **Cache poisoning.** `release.yml` restores no caches, and the job keeps that rule: `skipAddingSubstituter` means it never substitutes from our own cache, so it only writes to it. Only tag runs in the `cachix` environment hold a push token.
- **What gets pushed.** cachix-action pushes the paths the job built. That's the `tisvlay` output plus any build-only paths not on cache.nixos.org (e.g. the vendored crate sources). Check the first release's upload list, and add a `pushFilter` if build-only paths turn out large.
- **Pinning.** Pin both actions by SHA, like the rest of the workflows, and add them to the zizmor/dependabot coverage.

### 3. Docs

- [ ] **Book, `getting-started/installation.md`, NixOS section:** a "Prebuilt binaries (optional)" block covering:
  - pinning the input to a release tag;
  - adding the substituter and key through `nix.settings.substituters` / `trusted-public-keys`, not the flake's `nixConfig`, which `nixos-rebuild` of a consuming flake doesn't apply;
  - setting `services.tisvlay.package = tisvlay.packages.${pkgs.stdenv.hostPlatform.system}.default`;
  - the trade-offs above (duplicate libraries, security fixes arriving with our releases, the GL caveat);
  - not setting `inputs.tisvlay.inputs.nixpkgs.follows`, which also changes the hash.
- [ ] **`module.nix`, `package` option description:** mention the cached alternative in one line.
- [ ] **`packaging/nix/README.md`:** a "Binary cache" section saying what's pushed, when, and from where. Also fix the stale "no binary cache" comments in `package.nix` and `flake.nix` (the `doCheck = false` rationale still holds for users who build locally).

### 4. Verify on the first release after merge

- [ ] On the draft release, check the `nix-cache` job's upload list and size against the 5 GB budget.
- [ ] On a NixOS host with the documented config, run `nixos-rebuild build` and confirm `tisvlay` is fetched from tisvlay.cachix.org, not built. Check with `nix path-info --store https://tisvlay.cachix.org <path>`.

## Open questions

- **aarch64-linux.** GitHub's `ubuntu-24.04-arm` runners are free for public repos, so a second matrix entry would cover ARM users at the cost of more cache space. In or out for the first cut?
- **Cache name and owner.** Is `tisvlay` free on Cachix, and does a Cachix organization exist for TisVlay yet?
- **Promote-time gate.** Pushing at tag time puts unverified builds in the cache before the release is promoted. That's harmless (a bad tag is bad with or without the cache), but the job could move to phase 2 if we'd rather only cache promoted releases.

## Alternatives considered

- **FlakeHub Cache:** paid, with authenticated reads; see Decisions. Publishing to FlakeHub (free for public flakes) would add versioned flake URLs (`flakehub.com/f/TisVlay/TisVlay/0.3`) but no binaries. That could be a separate follow-up.
- **Magic Nix Cache:** discontinued on 2025-02-01.
- **Self-hosted (Attic, S3):** a server to run and pay for, for a project this size.
- **Defaulting the module to our package:** cache hits for everyone, at the cost of duplicate runtime libraries, security fixes tied to our release cadence, and the GL mismatch risk for the GUI. Rejected in favor of opt-in.

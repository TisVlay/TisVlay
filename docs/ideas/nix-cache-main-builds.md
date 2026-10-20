# Cache `main` builds in tisvlay.cachix.org

**Trigger:** NixOS users following `github:TisVlay/TisVlay` (`main`) rather than a release tag ask for prebuilt binaries, or report long rebuilds on every update.

Today only `v*` tag builds go to the cache (packaging/nix/README.md § "Binary cache"). A user following `main` always compiles locally, even with the cache configured, because no `main` commit has a cached build.

## What it takes

1. **Environment:** add a `main` branch rule to the `cachix` environment beside its `v*` tag rule. Anyone who can land a commit on `main` can then push to the cache, but landing on `main` already goes through PR review, so the trust level is close to tagging.
2. **Workflow:** move the push out of `release.yml`, which is the tag and promotion pipeline, into its own `nix-cache.yml`, triggered by pushes to `main` and `v*` tags. Keep the same environment, runner matrix and write-only setup (`skipAddingSubstituter`).
3. **Docs:** the install guide's "Prebuilt binaries" section stops requiring a pinned release tag, because a `main` input locks to a merge commit that CI has built.
4. **Budget:** this is the real cost. The free plan holds 5 GB and evicts the least recently accessed paths past it, or paths never accessed, in upload order. Without pins (below), heavy `main` churn can evict a release build nobody has fetched lately, and users pinned to it go back to compiling.

## Sizes (measured 2026-10-19, x86_64)

| Path | Size | On cache.nixos.org? |
|---|---|---|
| `tisvlay` output | 170 MiB unpacked, ≈ 49 MiB as zstd | no, ours |
| 834 vendored crate sources (`<crate>-<version>`) | 829 MiB unpacked | ~30 % (6 of 20 sampled) |
| repo `source` | 5.7 MiB | no |

The crate sources are build-only: they're not in the output's runtime closure. Each architecture builds its own copies, with different store paths. A push that includes them uploads several hundred MiB on the first run and every new crate version after that, all of it useless to users.

## Mitigations

### Push the output's closure, not everything built

cachix-action's default (daemon mode) pushes every path the job builds through a post-build hook, crate sources included. Its `pushFilter` input is a deny-list: the hook runs `grep -vE` with it over each full `/nix/store/<hash>-<name>` path. The README's example, `(-source$|nixpkgs\.tar\.gz$)`, doesn't match crate sources named `<crate>-<version>`, and a regex over 834 crate names isn't maintainable.

An allow-list fits better. Set the action's `skipPush: true`, then push only the output and its runtime closure:

```yaml
- name: Push
  run: cachix push tisvlay "$(nix build --print-out-paths .#default)"
```

`cachix push` uploads the closure and skips anything cache.nixos.org already serves. That leaves the `tisvlay` output plus the few runtime paths nixpkgs doesn't build. (`pathsToPush` can't do this: it's read when the action step starts, before the build has produced the path.)

The release job already pushes this way, so `nix-cache.yml` inherits it as is.

### Pin builds by name

Cachix pins (cachix ≥ 1.5) name a store path, and "pinned paths are immune from garbage collection by default". Each pin under the same name adds a revision, kept indefinitely unless `--keep-revisions N` or `--keep-days N` caps the history.

The release job already pins each build as `<tag>-<system>` with `--keep-revisions 1`. A tag re-pushed to a new commit moves its pin, and the abandoned build falls to normal eviction. `main` builds would get a rolling pin of their own, so the newest few survive churn and older ones fall to LRU:

```sh
cachix pin tisvlay "main-${SYSTEM}" "$OUT" --keep-revisions 3
```

**To verify:** whether a pin protects the path's whole closure or only the top path. Cachix never serves a narinfo without its full closure, so an evicted dependency would make the pinned output unusable too. For us the closure beyond the output is mostly cache.nixos.org paths, which aren't stored in our cache anyway, but confirm it on the dashboard's GC preview after the first release.

## Sources

- [Cachix pins](https://docs.cachix.org/pins) and [introducing pins](https://blog.cachix.org/posts/2023-05-23-introducing-pins-permanent-nix-binary-storage/)
- [Cachix garbage collection](https://docs.cachix.org/garbage-collection)
- [Cachix pricing](https://www.cachix.org/pricing): 5 GB free for open source
- [cachix-action](https://github.com/cachix/cachix-action): `pushFilter`, `skipPush`, `pathsToPush`; the post-build hook's `grep -vE` filter is in `src/main.ts`

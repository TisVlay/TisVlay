# Licensing

Developer-facing reference for the third-party-license attribution and policy-gate pipeline. Audience: someone bumping a dep, adding a new crate, or auditing the as-shipped license posture. For the distro-maintainer view (where files land in the package, build pipeline, glibc baseline) see [packaging/README.md](../packaging/README.md).

## Pipeline

Two complementary tools, each with one job:

- **[`cargo-about`](https://github.com/EmbarkStudios/cargo-about)** generates the bundled-deps attribution artifact (`THIRD-PARTY-LICENSES.html`) used both at runtime in the GUI and in the distro packages.
- **[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)** runs four checks across two contexts. Blocking, wired into `prek` via `just deny`: `licenses` enforces the SPDX allow-list (a forbidden license — GPL, LGPL, MPL not on the exception list — fails the build); `bans` fails on multi-version dep duplication (`multiple-versions = "deny"`) except for the reasoned skip-list of upstream-forced duplicates; `sources` pins code provenance to crates.io, so it is a tripwire against any unvetted source sneaking in. Kept out of `just deny`, run by [`.github/workflows/audit.yml`](../.github/workflows/audit.yml) weekly and on PRs that change `Cargo.lock` or `deny.toml`: `advisories` gates RustSec and yanked crates. A transitive vulnerability with no upstream fix therefore can't block unrelated PRs, while a dependency update that pulls one in is flagged on its own PR and a newly-disclosed advisory still surfaces on the schedule. The `[advisories]` ignore-list in [`deny.toml`](../packaging/licensing/deny.toml) records the already-triaged advisories (each with a reason) so the audit run stays green until a new one lands. The `bans` skip-list and `[graph] targets` filter document already-evaluated multi-version cases — see [`docs/build.md`](build.md) "Dep-graph posture" for the routine and rationale.

Both tools live in the Nix dev shell ([`flake.nix`](../flake.nix) `nativeBuildInputs`). The Nix package derivation also pulls them in for sandbox-time generation.

## Layout

Everything related to license policy + attribution lives under [`packaging/licensing/`](../packaging/licensing/) at the repo root:

| File | Role | Tracked? |
|---|---|---|
| `about.toml` | cargo-about config: SPDX accept-list, OR-priority, per-crate clarifications, target-triple filter, build-env knobs | ✅ |
| `about.html.hbs` | Handlebars template for the generated HTML — overview list + per-license sections, prefers-color-scheme styling | ✅ |
| `deny.toml` | cargo-deny config: SPDX allow-list (mirrors `about.toml`'s `accepted`) + per-crate `exceptions` | ✅ |
| `NOTICE` | Hand-written project copyright + Apache-2.0 election (~4 lines, bat-style) | ✅ |
| `THIRD-PARTY-LICENSES.html` | Generated workspace-wide bundle | ⛔ gitignored |

The project's own license is at [`LICENSE`](../LICENSE) (Apache-2.0, root). Cargo workspace metadata (`[workspace.package].license = "Apache-2.0"`) makes that the resolved expression for every workspace crate.

## License elections

- **Project** ships under **Apache-2.0**. `NOTICE` declares the election.
- **Slint** is triple-licensed `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`. We elect the **Royalty-free** option, which triggers the `AboutSlint` widget requirement (satisfied by [tisvlay-gui's About pane](../crates/tisvlay-gui/ui/components/about_pane.slint)). GPL-3.0 would force the whole binary under GPL-3.0, conflicting with our Apache-2.0; the paid Software 3.0 license drops the AboutSlint requirement but isn't free-for-OSS.

## Per-crate exception catalog

`about.toml` and `deny.toml` carry a small set of per-crate overrides — kept off the global allow-list so any future dep with the same license forces a deliberate decision. Each is documented inline in both files; this table is the at-a-glance summary:

| Crate | License | Why allowed |
|---|---|---|
| `slint`, `slint-build`, `slint-macros`, `i-slint-{backend-selector, backend-winit, common, compiler, core, core-macros, renderer-femtovg, renderer-skia}` | `LicenseRef-Slint-Royalty-free-2.0` | We elected the Royalty-free option (see above). `LicenseRef-*` ids aren't standard SPDX so cargo-deny needs explicit per-crate entries. |
| `option-ext` | `MPL-2.0` | Pulled in transitively via `directories` (XDG path resolution) and `dirs` (via `freedesktop-icons`). MPL-2.0 is **file-level weak copyleft** — modifications to MPL-licensed files themselves must be disclosed; using the crate unmodified against an Apache-2.0 distribution is well-established. |
| `generational-arena` | `MPL-2.0` | Same MPL rationale as `option-ext`. Pulled in via Slint renderers under `--all-features`. |
| `libfuzzer-sys` | `NCSA` | Reached via `image → ravif → rav1e`'s fuzz feature. Not in shipped binaries (rav1e gates libfuzzer-sys behind a non-default feature), but cargo-about's `--all-features` resolution surfaces it. NCSA is OSI-approved + FSF-free. |
| `linux-cec`, `linux-cec-macros` | `LGPL-2.1-or-later` | Valve's HDMI-CEC library + its proc-macro helper, the foundation of the `tisvlay-cec` backend. Built from our fork ([TisVlay/linux-cec](https://github.com/TisVlay/linux-cec), branch `rc-passthru`: 0.2.1 plus a log-address flags setter and a fix for reading a claim in progress), whose modified files carry LGPL-2.1 §2's change notices; the source is public, which keeps the relink obligation below met. LGPL-2.1 is **weak (library) copyleft**: linking — including static linking — is permitted, with the §6 obligation that a recipient can relink against a modified `linux-cec`. tisvlay being open source (Apache-2.0, full source on GitHub) satisfies that obligation directly. The sibling `linux-cec-sys` (the `/dev/cec` ioctl bindings) is BSD-3 and stays on the global allow-list. Revisit if tisvlay ever ships a **closed-source** binary — at that point either provide relinkable object files or drop to the BSD-3 `linux-cec-sys` layer behind a hand-written safe wrapper. |

When a dep bump introduces a new license, **update both `about.toml`'s per-crate `accepted` and `deny.toml`'s `exceptions`** — they're separate tools with separate vocabularies for the same policy. Keeping them in sync is part of the bump review.

## `about.toml` config rationale

Worth knowing what each global knob does (full list in the file's header comment):

- **`accepted`** is **priority-ordered**. cargo-about picks the **first** match for `OR` expressions, so MIT-first minimizes Apache-2.0 NOTICE-propagation work.
- **`targets = ["x86_64-unknown-linux-gnu"]`** prunes Windows/macOS-only crates (`core-foundation`, `windows-sys`, …) from the bundle — Linux-only project.
- **`workarounds = ["rustls", "ring", "wasmtime", "rustix"]`** opts into cargo-about's built-in clarifications for crates with unusual license packaging. Review on every dep-graph shift; the catalogue lives at [cargo-about/docs/src/cli/generate/workarounds.md](https://github.com/EmbarkStudios/cargo-about/blob/main/docs/src/cli/generate/workarounds.md) (upstream).
- **`private = { ignore = true }`** skips license checks on our own workspace crates (they're `publish = false`).
- **`ignore-dev-dependencies = true`** keeps test-only crates out of the bundle. `ignore-build-dependencies = false` because build-deps' generated output ships in our binaries.

## Build-env behavior

Three build paths produce the attribution artifact, each with slightly different mechanics:

| Build env | How `THIRD-PARTY-LICENSES.html` lands | Where the GUI finds it at runtime |
|---|---|---|
| **Source / dev** (`cargo build` / `just gui-start`) | `crates/tisvlay-gui-app/build.rs` shells out to `cargo-about` and writes to `$OUT_DIR`. If `cargo-about` isn't on `$PATH`, writes a stub blob with install instructions + emits a `cargo:warning=` (no build failure). | Embedded via `include_str!`; written to a per-process tempfile on click, opened with `xdg-open`. |
| **nfpm packages** (`just package-{deb,rpm,archlinux}`) | `just licenses-html` regenerates at `packaging/licensing/THIRD-PARTY-LICENSES.html`; `package-*` recipes depend on it. nfpm contents map it to `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`. | FHS path matched by [`installed_notices_path()`](../crates/tisvlay-gui-app/src/main.rs)'s first probe. |
| **Nix package** (`nix build`) | [`packaging/nix/package.nix`](../packaging/nix/package.nix) `preBuild` runs `cargo about generate` with [`packaging/nix/cargo-about.nix`](../packaging/nix/cargo-about.nix)'s cargo-about (at least 0.9.1, which the Slint clarifications need; the dev shell uses the same one). It needs no network, since cargo-about no longer queries clearlydefined.io. `postInstall` installs to `$out/share/doc/tisvlay/`. Mirrors the upstream `tracexec` precedent (`pkgs/by-name/tr/tracexec/package.nix`). | `installed_notices_path()` falls through to the `current_exe()`-relative probe (`$prefix/share/doc/tisvlay/...`). |

The browser-handoff path itself (why we don't render the bundle in-app) is GUI-architecture knowledge — see [`.claude/slint.md`](../.claude/slint.md) "Slint text-rendering for large documents".

## Operational tasks

```bash
# Regenerate the packaged HTML (gitignored output at packaging/licensing/THIRD-PARTY-LICENSES.html)
just licenses-html

# Run the policy gate
just deny

# Run the policy gate as part of the pre-commit pipeline
prek run -a deny
```

When `just deny` fails on a new transitive dep:

1. Read the SPDX expression cargo-deny rejected.
2. Decide: is this license *globally* fine for the project (add to `allow` in both files), or only OK for this one crate (add a per-crate exception)?
3. Update `packaging/licensing/about.toml` and `packaging/licensing/deny.toml` symmetrically. Keep the inline rationale comment honest — future bump-reviewers read those.
4. Re-run `just deny` to confirm; commit.

Both invocations pass `--locked --fail`: `--locked` keeps the bundle reproducible (fails on `Cargo.lock` drift), `--fail` exits non-zero if any crate's license can't be resolved (catches new weird-packaging deps at PR time).

## SBOMs

Releases carry one CycloneDX 1.5 JSON SBOM per shipped binary (`tisvlay`, `tisvlay-gui`) per architecture, as assets and as attestations ([packaging/README.md](../packaging/README.md) § Release signing and verification). `just sbom` is the one recipe that writes them.

- **cargo-cyclonedx, limited to the binary.** Each SBOM covers that binary, its target and its features, with licenses and checksums: a probe listed 456 crates for `tisvlay` and 608 for `tisvlay-gui`, every one with a license.
- **Checked against cargo-auditable.** cargo-cyclonedx works the dependency set out from cargo metadata, not from the build, so the packages are built with cargo-auditable, which embeds the set actually compiled, and `just sbom-check` fails if the two disagree. The embedded data also lets Trivy, grype, syft and `cargo audit bin` scan an installed binary.
- **Assets and attestations both.** An asset serves anyone who wants the file (Dependency-Track, `grype sbom:…`); an attestation binds each SBOM to its binary and to its architecture's packages, which an asset alone can't do. Not inside the packages: [docs/ideas/in-package-sbom.md](ideas/in-package-sbom.md).
- **Known gaps:** [docs/ideas/sbom-precision.md](ideas/sbom-precision.md) (feature unification across binaries, and Slint's elected license).

### Tooling survey (2026-10-23)

Downloads are crates.io's last-90-day counts on that date; the first two rows were measured on this repo, the rest come from each tool's docs.

| Tool | Limited to what ships? | Licenses | Checksums | Last release | Downloads (90 d) |
|---|---|---|---|---|---|
| cargo-sbom (used before) | No: the whole workspace, or one package | Yes | None | 0.10.0, 2025-06-17 | 69k |
| **cargo-cyclonedx** (chosen) | Yes: one binary, target and feature set | Yes | Yes, except workspace crates | 0.5.9, 2026-03-19 | 867k |
| cargo-auditable, read by syft or Trivy | Exact: read from the compiled binary | No | No | 0.7.7, 2026-10-02 | 429k |
| syft or Trivy on `Cargo.lock` | No: every target | No | not checked | 2026-10 | Go tools |
| Cargo's `-Z sbom` | Exact: written by the build | No | not checked | nightly only | n/a |

- cargo-sbom also rewrote Slint's `GPL-3.0-only` as the deprecated SPDX id `GPL-3.0`, and has no target, feature or binary options.
- cargo-auditable's embedded data is the distro convention for Rust (Alpine, NixOS, openSUSE, Void, Chimera, Wolfi, some Ubuntu 26.04 packages), and every major scanner reads it, but it records no licenses or checksums, and syft can't add licenses for Rust.
- Cargo's own SBOM support ([RFC 3553](https://github.com/rust-lang/rfcs/pull/3553)) is the long-term answer, still unstable when surveyed; its precursor files carry no licenses either, so licenses would still be joined in from cargo metadata. Switching means changing only `just sbom`.
- dist (cargo-dist) bundles cargo-auditable, cargo-cyclonedx and GitHub attestations, but replacing the nfpm and zigbuild pipeline to get them wasn't worth it.

## References

- [cargo-about CLI docs](https://embarkstudios.github.io/cargo-about/)
- [cargo-deny CLI docs](https://embarkstudios.github.io/cargo-deny/)
- [Apache 2.0 §4(d) NOTICE propagation](https://www.apache.org/licenses/LICENSE-2.0)
- [Slint Royalty-free 2.0 license](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md)
- nixpkgs precedent for Rust + cargo-about: `pkgs/by-name/tr/tracexec/package.nix`

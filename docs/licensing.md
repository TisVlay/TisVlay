# Licensing

Developer-facing reference for the third-party-license attribution and policy-gate pipeline. Audience: someone bumping a dep, adding a new crate, or auditing the as-shipped license posture. For the distro-maintainer view (where files land in the package, build pipeline, glibc baseline) see [packaging/README.md](../packaging/README.md).

## Pipeline

Two complementary tools, each with one job:

- **[`cargo-about`](https://github.com/EmbarkStudios/cargo-about)** generates the bundled-deps attribution artifact (`THIRD-PARTY-LICENSES.html`) used both at runtime in the GUI and in the distro packages.
- **[`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny)** runs four checks across two contexts. Blocking, wired into `prek` via `just deny`: `licenses` enforces the SPDX allow-list (a forbidden license — GPL, LGPL, MPL not on the exception list — fails the build); `bans` surfaces multi-version dep duplication as warnings (`multiple-versions = "warn"`, never fails); `sources` pins code provenance to crates.io (the workspace has no git or alternate-registry deps, so it is a tripwire against one sneaking in). Non-blocking, run weekly by [`.github/workflows/audit.yml`](../.github/workflows/audit.yml): `advisories` gates RustSec and yanked crates — deliberately kept out of `just deny` so a transitive vulnerability with no upstream fix cannot block every PR/commit, while a newly-disclosed advisory still surfaces on the schedule. The `[advisories]` ignore-list in [`deny.toml`](../packaging/licensing/deny.toml) records the already-triaged advisories (each with a reason) so the weekly run stays green until a new one lands. The `bans` skip-list and `[graph] targets` filter document already-evaluated multi-version cases — see [`docs/build.md`](build.md) "Dep-graph posture" for the routine and rationale.

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
| `linux-cec`, `linux-cec-macros` | `LGPL-2.1-or-later` | Valve's HDMI-CEC library + its proc-macro helper, the foundation of the `tisvlay-cec` backend. LGPL-2.1 is **weak (library) copyleft**: linking — including static linking — is permitted, with the §6 obligation that a recipient can relink against a modified `linux-cec`. tisvlay being open source (Apache-2.0, full source on GitHub) satisfies that obligation directly. The sibling `linux-cec-sys` (the `/dev/cec` ioctl bindings) is BSD-3 and stays on the global allow-list. Revisit if tisvlay ever ships a **closed-source** binary — at that point either provide relinkable object files or drop to the BSD-3 `linux-cec-sys` layer behind a hand-written safe wrapper. |

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
| **Source / dev** (`cargo build` / `just gui-start`) | `crates/tisvlay-gui/build.rs` shells out to `cargo-about` and writes to `$OUT_DIR`. If `cargo-about` isn't on `$PATH`, writes a stub blob with install instructions + emits a `cargo:warning=` (no build failure). | Embedded via `include_str!`; written to a per-process tempfile on click, opened with `xdg-open`. |
| **nfpm packages** (`just package-{deb,rpm,archlinux}`) | `just licenses-html` regenerates at `packaging/licensing/THIRD-PARTY-LICENSES.html`; `package-*` recipes depend on it. nfpm contents map it to `/usr/share/doc/tisvlay/THIRD-PARTY-LICENSES.html`. | FHS path matched by [`installed_notices_path()`](../crates/tisvlay-gui/src/main.rs)'s first probe. |
| **Nix package** (`nix build`) | [`packaging/nix/package.nix`](../packaging/nix/package.nix) `preBuild` runs `cargo about generate` (after `sed`-prepending `no-clearly-defined = true` to the build copy of `about.toml` — Nix sandbox has no network). `postInstall` installs to `$out/share/doc/tisvlay/`. Mirrors the upstream `tracexec` precedent (`pkgs/by-name/tr/tracexec/package.nix`). | `installed_notices_path()` falls through to the `current_exe()`-relative probe (`$prefix/share/doc/tisvlay/...`). |

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

## References

- [cargo-about CLI docs](https://embarkstudios.github.io/cargo-about/)
- [cargo-deny CLI docs](https://embarkstudios.github.io/cargo-deny/)
- [Apache 2.0 §4(d) NOTICE propagation](https://www.apache.org/licenses/LICENSE-2.0)
- [Slint Royalty-free 2.0 license](https://github.com/slint-ui/slint/blob/master/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md)
- nixpkgs precedent for Rust + cargo-about: `pkgs/by-name/tr/tracexec/package.nix`

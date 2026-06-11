# Plan: production-crate tracking + crates.io publishing & namespace defense

## Context

The project ships via Nix flake (`github:ruffsl/tisvlay`) and distro packages
(`.deb`/`.rpm`/`.pkg` built by nfpm). Two needs have emerged:

1. **The "shipping set" is duplicated.** The five shipping binary crates
   (`tisvlayd`, `tisvlay-cli`, `tisvlay-session`, `tisvlay-tray`, `tisvlay-gui`)
   are now hardcoded as `-p` flags in **both** the justfile *and*
   `packaging/nix/package.nix` — commit `07ede89` added the second copy. There's
   no single source of truth.
2. **crates.io presence + namespace defense.** The maintainer wants to publish
   the production crates to crates.io as a real channel, and to stop a malicious
   actor from registering `tisvlay` / `tisvlay-*` and impersonating the project.
   **No `tisvlay*` name is registered today** — the whole namespace is open.

A literal `tisvlay` "meta crate" was the initial idea. It's the **wrong tool for
both goals**: depending on binary crates does *not* build their binaries (so it
can't replace the `-p` lists), and it reserves only the one name `tisvlay` (a
weak namespace defense). The correct tools are `workspace.default-members`
(tracking) and **publishing the real crates** (namespace — also blocks
`-`/`_`/case variants of each name).

Decisions made with the user: pursue **both** goals; **full crates.io presence**;
release automation via **release-plz**; the `tisvlay` name claimed by **renaming
`tisvlay-cli` → `tisvlay`** (recommended — facade-lib is the documented fallback).

---

## Part A — Centralize the shipping set (`default-members`)

Single source of truth for "what ships", de-duplicating the two `-p` lists.

- **`Cargo.toml` (root)** — add to `[workspace]`:
  ```toml
  default-members = [
      "crates/tisvlayd",
      "crates/tisvlay-cli",      # → "crates/tisvlay" after Part C
      "crates/tisvlay-session",
      "crates/tisvlay-tray",
      "crates/tisvlay-gui",
  ]
  ```
  Now bare `cargo build` builds exactly the shipping binaries + dep closure.
- **`packaging/nix/package.nix`** — remove the `cargoBuildFlags` block added in
  `07ede89` (bare `cargo build` now uses `default-members`). **Add `--workspace`
  to `cargoTestFlags`** (currently `["--all-features"]`): with `default-members`
  set, `cargo test --all-features` would otherwise scope to the 5 binaries and
  silently skip the lib/dev-crate tests. New value:
  `cargoTestFlags = [ "--workspace" "--all-features" ]`.
- **`justfile`** — drop the `-p` flags from the `package-build` recipe (rely on
  `default-members`). Confirm `check`/`build`/`test` recipes already pass
  `--workspace` (they do) so they're unaffected.
- **Document the behavior change** in a comment on `default-members`: bare
  `cargo build`/`cargo test` now scope to the shipping set; use `just check` or
  `--workspace` for everything. (`just` is the canonical dev path, so low impact.)
- **Bare-cargo footgun (review finding).** After this, bare `cargo build`/`test`/
  `check` (no `--workspace`) cover only the 5 shipping crates, skipping the libs +
  dev crates. prek hooks and CI are safe — they all go through `just`, which passes
  `--workspace`. rust-analyzer's check-on-save defaults to `--workspace` (so the IDE
  is unaffected — verify once). Residual risk is a dev manually running bare
  `cargo test`; mitigate with a one-line CONTRIBUTING/README note and the inline
  `default-members` comment above.

Independent of publishing — ship this first as a small PR.

## Part B — crates.io readiness (publish flags, the mock blocker, metadata)

**Publish set = 10 crates**: the 9 production crates + `tisvlay-mock`.
`tisvlay-test` / `tisvlay-harness` stay `publish = false` (dev-only; they appear
only as `[dev-dependencies]`).

- **The mock blocker (must fix).** `crates/tisvlay-backends/Cargo.toml` has
  `tisvlay-mock = { workspace = true, optional = true }` behind its `mock`
  feature. `cargo publish` requires *optional* deps to be on crates.io (only
  `dev-dependencies` are exempt) — so publishing `tisvlay-backends` requires
  `tisvlay-mock` published too. **Resolution: publish `tisvlay-mock`** (legit
  in-memory backend; also reserves the name).
- **Dev-dep blocker (found during implementation).** Beyond the mock optional-dep,
  every production crate that dev-deps on `tisvlay-test` / `tisvlay-harness` is
  *also* blocked: those `workspace.dependencies` entries carry a `version`, so
  `cargo publish` keeps the dev-dep and demands the (unpublished) crate from
  crates.io — `tisvlay-core` fails the dry-run on exactly this. **Fix: make
  `tisvlay-test` / `tisvlay-harness` path-only** (drop `version`) in
  `workspace.dependencies`; a versionless dev-dep is dropped from published
  manifests. (`tisvlay-mock` keeps its version — it's a real published optional
  dep of `tisvlay-backends`.)
- **Flip publish flags** — change `publish.workspace = true` → `publish = true`
  on each of: `tisvlay-core`, `tisvlay-lg`, `tisvlay-backends`, `tisvlay-cli`
  (→`tisvlay`), `tisvlayd`, `tisvlay-session`, `tisvlay-tray`, `tisvlay-gui`,
  `tisvlay-mock`. (`tisvlay-ssdp` already `publish = true`.) Keep the workspace
  default `publish = false` (explicit opt-in is the safer supply-chain posture —
  a new crate isn't auto-publishable).
- **Metadata** — all 9 production crates already have the required `description`.
  **Verify `tisvlay-mock` has a `description`** (newly in the publish set); add
  one if missing. READMEs are *optional* on crates.io — treat per-crate
  `README.md` + `readme = "README.md"` as nice-to-have polish, not a blocker
  (model: `crates/tisvlay-ssdp`).
- **`build.rs` is publish-safe** — `tisvlay-gui` (`cargo-about`, `git`, `rustc`)
  and `tisvlay-tray` (`msgfmt`) all soft-fail to stubs, so `cargo publish`'s
  verify-build succeeds without those tools. (Optional: `[package.metadata.docs.rs]`
  for `tisvlay-gui` so docs.rs has its system libs — cosmetic.)

- **Two separate lists (review finding).** `default-members` (5 shipping binaries)
  and the `publish = true` set (10) are now two hand-maintained lists with no code
  tie — a new crate must be added to whichever it belongs to. Tie them with a short
  comment block at the top of root `Cargo.toml` documenting both sets.
- **Lighter alternative (noted, not chosen).** You chose full presence. If the
  per-release burden of 10 crates ever bites, the *namespace* goal alone is met by
  publishing just the libraries (`core`, `ssdp`, `lg`, `backends`, `mock`) and
  documenting the binaries' distribution channels in `SECURITY.md`.

**Topological publish order** (release-plz computes this, listed for reference):
`tisvlay-core`, `tisvlay-ssdp` → `tisvlay-lg`, `tisvlay-mock` → `tisvlay-backends`
→ `tisvlay`/`tisvlayd`/`tisvlay-session`/`tisvlay-tray`/`tisvlay-gui`.

## Part C — Claim the `tisvlay` name (rename `tisvlay-cli` → `tisvlay`)

The binary is **already** `tisvlay` (`[[bin]] name = "tisvlay"`), so user-facing
naming and the nfpm install paths (`target/.../release/tisvlay`) are unchanged —
only the crate/package id changes.

- `git mv crates/tisvlay-cli crates/tisvlay`.
- `crates/tisvlay/Cargo.toml`: `name = "tisvlay"`; keep `[[bin]] name = "tisvlay"`;
  `publish = true`.
- `Cargo.toml` (root): update the `members` and `default-members` path
  `crates/tisvlay-cli` → `crates/tisvlay`. **No `[workspace.dependencies]` change**
  (review finding) — `tisvlay-cli` is bin-only and nothing depends on it as a
  crate, so it isn't listed there (unlike `tisvlayd`/`-session`/`-gui`, which are
  exposed as libs for the harness).
- **Grep-and-update every `tisvlay-cli` / `tisvlay_cli` reference** (full list per
  review): root `Cargo.toml` members; `packaging/nix/package.nix` + `justfile`
  (only if any `-p` survives Part A); `.claude/CLAUDE.md`; `docs/design.md` /
  `docs/testing.md` / `docs/ideas/*`; `scripts/bench_build_baselines.py`; and a
  comment in `crates/tisvlay-cli/tests/pair_snapshot.rs`. No Rust `use tisvlay_cli`
  exists (bin-only). No backwards-compat shim (project policy).
- **Fallback (not chosen):** a facade `tisvlay` *lib* crate re-exporting
  `tisvlay-core` — pick this only if a public library API ("TV-control SDK")
  becomes a goal; it gives no `cargo install` and weaker content.

## Part D — release-plz automation

- **`release-plz.toml`** (root): unified workspace version (keep the shared
  `0.1.0` scheme); release-plz auto-skips `publish = false` crates and publishes
  the rest in topological order.
- **`.github/workflows/release-plz.yml`**: the standard two jobs — `release-pr`
  (opens version-bump + changelog PRs) and `release` (tags + publishes to
  crates.io on merge to `main`). Secrets: `CARGO_REGISTRY_TOKEN` (crates.io) and
  a GitHub token with PR/contents write.
- **Integrate with `release.yml` — decoupled, not tag-coupled** (implementation
  finding). release-plz creates one tag *per package* (`{{ package }}-v{{ version }}`);
  a single shared `v{{ version }}` tag "may create conflicts in multi-package
  workspaces" (it tags every published crate at the same name). So we don't couple
  via the tag. Instead: release-plz owns crates.io (version bump + publish +
  per-package tags + CHANGELOGs) with **`git_release_enable = false`** so it makes
  no GitHub release; the existing `release.yml` keeps owning the distro packages +
  GitHub release, still triggered by a **manually-pushed `v*` tag** (its
  per-package tags don't match `v*`, so the two paths don't collide). Net flow:
  merge the release PR → crates.io auto-publish; push `v0.x.y` → distro packages +
  GitHub release (unchanged from today).
- **Bail-out.** If release-plz's automation proves heavy, the fallback is a small
  scripted `cargo publish` in topological order (with `CRATES_IO_TOKEN`) invoked
  from `release.yml` — the publish set + order are already known.
- Requires Parts B + C done first (every crate must be publishable in order).

## Part E — Ship the mock backend as a visible debug aid

A field-debugging aid: with the mock backend in the shipped daemon, a user can
add a `mock` TV and confirm the daemon / D-Bus / CLI / GUI / tray stack works
end-to-end independent of a real TV — isolating "tisvlay bug" from "TV/network
issue." The production path **already exists**
(`tisvlay-backends::register_mock_production`, `#[cfg(feature = "mock")]`, called
from `create_registry`); this just turns the feature on for shipped builds.

- **Enable the feature on the shipped `tisvlayd`** — only the daemon builds the
  registry, so add a passthrough feature `mock = ["tisvlay-backends/mock"]` to
  `crates/tisvlayd/Cargo.toml` and build the packages with it on:
  - `justfile` `package-build`: `cargo zigbuild --release … --features tisvlayd/mock`.
  - `packaging/nix/package.nix`: `buildFeatures = [ "tisvlayd/mock" ]`.
  The CLI / GUI / tray need no change — they pick up the new vendor over D-Bus via
  `Manager1.GetSupportedVendors`.
- **Visible (chosen).** `mock` appears in the pairing wizard's manual-entry vendor
  dropdown and the CLI's vendor list — discoverable for self-service triage. Give
  the backend a clear display name (e.g. "Mock (test backend)") in
  `crates/tisvlay-mock/src/backend.rs` so it isn't mistaken for real hardware. Its
  discovery returns empty, so it never surfaces fake TVs in SSDP results.
- **Robustness pass (it's shipped code now).** Audit the ~16 `.expect()` / `panic!`
  sites in `crates/tisvlay-mock/src/` — especially `MockVendorConfig` TOML parsing
  reachable from user/config input — and convert to error returns so a malformed
  mock config can't panic the system-service daemon.
- **Label it as a debug backend** in `tisvlay-mock`'s crate `description` + a line
  in the root README's vendor/debug section.

Independent of the crates.io work (it only needs the feature flag), but pairs
thematically — `tisvlay-mock` graduates from test fixture to supported debug
backend.

- **Implementation note.** Most of Part E was already in place: the `mock`
  passthrough feature exists on `tisvlayd`, the display name is already
  `VENDOR_NAME = "Mock (testing)"`, and the ~16 panic sites are all
  `#[cfg(test)]` in `handle.rs` (the production path — `new_production_parts`
  → `MockConnector` / `MockDiscovery`, config via serde — has none). So the
  actual change was enabling the feature in the two build recipes plus the
  README / crate-description label.

---

## Suggested phasing (separate PRs)

1. **PR 1 — Part A** (`default-members`): small, low-risk, immediate de-dup win.
2. **PR 2 — Parts B + C**: publish flags + mock + the `tisvlay-cli`→`tisvlay`
   rename. The "publish-ready" PR.
3. **PR 3 — Part D**: release-plz workflow + config.
4. **PR 4 — Part E**: ship the mock backend visibly (feature on in release builds
   + label + robustness pass). Independent — can land any time after PR 1.

## Verification

- **Part A**: `cargo build` (no args) builds only the 5 binaries; `cargo build
  --workspace` and `just check` build/test everything; `just package-deb` and the
  nix build still produce the same artifacts.
- **Part B/C**: `cargo build -p tisvlay` works; `grep -rn 'tisvlay-cli\|tisvlay_cli'`
  returns nothing (or only intentional history); **`cargo publish --dry-run -p
  <crate>`** for each of the 10 in topological order verifies metadata + dep
  availability without uploading (this is the real proof the mock blocker is
  resolved — dry-run `tisvlay-backends` after a real/`--dry-run` mock).
- **Part D**: run `release-plz` in dry-run / let the `release-pr` job open a PR on
  a branch; confirm it targets the 10 publishable crates in order and skips
  test/harness. First real publish is `0.1.0`.
- **Part E**: `cargo run -p tisvlayd --features mock` (or an installed package
  built with the feature) lists `mock` in `Manager1.GetSupportedVendors`; add a
  mock TV and exercise power / screen / volume via CLI + GUI; feed a malformed
  mock config and confirm the daemon logs an error rather than panicking.
- **Post-publish smoke**: in a scratch dir, `cargo new x && cd x && cargo add
  tisvlay-core` (and `tisvlay`) resolves from crates.io.

## Typosquatting residual (document, don't over-engineer)

Publishing the 10 crates + `tisvlay` reserves those names and blocks their
`-`/`_`/case variants. **Not** covered: `tisvlay-test` / `tisvlay-harness`
(low-value dev names, left open) and arbitrary typos like `tisvlai-*` (crates.io
does no typo-distance blocking). Cheap mitigation: a short "Official
distribution channels" note in the root `README.md` (and/or `SECURITY.md`) so
users verify provenance. Optionally reserve `tisvlay-test`/`-harness` later.

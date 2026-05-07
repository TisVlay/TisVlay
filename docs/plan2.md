# Diagnostic blob enrichment

> **Status:** ✅ Both PRs shipped. PR #1 (in-process enrichment, `2056dc3`), PR #2 (build / OS metadata, `d7305d8`), with a follow-up dropping `gui.slint` after the fact (`80ff09d`) — rationale in "As-shipped deltas" below. Follow-on work (log ring buffer, consent UI, support-bundle CLI, crash capture, optional remote ingest) lives in [`docs/ideas.md`](ideas.md).

The About pane's "Diagnostics" button copies a JSON blob to the clipboard so users can paste it into a GitHub issue. Pre-sprint, the blob carried the gui / daemon / session versions and not much else — peer connectivity, runtime context, and build provenance all had to be asked for separately. This sprint widened the blob to "one paste covers most triage questions" without introducing remote ingestion, bundle archives, or panic-time capture (those land later behind their own gating).

## Target schema after PR #2

```json
{
  "schema_version": 1,
  "gui": {
    "version": "0.1.0",
    "profile": "release",
    "target": "x86_64-unknown-linux-gnu",
    "rustc": "1.85.0",
    "git_sha": "9811118"
  },
  "components": {
    "daemon":  { "version": "0.1.0", "connected": true,  "config": true  },
    "session": { "version": "0.1.0", "connected": true,  "config": true  },
    "tray":    { "version": "0.1.0", "connected": false, "config": false }
  },
  "runtime": {
    "bus": "user",
    "desktop": "KDE",
    "session_type": "wayland",
    "locale": "en_US.UTF-8",
    "kernel": "6.18.24",
    "glibc": "2.40",
    "os_id": "nixos",
    "os_version": "25.11"
  },
  "tvs": {
    "configured": 2,
    "connected": 2,
    "by_vendor": { "lg": 2 }
  },
  "vendors_supported": ["lg", "mock"]
}
```

`schema_version: 1` is in the blob from PR #1 onwards so issue templates that quote field paths don't break on future field renames.

## PR #1 — In-process enrichment

No new deps. Surfaces fields that already live inside the GUI process or sit one cached D-Bus call away.

### Fields added

| Path | Source |
|---|---|
| `schema_version` | constant `1` |
| `components.daemon.version` | `about_components` row "tisvlayd" (already fetched) |
| `components.daemon.connected` | `entry.bus_address != ""` |
| `components.daemon.config` | `entry.config_path != ""` — bool only; the path itself is PII (leaks `$HOME` / username) |
| `components.session.{version,connected,config}` | row "tisvlay-session" |
| `components.tray.{version,connected,config}` | row "tisvlay-tray" — currently absent from the blob |
| `runtime.bus` | `Daemon.bus_kind` global ("user" or "system") |
| `runtime.desktop` | `std::env::var("XDG_CURRENT_DESKTOP")`, lowercased |
| `runtime.session_type` | `std::env::var("XDG_SESSION_TYPE")` ("wayland" / "x11" / "tty") |
| `runtime.locale` | `std::env::var("LANG")` |
| `tvs.configured` | TV model row count |
| `tvs.connected` | filter on `power == "on" \|\| connected` |
| `tvs.by_vendor` | rollup of TV model `vendor_id` |
| `vendors_supported` | cached `Manager1.GetSupportedVendors` result (one-shot at startup) |

### Implementation notes

- `build_diagnostic_blob` moved out of [crates/tisvlay-gui/src/main.rs](../crates/tisvlay-gui/src/main.rs) into a new [`diagnostic` module](../crates/tisvlay-gui/src/diagnostic.rs). Renderer takes plain `ComponentSnap` / `TvSnap` snapshot structs (no Slint types) so unit tests don't need an event loop. Subfunctions: `gui_section`, `components_section`, `runtime_section`, `tvs_section`.
- Empty-string env vars (KDE-on-X11 sometimes leaves `XDG_SESSION_TYPE` unset) serialize as `null` rather than `""` for cleaner JSON consumption.
- `vendors_supported` ordering: sorted alphabetically so diffs across captures stay stable.
- `vendors_supported` is fetched once at startup (it's effectively a build-time constant for the running daemon); cached in a process-wide `Arc<std::sync::RwLock<Vec<String>>>` populated by `initial_fetch_and_watch` and read at click time.

### Tests

Inline `#[cfg(test)] mod tests` in `src/diagnostic.rs` rather than a dedicated `tests/` integration crate — the renderer is pure so unit-test scope is the right fit, and `insta` would have been overkill for the shape we're guarding.

- `schema_shape_is_stable` — JSON shape with synthetic inputs.
- `missing_component_entries_default_safely` — empty inputs surface `null` / `0` / `false` rather than panic.
- `empty_vendor_strings_are_skipped_in_rollup` — by-vendor rollup ignores empty vendor strings.
- `normalize_env_handles_unset_empty_and_lowercase` — env-var fallback (split out from `env_or_null` so tests don't mutate process-wide state).

## PR #2 — Build + OS metadata

Adds `vergen-gix` (build-dep on `tisvlay-gui` only) and `etc-os-release` (runtime dep). `rustix::system::uname()` and `libc::gnu_get_libc_version()` are reused — both already in the dep tree transitively (rustix via zbus, libc as ubiquitous).

### Fields added

| Path | Source |
|---|---|
| `gui.profile` | `cfg!(debug_assertions) ? "debug" : "release"` |
| `gui.target` | `env!("VERGEN_CARGO_TARGET_TRIPLE")` |
| `gui.rustc` | `env!("VERGEN_RUSTC_SEMVER")` |
| `gui.git_sha` | `env!("VERGEN_GIT_SHA")` (short, 7 chars) |
| `runtime.kernel` | `rustix::system::uname().release()` |
| `runtime.glibc` | `libc::gnu_get_libc_version()` via FFI helper, gated `cfg(target_env = "gnu")` |
| `runtime.os_id` | `etc_os_release::OsRelease::open()?.id` |
| `runtime.os_version` | `etc_os_release::OsRelease::open()?.version_id` |

### Why these crates

- **`vergen-gix` over `vergen-git2`** — pure-Rust gitoxide backend, no libgit2 / system git binary needed. Builds correctly under the Nix sandbox where `nativeBuildInputs` doesn't pull `git`. `vergen-gitcl` (shells out to `git`) would break there.
- **`etc-os-release` over `os_info`** — typed `/etc/os-release` parser with ~1 transitive dep (`thiserror`). `os_info` covers Windows/macOS too; we'd be paying for surfaces we don't ship. Both recognize NixOS's `/etc/os-release` correctly (verified in research).
- **`rustix::system::uname()` over `nix::sys::utsname::uname`** — rustix is already in the tree via zbus; nix would add a fresh root.
- **`libc::gnu_get_libc_version` direct FFI over `glibc_version` crate** — the latter is build-time-only; we want runtime so the deployment glibc is captured (matters for cross-distro packaging triage).

### Implementation notes

- `build.rs` extended with `vergen-gix`'s `Emitter` chain (`CargoBuilder` → `RustcBuilder` → `GixBuilder`). Outside-checkout builds (release tarballs, Nix sandbox without git history) fall back to vergen's `VERGEN_IDEMPOTENT_OUTPUT` sentinel; the renderer's `maybe_git_sha()` helper translates that to JSON `null` so consumers can tell tarball builds apart from real checkouts.
- Runtime probes live in a [`diagnostic::runtime_info`](../crates/tisvlay-gui/src/diagnostic/runtime_info.rs) submodule. `kernel_release()` (rustix uname), `glibc_version()` (~10-line FFI wrapper, `cfg(target_env = "gnu")` gate, documented `unsafe` block per the workspace's `undocumented_unsafe_blocks` lint), `os_release()` returning `(os_id, os_version)` — each `Option<String>`, mapped to JSON `null` by the renderer when absent.
- Field ordering in the JSON is stable across captures (serde's struct-field order) so diffs are noise-free.

### Tests

The PR #1 `schema_shape_is_stable` snapshot was extended to assert each PR #2 field is present and has the expected `string|null` type (the actual values churn per build). Plus one new pure unit test:

- `maybe_git_sha_collapses_idempotent_sentinel_to_null` — `""` and `VERGEN_IDEMPOTENT_OUTPUT` map to `Value::Null`; a real SHA round-trips as `Value::String`.

Out-of-band manual verification covered the Nix `dev-shell` path and a non-Nix `cargo build` (no CI matrix yet — would catch `vergen-gix` fallback regressions if they surface, but not part of this sprint).

## As-shipped deltas

Three notable refinements landed during implementation that aren't reflected in the original PR specs above:

- **`gui.slint` removed.** Original spec carried it alongside `gui.version`. With `gui.git_sha` shipping, every workspace dep version (slint included) is recoverable by checking out the commit and reading `Cargo.lock` — the field duplicated information now derivable on demand, and locked the schema to a specific UI toolkit choice. Dropping it makes `gui.{version, profile, target, rustc, git_sha}` framework-agnostic. Followed by removing the `TISVLAY_GUI_SLINT_VERSION` env emission, the `read_dep_version_from_lock` helper, and the `toml` build-dep that only existed to support it (`80ff09d`).
- **`peers` → `components`.** Field name renamed pre-flight to match the existing UI vocabulary (the About pane already labels this section "COMPONENTS"; the Slint model is `about_components`). Avoids forcing JSON consumers to learn fresh D-Bus jargon.
- **`config_path` → `config: bool`.** Original spec carried the path string. Refined to a bool to keep `$HOME` / username out of the inline blob (the path is PII). The folder-open button in the GUI still uses the live path internally; only the diagnostic surface drops it.

## Out of scope (tabled to `docs/ideas.md`)

| Topic | Idea entry |
|---|---|
| Tracing ring-buffer subscriber for in-blob log tail | "Diagnostic log-tail ring buffer" |
| Modal sheet preview UI with consent toggles | "Diagnostic preview-and-consent pane" |
| `tisvlay diagnostic [--bundle]` CLI + zip support | "Support-bundle CLI subcommand (tisvlay diagnostic)" |
| Panic hook + crash-time JSON dump | "Crash-time diagnostic capture" |
| Opt-in remote ingest (sentry-rust + GlitchTip) | "Opt-in remote crash reporting (dev-telemetry feature)" |
| Native crash capture (minidumper / crash-handler) | "Native crash capture via minidumper" |

Each is a meaningful learning surface on its own; the PR ladder lets them ship independently once the schema baseline lands.

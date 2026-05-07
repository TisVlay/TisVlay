# Diagnostic blob enrichment

> **Status:** PR #1 (in-process Tier 1+2 enrichment, no new deps) and PR #2 (build / OS metadata via `vergen-gix` + `etc-os-release`) scoped here. Follow-on work — log ring buffer, consent UI, support-bundle CLI, crash capture, optional remote ingest — tabled in [`docs/ideas.md`](ideas.md) and picked up after these two ship.

The About pane's "Diagnostics" button copies a JSON blob to the clipboard so users can paste it into a GitHub issue. Today's blob carries the gui / daemon / session versions and not much else — peer connectivity, runtime context, and build provenance all have to be asked for separately. This sprint widens the blob to "one paste covers most triage questions" without introducing remote ingestion, bundle archives, or panic-time capture (those land later behind their own gating).

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

- `build_diagnostic_blob` already lives in [crates/tisvlay-gui/src/main.rs](../crates/tisvlay-gui/src/main.rs). Refactor into a small `diagnostic` module split into subfunctions per top-level section (`build_peers`, `build_runtime`, `build_tvs`) so PR #2's deps can land in clean files.
- Empty-string env vars (KDE-on-X11 sometimes leaves `XDG_SESSION_TYPE` unset) serialize as `null` rather than `""` for cleaner JSON consumption.
- `vendors_supported` ordering: sorted alphabetically so diffs across captures stay stable.
- `vendors_supported` is fetched once at startup (it's effectively a build-time constant for the running daemon); cache on `Daemon` global.

### Tests

- `tests/diagnostic_snapshot.rs` (insta) capturing the JSON shape with synthetic inputs (no live D-Bus) — guards against accidental field renames or shape drift.
- One test per env-var fallback (unset → null, weird casing → preserved as-given).
- Coverage of the by-vendor rollup with mixed vendors and an empty TV list.

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

- `build.rs` for `tisvlay-gui` already exists for Slint compilation. Extend it with `vergen`'s `EmitBuilder` for git + cargo + rustc emission. Outside-checkout builds (release tarballs) fall back to compile-time stubs — match `vergen`'s `fallback` patterns so the Nix sandbox or vendored builds don't break.
- `etc_os_release::OsRelease::open()` returns `Result` — wrap in a `runtime_info()` helper that returns `None` for missing fields rather than failing the whole capture. NixOS images without `/etc/os-release` are theoretical but possible.
- `glibc_version_helper` module: ~5-line FFI wrapper, gated `cfg(target_env = "gnu")`, returns `Option<String>` so musl builds (theoretical) surface as `null`.
- Field ordering in the JSON is stable across captures (serde's struct-field order) so diffs are noise-free.

### Tests

- Extend the PR #1 snapshot test with one variant that asserts the build-time fields are non-empty strings (not the actual values, which churn per build).
- Unit test for the os-release parser's null-fallback behavior on an empty `/etc/os-release`.
- Smoke test on the `dev-shell` (Nix) and `cargo build` (non-Nix) paths to catch the `vergen-gix` fallback regression.

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

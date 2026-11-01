# Diagnostic blob

The About pane's "Diagnostics" button copies a JSON blob to the user's clipboard so they can paste it into a GitHub issue. The blob is a one-shot snapshot of the running GUI process's view of itself, its peer services, the host runtime, and the configured TV inventory — enough to answer most "what's the user's setup?" questions without a follow-up round-trip.

The renderer lives in [`crates/tisvlay-gui-app/src/diagnostic.rs`](../crates/tisvlay-gui-app/src/diagnostic.rs); host probes (kernel / glibc / OS) live in the [`runtime_info`](../crates/tisvlay-gui-app/src/diagnostic/runtime_info.rs) submodule.

## Schema (v1)

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

`schema_version: 1` is in the blob so consumers branch on the version rather than guess from field presence; bump on breaking field renames or removals.

## Field sources

| Path | Source |
|---|---|
| `schema_version` | constant `1` |
| `gui.version` | `env!("CARGO_PKG_VERSION")` |
| `gui.profile` | `cfg!(debug_assertions) ? "debug" : "release"` |
| `gui.target` | `env!("VERGEN_CARGO_TARGET_TRIPLE")` |
| `gui.rustc` | `env!("VERGEN_RUSTC_SEMVER")` |
| `gui.git_sha` | `env!("VERGEN_GIT_SHA")` (short, 7 chars) |
| `components.{daemon,session,tray}.version` | `about_components` model rows (each service's `info` rpc) |
| `components.{daemon,session,tray}.connected` | `row.online` (the service's liveliness token) |
| `components.{daemon,session,tray}.config` | `row.config_path != ""` — bool only; the path itself is PII |
| `runtime.bus` | always `"user"`: the GUI passes a constant |
| `runtime.desktop` | `std::env::var("XDG_CURRENT_DESKTOP")`, lowercased |
| `runtime.session_type` | `std::env::var("XDG_SESSION_TYPE")` ("wayland" / "x11" / "tty") |
| `runtime.locale` | `std::env::var("LANG")` |
| `runtime.kernel` | `rustix::system::uname().release()` |
| `runtime.glibc` | `libc::gnu_get_libc_version()` via FFI helper, gated `cfg(target_env = "gnu")` |
| `runtime.os_id` | `os_info::get().os_type()` lowercased to match `/etc/os-release ID=` convention |
| `runtime.os_version` | `os_info::get().version()` (see caveats — `Version::Semantic` normalizes the raw string) |
| `tvs.configured` | TV model row count |
| `tvs.connected` | filter on `power == "on" \|\| connected` |
| `tvs.by_vendor` | rollup of TV model `vendor_id`; empty vendors skipped |
| `vendors_supported` | the cached `manager/vendors` catalog, sorted alphabetically for stable diffs |

## Caveats — what `null` means

- **`gui.git_sha: null`** — build ran outside a git checkout (release tarball, Nix sandbox without git history). the GUI's build script falls back to a `VERGEN_IDEMPOTENT_OUTPUT` sentinel, which the renderer's `maybe_git_sha()` collapses to JSON `null`. Lets a maintainer tell tarball builds apart from real checkouts.
- **`runtime.os_version` normalization** — `os_info` parses `VERSION_ID` into a typed `Version::Semantic(major, minor, patch)` enum and re-renders, which drops formatting from the original string. NixOS's YY.MM convention loses its leading zero: `VERSION_ID="26.05"` shows up as `"26.5.0"` in the blob. Acknowledged upstream as a bug ([stanislav-tkach/os_info#318](https://github.com/stanislav-tkach/os_info/issues/318), proposed string-based redesign in [#280](https://github.com/stanislav-tkach/os_info/issues/280)); fixes without a code change here when the upstream lands.
- **`runtime.{kernel,glibc,os_id,os_version}: null`** — the corresponding probe returned `None` (musl builds for `glibc`; `Type::Unknown` / `Version::Unknown` for OS fields; non-UTF-8 syscall output for kernel — the last is theoretical).
- **`runtime.{desktop,session_type,locale}: null`** — env var unset or empty (e.g., `$XDG_SESSION_TYPE` is sometimes unset under KDE-on-X11).

## Redactions — what's deliberately not in the blob

- **No file paths.** `components.*.config` is a bool; the actual path leaks `$HOME` / username and stays in-process for the GUI's "open config folder" affordance.
- **No identifying TV data.** `tvs.by_vendor` is a count rollup keyed by vendor id (`{"lg": 2}`); UUIDs, MAC addresses, IPs, friendly names, and hostnames don't cross the blob boundary.
- **No host identity.** Hostname, username, MAC of the local machine — not collected.

## Crate rationale

- **A hand-rolled build script over the `vergen` crates.** `tisvlay-gui-app/build.rs` sets the `VERGEN_*` variables itself, from cargo's `TARGET` and `RUSTC` and one `git rev-parse`, keeping vergen's names so the renderer didn't change. The gitoxide backend of `vergen-gix` cost more build time than three values justify (`docs/build.md` § Build scripts).
- **`os_info` over `etc-os-release`** — `os_info` is more widely adopted (used by `cargo` itself), and its typed `Type::NixOS` enum defends against distro-detection typos. `etc-os-release` was the initial choice for its leanness; swapped after a reviewer flagged adoption signal. Net dep cost was zero — `os_info`'s direct deps (`log`, `nix`) are already transitive in our tree. The swap introduced the `os_version` normalization caveat above.
- **`rustix::system::uname()` over `nix::sys::utsname::uname`** — `rustix` is already in the tree via `zbus`'s transitive surface; `nix` would add a fresh root. (Note: `os_info` later pulled `nix` into the tree anyway for its own purposes, but `rustix` is still the right choice for direct calls — its API is leaner and it's pure-Rust.)
- **`libc::gnu_get_libc_version` direct FFI over the `glibc_version` crate** — the latter is build-time-only; we want runtime so the deployment glibc is captured (matters for cross-distro packaging triage). The FFI wrapper is ~10 lines with a documented `unsafe` block per the workspace's `undocumented_unsafe_blocks` lint, gated `cfg(target_env = "gnu")` so musl builds surface `null`.

## Renderer architecture

The renderer takes plain Rust snapshot structs (`ComponentSnap`, `TvSnap`, `Inputs`) — no Slint types — so unit tests don't need to boot an event loop. `about.rs` (`build_diagnostic_blob`) extracts from `app.get_about_components()` / `app.get_tvs()` into the snapshot Vecs at click time, then calls `diagnostic::build_blob(Inputs<'_>)`. Subfunctions (`gui_section`, `components_section`, `runtime_section`, `tvs_section`) keep each top-level JSON object's logic isolated so future enrichments — log-tail capture, support-bundle CLI, etc. (tracked in [`docs/ideas/`](ideas/README.md)) — slot in cleanly.

`vendors_supported` is fetched once per daemon lifetime from the `manager/vendors` catalog and cached in a process-wide `Arc<std::sync::RwLock<Vec<String>>>` populated by the state watcher's lifetime fetch (`fetch_and_publish_vendors`). Reading at click time is racy with startup but bounded — empty before the first fetch returns, stable within a daemon lifetime; the renderer sorts a copy for diff stability across captures.

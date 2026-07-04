//! Diagnostic JSON blob produced by the About-pane "Diagnostics"
//! button. Schema + rationale: `docs/diagnostics.md`.
//!
//! The renderer takes plain Rust snapshot structs (no Slint types)
//! so it's testable without booting an event loop. `main.rs` extracts
//! from `app.get_about_components()` / `app.get_tvs()` and threads
//! the per-process context (`user_bus`, cached `supported_vendors`)
//! into [`Inputs`] at click time.

use serde_json::{Value, json};

mod runtime_info;

/// Embedded blob schema version. Bump on breaking field renames /
/// removals so consumers branch on the version rather than guessing
/// from field presence.
pub const SCHEMA_VERSION: u32 = 1;

const GUI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build profile baked in at compile time (`debug` for `cargo build`
/// and `--profile test`, `release` for `--release`). Folded into the
/// blob's `gui.profile` field — useful triage signal because dev and
/// release builds expose different log levels and feature gates.
const GUI_PROFILE: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "release"
};

/// Target triple emitted by `vergen-gix`. Always present.
const GUI_TARGET: &str = env!("VERGEN_CARGO_TARGET_TRIPLE");

/// `rustc --version` semver token emitted by `vergen-gix`. Always
/// present.
const GUI_RUSTC: &str = env!("VERGEN_RUSTC_SEMVER");

/// 7-char git short SHA emitted by `vergen-gix`. Falls back to
/// `VERGEN_IDEMPOTENT_OUTPUT` when the build runs outside a git
/// checkout (release tarball, Nix sandbox without git history); the
/// renderer translates that sentinel to `null` so consumers can tell
/// "tarball build" apart from a clean repo build.
const GUI_GIT_SHA: &str = env!("VERGEN_GIT_SHA");
const VERGEN_IDEMPOTENT_SENTINEL: &str = "VERGEN_IDEMPOTENT_OUTPUT";

/// Per-component snapshot. One of these per row in the About pane's
/// COMPONENTS table. `name` is the catalogue id (`"tisvlayd"`,
/// `"tisvlay-session"`, `"tisvlay-tray"`); the renderer routes by it.
#[derive(Debug, Clone, Default)]
pub struct ComponentSnap {
    pub name: String,
    pub version: String,
    pub connected: bool,
    pub has_config: bool,
}

/// Per-TV snapshot, reduced to the two fields the diagnostic blob
/// rolls up (vendor for `by_vendor` count, connected for `connected`
/// count). Matches a subset of `TvViewModel`.
#[derive(Debug, Clone, Default)]
pub struct TvSnap {
    pub vendor: String,
    pub connected: bool,
}

/// All inputs the renderer needs. Pure data; no Slint, no D-Bus.
#[derive(Debug)]
pub struct Inputs<'a> {
    pub components: &'a [ComponentSnap],
    pub tvs: &'a [TvSnap],
    /// `true` when the GUI is dialed into the per-user dev daemon
    /// socket, `false` for the packaged system socket.
    pub user_bus: bool,
    /// Cached supported-vendor IDs (the daemon's `manager/vendors`
    /// catalog). Empty before the startup fetch returns; renderer
    /// sorts a copy for stable diffs.
    pub supported_vendors: &'a [String],
}

/// Render the diagnostic blob. Returns pretty-printed JSON; falls
/// back to compact JSON if pretty-print fails (effectively never).
pub fn build_blob(inputs: Inputs<'_>) -> String {
    let value = render(&inputs);
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
}

fn render(inputs: &Inputs<'_>) -> Value {
    let mut sorted_vendors = inputs.supported_vendors.to_vec();
    sorted_vendors.sort();
    json!({
        "schema_version": SCHEMA_VERSION,
        "gui": gui_section(),
        "components": components_section(inputs.components),
        "runtime": runtime_section(inputs.user_bus),
        "tvs": tvs_section(inputs.tvs),
        "vendors_supported": sorted_vendors,
    })
}

fn gui_section() -> Value {
    json!({
        "version": GUI_VERSION,
        "profile": GUI_PROFILE,
        "target": GUI_TARGET,
        "rustc": GUI_RUSTC,
        "git_sha": maybe_git_sha(GUI_GIT_SHA),
    })
}

/// Collapse `vergen-gix`'s "no git history" sentinel to JSON `null`
/// so consumers can tell a tarball / Nix sandbox build apart from a
/// real checkout. Pure — split out for unit tests.
fn maybe_git_sha(raw: &str) -> Value {
    if raw.is_empty() || raw == VERGEN_IDEMPOTENT_SENTINEL {
        Value::Null
    } else {
        Value::String(raw.to_owned())
    }
}

fn components_section(components: &[ComponentSnap]) -> Value {
    let lookup = |name: &str| -> Value {
        components
            .iter()
            .find(|c| c.name == name)
            .map(|c| {
                json!({
                    "version": c.version,
                    "connected": c.connected,
                    "config": c.has_config,
                })
            })
            .unwrap_or_else(|| json!({ "version": "", "connected": false, "config": false }))
    };
    json!({
        "daemon": lookup("tisvlayd"),
        "session": lookup("tisvlay-session"),
        "tray": lookup("tisvlay-tray"),
    })
}

fn runtime_section(user_bus: bool) -> Value {
    let (os_id, os_version) = runtime_info::os_release();
    json!({
        "bus": if user_bus { "user" } else { "system" },
        "desktop": env_or_null("XDG_CURRENT_DESKTOP", true),
        "session_type": env_or_null("XDG_SESSION_TYPE", true),
        "locale": env_or_null("LANG", false),
        "kernel": opt_string(runtime_info::kernel_release()),
        "glibc": opt_string(runtime_info::glibc_version()),
        "os_id": opt_string(os_id),
        "os_version": opt_string(os_version),
    })
}

fn opt_string(value: Option<String>) -> Value {
    match value {
        Some(s) => Value::String(s),
        None => Value::Null,
    }
}

fn tvs_section(tvs: &[TvSnap]) -> Value {
    let configured = tvs.len();
    let connected = tvs.iter().filter(|t| t.connected).count();
    let mut by_vendor: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for t in tvs {
        if t.vendor.is_empty() {
            continue;
        }
        *by_vendor.entry(t.vendor.clone()).or_insert(0) += 1;
    }
    json!({
        "configured": configured,
        "connected": connected,
        "by_vendor": by_vendor,
    })
}

/// Read an env var; return `Value::Null` when unset or empty so the
/// JSON consumer can branch on `null` rather than `""`. `lowercase`
/// folds the value ($XDG_CURRENT_DESKTOP / $XDG_SESSION_TYPE arrive
/// in mixed case across DEs; locale strings are case-sensitive).
fn env_or_null(key: &str, lowercase: bool) -> Value {
    normalize_env(std::env::var(key).ok(), lowercase)
}

/// Pure form of [`env_or_null`] split out so unit tests don't mutate
/// process-wide env state (`set_var` is `unsafe` since Rust 2024 and
/// races other tests run in parallel).
fn normalize_env(raw: Option<String>, lowercase: bool) -> Value {
    match raw {
        Some(s) if !s.is_empty() => Value::String(if lowercase { s.to_lowercase() } else { s }),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inputs() -> (Vec<ComponentSnap>, Vec<TvSnap>, Vec<String>) {
        let components = vec![
            ComponentSnap {
                name: "tisvlayd".into(),
                version: "0.1.0".into(),
                connected: true,
                has_config: true,
            },
            ComponentSnap {
                name: "tisvlay-session".into(),
                version: "0.1.0".into(),
                connected: true,
                has_config: true,
            },
            ComponentSnap {
                name: "tisvlay-tray".into(),
                version: "0.0.9".into(),
                connected: false,
                has_config: false,
            },
        ];
        let tvs = vec![
            TvSnap {
                vendor: "lg".into(),
                connected: true,
            },
            TvSnap {
                vendor: "lg".into(),
                connected: false,
            },
            TvSnap {
                vendor: "mock".into(),
                connected: true,
            },
        ];
        let vendors = vec!["mock".into(), "lg".into()];
        (components, tvs, vendors)
    }

    #[test]
    fn schema_shape_is_stable() {
        let (components, tvs, vendors) = sample_inputs();
        let v = render(&Inputs {
            components: &components,
            tvs: &tvs,
            user_bus: true,
            supported_vendors: &vendors,
        });

        assert_eq!(v["schema_version"], SCHEMA_VERSION);
        assert_eq!(v["gui"]["version"], GUI_VERSION);
        // PR #2 build-time fields: assert presence + non-empty rather
        // than exact values (the latter churn per build).
        assert!(v["gui"]["profile"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(v["gui"]["target"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(v["gui"]["rustc"].as_str().is_some_and(|s| !s.is_empty()));
        // git_sha is either a 7-char short SHA or null (tarball / Nix
        // sandbox build outside a checkout).
        assert!(v["gui"]["git_sha"].is_string() || v["gui"]["git_sha"].is_null());
        // PR #2 runtime fields: each is `Option<String>` → string or null.
        for key in ["kernel", "glibc", "os_id", "os_version"] {
            let field = &v["runtime"][key];
            assert!(
                field.is_string() || field.is_null(),
                "runtime.{key} expected string|null, got {field:?}"
            );
        }
        assert_eq!(v["components"]["daemon"]["connected"], true);
        assert_eq!(v["components"]["daemon"]["config"], true);
        assert_eq!(v["components"]["tray"]["connected"], false);
        assert_eq!(v["components"]["tray"]["config"], false);
        assert_eq!(v["runtime"]["bus"], "user");
        assert_eq!(v["tvs"]["configured"], 3);
        assert_eq!(v["tvs"]["connected"], 2);
        assert_eq!(v["tvs"]["by_vendor"]["lg"], 2);
        assert_eq!(v["tvs"]["by_vendor"]["mock"], 1);
        // Sorted alphabetically for stable diffs across captures.
        assert_eq!(v["vendors_supported"][0], "lg");
        assert_eq!(v["vendors_supported"][1], "mock");
    }

    #[test]
    fn missing_component_entries_default_safely() {
        let v = render(&Inputs {
            components: &[],
            tvs: &[],
            user_bus: false,
            supported_vendors: &[],
        });
        assert_eq!(v["components"]["daemon"]["version"], "");
        assert_eq!(v["components"]["daemon"]["connected"], false);
        assert_eq!(v["components"]["daemon"]["config"], false);
        assert_eq!(v["runtime"]["bus"], "system");
        assert_eq!(v["tvs"]["configured"], 0);
        assert_eq!(v["tvs"]["connected"], 0);
        assert!(v["tvs"]["by_vendor"].as_object().unwrap().is_empty());
    }

    #[test]
    fn empty_vendor_strings_are_skipped_in_rollup() {
        let tvs = vec![
            TvSnap {
                vendor: "lg".into(),
                connected: true,
            },
            TvSnap {
                vendor: String::new(),
                connected: false,
            },
        ];
        let v = render(&Inputs {
            components: &[],
            tvs: &tvs,
            user_bus: false,
            supported_vendors: &[],
        });
        let by_vendor = v["tvs"]["by_vendor"].as_object().unwrap();
        assert_eq!(by_vendor.len(), 1);
        assert_eq!(by_vendor["lg"].as_u64(), Some(1));
    }

    #[test]
    fn maybe_git_sha_collapses_idempotent_sentinel_to_null() {
        assert_eq!(maybe_git_sha(""), Value::Null);
        assert_eq!(maybe_git_sha("VERGEN_IDEMPOTENT_OUTPUT"), Value::Null);
        assert_eq!(maybe_git_sha("9811118"), Value::String("9811118".into()));
    }

    #[test]
    fn normalize_env_handles_unset_empty_and_lowercase() {
        assert_eq!(normalize_env(None, false), Value::Null);
        assert_eq!(normalize_env(Some(String::new()), false), Value::Null);
        assert_eq!(
            normalize_env(Some("Wayland".into()), true),
            Value::String("wayland".into())
        );
        assert_eq!(
            normalize_env(Some("en_US.UTF-8".into()), false),
            Value::String("en_US.UTF-8".into())
        );
    }
}

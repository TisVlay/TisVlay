//! Emits build-time env vars and the bundled-deps blob. Lives at the GUI
//! app crate's root because it owns artifacts that end up baked into the
//! binary itself; the Slint UI compiles in the `tisvlay-gui` crate.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    emit_build_env_vars();
    generate_third_party_licenses();
}

/// Sentinel emitted when git or rustc info isn't available
/// (vendored tarball, Nix sandbox without `.git/`, parse failure).
/// Kept on the same string contract `vergen-gix` previously
/// produced so `diagnostic::maybe_git_sha` can collapse it to JSON
/// `null` without changes.
const IDEMPOTENT_SENTINEL: &str = "VERGEN_IDEMPOTENT_OUTPUT";

/// Emit `VERGEN_CARGO_TARGET_TRIPLE`, `VERGEN_RUSTC_SEMVER`, and
/// `VERGEN_GIT_SHA` (7-char short) for the diagnostic blob's
/// `gui.{target, rustc, git_sha}` fields. Hand-rolled rather than
/// pulled from `vergen-gix` because that crate's gitoxide backend
/// drags 46 transitive crates and pins two `hashbrown` versions
/// (`gix-tempfile` → `dashmap@6` → `hashbrown 0.14`; `gix-diff` →
/// `imara-diff` → `hashbrown 0.15`). All three env vars are trivial
/// to derive from cargo's own `TARGET`/`RUSTC` env vars and one
/// `git rev-parse` shell-out.
fn emit_build_env_vars() {
    // Target triple — cargo always sets `TARGET` for build scripts.
    let target = std::env::var("TARGET").unwrap_or_else(|_| IDEMPOTENT_SENTINEL.to_string());
    println!("cargo:rustc-env=VERGEN_CARGO_TARGET_TRIPLE={target}");

    // Rustc semver — parse `rustc -V` output, e.g.
    //     rustc 1.95.0 (59807616e 2026-04-14)
    // → "1.95.0". cargo sets `RUSTC` to the rustc binary path.
    let rustc_bin = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_v = Command::new(&rustc_bin)
        .arg("-V")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().nth(1).map(str::to_string))
        .unwrap_or_else(|| IDEMPOTENT_SENTINEL.to_string());
    println!("cargo:rustc-env=VERGEN_RUSTC_SEMVER={rustc_v}");

    // Git SHA. Watch `.git/HEAD` so a branch switch invalidates the
    // build script; per-commit invalidation needs the active ref's
    // path which we don't know at build time, so the SHA can be
    // stale by a commit or two on local dev — acceptable for a
    // diagnostic field. Failures (vendored tarball, Nix sandbox,
    // missing `git` binary, detached state with empty output) emit
    // the sentinel.
    let workspace = workspace_root();
    let git_head = workspace.join(".git/HEAD");
    if git_head.exists() {
        println!("cargo:rerun-if-changed={}", git_head.display());
    }
    let git_sha = Command::new("git")
        .args(["-C", workspace.to_str().unwrap_or(".")])
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| IDEMPOTENT_SENTINEL.to_string());
    println!("cargo:rustc-env=VERGEN_GIT_SHA={git_sha}");
}

/// Generate `$OUT_DIR/THIRD-PARTY-LICENSES.html` for `include_str!`
/// embedding into the GUI binary, consumed by the About pane's
/// "View third-party notices" button via the browser-handoff path
/// in `src/main.rs::open_notices_html`. See `docs/licensing.md` for
/// the pipeline and `.claude/slint.md` "Slint text-rendering for
/// large documents" for why we hand off rather than render in-app.
///
/// The whole-workspace cargo-about walk is by far the slowest step in
/// this build script, so it is gated on an input stamp. The bundle is
/// fully determined by the dependency graph (`Cargo.lock`) plus the
/// config and template, so when none of those changed since the last
/// successful generate the existing bundle is reused verbatim. Editing
/// a `.slint` re-triggers this script (slint-build tracks the UI files)
/// but touches none of those inputs, so the GUI edit loop skips the walk.
///
/// On failure (most commonly: `cargo-about` not on `$PATH` in a
/// clean dev env without the Nix shell) writes a stub blob with
/// install instructions and emits a `cargo:warning=` rather than
/// failing the build. The stub path leaves no stamp, so the next
/// build retries once the real generator is available.
fn generate_third_party_licenses() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let workspace = workspace_root();
    let manifest = workspace.join("Cargo.toml");
    let lockfile = workspace.join("Cargo.lock");

    let about_dir = workspace.join("packaging/licensing");
    let about_toml = about_dir.join("about.toml");
    let about_template = about_dir.join("about.html.hbs");

    // Re-run this script when a license *input* changes. The lockfile is
    // load-bearing: a dependency bump changes the attribution set without
    // touching the config or template, and without watching it the embedded
    // bundle would silently go stale until some other input happened to move.
    println!("cargo:rerun-if-changed={}", about_toml.display());
    println!("cargo:rerun-if-changed={}", about_template.display());
    println!("cargo:rerun-if-changed={}", lockfile.display());

    let out_path = Path::new(&out_dir).join("THIRD-PARTY-LICENSES.html");
    let stamp_path = Path::new(&out_dir).join("THIRD-PARTY-LICENSES.stamp");

    let inputs_hash = hash_inputs(&[&lockfile, &about_toml, &about_template]);
    if let Some(hash) = inputs_hash
        && out_path.exists()
        && stamp_matches(&stamp_path, hash)
    {
        return;
    }

    let generated = generate_one(
        &manifest,
        &about_toml,
        &about_template,
        &out_path,
        stub_notices_blob_html(),
    );

    // Stamp only a real bundle. The stub path is intentionally left
    // unstamped so a later build retries once cargo-about is on `$PATH`.
    if generated && let Some(hash) = inputs_hash {
        let _ = std::fs::write(&stamp_path, hash.to_string());
    }
}

/// Run cargo-about, streaming the bundle to `out_path`. Returns `true`
/// when the real generator succeeded and `false` when the stub was
/// written instead, so the caller knows not to stamp the inputs.
fn generate_one(
    manifest: &Path,
    config: &Path,
    template: &Path,
    out_path: &Path,
    stub: String,
) -> bool {
    // `-o <path>` makes cargo-about stream the bundle directly to
    // the file rather than us buffering hundreds of KB through
    // `Command::output()`'s captured stdout. On the failure paths
    // (non-zero exit, spawn failure) we write the stub ourselves;
    // cargo-about may have left a partial file behind so the stub
    // write also overwrites whatever's there.
    let status = Command::new("cargo-about")
        .arg("generate")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--config")
        .arg(config)
        .arg("--workspace")
        .arg("--all-features")
        .arg("--locked")
        .arg("--fail")
        .arg("-o")
        .arg(out_path)
        .arg(template)
        .status();

    match status {
        Ok(s) if s.success() => true,
        Ok(s) => {
            // cargo-about will have already printed its diagnostic
            // to stderr; surface a single cargo:warning so the
            // dev sees their build degraded, then write the stub.
            println!(
                "cargo:warning=cargo-about generate failed for {} (status: {}); embedding stub.",
                template.display(),
                s
            );
            std::fs::write(out_path, stub.into_bytes()).expect("write THIRD-PARTY-LICENSES stub");
            false
        }
        Err(e) => {
            println!(
                "cargo:warning=cargo-about not invocable ({e}); embedding stub blob for {}. Install with `cargo install --locked cargo-about` (or run inside the Nix dev shell) to populate.",
                template.display()
            );
            std::fs::write(out_path, stub.into_bytes()).expect("write THIRD-PARTY-LICENSES stub");
            false
        }
    }
}

/// Hash the byte contents of the license-bundle inputs into one value.
/// `None` if any input can't be read, which makes the caller fall
/// through to a full (re)generate rather than trust a partial hash.
/// `DefaultHasher` seeds from fixed keys, so the digest is stable across
/// separate runs of this build script — exactly what the stamp compares.
fn hash_inputs(paths: &[&Path]) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in paths {
        std::fs::read(path).ok()?.hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// True when the stamp file exists and records exactly `hash`.
fn stamp_matches(stamp_path: &Path, hash: u64) -> bool {
    std::fs::read_to_string(stamp_path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .is_some_and(|stored| stored == hash)
}

fn stub_notices_blob_html() -> String {
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>TisVlay — Third-Party Licenses</title></head><body>\
     <h1>Third-party license attribution was not generated for this build.</h1>\
     <p>The About → Notices pane normally opens a workspace-wide bundle produced by <code>cargo-about</code> at build time. To populate it:</p>\
     <ul>\
     <li>Use the Nix dev shell (cargo-about is provisioned there), or</li>\
     <li>Install manually: <code>cargo install --locked cargo-about</code></li>\
     </ul>\
     <p>Once cargo-about is on <code>$PATH</code>, rebuild <code>tisvlay-gui</code> to regenerate this bundle.</p>\
     </body></html>\n"
        .to_string()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("couldn't canonicalize workspace root")
}

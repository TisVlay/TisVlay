//! Compiles the Slint UI and emits build-time env vars / bundled-deps
//! blob. Lives at the GUI crate's root because it owns artifacts that
//! end up baked into the binary itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use vergen_gix::{CargoBuilder, Emitter, GixBuilder, RustcBuilder};

fn main() {
    slint_build::compile("ui/main.slint").unwrap();

    emit_vergen_env_vars();
    generate_third_party_licenses();
}

/// Emit `VERGEN_CARGO_TARGET_TRIPLE`, `VERGEN_RUSTC_SEMVER`, and
/// `VERGEN_GIT_SHA` (short) for the diagnostic blob's `gui.{target,
/// rustc, git_sha}` fields. Failures are deliberately swallowed —
/// vergen falls back to `VERGEN_IDEMPOTENT_OUTPUT` when `.git/` is
/// missing (release tarball, Nix sandbox without git history), and
/// the diagnostic builder treats that sentinel as a non-fatal absence.
fn emit_vergen_env_vars() {
    let cargo = CargoBuilder::default().target_triple(true).build();
    let rustc = RustcBuilder::default().semver(true).build();
    // `.sha(true)` = 7-char short sha. False = full 40-char.
    let gix = GixBuilder::default().sha(true).build();

    let mut emitter = Emitter::default();
    let mut chain = Ok(&mut emitter);
    if let Ok(c) = &cargo {
        chain = chain.and_then(|e| e.add_instructions(c));
    }
    if let Ok(r) = &rustc {
        chain = chain.and_then(|e| e.add_instructions(r));
    }
    if let Ok(g) = &gix {
        chain = chain.and_then(|e| e.add_instructions(g));
    }
    if let Err(err) = chain.and_then(|e| e.emit()) {
        // Don't fail the build; the diagnostic blob will surface
        // `VERGEN_IDEMPOTENT_OUTPUT` as `null` so consumers can tell
        // a non-pristine build apart from a clean one.
        println!("cargo:warning=vergen-gix instructions failed: {err}");
    }
}

/// Generate `$OUT_DIR/THIRD-PARTY-LICENSES.html` for `include_str!`
/// embedding into the GUI binary, consumed by the About pane's
/// "View third-party notices" button via the browser-handoff path
/// in `src/main.rs::open_notices_html`. See `docs/licensing.md` for
/// the pipeline and `.claude/slint.md` "Slint text-rendering for
/// large documents" for why we hand off rather than render in-app.
///
/// On failure (most commonly: `cargo-about` not on `$PATH` in a
/// clean dev env without the Nix shell) writes a stub blob with
/// install instructions and emits a `cargo:warning=` rather than
/// failing the build.
fn generate_third_party_licenses() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let workspace = workspace_root();
    let manifest = workspace.join("Cargo.toml");

    let about_dir = workspace.join("packaging/licensing");
    let about_toml = about_dir.join("about.toml");
    let about_template = about_dir.join("about.html.hbs");

    println!("cargo:rerun-if-changed={}", about_toml.display());
    println!("cargo:rerun-if-changed={}", about_template.display());

    generate_one(
        &manifest,
        &about_toml,
        &about_template,
        &Path::new(&out_dir).join("THIRD-PARTY-LICENSES.html"),
        stub_notices_blob_html(),
    );
}

fn generate_one(manifest: &Path, config: &Path, template: &Path, out_path: &Path, stub: String) {
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
        Ok(s) if s.success() => {}
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
        }
        Err(e) => {
            println!(
                "cargo:warning=cargo-about not invocable ({e}); embedding stub blob for {}. Install with `cargo install --locked cargo-about` (or run inside the Nix dev shell) to populate.",
                template.display()
            );
            std::fs::write(out_path, stub.into_bytes()).expect("write THIRD-PARTY-LICENSES stub");
        }
    }
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

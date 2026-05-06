//! Compiles the Slint UI and emits build-time env vars / bundled-deps
//! blob. Lives at the GUI crate's root because it owns artifacts that
//! end up baked into the binary itself.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    slint_build::compile("ui/main.slint").unwrap();

    // The About pane's diagnostic-copy button emits a JSON blob that
    // includes the bundled Slint version. Lift it from `Cargo.lock`
    // so it stays in sync with the resolved dep automatically — no
    // hand-edit when the workspace bumps slint.
    let version = read_dep_version_from_lock("slint")
        .expect("couldn't determine slint version from Cargo.lock");
    println!("cargo:rustc-env=TISVLAY_GUI_SLINT_VERSION={version}");

    let lock = workspace_root().join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());

    generate_third_party_licenses();
}

/// Generate `$OUT_DIR/THIRD-PARTY-LICENSES.html` for `include_str!`
/// embedding into the GUI binary. The About pane's "View third-party
/// notices" button hands this off to the user's browser via
/// `xdg-open`: in packaged builds we prefer the distro-shipped copy
/// at `/usr/share/doc/tisvlay/`; the embedded blob is the dev-build
/// fallback (and a guarantee that the binary can always show its
/// own attribution without external files).
///
/// We don't render the bundle in-app: Slint's text engine can't
/// match a browser for ~1400-row monospace docs (femtovg lacks
/// dirty-region partial rendering — slint #6754; Skia helps
/// modestly but Text widgets render no link colors, no clickable
/// anchors, no copy-across-rows). Browser handoff is the principled
/// path. See `.claude/slint.md` "Slint text-rendering for large
/// documents" for the upstream-research trail that grounded the
/// pivot.
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

fn read_dep_version_from_lock(name: &str) -> Option<String> {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).ok()?;
    let value: toml::Value = toml::from_str(&lock).ok()?;
    let packages = value.get("package")?.as_array()?;
    for pkg in packages {
        if pkg.get("name")?.as_str() == Some(name) {
            return pkg.get("version")?.as_str().map(str::to_owned);
        }
    }
    None
}

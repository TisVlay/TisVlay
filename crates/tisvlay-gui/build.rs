//! Compiles the Slint UI and emits build-time env vars for versions
//! we can't reach via the standard `CARGO_PKG_*` set (which cargo
//! only populates for the current crate, not its dependencies).

use std::path::Path;

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
}

fn workspace_root() -> std::path::PathBuf {
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

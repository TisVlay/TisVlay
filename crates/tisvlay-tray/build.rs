//! Compiles per-locale `.po` translation catalogs into `.mo` files
//! that the runtime's `bindtextdomain("tisvlay-tray", …)` lookup
//! resolves at startup.
//!
//! The tray has no Slint dependency to do this transparently (the
//! GUI gets `.mo` compilation as a side effect of `slint_build`),
//! so we walk `crates/tisvlay-tray/lang/<locale>/LC_MESSAGES/*.po`
//! and shell out to `msgfmt` ourselves.
//!
//! Failure mode is intentionally soft: a missing `msgfmt` binary
//! emits a `cargo:warning=` and skips the file. Runtime then falls
//! back to the literal source string, which matches the no-catalog
//! behaviour you get on a fresh `git clone` before any translator
//! has contributed a `.po`.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let lang_dir = Path::new(&manifest_dir).join("lang");

    // Watch the lang root so a newly-added locale subdirectory
    // triggers a rebuild. Individual `.po` files get their own
    // `rerun-if-changed=` below.
    println!("cargo:rerun-if-changed={}", lang_dir.display());

    if !lang_dir.is_dir() {
        return;
    }

    let Ok(locales) = std::fs::read_dir(&lang_dir) else {
        return;
    };

    for locale in locales.flatten() {
        let lc_messages = locale.path().join("LC_MESSAGES");
        if !lc_messages.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&lc_messages) else {
            continue;
        };
        for entry in entries.flatten() {
            let po = entry.path();
            if po.extension().and_then(|e| e.to_str()) != Some("po") {
                continue;
            }
            println!("cargo:rerun-if-changed={}", po.display());
            let mo = po.with_extension("mo");
            compile_po(&po, &mo);
        }
    }
}

fn compile_po(po: &Path, mo: &Path) {
    let status = Command::new("msgfmt")
        .arg("--output-file")
        .arg(mo)
        .arg(po)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            println!(
                "cargo:warning=msgfmt failed for {} (status: {}); skipping. \
                 Runtime will fall back to source strings for this locale.",
                po.display(),
                s
            );
        }
        Err(e) => {
            println!(
                "cargo:warning=msgfmt not invocable ({e}); skipping {}. \
                 Install the gettext tools (or use the Nix dev shell) to \
                 populate per-locale `.mo` files.",
                po.display()
            );
        }
    }
}

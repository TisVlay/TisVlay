//! The compiled Slint UI of `tisvlay-gui`: every component, global and
//! struct `ui/main.slint` exports, plus the translation setup tied to this
//! package's gettext domain. `tisvlay-gui-app` re-exports it and drives it.

// Slint's `include_modules!()` expands a large block of generated code
// (`InnerAppWindow`, repeater types, etc.) that we don't get to
// annotate. Suppress the workspace lints they trip; they don't apply to
// generated UI scaffolding.
#![allow(unreachable_pub, missing_debug_implementations, missing_docs)]

slint::include_modules!();

/// Register the gettext catalogs for both Slint's `@tr()` and Rust-side
/// `gettext()` calls; they share one domain. `init_translations!` names the
/// domain after the package it expands in, so it must expand here, where
/// the `.slint` files compile. `lang/` resolves relative to this crate at
/// compile time; installed binaries fall back to the system locale dir.
pub fn init_translations() {
    slint::init_translations!(concat!(env!("CARGO_MANIFEST_DIR"), "/lang"));
}

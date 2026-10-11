//! Compiles the Slint UI; `slint::include_modules!()` in `src/lib.rs` reads
//! the generated code. With the `gettext` feature this also builds the
//! `lang/` catalogs' `.mo` files.

fn main() {
    slint_build::compile("ui/main.slint")
        .expect("the Slint UI failed to compile; slint-build printed the errors above");
}

fn main() {
    // Build-time Slint style. `cosmic` (System76's COSMIC desktop
    // style) is closer to KDE Breeze than the default `fluent`,
    // which on Linux + unfocused windows produces low-contrast
    // borders and washed-out disabled states. Override at build
    // time with `SLINT_STYLE=<name>` if desired (other built-ins:
    // fluent, material, cupertino, qt).
    let style = std::env::var("SLINT_STYLE").unwrap_or_else(|_| "cosmic".to_string());
    let config = slint_build::CompilerConfiguration::new().with_style(style);
    slint_build::compile_with_config("ui/main.slint", config).unwrap();
}

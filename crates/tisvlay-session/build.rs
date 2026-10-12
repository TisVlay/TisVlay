//! Record where pkg-config says SPA plugins live, the same answer
//! `pipewire-native`'s own build bakes in, so the session can check the
//! support plugin is there before the PipeWire client would panic over
//! it. Without a pkg-config entry nothing is recorded, and the session
//! checks the FHS layouts alone.

fn main() {
    println!("cargo::rerun-if-env-changed=PKG_CONFIG_PATH");
    if let Ok(plugindir) = pkg_config::get_variable("libspa-0.2", "plugindir") {
        println!("cargo::rustc-env=TISVLAY_SPA_PLUGINDIR={plugindir}");
    }
}

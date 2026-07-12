//! Binary entry point. Real code lives in [`tisvlay_gui`] (the
//! library target) so the integration tests in `tisvlay-gui/tests/`
//! can import the same modules and Slint types the binary uses.

fn main() -> anyhow::Result<()> {
    tisvlay_gui::run()
}

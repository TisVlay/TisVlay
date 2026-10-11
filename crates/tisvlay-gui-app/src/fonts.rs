//! The desktop's monospace face, for text whose characters must line up
//! (MAC and IP fields, wire identities, a parse error's caret). Slint
//! takes a `font-family` as one literal family name and never resolves
//! the generic `monospace`, so the family is looked up here, through
//! fontique (the font library Slint itself uses, so it sees the same
//! fontconfig setup), and pushed into `Theme.mono-font`. Until it lands,
//! or if no monospace face is found, that text uses the UI font.

use slint::{ComponentHandle, SharedString};
use tracing::debug;

use crate::{AppWindow, Theme, on_ui};

/// Look up the monospace family off the UI thread (the system font scan
/// blocks) and set `Theme.mono-font`.
pub(crate) fn resolve_monospace(app_weak: slint::Weak<AppWindow>) {
    tokio::task::spawn_blocking(move || match system_monospace_family() {
        Some(family) => {
            debug!(%family, "monospace font family");
            on_ui(&app_weak, move |app| {
                app.global::<Theme>()
                    .set_mono_font(SharedString::from(family));
            });
        }
        None => debug!("no monospace font family found; keeping the UI font"),
    });
}

/// The family the system maps the generic `monospace` to (fontconfig's
/// alias on Linux), if any.
fn system_monospace_family() -> Option<String> {
    let mut collection = fontique::Collection::new(fontique::CollectionOptions {
        shared: false,
        system_fonts: true,
    });
    let family = collection
        .generic_families(fontique::GenericFamily::Monospace)
        .next()?;
    collection.family_name(family).map(str::to_owned)
}

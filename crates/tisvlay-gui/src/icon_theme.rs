//! Freedesktop icon-theme resolution. KDE's chosen theme via
//! `~/.config/kdeglobals` first, then Breeze / Adwaita / hicolor as
//! fallbacks — covers KDE Plasma + GNOME without linking dconf.

use tracing::{debug, warn};

/// Detect the user's freedesktop icon theme name. KDE writes its
/// choice to `~/.config/kdeglobals` `[Icons] Theme=...`; GNOME / etc.
/// keep theirs in dconf which we don't link. Returns `None` if the
/// file isn't present or the key isn't found — caller falls back
/// through the hardcoded theme list.
fn detect_icon_theme() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = std::path::PathBuf::from(home).join(".config/kdeglobals");
    let content = std::fs::read_to_string(&path).ok()?;
    let mut in_icons_section = false;
    for line in content.lines() {
        let line = line.trim();
        if line == "[Icons]" {
            in_icons_section = true;
        } else if line.starts_with('[') {
            in_icons_section = false;
        } else if in_icons_section && let Some(value) = line.strip_prefix("Theme=") {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Resolve a freedesktop icon name (with fallbacks) and load it as a
/// Slint `Image`. The crate's default theme is `hicolor`, which on
/// NixOS / minimal distros ships zero icons — so we explicitly try
/// the user's chosen theme (read from `~/.config/kdeglobals` on
/// KDE), then `breeze` / `Adwaita` / `hicolor` in turn.
///
/// Size 22 because Breeze's "actions" / "devices" categories ship
/// 16/22/24/32/64 buckets unevenly — 22 is the most universally
/// populated and Slint downscales cleanly to the rendered 16×16.
///
/// Returns the default empty image on total miss; Slint draws
/// nothing for an empty source so the SidebarItem layout degrades
/// gracefully to label-only.
pub(crate) fn load_themed_icon(names: &[&str]) -> slint::Image {
    let user_theme = detect_icon_theme();
    let themes: Vec<&str> = user_theme
        .as_deref()
        .into_iter()
        .chain(["breeze", "Adwaita", "hicolor"])
        .collect();

    for name in names {
        for theme in &themes {
            let Some(path) = freedesktop_icons::lookup(name)
                .with_size(22)
                .with_theme(theme)
                .with_cache()
                .find()
            else {
                continue;
            };
            match slint::Image::load_from_path(&path) {
                Ok(img) => {
                    debug!(
                        icon = name,
                        theme = theme,
                        path = %path.display(),
                        "icon loaded",
                    );
                    return img;
                }
                Err(e) => {
                    warn!(
                        icon = name,
                        theme = theme,
                        path = %path.display(),
                        error = ?e,
                        "found path but Slint couldn't load",
                    );
                }
            }
        }
    }
    warn!(
        ?names,
        "no fallback resolved across user / breeze / Adwaita / hicolor"
    );
    slint::Image::default()
}

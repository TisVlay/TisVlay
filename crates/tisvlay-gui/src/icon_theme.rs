//! Freedesktop icon-theme resolution. The active desktop's chosen theme
//! first — KDE via `~/.config/kdeglobals`, GNOME/GTK via `gsettings` —
//! then Breeze / Adwaita / hicolor as fallbacks. Covers KDE Plasma +
//! GNOME (Ubuntu's Yaru, Fedora's Adwaita) without linking dconf.

use std::sync::OnceLock;

use tracing::{debug, warn};

/// The ordered icon-theme search list, resolved once. Leads with the
/// active desktop's chosen theme(s) so lookups land in the theme the
/// user actually sees, then the hardcoded fallbacks. The `freedesktop`
/// resolver follows each theme's `Inherits` chain, so a single detected
/// theme (e.g. `Yaru-dark`) already cascades through its parents.
fn icon_themes() -> &'static [String] {
    static THEMES: OnceLock<Vec<String>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let mut themes: Vec<String> = Vec::new();
        let mut push = |t: String| {
            if !t.is_empty() && !themes.contains(&t) {
                themes.push(t);
            }
        };
        if let Some(t) = detect_kde_theme() {
            push(t);
        }
        if let Some(t) = detect_gnome_theme() {
            push(t);
        }
        for fallback in ["breeze", "Adwaita", "hicolor"] {
            push(fallback.to_string());
        }
        debug!(?themes, "icon theme search order");
        themes
    })
}

/// KDE writes its icon-theme choice to `~/.config/kdeglobals`
/// `[Icons] Theme=...`. Returns `None` when the file or key is absent.
fn detect_kde_theme() -> Option<String> {
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

/// GNOME/GTK keep the icon theme in gsettings (dconf-backed). Shell out
/// to the `gsettings` CLI rather than linking glib — best-effort, so a
/// missing binary or non-GNOME session just yields `None` and we fall
/// through the hardcoded list.
fn detect_gnome_theme() -> Option<String> {
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    // gsettings prints the GVariant-quoted value, e.g. `'Yaru-dark'\n`.
    let theme = raw.trim().trim_matches(['\'', '"']).trim();
    (!theme.is_empty()).then(|| theme.to_string())
}

/// Resolve a freedesktop icon name (with fallbacks) and load it as a
/// Slint `Image`. The crate's default theme is `hicolor`, which on
/// NixOS / minimal distros ships zero icons — so we explicitly try the
/// active desktop's theme (see [`icon_themes`]), then `breeze` /
/// `Adwaita` / `hicolor` in turn.
///
/// Symbolic-first: for each requested name we prefer its `-symbolic`
/// variant, then the plain colored form. Modern themes (Adwaita, GNOME's
/// Yaru) ship the symbolic form of most action icons and drop the plain
/// one, and a flat monochrome glyph is the intended look for chrome — so
/// this both fixes resolution and matches the design. Use
/// [`load_color_icon`] for the handful of icons whose color carries
/// meaning (app identity, status).
pub(crate) fn load_symbolic_icon(names: &[&str]) -> slint::Image {
    load_icon(names, Variant::Symbolic)
}

/// Color-first counterpart of [`load_symbolic_icon`]: prefers the plain
/// colored form and only falls back to `-symbolic` when no theme ships
/// it. For icons whose color is the point — the app-identity logo, the
/// sidebar TV glyph — where a monochrome fallback would flatten meaning.
pub(crate) fn load_color_icon(names: &[&str]) -> slint::Image {
    load_icon(names, Variant::Color)
}

/// Which form of an icon name to try first.
#[derive(Clone, Copy)]
enum Variant {
    Symbolic,
    Color,
}

/// Split a requested name into its `(symbolic, color)` forms. A name
/// already ending in `-symbolic` yields itself plus the stripped base;
/// anything else yields the `-symbolic` suffixed form plus itself.
fn name_variants(name: &str) -> (String, String) {
    match name.strip_suffix("-symbolic") {
        Some(base) => (name.to_string(), base.to_string()),
        None => (format!("{name}-symbolic"), name.to_string()),
    }
}

/// Resolve the first name (across the theme cascade) that loads, trying
/// the preferred [`Variant`] of every name before the other variant of
/// any — so an explicit fallback name never loses to a synthesized
/// variant of an earlier name. Size 22 because Breeze's "actions" /
/// "devices" categories ship 16/22/24/32/64 buckets unevenly — 22 is the
/// most universally populated and Slint downscales cleanly to the
/// rendered 16×16. Returns the default empty image on total miss; Slint
/// draws nothing for an empty source so the layout degrades gracefully
/// to label-only.
fn load_icon(names: &[&str], prefer: Variant) -> slint::Image {
    let themes = icon_themes();
    for candidate in &icon_candidates(names, prefer) {
        for theme in themes {
            let Some(path) = freedesktop_icons::lookup(candidate)
                .with_size(22)
                .with_theme(theme)
                .with_cache()
                .find()
            else {
                continue;
            };
            match slint::Image::load_from_path(&path) {
                Ok(img) => {
                    debug!(icon = candidate, theme, path = %path.display(), "icon loaded");
                    return img;
                }
                Err(e) => {
                    warn!(
                        icon = candidate, theme, path = %path.display(), error = ?e,
                        "found path but Slint couldn't load",
                    );
                }
            }
        }
    }
    warn!(?names, ?themes, "no icon resolved across theme cascade");
    slint::Image::default()
}

/// The ordered, deduplicated lookup names for a request: the preferred
/// variant of every requested name first, then the other variant of
/// every name — so an explicit fallback name never loses to a
/// synthesized variant of an earlier name. Dedup preserves order and
/// drops the redundant candidate a name like `foo-symbolic` produces.
fn icon_candidates(names: &[&str], prefer: Variant) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::with_capacity(names.len() * 2);
    let want_symbolic = matches!(prefer, Variant::Symbolic);
    for preferred_pass in [true, false] {
        for name in names {
            let (symbolic, color) = name_variants(name);
            let candidate = if want_symbolic == preferred_pass {
                symbolic
            } else {
                color
            };
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_variants_splits_symbolic_and_color() {
        assert_eq!(
            name_variants("edit-copy"),
            ("edit-copy-symbolic".into(), "edit-copy".into())
        );
        assert_eq!(
            name_variants("help-contextual-symbolic"),
            ("help-contextual-symbolic".into(), "help-contextual".into())
        );
    }

    #[test]
    fn symbolic_first_prefers_symbolic_across_all_names() {
        // Every name's symbolic form is tried before any plain form, so
        // a theme that only ships symbolics resolves the first name.
        assert_eq!(
            icon_candidates(&["document-edit", "edit"], Variant::Symbolic),
            [
                "document-edit-symbolic",
                "edit-symbolic",
                "document-edit",
                "edit"
            ]
        );
    }

    #[test]
    fn color_first_prefers_plain_across_all_names() {
        // The colored form of a later fallback (video-display) wins over
        // a synthesized symbolic of an earlier name (video-television).
        assert_eq!(
            icon_candidates(&["video-television", "video-display"], Variant::Color),
            [
                "video-television",
                "video-display",
                "video-television-symbolic",
                "video-display-symbolic",
            ]
        );
    }

    #[test]
    fn already_symbolic_name_does_not_double_suffix() {
        // A `-symbolic` name yields itself (once) plus its stripped base;
        // no `foo-symbolic-symbolic`, and dedup keeps each form once.
        assert_eq!(
            icon_candidates(
                &["network-connect-symbolic", "network-connect"],
                Variant::Symbolic
            ),
            ["network-connect-symbolic", "network-connect"]
        );
    }
}

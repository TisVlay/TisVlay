//! Couch mode: the GUI's layout for a TV across the room, driven by a
//! remote. Maximized, scaled up on top of the display's own scale, and
//! with the keys adapted for a 10-foot session.
//!
//! The preference is the GUI's own file, `gui.toml` in the user's
//! config directory, the first state the GUI keeps for itself; nothing
//! on the wire. `Auto` follows the session's own signal, so Plasma
//! Bigscreen gets the layout without a setting, and the file lets a
//! plain desktop driving a TV ask for it. The GUI is D-Bus-activated and
//! takes no arguments, so the override for testing is an environment
//! variable rather than a flag.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

/// The environment override: `auto`, `on` or `off`. Anything else is
/// ignored with a warning.
pub(crate) const ENV_OVERRIDE: &str = "TISVLAY_COUCH_MODE";

/// The scale on top of the compositor's when nothing else is set:
/// Plasma Bigscreen's body font over the GUI's.
pub(crate) const DEFAULT_SCALE: f32 = 1.4;

/// Whether couch mode follows the session or is forced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CouchMode {
    #[default]
    Auto,
    On,
    Off,
}

impl CouchMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// The couch-mode preference.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct CouchPrefs {
    pub(crate) mode: CouchMode,
    /// The scale on top of the compositor's.
    pub(crate) scale: f32,
}

impl Default for CouchPrefs {
    fn default() -> Self {
        Self {
            mode: CouchMode::Auto,
            scale: DEFAULT_SCALE,
        }
    }
}

impl CouchPrefs {
    /// The scale within the Settings control's bounds (in percent):
    /// the file is hand-editable, so a value outside them is clamped,
    /// and one that isn't a number (TOML's `nan`, `inf`) is the default.
    pub(crate) fn sanitized(self, min_percent: i32, max_percent: i32) -> Self {
        debug_assert!(
            0 < min_percent && min_percent <= max_percent,
            "the scale's bounds are a positive, ordered range (the Couch global's)"
        );
        let scale = if self.scale.is_finite() {
            self.scale
                .clamp(min_percent as f32 / 100.0, max_percent as f32 / 100.0)
        } else {
            DEFAULT_SCALE
        };
        // Whole percents, as the control shows and the hook applies.
        let scale = (scale * 100.0).round() / 100.0;
        Self { scale, ..self }
    }
}

/// `gui.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct GuiConfig {
    version: u32,
    #[serde(default)]
    couch: CouchPrefs,
}

const SCHEMA_VERSION: u32 = 1;

/// Where `gui.toml` lives: the user's config directory.
pub(crate) fn default_config_path() -> PathBuf {
    directories::ProjectDirs::from("", "", "tisvlay")
        .map(|dirs| dirs.config_dir().join("gui.toml"))
        .unwrap_or_else(|| PathBuf::from("tisvlay-gui.toml"))
}

/// The preference on disk, or the default when the file is missing. An
/// unreadable file is reported and read as the default, since the GUI
/// must still open, and set aside: the next save would overwrite it.
pub(crate) fn load(path: &Path) -> CouchPrefs {
    if matches!(path.try_exists(), Ok(false)) {
        return CouchPrefs::default();
    }
    match tisvlay_runtime::fs::read_toml::<GuiConfig>(path) {
        Ok(config) => config.couch,
        Err(e) => {
            match tisvlay_runtime::fs::set_aside(path) {
                Ok(aside) => warn!(
                    error = %e,
                    file = %path.display(),
                    aside = %aside.display(),
                    "couch preference file unreadable; set aside"
                ),
                Err(aside_error) => warn!(
                    error = %e,
                    file = %path.display(),
                    %aside_error,
                    "couch preference file unreadable, and couldn't be set aside"
                ),
            }
            CouchPrefs::default()
        }
    }
}

/// Write the preference durably, keeping a hand edit's comments.
pub(crate) fn save(path: &Path, prefs: CouchPrefs) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let config = GuiConfig {
        version: SCHEMA_VERSION,
        couch: prefs,
    };
    tisvlay_runtime::fs::write_toml(path, &config).map(|_| ())
}

/// The 10-foot-session signals a desktop exposes to its apps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionSignals {
    /// Plasma's platform list, `PLASMA_PLATFORM`: Bigscreen sets it to
    /// `mediacenter`, the key its own apps pick their TV layouts by.
    pub(crate) plasma_platform: Option<String>,
}

impl SessionSignals {
    pub(crate) fn from_env() -> Self {
        Self {
            plasma_platform: std::env::var("PLASMA_PLATFORM").ok(),
        }
    }
}

/// Whether the session is a 10-foot one. `PLASMA_PLATFORM` is a
/// colon-separated list, so a component is matched, not the whole.
pub(crate) fn is_ten_foot_session(signals: &SessionSignals) -> bool {
    signals
        .plasma_platform
        .as_deref()
        .is_some_and(|list| list.split(':').any(|item| item == "mediacenter"))
}

/// Whether couch mode is on: the override wins, then the preference,
/// and `Auto` follows the session.
pub(crate) fn active(
    mode: CouchMode,
    env_override: Option<&str>,
    signals: &SessionSignals,
) -> bool {
    let mode = match env_override {
        Some(text) => match CouchMode::parse(text) {
            Some(forced) => forced,
            None => {
                warn!(
                    value = text,
                    "{ENV_OVERRIDE} isn't auto, on or off; ignored"
                );
                mode
            }
        },
        None => mode,
    };
    match mode {
        CouchMode::On => true,
        CouchMode::Off => false,
        CouchMode::Auto => is_ten_foot_session(signals),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bigscreen() -> SessionSignals {
        SessionSignals {
            plasma_platform: Some("mediacenter".into()),
        }
    }

    #[test]
    fn auto_follows_the_session() {
        assert!(active(CouchMode::Auto, None, &bigscreen()));
        assert!(!active(CouchMode::Auto, None, &SessionSignals::default()));
        let desktop = SessionSignals {
            plasma_platform: Some("desktop".into()),
        };
        assert!(!active(CouchMode::Auto, None, &desktop));
    }

    #[test]
    fn a_platform_list_matches_by_component() {
        let listed = SessionSignals {
            plasma_platform: Some("phone:mediacenter".into()),
        };
        assert!(is_ten_foot_session(&listed));
        let prefix = SessionSignals {
            plasma_platform: Some("mediacenter-ish".into()),
        };
        assert!(!is_ten_foot_session(&prefix));
    }

    #[test]
    fn the_preference_forces_either_way() {
        assert!(active(CouchMode::On, None, &SessionSignals::default()));
        assert!(!active(CouchMode::Off, None, &bigscreen()));
    }

    #[test]
    fn the_override_wins_and_nonsense_is_ignored() {
        assert!(active(
            CouchMode::Off,
            Some("on"),
            &SessionSignals::default()
        ));
        assert!(!active(CouchMode::On, Some("off"), &bigscreen()));
        assert!(active(CouchMode::Off, Some("auto"), &bigscreen()));
        assert!(active(
            CouchMode::On,
            Some("sideways"),
            &SessionSignals::default()
        ));
    }

    #[test]
    fn the_preference_round_trips_and_a_missing_file_is_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("gui.toml");
        assert_eq!(load(&path), CouchPrefs::default());
        let prefs = CouchPrefs {
            mode: CouchMode::On,
            scale: 1.8,
        };
        save(&path, prefs).unwrap();
        assert_eq!(load(&path), prefs);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mode = \"on\""), "{text}");
    }

    #[test]
    fn an_unreadable_file_reads_as_the_default_and_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gui.toml");
        std::fs::write(&path, "version = = 1\n").unwrap();
        assert_eq!(load(&path), CouchPrefs::default());
        assert!(
            !path.exists(),
            "the unreadable file is out of the next save's way"
        );
        assert!(dir.path().join("gui.toml.edited").exists());
    }

    #[test]
    fn a_scale_outside_the_controls_bounds_is_clamped_and_nonsense_is_the_default() {
        let percent = |scale: f32| {
            let prefs = CouchPrefs {
                mode: CouchMode::On,
                scale,
            }
            .sanitized(100, 250);
            assert_eq!(prefs.mode, CouchMode::On, "only the scale is touched");
            (prefs.scale * 100.0).round() as i32
        };
        assert_eq!(percent(0.0), 100);
        assert_eq!(percent(-1.0), 100);
        assert_eq!(percent(1e9), 250);
        assert_eq!(percent(1.8), 180);
        let default = (DEFAULT_SCALE * 100.0).round() as i32;
        assert_eq!(percent(f32::NAN), default);
        assert_eq!(percent(f32::INFINITY), default);
    }
}

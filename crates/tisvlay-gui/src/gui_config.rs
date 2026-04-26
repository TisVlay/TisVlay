//! GUI-local user preferences (appearance, etc.).
//!
//! Stored at `~/.config/tisvlay/gui.toml`. Distinct from the daemon's
//! `config.toml` because these settings only affect this app's chrome
//! and shouldn't round-trip through D-Bus.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// User-facing appearance preference. Mirrors `AppearanceMode` in
/// `ui/theme.slint`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct GuiConfig {
    #[serde(default)]
    pub appearance: AppearanceMode,
}

impl GuiConfig {
    pub fn path() -> Result<PathBuf> {
        let dirs = directories::ProjectDirs::from("", "", "tisvlay")
            .context("Could not determine config directory")?;
        Ok(dirs.config_dir().join("gui.toml"))
    }

    /// Read from disk. A missing file or parse error yields defaults —
    /// the GUI must always start, regardless of config-file health.
    pub fn load_or_default() -> Self {
        let Ok(path) = Self::path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all({})", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialize gui config")?;
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

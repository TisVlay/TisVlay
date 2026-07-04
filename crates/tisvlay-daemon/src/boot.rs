//! Boot config: the TOML file that seeds TV entities at daemon start.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use uuid::Uuid;

#[derive(Deserialize, Debug, Default)]
pub struct BootConfig {
    #[serde(default)]
    pub tv: Vec<BootTv>,
}

#[derive(Deserialize, Debug)]
pub struct BootTv {
    /// Stable id; omitted → generated fresh each boot (fine for mock play,
    /// wrong for real TVs — persistence is a later milestone).
    pub id: Option<Uuid>,
    pub name: String,
    pub vendor: String,
    #[serde(default = "default_true")]
    pub managed: bool,
    /// Vendor config table, passed to the backend verbatim.
    pub config: toml::Value,
}

fn default_true() -> bool {
    true
}

pub fn load_boot_config(path: &Path) -> Result<BootConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read boot config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse boot config {}", path.display()))
}

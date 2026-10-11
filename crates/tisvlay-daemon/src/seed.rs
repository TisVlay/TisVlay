//! Seed config: the TOML file that declaratively provisions TVs at
//! daemon start.
//!
//! Provisioning semantics (the Grafana/Home-Assistant shape, and the
//! NixOS module's contract): the seed re-applies on **every** start,
//! and an entry lands only when its id is absent from the daemon's
//! own file — so already-provisioned TVs keep their runtime edits,
//! new declarations appear on the next restart, and a still-declared
//! TV that was removed at runtime comes back (the declaration is
//! authoritative; drop the entry to drop the TV).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use tisvlay_core::VendorConfig;
use uuid::Uuid;

#[derive(Deserialize, Debug, Default)]
pub struct SeedConfig {
    #[serde(default)]
    pub tv: Vec<SeedTv>,
}

#[derive(Deserialize, Debug)]
pub struct SeedTv {
    /// Stable id; omitted → derived deterministically from the entry
    /// itself (see [`SeedTv::stable_id`]).
    pub id: Option<Uuid>,
    pub name: String,
    pub vendor: String,
    #[serde(default = "default_true")]
    pub managed: bool,
    /// Vendor config table, passed to the backend verbatim.
    pub config: VendorConfig,
}

impl SeedTv {
    /// The entry's identity for seed-once matching. A declared `id`
    /// wins; otherwise a UUIDv5 over the entry's content, so the same
    /// declaration maps to the same TV across restarts (a fresh id
    /// per boot would re-seed a persisted daemon into duplicates).
    /// Editing a declared entry changes its derived identity — that
    /// reads as remove-old + add-new, which is the honest projection
    /// of "the declaration changed" without a declared id.
    pub fn stable_id(&self) -> Uuid {
        if let Some(id) = self.id {
            return id;
        }
        let config = toml::Value::try_from(&self.config)
            .expect("a vendor config is a TOML table, which always serializes");
        let key = format!("tisvlay-seed:{}:{}:{config}", self.name, self.vendor);
        Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes())
    }
}

fn default_true() -> bool {
    true
}

pub fn load_seed_config(path: &Path) -> Result<SeedConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read seed config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse seed config {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_deterministic_and_content_keyed() {
        let parse =
            |text: &str| -> SeedTv { toml::from_str::<SeedConfig>(text).unwrap().tv.remove(0) };
        let a = parse(
            "[[tv]]\nname = \"Den\"\nvendor = \"mock\"\nconfig = { address = \"mock://den\" }\n",
        );
        let b = parse(
            "[[tv]]\nname = \"Den\"\nvendor = \"mock\"\nconfig = { address = \"mock://den\" }\n",
        );
        assert_eq!(
            a.stable_id(),
            b.stable_id(),
            "same declaration must map to the same TV across boots"
        );

        let c = parse(
            "[[tv]]\nname = \"Den\"\nvendor = \"mock\"\nconfig = { address = \"mock://attic\" }\n",
        );
        assert_ne!(
            a.stable_id(),
            c.stable_id(),
            "content change is a new identity"
        );

        let declared = parse(
            "[[tv]]\nid = \"9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40\"\nname = \"Den\"\nvendor = \"mock\"\nconfig = { address = \"mock://den\" }\n",
        );
        assert_eq!(
            declared.stable_id(),
            "9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40"
                .parse::<Uuid>()
                .unwrap(),
            "declared id wins"
        );
    }
}

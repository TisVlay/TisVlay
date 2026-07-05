//! State persistence: TOML on disk, debounced writes, atomic rename.
//! Credentials live here (vendor config verbatim) and only here.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tisvlay_world::StoreSnapshot;
use tokio::sync::mpsc;
use tracing::{debug, error};

/// Load the store; a missing file is an empty store.
pub fn load_store(path: &Path) -> Result<StoreSnapshot> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("parse store {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreSnapshot::default()),
        Err(e) => Err(e).with_context(|| format!("read store {}", path.display())),
    }
}

/// Debounce window between a snapshot arriving and the write landing.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Consume snapshots, coalescing bursts; every write is atomic
/// (tmp + same-dir rename). The final pending snapshot flushes when the
/// channel closes.
pub(crate) async fn store_task(path: PathBuf, mut rx: mpsc::UnboundedReceiver<StoreSnapshot>) {
    while let Some(mut snapshot) = rx.recv().await {
        loop {
            match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                Ok(Some(newer)) => snapshot = newer,
                Ok(None) => {
                    write_atomic(&path, &snapshot);
                    return;
                }
                Err(_) => break,
            }
        }
        write_atomic(&path, &snapshot);
    }
}

fn write_atomic(path: &Path, snapshot: &StoreSnapshot) {
    let text = match toml::to_string_pretty(snapshot) {
        Ok(text) => text,
        Err(e) => {
            error!(%e, "store serialize failed");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        error!(%e, "store dir create failed");
        return;
    }
    let tmp = path.with_extension("toml.tmp");
    if let Err(e) = std::fs::write(&tmp, text) {
        error!(%e, "store tmp write failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        error!(%e, "store rename failed");
        return;
    }
    debug!(store = %path.display(), tvs = snapshot.tv.len(), profiles = snapshot.system_profile.len(), "store written");
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_world::TvRecord;
    use uuid::Uuid;

    #[test]
    fn store_round_trips_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        let snapshot = StoreSnapshot {
            version: 1,
            tv: vec![TvRecord {
                id: Uuid::new_v4(),
                name: "Den".into(),
                vendor: "mock".into(),
                managed: true,
                system_profile: Some(Uuid::new_v4()),
                primary_input: Some("HDMI_1".into()),
                automate_when: "always".into(),
                config: toml::Value::Table(toml::value::Table::from_iter([(
                    "address".to_string(),
                    toml::Value::String("mock://den".into()),
                )])),
            }],
            system_profile: vec![tisvlay_world::ProfileRecord {
                id: Uuid::new_v4(),
                body: tisvlay_core::SystemProfileConfig {
                    name: "Cinema".into(),
                    ..Default::default()
                },
            }],
        };
        write_atomic(&path, &snapshot);
        let loaded = load_store(&path).unwrap();
        assert_eq!(loaded, snapshot);
        assert!(!path.with_extension("toml.tmp").exists(), "tmp cleaned up");
    }

    #[test]
    fn missing_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_store(&dir.path().join("nope.toml")).unwrap(),
            StoreSnapshot::default()
        );
    }
}

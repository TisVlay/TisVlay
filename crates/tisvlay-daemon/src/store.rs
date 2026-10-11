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
                    write_blocking(&path, snapshot).await;
                    return;
                }
                Err(_) => break,
            }
        }
        write_blocking(&path, snapshot).await;
    }
}

/// [`write_atomic`] off the async runtime: the fsyncs block.
async fn write_blocking(path: &Path, snapshot: StoreSnapshot) {
    let path = path.to_path_buf();
    if let Err(e) = tokio::task::spawn_blocking(move || write_atomic(&path, &snapshot)).await {
        error!(%e, "store write task failed");
    }
}

/// Serialize and durably replace the store file (`write_atomic`:
/// temp file, fsync, rename). Blocking; the store task runs it on
/// `spawn_blocking`.
fn write_atomic(path: &Path, snapshot: &StoreSnapshot) {
    let text = match toml::to_string_pretty(snapshot) {
        Ok(text) => text,
        Err(e) => {
            error!(%e, "store serialize failed");
            return;
        }
    };
    match tisvlay_runtime::fs::write_atomic(path, text.as_bytes()) {
        Ok(()) => {
            debug!(store = %path.display(), tvs = snapshot.tv.len(), profiles = snapshot.system_profile.len(), "store written");
        }
        Err(e) => error!(%e, store = %path.display(), "store write failed"),
    }
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
                config: toml::Table::from_iter([(
                    "address".to_string(),
                    toml::Value::String("mock://den".into()),
                )])
                .into(),
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

    /// The LG client key lives only in the store: it survives a load and
    /// re-save verbatim, while the loaded record's `Debug` names the key
    /// without showing it.
    #[test]
    fn store_keeps_the_client_key_out_of_debug() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        std::fs::write(
            &path,
            r#"
[[tv]]
id = "9a3f1b22-c4d8-4e90-b21f-7f1a2c8e5b40"
name = "Living Room"
vendor = "lg"
managed = true

[tv.config]
ip = "192.0.2.10"
client_key = "lg-client-key-1234"
scheme = "wss"
"#,
        )
        .unwrap();

        let loaded = load_store(&path).unwrap();
        let shown = format!("{loaded:?}");
        assert!(shown.contains("client_key"), "{shown}");
        assert!(!shown.contains("lg-client-key-1234"), "{shown}");

        write_atomic(&path, &loaded);
        let reloaded = load_store(&path).unwrap();
        let lg: tisvlay_lg::LgVendorConfig = reloaded.tv[0].config.parse("lg").unwrap();
        assert_eq!(
            lg.client_key.as_ref().map(tisvlay_core::Secret::expose),
            Some("lg-client-key-1234")
        );
        assert_eq!(reloaded, loaded);
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

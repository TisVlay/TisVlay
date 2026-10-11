//! State persistence: TOML on disk, debounced writes, atomic rename.
//! Credentials live here (vendor config verbatim) and only here.
//!
//! The store writer owns the record of what's on disk: the snapshot it
//! last wrote, or loaded at boot. A reload compares the file with that
//! record, never with what the world last sent, since a sent snapshot can
//! still be waiting out the debounce.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tisvlay_wire::endpoint::manager;
use tisvlay_world::{Command, Reply, StoreEdit, StoreMsg, StoreSnapshot, WireError, WorldClient};
use tokio::sync::mpsc;
use tokio::time::Instant;
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

/// The store file as a reload sees it: unlike boot, a missing file is
/// refused rather than read as empty, which would remove every TV.
fn read_for_reload(path: &Path) -> Result<StoreSnapshot, WireError> {
    let text = std::fs::read_to_string(path).map_err(|e| WireError::store_unreadable(path, e))?;
    toml::from_str(&text).map_err(|e| WireError::store_unreadable(path, e))
}

/// Debounce window between a snapshot arriving and the write landing.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// A request to apply a hand edit of the store file (the reload rpc, or
/// SIGHUP with no reply).
#[derive(Debug)]
pub(crate) struct ReloadRequest {
    pub reply: Option<Reply<manager::Reload>>,
}

/// Write the world's snapshots, coalescing bursts, and serve reloads.
/// `base` is the snapshot loaded at boot.
///
/// A reload reads the file and sends the world the edit since `base`.
/// Writes wait until the world answers, so a pending snapshot can't land
/// mid-reload; on success the file read becomes `base`. The final pending
/// snapshot flushes when the world's channel closes.
pub(crate) async fn store_task(
    path: PathBuf,
    mut base: StoreSnapshot,
    mut snapshots: mpsc::UnboundedReceiver<StoreMsg>,
    mut reloads: mpsc::UnboundedReceiver<ReloadRequest>,
    world: WorldClient,
) {
    let mut pending: Option<StoreSnapshot> = None;
    let mut due: Option<Instant> = None;
    let mut reloading = false;
    loop {
        tokio::select! {
            msg = snapshots.recv() => match msg {
                Some(StoreMsg::Persist(snapshot)) => {
                    pending = Some(*snapshot);
                    due = Instant::now().checked_add(DEBOUNCE);
                }
                Some(StoreMsg::Reloaded(applied)) => {
                    reloading = false;
                    if let Some(disk) = applied {
                        base = *disk;
                    }
                }
                None => {
                    if let Some(snapshot) = pending.take() {
                        write_blocking(&path, snapshot).await;
                    }
                    return;
                }
            },
            Some(request) = reloads.recv(), if !reloading => {
                let edit = read_for_reload(&path).map(|disk| {
                    Box::new(StoreEdit { base: base.clone(), disk })
                });
                reloading = edit.is_ok();
                world.command(Command::ReloadStore { edit, reply: request.reply });
            }
            () = async {
                tokio::time::sleep_until(due.expect("the branch is enabled only while due is Some")).await;
            }, if due.is_some() && !reloading => {
                due = None;
                if let Some(snapshot) = pending.take()
                    && let Some(written) = write_blocking(&path, snapshot).await
                {
                    base = written;
                }
            }
        }
    }
}

/// [`write_atomic`] off the async runtime, since the fsyncs block. Returns
/// the snapshot once it's on disk.
async fn write_blocking(path: &Path, snapshot: StoreSnapshot) -> Option<StoreSnapshot> {
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || write_atomic(&path, &snapshot).then_some(snapshot))
        .await
    {
        Ok(written) => written,
        Err(e) => {
            error!(%e, "store write task failed");
            None
        }
    }
}

/// Serialize and durably replace the store file (`write_atomic`:
/// temp file, fsync, rename). Blocking; the store task runs it on
/// `spawn_blocking`. Returns whether the file was written.
fn write_atomic(path: &Path, snapshot: &StoreSnapshot) -> bool {
    let text = match toml::to_string_pretty(snapshot) {
        Ok(text) => text,
        Err(e) => {
            error!(%e, "store serialize failed");
            return false;
        }
    };
    match tisvlay_runtime::fs::write_atomic(path, text.as_bytes()) {
        Ok(()) => {
            debug!(store = %path.display(), tvs = snapshot.tv.len(), profiles = snapshot.system_profile.len(), "store written");
            true
        }
        Err(e) => {
            error!(%e, store = %path.display(), "store write failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_world::TvRecord;
    use uuid::Uuid;

    /// A snapshot arriving mid-reload waits for the world's answer, so it
    /// can't overwrite the file the reload is applying.
    #[tokio::test]
    async fn writes_wait_for_a_reload_to_finish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.toml");
        let base = StoreSnapshot::default();
        assert!(write_atomic(&path, &base));
        let (_reactor, _effects, world) = tisvlay_world::Reactor::new("t", Vec::new());
        let (persist_tx, persist_rx) = mpsc::unbounded_channel();
        let (reload_tx, reload_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(store_task(
            path.clone(),
            base.clone(),
            persist_rx,
            reload_rx,
            world,
        ));
        reload_tx.send(ReloadRequest { reply: None }).unwrap();
        let mut pending = StoreSnapshot::default();
        pending.tv.push(TvRecord {
            id: Uuid::new_v4(),
            name: "Pending".into(),
            vendor: "mock".into(),
            managed: true,
            system_profile: None,
            primary_input: None,
            config: tisvlay_core::VendorConfig::new(),
        });
        persist_tx
            .send(StoreMsg::Persist(Box::new(pending)))
            .unwrap();
        tokio::time::sleep(DEBOUNCE * 2).await;
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("Pending"),
            "no write while the reload is outstanding"
        );

        persist_tx
            .send(StoreMsg::Reloaded(Some(Box::new(base))))
            .unwrap();
        drop(persist_tx);
        task.await.unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("Pending"));
    }

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

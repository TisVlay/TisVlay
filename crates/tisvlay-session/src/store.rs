//! User-side persistence: profiles + per-TV assignments, TOML on disk,
//! debounced atomic writes — the daemon store's shape at session scale.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tisvlay_core::UserProfileConfig;
use tisvlay_wire::error::WireError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tracing::{debug, error, warn};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserStoreSnapshot {
    /// Schema version — absence *defines* version 1 (the pre-tag
    /// baseline); migration logic waits until a v2 exists.
    #[serde(default = "schema_v1")]
    pub version: u32,
    #[serde(default)]
    pub profile: Vec<UserProfileRecord>,
    #[serde(default)]
    pub assignment: Vec<Assignment>,
}

fn schema_v1() -> u32 {
    1
}

impl Default for UserStoreSnapshot {
    fn default() -> Self {
        Self {
            version: schema_v1(),
            profile: Vec::new(),
            assignment: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UserProfileRecord {
    pub id: Uuid,
    pub body: UserProfileConfig,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub tv: Uuid,
    pub profile: Uuid,
}

/// Load the store; a missing file is an empty store.
pub fn load_user_store(path: &Path) -> Result<UserStoreSnapshot> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("parse user store {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserStoreSnapshot::default()),
        Err(e) => Err(e).with_context(|| format!("read user store {}", path.display())),
    }
}

/// Debounce window between a snapshot arriving and the write landing.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// A hand edit of the store file: the snapshot the writer last wrote (or
/// loaded at boot), and the file as it reads now. A reload applies only
/// the difference, so changes still awaiting their write survive.
#[derive(Debug)]
pub(crate) struct UserStoreEdit {
    pub base: UserStoreSnapshot,
    pub disk: UserStoreSnapshot,
}

/// What the core sends the store writer.
#[derive(Debug)]
pub(crate) enum StoreMsg {
    /// The state changed: write it.
    Persist(UserStoreSnapshot),
    /// A reload finished: `Some` carries the file it applied (now the
    /// writer's record of what's on disk), `None` a refusal.
    Reloaded(Option<UserStoreSnapshot>),
}

/// A reload request: the writer answers with the edit since its record,
/// or why the file can't be read, and holds its writes until the core
/// reports the outcome.
#[derive(Debug)]
pub(crate) struct ReloadRequest {
    pub reply: oneshot::Sender<Result<UserStoreEdit, WireError>>,
}

/// The store file as a reload sees it: a missing file is refused rather
/// than read as empty, which would drop every profile.
fn read_for_reload(path: &Path) -> Result<UserStoreSnapshot, WireError> {
    let text = std::fs::read_to_string(path).map_err(|e| WireError::store_unreadable(path, e))?;
    toml::from_str(&text).map_err(|e| WireError::store_unreadable(path, e))
}

/// Write the core's snapshots, coalescing bursts, and serve reloads.
/// `base` is the file as loaded at boot: the writer's record of what's on
/// disk, which a reload compares the file against. The final pending
/// snapshot flushes when the core's channel closes.
pub(crate) async fn store_task(
    path: PathBuf,
    mut base: UserStoreSnapshot,
    mut snapshots: mpsc::UnboundedReceiver<StoreMsg>,
    mut reloads: mpsc::UnboundedReceiver<ReloadRequest>,
) {
    let mut pending: Option<UserStoreSnapshot> = None;
    let mut due: Option<Instant> = None;
    let mut reloading = false;
    loop {
        tokio::select! {
            msg = snapshots.recv() => match msg {
                Some(StoreMsg::Persist(snapshot)) => {
                    pending = Some(snapshot);
                    due = Instant::now().checked_add(DEBOUNCE);
                }
                Some(StoreMsg::Reloaded(applied)) => {
                    reloading = false;
                    if let Some(disk) = applied {
                        base = disk;
                    }
                }
                None => {
                    if let Some(snapshot) = pending.take() {
                        write_blocking(&path, snapshot, base).await;
                    }
                    return;
                }
            },
            Some(request) = reloads.recv(), if !reloading => {
                let edit = read_for_reload(&path).map(|disk| UserStoreEdit {
                    base: base.clone(),
                    disk,
                });
                // Hold writes only if the core got an edit to report back on.
                let has_edit = edit.is_ok();
                reloading = request.reply.send(edit).is_ok() && has_edit;
            }
            () = async {
                tokio::time::sleep_until(due.expect("the branch is enabled only while due is Some")).await;
            }, if due.is_some() && !reloading => {
                due = None;
                if let Some(snapshot) = pending.take()
                    && let Some(written) = write_blocking(&path, snapshot, base.clone()).await
                {
                    base = written;
                }
            }
        }
    }
}

/// [`write_atomic`] off the async runtime, since the fsyncs block, after
/// keeping aside a hand edit nobody reloaded (the file no longer holds
/// `base`, what the writer last wrote). Returns the snapshot once it's on
/// disk.
async fn write_blocking(
    path: &Path,
    snapshot: UserStoreSnapshot,
    base: UserStoreSnapshot,
) -> Option<UserStoreSnapshot> {
    let path = path.to_path_buf();
    match tokio::task::spawn_blocking(move || {
        keep_unreloaded_edit(&path, &base);
        write_atomic(&path, &snapshot).then_some(snapshot)
    })
    .await
    {
        Ok(written) => written,
        Err(e) => {
            error!(%e, "user store write task failed");
            None
        }
    }
}

/// If the file no longer holds what the writer last wrote, someone edited
/// it and never reloaded: move it aside to `<path>.edited` (with a
/// warning) rather than overwrite the edit. A missing file has nothing to
/// keep.
fn keep_unreloaded_edit(path: &Path, base: &UserStoreSnapshot) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    if toml::from_str::<UserStoreSnapshot>(&text).is_ok_and(|disk| same_content(&disk, base)) {
        return;
    }
    match tisvlay_runtime::fs::set_aside(path) {
        Ok(aside) => warn!(
            store = %path.display(),
            aside = %aside.display(),
            "the user store held an edit nobody reloaded; kept it aside before writing"
        ),
        Err(e) => {
            error!(%e, store = %path.display(), "setting aside an unreloaded user store edit failed");
        }
    }
}

/// The same profiles and assignments, whatever their order in the file.
fn same_content(a: &UserStoreSnapshot, b: &UserStoreSnapshot) -> bool {
    fn sorted<T: Clone>(items: &[T], key: impl Fn(&T) -> Uuid) -> Vec<T> {
        let mut items = items.to_vec();
        items.sort_by_key(|item| key(item));
        items
    }
    sorted(&a.profile, |p| p.id) == sorted(&b.profile, |p| p.id)
        && sorted(&a.assignment, |a| a.tv) == sorted(&b.assignment, |a| a.tv)
}

/// Serialize and durably replace the store file (`write_atomic`:
/// temp file, fsync, rename). Blocking; the store task runs it on
/// `spawn_blocking`. Returns whether the file was written.
fn write_atomic(path: &Path, snapshot: &UserStoreSnapshot) -> bool {
    let text = match toml::to_string_pretty(snapshot) {
        Ok(text) => text,
        Err(e) => {
            error!(%e, "user store serialize failed");
            return false;
        }
    };
    match tisvlay_runtime::fs::write_atomic(path, text.as_bytes()) {
        Ok(()) => {
            debug!(store = %path.display(), profiles = snapshot.profile.len(), assignments = snapshot.assignment.len(), "user store written");
            true
        }
        Err(e) => {
            error!(%e, store = %path.display(), "user store write failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_store_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.toml");
        let profile = Uuid::new_v4();
        let snapshot = UserStoreSnapshot {
            version: 1,
            profile: vec![UserProfileRecord {
                id: profile,
                body: UserProfileConfig {
                    name: "Late night".into(),
                    on_session_lock: Some(tisvlay_core::SessionAction::None.into()),
                    ..Default::default()
                },
            }],
            assignment: vec![Assignment {
                tv: Uuid::new_v4(),
                profile,
            }],
        };
        write_atomic(&path, &snapshot);
        assert_eq!(load_user_store(&path).unwrap(), snapshot);
        assert!(!path.with_extension("toml.tmp").exists(), "tmp cleaned up");
    }

    #[test]
    fn missing_user_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_user_store(&dir.path().join("nope.toml")).unwrap(),
            UserStoreSnapshot::default()
        );
    }
}

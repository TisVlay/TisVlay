//! User-side persistence: profiles + per-TV assignments, TOML on disk,
//! debounced atomic writes — the daemon store's shape at session scale.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tisvlay_core::UserProfileConfig;
use tokio::sync::mpsc;
use tracing::{debug, error};
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

/// Consume snapshots, coalescing bursts; every write is atomic
/// (tmp + same-dir rename). The final pending snapshot flushes when the
/// channel closes.
pub(crate) async fn store_task(path: PathBuf, mut rx: mpsc::UnboundedReceiver<UserStoreSnapshot>) {
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

fn write_atomic(path: &Path, snapshot: &UserStoreSnapshot) {
    let text = match toml::to_string_pretty(snapshot) {
        Ok(text) => text,
        Err(e) => {
            error!(%e, "user store serialize failed");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        error!(%e, "user store dir create failed");
        return;
    }
    let tmp = path.with_extension("toml.tmp");
    if let Err(e) = std::fs::write(&tmp, text) {
        error!(%e, "user store tmp write failed");
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        error!(%e, "user store rename failed");
        return;
    }
    debug!(store = %path.display(), profiles = snapshot.profile.len(), assignments = snapshot.assignment.len(), "user store written");
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

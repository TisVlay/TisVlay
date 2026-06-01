//! Shared `/sys/class/drm` connector enumeration.
//!
//! Both the desktop-identity probe ([`crate::desktop_probe`]) and the
//! DPMS watcher ([`crate::dpms`]) walk `/sys/class/drm/` for connector
//! directories (`card0-HDMI-A-1`) and read per-connector sysfs
//! attributes. This module owns the directory walk and the
//! `cardN-<connector>` name derivation so the two callers can't drift on
//! the sysfs layout.

use std::path::{Path, PathBuf};

use tracing::debug;

/// A DRM connector directory under `/sys/class/drm`.
pub(crate) struct ConnectorDir {
    /// Bare connector name (`HDMI-A-1`), derived from the `cardN-<conn>`
    /// directory name.
    pub name: String,
    /// Absolute path to the connector directory.
    pub dir: PathBuf,
}

/// Enumerate connector directories under `root` (`/sys/class/drm`).
///
/// Only `cardN-<connector>` entries yield a connector; the bare `cardN`
/// and `renderD*` render nodes have no `-<connector>` tail and are
/// skipped. Returns empty when `root` is unreadable (sandbox / no DRM).
pub(crate) fn enumerate_connectors(root: &Path) -> Vec<ConnectorDir> {
    let Ok(entries) = std::fs::read_dir(root) else {
        debug!(root = %root.display(), "DRM sysfs not readable");
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(dir_name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some((_, name)) = dir_name.split_once('-') {
            out.push(ConnectorDir {
                name: name.to_string(),
                dir,
            });
        }
    }
    out
}

/// Read `<dir>/<attr>` as a trimmed string, or `None` if unreadable.
pub(crate) fn read_attr(dir: &Path, attr: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(attr))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Whether the connector's `status` attribute reads `connected`.
pub(crate) fn is_connected(dir: &Path) -> bool {
    read_attr(dir, "status").as_deref() == Some("connected")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_connector(root: &Path, dir_name: &str, status: &str) {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("status"), status).unwrap();
    }

    #[test]
    fn enumerate_skips_render_nodes_and_derives_bare_connector() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_connector(root, "card0-HDMI-A-1", "connected");
        write_connector(root, "card0-DP-2", "disconnected");
        // Render nodes have no `-<connector>` tail and must be skipped.
        fs::create_dir_all(root.join("card0")).unwrap();
        fs::create_dir_all(root.join("renderD128")).unwrap();

        let mut names: Vec<_> = enumerate_connectors(root)
            .into_iter()
            .map(|c| c.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["DP-2", "HDMI-A-1"]);
    }

    #[test]
    fn enumerate_unreadable_root_is_empty() {
        assert!(enumerate_connectors(Path::new("/nonexistent/sysfs/root")).is_empty());
    }

    #[test]
    fn is_connected_and_read_attr() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_connector(root, "card0-HDMI-A-1", "connected\n");
        write_connector(root, "card0-DP-1", "disconnected\n");

        let connected = root.join("card0-HDMI-A-1");
        let disconnected = root.join("card0-DP-1");
        assert!(is_connected(&connected));
        assert!(!is_connected(&disconnected));
        assert_eq!(
            read_attr(&connected, "status").as_deref(),
            Some("connected")
        );
        assert_eq!(read_attr(&connected, "missing"), None);
    }
}

//! Crash-safe file replacement for the services' state files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Replace `path` with `contents` atomically and durably: write a sibling
/// temp file (mode 0600, since these files hold credentials), fsync it,
/// rename it over `path`, then fsync the directory so the rename itself
/// survives a power loss. A crash at any point leaves either the old file
/// or the new one, never a torn or empty one. The temp file is removed if
/// any step fails.
///
/// Blocking: async callers run it on `spawn_blocking`.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(dir)?;
    let tmp = temp_path(path)?;
    // A temp file left by a crashed write may carry other permissions;
    // `create_new` below only applies the mode to a fresh file.
    match fs::remove_file(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// `<path>.tmp`, beside the target so the rename stays on one filesystem.
fn temp_path(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path names no file"))?
        .to_owned();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replaces_the_file_owner_only_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/state.toml");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!dir.path().join("nested/state.toml.tmp").exists());
    }

    #[test]
    fn a_stale_temp_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        let stale = dir.path().join("state.toml.tmp");
        fs::write(&stale, b"stale").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, b"fresh").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"fresh");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_failed_write_removes_its_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        // Renaming a file over a directory fails after the temp is written.
        let path = dir.path().join("occupied");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("keep"), b"x").unwrap();
        assert!(write_atomic(&path, b"data").is_err());
        assert!(!dir.path().join("occupied.tmp").exists());
    }
}

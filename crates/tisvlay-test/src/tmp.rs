//! Temp dirs that can hold Unix sockets.

use std::path::PathBuf;

use tempfile::TempDir;

/// Longest socket path the kernel accepts: `sun_path` is 108 bytes,
/// including the terminating NUL.
const SUN_PATH_MAX: usize = 107;

/// Room left after the base dir for the temp dir's own name plus the
/// socket file the rigs put in it (`/tvXXXXXX/session.sock` and similar),
/// with margin.
const SOCKET_ROOM: usize = 40;

/// A fresh temp dir short enough for the Unix sockets the test rigs bind
/// in it. `TMPDIR` when it leaves room under the `sun_path` limit, else
/// `/tmp`: a long `TMPDIR` (a nested CI or tool scratch dir) would push
/// every socket path past the limit and fail each bind.
pub fn socket_tempdir() -> TempDir {
    let base = std::env::temp_dir();
    let base = if base.as_os_str().len() + SOCKET_ROOM <= SUN_PATH_MAX {
        base
    } else {
        PathBuf::from("/tmp")
    };
    tempfile::Builder::new()
        .prefix("tv")
        .tempdir_in(&base)
        .unwrap_or_else(|e| panic!("socket temp dir under {}: {e}", base.display()))
}

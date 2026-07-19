//! POSIX access-ACL granting the login greeter's group access to the
//! daemon socket, applied at bind.
//!
//! The modern GDM greeter is a systemd-userdb dynamic user whose primary
//! group is `gdm` (`Debian-gdm` on Debian), so it cannot be reached via
//! `/etc/group` supplementary membership. Granting that group an ACL on
//! the socket (rw) and its runtime directory (traverse) is the one
//! mechanism that works across the static-account (NixOS) and dynamic
//! (Fedora/Ubuntu) greeters alike — it keys on the group, not the user,
//! matching how GDM's own polkit/D-Bus policies gate the greeter.
//!
//! Done in-process via the `system.posix_acl_access` xattr rather than by
//! linking libacl: a plain access ACL is a small, stable binary blob, and
//! *removing* the xattr reverts a file straight to its mode bits — which
//! is exactly the reconcile-on-restart behavior the preserved runtime
//! directory needs when the grant is turned off.

use std::ffi::CString;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use tracing::{debug, info, warn};

/// Greeter-group access policy for the daemon socket.
#[derive(Debug, Clone)]
pub enum GreeterAccess {
    /// No greeter ACL; any prior ACL is stripped back to mode bits.
    Disabled,
    /// Auto-detect the display manager's greeter group from the known
    /// set — the packaged single unit's mode, since the name differs by
    /// distro (`gdm` vs Debian's `Debian-gdm`).
    Auto,
    /// An explicit greeter group name, for a DM that knows its own
    /// (NixOS, or a plasma-login greeter whose group isn't auto-detected).
    Group(String),
}

/// Greeter groups tried under [`GreeterAccess::Auto`], in order.
const KNOWN_GREETER_GROUPS: &[&str] = &["gdm", "Debian-gdm"];

/// Null-terminated for a direct FFI `*const c_char`.
const ACL_XATTR: &[u8] = b"system.posix_acl_access\0";

// POSIX.1e ACL entry tags, in the kernel's on-disk xattr format. Entries
// must be emitted in ascending tag order.
const ACL_USER_OBJ: u16 = 0x01;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const ACL_UNDEFINED_ID: u32 = 0xffff_ffff;

/// Reconcile the greeter ACL on the socket and its runtime directory to
/// `access`. Best-effort: a failure denies the greeter, never the daemon,
/// so problems are logged rather than propagated. Applying on every
/// startup (and clearing when disabled) is what keeps the grant revocable
/// even though the runtime directory is preserved across restarts.
pub(crate) fn reconcile(socket: &Path, access: &GreeterAccess) {
    let Some(dir) = socket.parent() else {
        warn!("socket has no parent directory; skipping greeter ACL");
        return;
    };
    match resolve_gid(access) {
        Some(gid) => {
            apply(socket, gid);
            apply(dir, gid);
            info!(gid, socket = %socket.display(), "greeter-group socket ACL applied");
        }
        None => {
            // Disabled, or requested but absent: strip any prior ACL so
            // the (preserved) runtime directory reverts to its mode bits.
            clear(socket);
            clear(dir);
            match access {
                GreeterAccess::Disabled => {}
                // Absent on a non-GDM desktop is the normal case for the
                // packaged unit's blanket --greeter-access, so keep quiet.
                GreeterAccess::Auto => {
                    debug!("no known greeter group present; socket ACL not applied")
                }
                GreeterAccess::Group(name) => {
                    warn!(name, "greeter group not found; socket ACL not applied")
                }
            }
        }
    }
}

fn resolve_gid(access: &GreeterAccess) -> Option<u32> {
    match access {
        GreeterAccess::Disabled => None,
        GreeterAccess::Group(name) => gid_of(name),
        GreeterAccess::Auto => KNOWN_GREETER_GROUPS.iter().find_map(|g| gid_of(g)),
    }
}

/// Resolve a group name to its gid via the reentrant `getgrnam_r`: the
/// daemon is multi-threaded and its adapters touch NSS elsewhere, so the
/// static buffer behind plain `getgrnam` is unsafe to share.
fn gid_of(name: &str) -> Option<u32> {
    let cname = CString::new(name).ok()?;
    let mut buflen = 1024usize;
    loop {
        let mut buf = vec![0u8; buflen];
        // SAFETY: `libc::group` is a plain C struct; an all-zero value is
        // a valid empty initializer that getgrnam_r overwrites before any
        // field is read.
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: getgrnam_r fills `grp` and `result` out of `buf`; on a
        // missing group it returns 0 with a null `result`. `grp` is read
        // only when the call succeeds and `result` is non-null.
        let ret = unsafe {
            libc::getgrnam_r(
                cname.as_ptr(),
                &mut grp,
                buf.as_mut_ptr() as *mut libc::c_char,
                buflen,
                &mut result,
            )
        };
        if ret == 0 {
            return (!result.is_null()).then_some(grp.gr_gid);
        }
        if ret == libc::ERANGE && buflen < (1 << 20) {
            buflen *= 2;
            continue;
        }
        return None;
    }
}

/// Set the access ACL granting group `gid` the same permission the file's
/// owning group already has (rw on the 0660 socket, r-x on the 0750 dir),
/// derived from the current mode so it is never assumed. The mask matches,
/// so the owning `tisvlay` group keeps its effective access too.
fn apply(path: &Path, gid: u32) {
    let mode = match std::fs::metadata(path) {
        Ok(m) => m.mode(),
        Err(e) => {
            warn!(path = %path.display(), %e, "stat for greeter ACL failed");
            return;
        }
    };
    let user_obj = ((mode >> 6) & 7) as u16;
    let group_obj = ((mode >> 3) & 7) as u16;
    let other = (mode & 7) as u16;
    let entries = [
        (ACL_USER_OBJ, user_obj, ACL_UNDEFINED_ID),
        (ACL_GROUP_OBJ, group_obj, ACL_UNDEFINED_ID),
        (ACL_GROUP, group_obj, gid),
        (ACL_MASK, group_obj, ACL_UNDEFINED_ID),
        (ACL_OTHER, other, ACL_UNDEFINED_ID),
    ];
    let mut buf = Vec::with_capacity(4 + entries.len() * 8);
    buf.extend_from_slice(&2u32.to_le_bytes()); // POSIX ACL xattr version
    for (tag, perm, id) in entries {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&perm.to_le_bytes());
        buf.extend_from_slice(&id.to_le_bytes());
    }
    if let Err(e) = setxattr(path, &buf) {
        warn!(path = %path.display(), %e, "set greeter ACL xattr failed");
    }
}

/// Strip the access ACL, reverting the file to its mode bits. A file that
/// never had one yields `ENODATA`, which is success for our purposes.
fn clear(path: &Path) {
    if let Err(e) = removexattr(path)
        && e.raw_os_error() != Some(libc::ENODATA)
    {
        warn!(path = %path.display(), %e, "clear greeter ACL xattr failed");
    }
}

fn setxattr(path: &Path, value: &[u8]) -> std::io::Result<()> {
    let cpath = cpath(path)?;
    // SAFETY: the pointers/len describe `cpath`, the static xattr name,
    // and `value`, all valid for the call; flag 0 = create-or-replace.
    let ret = unsafe {
        libc::setxattr(
            cpath.as_ptr(),
            ACL_XATTR.as_ptr() as *const libc::c_char,
            value.as_ptr() as *const libc::c_void,
            value.len(),
            0,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn removexattr(path: &Path) -> std::io::Result<()> {
    let cpath = cpath(path)?;
    // SAFETY: both pointers reference valid C strings for the duration.
    let ret =
        unsafe { libc::removexattr(cpath.as_ptr(), ACL_XATTR.as_ptr() as *const libc::c_char) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn cpath(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))
}

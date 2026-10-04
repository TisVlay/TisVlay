//! POSIX access-ACL on the daemon socket: named grants beyond the owning
//! `tisvlay` group, applied at bind and reconciled at runtime.
//!
//! Two kinds of named entries share the one ACL:
//!
//! - **Greeter group** (`ACL_GROUP`): the modern GDM greeter is a
//!   systemd-userdb dynamic user whose primary group is `gdm`
//!   (`Debian-gdm` on Debian), so it cannot be reached via `/etc/group`
//!   supplementary membership. Granting that group an ACL on the socket
//!   (rw) and its runtime directory (traverse) is the one mechanism that
//!   works across the static-account (NixOS) and dynamic (Fedora/Ubuntu)
//!   greeters alike — it keys on the group, not the user, matching how
//!   GDM's own polkit/D-Bus policies gate the greeter.
//! - **Active-seat users** (`ACL_USER`): presence-scoped access for the
//!   human at the console — `uaccess` semantics applied to our socket
//!   instead of a device node. The seat watch (`seat.rs`) feeds the uid
//!   set; the durable grant remains the `tisvlay` group.
//!
//! Done in-process via the `system.posix_acl_access` xattr rather than by
//! linking libacl: a plain access ACL is a small, stable binary blob, and
//! *removing* the xattr reverts a file straight to its mode bits — which
//! is exactly the reconcile-on-restart behavior the preserved runtime
//! directory needs when a grant is turned off.

#![expect(
    unsafe_code,
    reason = "libc xattr calls that read and write the socket's POSIX ACL, and getgrnam_r to resolve group names"
)]

use std::collections::BTreeSet;
use std::ffi::CString;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{debug, info, warn};

/// Greeter-group access policy for the daemon socket.
#[derive(Debug, Clone)]
pub enum GreeterAccess {
    /// No greeter ACL; any prior greeter entry is dropped.
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
// must be emitted in ascending tag order, and named entries ascending by
// qualifier id within a tag.
const ACL_USER_OBJ: u16 = 0x01;
const ACL_USER: u16 = 0x02;
const ACL_GROUP_OBJ: u16 = 0x04;
const ACL_GROUP: u16 = 0x08;
const ACL_MASK: u16 = 0x10;
const ACL_OTHER: u16 = 0x20;
const ACL_UNDEFINED_ID: u32 = 0xffff_ffff;

/// Reconcile the ACL on the socket and its runtime directory to the
/// desired grants: the greeter group per `greeter` plus a named-user entry
/// per active-seat uid. Best-effort: a failure denies the grantee, never
/// the daemon, so problems are logged rather than propagated. Rebuilding
/// the whole ACL from desired state on every call (and stripping it when
/// nothing is granted) is what keeps grants revocable even though the
/// runtime directory is preserved across restarts. Writes only on drift
/// — the current xattr is read back and compared first — so callers can
/// reconcile on a timer to repair failed or externally-clobbered writes
/// without log spam or churn.
pub(crate) fn reconcile(socket: &Path, greeter: &GreeterAccess, seat_uids: &BTreeSet<u32>) {
    let Some(dir) = socket.parent() else {
        warn!("socket has no parent directory; skipping socket ACL");
        return;
    };
    let gid = resolve_gid(greeter);
    if gid.is_none() && seat_uids.is_empty() {
        // Nothing granted: strip any prior ACL so the (preserved) runtime
        // directory reverts to its mode bits.
        let socket_stripped = clear(socket);
        let dir_stripped = clear(dir);
        if socket_stripped || dir_stripped {
            info!(socket = %socket.display(), "socket ACL cleared");
        }
        return;
    }
    let socket_changed = apply(socket, gid, seat_uids);
    let dir_changed = apply(dir, gid, seat_uids);
    if socket_changed || dir_changed {
        info!(?gid, ?seat_uids, socket = %socket.display(), "socket ACL applied");
    }
}

/// One warn per process for a configured-but-absent greeter group: the
/// seat watch reconciles on every seat event, and re-warning the same
/// static misconfiguration at each login/logout would bury real warns.
static MISSING_GROUP_WARNED: AtomicBool = AtomicBool::new(false);

fn resolve_gid(access: &GreeterAccess) -> Option<u32> {
    match access {
        GreeterAccess::Disabled => None,
        GreeterAccess::Group(name) => {
            let gid = gid_of(name);
            if gid.is_none() && !MISSING_GROUP_WARNED.swap(true, Ordering::Relaxed) {
                warn!(name, "greeter group not found; no greeter socket ACL");
            }
            gid
        }
        GreeterAccess::Auto => {
            let gid = KNOWN_GREETER_GROUPS.iter().find_map(|g| gid_of(g));
            if gid.is_none() {
                // Absent on a non-GDM desktop is the normal case for the
                // packaged unit's blanket --greeter-access, so keep quiet.
                debug!("no known greeter group present; no greeter socket ACL");
            }
            gid
        }
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
                &raw mut grp,
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buflen,
                &raw mut result,
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

/// Set the access ACL granting each named user/group the same permission
/// the file's owning group already has (rw on the 0660 socket, r-x on the
/// 0750 dir), derived from the current mode so it is never assumed. Full
/// class perms on every named entry are load-bearing: ACL evaluation is
/// first-match-by-class, so a narrower named-user entry would *subtract*
/// access from a user who is also a group member. The mask matches, so the
/// owning `tisvlay` group keeps its effective access too.
///
/// Returns whether anything was written: the current xattr is compared
/// first, so an in-sync file is left untouched (and unlogged), while a
/// failed or externally-clobbered previous write reads as drift and is
/// repaired on the next call.
fn apply(path: &Path, gid: Option<u32>, uids: &BTreeSet<u32>) -> bool {
    let mode = match std::fs::metadata(path) {
        Ok(m) => m.mode(),
        Err(e) => {
            warn!(path = %path.display(), %e, "stat for socket ACL failed");
            return false;
        }
    };
    let desired = blob(mode, gid, uids);
    if getxattr(path).ok().flatten().as_deref() == Some(desired.as_slice()) {
        return false;
    }
    match setxattr(path, &desired) {
        Ok(()) => true,
        Err(e) => {
            warn!(path = %path.display(), %e, "set socket ACL xattr failed");
            false
        }
    }
}

/// The `system.posix_acl_access` value for `mode` plus the named grants.
fn blob(mode: u32, gid: Option<u32>, uids: &BTreeSet<u32>) -> Vec<u8> {
    let user_obj = ((mode >> 6) & 7) as u16;
    let group_obj = ((mode >> 3) & 7) as u16;
    let other = (mode & 7) as u16;
    let mut entries: Vec<(u16, u16, u32)> = Vec::with_capacity(5 + uids.len());
    entries.push((ACL_USER_OBJ, user_obj, ACL_UNDEFINED_ID));
    // BTreeSet iteration gives the ascending-uid order the kernel requires.
    for &uid in uids {
        entries.push((ACL_USER, group_obj, uid));
    }
    entries.push((ACL_GROUP_OBJ, group_obj, ACL_UNDEFINED_ID));
    if let Some(gid) = gid {
        entries.push((ACL_GROUP, group_obj, gid));
    }
    entries.push((ACL_MASK, group_obj, ACL_UNDEFINED_ID));
    entries.push((ACL_OTHER, other, ACL_UNDEFINED_ID));
    let mut buf = Vec::with_capacity(4 + entries.len() * 8);
    buf.extend_from_slice(&2u32.to_le_bytes()); // POSIX ACL xattr version
    for (tag, perm, id) in entries {
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&perm.to_le_bytes());
        buf.extend_from_slice(&id.to_le_bytes());
    }
    buf
}

/// Strip the access ACL, reverting the file to its mode bits. A file that
/// never had one yields `ENODATA`, which is success for our purposes.
/// Returns whether an ACL was actually removed.
fn clear(path: &Path) -> bool {
    match removexattr(path) {
        Ok(()) => true,
        Err(e) if e.raw_os_error() == Some(libc::ENODATA) => false,
        Err(e) => {
            warn!(path = %path.display(), %e, "clear socket ACL xattr failed");
            false
        }
    }
}

fn setxattr(path: &Path, value: &[u8]) -> std::io::Result<()> {
    let cpath = cpath(path)?;
    // SAFETY: the pointers/len describe `cpath`, the static xattr name,
    // and `value`, all valid for the call; flag 0 = create-or-replace.
    let ret = unsafe {
        libc::setxattr(
            cpath.as_ptr(),
            ACL_XATTR.as_ptr().cast::<libc::c_char>(),
            value.as_ptr().cast::<libc::c_void>(),
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
        unsafe { libc::removexattr(cpath.as_ptr(), ACL_XATTR.as_ptr().cast::<libc::c_char>()) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Read the current ACL xattr; `None` when the file has none.
fn getxattr(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    let cpath = cpath(path)?;
    let mut buf = vec![0u8; 256];
    loop {
        // SAFETY: the pointers/len describe `cpath`, the static xattr
        // name, and `buf`, all valid for the call.
        let n = unsafe {
            libc::getxattr(
                cpath.as_ptr(),
                ACL_XATTR.as_ptr().cast::<libc::c_char>(),
                buf.as_mut_ptr().cast::<libc::c_void>(),
                buf.len(),
            )
        };
        if n >= 0 {
            buf.truncate(n as usize);
            return Ok(Some(buf));
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::ENODATA) => return Ok(None),
            Some(libc::ERANGE) => {
                buf = vec![0u8; buf.len() * 2];
                continue;
            }
            _ => return Err(e),
        }
    }
}

fn cpath(path: &Path) -> std::io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a blob back into `(tag, perm, id)` entries for assertions.
    fn decode(blob: &[u8]) -> Vec<(u16, u16, u32)> {
        assert_eq!(&blob[..4], &2u32.to_le_bytes());
        blob[4..]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| {
                (
                    u16::from_le_bytes([c[0], c[1]]),
                    u16::from_le_bytes([c[2], c[3]]),
                    u32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                )
            })
            .collect()
    }

    #[test]
    fn blob_orders_tags_and_uids_ascending() {
        let uids = BTreeSet::from([1207, 42, 60578]);
        let entries = decode(&blob(0o660, Some(975), &uids));
        let tags: Vec<u16> = entries.iter().map(|e| e.0).collect();
        assert_eq!(
            tags,
            [
                ACL_USER_OBJ,
                ACL_USER,
                ACL_USER,
                ACL_USER,
                ACL_GROUP_OBJ,
                ACL_GROUP,
                ACL_MASK,
                ACL_OTHER
            ]
        );
        // Named users ascend by uid — the kernel rejects any other order.
        let named_uids: Vec<u32> = entries
            .iter()
            .filter(|e| e.0 == ACL_USER)
            .map(|e| e.2)
            .collect();
        assert_eq!(named_uids, [42, 1207, 60578]);
    }

    #[test]
    fn blob_named_entries_carry_the_group_class_perm() {
        // 0660 socket: named entries + mask must all be rw (6) — anything
        // narrower would subtract access via first-match-by-class.
        let entries = decode(&blob(0o660, Some(975), &BTreeSet::from([1000])));
        for (tag, perm, _) in &entries {
            match *tag {
                ACL_USER | ACL_GROUP | ACL_GROUP_OBJ | ACL_MASK => assert_eq!(*perm, 6),
                ACL_USER_OBJ => assert_eq!(*perm, 6),
                ACL_OTHER => assert_eq!(*perm, 0),
                other => panic!("unexpected tag {other:#x}"),
            }
        }
        // 0750 dir: named entries + mask are r-x (5).
        let entries = decode(&blob(0o750, Some(975), &BTreeSet::from([1000])));
        for (tag, perm, _) in &entries {
            if matches!(*tag, ACL_USER | ACL_GROUP | ACL_MASK) {
                assert_eq!(*perm, 5);
            }
        }
    }

    #[test]
    fn blob_without_greeter_group_has_user_entries_only() {
        let entries = decode(&blob(0o660, None, &BTreeSet::from([1000])));
        let tags: Vec<u16> = entries.iter().map(|e| e.0).collect();
        assert_eq!(
            tags,
            [ACL_USER_OBJ, ACL_USER, ACL_GROUP_OBJ, ACL_MASK, ACL_OTHER]
        );
    }

    #[test]
    fn greeter_only_blob_matches_the_original_five_entry_layout() {
        let entries = decode(&blob(0o660, Some(975), &BTreeSet::new()));
        assert_eq!(
            entries,
            [
                (ACL_USER_OBJ, 6, ACL_UNDEFINED_ID),
                (ACL_GROUP_OBJ, 6, ACL_UNDEFINED_ID),
                (ACL_GROUP, 6, 975),
                (ACL_MASK, 6, ACL_UNDEFINED_ID),
                (ACL_OTHER, 0, ACL_UNDEFINED_ID),
            ]
        );
    }

    /// Round-trip through the kernel: set the blob on a real file, read it
    /// back, and confirm the kernel accepted the layout (an invalid entry
    /// order fails at `setxattr` with `EINVAL`). Also proves the
    /// drift-repair contract: a differing stored ACL reads as a write, an
    /// in-sync one as a no-op.
    #[test]
    fn kernel_accepts_and_round_trips_the_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acl-target");
        std::fs::write(&path, b"").unwrap();
        let uids = BTreeSet::from([12, 34]);
        let value = blob(0o660, Some(7), &uids);
        match setxattr(&path, &value) {
            Ok(()) => {}
            // The environment refuses xattrs outright — nothing to verify
            // there; the layout stays covered by the blob tests. The Nix
            // build sandbox seccomp-filters the setxattr family (ENOTSUP)
            // to keep xattrs out of store paths; a container seccomp
            // policy may answer EPERM instead (we own the file, so EPERM
            // is the environment, not the blob).
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EPERM)) => return,
            Err(e) => panic!("kernel rejected the ACL blob: {e}"),
        }
        assert_eq!(getxattr(&path).unwrap().as_deref(), Some(value.as_slice()));
        // Setting the ACL also rewrote the file's mode to match its
        // object entries (mode and ACL are two views of one state), so
        // the file is now fully in sync: apply() must read no drift.
        assert!(!apply(&path, Some(7), &uids));
        // Clobber the ACL (as an external strip would) and apply() must
        // detect the drift, repair it, and then go quiet again.
        assert!(clear(&path));
        assert!(apply(&path, Some(7), &uids));
        assert!(!apply(&path, Some(7), &uids));
    }
}

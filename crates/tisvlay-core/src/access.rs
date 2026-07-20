//! One-click enrollment into the group that gates the daemon socket.
//!
//! The packaged install can't add the installing user to the socket group: a
//! postinstall runs as root with no `$SUDO_USER` under PackageKit / GNOME
//! Software, so any "now run `sudo usermod -aG …`" hint reaches no one.
//! Instead the CLI and GUI offer a one-click grant — `pkexec usermod` from
//! the user's own session, authenticated once through the desktop's polkit
//! agent, which knows exactly who to enroll.
//!
//! Raw `libc` (not `nix`) mirrors the daemon's `acl.rs` reentrant group
//! resolution; the outcome is returned, not printed, so each surface renders
//! it its own way (the CLI prints, the GUI shows a dialog).

use std::ffi::CStr;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The packaged default daemon group, used to name the socket's group when
/// the socket is absent — the group is created at install time even before
/// the daemon first starts.
pub const DEFAULT_GROUP: &str = "tisvlay";

/// The result of an [`enable_access`] call, for the caller to render.
#[derive(Debug, Clone)]
pub enum EnrollOutcome {
    /// The session was already a member of the group; nothing changed.
    AlreadyMember { user: String, group: String },
    /// `usermod` added the user; a re-login is required for it to take effect.
    Enrolled { user: String, group: String },
}

/// Whether the current login session can reach the daemon socket. Drives
/// whether a surface offers the enroll affordance at all.
///
/// A live probe, not a group check: the daemon also grants presence-scoped
/// access (the active-seat ACL), which confers access without membership —
/// a group check alone would nag a console user whose access works.
/// `EACCES` on connect is a definitive no. Any other failure (absent
/// socket, refused) is ambiguous — the daemon may simply be down, or a
/// non-traversable runtime dir may have hidden the live socket from the
/// caller's path resolution entirely — so it falls back to the durable
/// criterion: membership in the socket's owning group. A member whose
/// daemon is down reads as having access (their problem is the daemon); a
/// non-member who can't even see the socket reads as not.
pub fn has_access(socket: &Path) -> bool {
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => false,
        Err(_) => member_of(&socket_group_or_default(socket)),
    }
}

/// Add the current user to the socket's owning group via `pkexec usermod`
/// (idempotent). Blocks on the polkit prompt — call it off any UI thread.
pub fn enable_access(socket: &Path) -> Result<EnrollOutcome> {
    let group = socket_group_or_default(socket);
    let user = current_user_name().context("resolve the current user name")?;

    if member_of(&group) {
        return Ok(EnrollOutcome::AlreadyMember { user, group });
    }

    let status = Command::new("pkexec")
        .args(["usermod", "-aG", group.as_str(), user.as_str()])
        .status()
        .context("run `pkexec usermod` (is polkit installed?)")?;
    if !status.success() {
        bail!(
            "granting access did not complete (pkexec exited {status}). \
             Run it manually: sudo usermod -aG {group} {user}"
        );
    }
    Ok(EnrollOutcome::Enrolled { user, group })
}

/// The group gating `socket`: its owning group when the socket exists, else
/// the packaged default (a fresh install whose daemon has not started yet
/// still has the group from the postinstall).
fn socket_group_or_default(socket: &Path) -> String {
    socket_group(socket).unwrap_or_else(|| DEFAULT_GROUP.to_string())
}

/// The name of the group owning `socket`, if the socket exists and resolves.
fn socket_group(socket: &Path) -> Option<String> {
    let gid = std::fs::metadata(socket).ok()?.gid();
    group_name(gid)
}

/// Whether the current login session's group set already includes `group`
/// — i.e. whether this session can already reach the group-owned socket. A
/// pending `usermod` that has not taken effect until re-login reads here as
/// "not a member," which is the correct prompt (re-login still needed).
fn member_of(group: &str) -> bool {
    let Some(gid) = gid_of(group) else {
        return false;
    };
    // SAFETY: getegid is always safe and takes no arguments.
    if unsafe { libc::getegid() } == gid {
        return true;
    }
    // SAFETY: a size of 0 with a null list asks only for the count.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    if count <= 0 {
        return false;
    }
    let mut groups = vec![0 as libc::gid_t; count as usize];
    // SAFETY: `groups` has room for `count` entries; getgroups fills up to
    // that many and returns how many it wrote (or -1).
    let got = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    got >= 0 && groups[..got as usize].contains(&gid)
}

/// Resolve a group gid to its name via the reentrant `getgrgid_r`.
fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: an all-zero `libc::group` is a valid empty initializer
        // that getgrgid_r overwrites before any field is read.
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: getgrgid_r fills `grp`/`result` out of `buf`; `result` is
        // null when the gid has no entry. `grp` is read only on success.
        let ret = unsafe {
            libc::getgrgid_r(
                gid,
                &mut grp,
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut result,
            )
        };
        if ret == 0 {
            if result.is_null() {
                return None;
            }
            // SAFETY: on success `grp.gr_name` points into `buf`, a valid
            // NUL-terminated C string for the lifetime of this borrow.
            let name = unsafe { CStr::from_ptr(grp.gr_name) };
            return Some(name.to_string_lossy().into_owned());
        }
        if ret == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return None;
    }
}

/// Resolve a group name to its gid via the reentrant `getgrnam_r`.
fn gid_of(name: &str) -> Option<u32> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: see `group_name` — an all-zero `libc::group` is a valid
        // initializer getgrnam_r overwrites before it is read.
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::group = std::ptr::null_mut();
        // SAFETY: getgrnam_r fills `grp`/`result` from `buf`; `result` is
        // null when the name has no entry.
        let ret = unsafe {
            libc::getgrnam_r(
                cname.as_ptr(),
                &mut grp,
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut result,
            )
        };
        if ret == 0 {
            return (!result.is_null()).then_some(grp.gr_gid);
        }
        if ret == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        return None;
    }
}

/// The current effective user's name via the reentrant `getpwuid_r`.
fn current_user_name() -> Result<String> {
    // SAFETY: geteuid is always safe and takes no arguments.
    let uid = unsafe { libc::geteuid() };
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: an all-zero `libc::passwd` is a valid empty initializer
        // that getpwuid_r overwrites before any field is read.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: getpwuid_r fills `pwd`/`result` out of `buf`; `result` is
        // null when the uid has no entry. `pwd` is read only on success.
        let ret = unsafe {
            libc::getpwuid_r(
                uid,
                &mut pwd,
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                &mut result,
            )
        };
        if ret == 0 {
            if result.is_null() {
                bail!("no passwd entry for uid {uid}");
            }
            // SAFETY: on success `pwd.pw_name` points into `buf`, a valid
            // NUL-terminated C string for the lifetime of this borrow.
            let name = unsafe { CStr::from_ptr(pwd.pw_name) };
            return Ok(name.to_string_lossy().into_owned());
        }
        if ret == libc::ERANGE && buf.len() < (1 << 20) {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        bail!("getpwuid_r failed for uid {uid}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_group_reads_as_no_access() {
        // A group that can't exist resolves to no gid, so membership is
        // false — never a panic. This is the safe default the access gate
        // relies on: an unresolvable group must never look reachable.
        assert!(!member_of("tisvlay-group-that-cannot-exist-zzz"));
    }

    #[test]
    fn unknown_gid_has_no_name() {
        assert_eq!(group_name(u32::MAX), None);
    }

    #[test]
    fn absent_socket_falls_back_to_default_group() {
        let missing = Path::new("/nonexistent/tisvlay/zenoh.sock");
        assert_eq!(socket_group_or_default(missing), DEFAULT_GROUP);
        // And `has_access` over an absent socket doesn't panic — the
        // connect probe fails without a permission error, so it falls back
        // to checking membership of the default group.
        let _ = has_access(missing);
    }

    #[test]
    fn connectable_socket_grants_access_regardless_of_group() {
        // A live, connectable socket is access — the presence-grant case,
        // where the caller is in no relevant group at all.
        let dir = std::env::temp_dir().join(format!("tisvlay-access-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        assert!(has_access(&path));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn permission_denied_socket_reads_as_no_access() {
        // SAFETY: geteuid is always safe and takes no arguments.
        if unsafe { libc::geteuid() } == 0 {
            return; // root bypasses DAC; the probe can't be denied
        }
        let dir = std::env::temp_dir().join(format!("tisvlay-noaccess-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // Connecting requires write on the socket inode; a 0 mode denies
        // even the owner, which is the deterministic EACCES this needs.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert!(!has_access(&path));
        std::fs::remove_dir_all(&dir).ok();
    }
}

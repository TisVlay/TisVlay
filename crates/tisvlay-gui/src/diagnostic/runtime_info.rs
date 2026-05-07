//! Runtime probes for the diagnostic blob's `runtime.{kernel, glibc,
//! os_id, os_version}` fields. Each probe returns `Option<String>`
//! and the renderer maps `None` to JSON `null` so consumers can tell
//! "probe didn't run" apart from "field was empty".

use etc_os_release::OsRelease;

/// Kernel release string from `uname()` (e.g. `6.18.24`). Returns
/// `None` when the syscall surface yields a non-UTF-8 value, which
/// is exceedingly rare on Linux but possible (the kernel's release
/// field is typically a stable ASCII subset).
pub fn kernel_release() -> Option<String> {
    let utsname = rustix::system::uname();
    let release = utsname.release().to_string_lossy().into_owned();
    if release.is_empty() {
        None
    } else {
        Some(release)
    }
}

/// glibc version via `gnu_get_libc_version()`. Gated on
/// `target_env = "gnu"` — musl builds or any other libc surface
/// `None`. Captured at runtime (not build time) because cross-distro
/// packaging triage cares about the *deployment* glibc, not whatever
/// the binary was built against.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn glibc_version() -> Option<String> {
    use std::ffi::CStr;

    // SAFETY: `gnu_get_libc_version` is a thread-safe libc call that
    // returns a pointer to a static C string with no aliasing
    // concerns. The pointer is valid for the lifetime of the
    // process; `CStr::from_ptr` only dereferences it for the
    // duration of the call. Returns `None` if the pointer is null
    // (theoretical) or the bytes aren't valid UTF-8.
    unsafe {
        let p = libc::gnu_get_libc_version();
        if p.is_null() {
            return None;
        }
        CStr::from_ptr(p).to_str().ok().map(str::to_owned)
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub fn glibc_version() -> Option<String> {
    None
}

/// `(os_id, os_version)` from `/etc/os-release`. `os_id` is required
/// by the spec but the file itself is optional — missing-file /
/// permission-denied / parse-error all surface as `(None, None)`.
/// Per `etc-os-release`'s public surface, `version_id` is optional
/// even when the file is present (some immutable / appliance distros
/// omit it).
pub fn os_release() -> (Option<String>, Option<String>) {
    match OsRelease::open() {
        Ok(release) => (
            Some(release.id().to_owned()),
            release.version_id().map(str::to_owned),
        ),
        Err(_) => (None, None),
    }
}

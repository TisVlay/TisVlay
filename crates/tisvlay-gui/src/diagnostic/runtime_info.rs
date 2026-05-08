//! Runtime probes for the diagnostic blob's `runtime.{kernel, glibc,
//! os_id, os_version}` fields. Each probe returns `Option<String>`
//! and the renderer maps `None` to JSON `null` so consumers can tell
//! "probe didn't run" apart from "field was empty".

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

/// `(os_id, os_version)` from the `os_info` crate. `os_info::get()`
/// is infallible (returns `Type::Unknown` / `Version::Unknown` when
/// it can't detect, which we map to `None`), so missing-file /
/// permission-denied / parse-error all surface as the same absence.
/// The id is lowercased to match the `/etc/os-release ID=` convention
/// (`Type::NixOS.to_string()` is `"NixOS"`; the spec spells the id
/// `nixos`).
///
/// Known caveat: `os_info` parses `VERSION_ID` into a typed
/// `Version::Semantic(major, minor, patch)` enum and re-renders,
/// which drops formatting hints from the original string. NixOS's
/// YY.MM convention loses its leading zero: `VERSION_ID="26.05"` →
/// `Semantic(26, 5, 0)` → `"26.5.0"`. Acknowledged upstream as a bug
/// (stanislav-tkach/os_info#318, with a proposed string-based
/// redesign in #280); kept here because the alternative (raw inline
/// parser, or staying on `etc-os-release`) loses the typed
/// `Type::NixOS` defence against distro-detection typos. A future
/// `os_info` release that switches to raw-string version storage
/// will fix this without a code change on our side.
pub fn os_release() -> (Option<String>, Option<String>) {
    let info = os_info::get();
    let id = match info.os_type() {
        os_info::Type::Unknown => None,
        ty => Some(ty.to_string().to_lowercase()),
    };
    let version = match info.version() {
        os_info::Version::Unknown => None,
        v => Some(v.to_string()),
    };
    (id, version)
}

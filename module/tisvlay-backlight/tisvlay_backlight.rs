// SPDX-License-Identifier: GPL-2.0

//! Virtual backlight driver for tisvlay.
//!
//! Creates `/dev/tisvlay-bl` — a control interface for the tisvlay
//! daemon to create and manage virtual `/sys/class/backlight/` devices
//! per managed TV. When KDE writes to a backlight device, the daemon
//! reads the new value from the chardev and forwards it to the TV.
//!
//! Backlight registration and the event queue use C helpers
//! (`helpers.c`) because the kernel's backlight subsystem and wait
//! queue primitives have no Rust abstractions yet.

use kernel::ioctl::{_IO, _IOW};
use kernel::miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration};
use kernel::prelude::*;
use kernel::uaccess::UserPtr;

module! {
    type: TisvlayBacklight,
    name: "tisvlay_backlight",
    authors: ["tisvlay contributors"],
    description: "Virtual backlight devices for network-controlled TVs",
    license: "GPL",
}

// ── UAPI constants (shared with userspace) ─────────────────────

/// Maximum TV ID length including NUL. Must match TISVLAY_ID_MAX in helpers.c.
const ID_MAX: usize = 64;

/// Maximum EDID size. Must match TISVLAY_EDID_MAX in helpers.c.
const EDID_MAX: usize = 256;

/// ioctl magic number (arbitrary, 'T' for tisvlay).
const IOCTL_MAGIC: u32 = b'T' as u32;

/// Create a backlight device.
const IOCTL_CREATE: u32 = _IOW::<IoctlCreate>(IOCTL_MAGIC, 1);
/// Destroy a backlight device.
const IOCTL_DESTROY: u32 = _IOW::<IoctlDestroy>(IOCTL_MAGIC, 2);
/// Set brightness (reverse path: TV → KDE).
const IOCTL_SET_BRIGHTNESS: u32 = _IOW::<IoctlSetBrightness>(IOCTL_MAGIC, 3);

/// Argument for IOCTL_CREATE.
#[repr(C)]
struct IoctlCreate {
    tv_id: [u8; ID_MAX],
    max_brightness: i32,
    initial_brightness: i32,
    edid_len: i32,
    edid: [u8; EDID_MAX],
}

/// Argument for IOCTL_DESTROY.
#[repr(C)]
struct IoctlDestroy {
    tv_id: [u8; ID_MAX],
}

/// Argument for IOCTL_SET_BRIGHTNESS.
#[repr(C)]
struct IoctlSetBrightness {
    tv_id: [u8; ID_MAX],
    brightness: i32,
}

/// Event returned by read() — forward path (KDE → daemon).
#[repr(C)]
struct BrightnessEvent {
    tv_id: [u8; ID_MAX],
    brightness: i32,
}

// ── FFI declarations for helpers.c ─────────────────────────────

unsafe extern "C" {
    fn tisvlay_events_init();
    fn tisvlay_events_shutdown();
    fn tisvlay_pop_event(
        tv_id: *mut core::ffi::c_char,
        brightness: *mut core::ffi::c_int,
        block: bool,
    ) -> core::ffi::c_int;
    fn tisvlay_bl_create(
        tv_id: *const core::ffi::c_char,
        max_brightness: core::ffi::c_int,
        initial_brightness: core::ffi::c_int,
        edid_data: *const u8,
        edid_len: core::ffi::c_int,
    ) -> core::ffi::c_int;
    fn tisvlay_bl_destroy(tv_id: *const core::ffi::c_char) -> core::ffi::c_int;
    fn tisvlay_bl_set(
        tv_id: *const core::ffi::c_char,
        brightness: core::ffi::c_int,
    ) -> core::ffi::c_int;
    fn tisvlay_bl_destroy_all();
}

// ── Module init / exit ─────────────────────────────────────────

struct TisvlayBacklight {
    _miscdev: Pin<KBox<MiscDeviceRegistration<TisvlayBlMisc>>>,
}

impl kernel::Module for TisvlayBacklight {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // SAFETY: Called once during module init before any events.
        unsafe { tisvlay_events_init() };

        let opts = MiscDeviceOptions {
            name: kernel::c_str!("tisvlay-bl"),
        };
        let miscdev = KBox::pin_init(
            MiscDeviceRegistration::<TisvlayBlMisc>::register(opts),
            GFP_KERNEL,
        )?;

        pr_info!("loaded, /dev/tisvlay-bl available\n");
        Ok(Self { _miscdev: miscdev })
    }
}

impl Drop for TisvlayBacklight {
    fn drop(&mut self) {
        // SAFETY: Called once during module exit. Wakes blocked readers
        // and destroys all backlight devices.
        unsafe {
            tisvlay_events_shutdown();
            tisvlay_bl_destroy_all();
        }
        pr_info!("unloaded\n");
    }
}

// ── MiscDevice implementation ──────────────────────────────────

struct TisvlayBlMisc;

/// Copy a fixed-size C string from userspace ioctl arg.
fn copy_tv_id_from_user(user_ptr: UserPtr, offset: usize) -> Result<[u8; ID_MAX]> {
    let mut buf = [0u8; ID_MAX];
    let src = UserPtr::from_addr(user_ptr.as_const_ptr() as usize + offset);
    // SAFETY: We read ID_MAX bytes from the user pointer into our buffer.
    unsafe {
        kernel::bindings::copy_from_user(
            buf.as_mut_ptr().cast(),
            src.as_const_ptr(),
            ID_MAX,
        )
    };
    // Ensure NUL termination.
    buf[ID_MAX - 1] = 0;
    Ok(buf)
}

#[vtable]
impl MiscDevice for TisvlayBlMisc {
    type Ptr = KBox<Self>;

    fn open(
        _file: &kernel::fs::File,
        _misc: &MiscDeviceRegistration<Self>,
    ) -> Result<KBox<Self>> {
        Ok(KBox::new(Self, GFP_KERNEL)?)
    }

    fn ioctl(
        _device: <KBox<Self> as kernel::types::ForeignOwnable>::Borrowed<'_>,
        _file: &kernel::fs::File,
        cmd: u32,
        arg: usize,
    ) -> Result<isize> {
        let user = UserPtr::from_addr(arg);

        match cmd {
            IOCTL_CREATE => {
                // Copy the entire IoctlCreate struct from userspace.
                let mut create = core::mem::MaybeUninit::<IoctlCreate>::zeroed();
                let size = core::mem::size_of::<IoctlCreate>();
                unsafe {
                    kernel::bindings::copy_from_user(
                        create.as_mut_ptr().cast(),
                        user.as_const_ptr(),
                        size,
                    );
                }
                let create = unsafe { create.assume_init() };

                // Ensure NUL termination on tv_id.
                let mut tv_id = create.tv_id;
                tv_id[ID_MAX - 1] = 0;

                let edid_len = create.edid_len.clamp(0, EDID_MAX as i32);

                // SAFETY: tv_id is NUL-terminated, edid points to
                // stack-local data, C helper copies what it needs.
                let ret = unsafe {
                    tisvlay_bl_create(
                        tv_id.as_ptr().cast(),
                        create.max_brightness,
                        create.initial_brightness,
                        create.edid.as_ptr(),
                        edid_len,
                    )
                };
                if ret < 0 {
                    return Err(Error::from_errno(ret));
                }
                pr_info!("created backlight device for '{}'\n",
                    core::str::from_utf8(&tv_id).unwrap_or("?").trim_end_matches('\0'));
                Ok(0)
            }

            IOCTL_DESTROY => {
                let tv_id = copy_tv_id_from_user(user, 0)?;
                // SAFETY: tv_id is NUL-terminated.
                let ret = unsafe { tisvlay_bl_destroy(tv_id.as_ptr().cast()) };
                if ret < 0 {
                    return Err(Error::from_errno(ret));
                }
                pr_info!("destroyed backlight device for '{}'\n",
                    core::str::from_utf8(&tv_id).unwrap_or("?").trim_end_matches('\0'));
                Ok(0)
            }

            IOCTL_SET_BRIGHTNESS => {
                let tv_id = copy_tv_id_from_user(user, 0)?;
                let mut brightness: i32 = 0;
                let br_ptr = UserPtr::from_addr(arg + ID_MAX);
                unsafe {
                    kernel::bindings::copy_from_user(
                        (&raw mut brightness).cast(),
                        br_ptr.as_const_ptr(),
                        4usize,
                    );
                }
                // SAFETY: tv_id is NUL-terminated.
                let ret = unsafe {
                    tisvlay_bl_set(tv_id.as_ptr().cast(), brightness)
                };
                if ret < 0 {
                    return Err(Error::from_errno(ret));
                }
                Ok(0)
            }

            _ => Err(ENOTTY),
        }
    }

    fn read_iter(
        _kiocb: kernel::fs::Kiocb<'_, Self::Ptr>,
        iov: &mut kernel::iov::IovIterDest<'_>,
    ) -> Result<usize> {
        let mut tv_id = [0i8; ID_MAX];
        let mut brightness: i32 = 0;

        // SAFETY: Blocks until an event is available or the module shuts down.
        let ret = unsafe {
            tisvlay_pop_event(tv_id.as_mut_ptr(), &raw mut brightness, true)
        };
        if ret < 0 {
            return Err(Error::from_errno(ret));
        }

        let evt = BrightnessEvent {
            tv_id: unsafe { core::mem::transmute(tv_id) },
            brightness,
        };

        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&raw const evt).cast::<u8>(),
                core::mem::size_of::<BrightnessEvent>(),
            )
        };

        let written = iov.copy_to_iter(bytes);
        Ok(written)
    }
}

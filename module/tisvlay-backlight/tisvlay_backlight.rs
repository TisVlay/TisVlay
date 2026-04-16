// SPDX-License-Identifier: GPL-2.0

//! Virtual backlight driver for tisvlay.
//!
//! Creates `/dev/tisvlay-bl` — a control interface for the tisvlay
//! daemon to create and manage virtual `/sys/class/backlight/` devices
//! per managed TV. When KDE writes to a backlight device, the daemon
//! reads the new value from the chardev and forwards it to the TV.
//!
//! Backlight registration uses C helpers (`helpers.c`) because the
//! kernel's backlight subsystem has no Rust abstractions yet.

use kernel::miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration};
use kernel::prelude::*;

module! {
    type: TisvlayBacklight,
    name: "tisvlay_backlight",
    authors: ["tisvlay contributors"],
    description: "Virtual backlight devices for network-controlled TVs",
    license: "GPL",
}

/// Module-level state: the miscdevice registration + shared data.
struct TisvlayBacklight {
    _miscdev: Pin<KBox<MiscDeviceRegistration<TisvlayBlMisc>>>,
}

impl kernel::Module for TisvlayBacklight {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        let opts = MiscDeviceOptions {
            name: kernel::c_str!("tisvlay-bl"),
        };
        let miscdev = KBox::pin_init(MiscDeviceRegistration::<TisvlayBlMisc>::register(opts), GFP_KERNEL)?;

        pr_info!("tisvlay_backlight: loaded, /dev/tisvlay-bl available\n");
        Ok(Self { _miscdev: miscdev })
    }
}

impl Drop for TisvlayBacklight {
    fn drop(&mut self) {
        pr_info!("tisvlay_backlight: unloaded\n");
    }
}

/// Per-open-file state for the miscdevice.
struct TisvlayBlMisc;

#[vtable]
impl MiscDevice for TisvlayBlMisc {
    type Ptr = KBox<Self>;

    fn open(_file: &kernel::fs::File, _misc: &MiscDeviceRegistration<Self>) -> Result<KBox<Self>> {
        pr_info!("tisvlay_backlight: chardev opened\n");
        Ok(KBox::new(Self, GFP_KERNEL)?)
    }

    fn ioctl(
        _device: <KBox<Self> as kernel::types::ForeignOwnable>::Borrowed<'_>,
        _file: &kernel::fs::File,
        cmd: u32,
        arg: usize,
    ) -> Result<isize> {
        pr_info!("tisvlay_backlight: ioctl cmd={} arg={}\n", cmd, arg);
        // TODO: implement CREATE, DESTROY, SET_BRIGHTNESS ioctls
        Err(ENOTTY)
    }
}

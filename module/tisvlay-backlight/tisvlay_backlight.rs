// SPDX-License-Identifier: GPL-2.0

//! Virtual backlight driver for tisvlay.
//!
//! Creates `/dev/tisvlay-bl` — a control interface for the tisvlay
//! daemon to create and manage virtual `/sys/class/backlight/` devices
//! per managed TV. When KDE writes to a backlight device, the daemon
//! reads the new value from the chardev and forwards it to the TV.

use kernel::prelude::*;

module! {
    type: TisvlayBacklight,
    name: "tisvlay_backlight",
    authors: ["tisvlay contributors"],
    description: "Virtual backlight devices for network-controlled TVs",
    license: "GPL",
}

struct TisvlayBacklight;

impl kernel::Module for TisvlayBacklight {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("tisvlay_backlight: loaded\n");
        Ok(Self)
    }
}

impl Drop for TisvlayBacklight {
    fn drop(&mut self) {
        pr_info!("tisvlay_backlight: unloaded\n");
    }
}

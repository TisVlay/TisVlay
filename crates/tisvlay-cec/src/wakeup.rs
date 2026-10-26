//! Which device lets the TV wake this PC: a CEC adapter's first USB-device
//! ancestor in sysfs, when the kernel can let that device wake the system.
//!
//! Its `power/wakeup` file's presence is what counts, not its value, which
//! the wakeup unit toggles. A DisplayPort-AUX adapter has no USB ancestor,
//! and walking on to any ancestor with `power/wakeup` would find the GPU's
//! or the USB controller's, so the walk stops at the first USB device.

// External-input profile: this module reads sysfs data, so indexing,
// arithmetic, string slicing and narrowing casts must be checked here
// (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use std::path::{Path, PathBuf};

/// The CEC node name (`cecN`) a device path names, following a udev alias
/// (a symlink) to the node. `None` for anything else, or a path that
/// doesn't resolve (the adapter is unplugged).
pub fn cec_node(device: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(device).ok()?;
    let name = resolved.file_name()?.to_str()?;
    is_cec_node(name).then(|| name.to_owned())
}

/// Whether `name` is a CEC node's: `cec` and a number. What keeps a name
/// from walking anywhere else under sysfs.
fn is_cec_node(name: &str) -> bool {
    name.strip_prefix("cec")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The USB device that can let CEC node `node` wake this PC, under the
/// sysfs mounted at `sysfs` (`/sys`): the node's first USB-device ancestor,
/// when it has a `power/wakeup` file. `None` without one, or for a node
/// with no USB ancestor.
pub fn usb_wakeup_device(sysfs: &Path, node: &str) -> Option<PathBuf> {
    if !is_cec_node(node) {
        return None;
    }
    let root = std::fs::canonicalize(sysfs).ok()?;
    let start = std::fs::canonicalize(sysfs.join("bus/cec/devices").join(node)).ok()?;
    let device = start
        .ancestors()
        .take_while(|dir| *dir != root && dir.starts_with(&root))
        .find(|dir| is_usb_device(dir))?;
    device
        .join("power/wakeup")
        .exists()
        .then(|| device.to_path_buf())
}

/// Whether sysfs directory `dir` is a USB device (not an interface): its
/// `uevent` says `DEVTYPE=usb_device`.
fn is_usb_device(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("uevent")).is_ok_and(|uevent| uevent_is_usb_device(&uevent))
}

fn uevent_is_usb_device(uevent: &str) -> bool {
    uevent
        .lines()
        .any(|line| line.trim() == "DEVTYPE=usb_device")
}

#[cfg(any(test, feature = "test-link"))]
pub mod fixture {
    //! A fake sysfs tree: a CEC node under a chain of devices, linked from
    //! `bus/cec/devices`. For tests, under a temporary directory.

    use std::path::{Path, PathBuf};

    /// One directory on the node's path, outermost first.
    #[derive(Debug)]
    pub struct Level<'a> {
        /// The directory's name.
        pub name: &'a str,
        /// Its `uevent`, if it has one.
        pub uevent: Option<&'a str>,
        /// Whether it has `power/wakeup` (written `disabled`).
        pub wakeup: bool,
    }

    /// Build `levels` under `root/devices`, ending in CEC node `node`, and
    /// link it from `root/bus/cec/devices/<node>`. Returns the node's dir.
    pub fn build(root: &Path, levels: &[Level<'_>], node: &str) -> PathBuf {
        let mut dir = root.join("devices");
        for level in levels {
            dir = dir.join(level.name);
            create(&dir);
            if let Some(uevent) = level.uevent {
                write(&dir.join("uevent"), uevent);
            }
            if level.wakeup {
                create(&dir.join("power"));
                write(&dir.join("power/wakeup"), "disabled\n");
            }
        }
        let node_dir = dir.join(node);
        create(&node_dir);
        write(&node_dir.join("uevent"), "MAJOR=241\nMINOR=0\n");
        let bus = root.join("bus/cec/devices");
        create(&bus);
        std::os::unix::fs::symlink(&node_dir, bus.join(node))
            .expect("a test's temporary directory takes a symlink");
        node_dir
    }

    fn create(dir: &Path) {
        std::fs::create_dir_all(dir).expect("a test's temporary directory takes a directory");
    }

    fn write(path: &Path, contents: &str) {
        std::fs::write(path, contents).expect("a test's temporary directory takes a file");
    }

    /// A Pulse-Eight USB adapter: a USB device under the xHCI controller (whose
    /// own `power/wakeup` must not be found), then its interface, the tty
    /// and the serio port the CEC node hangs off.
    pub fn pulse_eight(root: &Path) -> PathBuf {
        build(
            root,
            &[
                Level {
                    name: "pci0000:00",
                    uevent: None,
                    wakeup: false,
                },
                Level {
                    name: "0000:00:14.0",
                    uevent: Some("DRIVER=xhci_hcd\n"),
                    wakeup: true,
                },
                Level {
                    name: "usb1",
                    uevent: Some("DEVTYPE=usb_device\n"),
                    wakeup: true,
                },
                Level {
                    name: "1-5",
                    uevent: Some("MAJOR=189\nDEVTYPE=usb_device\nDRIVER=usb\n"),
                    wakeup: true,
                },
                Level {
                    name: "1-5:1.0",
                    uevent: Some("DEVTYPE=usb_interface\n"),
                    wakeup: false,
                },
                Level {
                    name: "tty",
                    uevent: None,
                    wakeup: false,
                },
                Level {
                    name: "ttyACM0",
                    uevent: Some("MAJOR=166\n"),
                    wakeup: false,
                },
                Level {
                    name: "serio0",
                    uevent: Some("DRIVER=pulse8-cec\n"),
                    wakeup: false,
                },
            ],
            "cec0",
        );
        root.join("devices/pci0000:00/0000:00:14.0/usb1/1-5")
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{Level, build, pulse_eight};
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_usb_adapter_resolves_to_its_own_usb_device() {
        let sysfs = tempfile::tempdir().unwrap();
        let device = pulse_eight(sysfs.path());
        assert_eq!(
            usb_wakeup_device(sysfs.path(), "cec0"),
            Some(std::fs::canonicalize(device).unwrap())
        );
    }

    /// A DisplayPort-AUX adapter hangs off the GPU, which may have
    /// `power/wakeup` itself: no USB device, no wake.
    #[test]
    fn a_dp_aux_adapter_under_a_gpu_doesnt() {
        let sysfs = tempfile::tempdir().unwrap();
        build(
            sysfs.path(),
            &[
                Level {
                    name: "pci0000:00",
                    uevent: None,
                    wakeup: false,
                },
                Level {
                    name: "0000:01:00.0",
                    uevent: Some("DRIVER=amdgpu\n"),
                    wakeup: true,
                },
                Level {
                    name: "drm",
                    uevent: None,
                    wakeup: false,
                },
                Level {
                    name: "card0-DP-1",
                    uevent: Some("DEVTYPE=drm_connector\n"),
                    wakeup: false,
                },
            ],
            "cec1",
        );
        assert_eq!(usb_wakeup_device(sysfs.path(), "cec1"), None);
    }

    /// A USB device without `power/wakeup` can't wake the system, and the
    /// walk doesn't go on to the hub's.
    #[test]
    fn the_first_usb_device_decides() {
        let sysfs = tempfile::tempdir().unwrap();
        build(
            sysfs.path(),
            &[
                Level {
                    name: "usb1",
                    uevent: Some("DEVTYPE=usb_device\n"),
                    wakeup: true,
                },
                Level {
                    name: "1-3",
                    uevent: Some("DEVTYPE=usb_device\n"),
                    wakeup: false,
                },
                Level {
                    name: "1-3:1.0",
                    uevent: Some("DEVTYPE=usb_interface\n"),
                    wakeup: false,
                },
            ],
            "cec2",
        );
        assert_eq!(usb_wakeup_device(sysfs.path(), "cec2"), None);
    }

    #[test]
    fn only_cec_node_names_resolve() {
        let sysfs = tempfile::tempdir().unwrap();
        pulse_eight(sysfs.path());
        for name in ["", "cec", "cecx", "../cec0", "cec0/..", "sda", "cec0 "] {
            assert_eq!(usb_wakeup_device(sysfs.path(), name), None, "{name:?}");
        }
        assert_eq!(usb_wakeup_device(sysfs.path(), "cec7"), None, "absent");
    }

    #[test]
    fn a_device_path_names_its_node_through_an_alias() {
        let dev = tempfile::tempdir().unwrap();
        std::fs::write(dev.path().join("cec0"), "").unwrap();
        std::os::unix::fs::symlink(dev.path().join("cec0"), dev.path().join("tv")).unwrap();
        assert_eq!(cec_node(&dev.path().join("tv")).as_deref(), Some("cec0"));
        assert_eq!(cec_node(&dev.path().join("cec0")).as_deref(), Some("cec0"));
        std::fs::write(dev.path().join("ttyACM0"), "").unwrap();
        assert_eq!(cec_node(&dev.path().join("ttyACM0")), None);
        assert_eq!(cec_node(&dev.path().join("missing")), None);
    }

    proptest! {
        /// Whatever a `uevent` holds and whatever name arrives, reading
        /// them never panics.
        #[test]
        fn never_panics(uevent in ".*", name in ".*") {
            let _ = uevent_is_usb_device(&uevent);
            let _ = is_cec_node(&name);
            let _ = usb_wakeup_device(Path::new("/nonexistent"), &name);
        }
    }
}

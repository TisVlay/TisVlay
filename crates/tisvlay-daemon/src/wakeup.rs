//! The TV waking the PC: the daemon's wake list, and the root unit that
//! arms the adapters on it before each sleep.
//!
//! Nothing in TisVlay runs while the PC sleeps, and the daemon can't write
//! sysfs (`ProtectKernelTunables=true`), so the daemon only writes the list:
//! the device nodes of the TVs whose system profile's `on_tv_power_on` is
//! `wake`, beside its socket (`/run/tisvlay/wakeup` when packaged). The
//! static `tisvlay-wakeup.service` runs [`main`] as root before
//! `sleep.target`: it resolves each node to its USB device and enables that
//! device's `power/wakeup`, records what it enabled under its own runtime
//! directory, and disables only those once they're off the list. An adapter
//! it never enabled stays as the host configured it, so the default `none`
//! never overrides a user's own udev rule. With the daemon stopped, its
//! runtime directory is gone, the list reads empty, and the unit disables
//! only what it had enabled.

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Where the daemon writes the wake list: beside its socket.
pub(crate) fn list_path(socket: &Path) -> PathBuf {
    socket.with_file_name("wakeup")
}

/// Write each list the world sends to `path`, one node per line.
pub(crate) async fn write_lists(path: PathBuf, mut lists: mpsc::UnboundedReceiver<Vec<PathBuf>>) {
    while let Some(nodes) = lists.recv().await {
        let mut contents = Vec::new();
        for node in &nodes {
            contents.extend_from_slice(node.as_os_str().as_bytes());
            contents.push(b'\n');
        }
        let target = path.clone();
        let written = tokio::task::spawn_blocking(move || {
            tisvlay_runtime::fs::write_atomic(&target, &contents)
        })
        .await;
        match written {
            Ok(Ok(())) => info!(path = %path.display(), ?nodes, "wake list written"),
            Ok(Err(error)) => warn!(path = %path.display(), %error, "couldn't write the wake list"),
            Err(error) => warn!(%error, "the wake-list write task failed"),
        }
    }
}

/// `tisvlay service wakeup`: what `tisvlay-wakeup.service` runs, as root,
/// before each sleep.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// The daemon's wake list: one CEC device node per line.
    #[arg(long, default_value = "/run/tisvlay/wakeup")]
    list: PathBuf,

    /// Where this unit records the devices it enabled, so it only ever
    /// disables those.
    #[arg(long, default_value = "/run/tisvlay-wakeup/enabled")]
    record: PathBuf,
}

/// Arm the listed adapters and disarm the ones this unit armed that are
/// no longer listed.
pub fn main(args: Args) -> anyhow::Result<()> {
    tisvlay_runtime::logging::init("tisvlay-wakeup", &["tisvlay_daemon=info"], &[])?;
    let applied = apply(Path::new("/sys"), &args.list, &args.record)?;
    info!(enabled = ?applied.enabled, disabled = ?applied.disabled, "wake sources applied");
    Ok(())
}

/// What one [`apply`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// USB devices whose `power/wakeup` this run enabled.
    pub enabled: Vec<PathBuf>,
    /// USB devices whose `power/wakeup` this run disabled again.
    pub disabled: Vec<PathBuf>,
}

/// Apply `list` under the sysfs at `sysfs`, keeping `record`.
pub fn apply(sysfs: &Path, list: &Path, record: &Path) -> anyhow::Result<Applied> {
    let listed: BTreeSet<PathBuf> = read_lines(list)?
        .iter()
        .filter_map(|line| {
            let device = tisvlay_cec::wakeup::cec_node(Path::new(line))
                .and_then(|node| tisvlay_cec::wakeup::usb_wakeup_device(sysfs, &node));
            if device.is_none() {
                warn!(node = line, "no USB device to wake the PC from");
            }
            device
        })
        .collect();
    let recorded: BTreeSet<PathBuf> = read_lines(record)?.into_iter().map(PathBuf::from).collect();

    let mut applied = Applied::default();
    let mut ours = BTreeSet::new();
    for device in &listed {
        match wakeup(device) {
            Some(Wakeup::Disabled) => {
                set_wakeup(device, Wakeup::Enabled)?;
                applied.enabled.push(device.clone());
                ours.insert(device.clone());
            }
            Some(Wakeup::Enabled) if recorded.contains(device) => {
                ours.insert(device.clone());
            }
            // Enabled by the host: not ours to record, or to undo later.
            Some(Wakeup::Enabled) => {}
            None => warn!(device = %device.display(), "unreadable power/wakeup"),
        }
    }
    for device in recorded.difference(&listed) {
        if wakeup(device) == Some(Wakeup::Enabled) {
            set_wakeup(device, Wakeup::Disabled)?;
            applied.disabled.push(device.clone());
        }
    }
    let mut contents = Vec::new();
    for device in &ours {
        contents.extend_from_slice(device.as_os_str().as_bytes());
        contents.push(b'\n');
    }
    tisvlay_runtime::fs::write_atomic(record, &contents)
        .with_context(|| format!("record the enabled devices in {}", record.display()))?;
    Ok(applied)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wakeup {
    Enabled,
    Disabled,
}

fn wakeup(device: &Path) -> Option<Wakeup> {
    match std::fs::read_to_string(device.join("power/wakeup"))
        .ok()?
        .trim()
    {
        "enabled" => Some(Wakeup::Enabled),
        "disabled" => Some(Wakeup::Disabled),
        _ => None,
    }
}

fn set_wakeup(device: &Path, value: Wakeup) -> anyhow::Result<()> {
    let value = match value {
        Wakeup::Enabled => "enabled",
        Wakeup::Disabled => "disabled",
    };
    let path = device.join("power/wakeup");
    std::fs::write(&path, value).with_context(|| format!("write {value} to {}", path.display()))
}

/// A file's non-empty lines; a missing file has none.
fn read_lines(path: &Path) -> anyhow::Result<Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tisvlay_cec::wakeup::fixture::pulse_eight;

    struct Host {
        sysfs: tempfile::TempDir,
        run: tempfile::TempDir,
        device: PathBuf,
    }

    impl Host {
        /// A Pulse-Eight adapter as `dev/cec0` and in sysfs, its wakeup disabled.
        fn new() -> Self {
            let sysfs = tempfile::tempdir().unwrap();
            let device = std::fs::canonicalize(pulse_eight(sysfs.path())).unwrap();
            let run = tempfile::tempdir().unwrap();
            std::fs::create_dir(run.path().join("dev")).unwrap();
            std::fs::write(run.path().join("dev/cec0"), "").unwrap();
            Self { sysfs, run, device }
        }

        fn list(&self, listed: bool) {
            let node = self.run.path().join("dev/cec0");
            let contents = if listed {
                format!("{}\n", node.display())
            } else {
                String::new()
            };
            std::fs::write(self.run.path().join("wakeup"), contents).unwrap();
        }

        fn apply(&self) -> Applied {
            apply(
                self.sysfs.path(),
                &self.run.path().join("wakeup"),
                &self.run.path().join("unit/enabled"),
            )
            .unwrap()
        }

        fn wakeup(&self) -> String {
            std::fs::read_to_string(self.device.join("power/wakeup"))
                .unwrap()
                .trim()
                .to_owned()
        }

        fn set(&self, value: &str) {
            std::fs::write(self.device.join("power/wakeup"), value).unwrap();
        }
    }

    #[test]
    fn a_listed_adapter_is_armed_once_and_disarmed_when_delisted() {
        let host = Host::new();
        host.list(true);
        assert_eq!(host.apply().enabled, std::slice::from_ref(&host.device));
        assert_eq!(host.wakeup(), "enabled");
        assert_eq!(host.apply(), Applied::default(), "already armed");

        host.list(false);
        assert_eq!(host.apply().disabled, std::slice::from_ref(&host.device));
        assert_eq!(host.wakeup(), "disabled");
        assert_eq!(host.apply(), Applied::default(), "nothing left to undo");
    }

    /// An adapter the host enabled itself (a udev rule) is never disabled.
    #[test]
    fn the_hosts_own_wakeup_is_left_alone() {
        let host = Host::new();
        host.set("enabled");
        host.list(true);
        assert_eq!(host.apply(), Applied::default());
        host.list(false);
        assert_eq!(host.apply(), Applied::default());
        assert_eq!(host.wakeup(), "enabled");
    }

    /// With the daemon stopped there's no list: only what this unit armed
    /// is disarmed.
    #[test]
    fn a_missing_list_disarms_only_what_was_armed() {
        let host = Host::new();
        host.list(true);
        host.apply();
        std::fs::remove_file(host.run.path().join("wakeup")).unwrap();
        assert_eq!(host.apply().disabled, std::slice::from_ref(&host.device));
    }

    /// A line that isn't a CEC node with a USB device arms nothing.
    #[test]
    fn other_lines_arm_nothing() {
        let host = Host::new();
        std::fs::write(
            host.run.path().join("wakeup"),
            "/dev/sda\n/nonexistent/cec3\n\n",
        )
        .unwrap();
        assert_eq!(host.apply(), Applied::default());
        assert_eq!(host.wakeup(), "disabled");
    }
}

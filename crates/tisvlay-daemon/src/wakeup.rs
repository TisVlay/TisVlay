//! The TV waking the PC: the daemon's wake list, and the root unit that
//! arms or blocks the adapters on it for each sleep.
//!
//! Nothing in TisVlay runs while the PC sleeps, and the daemon can't write
//! sysfs (`ProtectKernelTunables=true`), so the daemon only writes the list:
//! every TV's device node with its system profile's `on_tv_power_on`,
//! beside its socket (`/run/tisvlay/wakeup` when packaged). The static
//! `tisvlay-wakeup.service` runs [`main`] as root when the PC goes to sleep
//! and again when it resumes. Going to sleep, it resolves each node to its
//! USB device: for `wake` it enables the device's `power/wakeup`; for
//! `none` it disables the device's hub port, since some adapters wake the
//! PC whatever their `power/wakeup` says, and only a port they're cut off
//! keeps them from it. Resuming, it enables the ports it disabled. It
//! records what it changed under its own runtime directory and only ever
//! undoes that: a device's wakeup it enabled is disabled once the device
//! is off the list, and a port or a wakeup the host set itself stays as
//! the host has it. With the daemon stopped, its runtime directory is
//! gone, the list reads empty, and the unit undoes only what it had done.

use std::collections::BTreeSet;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::Context;
use tisvlay_core::SystemWakeAction;
use tisvlay_world::WakeList;
use tokio::sync::mpsc;
use tracing::{info, warn};

/// Where the daemon writes the wake list: beside its socket.
pub(crate) fn list_path(socket: &Path) -> PathBuf {
    socket.with_file_name("wakeup")
}

/// Write each list the world sends to `path`, one `<action> <node>` line
/// per node.
pub(crate) async fn write_lists(path: PathBuf, mut lists: mpsc::UnboundedReceiver<WakeList>) {
    while let Some(list) = lists.recv().await {
        let mut contents = Vec::new();
        for (node, action) in &list {
            let action = match action {
                SystemWakeAction::Wake => WAKE,
                SystemWakeAction::None => NONE,
            };
            contents.extend_from_slice(action.as_bytes());
            contents.push(b' ');
            contents.extend_from_slice(node.as_os_str().as_bytes());
            contents.push(b'\n');
        }
        let target = path.clone();
        let written = tokio::task::spawn_blocking(move || {
            tisvlay_runtime::fs::write_atomic(&target, &contents)
        })
        .await;
        match written {
            Ok(Ok(())) => info!(path = %path.display(), ?list, "wake list written"),
            Ok(Err(error)) => warn!(path = %path.display(), %error, "couldn't write the wake list"),
            Err(error) => warn!(%error, "the wake-list write task failed"),
        }
    }
}

/// The list's action words: `on_tv_power_on`'s tokens.
const WAKE: &str = "wake";
const NONE: &str = "none";

/// `tisvlay service wakeup`: what `tisvlay-wakeup.service` runs, as root,
/// when the PC goes to sleep and when it resumes.
#[derive(clap::Args, Debug)]
pub struct Args {
    /// The daemon's wake list: one `<wake|none> <CEC device node>` per line.
    #[arg(long, default_value = "/run/tisvlay/wakeup")]
    list: PathBuf,

    /// Where this unit records the wakeups it enabled and the ports it
    /// disabled, so it only ever undoes those.
    #[arg(long, default_value = "/run/tisvlay-wakeup/applied")]
    record: PathBuf,

    /// The PC resumed: enable the ports disabled for the sleep.
    #[arg(long)]
    resume: bool,
}

/// Apply the list for a sleep, or undo its blocked ports on resume.
pub fn main(args: Args) -> anyhow::Result<()> {
    tisvlay_runtime::logging::init("tisvlay-wakeup", &["tisvlay_daemon=info"], &[])?;
    if args.resume {
        let unblocked = resume(&args.record)?;
        info!(?unblocked, "adapter ports enabled after the resume");
    } else {
        let applied = apply(Path::new("/sys"), &args.list, &args.record)?;
        info!(
            enabled = ?applied.enabled,
            disabled = ?applied.disabled,
            blocked = ?applied.blocked,
            unblocked = ?applied.unblocked,
            "wake sources applied"
        );
    }
    Ok(())
}

/// What one [`apply`] changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// USB devices whose `power/wakeup` this run enabled.
    pub enabled: Vec<PathBuf>,
    /// USB devices whose `power/wakeup` this run disabled again.
    pub disabled: Vec<PathBuf>,
    /// Hub ports this run disabled.
    pub blocked: Vec<PathBuf>,
    /// Hub ports this run enabled again: a `wake` now, or left disabled
    /// by a resume the unit missed.
    pub unblocked: Vec<PathBuf>,
}

/// Apply `list` under the sysfs at `sysfs` for a sleep, keeping `record`.
/// A device or port that won't take a write is logged and skipped, so one
/// failure doesn't leave the rest unapplied, or the unit failed with
/// ports blocked and no resume run to enable them.
pub fn apply(sysfs: &Path, list: &Path, record: &Path) -> anyhow::Result<Applied> {
    let mut wake = BTreeSet::new();
    let mut block = BTreeSet::new();
    for line in read_lines(list)? {
        let Some((action, node)) = line.split_once(' ') else {
            warn!(line, "a wake-list line without an action");
            continue;
        };
        let Some(device) = tisvlay_cec::wakeup::cec_node(Path::new(node))
            .and_then(|node| tisvlay_cec::wakeup::usb_wakeup_device(sysfs, &node))
        else {
            warn!(node, "no USB device for the TV's node");
            continue;
        };
        match action {
            WAKE => {
                wake.insert(device);
            }
            NONE => match tisvlay_cec::wakeup::usb_port(&device) {
                Some(port) => {
                    block.insert(port);
                }
                None => warn!(device = %device.display(), "no port to keep the adapter off"),
            },
            _ => warn!(line, "an unknown wake action"),
        }
    }
    let recorded = Record::read(record)?;

    let mut applied = Applied::default();
    let mut ours = Record::default();
    for device in &wake {
        match wakeup(device) {
            Some(Toggle::Off) => {
                if set_wakeup(device, Toggle::On) {
                    applied.enabled.push(device.clone());
                    ours.wakeups.insert(device.clone());
                }
            }
            Some(Toggle::On) if recorded.wakeups.contains(device) => {
                ours.wakeups.insert(device.clone());
            }
            // Enabled by the host: not ours to record, or to undo later.
            Some(Toggle::On) => {}
            None => warn!(device = %device.display(), "unreadable power/wakeup"),
        }
    }
    for device in recorded.wakeups.difference(&wake) {
        if wakeup(device) == Some(Toggle::On) {
            if set_wakeup(device, Toggle::Off) {
                applied.disabled.push(device.clone());
            } else {
                ours.wakeups.insert(device.clone());
            }
        }
    }
    for port in recorded.ports.difference(&block) {
        if unblock(port) {
            applied.unblocked.push(port.clone());
        } else {
            ours.ports.insert(port.clone());
        }
    }
    let mut to_block = Vec::new();
    for port in &block {
        match disabled(port) {
            Some(Toggle::Off) => to_block.push(port.clone()),
            Some(Toggle::On) if recorded.ports.contains(port) => {}
            // Disabled by the host: not ours to record, or to undo later.
            Some(Toggle::On) => continue,
            None => {
                warn!(port = %port.display(), "unreadable port disable");
                continue;
            }
        }
        ours.ports.insert(port.clone());
    }
    // Record the ports before disabling them, so a write that fails
    // partway still has its resume.
    ours.write(record)?;
    for port in to_block {
        if set_disabled(&port, Toggle::On) {
            applied.blocked.push(port);
        }
    }
    Ok(applied)
}

/// Enable the ports in `record` again after a resume, dropping them from
/// it; one that won't enable stays recorded for the next run. Returns the
/// ports enabled.
pub fn resume(record: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut recorded = Record::read(record)?;
    let mut unblocked = Vec::new();
    recorded.ports.retain(|port| {
        let done = unblock(port);
        if done {
            unblocked.push(port.clone());
        }
        !done
    });
    recorded.write(record)?;
    Ok(unblocked)
}

/// What the unit changed and must undo: one `wakeup <device>` or
/// `port <port>` line each.
#[derive(Debug, Default)]
struct Record {
    wakeups: BTreeSet<PathBuf>,
    ports: BTreeSet<PathBuf>,
}

impl Record {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let mut record = Self::default();
        for line in read_lines(path)? {
            match line.split_once(' ') {
                Some(("wakeup", device)) => {
                    record.wakeups.insert(device.into());
                }
                Some(("port", port)) => {
                    record.ports.insert(port.into());
                }
                _ => warn!(line, "an unknown record line"),
            }
        }
        Ok(record)
    }

    fn write(&self, path: &Path) -> anyhow::Result<()> {
        let mut contents = Vec::new();
        for (kind, paths) in [("wakeup", &self.wakeups), ("port", &self.ports)] {
            for path in paths {
                contents.extend_from_slice(kind.as_bytes());
                contents.push(b' ');
                contents.extend_from_slice(path.as_os_str().as_bytes());
                contents.push(b'\n');
            }
        }
        tisvlay_runtime::fs::write_atomic(path, &contents)
            .with_context(|| format!("write the record {}", path.display()))
    }
}

/// A sysfs switch's two states: `power/wakeup`'s `enabled`/`disabled`, or
/// a port's `disable` `1`/`0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    On,
    Off,
}

fn wakeup(device: &Path) -> Option<Toggle> {
    read_toggle(&device.join("power/wakeup"), "enabled", "disabled")
}

fn set_wakeup(device: &Path, value: Toggle) -> bool {
    let value = match value {
        Toggle::On => "enabled",
        Toggle::Off => "disabled",
    };
    write_toggle(&device.join("power/wakeup"), value)
}

fn disabled(port: &Path) -> Option<Toggle> {
    read_toggle(&port.join("disable"), "1", "0")
}

fn set_disabled(port: &Path, value: Toggle) -> bool {
    let value = match value {
        Toggle::On => "1",
        Toggle::Off => "0",
    };
    write_toggle(&port.join("disable"), value)
}

/// Enable `port` if it's disabled. Whether it's now enabled, or gone
/// (nothing left to undo).
fn unblock(port: &Path) -> bool {
    match disabled(port) {
        Some(Toggle::On) => set_disabled(port, Toggle::Off),
        Some(Toggle::Off) => true,
        None => {
            warn!(port = %port.display(), "unreadable port disable, dropped");
            true
        }
    }
}

fn read_toggle(path: &Path, on: &str, off: &str) -> Option<Toggle> {
    let value = std::fs::read_to_string(path).ok()?;
    match value.trim() {
        v if v == on => Some(Toggle::On),
        v if v == off => Some(Toggle::Off),
        _ => None,
    }
}

/// Write `value` to `path`, logging a failure. Whether it took.
fn write_toggle(path: &Path, value: &str) -> bool {
    match std::fs::write(path, value) {
        Ok(()) => true,
        Err(error) => {
            warn!(path = %path.display(), value, %error, "couldn't write");
            false
        }
    }
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
        port: PathBuf,
    }

    impl Host {
        /// A Pulse-Eight adapter as `dev/cec0` and in sysfs, its wakeup
        /// disabled and its port enabled.
        fn new() -> Self {
            let sysfs = tempfile::tempdir().unwrap();
            let device = std::fs::canonicalize(pulse_eight(sysfs.path())).unwrap();
            let port = std::fs::canonicalize(device.join("port")).unwrap();
            let run = tempfile::tempdir().unwrap();
            std::fs::create_dir(run.path().join("dev")).unwrap();
            std::fs::write(run.path().join("dev/cec0"), "").unwrap();
            Self {
                sysfs,
                run,
                device,
                port,
            }
        }

        /// List `dev/cec0` with `action`, or nothing.
        fn list(&self, action: Option<&str>) {
            let node = self.run.path().join("dev/cec0");
            let contents = match action {
                Some(action) => format!("{action} {}\n", node.display()),
                None => String::new(),
            };
            std::fs::write(self.run.path().join("wakeup"), contents).unwrap();
        }

        fn record(&self) -> PathBuf {
            self.run.path().join("unit/applied")
        }

        fn apply(&self) -> Applied {
            apply(
                self.sysfs.path(),
                &self.run.path().join("wakeup"),
                &self.record(),
            )
            .unwrap()
        }

        fn resume(&self) -> Vec<PathBuf> {
            resume(&self.record()).unwrap()
        }

        fn read(path: &Path) -> String {
            std::fs::read_to_string(path).unwrap().trim().to_owned()
        }

        fn wakeup(&self) -> String {
            Self::read(&self.device.join("power/wakeup"))
        }

        fn disable(&self) -> String {
            Self::read(&self.port.join("disable"))
        }
    }

    #[test]
    fn a_waking_adapter_is_armed_once_and_disarmed_when_delisted() {
        let host = Host::new();
        host.list(Some("wake"));
        assert_eq!(host.apply().enabled, std::slice::from_ref(&host.device));
        assert_eq!(host.wakeup(), "enabled");
        assert_eq!(host.apply(), Applied::default(), "already armed");
        assert_eq!(host.resume(), Vec::<PathBuf>::new(), "no port to enable");
        assert_eq!(host.wakeup(), "enabled", "armed across the resume");

        host.list(None);
        assert_eq!(host.apply().disabled, std::slice::from_ref(&host.device));
        assert_eq!(host.wakeup(), "disabled");
        assert_eq!(host.apply(), Applied::default(), "nothing left to undo");
    }

    /// `none` disables the adapter's port for the sleep, and the resume
    /// enables it again.
    #[test]
    fn a_non_waking_adapters_port_is_blocked_for_the_sleep() {
        let host = Host::new();
        host.list(Some("none"));
        assert_eq!(host.apply().blocked, std::slice::from_ref(&host.port));
        assert_eq!(host.disable(), "1");
        assert_eq!(host.resume(), std::slice::from_ref(&host.port));
        assert_eq!(host.disable(), "0");
        assert_eq!(host.resume(), Vec::<PathBuf>::new(), "nothing left to undo");
    }

    /// A resume the unit missed leaves the port recorded: the next sleep
    /// keeps it blocked, and taking it off the list enables it.
    #[test]
    fn a_missed_resume_is_caught_up() {
        let host = Host::new();
        host.list(Some("none"));
        host.apply();
        assert_eq!(host.apply(), Applied::default(), "still blocked");
        assert_eq!(host.disable(), "1");

        host.list(Some("wake"));
        let applied = host.apply();
        assert_eq!(applied.unblocked, std::slice::from_ref(&host.port));
        assert_eq!(applied.enabled, std::slice::from_ref(&host.device));
        assert_eq!(host.disable(), "0");
    }

    /// A wakeup or a port the host set itself (a udev rule) is never undone.
    #[test]
    fn the_hosts_own_settings_are_left_alone() {
        let host = Host::new();
        std::fs::write(host.device.join("power/wakeup"), "enabled").unwrap();
        host.list(Some("wake"));
        assert_eq!(host.apply(), Applied::default());
        host.list(None);
        assert_eq!(host.apply(), Applied::default());
        assert_eq!(host.wakeup(), "enabled");

        std::fs::write(host.port.join("disable"), "1").unwrap();
        host.list(Some("none"));
        assert_eq!(host.apply(), Applied::default());
        assert_eq!(host.resume(), Vec::<PathBuf>::new());
        assert_eq!(host.disable(), "1");
    }

    /// With the daemon stopped there's no list: only what this unit did
    /// is undone.
    #[test]
    fn a_missing_list_undoes_only_what_was_done() {
        let host = Host::new();
        host.list(Some("wake"));
        host.apply();
        std::fs::remove_file(host.run.path().join("wakeup")).unwrap();
        assert_eq!(host.apply().disabled, std::slice::from_ref(&host.device));

        host.list(Some("none"));
        host.apply();
        std::fs::remove_file(host.run.path().join("wakeup")).unwrap();
        assert_eq!(host.apply().unblocked, std::slice::from_ref(&host.port));
        assert_eq!(host.disable(), "0");
    }

    /// A line that isn't an action and a CEC node with a USB device
    /// changes nothing.
    #[test]
    fn other_lines_change_nothing() {
        let host = Host::new();
        let node = host.run.path().join("dev/cec0");
        std::fs::write(
            host.run.path().join("wakeup"),
            format!(
                "{node}\nwake /dev/sda\nnone /nonexistent/cec3\nblock {node}\n\n",
                node = node.display()
            ),
        )
        .unwrap();
        assert_eq!(host.apply(), Applied::default());
        assert_eq!(host.wakeup(), "disabled");
        assert_eq!(host.disable(), "0");
    }
}

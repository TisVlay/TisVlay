//! Local-device watcher: reports whether each local-device TV's node
//! (a CEC adapter's `/dev/cecN`) is usable, so the world can wait for an
//! absent adapter instead of dialing into it, and redial when it's back.
//!
//! The reactor requests monitoring per TV ([`DeviceRequest`]); this module
//! answers with `InEvent::DeviceAvailability` at request time and whenever
//! udev reports a device coming or going. It listens on udev's netlink
//! group rather than the kernel's: udev broadcasts only after its rules
//! have run, so the node's permissions and aliases are in place by then.
//! An event only triggers a re-check of every monitored path; its content
//! is never trusted further. A slow poll of unavailable paths covers hosts
//! without udev events.

// External-input profile: this module parses udev netlink messages, so
// indexing, arithmetic, string slicing and narrowing casts must be checked
// here (docs/build.md, Lint policy).
#![warn(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::string_slice,
    clippy::cast_possible_truncation
)]

use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use rtnetlink::sys::protocols::NETLINK_KOBJECT_UEVENT;
use rtnetlink::sys::{AsyncSocket, AsyncSocketExt, SocketAddr, TokioSocket};
use tisvlay_world::{DeviceRequest, InEvent, WorldClient};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// udev's multicast group on `NETLINK_KOBJECT_UEVENT`, as a bind mask (the
/// kernel's own uevents are group 1).
const UDEV_GROUP_MASK: u32 = 0b10;

/// Where udev keeps its database. A node it has finished processing has an
/// entry here (`c<major>:<minor>` for a character device), which is
/// libudev's own "initialized" test.
const UDEV_RUN: &str = "/run/udev";

/// Re-check unavailable paths this often, for hosts whose udev events never
/// reach us (containers, minimal systems). A check is a `stat`, not an open.
const RECHECK_PERIOD: Duration = Duration::from_secs(60);

/// Socket receive buffer: a boot coldplug (`udevadm trigger`) is a burst of
/// hundreds of events.
const RX_BUFFER: usize = 1 << 20;

/// Reopen backoff after the udev socket fails, as in the network monitor.
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

pub(crate) fn spawn_device_watcher(
    world: WorldClient,
    requests: mpsc::UnboundedReceiver<DeviceRequest>,
) {
    tokio::spawn(supervise(world, requests));
}

/// Keep the watcher running for the world's lifetime. When the udev socket
/// fails, requests are still served (each is a direct check) through a
/// capped backoff, then the socket reopens. The view survives reopens.
async fn supervise(world: WorldClient, mut requests: mpsc::UnboundedReceiver<DeviceRequest>) {
    let udev = Path::new(UDEV_RUN);
    let mut state = WatcherState::default();
    let mut backoff = RECONNECT_MIN;
    loop {
        let started = tokio::time::Instant::now();
        let error = match watch_devices(&world, &mut requests, &mut state, udev).await {
            Ok(()) => return,
            Err(error) => error,
        };
        if started.elapsed() >= RECONNECT_MAX {
            backoff = RECONNECT_MIN;
        }
        warn!(error = %format!("{error:#}"), retry_in = ?backoff, "device watcher lost its udev socket; reopening");
        let pause = tokio::time::sleep(backoff);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => {
                        if !handle_request(&world, &mut state, udev, request) {
                            return;
                        }
                    }
                    None => return,
                },
                () = &mut pause => break,
            }
        }
        backoff = backoff.saturating_mul(2).min(RECONNECT_MAX);
    }
}

/// The watcher's view: each monitored TV's path and the availability last
/// reported for it. Pure bookkeeping; the I/O loop feeds it checks and
/// forwards the events it returns.
#[derive(Debug, Default)]
struct WatcherState {
    monitored: HashMap<Uuid, PathBuf>,
    reported: HashMap<Uuid, bool>,
}

impl WatcherState {
    fn monitor(&mut self, tv: Uuid, path: PathBuf) {
        self.monitored.insert(tv, path);
    }

    fn forget(&mut self, tv: Uuid) {
        self.monitored.remove(&tv);
        self.reported.remove(&tv);
    }

    /// A TV's freshly checked availability; the event when it changed, or
    /// always when `force` (a monitor request: the world may be asking
    /// about a TV it just enrolled, whose earlier reports it never saw).
    fn on_checked(&mut self, tv: Uuid, available: bool, force: bool) -> Option<InEvent> {
        let changed = self.reported.insert(tv, available) != Some(available);
        (changed || force).then_some(InEvent::DeviceAvailability { tv, available })
    }

    /// The monitored TVs and their paths, for a re-check pass.
    fn monitored(&self) -> Vec<(Uuid, PathBuf)> {
        self.monitored
            .iter()
            .map(|(&tv, path)| (tv, path.clone()))
            .collect()
    }

    /// The monitored paths last reported unavailable.
    fn unavailable(&self) -> Vec<&Path> {
        self.monitored
            .iter()
            .filter(|(tv, _)| self.reported.get(tv) == Some(&false))
            .map(|(_, path)| path.as_path())
            .collect()
    }
}

/// Whether the node at `path` is usable: it exists (following a udev
/// alias), and, where udev runs (`udev` exists), udev has initialized it,
/// so its group and mode rules have applied. A devtmpfs node exists before
/// that, still root-only.
fn available(path: &Path, udev: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !udev.exists() {
        return true;
    }
    let rdev = meta.rdev();
    let kind = if meta.file_type().is_block_device() {
        'b'
    } else {
        'c'
    };
    udev.join("data")
        .join(format!("{kind}{}:{}", libc::major(rdev), libc::minor(rdev)))
        .exists()
}

/// The fields of a udev netlink message this watcher reads.
#[derive(Debug, PartialEq, Eq)]
struct UdevEvent {
    action: String,
    devname: Option<String>,
}

/// Parse a udev monitor message: the `libudev\0` prefix, a big-endian
/// magic, then host-endian header fields locating a block of NUL-separated
/// `KEY=VALUE` properties. `None` for anything else, including the kernel's
/// own uevent format.
fn parse_udev(buf: &[u8]) -> Option<UdevEvent> {
    const PREFIX: &[u8] = b"libudev\0";
    const MAGIC: u32 = 0xfeed_cafe;
    if buf.get(..PREFIX.len())? != PREFIX {
        return None;
    }
    let field = |offset: usize| -> Option<[u8; 4]> {
        buf.get(offset..offset.checked_add(4)?)?.try_into().ok()
    };
    if u32::from_be_bytes(field(8)?) != MAGIC {
        return None;
    }
    let offset = usize::try_from(u32::from_ne_bytes(field(16)?)).ok()?;
    let len = usize::try_from(u32::from_ne_bytes(field(20)?)).ok()?;
    let properties = buf.get(offset..offset.checked_add(len)?)?;
    let mut action = None;
    let mut devname = None;
    for pair in properties.split(|&b| b == 0) {
        let Ok(pair) = std::str::from_utf8(pair) else {
            continue;
        };
        match pair.split_once('=') {
            Some(("ACTION", value)) => action = Some(value.to_string()),
            Some(("DEVNAME", value)) => devname = Some(value.to_string()),
            _ => {}
        }
    }
    Some(UdevEvent {
        action: action?,
        devname,
    })
}

/// When a node appears that no TV is configured for while one that is
/// stays unavailable, the adapter may have come back under a new number
/// (the kernel reuses a node's number only once its last descriptor
/// closes). Matches a sibling with the same name stem: `/dev/cec1` for an
/// unavailable `/dev/cec0`.
fn renumbered_from<'a>(added: &Path, unavailable: &[&'a Path]) -> Option<&'a Path> {
    let stem = |path: &Path| {
        path.file_name().and_then(|name| name.to_str()).map(|name| {
            name.trim_end_matches(|c: char| c.is_ascii_digit())
                .to_string()
        })
    };
    let added_stem = stem(added).filter(|s| !s.is_empty())?;
    unavailable.iter().copied().find(|path| {
        *path != added && path.parent() == added.parent() && stem(path) == Some(added_stem.clone())
    })
}

/// Forward an event to the world; false once the world is gone.
fn send(world: &WorldClient, event: InEvent) -> bool {
    if let InEvent::DeviceAvailability { tv, available } = &event {
        debug!(%tv, available, "device availability");
    }
    world.send(event)
}

/// One socket's lifetime: `Ok` once the world is gone, `Err` when the udev
/// socket fails and should be reopened.
async fn watch_devices(
    world: &WorldClient,
    requests: &mut mpsc::UnboundedReceiver<DeviceRequest>,
    state: &mut WatcherState,
    udev: &Path,
) -> anyhow::Result<()> {
    // Bind before the re-check, so no event falls between them.
    let mut socket = TokioSocket::new(NETLINK_KOBJECT_UEVENT).context("open a uevent socket")?;
    socket
        .socket_mut()
        .bind(&SocketAddr::new(0, UDEV_GROUP_MASK))
        .context("join udev's netlink group")?;
    if let Err(error) = socket.socket_ref().set_rx_buf_sz(RX_BUFFER) {
        debug!(%error, "couldn't grow the uevent receive buffer");
    }
    if !recheck(world, state, udev) {
        return Ok(());
    }
    info!("device watcher started");
    let mut poll = tokio::time::interval(RECHECK_PERIOD);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            request = requests.recv() => match request {
                Some(request) => {
                    if !handle_request(world, state, udev, request) {
                        return Ok(());
                    }
                }
                None => return Ok(()),
            },
            message = socket.recv_from_full() => match message {
                Ok((buf, _)) => {
                    let Some(event) = parse_udev(&buf) else { continue };
                    if event.action != "add" && event.action != "remove" {
                        continue;
                    }
                    if event.action == "add"
                        && let Some(devname) = &event.devname
                        && let Some(path) = renumbered_from(Path::new(devname), &state.unavailable())
                    {
                        info!(added = %devname, configured = %path.display(), "a device node appeared under another name while a configured one is unavailable; the adapter may have been renumbered");
                    }
                    if !recheck(world, state, udev) {
                        return Ok(());
                    }
                }
                // The kernel overran the receive buffer and dropped
                // events (a coldplug burst): the view may be stale.
                Err(error) if error.raw_os_error() == Some(libc::ENOBUFS) => {
                    warn!("udev events were dropped; re-checking every device");
                    if !recheck(world, state, udev) {
                        return Ok(());
                    }
                }
                Err(error) => return Err(error).context("receive from the uevent socket"),
            },
            _ = poll.tick() => {
                if !state.unavailable().is_empty() && !recheck(world, state, udev) {
                    return Ok(());
                }
            }
        }
    }
}

/// Serve one request; false once the world is gone.
fn handle_request(
    world: &WorldClient,
    state: &mut WatcherState,
    udev: &Path,
    request: DeviceRequest,
) -> bool {
    match request {
        DeviceRequest::Monitor { tv, path } => {
            let usable = available(&path, udev);
            state.monitor(tv, path);
            state
                .on_checked(tv, usable, true)
                .is_none_or(|event| send(world, event))
        }
        DeviceRequest::Forget { tv } => {
            state.forget(tv);
            true
        }
    }
}

/// One check over every monitored path; reports changes only. False once
/// the world is gone.
fn recheck(world: &WorldClient, state: &mut WatcherState, udev: &Path) -> bool {
    for (tv, path) in state.monitored() {
        let usable = available(&path, udev);
        if let Some(event) = state.on_checked(tv, usable, false)
            && !send(world, event)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A udev monitor message carrying `properties` (`KEY=VALUE` pairs).
    fn udev_message(properties: &[&str]) -> Vec<u8> {
        let mut block = Vec::new();
        for pair in properties {
            block.extend_from_slice(pair.as_bytes());
            block.push(0);
        }
        let header_size: u32 = 40;
        let mut buf = b"libudev\0".to_vec();
        buf.extend_from_slice(&0xfeed_cafe_u32.to_be_bytes());
        buf.extend_from_slice(&header_size.to_ne_bytes());
        buf.extend_from_slice(&header_size.to_ne_bytes());
        buf.extend_from_slice(&u32::try_from(block.len()).unwrap().to_ne_bytes());
        buf.resize(40, 0);
        buf.extend_from_slice(&block);
        buf
    }

    #[test]
    fn parses_a_udev_message() {
        let msg = udev_message(&["ACTION=add", "SUBSYSTEM=cec", "DEVNAME=/dev/cec0"]);
        assert_eq!(
            parse_udev(&msg),
            Some(UdevEvent {
                action: "add".into(),
                devname: Some("/dev/cec0".into()),
            })
        );
    }

    #[test]
    fn rejects_the_kernel_format_and_truncated_messages() {
        assert_eq!(
            parse_udev(b"add@/devices/virtual/cec/cec0\0ACTION=add\0"),
            None
        );
        let msg = udev_message(&["ACTION=remove"]);
        assert_eq!(parse_udev(&msg[..30]), None, "a header cut short");
        assert_eq!(
            parse_udev(&msg[..msg.len() - 3]),
            None,
            "properties cut short"
        );
        let mut bad_magic = msg;
        bad_magic[8] ^= 0xff;
        assert_eq!(parse_udev(&bad_magic), None);
        assert_eq!(
            parse_udev(&udev_message(&["SUBSYSTEM=cec"])),
            None,
            "no action"
        );
    }

    #[test]
    fn availability_reports_changes_only_except_on_request() {
        let mut state = WatcherState::default();
        let tv = Uuid::new_v4();
        state.monitor(tv, "/dev/cec0".into());
        assert!(state.on_checked(tv, false, false).is_some(), "first check");
        assert!(state.on_checked(tv, false, false).is_none(), "no change");
        assert!(
            state.on_checked(tv, false, true).is_some(),
            "a monitor request always answers"
        );
        assert!(matches!(
            state.on_checked(tv, true, false),
            Some(InEvent::DeviceAvailability {
                available: true,
                ..
            })
        ));
        assert_eq!(state.unavailable(), Vec::<&Path>::new());
        state.forget(tv);
        assert!(state.monitored().is_empty());
        assert!(
            state.on_checked(tv, true, false).is_some(),
            "a forgotten TV reports afresh"
        );
    }

    #[test]
    fn a_node_counts_once_it_exists_and_udev_initialized_it() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("cec0");
        let udev = dir.path().join("udev");
        assert!(!available(&node, &udev), "absent");
        std::fs::write(&node, b"").unwrap();
        assert!(available(&node, &udev), "present, and no udev on this host");
        std::fs::create_dir_all(udev.join("data")).unwrap();
        assert!(!available(&node, &udev), "udev hasn't processed it yet");
        let rdev = std::fs::metadata(&node).unwrap().rdev();
        std::fs::write(
            udev.join("data")
                .join(format!("c{}:{}", libc::major(rdev), libc::minor(rdev))),
            b"",
        )
        .unwrap();
        assert!(available(&node, &udev), "initialized");
    }

    #[test]
    fn a_sibling_with_the_same_stem_hints_at_renumbering() {
        let cec0 = Path::new("/dev/cec0");
        let alias = Path::new("/dev/cec-pulse8");
        assert_eq!(
            renumbered_from(Path::new("/dev/cec1"), &[cec0, alias]),
            Some(cec0)
        );
        assert_eq!(renumbered_from(cec0, &[cec0]), None, "the node itself");
        assert_eq!(renumbered_from(Path::new("/dev/ttyACM0"), &[cec0]), None);
        assert_eq!(
            renumbered_from(Path::new("/dev/input/event3"), &[cec0]),
            None
        );
    }
}

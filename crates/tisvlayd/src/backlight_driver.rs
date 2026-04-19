//! Virtual backlight kernel module integration.
//!
//! Creates per-TV `/sys/class/backlight/tisvlay-<tv_id>` devices via
//! the `tisvlay_backlight` kernel module (loaded separately). When
//! KWin writes to the sysfs brightness file, the module queues an
//! event on `/dev/tisvlay-bl`; this task reads events and forwards
//! them to `Tv1.Backlight` on the connected TV.
//!
//! Device lifecycle is managed via ioctl:
//! - `CREATE` when a TV connects with `sync_backlight = "kernel"`
//!   and an EDID/connector has been resolved (Phase 1).
//! - `DESTROY` on disconnect or profile flip.
//! - `SET_BRIGHTNESS` on TV-side backlight changes (reverse path —
//!   updates sysfs so KDE's slider reflects the TV state).
//!
//! If the module isn't loaded or `/dev/tisvlay-bl` isn't accessible,
//! the task logs once and exits; brightness sync falls back to the
//! session service's Wayland/D-Bus paths.

use std::collections::{HashMap, VecDeque};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use crate::tv_manager::TvManager;

/// Cap on the per-TV forward queue so an unresponsive TV (one that
/// never echoes subscription updates) can't grow the queue unbounded.
/// Beyond this many pending forwards the oldest entries silently
/// drop — matching echoes after the drop would just not be
/// suppressed, which is acceptable.
const MAX_PENDING_FORWARDS: usize = 32;

/// Control chardev path.
const CHARDEV: &str = "/dev/tisvlay-bl";

/// Module sysfs path — used to detect if the module is loaded.
/// Matches the composite module name in `Kbuild` (`tisvlay_bl.o`),
/// which differs from the Rust source file `tisvlay_backlight.rs`.
const MODULE_SYSFS: &str = "/sys/module/tisvlay_bl";

/// Maximum TV ID length (must match TISVLAY_ID_MAX in the module).
const ID_MAX: usize = 64;

/// Maximum EDID size (must match TISVLAY_EDID_MAX in the module).
const EDID_MAX: usize = 256;

/// Size of the BrightnessEvent struct returned by read().
/// `{ tv_id: [u8; 64], brightness: i32 }` = 68 bytes.
const EVENT_SIZE: usize = ID_MAX + 4;

// ── ioctl numbers ──────────────────────────────────────────────
//
// The kernel module uses Linux ioctl encoding:
//   (dir << 30) | (size << 16) | (type << 8) | nr
// where dir=1 (write), type='T', nr=1/2/3 for CREATE/DESTROY/SET.

/// `IoctlCreate`: tv_id[64] + max_brightness(i32) + initial(i32) + edid_len(i32) + edid[256] = 332 bytes
const IOCTL_CREATE: u64 = (1u64 << 30) | (332u64 << 16) | (b'T' as u64) << 8 | 1;
/// `IoctlDestroy`: tv_id[64] = 64 bytes
const IOCTL_DESTROY: u64 = (1u64 << 30) | (64u64 << 16) | (b'T' as u64) << 8 | 2;
/// `IoctlSetBrightness`: tv_id[64] + brightness(i32) = 68 bytes
const IOCTL_SET_BRIGHTNESS: u64 = (1u64 << 30) | (68u64 << 16) | (b'T' as u64) << 8 | 3;

/// Ioctl struct for CREATE.
#[repr(C)]
struct IoctlCreate {
    tv_id: [u8; ID_MAX],
    max_brightness: i32,
    initial_brightness: i32,
    edid_len: i32,
    edid: [u8; EDID_MAX],
}

/// Ioctl struct for DESTROY.
#[repr(C)]
struct IoctlDestroy {
    tv_id: [u8; ID_MAX],
}

/// Ioctl struct for SET_BRIGHTNESS.
#[repr(C)]
struct IoctlSetBrightness {
    tv_id: [u8; ID_MAX],
    brightness: i32,
}

/// Event returned by read().
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct BrightnessEvent {
    tv_id: [u8; ID_MAX],
    brightness: i32,
}

// ── Commands the coordinator sends to the driver task ──────────

/// Commands the daemon sends to the backlight driver.
#[derive(Debug)]
pub enum BacklightCommand {
    /// Create a sysfs backlight device for the TV.
    Create {
        tv_id: String,
        initial_brightness: u8,
        edid: Vec<u8>,
    },
    /// Destroy the sysfs backlight device.
    Destroy { tv_id: String },
    /// Update the sysfs brightness (reverse path: TV → KDE slider).
    SetBrightness { tv_id: String, brightness: u8 },
}

/// Handle for sending commands to the backlight driver task.
#[derive(Debug, Clone)]
pub struct BacklightDriver {
    tx: mpsc::UnboundedSender<BacklightCommand>,
    /// Pending forwards we've sent to the TV but haven't seen echoed
    /// back yet, in FIFO order per TV. `handle_event` appends on every
    /// forward; `consume_echo` drains matches. The reverse path uses
    /// this to distinguish TV-initiated changes (user pressed the
    /// remote) from echoes of our own forward writes — without it the
    /// daemon re-pushes every echo to sysfs, PowerDevil observes the
    /// sysfs change as external input, and the KDE slider visually
    /// bounces through the ramp-echo history before settling.
    pending_forwards: Arc<Mutex<HashMap<String, VecDeque<u8>>>>,
}

impl BacklightDriver {
    /// Send a command to the driver. Errors are logged but not
    /// propagated — the driver is best-effort.
    pub fn send(&self, cmd: BacklightCommand) {
        if let Err(e) = self.tx.send(cmd) {
            debug!(error = %e, "backlight driver: command channel closed");
        }
    }

    /// Called by the reverse path (state_forwarder on `Tv1.Backlight`
    /// subscription updates) to decide whether `level` is an echo of a
    /// value we recently forwarded to the TV. If so, drain the queue
    /// up through the matched entry and return `true` so the caller
    /// skips the `SetBrightness` ioctl. Any queue entries older than
    /// the match are dropped too — TV's physical backlight ramp tends
    /// to emit only a subset of the values it was driven through.
    pub fn consume_echo(&self, tv_id: &str, level: u8) -> bool {
        let mut map = self.pending_forwards.lock().expect("poisoned");
        if let Some(queue) = map.get_mut(tv_id)
            && let Some(pos) = queue.iter().position(|&v| v == level)
        {
            queue.drain(0..=pos);
            return true;
        }
        false
    }
}

/// Check if the kernel module is loaded and the chardev exists.
fn module_available() -> bool {
    Path::new(MODULE_SYSFS).is_dir() && Path::new(CHARDEV).exists()
}

/// Spawn the backlight driver task. Returns `Some(handle)` if the
/// kernel module is loaded and the chardev is accessible; `None`
/// otherwise (brightness sync falls back to other paths).
pub fn spawn(manager: Arc<TvManager>) -> Option<BacklightDriver> {
    if !module_available() {
        debug!("tisvlay_backlight kernel module not loaded; skipping chardev driver");
        return None;
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let pending_forwards: Arc<Mutex<HashMap<String, VecDeque<u8>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    tokio::spawn(run(manager, rx, pending_forwards.clone()));
    Some(BacklightDriver {
        tx,
        pending_forwards,
    })
}

async fn run(
    manager: Arc<TvManager>,
    mut rx: mpsc::UnboundedReceiver<BacklightCommand>,
    pending_forwards: Arc<Mutex<HashMap<String, VecDeque<u8>>>>,
) {
    // Open the chardev. Hold it for the task's lifetime — the kernel
    // module's event queue is shared across all openers, so reusing
    // one handle avoids races and simplifies shutdown.
    let file = match tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(CHARDEV)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = CHARDEV, "failed to open chardev; brightness driver disabled");
            return;
        }
    };
    info!(path = CHARDEV, "backlight driver task started");

    let fd = file.as_raw_fd();

    // Spawn a blocking reader for /dev/tisvlay-bl events. Converting
    // the sync read loop to async would require epoll on the chardev;
    // the simpler path is to let tokio run the blocking read on a
    // dedicated thread.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<BrightnessEvent>();
    let reader_fd = fd;
    std::thread::Builder::new()
        .name("tisvlay-bl-reader".into())
        .spawn(move || read_loop(reader_fd, event_tx))
        .ok();

    loop {
        tokio::select! {
            cmd = rx.recv() => match cmd {
                Some(cmd) => handle_command(fd, cmd),
                None => {
                    debug!("backlight driver: command channel closed, exiting");
                    break;
                }
            },
            evt = event_rx.recv() => match evt {
                Some(evt) => handle_event(&manager, &pending_forwards, evt).await,
                None => {
                    debug!("backlight driver: reader thread exited, shutting down");
                    break;
                }
            },
        }
    }
}

fn handle_command(fd: i32, cmd: BacklightCommand) {
    match cmd {
        BacklightCommand::Create {
            tv_id,
            initial_brightness,
            edid,
        } => {
            let mut arg = IoctlCreate {
                tv_id: [0; ID_MAX],
                max_brightness: 100,
                initial_brightness: initial_brightness as i32,
                edid_len: edid.len().min(EDID_MAX) as i32,
                edid: [0; EDID_MAX],
            };
            copy_tv_id(&mut arg.tv_id, &tv_id);
            let n = edid.len().min(EDID_MAX);
            arg.edid[..n].copy_from_slice(&edid[..n]);

            match ioctl_call(fd, IOCTL_CREATE, &arg) {
                Ok(()) => info!(tv_id, "created sysfs backlight device"),
                Err(e) => warn!(tv_id, error = %e, "CREATE ioctl failed"),
            }
        }
        BacklightCommand::Destroy { tv_id } => {
            let mut arg = IoctlDestroy { tv_id: [0; ID_MAX] };
            copy_tv_id(&mut arg.tv_id, &tv_id);
            match ioctl_call(fd, IOCTL_DESTROY, &arg) {
                Ok(()) => info!(tv_id, "destroyed sysfs backlight device"),
                Err(e) => debug!(tv_id, error = %e, "DESTROY ioctl (may be absent)"),
            }
        }
        BacklightCommand::SetBrightness { tv_id, brightness } => {
            let mut arg = IoctlSetBrightness {
                tv_id: [0; ID_MAX],
                brightness: brightness as i32,
            };
            copy_tv_id(&mut arg.tv_id, &tv_id);
            match ioctl_call(fd, IOCTL_SET_BRIGHTNESS, &arg) {
                Ok(()) => trace!(tv_id, brightness, "pushed observed brightness to sysfs"),
                Err(e) => warn!(tv_id, error = %e, "SET_BRIGHTNESS ioctl failed"),
            }
        }
    }
}

async fn handle_event(
    manager: &TvManager,
    pending_forwards: &Mutex<HashMap<String, VecDeque<u8>>>,
    evt: BrightnessEvent,
) {
    let tv_id = match std::str::from_utf8(&evt.tv_id) {
        Ok(s) => s.trim_end_matches('\0').to_string(),
        Err(_) => return,
    };
    let level = (evt.brightness as u8).min(100);
    trace!(tv_id, level, "backlight event from sysfs");

    // Record the pending forward *before* sending to the TV. The TV
    // will eventually echo this value (possibly along with ramp-
    // intermediate values) via a subscription update, and the reverse
    // path needs the record in place so it can match and suppress.
    {
        let mut map = pending_forwards.lock().expect("poisoned");
        let queue = map.entry(tv_id.clone()).or_default();
        queue.push_back(level);
        while queue.len() > MAX_PENDING_FORWARDS {
            queue.pop_front();
        }
    }

    if let Err(e) = manager.set_backlight(&tv_id, level).await {
        warn!(tv_id, level, error = %e, "forward from sysfs to TV failed");
    }
}

/// Read the raw EDID bytes for a DRM connector (e.g., `"HDMI-A-2"`).
///
/// Scans `/sys/class/drm/` for a directory whose suffix matches the
/// connector name (e.g., `card0-HDMI-A-2`) and returns the contents of
/// its `edid` file. Returns an empty vec if the connector isn't
/// present, the edid file is missing, or reads shorter than the 128-byte
/// base block.
pub fn read_edid_for_connector(connector: &str) -> Vec<u8> {
    let drm_root = Path::new("/sys/class/drm");
    let Ok(entries) = std::fs::read_dir(drm_root) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some((_, suffix)) = name.split_once('-') else {
            continue;
        };
        if suffix != connector {
            continue;
        }
        let bytes = std::fs::read(entry.path().join("edid")).unwrap_or_default();
        if bytes.len() < 128 {
            return Vec::new();
        }
        let n = bytes.len().min(EDID_MAX);
        return bytes[..n].to_vec();
    }
    Vec::new()
}

fn copy_tv_id(dst: &mut [u8; ID_MAX], src: &str) {
    let bytes = src.as_bytes();
    let n = bytes.len().min(ID_MAX - 1);
    dst[..n].copy_from_slice(&bytes[..n]);
    // Remaining bytes stay zero (NUL-terminated).
}

fn ioctl_call<T>(fd: i32, request: u64, arg: &T) -> Result<()> {
    // SAFETY: `arg` points to a packed repr(C) struct of the size
    // encoded into `request`. The kernel copies from the pointer;
    // we don't retain aliasing.
    let ret = unsafe { libc::ioctl(fd, request, arg as *const T as *const libc::c_void) };
    if ret < 0 {
        let err = std::io::Error::last_os_error();
        Err(anyhow!("ioctl {:#x}: {err}", request))
    } else {
        Ok(())
    }
}

fn read_loop(fd: i32, tx: mpsc::UnboundedSender<BrightnessEvent>) {
    let mut buf = [0u8; EVENT_SIZE];
    loop {
        // SAFETY: Blocking read into a sized buffer on a valid fd.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            warn!(error = %err, "backlight chardev read failed; exiting reader");
            break;
        }
        if n == 0 {
            debug!("backlight chardev EOF; exiting reader");
            break;
        }
        if n as usize != EVENT_SIZE {
            debug!(got = n, expected = EVENT_SIZE, "short read, ignoring");
            continue;
        }
        // SAFETY: buf is exactly EVENT_SIZE bytes of POD data.
        let evt: BrightnessEvent = unsafe { std::ptr::read(buf.as_ptr() as *const _) };
        if tx.send(evt).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bare `BacklightDriver` wired to a fresh command channel
    /// and empty queue map, bypassing `spawn()` (which refuses to run
    /// without the kernel module loaded).
    fn mk_driver() -> (BacklightDriver, mpsc::UnboundedReceiver<BacklightCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending_forwards = Arc::new(Mutex::new(HashMap::new()));
        (
            BacklightDriver {
                tx,
                pending_forwards,
            },
            rx,
        )
    }

    /// Mirror the forward-recording logic inside `handle_event` so tests
    /// can simulate a sysfs-read-and-forward without spinning up a real
    /// driver task. Any change to the real recording logic must mirror
    /// into here.
    fn record(driver: &BacklightDriver, tv_id: &str, level: u8) {
        let mut map = driver.pending_forwards.lock().expect("poisoned");
        let queue = map.entry(tv_id.to_string()).or_default();
        queue.push_back(level);
        while queue.len() > MAX_PENDING_FORWARDS {
            queue.pop_front();
        }
    }

    #[test]
    fn consume_echo_returns_false_on_empty_queue() {
        let (driver, _rx) = mk_driver();
        assert!(!driver.consume_echo("office", 42));
    }

    #[test]
    fn consume_echo_drains_a_single_matching_forward() {
        let (driver, _rx) = mk_driver();
        record(&driver, "office", 42);
        assert!(driver.consume_echo("office", 42));
        // Queue is now empty; a second echo with the same value is
        // not suppressed (treated as genuine TV input).
        assert!(!driver.consume_echo("office", 42));
    }

    /// Regression: during a rapid slider drag the TV emits multiple
    /// ramp-echo updates. Without echo suppression, each echo fires a
    /// `SetBrightness` ioctl that writes sysfs back to a stale value,
    /// visibly yanking the KDE slider through the echo train. The
    /// queue-drain-up-through-match semantics is what makes this work
    /// when the TV only echoes a subset of the values it was driven
    /// through.
    #[test]
    fn consume_echo_drains_ramp_up_through_matched_value() {
        let (driver, _rx) = mk_driver();
        for v in [30u8, 40, 50, 60, 70, 80] {
            record(&driver, "office", v);
        }

        // TV echoes an intermediate ramp value — everything up through
        // the match should be drained, leaving later forwards pending.
        assert!(driver.consume_echo("office", 50));
        {
            let map = driver.pending_forwards.lock().unwrap();
            let queue = map.get("office").expect("queue should still exist");
            assert_eq!(
                queue.iter().copied().collect::<Vec<_>>(),
                vec![60, 70, 80],
                "queue should retain forwards newer than the matched echo"
            );
        }

        // TV eventually echoes the final target — drains the rest.
        assert!(driver.consume_echo("office", 80));
        let map = driver.pending_forwards.lock().unwrap();
        assert!(map.get("office").unwrap().is_empty());
    }

    #[test]
    fn consume_echo_returns_false_for_unknown_value() {
        // Genuine TV-initiated change (user presses backlight on remote)
        // must NOT be suppressed even while forwards are pending — the
        // KDE slider still needs to track user-driven TV state.
        let (driver, _rx) = mk_driver();
        record(&driver, "office", 30);
        record(&driver, "office", 40);

        assert!(!driver.consume_echo("office", 99));

        // Queue is untouched.
        let map = driver.pending_forwards.lock().unwrap();
        assert_eq!(
            map.get("office").unwrap().iter().copied().collect::<Vec<_>>(),
            vec![30, 40]
        );
    }

    #[test]
    fn consume_echo_is_isolated_per_tv_id() {
        let (driver, _rx) = mk_driver();
        record(&driver, "living_room", 50);
        // Other TV's queue is unaffected by this TV's echo — and an
        // echo for a TV that never forwarded anything gets no
        // spurious match.
        assert!(!driver.consume_echo("office", 50));
        assert!(driver.consume_echo("living_room", 50));
    }

    #[test]
    fn pending_queue_is_capped_at_max_pending_forwards() {
        // If a TV goes silent and never echoes, the queue is bounded
        // so the process doesn't leak unbounded memory. Matching
        // echoes for values older than the cap simply aren't
        // suppressed — acceptable trade-off for bounded memory.
        let (driver, _rx) = mk_driver();
        for v in 0u8..=(MAX_PENDING_FORWARDS as u8 + 10) {
            record(&driver, "office", v);
        }
        let queue_len = driver
            .pending_forwards
            .lock()
            .unwrap()
            .get("office")
            .unwrap()
            .len();
        assert_eq!(queue_len, MAX_PENDING_FORWARDS);
    }
}

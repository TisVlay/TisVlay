//! tisvlay-greeter — login-screen idle client, the root `tisvlay-greeter`
//! reshaped for zenoh.
//!
//! Runs inside the greeter session and watches the greeter compositor's
//! `ext-idle-notify-v1`. Idle/resume edges become `{idle}` puts on
//! `tisvlay/<site>/greeter/idle`; the daemon maps them to each automated
//! TV's system-profile `on_idle_timeout` / `on_idle_resume`. This is the
//! only component running before login, so it's what protects an OLED
//! panel at an unattended login screen.
//!
//! The timeout to register comes from the daemon's cached
//! `greeter/timeout` key (`0` = disabled: assert presence, then exit).
//! The idle key itself is a cached publish, so a daemon restarting under
//! a live greeter re-hydrates the current idle state instead of needing
//! the D-Bus stack's notify-with-retry loop.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::json;
use tisvlay_protocol::keys;
use tisvlay_runtime::idle_mutter::IdleWatch;
use tisvlay_runtime::wayland::{
    IdleNotification, IdleTransition, WatchHandle, WaylandIdleError, start_idle_watch_on_socket,
};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use zenoh_ext::{
    AdvancedPublisherBuilderExt, AdvancedSubscriberBuilderExt, CacheConfig, HistoryConfig,
    MissDetectionConfig,
};

/// Decide what (if anything) to publish for a transition, given the last
/// idle state successfully sent. Skips redundant repeats — dispatch is
/// idempotent daemon-side, but the de-dup keeps the wire quiet.
fn next_notify(transition: IdleTransition, last_sent: Option<bool>) -> Option<bool> {
    let idle = matches!(transition, IdleTransition::Idled);
    if last_sent == Some(idle) {
        None
    } else {
        Some(idle)
    }
}

/// Connect the greeter compositor's idle notifications to the daemon.
///
/// On startup it publishes `{idle: false}` to assert the login screen is
/// present (lit), so a screen-off latched by the prior user session
/// doesn't strand it dark — regardless of the idle timer. It then reads
/// the daemon's greeter timeout; `0` returns immediately. Otherwise it
/// registers the idle watch and forwards each edge until the compositor
/// disconnects (greeter session ended), which is a clean exit.
pub async fn run(socket: &Path, site: &str) -> Result<()> {
    run_with_watch(socket, site, |timeout, sender| async move {
        connect_idle_watch(timeout, &sender).await
    })
    .await
}

async fn run_with_watch<Attach, Attached, Guard>(
    socket: &Path,
    site: &str,
    attach: Attach,
) -> Result<()>
where
    Attach: FnOnce(u32, mpsc::UnboundedSender<(u32, IdleTransition)>) -> Attached,
    Attached: Future<Output = Result<Guard>>,
{
    // Pre-flight the daemon socket: a greeter enabled on a session whose
    // user isn't in the tisvlay group — GDM's DynamicUser greeter (Ubuntu),
    // or a grant that hasn't taken effect — can never reach the 0660 socket.
    // Bow out cleanly so `Restart=on-failure` doesn't loop for the whole
    // login screen. Other connect errors (daemon still coming up) fall
    // through to zenoh, whose failure keeps the supervised-restart retry.
    if let Err(e) = UnixStream::connect(socket)
        && e.kind() == std::io::ErrorKind::PermissionDenied
    {
        info!(
            socket = %socket.display(),
            "daemon socket not accessible to this greeter session; idle \
             protection disabled (grant the greeter user tisvlay-group access)"
        );
        return Ok(());
    }

    let session = zenoh::open(
        tisvlay_protocol::bootstrap::client_config(socket).context("greeter session config")?,
    )
    .await
    .map_err(|e| anyhow!("connect to daemon socket {}: {e}", socket.display()))?;

    let publisher = session
        .declare_publisher(keys::greeter_idle(site))
        .encoding(zenoh::bytes::Encoding::APPLICATION_JSON)
        .cache(CacheConfig::default().max_samples(1))
        .sample_miss_detection(MissDetectionConfig::default().heartbeat(Duration::from_millis(500)))
        .publisher_detection()
        .await
        .map_err(|e| anyhow!("declare greeter idle publisher: {e}"))?;
    let publish = |idle: bool| {
        let publisher = &publisher;
        async move {
            let payload = serde_json::to_vec(&json!({ "idle": idle })).unwrap_or_default();
            if let Err(e) = publisher.put(payload).await {
                warn!(idle, %e, "greeter idle publish failed");
                false
            } else {
                true
            }
        }
    };

    // Assert presence first — a prior session may have latched the panel
    // off; the resume mapping lights the login screen for whoever is at it.
    let mut last_sent: Option<bool> = None;
    if publish(false).await {
        last_sent = Some(false);
    }

    let timeout_secs = read_timeout(&session, site).await;
    if timeout_secs == 0 {
        info!("greeter idle timeout is 0 (disabled); presence asserted, nothing to watch");
        return Ok(());
    }
    let timeout_ms = u32::try_from(timeout_secs.saturating_mul(1000)).unwrap_or(u32::MAX);
    info!(timeout_secs, "starting greeter idle watch");

    let (tx, mut rx) = mpsc::unbounded_channel::<(u32, IdleTransition)>();
    // Held for the lifetime of `run` — dropping it stops the watch. One
    // process per greeter session, so there is no re-register path.
    let _watch = attach(timeout_ms, tx.clone()).await?;
    drop(tx);

    while let Some((_tag, transition)) = rx.recv().await {
        let Some(idle) = next_notify(transition, last_sent) else {
            continue;
        };
        if publish(idle).await {
            last_sent = Some(idle);
        }
    }

    // The compositor socket closed — normally the greeter session ending
    // at login, where systemd is already stopping this unit.
    warn!("greeter idle watch ended (compositor disconnected); exiting");
    Ok(())
}

/// Read the daemon-computed timeout from the cached `greeter/timeout`
/// key. Read once at greeter start (each greeter session is a fresh
/// process); absent daemon or key means 0 (disabled).
async fn read_timeout(session: &zenoh::Session, site: &str) -> u64 {
    let sub = match session
        .declare_subscriber(keys::greeter_timeout(site))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
    {
        Ok(sub) => sub,
        Err(e) => {
            warn!(%e, "greeter timeout subscribe failed; treating as disabled");
            return 0;
        }
    };
    match tokio::time::timeout(Duration::from_secs(3), sub.recv_async()).await {
        Ok(Ok(sample)) => serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes())
            .ok()
            .and_then(|value| value.get("seconds").and_then(serde_json::Value::as_u64))
            .unwrap_or(0),
        _ => {
            warn!("no greeter timeout sample; treating as disabled");
            0
        }
    }
}

/// Attach the greeter idle watch. Tries the compositor's wayland socket
/// first (discovered under `XDG_RUNTIME_DIR` — greeter units carry no
/// `WAYLAND_DISPLAY`), retrying while the socket appears; if the socket is
/// up but `ext-idle-notify` is unadvertised (Mutter), falls back to
/// `Mutter.IdleMonitor` on the greeter's session bus.
async fn connect_idle_watch(
    timeout_ms: u32,
    tx: &mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<IdleWatch> {
    connect_idle_watch_with(
        || try_wayland(timeout_ms, tx.clone()).map(IdleWatch::Wayland),
        || async {
            tisvlay_runtime::idle_mutter::start_idle_watch(
                vec![IdleNotification { tag: 0, timeout_ms }],
                tx.clone(),
            )
            .await
            .map(IdleWatch::Mutter)
            .map_err(Into::into)
        },
    )
    .await
}

async fn connect_idle_watch_with<Guard, Wayland, Mutter, Attached>(
    mut wayland: Wayland,
    mutter: Mutter,
) -> Result<Guard>
where
    Wayland: FnMut() -> Result<Guard, TryError>,
    Mutter: FnOnce() -> Attached,
    Attached: Future<Output = Result<Guard>>,
{
    const MAX_ATTEMPTS: u32 = 15;
    // Why the wayland path was abandoned, carried into the final error so a
    // both-transports-failed report names the real wayland reason (a slow
    // wlroots socket) instead of looking like a pure Mutter failure.
    let mut wayland_err = anyhow!("wayland idle watch not attempted");
    for attempt in 1..=MAX_ATTEMPTS {
        match wayland() {
            Ok(handle) => return Ok(handle),
            // Connected, but this compositor doesn't advertise the
            // protocol (Mutter) — no amount of retrying helps; go D-Bus.
            Err(TryError::Unsupported(e)) => {
                debug!(error = %e, "greeter compositor lacks ext-idle-notify; trying Mutter");
                wayland_err = e;
                break;
            }
            // Socket not there yet — the compositor is still coming up.
            Err(TryError::NotReady(e)) => {
                debug!(attempt, error = %e, "greeter compositor not ready; retrying");
                wayland_err = e;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    // GNOME greeter (or a wayland path that never came up): the D-Bus
    // Mutter idle monitor on the greeter session's bus.
    match mutter().await {
        Ok(handle) => Ok(handle),
        Err(e) => {
            warn!(
                xdg_runtime_dir = ?std::env::var("XDG_RUNTIME_DIR").ok(),
                wayland_display = ?std::env::var("WAYLAND_DISPLAY").ok(),
                sockets = ?wayland_sockets(),
                "could not attach a greeter idle watch (wayland or Mutter)"
            );
            Err(anyhow!("Mutter idle monitor: {e}")
                .context(format!("wayland idle unavailable: {wayland_err:#}"))
                .context("attach greeter idle watch"))
        }
    }
}

/// Why a wayland attach attempt failed: `NotReady` is retry-worthy (the
/// compositor socket has not appeared yet); `Unsupported` means it is up
/// but lacks `ext-idle-notify`, so the caller should switch to Mutter.
enum TryError {
    NotReady(anyhow::Error),
    Unsupported(anyhow::Error),
}

fn try_wayland(
    timeout_ms: u32,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<WatchHandle, TryError> {
    try_wayland_at(greeter_socket_path(), timeout_ms, tx)
}

fn try_wayland_at(
    path: Option<PathBuf>,
    timeout_ms: u32,
    tx: mpsc::UnboundedSender<(u32, IdleTransition)>,
) -> Result<WatchHandle, TryError> {
    let path =
        path.ok_or_else(|| TryError::NotReady(anyhow!("no wayland socket under XDG_RUNTIME_DIR")))?;
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("connect wayland socket {}", path.display()))
        .map_err(TryError::NotReady)?;
    start_idle_watch_on_socket(stream, vec![IdleNotification { tag: 0, timeout_ms }], tx).map_err(
        |e| match e {
            WaylandIdleError::ProtocolNotSupported(_) => TryError::Unsupported(anyhow!("{e}")),
            other => TryError::NotReady(anyhow!("{other}")),
        },
    )
}

/// Resolve the compositor socket path: `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY`
/// when set, else the first `wayland-<n>` socket in `$XDG_RUNTIME_DIR`.
fn greeter_socket_path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    let display = std::env::var_os("WAYLAND_DISPLAY").map(PathBuf::from);
    socket_path_in(&dir, display.as_deref())
}

fn socket_path_in(dir: &Path, display: Option<&Path>) -> Option<PathBuf> {
    if let Some(display) = display {
        return Some(if display.is_absolute() {
            display.to_path_buf()
        } else {
            dir.join(display)
        });
    }
    wayland_sockets_in(dir)
        .into_iter()
        .next()
        .map(|name| dir.join(name))
}

/// Sorted `wayland-<n>` socket names in `$XDG_RUNTIME_DIR` (`.lock`
/// companions excluded). Also used for failure diagnostics.
fn wayland_sockets() -> Vec<String> {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return Vec::new();
    };
    wayland_sockets_in(Path::new(&dir))
}

fn wayland_sockets_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix("wayland-")
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn retries_not_ready_but_unsupported_goes_straight_to_fallback() {
        let attempts = std::cell::Cell::new(0);
        let result = connect_idle_watch_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err(TryError::NotReady(anyhow!("not ready")))
                } else {
                    Ok(7u32)
                }
            },
            || async { Err(anyhow!("fallback must not run")) },
        )
        .await
        .unwrap();
        assert_eq!(result, 7);
        assert_eq!(attempts.get(), 3);
        attempts.set(0);
        let result = connect_idle_watch_with(
            || {
                attempts.set(attempts.get() + 1);
                Err(TryError::Unsupported(anyhow!("no idle protocol")))
            },
            || async { Ok(9u32) },
        )
        .await
        .unwrap();
        assert_eq!(result, 9);
        assert_eq!(attempts.get(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_retry_reports_both_transport_failures() {
        let attempts = std::cell::Cell::new(0);
        let error = connect_idle_watch_with(
            || {
                attempts.set(attempts.get() + 1);
                Err(TryError::NotReady(anyhow!("socket absent")))
            },
            || async { Err::<(), _>(anyhow!("monitor absent")) },
        )
        .await
        .unwrap_err();
        assert_eq!(attempts.get(), 15);
        let report = format!("{error:#}");
        assert!(report.contains("socket absent"));
        assert!(report.contains("monitor absent"));
        assert!(report.contains("attach greeter idle watch"));
    }

    #[test]
    fn discovers_sorted_sockets_and_resolves_explicit_display_paths() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(socket_path_in(directory.path(), None), None);
        for name in [
            "wayland-2",
            "wayland-0",
            "wayland-0.lock",
            "wayland-x",
            "wayland-",
        ] {
            std::fs::write(directory.path().join(name), "").unwrap();
        }
        assert_eq!(
            wayland_sockets_in(directory.path()),
            ["wayland-0", "wayland-2"]
        );
        assert_eq!(
            socket_path_in(directory.path(), None),
            Some(directory.path().join("wayland-0"))
        );
        assert_eq!(
            socket_path_in(directory.path(), Some(Path::new("chosen"))),
            Some(directory.path().join("chosen"))
        );
        assert_eq!(
            socket_path_in(directory.path(), Some(Path::new("/absolute/socket"))),
            Some("/absolute/socket".into())
        );
        assert!(wayland_sockets_in(&directory.path().join("missing")).is_empty());
        let (sender, _receiver) = mpsc::unbounded_channel();
        assert!(matches!(
            try_wayland_at(None, 1, sender.clone()),
            Err(TryError::NotReady(_))
        ));
        assert!(matches!(
            try_wayland_at(Some(directory.path().join("missing")), 1, sender),
            Err(TryError::NotReady(_))
        ));
    }

    struct Wire {
        _directory: tempfile::TempDir,
        socket: PathBuf,
        router: zenoh::Session,
    }

    impl Wire {
        async fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let socket = directory.path().join("greeter.sock");
            let router = zenoh::open(tisvlay_protocol::bootstrap::router_config(&socket).unwrap())
                .await
                .unwrap();
            Self {
                _directory: directory,
                socket,
                router,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publishes_presence_edges_and_cached_idle_without_a_desktop() {
        let wire = Wire::new().await;
        let timeout = wire
            .router
            .declare_publisher(keys::greeter_timeout("test"))
            .cache(CacheConfig::default().max_samples(1))
            .await
            .unwrap();
        timeout.put(r#"{"seconds":2}"#).await.unwrap();
        let subscriber = wire
            .router
            .declare_subscriber(keys::greeter_idle("test"))
            .await
            .unwrap();
        let (attached, attachment) = tokio::sync::oneshot::channel();
        let socket = wire.socket.clone();
        let task = tokio::spawn(async move {
            run_with_watch(&socket, "test", |timeout, sender| async move {
                attached.send((timeout, sender)).unwrap();
                Ok(())
            })
            .await
        });
        let (timeout_ms, sender) = tokio::time::timeout(Duration::from_secs(10), attachment)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(timeout_ms, 2000);
        let sample = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_async())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes()).unwrap(),
            json!({"idle": false})
        );
        sender.send((0, IdleTransition::Resumed)).unwrap();
        sender.send((0, IdleTransition::Idled)).unwrap();
        sender.send((0, IdleTransition::Idled)).unwrap();
        let sample = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_async())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes()).unwrap(),
            json!({"idle": true})
        );
        let late = wire
            .router
            .declare_subscriber(keys::greeter_idle("test"))
            .history(HistoryConfig::default().detect_late_publishers())
            .await
            .unwrap();
        let sample = tokio::time::timeout(Duration::from_secs(5), late.recv_async())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes()).unwrap(),
            json!({"idle": true})
        );
        sender.send((0, IdleTransition::Resumed)).unwrap();
        sender.send((0, IdleTransition::Resumed)).unwrap();
        let sample = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_async())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes()).unwrap(),
            json!({"idle": false})
        );
        drop(sender);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), subscriber.recv_async())
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disabled_timeout_asserts_presence_without_attaching_idle_transport() {
        let wire = Wire::new().await;
        let timeout = wire
            .router
            .declare_publisher(keys::greeter_timeout("test"))
            .cache(CacheConfig::default().max_samples(1))
            .await
            .unwrap();
        timeout.put(r#"{"seconds":0}"#).await.unwrap();
        let subscriber = wire
            .router
            .declare_subscriber(keys::greeter_idle("test"))
            .await
            .unwrap();
        run_with_watch(&wire.socket, "test", |_, _| async {
            Err::<(), _>(anyhow!("disabled timeout must not attach"))
        })
        .await
        .unwrap();
        let sample = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_async())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&sample.payload().to_bytes()).unwrap(),
            json!({"idle": false})
        );
    }

    #[test]
    fn first_transition_always_sends() {
        assert_eq!(next_notify(IdleTransition::Idled, None), Some(true));
        assert_eq!(next_notify(IdleTransition::Resumed, None), Some(false));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_or_absent_timeout_is_disabled() {
        let wire = Wire::new().await;
        let publisher = wire
            .router
            .declare_publisher(keys::greeter_timeout("test"))
            .cache(CacheConfig::default().max_samples(1))
            .await
            .unwrap();
        for body in [
            "not-json",
            r#"{"seconds":"invalid"}"#,
            r#"{"seconds":-1}"#,
            "{}",
        ] {
            publisher.put(body).await.unwrap();
            assert_eq!(read_timeout(&wire.router, "test").await, 0);
        }
        assert_eq!(read_timeout(&wire.router, "absent").await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn large_timeout_is_clamped_and_attach_failure_is_returned() {
        let wire = Wire::new().await;
        let publisher = wire
            .router
            .declare_publisher(keys::greeter_timeout("test"))
            .cache(CacheConfig::default().max_samples(1))
            .await
            .unwrap();
        publisher
            .put(serde_json::to_vec(&json!({"seconds":u64::MAX})).unwrap())
            .await
            .unwrap();
        let error = run_with_watch(&wire.socket, "test", |timeout, _sender| async move {
            assert_eq!(timeout, u32::MAX);
            Err::<(), _>(anyhow!("attach failed"))
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("attach failed"));
    }

    #[test]
    fn repeated_same_state_is_skipped() {
        assert_eq!(next_notify(IdleTransition::Idled, Some(true)), None);
        assert_eq!(next_notify(IdleTransition::Resumed, Some(false)), None);
    }

    #[test]
    fn opposite_state_sends() {
        assert_eq!(
            next_notify(IdleTransition::Resumed, Some(true)),
            Some(false)
        );
        assert_eq!(next_notify(IdleTransition::Idled, Some(false)), Some(true));
    }
}

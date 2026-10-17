use super::*;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use wayland_protocols::ext::idle_notify::v1::server::{
    ext_idle_notification_v1::ExtIdleNotificationV1 as ServerNotification,
    ext_idle_notifier_v1::{self as server_notifier, ExtIdleNotifierV1 as ServerNotifier},
};
use wayland_server::protocol::wl_seat::WlSeat as ServerSeat;
use wayland_server::{
    Client, DataInit, Dispatch as ServerDispatch, Display, DisplayHandle, GlobalDispatch, New,
    Resource,
};

struct ServerState {
    registrations: Arc<Mutex<Vec<(u32, u32, u32)>>>,
}

impl GlobalDispatch<ServerSeat, ()> for ServerState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ServerSeat>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        let seat = init.init(resource, ());
        seat.capabilities(wayland_server::protocol::wl_seat::Capability::Pointer);
    }
}

impl ServerDispatch<ServerSeat, ()> for ServerState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ServerSeat,
        _request: <ServerSeat as Resource>::Request,
        _data: &(),
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl GlobalDispatch<ServerNotifier, ()> for ServerState {
    fn bind(
        _state: &mut Self,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<ServerNotifier>,
        _data: &(),
        init: &mut DataInit<'_, Self>,
    ) {
        init.init(resource, ());
    }
}

impl ServerDispatch<ServerNotifier, ()> for ServerState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ServerNotifier,
        request: server_notifier::Request,
        _data: &(),
        _display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if let server_notifier::Request::GetIdleNotification { id, timeout, seat } = request {
            state
                .registrations
                .lock()
                .unwrap()
                .push((timeout, resource.version(), seat.version()));
            let notification = init.init(id, ());
            notification.idled();
            notification.resumed();
        }
    }
}

impl ServerDispatch<ServerNotification, ()> for ServerState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ServerNotification,
        _request: <ServerNotification as Resource>::Request,
        _data: &(),
        _display: &DisplayHandle,
        _init: &mut DataInit<'_, Self>,
    ) {
    }
}

struct Compositor {
    stop: Option<OwnedFd>,
    thread: Option<thread::JoinHandle<()>>,
    registrations: Arc<Mutex<Vec<(u32, u32, u32)>>>,
}

impl Compositor {
    fn new(notifier: bool, seat: bool) -> (Self, UnixStream) {
        let mut display = Display::<ServerState>::new().unwrap();
        if notifier {
            display
                .handle()
                .create_global::<ServerState, ServerNotifier, _>(
                    ServerNotifier::interface().version,
                    (),
                );
        }
        if seat {
            display
                .handle()
                .create_global::<ServerState, ServerSeat, _>(ServerSeat::interface().version, ());
        }
        let (client, server) = UnixStream::pair().unwrap();
        display
            .handle()
            .insert_client(server, Arc::new(()))
            .unwrap();
        let (stop_read, stop_write) = rustix::pipe::pipe().unwrap();
        let registrations = Arc::new(Mutex::new(Vec::new()));
        let mut state = ServerState {
            registrations: registrations.clone(),
        };
        let thread = thread::spawn(move || {
            loop {
                let stopped = {
                    let mut descriptors = [
                        PollFd::new(&display, PollFlags::IN),
                        PollFd::new(&stop_read, PollFlags::HUP),
                    ];
                    poll(&mut descriptors, None).unwrap();
                    descriptors[1].revents().contains(PollFlags::HUP)
                };
                if stopped {
                    break;
                }
                display.dispatch_clients(&mut state).unwrap();
                display.flush_clients().unwrap();
            }
        });
        (
            Self {
                stop: Some(stop_write),
                thread: Some(thread),
                registrations,
            },
            client,
        )
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        drop(self.stop.take());
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn reports_missing_idle_protocol_without_using_the_desktop() {
    let (_server, client) = Compositor::new(false, false);
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = start_idle_watch_on_socket(client, Vec::new(), sender).unwrap_err();
    assert!(
        matches!(error, WaylandIdleError::ProtocolNotSupported(message) if message.contains("ext_idle_notifier_v1"))
    );
}

#[test]
fn reports_missing_seat() {
    let (_server, client) = Compositor::new(true, false);
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = start_idle_watch_on_socket(client, Vec::new(), sender).unwrap_err();
    assert!(
        matches!(error, WaylandIdleError::ProtocolNotSupported(message) if message.contains("wl_seat"))
    );
}

#[tokio::test]
async fn forwards_tags_caps_versions_and_stops_when_handle_drops() {
    let (server, client) = Compositor::new(true, true);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let handle = start_idle_watch_on_socket(
        client,
        vec![
            IdleNotification {
                tag: 7,
                timeout_ms: 100,
            },
            IdleNotification {
                tag: 42,
                timeout_ms: 900,
            },
        ],
        sender,
    )
    .unwrap();
    for tag in [7, 42] {
        for transition in [IdleTransition::Idled, IdleTransition::Resumed] {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), receiver.recv())
                    .await
                    .unwrap(),
                Some((tag, transition))
            );
        }
    }
    assert_eq!(
        *server.registrations.lock().unwrap(),
        [(100, 1, 1), (900, 1, 1)]
    );
    drop(handle);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn compositor_disconnect_closes_transition_channel() {
    let (server, client) = Compositor::new(true, true);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let handle = start_idle_watch_on_socket(
        client,
        vec![IdleNotification {
            tag: 9,
            timeout_ms: 50,
        }],
        sender,
    )
    .unwrap();
    for transition in [IdleTransition::Idled, IdleTransition::Resumed] {
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), receiver.recv())
                .await
                .unwrap(),
            Some((9, transition))
        );
    }
    drop(server);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap(),
        None
    );
    drop(handle);
}

#[test]
fn a_closed_compositor_socket_returns_a_connection_error() {
    let (client, server) = UnixStream::pair().unwrap();
    drop(server);
    let (sender, _receiver) = mpsc::unbounded_channel();
    assert!(matches!(
        start_idle_watch_on_socket(client, Vec::new(), sender),
        Err(WaylandIdleError::ConnectionFailed(_))
    ));
}

#[tokio::test]
async fn environment_bootstrap_reports_an_absent_compositor() {
    if std::env::var_os("TISVLAY_WAYLAND_TEST_CHILD").is_some() {
        let (sender, _receiver) = mpsc::unbounded_channel();
        assert!(matches!(
            start_idle_watch(Vec::new(), sender),
            Err(WaylandIdleError::ConnectionFailed(_))
        ));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wayland::tests::environment_bootstrap_reports_an_absent_compositor",
            ])
            .env("TISVLAY_WAYLAND_TEST_CHILD", "1")
            .env("XDG_RUNTIME_DIR", directory.path())
            .env("WAYLAND_DISPLAY", "missing-test-compositor")
            .env_remove("WAYLAND_SOCKET")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "child test failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_closed_consumer_does_not_panic() {
    let (sender, receiver) = mpsc::unbounded_channel();
    drop(receiver);
    let state = WaylandState::new(sender);
    state.forward(7, IdleTransition::Idled);
    assert!(state.tx.is_closed());
}

#[test]
fn startup_errors_identify_the_failing_stage() {
    for (error, expected) in [
        (
            WaylandIdleError::ConnectionFailed("offline".into()),
            "Wayland connection failed: offline",
        ),
        (
            WaylandIdleError::ProtocolNotSupported("idle".into()),
            "Protocol not supported: idle",
        ),
        (
            WaylandIdleError::ThreadSpawnFailed("limit".into()),
            "Thread spawn failed: limit",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
    }
}

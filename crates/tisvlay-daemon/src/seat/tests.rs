use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

struct Manager {
    seats: Arc<RwLock<Vec<(String, OwnedObjectPath)>>>,
}

#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Manager {
    fn list_seats(&self) -> Vec<(String, OwnedObjectPath)> {
        self.seats.read().unwrap().clone()
    }

    #[zbus(signal)]
    async fn seat_new(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: &str,
        path: &OwnedObjectPath,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn seat_removed(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        id: &str,
        path: &OwnedObjectPath,
    ) -> zbus::Result<()>;
}

struct LoginSeat {
    active: Arc<RwLock<OwnedObjectPath>>,
    fail: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.login1.Seat")]
impl LoginSeat {
    #[zbus(property)]
    fn active_session(&self) -> zbus::fdo::Result<(String, OwnedObjectPath)> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(zbus::fdo::Error::Failed("seat read failed".into()));
        }
        let path = self.active.read().unwrap().clone();
        Ok((
            if path.as_str() == "/" {
                String::new()
            } else {
                "test".into()
            },
            path,
        ))
    }
}

fn grants(path: &std::path::Path) -> BTreeSet<u32> {
    let Some(blob) = acl::getxattr(path).unwrap() else {
        return BTreeSet::new();
    };
    assert_eq!(&blob[..4], &2u32.to_le_bytes());
    blob[4..]
        .as_chunks::<8>()
        .0
        .iter()
        .filter(|entry| u16::from_le_bytes([entry[0], entry[1]]) == 2)
        .map(|entry| u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]))
        .collect()
}

async fn wait_for_grants(path: &std::path::Path, expected: BTreeSet<u32>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        while grants(path) != expected {
            tick.tick().await;
        }
    })
    .await
    .unwrap();
}

struct LoginSession {
    uid: u32,
    class: String,
    kind: String,
    remote: bool,
}

#[zbus::interface(name = "org.freedesktop.login1.Session")]
impl LoginSession {
    #[zbus(property)]
    fn user(&self) -> (u32, OwnedObjectPath) {
        (self.uid, OwnedObjectPath::try_from("/").unwrap())
    }

    #[zbus(property)]
    fn class(&self) -> &str {
        &self.class
    }

    #[zbus(property, name = "Type")]
    fn session_type(&self) -> &str {
        &self.kind
    }

    #[zbus(property)]
    fn remote(&self) -> bool {
        self.remote
    }
}

#[tokio::test]
async fn only_local_human_desktop_sessions_qualify() {
    let bus = tisvlay_test::dbus::PrivateBus::isolated();
    let service = bus.connect().await.unwrap();
    service
        .request_name("org.freedesktop.login1")
        .await
        .unwrap();
    let peer = bus.connect().await.unwrap();
    for (index, (class, kind, remote, qualifies)) in [
        ("user", "wayland", false, true),
        ("user", "x11", false, true),
        ("greeter", "wayland", false, false),
        ("user", "tty", false, false),
        ("user", "wayland", true, false),
        ("user", "x11", true, false),
    ]
    .into_iter()
    .enumerate()
    {
        let path =
            OwnedObjectPath::try_from(format!("/org/freedesktop/login1/session/test{index}"))
                .unwrap();
        service
            .object_server()
            .at(
                path.clone(),
                LoginSession {
                    uid: 4242,
                    class: class.into(),
                    kind: kind.into(),
                    remote,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            qualifying_uid(&peer, &path).await,
            qualifies.then_some(4242),
            "{class}/{kind}/remote={remote}"
        );
    }
    assert_eq!(
        qualifying_uid(&peer, &OwnedObjectPath::try_from("/missing").unwrap()).await,
        None
    );
}

#[tokio::test]
async fn seat_changes_reconcile_real_socket_acls_and_failed_reads_hold_grants() {
    let directory = tisvlay_test::socket_tempdir();
    let socket = directory.path().join("seat.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // The environment refuses xattrs outright, so there are no ACLs to
    // reconcile: the Nix build sandbox answers ENOTSUP, a container
    // seccomp policy may answer EPERM (as in `acl`'s round-trip test).
    if let Err(e) = acl::getxattr(&socket)
        && matches!(e.raw_os_error(), Some(libc::ENOTSUP | libc::EPERM))
    {
        return;
    }
    let bus = tisvlay_test::dbus::PrivateBus::isolated();
    let service = bus.connect().await.unwrap();
    service
        .request_name("org.freedesktop.login1")
        .await
        .unwrap();
    let first = OwnedObjectPath::try_from("/org/freedesktop/login1/session/first").unwrap();
    let second = OwnedObjectPath::try_from("/org/freedesktop/login1/session/second").unwrap();
    for (path, uid) in [(first.clone(), 4242), (second.clone(), 4243)] {
        service
            .object_server()
            .at(
                path,
                LoginSession {
                    uid,
                    class: "user".into(),
                    kind: "wayland".into(),
                    remote: false,
                },
            )
            .await
            .unwrap();
    }
    let seat_path = OwnedObjectPath::try_from("/org/freedesktop/login1/seat/seat0").unwrap();
    let active = Arc::new(RwLock::new(first.clone()));
    let fail = Arc::new(AtomicBool::new(false));
    service
        .object_server()
        .at(
            seat_path.clone(),
            LoginSeat {
                active: active.clone(),
                fail: fail.clone(),
            },
        )
        .await
        .unwrap();
    let seats = Arc::new(RwLock::new(vec![("seat0".into(), seat_path.clone())]));
    service
        .object_server()
        .at(
            "/org/freedesktop/login1",
            Manager {
                seats: seats.clone(),
            },
        )
        .await
        .unwrap();
    let peer = bus.connect().await.unwrap();
    let watch_peer = peer.clone();
    let watch_socket = socket.clone();
    let watcher = tokio::spawn(async move {
        watch_seats(
            watch_peer,
            &watch_socket,
            &GreeterAccess::Disabled,
            Duration::from_millis(100),
        )
        .await
    });
    wait_for_grants(&socket, BTreeSet::from([4242])).await;
    assert_eq!(grants(directory.path()), BTreeSet::from([4242]));
    acl::reconcile(&socket, &GreeterAccess::Disabled, &BTreeSet::new());
    assert!(grants(&socket).is_empty());
    wait_for_grants(&socket, BTreeSet::from([4242])).await;
    let seat = service
        .object_server()
        .interface::<_, LoginSeat>(seat_path.clone())
        .await
        .unwrap();
    *active.write().unwrap() = second;
    seat.get()
        .await
        .active_session_changed(seat.signal_emitter())
        .await
        .unwrap();
    wait_for_grants(&socket, BTreeSet::from([4243])).await;

    fail.store(true, Ordering::SeqCst);
    let uncached = Login1SeatProxy::builder(&peer)
        .path(seat_path)
        .unwrap()
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
        .unwrap();
    reconcile_seats(&peer, &[uncached], &socket, &GreeterAccess::Disabled).await;
    assert_eq!(grants(&socket), BTreeSet::from([4243]));
    fail.store(false, Ordering::SeqCst);
    *active.write().unwrap() = OwnedObjectPath::try_from("/").unwrap();
    seat.get()
        .await
        .active_session_changed(seat.signal_emitter())
        .await
        .unwrap();
    wait_for_grants(&socket, BTreeSet::new()).await;

    let added_path = OwnedObjectPath::try_from("/org/freedesktop/login1/seat/seat1").unwrap();
    service
        .object_server()
        .at(
            added_path.clone(),
            LoginSeat {
                active: Arc::new(RwLock::new(first)),
                fail: Arc::new(AtomicBool::new(false)),
            },
        )
        .await
        .unwrap();
    seats
        .write()
        .unwrap()
        .push(("seat1".into(), added_path.clone()));
    let manager = service
        .object_server()
        .interface::<_, Manager>("/org/freedesktop/login1")
        .await
        .unwrap();
    Manager::seat_new(manager.signal_emitter(), "seat1", &added_path)
        .await
        .unwrap();
    wait_for_grants(&socket, BTreeSet::from([4242])).await;
    seats.write().unwrap().retain(|(id, _)| id != "seat1");
    Manager::seat_removed(manager.signal_emitter(), "seat1", &added_path)
        .await
        .unwrap();
    wait_for_grants(&socket, BTreeSet::new()).await;
    assert!(grants(directory.path()).is_empty());
    watcher.abort();
    assert!(watcher.await.unwrap_err().is_cancelled());
}

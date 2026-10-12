//! Consumed-D-Bus adapter test: a fake logind on a private bus drives the
//! world's sleep/resume actions end to end (signal → system event → stock
//! profile → mock backend → state publish on the wire), including after
//! the watcher's bus connection is dropped.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_protocol::caller::Caller;
use tisvlay_protocol::endpoint::manager;
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_test::PrivateBus;
use tisvlay_test::fake_logind::FakeLogind;
use uuid::Uuid;
use zenoh_ext::{AdvancedSubscriberBuilderExt, HistoryConfig};

type PowerSub =
    zenoh_ext::AdvancedSubscriber<zenoh::handlers::FifoChannelHandler<zenoh::sample::Sample>>;

/// A daemon with one managed mock TV, already active, plus a fake logind
/// on a private bus. The watcher is the test's to start.
struct Fixture {
    bus: PrivateBus,
    fake: FakeLogind,
    stack: tisvlay_daemon::Stack,
    sub: PowerSub,
    id: Uuid,
    _client: zenoh::Session,
    _dir: tempfile::TempDir,
    _service_conn: zbus::Connection,
}

async fn fixture() -> Fixture {
    let bus = PrivateBus::isolated();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");

    let dir = tisvlay_test::socket_tempdir();
    let socket = dir.path().join("l.sock");
    let (connector, discovery, _handle) = new_test_parts();
    let stack = tisvlay_daemon::serve(
        &socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();

    let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
        .await
        .unwrap();
    // Token-after-queryables ordering makes a visible liveliness token
    // mean the rpc surface is ready — required before the raw get
    // below (a get matching zero queryables returns instantly empty).
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let replies = client
                .liveliness()
                .get(keys::daemon_alive("t"))
                .await
                .unwrap();
            if replies.recv_async().await.is_ok() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "daemon liveliness never became visible"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    let request = rpc::AddTv {
        id: None,
        name: "Den".into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": "mock://logind" })).unwrap(),
    };
    let id = Caller::new(client.clone(), "t")
        .call::<manager::AddTv>(&(), &request)
        .await
        .expect("add-tv ok")
        .id;

    let sub = client
        .declare_subscriber(keys::tv_all("t"))
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap();
    wait_power(&sub, id, "active").await;
    Fixture {
        bus,
        fake,
        stack,
        sub,
        id,
        _client: client,
        _dir: dir,
        _service_conn: service_conn,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn logind_sleep_resume_drive_profile_actions() {
    let f = fixture().await;
    let watcher_conn = f.bus.connect().await.expect("watcher conn");
    let world = f.stack.world.clone();
    tokio::spawn(async move {
        let _ = tisvlay_daemon::watch_logind(watcher_conn, world).await;
    });

    // Give the watcher a beat to be subscribed before emitting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Stock mapping (root-parity): sleep powers the TV off, resume wakes
    // it back through the wake dial.
    f.fake.emit_prepare_for_sleep(true).await.unwrap();
    wait_power(&f.sub, f.id, "standby").await;

    f.fake.emit_prepare_for_sleep(false).await.unwrap();
    wait_power(&f.sub, f.id, "active").await;
}

/// The bus dropping the watcher's connection (as dbus-broker does to a
/// peer whose user is over quota) must not end sleep handling: the
/// supervisor reconnects and the next sleep still powers the TV off.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_bus_connection_reconnects() {
    let f = fixture().await;
    let address = f.bus.address().to_owned();
    let opened: Arc<Mutex<Vec<zbus::Connection>>> = Arc::default();
    let connect = {
        let opened = opened.clone();
        move || {
            let address = address.clone();
            let opened = opened.clone();
            async move {
                let connection = zbus::connection::Builder::address(address.as_str())?
                    .build()
                    .await?;
                opened.lock().unwrap().push(connection.clone());
                Ok(connection)
            }
        }
    };
    tokio::spawn(tisvlay_daemon::supervise_logind(
        f.stack.world.clone(),
        connect,
        Duration::from_millis(50),
    ));

    let first = wait_connections(&opened, 1).await;
    first.close().await.expect("close the watcher's connection");
    let _reconnected = wait_connections(&opened, 2).await;

    // Give the new watch a beat to subscribe before emitting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    f.fake.emit_prepare_for_sleep(true).await.unwrap();
    wait_power(&f.sub, f.id, "standby").await;
}

/// The `n`th connection the supervisor opened, once it has opened that
/// many.
async fn wait_connections(opened: &Mutex<Vec<zbus::Connection>>, n: usize) -> zbus::Connection {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(connection) = opened.lock().unwrap().get(n - 1) {
            return connection.clone();
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the supervisor never opened connection {n}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_power(sub: &PowerSub, id: Uuid, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .expect("timed out waiting for power state")
            .expect("subscriber closed");
        if let Some((tv, path)) = keys::parse_tv_path("t", sample.key_expr().as_str())
            && tv == id
            && path == "state/power"
        {
            let value: Value =
                serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null);
            if value["state"] == want {
                return;
            }
        }
    }
}

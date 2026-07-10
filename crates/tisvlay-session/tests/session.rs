//! P4 session-service wire tests: gateway isolation (user/** never
//! transits the system peer), lock/unlock/idle dispatch through user
//! profiles, the ScreenSaver adapter, persistence, and the idle-timeout
//! watch.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_session::{SessionEvent, SessionOptions, SessionStack};
use tisvlay_wire::{bootstrap, keys, rpc};
use uuid::Uuid;
use zenoh::Session;
use zenoh::bytes::Encoding;
use zenoh::handlers::FifoChannelHandler;
use zenoh::sample::Sample;
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig};

type Sub = AdvancedSubscriber<FifoChannelHandler<Sample>>;

/// One daemon + one session service + the two client sessions a user
/// client holds (a zenoh client keeps a single link, so system and user
/// worlds are reached through separate sessions).
struct Rig {
    _dir: tempfile::TempDir,
    system_socket: PathBuf,
    stack: SessionStack,
    /// Client on the system socket — TV state and rpcs.
    client: Session,
    /// Client on the user socket — user profiles and assignments.
    user: Session,
    site: String,
    mock: tisvlay_mock::MockHandle,
}

async fn rig() -> Rig {
    rig_with_store(None).await
}

async fn rig_with_store(store: Option<&Path>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let (connector, discovery, handle) = new_test_parts();
    let _daemon = tisvlay_daemon::serve(
        &system_socket,
        "t",
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    let stack = tisvlay_session::serve(SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: store.map(Path::to_path_buf),
    })
    .await
    .unwrap();
    let client = zenoh::open(bootstrap::client_config(&system_socket).unwrap())
        .await
        .unwrap();
    let user = zenoh::open(bootstrap::client_config(&user_socket).unwrap())
        .await
        .unwrap();
    Rig {
        _dir: dir,
        system_socket,
        stack,
        client,
        user,
        site: "t".into(),
        mock: handle,
    }
}

async fn history_sub(session: &Session, key: String) -> Sub {
    session
        .declare_subscriber(key)
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap()
}

/// A get that matches zero queryables completes instantly with no
/// reply — the served side may still be declaring its queryables —
/// so the no-reply case retries briefly.
async fn call(client: &Session, key: &str, body: Value) -> Result<Value, Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let replies = client
            .get(key.to_string())
            .payload(serde_json::to_vec(&body).unwrap())
            .encoding(Encoding::APPLICATION_JSON)
            .timeout(Duration::from_secs(5))
            .await
            .unwrap();
        let reply = match tokio::time::timeout(Duration::from_secs(5), replies.recv_async())
            .await
            .expect("rpc timed out")
        {
            Ok(reply) => reply,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
            Err(e) => panic!("rpc got no reply: {e}"),
        };
        return match reply.result() {
            Ok(sample) => {
                Ok(serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null))
            }
            Err(err) => {
                Err(serde_json::from_slice(&err.payload().to_bytes()).unwrap_or(Value::Null))
            }
        };
    }
}

async fn recv_until(sub: &Sub, mut pred: impl FnMut(&Sample) -> bool) -> Vec<Sample> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    loop {
        let sample = tokio::time::timeout_at(deadline, sub.recv_async())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "timed out; saw: {:?}",
                    seen.iter()
                        .map(|s: &Sample| format!("{} {}", s.key_expr(), payload_json(s)))
                        .collect::<Vec<_>>()
                )
            })
            .expect("subscriber closed");
        let done = pred(&sample);
        seen.push(sample);
        if done {
            return seen;
        }
    }
}

async fn assert_no_sample(sub: &Sub, window: Duration, mut pred: impl FnMut(&Sample) -> bool) {
    let deadline = tokio::time::Instant::now() + window;
    while let Ok(Ok(sample)) = tokio::time::timeout_at(deadline, sub.recv_async()).await {
        assert!(!pred(&sample), "unexpected sample on {}", sample.key_expr());
    }
}

fn payload_json(sample: &Sample) -> Value {
    serde_json::from_slice(&sample.payload().to_bytes()).unwrap_or(Value::Null)
}

async fn add_tv(rig: &Rig, name: &str, address: &str) -> Uuid {
    let body = serde_json::to_value(rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: json!({ "address": address }),
    })
    .unwrap();
    let ok = call(&rig.client, &keys::manager_rpc(&rig.site, "add-tv"), body)
        .await
        .expect("add-tv ok");
    Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap()
}

async fn wait_tv_state(sub: &Sub, site: &str, id: Uuid, path: &str, pred: impl Fn(&Value) -> bool) {
    wait_all_tv_state(sub, site, [id], path, pred).await;
}

/// Wait until every TV in `ids` has matched once — a single drain, so one
/// TV's sample can't be consumed while waiting on another.
async fn wait_all_tv_state<const N: usize>(
    sub: &Sub,
    site: &str,
    ids: [Uuid; N],
    path: &str,
    pred: impl Fn(&Value) -> bool,
) {
    let mut pending: Vec<Uuid> = ids.into();
    recv_until(sub, |s| {
        if let Some((tv, p)) = keys::parse_tv_path(site, s.key_expr().as_str())
            && p == path
            && pending.contains(&tv)
            && pred(&payload_json(s))
        {
            pending.retain(|id| *id != tv);
        }
        pending.is_empty()
    })
    .await;
}

/// Inject a session event repeatedly until the expected TV state lands —
/// absorbs the session core's fleet-mirror hydration lag (dispatch is
/// idempotent).
async fn drive_until(rig: &Rig, sub: &Sub, event: SessionEvent, id: Uuid, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(rig.stack.client.send(event), "session core gone");
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            let recv = tokio::time::timeout_at(step, sub.recv_async()).await;
            match recv {
                Ok(Ok(sample)) => {
                    if keys::parse_tv_path(&rig.site, sample.key_expr().as_str())
                        .is_some_and(|(tv, p)| tv == id && p == "state/power")
                        && payload_json(&sample)["state"] == want
                    {
                        return;
                    }
                }
                Ok(Err(_)) => panic!("subscriber closed"),
                Err(_) => break,
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "TV never reached {want} for {event:?}"
        );
    }
}

/// The gateway invariant: user keys exist on the user socket only.
#[tokio::test(flavor = "multi_thread")]
async fn gateway_isolates_the_user_subtree() {
    let rig = rig().await;
    // A second session service for another "user", same site.
    let dir2 = tempfile::tempdir().unwrap();
    let user2_socket = dir2.path().join("user2.sock");
    let _stack2 = tisvlay_session::serve(SessionOptions {
        system_socket: rig.system_socket.clone(),
        user_socket: user2_socket.clone(),
        site: rig.site.clone(),
        store: None,
    })
    .await
    .unwrap();

    let ok = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "create"),
        json!({ "body": { "name": "Mine", "on_session_lock": "none" } }),
    )
    .await
    .expect("create ok");
    let profile = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();

    // The owner sees it (two-endpoint client, hydrated from cache)…
    let mine = history_sub(&rig.user, keys::user_all(&rig.site)).await;
    recv_until(&mine, |s| {
        s.key_expr().as_str() == keys::user_profile(&rig.site, &profile)
            && payload_json(s)["name"] == "Mine"
    })
    .await;

    // …the system socket never carries it…
    let system_only = zenoh::open(bootstrap::client_config(&rig.system_socket).unwrap())
        .await
        .unwrap();
    let leak = history_sub(&system_only, keys::user_all(&rig.site)).await;
    assert_no_sample(&leak, Duration::from_millis(800), |_| true).await;

    // …and neither does the other user's socket.
    let other = zenoh::open(bootstrap::client_config(&user2_socket).unwrap())
        .await
        .unwrap();
    let cross = history_sub(&other, keys::user_all(&rig.site)).await;
    assert_no_sample(&cross, Duration::from_millis(800), |s| {
        s.key_expr().as_str() == keys::user_profile(&rig.site, &profile)
    })
    .await;

    // The other user's own liveliness token is alive on their socket —
    // the subtree is per-user, not empty.
    let replies = other
        .liveliness()
        .get(keys::user_alive_session(&rig.site))
        .timeout(Duration::from_secs(2))
        .await
        .unwrap();
    let mut alive = false;
    while let Ok(reply) = replies.recv_async().await {
        alive |= reply.result().is_ok();
    }
    assert!(alive, "second session service declares its own liveliness");
}

/// Lock/unlock drive the hard-default user actions on every automated TV,
/// with the observability tap on the system socket.
#[tokio::test(flavor = "multi_thread")]
async fn lock_and_unlock_dispatch_default_actions() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://lock").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let tap = rig
        .client
        .declare_subscriber(format!("tisvlay/{}/event/session/*", rig.site))
        .await
        .unwrap();

    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    // Activity on the lock screen: the hard-default on_lock_wake is
    // screen_on, so the panel comes back before any unlock.
    drive_until(&rig, &sub, SessionEvent::LockWake, tv, "active").await;
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // The tap is a plain put racing the subscriber's interest
    // propagation, so keep re-driving (dispatch is idempotent) until
    // one lands.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    'outer: loop {
        assert!(
            rig.stack.client.send(SessionEvent::Unlock),
            "session core gone"
        );
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(sample)) = tokio::time::timeout_at(step, tap.recv_async()).await {
            if sample.key_expr().as_str().ends_with("/session/unlock") {
                break 'outer;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "unlock tap never observed"
        );
    }
}

/// A user profile's explicit `none` suppresses the lock blank for its TV
/// while unassigned TVs keep the default; deletion is guarded while
/// assigned; defaults/effective expose the resolution.
#[tokio::test(flavor = "multi_thread")]
async fn user_profile_overrides_and_guards() {
    let rig = rig().await;
    let quiet = add_tv(&rig, "Quiet", "mock://u-quiet").await;
    let stock = add_tv(&rig, "Stock", "mock://u-stock").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_all_tv_state(&sub, &rig.site, [quiet, stock], "state/link", |v| {
        v["connected"] == true
    })
    .await;

    let defaults = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "defaults"),
        Value::Null,
    )
    .await
    .expect("defaults ok");
    assert_eq!(
        defaults,
        json!({
            "name": "default",
            "on_session_lock": "screen_off",
            "on_session_unlock": { "action": "screen_on", "restore_input": true },
            "on_lock_wake": "screen_on",
            "locked_idle_timeout_seconds": 0,
            "unlocked_idle_timeout_seconds": 0,
            "on_idle_timeout": "screen_off",
            "on_idle_resume": "screen_on",
        })
    );

    let ok = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "create"),
        json!({ "body": { "name": "No blank", "on_session_lock": "none" } }),
    )
    .await
    .expect("create ok");
    let profile = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "assign"),
        json!({ "tv": quiet, "profile": profile }),
    )
    .await
    .expect("assign ok");

    // The assignment is visible on the user socket.
    let assignment = history_sub(&rig.user, keys::user_tv_profile(&rig.site, &quiet)).await;
    recv_until(&assignment, |s| {
        payload_json(s)["profile"] == profile.to_string()
    })
    .await;

    let effective = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "effective"),
        json!({ "id": quiet }),
    )
    .await
    .expect("effective ok");
    assert_eq!(effective["on_session_lock"], "none");
    assert_eq!(
        effective["on_session_unlock"],
        json!({ "action": "screen_on", "restore_input": true })
    );

    drive_until(&rig, &sub, SessionEvent::Lock, stock, "screenoff").await;
    assert_no_sample(&sub, Duration::from_millis(400), |s| {
        keys::parse_tv_path(&rig.site, s.key_expr().as_str())
            .is_some_and(|(tv, p)| tv == quiet && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    let err = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "delete"),
        json!({ "id": profile }),
    )
    .await
    .expect_err("delete while assigned must fail");
    assert_eq!(err["code"], "profile-in-use");
    call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "assign"),
        json!({ "tv": quiet }),
    )
    .await
    .expect("unassign ok");
    call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "delete"),
        json!({ "id": profile }),
    )
    .await
    .expect("delete ok");
}

/// The consumed-D-Bus adapter: FakeScreenSaver ActiveChanged transitions
/// drive lock/unlock end to end.
#[tokio::test(flavor = "multi_thread")]
async fn screensaver_adapter_drives_lock_unlock() {
    let bus = tisvlay_test::PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_screensaver::FakeScreenSaver::register(&service_conn)
        .await
        .expect("fake screensaver");

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://saver").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    let watcher_conn = bus.connect().await.expect("watcher conn");
    let client = rig.stack.client.clone();
    let is_locked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked = is_locked.clone();
    tokio::spawn(async move {
        let _ = tisvlay_session::watch_screensaver(watcher_conn, client, locked).await;
    });
    // Watcher subscription + session fleet mirror settle.
    tokio::time::sleep(Duration::from_millis(500)).await;

    fake.emit_active_changed(true).await.unwrap();
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
    assert!(
        is_locked.load(std::sync::atomic::Ordering::SeqCst),
        "lock flag mirrors ActiveChanged(true) for the lock-wake policy"
    );
    fake.emit_active_changed(false).await.unwrap();
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "active"
    })
    .await;
}

/// The idle drift re-assert: a panel that lights itself while the
/// locked-idle action holds (a stray remote press at the lock screen)
/// gets one idle-timeout of grace, then the idle action re-dispatches.
#[tokio::test(flavor = "multi_thread")]
async fn screen_drift_while_idle_reasserts_after_grace() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://drift").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // A short locked-idle timeout keeps the drift grace test-sized.
    let ok = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "create"),
        json!({ "body": {
            "name": "Drift",
            "locked_idle_timeout_seconds": 1,
        } }),
    )
    .await
    .expect("create ok");
    let profile = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "assign"),
        json!({ "tv": tv, "profile": profile }),
    )
    .await
    .expect("assign ok");

    // Locked (as the GetActive seed or a Lock edge leaves it), idle
    // action fired: the TV blanks.
    rig.stack
        .is_locked
        .store(true, std::sync::atomic::Ordering::SeqCst);
    drive_until(&rig, &sub, SessionEvent::IdleTimeout, tv, "screenoff").await;

    // The couch press: the panel lights itself, no desktop activity.
    rig.mock
        .inject_power_state("mock://drift", tisvlay_core::PowerState::Active);
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "active"
    })
    .await;

    // One grace period later the idle action re-asserts and the panel
    // goes dark again — with no further session event.
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
}

/// A lock that predates the watcher (KDE's auto-login lock activates
/// before the session service starts) is seeded from `GetActive` —
/// without the seed the locked-idle timer stays gated off and never
/// blanks the TV.
#[tokio::test(flavor = "multi_thread")]
async fn screensaver_seed_reads_a_preexisting_lock() {
    let bus = tisvlay_test::PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_screensaver::FakeScreenSaver::register(&service_conn)
        .await
        .expect("fake screensaver");
    // The lock lands before anyone subscribes — an edge with no witness.
    fake.emit_active_changed(true).await.unwrap();

    let rig = rig().await;
    let watcher_conn = bus.connect().await.expect("watcher conn");
    let client = rig.stack.client.clone();
    let is_locked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked = is_locked.clone();
    tokio::spawn(async move {
        let _ = tisvlay_session::watch_screensaver(watcher_conn, client, locked).await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !is_locked.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "GetActive seed never landed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Profiles and assignments survive a session-service restart, and the
/// idle-timeout watch tracks the resolved minimum.
#[tokio::test(flavor = "multi_thread")]
async fn user_store_and_idle_watch_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("session.toml");
    let rig = rig_with_store(Some(&store)).await;
    let tv = add_tv(&rig, "Den", "mock://u-store").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    assert_eq!(*rig.stack.idle_timeouts.borrow(), (0, 0));
    let ok = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "create"),
        json!({ "body": {
            "name": "Idle",
            "locked_idle_timeout_seconds": 45,
            "unlocked_idle_timeout_seconds": 120,
        } }),
    )
    .await
    .expect("create ok");
    let profile = Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap();
    call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "assign"),
        json!({ "tv": tv, "profile": profile }),
    )
    .await
    .expect("assign ok");

    // The watch resolves to the enabled minimums once the fleet mirror
    // knows the TV.
    let mut watch = rig.stack.idle_timeouts.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *watch.borrow_and_update() != (45, 120) {
            watch.changed().await.expect("watch closed");
        }
    })
    .await
    .expect("idle timeouts never resolved to (45, 120)");

    // Idle timeout blanks through the same dispatch rails.
    drive_until(&rig, &sub, SessionEvent::IdleTimeout, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::IdleResume, tv, "active").await;

    // Restart: fresh service, fresh user socket, same store.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&store)
            .is_ok_and(|t| t.contains(&profile.to_string()) && t.contains(&tv.to_string()))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "user store never captured profile + assignment"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let user2_socket = dir.path().join("user2.sock");
    let _stack2 = tisvlay_session::serve(SessionOptions {
        system_socket: rig.system_socket.clone(),
        user_socket: user2_socket.clone(),
        site: rig.site.clone(),
        store: Some(store),
    })
    .await
    .unwrap();
    let revived = zenoh::open(bootstrap::client_config(&user2_socket).unwrap())
        .await
        .unwrap();
    let profile_sub = history_sub(&revived, keys::user_profile(&rig.site, &profile)).await;
    recv_until(&profile_sub, |s| payload_json(s)["name"] == "Idle").await;
    let assign_sub = history_sub(&revived, keys::user_tv_profile(&rig.site, &tv)).await;
    recv_until(&assign_sub, |s| {
        payload_json(s)["profile"] == profile.to_string()
    })
    .await;
}

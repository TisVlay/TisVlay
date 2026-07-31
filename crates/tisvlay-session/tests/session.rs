//! P4 session-service wire tests: gateway isolation (user/** never
//! transits the system peer), lock/unlock/idle dispatch through user
//! profiles, the logind lock watcher, persistence, and the idle-timeout
//! watch.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_session::{RemoteReport, SessionEvent, SessionOptions, SessionStack};
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
            // Floor splits by direction: the off-going blanks (lock,
            // idle-timeout) project ungated (`while_remote = true`); the
            // wake-ish events (unlock, lock-wake, idle-resume) stay gated.
            "on_session_lock": { "action": "screen_off", "restore_input": false, "while_remote": true },
            "on_session_unlock": { "action": "screen_on", "restore_input": true, "while_remote": false },
            "on_lock_wake": "screen_on",
            "locked_idle_timeout_seconds": 0,
            "unlocked_idle_timeout_seconds": 0,
            "on_idle_timeout": { "action": "screen_off", "restore_input": false, "while_remote": true },
            "on_idle_resume": "screen_on",
            // Both remote edges floor to screen_off (blank on arm, stay dark
            // on disarm); capture arming ships off. The connect/disconnect
            // actions ignore the gate, so they project the plain default.
            // on_local_return wakes restoring on the first local input after
            // a disconnect (its table shape mirrors on_session_unlock).
            "on_remote_connect": "screen_off",
            "on_remote_disconnect": "screen_off",
            "on_local_return": { "action": "screen_on", "restore_input": true, "while_remote": false },
            "remote_screencast_arms": false,
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
        json!({ "action": "screen_on", "restore_input": true, "while_remote": false })
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

/// The consumed-D-Bus adapter: logind `LockedHint` transitions drive
/// lock/unlock end to end.
#[tokio::test(flavor = "multi_thread")]
async fn lock_watcher_drives_lock_unlock() {
    let bus = tisvlay_test::PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_logind::FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");

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
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    tokio::spawn(async move {
        let _ = tisvlay_session::watch_lock(watcher_conn, client, locked, active).await;
    });
    // Watcher hydration + LockedHint subscription + fleet mirror settle.
    tokio::time::sleep(Duration::from_millis(500)).await;

    fake.set_locked(true).await.unwrap();
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "screenoff"
    })
    .await;
    assert!(
        is_locked.load(std::sync::atomic::Ordering::SeqCst),
        "lock flag mirrors LockedHint(true) for the lock-wake policy"
    );
    fake.set_locked(false).await.unwrap();
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "active"
    })
    .await;
}

/// Active-session arbitration: while the user's session is not the seat's
/// foreground (fast-user-switched away), its seat-local automation is
/// suppressed — a backgrounded user must not drive a TV the active user is
/// using. `is_active` is the shared flag the lock watcher writes; here we
/// drive it directly. Group access is unchanged; only dispatch is gated.
#[tokio::test(flavor = "multi_thread")]
async fn active_session_arbitration_suppresses_backgrounded_automation() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Shared", "mock://arbitration").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // Active (the default): a lock drives the TV off, an unlock back on.
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // Fast-user-switched away: the session is no longer the foreground, so a
    // lock must NOT reach the TV the now-active user is using.
    rig.stack
        .is_active
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        rig.stack.client.send(SessionEvent::Lock),
        "session core gone"
    );
    assert_no_sample(&sub, Duration::from_millis(800), |s| {
        keys::parse_tv_path(&rig.site, s.key_expr().as_str())
            .is_some_and(|(t, p)| t == tv && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    // Foreground again: dispatch resumes and the lock drives the TV off.
    rig.stack
        .is_active
        .store(true, std::sync::atomic::Ordering::SeqCst);
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
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

    // Locked (as the LockedHint seed or a Lock edge leaves it), idle
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

/// A lock that predates the watcher (an auto-login lock activates before
/// the session service starts) is seeded from `LockedHint` — without the
/// seed the locked-idle timer stays gated off and never blanks the TV.
#[tokio::test(flavor = "multi_thread")]
async fn lock_watcher_seed_reads_a_preexisting_lock() {
    let bus = tisvlay_test::PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_logind::FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");
    // The lock lands before anyone subscribes — an edge with no witness,
    // caught only by the property seed, not a change signal.
    fake.set_locked(true).await.unwrap();

    let rig = rig().await;
    let watcher_conn = bus.connect().await.expect("watcher conn");
    let client = rig.stack.client.clone();
    let is_locked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked = is_locked.clone();
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    tokio::spawn(async move {
        let _ = tisvlay_session::watch_lock(watcher_conn, client, locked, active).await;
    });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !is_locked.load(std::sync::atomic::Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "LockedHint seed never landed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The startup seed observes a pre-existing lock *synchronously*: `is_locked`
/// is set by the time the await returns, before `main` spawns the idle
/// adapter. This is what closes the boot-already-locked race — the once-only
/// locked-idle `ext-idle-notify` transition arrives ~one timeout later and is
/// silently dropped if `is_locked` is still at its `false` default (and the
/// later unlocked-idle transition is dropped too, since the two tags gate on
/// opposite lock states). See the Session-side watcher note in
/// docs/architecture.md.
#[tokio::test(flavor = "multi_thread")]
async fn startup_seed_reads_a_preexisting_lock_synchronously() {
    let bus = tisvlay_test::PrivateBus::shared();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_logind::FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");
    // Locked before the service starts — a property to seed, not an edge.
    fake.set_locked(true).await.unwrap();

    let seed_conn = bus.connect().await.expect("seed conn");
    let is_locked = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let is_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    tisvlay_session::seed_lock_state_on(&seed_conn, &is_locked, &is_active).await;

    // No spawn, no polling: the seed is complete when the await returns.
    assert!(
        is_locked.load(std::sync::atomic::Ordering::SeqCst),
        "startup seed did not observe the pre-existing LockedHint(true)"
    );
    assert!(
        is_active.load(std::sync::atomic::Ordering::SeqCst),
        "startup seed should read Active=true for the foreground session"
    );
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

/// Create + assign a user profile carrying `body`; returns the profile id.
async fn create_and_assign(rig: &Rig, tv: Uuid, body: Value) -> Uuid {
    let ok = call(
        &rig.user,
        &keys::user_profile_rpc(&rig.site, "create"),
        json!({ "body": body }),
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
    profile
}

/// Toggle a manual source off→on until the TV reaches `want`, absorbing
/// fleet-mirror hydration lag: each on-edge re-fires the connect
/// dispatch, which is idempotent.
async fn arm_until(rig: &Rig, sub: &Sub, source: &str, id: Uuid, want: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            rig.stack.remote.report(RemoteReport::manual(source, false)),
            "session core gone"
        );
        assert!(
            rig.stack.remote.report(RemoteReport::manual(source, true)),
            "session core gone"
        );
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(sample)) = tokio::time::timeout_at(step, sub.recv_async()).await {
            if keys::parse_tv_path(&rig.site, sample.key_expr().as_str())
                .is_some_and(|(tv, p)| tv == id && p == "state/power")
                && payload_json(&sample)["state"] == want
            {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "TV never reached {want} after arming '{source}'"
        );
    }
}

/// While armed, an action whose `while_remote` gate is `false` is
/// suppressed — the physical TV stays dark for the empty room.
#[tokio::test(flavor = "multi_thread")]
async fn armed_remote_suppresses_gated_events() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-suppress").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // Gate the unlock (and idle-resume) wake actions off while armed. The
    // gate rides the action's table form, replacing the whole mapping.
    // `on_session_lock` is left at the stock floor, which is ungated for
    // the off-going blank — the test's setup depends on the lock still
    // blanking while armed, and the floor provides that without an
    // explicit `while_remote = true` here.
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Remote quiet",
            "on_session_unlock": { "action": "screen_on", "while_remote": false },
            "on_idle_resume": { "action": "screen_on", "while_remote": false },
        }),
    )
    .await;

    // Unarmed baseline: unlock wakes the panel (the default action fires).
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // Arm and confirm via the fact (this profile fires no connect
    // action, so the armed set on the fact is the observable).
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;
    assert!(
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", true)),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;

    // Blank via lock. With the source live, unlock is suppressed — the
    // panel must NOT come back.
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    assert!(rig.stack.client.send(SessionEvent::Unlock), "core gone");
    assert_no_sample(&sub, Duration::from_millis(600), |s| {
        keys::parse_tv_path(&rig.site, s.key_expr().as_str())
            .is_some_and(|(t, p)| t == tv && p == "state/power")
            && payload_json(s)["state"] == "active"
    })
    .await;
}

/// A live Sunshine source is refreshed on the wake edge: because the gate
/// and the `/serverinfo` detector share this process, an unlock probes the
/// endpoint synchronously and clears the source when the stream has ended,
/// so the wake fires instead of being suppressed for up to a poll interval.
/// The probe here observes the endpoint absent (no `sunshine` process under
/// test), which disarms exactly as a `FREE` reading would.
#[tokio::test(flavor = "multi_thread")]
async fn gated_wake_eagerly_refreshes_a_stale_sunshine_source() {
    use tisvlay_wire::payload::RemoteSourceKind;

    let sunshine = |active: bool| RemoteReport {
        kind: RemoteSourceKind::Sunshine,
        detail: "serverinfo".into(),
        active,
        provisional: false,
    };

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://eager-sunshine").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // Unlock gated off while armed; the remote edges stay inert (`none`) so
    // arming itself never moves the panel — the lock/unlock pair drives it.
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Sunshine eager",
            "on_session_unlock": { "action": "screen_on", "while_remote": false },
            "on_remote_connect": "none",
            "on_remote_disconnect": "none",
        }),
    )
    .await;

    // Baseline: hydrate the fleet mirror and confirm dispatch reaches the TV.
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // A live Sunshine source arms the TV (attributed kinds always arm).
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;
    assert!(rig.stack.remote.report(sunshine(true)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;

    // Blank via lock (an off-going event fires even while armed), then wake
    // via unlock: the eager pull observes the endpoint gone, clears the
    // stale source, and the unlock's screen_on fires rather than suppressing.
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // The source cleared with the eager refresh — the fact shows it gone.
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"] == json!([]) && v["armed"] == json!([])
        }
    })
    .await;
}

/// The connect/disconnect actions fire on arm and disarm edges.
#[tokio::test(flavor = "multi_thread")]
async fn remote_connect_disconnect_actions_fire() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-edges").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Remote edges",
            "on_remote_connect": "screen_off",
            "on_remote_disconnect": "screen_on",
        }),
    )
    .await;

    // The arm edge fires on_remote_connect (screen_off).
    arm_until(&rig, &sub, "manual", tv, "screenoff").await;
    // The disarm edge fires on_remote_disconnect (screen_on).
    assert!(
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", false)),
        "core gone"
    );
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "active"
    })
    .await;
}

/// After a remote disconnect leaves the panel dark, the first local input
/// wakes it — the case where nothing locked or idled, so no unlock /
/// lock-wake / idle-resume edge would otherwise fire. The core arms the
/// return watch on disconnect; `LocalReturn` (which the idle adapter
/// synthesizes from a lock-wake resume while unlocked + armed) wakes it.
#[tokio::test(flavor = "multi_thread")]
async fn local_return_wakes_after_remote_disconnect() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://local-return").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // Stay-dark on disconnect (the floor); on_local_return inherits the
    // floor wake (screen_on).
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Stay dark",
            "on_remote_connect": "screen_off",
            "on_remote_disconnect": "screen_off",
        }),
    )
    .await;

    // Arm → blank. Connect clears the return watch, so it must be unset.
    arm_until(&rig, &sub, "manual", tv, "screenoff").await;
    assert!(
        !rig.stack
            .local_return_armed
            .load(std::sync::atomic::Ordering::SeqCst),
        "connect must not arm the return watch"
    );

    // Disarm → stays dark, and the return watch arms.
    assert!(
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", false)),
        "core gone"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !rig
        .stack
        .local_return_armed
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "disconnect never armed the return watch"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // First local input after the disconnect wakes the panel...
    drive_until(&rig, &sub, SessionEvent::LocalReturn, tv, "active").await;
    // ...and the dispatch clears the watch (fire-once; the next event
    // supersedes it).
    assert!(
        !rig.stack
            .local_return_armed
            .load(std::sync::atomic::Ordering::SeqCst),
        "the return dispatch clears the watch"
    );
}

/// The fact publishes on the user socket (cached) and mirrors on the
/// system socket (a cache-1 advanced pub) with the live sources + armed
/// projection.
#[tokio::test(flavor = "multi_thread")]
async fn remote_fact_and_mirror_carry_sources_and_armed() {
    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-fact").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    // The user-socket fact is a cached pub (history-subscribable); the
    // system-socket mirror is a cache-1 advanced pub, but this plain
    // subscriber sees only its live samples (a plain get can't reach `@adv`).
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;
    let mirror = rig
        .client
        .declare_subscriber(keys::remote_session(&rig.site))
        .await
        .unwrap();

    // Arm; both surfaces must show the manual source and the armed TV.
    // Re-drive the arm edge (the mirror's live sample races subscriber
    // interest).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let armed_ok = |v: &Value| {
        v["sources"]
            .as_array()
            .is_some_and(|s| s.iter().any(|src| src["kind"] == "manual"))
            && v["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    };
    // The cached fact hydrates from history — assert it once armed.
    rig.stack
        .remote
        .report(RemoteReport::manual("manual", true));
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && armed_ok(&payload_json(s))
    })
    .await;
    // The mirror's live sample races subscriber interest; re-toggle until
    // one is caught.
    'mirror: loop {
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", false));
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", true));
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(sample)) = tokio::time::timeout_at(step, mirror.recv_async()).await {
            if armed_ok(&payload_json(&sample)) {
                break 'mirror;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mirror never carried the armed source"
        );
    }
}

/// The exact QA failure shape: a source arms while the session knows no
/// TVs yet (empty armed set), then a TV hydrates over the fleet mirror —
/// the arming edge that fires on TV *arrival* (not on a source or profile
/// change) must carry both the user-socket fact and the system-socket
/// mirror to the full armed set.
#[tokio::test(flavor = "multi_thread")]
async fn armed_converges_when_tv_hydrates_after_arming() {
    let rig = rig().await;

    // The user-socket fact (cached) and the system-socket mirror (a cache-1
    // advanced pub), subscribed before any TV exists. This plain subscriber
    // sees only the mirror's live samples.
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;
    let mirror = rig
        .client
        .declare_subscriber(keys::remote_session(&rig.site))
        .await
        .unwrap();

    // Arm with no TV known: the source is live but the armed set is empty.
    assert!(
        rig.stack
            .remote
            .report(RemoteReport::manual("manual", true)),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"]
                .as_array()
                .is_some_and(|src| src.iter().any(|s| s["kind"] == "manual"))
                && v["armed"] == json!([])
        }
    })
    .await;

    // Now a TV arrives — its meta hydrates into the session over the fleet
    // mirror, and the arming edge on TV arrival must move the armed set.
    let tv = add_tv(&rig, "Den", "mock://late-tv").await;
    let armed_ok = |v: &Value| {
        v["sources"]
            .as_array()
            .is_some_and(|s| s.iter().any(|src| src["kind"] == "manual"))
            && v["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    };
    // The cached fact hydrates from history — assert it converges.
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && armed_ok(&payload_json(s))
    })
    .await;
    // The mirror's live sample races subscriber interest; the TV-arrival
    // recompute re-mirrors the converged set, so a drain catches it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, mirror.recv_async()).await {
            Ok(Ok(sample)) if armed_ok(&payload_json(&sample)) => break,
            Ok(Ok(_)) => {}
            _ => panic!("mirror never converged to the armed TV"),
        }
    }
}

/// The outage variant: the session starts before its system daemon, so
/// its first mirror sample (issued while arming with the link down) never
/// reaches a daemon, but the cache-1 mirror publisher retains it. When the
/// daemon comes up it hydrates that standing sample from the session's
/// cache, and the TV-arrival recompute re-mirrors the converged set — so
/// the system-socket mirror still ends on the full armed set.
#[tokio::test(flavor = "multi_thread")]
async fn armed_survives_a_system_link_outage_at_arm_time() {
    let dir = tempfile::tempdir().unwrap();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");

    // The session boots first, dialing a socket no daemon is listening on
    // yet — the resilient link retries in the background.
    let stack = tisvlay_session::serve(SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: None,
    })
    .await
    .unwrap();

    // Arm while the link is down: the mirror sample reaches no daemon but
    // the cache-1 publisher retains it, and no TV is known, so the armed
    // set is empty at this point.
    assert!(
        stack.remote.report(RemoteReport::manual("manual", true)),
        "core gone"
    );
    // Let the mirror put(s) settle into the publisher cache before the
    // daemon exists.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bring the daemon up on the socket the session is dialing.
    let (connector, discovery, _mock) = new_test_parts();
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

    // A system-socket client observes the mirror and adds a TV.
    let client = zenoh::open(bootstrap::client_config(&system_socket).unwrap())
        .await
        .unwrap();
    let mirror = client
        .declare_subscriber(keys::remote_session("t"))
        .await
        .unwrap();
    let tv = {
        let body = serde_json::to_value(rpc::AddTv {
            id: None,
            name: "Den".into(),
            vendor: "mock".into(),
            managed: true,
            config: json!({ "address": "mock://outage" }),
        })
        .unwrap();
        let ok = call(&client, &keys::manager_rpc("t", "add-tv"), body)
            .await
            .expect("add-tv ok");
        Uuid::parse_str(ok["id"].as_str().unwrap()).unwrap()
    };

    // Once the link recovers, the mirror must converge to the armed TV —
    // the regression this guards is a stale empty-armed snapshot never being
    // corrected by the TV-arrival edge.
    let armed_tv = |v: &Value| {
        v["armed"]
            .as_array()
            .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, mirror.recv_async()).await {
            Ok(Ok(sample)) if armed_tv(&payload_json(&sample)) => break,
            Ok(Ok(_)) => {}
            _ => panic!("mirror never converged to the armed TV after link recovery"),
        }
    }
}

/// The verb rpc round-trips through the Caller on the user socket:
/// `set { source, active }` arms, `status` reflects it.
#[tokio::test(flavor = "multi_thread")]
async fn remote_verb_rpc_round_trips() {
    use tisvlay_wire::caller::Caller;

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-rpc").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    let caller = Caller::new(rig.user.clone());
    // Arm via the rpc — the reply echoes the fact.
    let reply = caller
        .call(
            &keys::user_remote_rpc(&rig.site, "set"),
            json!({ "source": "cli", "active": true }),
        )
        .await
        .expect("set arms");
    assert!(
        reply["sources"].as_array().is_some_and(|s| s
            .iter()
            .any(|src| src["kind"] == "manual" && src["detail"] == "cli")),
        "set reply carries the manual source, got {reply}"
    );

    // status reflects the armed source.
    let status = caller
        .fetch(&keys::user_remote_rpc(&rig.site, "status"), Value::Null)
        .await
        .expect("status ok");
    assert!(
        status["sources"]
            .as_array()
            .is_some_and(|s| s.iter().any(|src| src["detail"] == "cli")),
        "status carries the armed source, got {status}"
    );

    // Disarm; status goes empty.
    caller
        .call(
            &keys::user_remote_rpc(&rig.site, "set"),
            json!({ "source": "cli", "active": false }),
        )
        .await
        .expect("set disarms");
    let status = caller
        .fetch(&keys::user_remote_rpc(&rig.site, "status"), Value::Null)
        .await
        .expect("status ok");
    assert_eq!(status["sources"], json!([]), "disarmed status is empty");
}

/// A `Screencast`-kind report (the shape the PipeWire detector produces)
/// travels the detector seam into the fact, and its unattributed signal
/// arms a TV only when the resolved profile opts capture in — the whole
/// detector→fact→policy→armed contract without a live PipeWire.
#[tokio::test(flavor = "multi_thread")]
async fn screencast_source_arms_only_under_capture_policy() {
    use tisvlay_wire::payload::RemoteSourceKind;

    // The detector emits `Screencast` reports keyed on a summary detail;
    // reconstruct that shape here (the module's own pure fns are unit
    // tested — this checks the wire contract through the running stack).
    let screencast = |active: bool| RemoteReport {
        kind: RemoteSourceKind::Screencast,
        detail: "1 node (running)".into(),
        active,
        provisional: false,
    };

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://screencast").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;

    // Default profile does not arm on capture: the screencast source is
    // published in `sources`, but the TV is NOT armed.
    create_and_assign(&rig, tv, json!({ "name": "No capture arm" })).await;
    assert!(rig.stack.remote.report(screencast(true)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"]
                .as_array()
                .is_some_and(|src| src.iter().any(|s| s["kind"] == "screencast"))
                && v["armed"] == json!([])
        }
    })
    .await;

    // Opt capture in: the same live source now arms the TV.
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Capture arms", "remote_screencast_arms": true }),
    )
    .await;
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;

    // Node gone: the source clears and the TV disarms.
    assert!(rig.stack.remote.report(screencast(false)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"] == json!([]) && v["armed"] == json!([])
        }
    })
    .await;
}

/// The connect/disconnect edge keys on the ARMED set, not raw source
/// presence: an unarmed screencast (capture arming off — an OBS record or
/// a browser tab-share) publishes its source but must not fire the connect
/// action and blank every TV. Opting capture in makes the same signal arm
/// and drive the edge.
#[tokio::test(flavor = "multi_thread")]
async fn unarmed_screencast_publishes_but_fires_no_connect_action() {
    use tisvlay_wire::payload::RemoteSourceKind;

    let screencast = |active: bool| RemoteReport {
        kind: RemoteSourceKind::Screencast,
        detail: "1 node (running)".into(),
        active,
        provisional: false,
    };

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://screencast-edge").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;

    // A profile whose connect action would blank the panel and whose
    // disconnect action wakes it — capture arming stays off (default).
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Capture edges",
            "on_remote_connect": "screen_off",
            "on_remote_disconnect": "screen_on",
        }),
    )
    .await;

    // The unattributed screencast: the fact carries the source but the
    // armed set stays empty, and — the regression — NO connect action
    // reaches the TV, so the panel is never blanked for a mere capture.
    assert!(rig.stack.remote.report(screencast(true)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"]
                .as_array()
                .is_some_and(|src| src.iter().any(|s| s["kind"] == "screencast"))
                && v["armed"] == json!([])
        }
    })
    .await;
    assert_no_sample(&sub, Duration::from_millis(500), |s| {
        keys::parse_tv_path(&rig.site, s.key_expr().as_str())
            .is_some_and(|(t, p)| t == tv && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    // Opt capture in: the same live signal now arms the TV, so a re-toggled
    // source drives the arm edge and the connect action (screen_off) fires.
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Capture arms edges",
            "on_remote_connect": "screen_off",
            "on_remote_disconnect": "screen_on",
            "remote_screencast_arms": true,
        }),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(rig.stack.remote.report(screencast(false)), "core gone");
        assert!(rig.stack.remote.report(screencast(true)), "core gone");
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(sample)) = tokio::time::timeout_at(step, sub.recv_async()).await {
            if keys::parse_tv_path(&rig.site, sample.key_expr().as_str())
                .is_some_and(|(t, p)| t == tv && p == "state/power")
                && payload_json(&sample)["state"] == "screenoff"
            {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "capture-armed screencast never fired the connect action"
        );
    }
}

/// A `Portal`-kind report (the shape the portal monitor produces for an
/// allow-listed remote app) is attributed, so it arms the TV with no
/// `remote_screencast_arms` opt-in — unlike the unattributed screencast
/// signal above. The exe rides in the source `detail`.
#[tokio::test(flavor = "multi_thread")]
async fn portal_source_arms_without_capture_policy() {
    use tisvlay_wire::payload::RemoteSourceKind;

    let portal = |active: bool| RemoteReport {
        kind: RemoteSourceKind::Portal,
        detail: "krdpserver".into(),
        active,
        provisional: false,
    };

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://portal").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;

    // A default profile (capture NOT opted in): the attributed portal
    // source still arms the TV, and the exe is the source detail.
    create_and_assign(&rig, tv, json!({ "name": "Default" })).await;
    assert!(rig.stack.remote.report(portal(true)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"].as_array().is_some_and(|src| {
                src.iter()
                    .any(|s| s["kind"] == "portal" && s["detail"] == "krdpserver")
            }) && v["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
        }
    })
    .await;

    // Session closed: the source clears and the TV disarms.
    assert!(rig.stack.remote.report(portal(false)), "core gone");
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            v["sources"] == json!([]) && v["armed"] == json!([])
        }
    })
    .await;
}

/// A *provisional* `Portal` report (the monitor's bootstrap guess, owner
/// resolved but the session type unconfirmed) is held out of the fact
/// until a `Screencast` source corroborates a live capture — the
/// tree-walk join, exercised through the running stack.
#[tokio::test(flavor = "multi_thread")]
async fn provisional_portal_joins_on_corroborating_screencast() {
    use tisvlay_wire::payload::RemoteSourceKind;

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://join").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let fact = history_sub(&rig.user, keys::user_remote_session(&rig.site)).await;
    create_and_assign(&rig, tv, json!({ "name": "Default" })).await;

    // The bootstrap guess alone does not appear in the fact and does not
    // arm — an uncorroborated owner might be holding only an InputCapture
    // session (the lan-mouse false positive).
    assert!(
        rig.stack.remote.report(RemoteReport {
            kind: RemoteSourceKind::Portal,
            detail: "rustdesk".into(),
            active: true,
            provisional: true,
        }),
        "core gone"
    );
    // Drive a screencast source in; only then do BOTH the portal and the
    // screencast appear, and the TV arms (via the now-corroborated,
    // attributed portal source — capture policy is off).
    assert!(
        rig.stack.remote.report(RemoteReport {
            kind: RemoteSourceKind::Screencast,
            detail: "1 node (running)".into(),
            active: true,
            provisional: false,
        }),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && {
            let v = payload_json(s);
            let kinds: Vec<&str> = v["sources"]
                .as_array()
                .map(|src| src.iter().filter_map(|s| s["kind"].as_str()).collect())
                .unwrap_or_default();
            kinds.contains(&"portal")
                && kinds.contains(&"screencast")
                && v["armed"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
        }
    })
    .await;
}

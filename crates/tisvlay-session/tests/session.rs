//! P4 session-service wire tests: gateway isolation (user/** never
//! transits the system peer), lock/unlock/idle dispatch through user
//! profiles, the logind lock watcher, persistence, and the idle-timeout
//! watch.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "integration tests fail loudly by design; clippy's allow-*-in-tests keys don't reach helper fns under tests/"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tisvlay_mock::new_test_parts;
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{Unary, manager, user_profile, user_stream};
use tisvlay_protocol::error::ErrorCode;
use tisvlay_protocol::{bootstrap, keys, rpc};
use tisvlay_session::sunshine::{Observation, StaticProbe};
use tisvlay_session::{SessionEvent, SessionOptions, SessionStack, StreamReport};
use uuid::Uuid;
use zenoh::Session;
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
    /// Typed rpcs on the system socket.
    caller: Caller,
    /// Typed rpcs on the user socket.
    user_caller: Caller,
    site: String,
    mock: tisvlay_mock::MockHandle,
    /// The core's Sunshine observation — tests script it; `Absent` by default.
    sunshine: Arc<StaticProbe>,
}

async fn rig() -> Rig {
    rig_with_store(None).await
}

async fn rig_with_store(store: Option<&Path>) -> Rig {
    let dir = tisvlay_test::socket_tempdir();
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
    let sunshine = StaticProbe::absent();
    let stack = tisvlay_session::serve(SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: store.map(Path::to_path_buf),
        sunshine: sunshine.clone(),
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
        caller: Caller::new(client.clone(), "t"),
        user_caller: Caller::new(user.clone(), "t"),
        client,
        user,
        site: "t".into(),
        mock: handle,
        sunshine,
    }
}

async fn history_sub(session: &Session, key: String) -> Sub {
    session
        .declare_subscriber(key)
        .history(HistoryConfig::default().detect_late_publishers())
        .await
        .unwrap()
}

/// One typed rpc round-trip through the matching-gated caller.
async fn call<R: Unary>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<R::Response, RpcError> {
    caller.call::<R>(target, request).await
}

/// A request from a JSON literal, decoded into the rpc's request type.
fn body<T: serde::de::DeserializeOwned>(json: Value) -> T {
    serde_json::from_value(json).expect("the literal fits the request type")
}

/// The service's refusal behind a failed call.
fn refusal(error: &RpcError) -> &tisvlay_protocol::error::WireError {
    match error {
        RpcError::Service(refusal) => refusal,
        other => panic!("expected a service refusal, got {other:?}"),
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
    let request = rpc::AddTv {
        id: None,
        name: name.into(),
        vendor: "mock".into(),
        managed: true,
        config: serde_json::from_value(json!({ "address": address })).unwrap(),
    };
    let ok = call::<manager::AddTv>(&rig.caller, &(), &request)
        .await
        .expect("add-tv ok");
    ok.id
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

/// Send `event` until its observability tap lands on `tap` (subscribed to
/// `event/session/*`). The tap is a plain put racing the subscriber's
/// interest propagation, so this re-drives — callers pick an event whose
/// dispatch is idempotent where they are.
async fn drive_tap(rig: &Rig, tap: &Sub, event: SessionEvent, name: &str) {
    let key = keys::session_event(&rig.site, name);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(rig.stack.client.send(event), "session core gone");
        let step = tokio::time::Instant::now() + Duration::from_millis(500);
        while let Ok(Ok(sample)) = tokio::time::timeout_at(step, tap.recv_async()).await {
            if sample.key_expr().as_str() == key {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{name} tap never observed"
        );
    }
}

/// The gateway invariant: user keys exist on the user socket only.
/// The user-profile rpcs no other test reaches, through their typed endpoints.
#[tokio::test(flavor = "multi_thread")]
async fn user_profile_update_duplicate_and_info() {
    let rig = rig().await;
    let info = call::<user_profile::Info>(&rig.user_caller, &(), &())
        .await
        .expect("info ok");
    assert_eq!(info.version, env!("CARGO_PKG_VERSION"));

    let request = body(json!({ "body": { "name": "Original" } }));
    let id = call::<user_profile::Create>(&rig.user_caller, &(), &request)
        .await
        .expect("create ok")
        .id;
    let request = body(json!({ "id": id, "body": { "name": "Renamed" } }));
    call::<user_profile::Update>(&rig.user_caller, &(), &request)
        .await
        .expect("update ok");
    let request = rpc::ProfileDuplicate {
        id,
        name: "Copy".into(),
    };
    let copy = call::<user_profile::Duplicate>(&rig.user_caller, &(), &request)
        .await
        .expect("duplicate ok")
        .id;
    assert_ne!(copy, id, "a duplicate mints its own id");

    let sub = history_sub(&rig.user, keys::user_profile(&rig.site, &copy)).await;
    recv_until(&sub, |s| payload_json(s)["name"] == "Copy").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn gateway_isolates_the_user_subtree() {
    let rig = rig().await;
    // A second session service for another "user", same site.
    let dir2 = tisvlay_test::socket_tempdir();
    let user2_socket = dir2.path().join("user2.sock");
    let _stack2 = tisvlay_session::serve(SessionOptions {
        system_socket: rig.system_socket.clone(),
        user_socket: user2_socket.clone(),
        site: rig.site.clone(),
        store: None,
        sunshine: StaticProbe::absent(),
    })
    .await
    .unwrap();

    let ok = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": { "name": "Mine", "on_session_lock": "none" } })),
    )
    .await
    .expect("create ok");
    let profile = ok.id;

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
    let tap = history_sub(&rig.client, keys::session_event(&rig.site, "*")).await;

    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    // Activity on the lock screen: the hard-default on_lock_wake is
    // screen_on, so the panel comes back before any unlock.
    drive_until(&rig, &sub, SessionEvent::LockWake, tv, "active").await;
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;
    drive_tap(&rig, &tap, SessionEvent::Unlock, "unlock").await;
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

    let defaults = call::<user_profile::Defaults>(&rig.user_caller, &(), &())
        .await
        .expect("defaults ok");
    assert_eq!(
        serde_json::to_value(defaults).unwrap(),
        json!({
            "name": "default",
            // Floor splits by direction: the off-going blanks (lock,
            // idle-timeout) project ungated (`while_streaming = true`); the
            // wake-ish events (unlock, lock-wake, idle-resume) stay gated.
            "on_session_lock": { "action": "screen_off", "restore_input": false, "while_streaming": true },
            "on_session_unlock": { "action": "screen_on", "restore_input": true, "while_streaming": false },
            "on_lock_wake": "screen_on",
            "locked_idle_timeout_seconds": 0,
            "unlocked_idle_timeout_seconds": 0,
            "on_idle_timeout": { "action": "screen_off", "restore_input": false, "while_streaming": true },
            "on_idle_resume": "screen_on",
            // Both stream edges floor to screen_off (blank on arm, stay dark
            // on disarm); capture arming ships off. The connect/disconnect
            // actions ignore the gate, so they project the plain default.
            // on_local_return wakes restoring on the first local input after
            // a disconnect (its table shape mirrors on_session_unlock).
            "on_stream_start": "screen_off",
            "on_stream_end": "screen_off",
            "on_local_return": { "action": "screen_on", "restore_input": true, "while_streaming": false },
            "screencast_arms": false,
            // The going-off events act on the PC only when asked.
            "on_tv_power_off": "none",
            "on_remote_power": "none",
        })
    );

    let ok = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": { "name": "No blank", "on_session_lock": "none" } })),
    )
    .await
    .expect("create ok");
    let profile = ok.id;
    call::<user_profile::Assign>(
        &rig.user_caller,
        &(),
        &body(json!({ "tv": quiet, "profile": profile })),
    )
    .await
    .expect("assign ok");

    // The assignment is visible on the user socket.
    let assignment = history_sub(&rig.user, keys::user_tv_profile(&rig.site, &quiet)).await;
    recv_until(&assignment, |s| {
        payload_json(s)["profile"] == profile.to_string()
    })
    .await;

    let effective =
        call::<user_profile::Effective>(&rig.user_caller, &(), &body(json!({ "id": quiet })))
            .await
            .expect("effective ok");
    let effective = serde_json::to_value(effective).unwrap();
    assert_eq!(effective["on_session_lock"], "none");
    assert_eq!(
        effective["on_session_unlock"],
        json!({ "action": "screen_on", "restore_input": true, "while_streaming": false })
    );

    drive_until(&rig, &sub, SessionEvent::Lock, stock, "screenoff").await;
    assert_no_sample(&sub, Duration::from_millis(400), |s| {
        keys::parse_tv_path(&rig.site, s.key_expr().as_str())
            .is_some_and(|(tv, p)| tv == quiet && p == "state/power")
            && payload_json(s)["state"] == "screenoff"
    })
    .await;

    let err = call::<user_profile::Delete>(&rig.user_caller, &(), &body(json!({ "id": profile })))
        .await
        .expect_err("delete while assigned must fail");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileInUse);
    call::<user_profile::Assign>(&rig.user_caller, &(), &body(json!({ "tv": quiet })))
        .await
        .expect("unassign ok");
    call::<user_profile::Delete>(&rig.user_caller, &(), &body(json!({ "id": profile })))
        .await
        .expect("delete ok");
}

/// The consumed-D-Bus adapter: logind `LockedHint` transitions drive
/// lock/unlock end to end.
#[tokio::test(flavor = "multi_thread")]
async fn lock_watcher_drives_lock_unlock() {
    let bus = tisvlay_test::PrivateBus::isolated();
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
    let is_locked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked = is_locked.clone();
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
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
    let ok = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": {
            "name": "Drift",
            "locked_idle_timeout_seconds": 1,
        } })),
    )
    .await
    .expect("create ok");
    let profile = ok.id;
    call::<user_profile::Assign>(
        &rig.user_caller,
        &(),
        &body(json!({ "tv": tv, "profile": profile })),
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
    let bus = tisvlay_test::PrivateBus::isolated();
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
    let is_locked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let locked = is_locked.clone();
    let active = Arc::new(std::sync::atomic::AtomicBool::new(true));
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
    let bus = tisvlay_test::PrivateBus::isolated();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_logind::FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");
    // Locked before the service starts — a property to seed, not an edge.
    fake.set_locked(true).await.unwrap();

    let seed_conn = bus.connect().await.expect("seed conn");
    let is_locked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let is_active = Arc::new(std::sync::atomic::AtomicBool::new(true));

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
    let ok = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": {
            "name": "Idle",
            "locked_idle_timeout_seconds": 45,
            "unlocked_idle_timeout_seconds": 120,
        } })),
    )
    .await
    .expect("create ok");
    let profile = ok.id;
    call::<user_profile::Assign>(
        &rig.user_caller,
        &(),
        &body(json!({ "tv": tv, "profile": profile })),
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
        sunshine: StaticProbe::absent(),
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
async fn create_and_assign(rig: &Rig, tv: Uuid, profile_body: Value) -> Uuid {
    let ok = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": profile_body })),
    )
    .await
    .expect("create ok");
    let profile = ok.id;
    call::<user_profile::Assign>(
        &rig.user_caller,
        &(),
        &body(json!({ "tv": tv, "profile": profile })),
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
            rig.stack.stream.report(StreamReport::manual(source, false)),
            "session core gone"
        );
        assert!(
            rig.stack.stream.report(StreamReport::manual(source, true)),
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

/// While armed, an action whose `while_streaming` gate is `false` is
/// suppressed — the physical TV stays dark for the empty room.
#[tokio::test(flavor = "multi_thread")]
async fn armed_stream_suppresses_gated_events() {
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
    // explicit `while_streaming = true` here.
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Remote quiet",
            "on_session_unlock": { "action": "screen_on", "while_streaming": false },
            "on_idle_resume": { "action": "screen_on", "while_streaming": false },
        }),
    )
    .await;

    // Unarmed baseline: unlock wakes the panel (the default action fires).
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // Arm and confirm via the fact (this profile fires no connect
    // action, so the armed set on the fact is the observable).
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    assert!(
        rig.stack
            .stream
            .report(StreamReport::manual("manual", true)),
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

/// A TV armed by a live `Sunshine` source, its unlock gated off while
/// armed (the stream edges stay inert so arming never moves the panel).
/// Returns the rig, the TV, its state subscriber, and the remote fact.
async fn sunshine_armed() -> (Rig, Uuid, Sub, Sub) {
    use tisvlay_protocol::payload::StreamSourceKind;

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://eager-sunshine").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Sunshine eager",
            "on_session_unlock": { "action": "screen_on", "while_streaming": false },
            "on_stream_start": "none",
            "on_stream_end": "none",
        }),
    )
    .await;

    // Baseline: hydrate the fleet mirror and confirm dispatch reaches the TV.
    drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
    drive_until(&rig, &sub, SessionEvent::Unlock, tv, "active").await;

    // A live Sunshine source arms the TV (attributed kinds always arm).
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    assert!(
        rig.stack.stream.report(StreamReport {
            kind: StreamSourceKind::Sunshine,
            detail: "serverinfo".into(),
            active: true,
            provisional: false,
        }),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;
    (rig, tv, sub, fact)
}

/// A live Sunshine source is refreshed on the wake edge: because the gate
/// and the `/serverinfo` detector share this process, an unlock probes the
/// endpoint synchronously and clears the source when the stream has ended,
/// so the wake fires instead of being suppressed for up to a poll interval.
/// The scripted probe observes the process gone, which disarms exactly as a
/// `FREE` reading would.
#[tokio::test(flavor = "multi_thread")]
async fn gated_wake_eagerly_refreshes_a_stale_sunshine_source() {
    let (rig, tv, sub, fact) = sunshine_armed().await;
    rig.sunshine.set(Observation::Absent);

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

/// The eager pull only clears a source the endpoint says is gone: a busy
/// stream, or a reading it can't classify, keeps the source and the wake
/// stays suppressed.
#[tokio::test(flavor = "multi_thread")]
async fn gated_wake_stays_suppressed_while_sunshine_is_busy_or_unknown() {
    for observation in [Observation::Busy, Observation::Unknown] {
        let (rig, tv, sub, fact) = sunshine_armed().await;
        rig.sunshine.set(observation);
        let tap = history_sub(&rig.client, keys::session_event(&rig.site, "*")).await;

        drive_until(&rig, &sub, SessionEvent::Lock, tv, "screenoff").await;
        // The unlock's tap publishes after the eager pull and just before
        // the gate's decision, so once it lands the suppressed wake has had
        // its chance — the window below only spans rpc latency. Every
        // re-driven unlock meets the same gate.
        drive_tap(&rig, &tap, SessionEvent::Unlock, "unlock").await;
        assert_no_sample(&sub, Duration::from_millis(500), |s| {
            keys::parse_tv_path(&rig.site, s.key_expr().as_str())
                .is_some_and(|(id, path)| id == tv && path == "state/power")
                && payload_json(s)["state"] == "active"
        })
        .await;
        assert_no_sample(&fact, Duration::from_millis(200), |s| {
            payload_json(s)["sources"] == json!([])
        })
        .await;
    }
}

/// The connect/disconnect actions fire on arm and disarm edges.
#[tokio::test(flavor = "multi_thread")]
async fn stream_start_end_actions_fire() {
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
            "on_stream_start": "screen_off",
            "on_stream_end": "screen_on",
        }),
    )
    .await;

    // The arm edge fires on_stream_start (screen_off).
    arm_until(&rig, &sub, "manual", tv, "screenoff").await;
    // The disarm edge fires on_stream_end (screen_on).
    assert!(
        rig.stack
            .stream
            .report(StreamReport::manual("manual", false)),
        "core gone"
    );
    wait_tv_state(&sub, &rig.site, tv, "state/power", |v| {
        v["state"] == "active"
    })
    .await;
}

/// After a stream end leaves the panel dark, the first local input
/// wakes it — the case where nothing locked or idled, so no unlock /
/// lock-wake / idle-resume edge would otherwise fire. The core arms the
/// return watch on disconnect; `LocalReturn` (which the idle adapter
/// synthesizes from a lock-wake resume while unlocked + armed) wakes it.
#[tokio::test(flavor = "multi_thread")]
async fn local_return_wakes_after_stream_end() {
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
            "on_stream_start": "screen_off",
            "on_stream_end": "screen_off",
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
            .stream
            .report(StreamReport::manual("manual", false)),
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
async fn stream_fact_and_mirror_carry_sources_and_armed() {
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
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    let mirror = rig
        .client
        .declare_subscriber(keys::stream_state(&rig.site))
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
        .stream
        .report(StreamReport::manual("manual", true));
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put && armed_ok(&payload_json(s))
    })
    .await;
    // The mirror's live sample races subscriber interest; re-toggle until
    // one is caught.
    'mirror: loop {
        rig.stack
            .stream
            .report(StreamReport::manual("manual", false));
        rig.stack
            .stream
            .report(StreamReport::manual("manual", true));
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
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    let mirror = rig
        .client
        .declare_subscriber(keys::stream_state(&rig.site))
        .await
        .unwrap();

    // Arm with no TV known: the source is live but the armed set is empty.
    assert!(
        rig.stack
            .stream
            .report(StreamReport::manual("manual", true)),
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
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");

    // The session boots first, dialing a socket no daemon is listening on
    // yet — the resilient link retries in the background.
    let stack = tisvlay_session::serve(SessionOptions {
        system_socket: system_socket.clone(),
        user_socket: user_socket.clone(),
        site: "t".into(),
        store: None,
        sunshine: StaticProbe::absent(),
    })
    .await
    .unwrap();

    // Arm while the link is down: the mirror sample reaches no daemon but
    // the cache-1 publisher retains it, and no TV is known, so the armed
    // set is empty at this point.
    assert!(
        stack.stream.report(StreamReport::manual("manual", true)),
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
    let caller = Caller::new(client.clone(), "t");
    let mirror = client
        .declare_subscriber(keys::stream_state("t"))
        .await
        .unwrap();
    let tv = {
        let request = rpc::AddTv {
            id: None,
            name: "Den".into(),
            vendor: "mock".into(),
            managed: true,
            config: serde_json::from_value(json!({ "address": "mock://outage" })).unwrap(),
        };
        let ok = call::<manager::AddTv>(&caller, &(), &request)
            .await
            .expect("add-tv ok");
        ok.id
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
async fn stream_verb_rpc_round_trips() {
    use tisvlay_protocol::caller::Caller;

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-rpc").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    let caller = Caller::new(rig.user.clone(), rig.site.clone());
    // Arm via the rpc — the reply echoes the fact.
    let set = |active| rpc::StreamSet {
        source: Some("cli".into()),
        active,
    };
    let reply = caller
        .call::<user_stream::Set>(&(), &set(true))
        .await
        .map(|state| serde_json::to_value(state).unwrap())
        .expect("set arms");
    assert!(
        reply["sources"].as_array().is_some_and(|s| s
            .iter()
            .any(|src| src["kind"] == "manual" && src["detail"] == "cli")),
        "set reply carries the manual source, got {reply}"
    );

    // status reflects the armed source.
    let status = caller
        .call::<user_stream::Status>(&(), &())
        .await
        .map(|state| serde_json::to_value(state).unwrap())
        .expect("status ok");
    assert!(
        status["sources"]
            .as_array()
            .is_some_and(|s| s.iter().any(|src| src["detail"] == "cli")),
        "status carries the armed source, got {status}"
    );

    // Disarm; status goes empty.
    caller
        .call::<user_stream::Set>(&(), &set(false))
        .await
        .expect("set disarms");
    let status = caller
        .call::<user_stream::Status>(&(), &())
        .await
        .map(|state| serde_json::to_value(state).unwrap())
        .expect("status ok");
    assert_eq!(status["sources"], json!([]), "disarmed status is empty");
}

/// A `Screencast`-kind report (the shape the PipeWire detector produces)
/// travels the detector seam into the fact, and its unattributed signal
/// arms a TV only when the resolved profile opts capture in — the whole
/// detector→fact→policy→armed contract without a live PipeWire.
#[tokio::test(flavor = "multi_thread")]
async fn screencast_source_arms_only_under_capture_policy() {
    use tisvlay_protocol::payload::StreamSourceKind;

    // The detector emits `Screencast` reports keyed on a summary detail;
    // reconstruct that shape here (the module's own pure fns are unit
    // tested — this checks the wire contract through the running stack).
    let screencast = |active: bool| StreamReport {
        kind: StreamSourceKind::Screencast,
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
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;

    // Default profile does not arm on capture: the screencast source is
    // published in `sources`, but the TV is NOT armed.
    create_and_assign(&rig, tv, json!({ "name": "No capture arm" })).await;
    assert!(rig.stack.stream.report(screencast(true)), "core gone");
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
        json!({ "name": "Capture arms", "screencast_arms": true }),
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
    assert!(rig.stack.stream.report(screencast(false)), "core gone");
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
    use tisvlay_protocol::payload::StreamSourceKind;

    let screencast = |active: bool| StreamReport {
        kind: StreamSourceKind::Screencast,
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
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;

    // A profile whose connect action would blank the panel and whose
    // disconnect action wakes it — capture arming stays off (default).
    create_and_assign(
        &rig,
        tv,
        json!({
            "name": "Capture edges",
            "on_stream_start": "screen_off",
            "on_stream_end": "screen_on",
        }),
    )
    .await;

    // The unattributed screencast: the fact carries the source but the
    // armed set stays empty, and — the regression — NO connect action
    // reaches the TV, so the panel is never blanked for a mere capture.
    assert!(rig.stack.stream.report(screencast(true)), "core gone");
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
            "on_stream_start": "screen_off",
            "on_stream_end": "screen_on",
            "screencast_arms": true,
        }),
    )
    .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        assert!(rig.stack.stream.report(screencast(false)), "core gone");
        assert!(rig.stack.stream.report(screencast(true)), "core gone");
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
/// allow-listed streaming app) is attributed, so it arms the TV with no
/// `screencast_arms` opt-in — unlike the unattributed screencast
/// signal above. The exe rides in the source `detail`.
#[tokio::test(flavor = "multi_thread")]
async fn portal_source_arms_without_capture_policy() {
    use tisvlay_protocol::payload::StreamSourceKind;

    let portal = |active: bool| StreamReport {
        kind: StreamSourceKind::Portal,
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
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;

    // A default profile (capture NOT opted in): the attributed portal
    // source still arms the TV, and the exe is the source detail.
    create_and_assign(&rig, tv, json!({ "name": "Default" })).await;
    assert!(rig.stack.stream.report(portal(true)), "core gone");
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
    assert!(rig.stack.stream.report(portal(false)), "core gone");
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
    use tisvlay_protocol::payload::StreamSourceKind;

    let rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://join").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    create_and_assign(&rig, tv, json!({ "name": "Default" })).await;

    // The bootstrap guess alone does not appear in the fact and does not
    // arm — an uncorroborated owner might be holding only an InputCapture
    // session (the lan-mouse false positive).
    assert!(
        rig.stack.stream.report(StreamReport {
            kind: StreamSourceKind::Portal,
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
        rig.stack.stream.report(StreamReport {
            kind: StreamSourceKind::Screencast,
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

/// Wait until the session's store file mentions `needle`.
async fn await_store_contains(store: &Path, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(store).is_ok_and(|t| t.contains(needle)) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the store never mentioned {needle}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A hand edit of the session's store file applies on reload; a second
/// reload with nothing new changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn reload_applies_a_hand_edited_user_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("session.toml");
    let rig = rig_with_store(Some(&store)).await;
    let profile = call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": { "name": "Before" } })),
    )
    .await
    .expect("create ok")
    .id;
    await_store_contains(&store, "Before").await;
    let edited = std::fs::read_to_string(&store)
        .unwrap()
        .replace("\"Before\"", "\"After\"");
    std::fs::write(&store, edited).unwrap();

    let sub = history_sub(&rig.user, keys::user_profile(&rig.site, &profile)).await;
    let report = call::<user_profile::Reload>(&rig.user_caller, &(), &())
        .await
        .expect("reload ok");
    assert!(report.changed);
    recv_until(&sub, |s| payload_json(s)["name"] == "After").await;
    let again = call::<user_profile::Reload>(&rig.user_caller, &(), &())
        .await
        .expect("reload ok");
    assert!(!again.changed);
}

#[tokio::test(flavor = "multi_thread")]
async fn reload_refuses_bad_user_stores() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("session.toml");
    let rig = rig_with_store(Some(&store)).await;
    call::<user_profile::Create>(
        &rig.user_caller,
        &(),
        &body(json!({ "body": { "name": "Kept" } })),
    )
    .await
    .expect("create ok");
    await_store_contains(&store, "Kept").await;

    // Dropping the default profile is refused, whole.
    let text = std::fs::read_to_string(&store).unwrap();
    let parsed: tisvlay_session::UserStoreSnapshot = toml::from_str(&text).unwrap();
    let mut without_default = parsed.clone();
    without_default
        .profile
        .retain(|record| record.id != tisvlay_core::DEFAULT_USER_PROFILE_UUID);
    std::fs::write(&store, toml::to_string(&without_default).unwrap()).unwrap();
    let err = call::<user_profile::Reload>(&rig.user_caller, &(), &())
        .await
        .expect_err("the default profile can't go");
    assert_eq!(refusal(&err).code, ErrorCode::ProfileReserved);

    // A missing file is refused, not read as empty.
    std::fs::remove_file(&store).unwrap();
    let err = call::<user_profile::Reload>(&rig.user_caller, &(), &())
        .await
        .expect_err("a missing store is refused");
    assert_eq!(refusal(&err).code, ErrorCode::StoreUnreadable);

    let ephemeral = rig_with_store(None).await;
    let err = call::<user_profile::Reload>(&ephemeral.user_caller, &(), &())
        .await
        .expect_err("no store, nothing to reload");
    assert_eq!(refusal(&err).code, ErrorCode::NoStore);
}

/// Power the mock TV on and off until the core asks to lock, absorbing
/// fleet-mirror lag (the core acts only on a TV whose meta it holds):
/// each on-to-off toggle is a fresh live edge.
async fn lock_requested_after_power_off(rig: &mut Rig, address: &str, tv: Uuid) {
    use tisvlay_core::PowerState;

    for _ in 0..20 {
        rig.mock.inject_power_state(address, PowerState::Active);
        rig.mock.inject_power_state(address, PowerState::Standby);
        if let Ok(asked) =
            tokio::time::timeout(Duration::from_millis(500), rig.stack.lock_requests.recv()).await
        {
            assert_eq!(asked, Some(tv), "the request names the TV");
            return;
        }
    }
    panic!("the TV powering off never asked for a lock");
}

/// The TV powering off by itself locks the session when the user profile
/// asks: the world publishes the live edge, and the core hands the locker
/// a request naming the TV.
#[tokio::test(flavor = "multi_thread")]
async fn tv_powering_off_by_itself_requests_a_lock() {
    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://tv-off-lock").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Lock on TV off", "on_tv_power_off": "lock" }),
    )
    .await;

    lock_requested_after_power_off(&mut rig, "mock://tv-off-lock", tv).await;
}

/// While a stream arms the TV, its power-off locks nothing: the remote
/// user isn't locked out by a TV going dark in the empty room.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_holds_the_tv_power_off_lock_back() {
    use tisvlay_core::PowerState;

    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://tv-off-stream").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Lock on TV off", "on_tv_power_off": "lock" }),
    )
    .await;
    // Unarmed, it locks (so the core holds the TV's meta from here on).
    lock_requested_after_power_off(&mut rig, "mock://tv-off-stream", tv).await;

    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    assert!(
        rig.stack
            .stream
            .report(StreamReport::manual("manual", true)),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;

    rig.mock
        .inject_power_state("mock://tv-off-stream", PowerState::Active);
    rig.mock
        .inject_power_state("mock://tv-off-stream", PowerState::Standby);
    assert!(
        tokio::time::timeout(Duration::from_millis(600), rig.stack.lock_requests.recv())
            .await
            .is_err(),
        "a stream must hold the lock back"
    );
}

/// The stock profile maps the TV's power-off to nothing.
#[tokio::test(flavor = "multi_thread")]
async fn the_stock_profile_locks_nothing_on_tv_power_off() {
    use tisvlay_core::PowerState;

    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://tv-off-stock").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;

    rig.mock
        .inject_power_state("mock://tv-off-stock", PowerState::Standby);
    assert!(
        tokio::time::timeout(Duration::from_millis(600), rig.stack.lock_requests.recv())
            .await
            .is_err(),
        "the stock profile asks for no lock"
    );
}

/// The locker asks logind to lock this user's seat session.
#[tokio::test(flavor = "multi_thread")]
async fn the_locker_locks_the_seat_session() {
    let bus = tisvlay_test::PrivateBus::isolated();
    let service_conn = bus.connect().await.expect("private bus service conn");
    let fake = tisvlay_test::fake_logind::FakeLogind::register(&service_conn)
        .await
        .expect("fake logind");
    let (requests, rx) = tokio::sync::mpsc::unbounded_channel();
    let locker_conn = bus.connect().await.expect("locker conn");
    tokio::spawn(tisvlay_session::run_locker(locker_conn, rx));

    requests.send(Uuid::new_v4()).expect("locker alive");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fake.lock_requests().await == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the seat session was never asked to lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fake.lock_requests().await, 1);
}

/// Press the mock TV's remote power key until the core asks to lock,
/// absorbing fleet-mirror lag (the core resolves the TV's user profile
/// from the meta it holds).
async fn lock_requested_after_remote_power(rig: &mut Rig, address: &str, tv: Uuid) {
    for _ in 0..20 {
        rig.mock.press_remote_power(address);
        if let Ok(asked) =
            tokio::time::timeout(Duration::from_millis(500), rig.stack.lock_requests.recv()).await
        {
            assert_eq!(asked, Some(tv), "the request names the TV");
            return;
        }
    }
    panic!("the remote's power key never asked for a lock");
}

/// The remote's power key locks the session when the user profile asks,
/// even with automation gated off: the press is explicit.
#[tokio::test(flavor = "multi_thread")]
async fn the_remote_power_key_requests_a_lock_without_automation() {
    use tisvlay_protocol::endpoint::{profile, tv as tv_rpc};

    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-lock").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    // The mock sits on HDMI_1; automating only on the primary, HDMI_2,
    // gates automation off.
    let system = call::<profile::Create>(
        &rig.caller,
        &(),
        &body(json!({ "body": { "name": "Gated", "automate_when": "on_primary" } })),
    )
    .await
    .expect("create ok")
    .id;
    call::<tv_rpc::SetSystemProfile>(&rig.caller, &tv, &body(json!({ "profile": system })))
        .await
        .expect("assign ok");
    call::<tv_rpc::SetPrimaryInput>(&rig.caller, &tv, &body(json!({ "input": "HDMI_2" })))
        .await
        .expect("primary ok");
    wait_tv_state(&sub, &rig.site, tv, "meta/config", |v| {
        v["automated"] == false
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Lock on the remote", "on_remote_power": "lock" }),
    )
    .await;

    lock_requested_after_remote_power(&mut rig, "mock://remote-lock", tv).await;
}

/// An unmanaged TV's remote power key locks nothing; managed again, it
/// locks.
#[tokio::test(flavor = "multi_thread")]
async fn an_unmanaged_tvs_remote_power_key_locks_nothing() {
    use tisvlay_protocol::endpoint::tv as tv_rpc;

    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-unmanaged").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Lock on the remote", "on_remote_power": "lock" }),
    )
    .await;
    call::<tv_rpc::SetManaged>(&rig.caller, &tv, &body(json!({ "managed": false })))
        .await
        .expect("unmanage ok");
    wait_tv_state(&sub, &rig.site, tv, "meta/config", |v| {
        v["managed"] == false
    })
    .await;
    // The session mirrors the meta behind the daemon: give it time to see
    // the change before pressing.
    tokio::time::sleep(Duration::from_millis(500)).await;

    for _ in 0..3 {
        rig.mock.press_remote_power("mock://remote-unmanaged");
    }
    let asked =
        tokio::time::timeout(Duration::from_millis(1500), rig.stack.lock_requests.recv()).await;
    assert!(asked.is_err(), "an unmanaged TV locks nothing: {asked:?}");

    call::<tv_rpc::SetManaged>(&rig.caller, &tv, &body(json!({ "managed": true })))
        .await
        .expect("manage ok");
    lock_requested_after_remote_power(&mut rig, "mock://remote-unmanaged", tv).await;
}

/// While a stream arms the TV, its remote's power key locks nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_holds_the_remote_power_lock_back() {
    let mut rig = rig().await;
    let tv = add_tv(&rig, "Den", "mock://remote-stream").await;
    let sub = history_sub(&rig.client, keys::tv_all(&rig.site)).await;
    wait_tv_state(&sub, &rig.site, tv, "state/link", |v| {
        v["connected"] == true
    })
    .await;
    create_and_assign(
        &rig,
        tv,
        json!({ "name": "Lock on the remote", "on_remote_power": "lock" }),
    )
    .await;
    // Unarmed, it locks (so the core holds the TV's meta from here on).
    lock_requested_after_remote_power(&mut rig, "mock://remote-stream", tv).await;

    let fact = history_sub(&rig.user, keys::user_stream_state(&rig.site)).await;
    assert!(
        rig.stack
            .stream
            .report(StreamReport::manual("manual", true)),
        "core gone"
    );
    recv_until(&fact, |s| {
        s.kind() == zenoh::sample::SampleKind::Put
            && payload_json(s)["armed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|id| id == &json!(tv.to_string())))
    })
    .await;

    rig.mock.press_remote_power("mock://remote-stream");
    assert!(
        tokio::time::timeout(Duration::from_millis(600), rig.stack.lock_requests.recv())
            .await
            .is_err(),
        "a stream must hold the lock back"
    );
}

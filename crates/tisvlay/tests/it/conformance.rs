//! One set of cases every mirror must pass: each client against a fake
//! service, and the services' mirrors of each other against the real
//! stacks, with a [`Relay`] between them that breaks the link the way the
//! OS does while the service keeps running.
//!
//! A cut is a link whose lease expired after the PC woke: the service's
//! token goes and comes back under the same zenoh session. A stall is
//! the frozen slice before the sleep. A dropped batch is a sample lost
//! within a link's lifetime.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tisvlay::{DaemonClient, SessionClient, TrayClient};
use tisvlay_protocol::payload::{self, StatePayload};
use tisvlay_protocol::{VERSION, keys};
use tisvlay_test::Relay;
use uuid::Uuid;

use crate::fake::{FakeService, Family, SITE};

/// How long a case waits for a mirror to converge.
const CONVERGE: Duration = Duration::from_secs(20);

/// Poll `check` until it holds, failing with `what` at the deadline.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + CONVERGE;
    while !check().await {
        assert!(tokio::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A temp dir holding a service socket and the relay in front of it.
struct Sockets {
    _dir: TempDir,
    service: PathBuf,
    relay: Relay,
}

fn sockets() -> Sockets {
    let dir = tisvlay_test::socket_tempdir();
    let service = dir.path().join("service.sock");
    let relay = Relay::start(&dir.path().join("relay.sock"), &service);
    Sockets {
        _dir: dir,
        service,
        relay,
    }
}

fn named(name: &str) -> Value {
    json!({ "name": name })
}

async fn system_profile(daemon: &DaemonClient, id: Uuid) -> Option<String> {
    daemon.get_system_profile(id).await.map(|body| body.name)
}

async fn user_profile(session: &SessionClient, id: Uuid) -> Option<String> {
    session.get_user_profile(id).await.map(|body| body.name)
}

/// Cut the relay, wait until `down` holds, then restore it.
async fn cut_while<F, Fut>(relay: &Relay, down: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    relay.cut();
    eventually("the service never went away behind the cut", down).await;
    relay.restore();
}

// --- The daemon's mirror --------------------------------------------------

struct DaemonRig {
    sockets: Sockets,
    fake: FakeService,
    client: DaemonClient,
}

async fn daemon_rig() -> DaemonRig {
    let sockets = sockets();
    let fake = FakeService::start(&sockets.service, Family::Daemon, VERSION).await;
    let client = DaemonClient::connect(sockets.relay.front(), SITE)
        .await
        .unwrap();
    DaemonRig {
        sockets,
        fake,
        client,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_survives_a_cut_link() {
    let mut rig = daemon_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::profile_system(SITE, &profile);
    rig.fake.put(&key, &named("Kept")).await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        system_profile(client, profile).await.as_deref() == Some("Kept")
    })
    .await;

    cut_while(&rig.sockets.relay, async || !client.is_available().await).await;
    eventually("the profile never came back", async || {
        system_profile(client, profile).await.as_deref() == Some("Kept")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_survives_a_stalled_then_cut_link() {
    let mut rig = daemon_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::profile_system(SITE, &profile);
    rig.fake.put(&key, &named("Before")).await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        system_profile(client, profile).await.as_deref() == Some("Before")
    })
    .await;

    rig.sockets.relay.stall();
    rig.fake.put(&key, &named("During")).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    cut_while(&rig.sockets.relay, async || !client.is_available().await).await;
    eventually("the change made while stalled never arrived", async || {
        system_profile(client, profile).await.as_deref() == Some("During")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_replaces_a_restarted_daemon() {
    let DaemonRig {
        sockets,
        mut fake,
        client,
    } = daemon_rig().await;
    let (old, new) = (Uuid::from_u128(1), Uuid::from_u128(2));
    fake.put(&keys::profile_system(SITE, &old), &named("Old"))
        .await;
    eventually("the old profile never hydrated", async || {
        system_profile(&client, old).await.is_some()
    })
    .await;

    fake.stop().await;
    eventually("the daemon never went away", async || {
        !client.is_available().await
    })
    .await;
    let mut fake = FakeService::start(&sockets.service, Family::Daemon, VERSION).await;
    fake.put(&keys::profile_system(SITE, &new), &named("New"))
        .await;
    eventually("the new lifetime never replaced the old", async || {
        system_profile(&client, new).await.is_some() && system_profile(&client, old).await.is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_forgets_a_profile_deleted_while_down() {
    let mut rig = daemon_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::profile_system(SITE, &profile);
    rig.fake.put(&key, &named("Doomed")).await;
    rig.fake
        .put(
            &keys::profile_system(SITE, &Uuid::from_u128(2)),
            &named("Kept"),
        )
        .await;
    let client = &rig.client;
    eventually("the profiles never hydrated", async || {
        client.list_system_profile_bodies().await.len() == 2
    })
    .await;

    rig.sockets.relay.cut();
    eventually("the daemon never went away", async || {
        !client.is_available().await
    })
    .await;
    rig.fake.delete(&key).await;
    rig.sockets.relay.restore();
    eventually("the kept profile never came back", async || {
        system_profile(client, Uuid::from_u128(2)).await.is_some()
    })
    .await;
    assert_eq!(system_profile(client, profile).await, None);
}

/// A record deleted and then re-created under the same key: its new
/// publisher must reach a client that re-subscribes afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_follows_a_profile_recreated_under_its_key() {
    let mut rig = daemon_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::profile_system(SITE, &profile);
    rig.fake.put(&key, &named("First")).await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        system_profile(client, profile).await.is_some()
    })
    .await;
    rig.fake.delete(&key).await;
    eventually("the delete never arrived", async || {
        system_profile(client, profile).await.is_none()
    })
    .await;
    rig.fake.put(&key, &named("Second")).await;
    eventually("the re-created profile never arrived", async || {
        system_profile(client, profile).await.as_deref() == Some("Second")
    })
    .await;

    cut_while(&rig.sockets.relay, async || !client.is_available().await).await;
    eventually("the re-created profile never came back", async || {
        system_profile(client, profile).await.as_deref() == Some("Second")
    })
    .await;
    rig.fake.put(&key, &named("Third")).await;
    eventually("a later change never arrived", async || {
        system_profile(client, profile).await.as_deref() == Some("Third")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_recovers_a_put_lost_within_a_lifetime() {
    let mut rig = daemon_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::profile_system(SITE, &profile);
    rig.fake.put(&key, &named("Before")).await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        system_profile(client, profile).await.is_some()
    })
    .await;

    rig.sockets.relay.drop_batch_containing(b"LostInTransit");
    rig.fake.put(&key, &named("LostInTransit")).await;
    eventually("the relay never saw the sample", async || {
        rig.sockets.relay.dropped()
    })
    .await;
    eventually("the lost sample was never recovered", async || {
        system_profile(client, profile).await.as_deref() == Some("LostInTransit")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_mirror_shows_a_tv_only_with_its_identity() {
    let mut rig = daemon_rig().await;
    let (partial, whole) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let power = payload::Power {
        state: tisvlay_core::PowerState::Active,
        is_on: true,
        is_screen_on: true,
    };
    rig.fake
        .put(
            &keys::tv_path(SITE, &partial, payload::Power::PATH),
            &serde_json::to_value(&power).unwrap(),
        )
        .await;
    let identity = payload::Identity::new("Den", "mock");
    rig.fake
        .put(
            &keys::tv_path(SITE, &whole, payload::Identity::PATH),
            &serde_json::to_value(&identity).unwrap(),
        )
        .await;
    rig.fake
        .put(
            &keys::tv_path(SITE, &whole, payload::Meta::PATH),
            &serde_json::to_value(payload::Meta::new(true)).unwrap(),
        )
        .await;
    let client = &rig.client;
    eventually("the whole TV never hydrated", async || {
        client
            .list_tvs()
            .await
            .unwrap()
            .iter()
            .any(|tv| tv.uuid == whole)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let tvs = client.list_tvs().await.unwrap();
    assert!(
        tvs.iter().all(|tv| tv.uuid != partial),
        "a TV without its identity made a row: {tvs:?}"
    );
}

// --- The session's mirror -------------------------------------------------

struct SessionRig {
    sockets: Sockets,
    fake: FakeService,
    client: SessionClient,
}

async fn session_rig() -> SessionRig {
    let sockets = sockets();
    let fake = FakeService::start(&sockets.service, Family::Session, VERSION).await;
    let client = SessionClient::connect(sockets.relay.front(), SITE)
        .await
        .unwrap();
    SessionRig {
        sockets,
        fake,
        client,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn session_mirror_survives_a_cut_link() {
    let mut rig = session_rig().await;
    let profile = Uuid::from_u128(1);
    rig.fake
        .put(&keys::user_profile(SITE, &profile), &named("Kept"))
        .await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        user_profile(client, profile).await.is_some()
    })
    .await;

    cut_while(&rig.sockets.relay, async || !client.is_available().await).await;
    eventually("the session never came back", async || {
        client.is_available().await
    })
    .await;
    eventually("the profile never came back", async || {
        user_profile(client, profile).await.as_deref() == Some("Kept")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_mirror_clears_while_down() {
    let mut rig = session_rig().await;
    let profile = Uuid::from_u128(1);
    rig.fake
        .put(&keys::user_profile(SITE, &profile), &named("Kept"))
        .await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        user_profile(client, profile).await.is_some()
    })
    .await;

    rig.sockets.relay.cut();
    eventually("the session never went away", async || {
        !client.is_available().await
    })
    .await;
    assert_eq!(user_profile(client, profile).await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn session_mirror_forgets_a_profile_deleted_while_down() {
    let mut rig = session_rig().await;
    let (doomed, kept) = (Uuid::from_u128(1), Uuid::from_u128(2));
    let key = keys::user_profile(SITE, &doomed);
    rig.fake.put(&key, &named("Doomed")).await;
    rig.fake
        .put(&keys::user_profile(SITE, &kept), &named("Kept"))
        .await;
    let client = &rig.client;
    eventually("the profiles never hydrated", async || {
        client.list_user_profile_bodies().await.len() == 2
    })
    .await;

    rig.sockets.relay.cut();
    eventually("the session never went away", async || {
        !client.is_available().await
    })
    .await;
    rig.fake.delete(&key).await;
    rig.sockets.relay.restore();
    eventually("the session never came back", async || {
        client.is_available().await
    })
    .await;
    eventually("the deleted profile stayed", async || {
        user_profile(client, kept).await.is_some() && user_profile(client, doomed).await.is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_mirror_replaces_a_restarted_session() {
    let SessionRig {
        sockets,
        mut fake,
        client,
    } = session_rig().await;
    let (old, new) = (Uuid::from_u128(1), Uuid::from_u128(2));
    fake.put(&keys::user_profile(SITE, &old), &named("Old"))
        .await;
    eventually("the old profile never hydrated", async || {
        user_profile(&client, old).await.is_some()
    })
    .await;

    fake.stop().await;
    eventually("the session never went away", async || {
        !client.is_available().await
    })
    .await;
    let mut fake = FakeService::start(&sockets.service, Family::Session, VERSION).await;
    fake.put(&keys::user_profile(SITE, &new), &named("New"))
        .await;
    eventually("the new lifetime never replaced the old", async || {
        user_profile(&client, new).await.is_some() && user_profile(&client, old).await.is_none()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_mirror_recovers_a_put_lost_within_a_lifetime() {
    let mut rig = session_rig().await;
    let profile = Uuid::from_u128(1);
    let key = keys::user_profile(SITE, &profile);
    rig.fake.put(&key, &named("Before")).await;
    let client = &rig.client;
    eventually("the profile never hydrated", async || {
        user_profile(client, profile).await.is_some()
    })
    .await;

    rig.sockets.relay.drop_batch_containing(b"LostInTransit");
    rig.fake.put(&key, &named("LostInTransit")).await;
    eventually("the relay never saw the sample", async || {
        rig.sockets.relay.dropped()
    })
    .await;
    eventually("the lost sample was never recovered", async || {
        user_profile(client, profile).await.as_deref() == Some("LostInTransit")
    })
    .await;
}

/// Rows come from the service that owns them: an assignment the session
/// holds for a TV the daemon doesn't have is a lookup that misses, not a
/// TV.
#[tokio::test(flavor = "multi_thread")]
async fn an_assignment_for_an_unknown_tv_makes_no_row() {
    let mut daemon = daemon_rig().await;
    let mut session = session_rig().await;
    let (known, unknown, profile) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    for (path, body) in [
        (
            payload::Identity::PATH,
            serde_json::to_value(payload::Identity::new("Den", "mock")).unwrap(),
        ),
        (
            payload::Meta::PATH,
            serde_json::to_value(payload::Meta::new(true)).unwrap(),
        ),
    ] {
        daemon
            .fake
            .put(&keys::tv_path(SITE, &known, path), &body)
            .await;
    }
    session
        .fake
        .put(&keys::user_profile(SITE, &profile), &named("Quiet"))
        .await;
    session
        .fake
        .put(
            &keys::user_tv_profile(SITE, &unknown),
            &json!({ "profile": profile }),
        )
        .await;
    let (tvs, users) = (&daemon.client, &session.client);
    eventually("the assignment never hydrated", async || {
        users.get_tv_user_profile(unknown).await == Some(profile)
    })
    .await;
    eventually("the known TV never hydrated", async || {
        tvs.list_tvs()
            .await
            .unwrap()
            .iter()
            .any(|tv| tv.uuid == known)
    })
    .await;
    let rows = tvs.list_tvs().await.unwrap();
    assert!(
        rows.iter().all(|tv| tv.uuid != unknown),
        "an assignment made a TV row: {rows:?}"
    );
}

// --- The tray settings' mirror --------------------------------------------

struct TrayRig {
    sockets: Sockets,
    fake: FakeService,
    client: TrayClient,
}

async fn tray_rig() -> TrayRig {
    let sockets = sockets();
    let mut fake = FakeService::start(&sockets.service, Family::Tray, VERSION).await;
    fake.put(
        &keys::user_tray_settings(SITE),
        &serde_json::to_value(tisvlay::NotificationSettings::default()).unwrap(),
    )
    .await;
    let client = TrayClient::connect(sockets.relay.front(), SITE)
        .await
        .unwrap();
    eventually("the settings never hydrated", async || {
        client.notification_settings().await.is_some()
    })
    .await;
    TrayRig {
        sockets,
        fake,
        client,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn tray_mirror_survives_a_cut_link() {
    let rig = tray_rig().await;
    let client = &rig.client;
    cut_while(&rig.sockets.relay, async || !client.is_available().await).await;
    eventually("the tray never came back", async || {
        client.is_available().await
    })
    .await;
    eventually("the settings never came back", async || {
        client.notification_settings().await.is_some()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tray_mirror_clears_while_down() {
    let TrayRig {
        sockets,
        fake,
        client,
    } = tray_rig().await;
    sockets.relay.cut();
    eventually("the tray never went away", async || {
        !client.is_available().await
    })
    .await;
    assert_eq!(client.notification_settings().await, None);
    fake.stop().await;
}

// --- The services' mirrors of each other ----------------------------------

/// The real daemon and session, the session's system link through a
/// relay, and a client of each dialing its service directly.
struct ServicesRig {
    _dir: TempDir,
    relay: Relay,
    _daemon: tisvlay_daemon::Stack,
    _session: tisvlay_session::SessionStack,
    daemon: DaemonClient,
    session: SessionClient,
}

async fn services_rig() -> ServicesRig {
    let dir = tisvlay_test::socket_tempdir();
    let system_socket = dir.path().join("sys.sock");
    let user_socket = dir.path().join("user.sock");
    let relay = Relay::start(&dir.path().join("relay.sock"), &system_socket);
    let (connector, discovery, _handle) = tisvlay_mock::new_test_parts();
    let daemon_stack = tisvlay_daemon::serve(
        &system_socket,
        SITE,
        vec![
            tisvlay_daemon::VendorReg::new("mock", "Mock", Arc::new(connector))
                .with_discovery(Arc::new(discovery)),
        ],
        None,
    )
    .await
    .unwrap();
    let session_stack = tisvlay_session::serve(tisvlay_session::SessionOptions {
        system_socket: relay.front().to_path_buf(),
        user_socket: user_socket.clone(),
        site: SITE.into(),
        store: None,
        sunshine: tisvlay_session::sunshine::StaticProbe::absent(),
    })
    .await
    .unwrap();
    let daemon = DaemonClient::connect(&system_socket, SITE).await.unwrap();
    let session = SessionClient::connect(&user_socket, SITE).await.unwrap();
    eventually("the services never came up", async || {
        daemon.is_available().await && session.is_available().await
    })
    .await;
    ServicesRig {
        _dir: dir,
        relay,
        _daemon: daemon_stack,
        _session: session_stack,
        daemon,
        session,
    }
}

/// Pair a mock TV and wait until it's connected.
async fn pair(daemon: &DaemonClient, address: &str) -> Uuid {
    let config =
        serde_json::from_value(json!({ "address": address, "pairing": "accept" })).unwrap();
    let tv = daemon
        .pair_tv("Den", "mock", config, true)
        .await
        .unwrap()
        .id;
    eventually("the TV never connected", async || {
        daemon
            .list_tvs()
            .await
            .unwrap()
            .iter()
            .any(|info| info.uuid == tv && info.connected)
    })
    .await;
    tv
}

async fn armed(daemon: &DaemonClient, tv: Uuid) -> bool {
    daemon
        .list_tvs()
        .await
        .unwrap()
        .iter()
        .any(|info| info.uuid == tv && info.stream_armed)
}

/// The daemon disarms every TV when the session's token goes, and must
/// arm them again from the session's stream state when it returns.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "the daemon's stream mirror subscribes once, so the returning state is dropped as already seen"]
async fn a_streaming_tv_is_armed_again_when_the_sessions_link_returns() {
    let rig = services_rig().await;
    let tv = pair(&rig.daemon, "mock://stream").await;
    rig.session.stream_set("laptop", true).await.unwrap();
    let daemon = &rig.daemon;
    eventually("the TV never armed", async || armed(daemon, tv).await).await;

    cut_while(&rig.relay, async || !armed(daemon, tv).await).await;
    eventually("the TV never armed again", async || armed(daemon, tv).await).await;
}

/// The session's view of the fleet must lose a TV removed while its
/// system link was down: here, through the armed set it computes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "the session's fleet mirror subscribes once, so a removal made while it's down is missed"]
async fn the_session_forgets_a_tv_removed_while_its_link_was_down() {
    let rig = services_rig().await;
    let (doomed, kept) = (
        pair(&rig.daemon, "mock://doomed").await,
        pair(&rig.daemon, "mock://kept").await,
    );
    rig.session.stream_set("laptop", true).await.unwrap();
    let session = &rig.session;
    let armed_set = async || {
        session
            .stream_state()
            .await
            .map(|state| state.armed)
            .unwrap_or_default()
    };
    eventually("the TVs never armed", async || {
        let armed = armed_set().await;
        armed.contains(&doomed) && armed.contains(&kept)
    })
    .await;

    rig.relay.cut();
    rig.daemon.remove_tv(doomed).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.relay.restore();
    rig.session.stream_set("laptop", false).await.unwrap();
    rig.session.stream_set("laptop", true).await.unwrap();
    eventually("the removed TV stayed armed", async || {
        let armed = armed_set().await;
        armed.contains(&kept) && !armed.contains(&doomed)
    })
    .await;
}

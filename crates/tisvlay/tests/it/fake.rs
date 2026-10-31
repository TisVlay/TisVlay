//! A stand-in service built from the protocol's `serve` dispatch, as a
//! third party might mock one: it listens on a socket, holds its
//! liveliness token, answers `info` with a chosen release and a reload
//! with nothing changed, and publishes whatever keys a test puts, through
//! cached publishers configured like the real services', listing them in
//! its `info` reply.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tisvlay_protocol::keys;
use tokio::task::JoinHandle;
use zenoh::Session;
use zenoh::liveliness::LivelinessToken;
use zenoh_ext::{AdvancedPublisher, AdvancedPublisherBuilderExt, CacheConfig, MissDetectionConfig};

/// The site every fake serves.
pub(crate) const SITE: &str = "t";

/// Which service a fake stands in for: its rpc family and token.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Family {
    Daemon,
    Session,
    Tray,
}

/// A running fake service. Its publishers live as long as it does.
pub(crate) struct FakeService {
    session: Session,
    token: LivelinessToken,
    serve: JoinHandle<()>,
    publishers: HashMap<String, AdvancedPublisher<'static>>,
    /// The keys the `info` reply lists.
    listed: Arc<Mutex<BTreeSet<String>>>,
}

/// Answer `info` and `reload` on `$queryable` for rpc family `$family`.
macro_rules! answer {
    ($family:ident, $queryable:expr, $release:expr, $listed:expr) => {{
        use tisvlay_protocol::endpoint::$family;
        let queryable = $queryable;
        let release = $release;
        let listed = Arc::clone($listed);
        tokio::spawn(async move {
            while let Ok(query) = queryable.recv_async().await {
                match $family::Incoming::receive(SITE, query).await {
                    Some((_, $family::Incoming::Info((), responder))) => {
                        let info = tisvlay_protocol::rpc::ServiceInfo {
                            version: release.clone(),
                            state_path: String::new(),
                            sleep: None,
                            keys: listed.lock().unwrap().iter().cloned().collect(),
                        };
                        responder.ok(&info).await;
                    }
                    Some((_, $family::Incoming::Reload((), responder))) => {
                        let report = tisvlay_protocol::rpc::ReloadReport { changed: false };
                        responder.ok(&report).await;
                    }
                    _ => {}
                }
            }
        })
    }};
}

impl FakeService {
    /// Serve `family` on `socket`, reporting `release`.
    pub(crate) async fn start(socket: &Path, family: Family, release: &str) -> Self {
        let session = zenoh::open(tisvlay_protocol::bootstrap::peer_config(socket).unwrap())
            .await
            .unwrap();
        let (rpc_wild, alive) = match family {
            Family::Daemon => (keys::manager_rpc_wild(SITE), keys::daemon_alive(SITE)),
            Family::Session => (
                keys::user_profile_rpc_wild(SITE),
                keys::user_alive_session(SITE),
            ),
            Family::Tray => (keys::user_tray_rpc_wild(SITE), keys::user_alive_tray(SITE)),
        };
        let queryable = session.declare_queryable(rpc_wild).await.unwrap();
        let release = release.to_string();
        let listed = Arc::new(Mutex::new(BTreeSet::new()));
        let serve = match family {
            Family::Daemon => answer!(manager, queryable, release, &listed),
            Family::Session => answer!(user_profile, queryable, release, &listed),
            Family::Tray => answer!(user_tray, queryable, release, &listed),
        };
        let token = session.liveliness().declare_token(alive).await.unwrap();
        Self {
            session,
            token,
            serve,
            publishers: HashMap::new(),
            listed,
        }
    }

    /// Publish `body` on `key`, declaring its publisher on first use.
    pub(crate) async fn put(&mut self, key: &str, body: &Value) {
        if !self.publishers.contains_key(key) {
            let publisher = self
                .session
                .declare_publisher(key.to_string())
                .cache(CacheConfig::default().max_samples(1))
                .sample_miss_detection(
                    MissDetectionConfig::default().sporadic_heartbeat(Duration::from_millis(500)),
                )
                .publisher_detection()
                .await
                .unwrap();
            self.publishers.insert(key.to_string(), publisher);
            self.listed.lock().unwrap().insert(key.to_string());
        }
        self.publishers[key]
            .put(serde_json::to_vec(body).unwrap())
            .await
            .unwrap();
    }

    /// Delete `key` through its publisher, then drop the publisher, as
    /// the real services do.
    pub(crate) async fn delete(&mut self, key: &str) {
        let publisher = self.publishers.remove(key).unwrap();
        self.listed.lock().unwrap().remove(key);
        publisher.delete().await.unwrap();
    }

    /// List `key` in the `info` reply without publishing it: a key the
    /// service published, then deleted after the reply.
    pub(crate) fn list_unpublished(&self, key: &str) {
        self.listed.lock().unwrap().insert(key.to_string());
    }

    /// Stop: the token, the rpcs and the publishers go with the session.
    pub(crate) async fn stop(self) {
        self.serve.abort();
        drop(self.publishers);
        drop(self.token);
        self.session.close().await.unwrap();
    }
}

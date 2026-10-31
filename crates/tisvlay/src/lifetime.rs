//! Following one service's lifetime: the same way for every mirror.
//!
//! A service's lifetime is its liveliness token's presence. While the
//! token is present, the mirror holds one subscription per key expression
//! it mirrors, each replaying the service's caches as history; when the
//! token goes, the subscriptions go first, then the mirror's state.
//!
//! The subscriptions can't be kept across a lifetime: zenoh-ext's
//! advanced subscriber keeps each publisher's last delivered sequence
//! number and drops replayed samples at or below it, so a mirror that
//! cleared would never get an unchanged value back. A token that returns,
//! a restarted service's or the same one's after its links dropped (the
//! PC woke with this process frozen past the link lease), gets fresh
//! subscriptions that hydrate from the caches again.
//!
//! Within a lifetime, recovery retransmits a sample lost on the way from
//! the publisher's cache, once the publisher's heartbeat shows the gap.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::{manager, user_profile, user_tray};
use tisvlay_protocol::keys;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::warn;
use zenoh::Session;
use zenoh::handlers::FifoChannelHandler;
use zenoh::pubsub::Subscriber;
use zenoh::sample::{Sample, SampleKind};
use zenoh_ext::{AdvancedSubscriber, AdvancedSubscriberBuilderExt, HistoryConfig, RecoveryConfig};

use crate::handshake::Handshake;
use crate::{EVENT_CAPACITY, Error, Result, Service, ServiceInfo, StateEvent, on_liveliness};

/// The first wait before a failed subscription is retried, doubling up
/// to [`SUBSCRIBE_RETRY_MAX`].
const SUBSCRIBE_RETRY_FIRST: Duration = Duration::from_millis(100);
const SUBSCRIBE_RETRY_MAX: Duration = Duration::from_secs(5);

/// One service's mirrored state: what [`LifetimeMirror`] keeps current.
pub(crate) trait Mirror: Default + std::fmt::Debug + Send + 'static {
    /// The service whose keys this mirrors.
    const SERVICE: Service;

    /// The key expressions to subscribe for `site`.
    fn keys(site: &str) -> Vec<String>;

    /// Apply one sample; the returned event (if any) is what a watcher
    /// should broadcast.
    fn apply(&mut self, site: &str, sample: &Sample) -> Option<StateEvent>;

    /// Drop everything, the service having gone; one event per row that
    /// was showing.
    fn clear(&mut self, site: &str) -> Vec<StateEvent>;
}

#[derive(Debug, Default)]
struct Tracked<M> {
    mirror: M,
    /// Whether the service's token is present and every subscription is
    /// declared.
    alive: bool,
}

/// A mirror that follows its service's lifetime: see the module docs.
#[derive(Debug)]
pub(crate) struct LifetimeMirror<M> {
    tracked: Arc<Mutex<Tracked<M>>>,
    events: broadcast::Sender<StateEvent>,
    handshake: Arc<Handshake>,
}

impl<M> Clone for LifetimeMirror<M> {
    fn clone(&self) -> Self {
        Self {
            tracked: Arc::clone(&self.tracked),
            events: self.events.clone(),
            handshake: Arc::clone(&self.handshake),
        }
    }
}

/// One lifetime's tasks: the setup that subscribes (retrying until it
/// can), and a task per subscription.
struct Lifetime {
    setup: JoinHandle<()>,
    pumps: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Lifetime {
    /// Stop every task and wait for each, so no sample is still being
    /// applied once this returns.
    async fn end(self) {
        self.setup.abort();
        let _ = self.setup.await;
        let pumps = std::mem::take(
            &mut *self
                .pumps
                .lock()
                .expect("nothing panics while the pump list is held"),
        );
        for pump in pumps {
            pump.abort();
            let _ = pump.await;
        }
    }
}

impl<M: Mirror> LifetimeMirror<M> {
    /// Follow `M`'s service on `session`: its liveliness from history on,
    /// then a lifetime per token.
    pub(crate) async fn start(session: &Session, site: &str, caller: &Arc<Caller>) -> Result<Self> {
        let liveliness = session
            .liveliness()
            .declare_subscriber(alive_key(M::SERVICE, site))
            .history(true)
            .await
            .map_err(|e| Error::transport("subscribe to the service's liveliness", e))?;
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let this = Self {
            tracked: Arc::new(Mutex::new(Tracked::default())),
            events,
            handshake: Arc::new(Handshake::new(M::SERVICE)),
        };
        tokio::spawn(this.clone().follow(
            liveliness,
            session.clone(),
            site.to_string(),
            Arc::clone(caller),
        ));
        Ok(this)
    }

    /// Read the mirror.
    pub(crate) fn read<R>(&self, read: impl FnOnce(&M) -> R) -> R {
        read(&self.lock().mirror)
    }

    /// Whether the service's token is present and its keys subscribed.
    pub(crate) fn is_available(&self) -> bool {
        self.lock().alive
    }

    /// Where the mirror's events, and the release check's, go out.
    pub(crate) fn events(&self) -> &broadcast::Sender<StateEvent> {
        &self.events
    }

    /// The service's release check, cleared with each lifetime.
    pub(crate) fn handshake(&self) -> &Arc<Handshake> {
        &self.handshake
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Tracked<M>> {
        self.tracked
            .lock()
            .expect("nothing panics while the mirror lock is held")
    }

    async fn follow(
        self,
        liveliness: Subscriber<FifoChannelHandler<Sample>>,
        session: Session,
        site: String,
        caller: Arc<Caller>,
    ) {
        let mut lifetime: Option<Lifetime> = None;
        while let Ok(sample) = liveliness.recv_async().await {
            if sample.kind() == SampleKind::Put {
                if lifetime.is_none() {
                    lifetime = Some(self.begin(&session, &site, &caller));
                }
                continue;
            }
            if let Some(lifetime) = lifetime.take() {
                lifetime.end().await;
            }
            let removed = {
                let mut tracked = self.lock();
                tracked.alive = false;
                tracked.mirror.clear(&site)
            };
            for event in removed {
                let _ = self.events.send(event);
            }
            on_liveliness(
                false,
                disappeared(M::SERVICE),
                &self.handshake,
                &self.events,
                &caller,
                M::SERVICE,
                |caller| service_info(caller, M::SERVICE),
            );
        }
    }

    /// Subscribe, then report the service available: a client that waits
    /// for it finds the subscriptions in place, their history replaying
    /// as it arrives.
    fn begin(&self, session: &Session, site: &str, caller: &Arc<Caller>) -> Lifetime {
        let pumps = Arc::new(Mutex::new(Vec::new()));
        let setup = tokio::spawn({
            let this = self.clone();
            let session = session.clone();
            let site = site.to_string();
            let caller = Arc::clone(caller);
            let pumps = Arc::clone(&pumps);
            async move {
                for subscriber in subscribe::<M>(&session, &site).await {
                    let pump = tokio::spawn(this.clone().pump(subscriber, site.clone()));
                    pumps
                        .lock()
                        .expect("nothing panics while the pump list is held")
                        .push(pump);
                }
                this.lock().alive = true;
                on_liveliness(
                    true,
                    appeared(M::SERVICE),
                    &this.handshake,
                    &this.events,
                    &caller,
                    M::SERVICE,
                    |caller| service_info(caller, M::SERVICE),
                );
            }
        });
        Lifetime { setup, pumps }
    }

    async fn pump(self, subscriber: AdvancedSubscriber<FifoChannelHandler<Sample>>, site: String) {
        while let Ok(sample) = subscriber.recv_async().await {
            let event = self.lock().mirror.apply(&site, &sample);
            if let Some(event) = event {
                let _ = self.events.send(event);
            }
        }
    }
}

/// Subscribe to every key `M` mirrors, from history on and with recovery,
/// retrying until all are declared.
async fn subscribe<M: Mirror>(
    session: &Session,
    site: &str,
) -> Vec<AdvancedSubscriber<FifoChannelHandler<Sample>>> {
    let mut backoff = SUBSCRIBE_RETRY_FIRST;
    loop {
        let mut subscribers = Vec::new();
        let mut failed = None;
        for key in M::keys(site) {
            let declared = session
                .declare_subscriber(key)
                .history(HistoryConfig::default().detect_late_publishers())
                .recovery(RecoveryConfig::default().heartbeat())
                .await;
            match declared {
                Ok(subscriber) => subscribers.push(subscriber),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        let Some(error) = failed else {
            return subscribers;
        };
        warn!(service = %M::SERVICE, %error, ?backoff, "couldn't subscribe to the service's keys; retrying");
        drop(subscribers);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(SUBSCRIBE_RETRY_MAX);
    }
}

fn alive_key(service: Service, site: &str) -> String {
    match service {
        Service::Daemon => keys::daemon_alive(site),
        Service::Session => keys::user_alive_session(site),
        Service::Tray => keys::user_alive_tray(site),
    }
}

fn appeared(service: Service) -> StateEvent {
    match service {
        Service::Daemon => StateEvent::DaemonAppeared,
        Service::Session => StateEvent::SessionAppeared,
        Service::Tray => StateEvent::TrayAppeared,
    }
}

fn disappeared(service: Service) -> StateEvent {
    match service {
        Service::Daemon => StateEvent::DaemonDisappeared,
        Service::Session => StateEvent::SessionDisappeared,
        Service::Tray => StateEvent::TrayDisappeared,
    }
}

async fn service_info(
    caller: Arc<Caller>,
    service: Service,
) -> std::result::Result<ServiceInfo, RpcError> {
    match service {
        Service::Daemon => caller.call::<manager::Info>(&(), &()).await,
        Service::Session => caller.call::<user_profile::Info>(&(), &()).await,
        Service::Tray => caller.call::<user_tray::Info>(&(), &()).await,
    }
}

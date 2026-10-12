//! The release check each client runs against its service. A published
//! SDK talks only to services of its own release line (the same
//! `major.minor`, see [`tisvlay_protocol::compatible`]); the frozen `info`
//! rpc tells it the service's release before anything else is decoded.
//!
//! The verdict holds for one lifetime of the service: it's cleared when
//! the service's liveliness token goes away, so a service restarted on
//! another release (a package upgrade under a running client) is checked
//! again.

use std::future::Future;
use std::sync::Mutex;

use tisvlay_protocol::VERSION;
use tokio::sync::broadcast;

use crate::{Error, Result, Service, StateEvent};

/// What a client knows about its service's release.
#[derive(Debug)]
pub(crate) struct Handshake {
    service: Service,
    /// Serializes checks, so calls wait for one in flight instead of each
    /// asking. Never held while `known` is locked.
    checking: tokio::sync::Mutex<()>,
    /// The settled verdict for the current lifetime. A plain mutex, never
    /// held across an await, so a reader always sees a settled verdict.
    known: Mutex<Known>,
}

#[derive(Debug, Default)]
struct Known {
    /// Bumped each time the service goes away, so a check started in one
    /// lifetime can't settle the next one's verdict.
    lifetime: u64,
    verdict: Option<Verdict>,
}

#[derive(Debug, Clone)]
enum Verdict {
    Compatible,
    Mismatch(String),
}

impl Handshake {
    pub(crate) fn new(service: Service) -> Self {
        Self {
            service,
            checking: tokio::sync::Mutex::new(()),
            known: Mutex::new(Known::default()),
        }
    }

    /// Check the service's release unless this lifetime already has a
    /// verdict, then pass a compatible one and fail a mismatch. Calls wait
    /// for a check in flight, so nothing is sent to a service whose
    /// release isn't known yet. A mismatch is announced once on `events`.
    /// A failure to ask (the service down, a timeout) isn't remembered: the
    /// next call asks again.
    pub(crate) async fn ensure<F, Fut>(
        &self,
        events: &broadcast::Sender<StateEvent>,
        info: F,
    ) -> Result<()>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String>>,
    {
        if let Some(verdict) = self.settled() {
            return self.outcome(&verdict);
        }
        let _checking = self.checking.lock().await;
        let lifetime = {
            let known = self.known.lock().expect("handshake lock");
            if let Some(verdict) = known.verdict.clone() {
                return self.outcome(&verdict);
            }
            known.lifetime
        };
        let theirs = info().await?;
        let verdict = if tisvlay_protocol::compatible(VERSION, &theirs) {
            Verdict::Compatible
        } else {
            Verdict::Mismatch(theirs)
        };
        // Settle it only if the service hasn't gone away meanwhile; the
        // answer still stands for this call either way.
        let mut known = self.known.lock().expect("handshake lock");
        if known.lifetime == lifetime {
            known.verdict = Some(verdict.clone());
            if let Some(event) = self.event(&verdict) {
                let _ = events.send(event);
            }
        }
        drop(known);
        self.outcome(&verdict)
    }

    /// A mismatch already known, without asking: what the mirror reads
    /// respect. A check in flight counts as not known yet.
    pub(crate) fn known_mismatch(&self) -> Option<Error> {
        match self.settled()? {
            Verdict::Mismatch(theirs) => Some(self.mismatch(&theirs)),
            Verdict::Compatible => None,
        }
    }

    /// The [`StateEvent::VersionMismatch`] for a mismatch already known:
    /// what a watcher subscribing late replays, since the announcement
    /// went out before it listened.
    pub(crate) fn known_mismatch_event(&self) -> Option<StateEvent> {
        self.event(&self.settled()?)
    }

    /// The service went away: its next lifetime gets a fresh check. Doesn't
    /// wait for a check in flight; that check's verdict is dropped.
    pub(crate) fn reset(&self) {
        let mut known = self.known.lock().expect("handshake lock");
        known.lifetime += 1;
        known.verdict = None;
    }

    fn settled(&self) -> Option<Verdict> {
        self.known.lock().expect("handshake lock").verdict.clone()
    }

    fn outcome(&self, verdict: &Verdict) -> Result<()> {
        match verdict {
            Verdict::Mismatch(theirs) => Err(self.mismatch(theirs)),
            Verdict::Compatible => Ok(()),
        }
    }

    fn event(&self, verdict: &Verdict) -> Option<StateEvent> {
        match verdict {
            Verdict::Mismatch(theirs) => Some(StateEvent::VersionMismatch {
                service: self.service,
                ours: VERSION.to_owned(),
                theirs: theirs.clone(),
            }),
            Verdict::Compatible => None,
        }
    }

    fn mismatch(&self, theirs: &str) -> Error {
        Error::VersionMismatch {
            service: self.service,
            ours: VERSION.to_owned(),
            theirs: theirs.to_owned(),
        }
    }
}

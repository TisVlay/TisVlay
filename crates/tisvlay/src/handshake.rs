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

use tisvlay_protocol::VERSION;
use tokio::sync::{Mutex, broadcast};

use crate::{Error, Result, Service, StateEvent};

/// What a client knows about its service's release.
#[derive(Debug)]
pub(crate) struct Handshake {
    service: Service,
    verdict: Mutex<Option<Verdict>>,
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
            verdict: Mutex::new(None),
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
        let mut verdict = self.verdict.lock().await;
        if verdict.is_none() {
            let theirs = info().await?;
            *verdict = Some(if tisvlay_protocol::compatible(VERSION, &theirs) {
                Verdict::Compatible
            } else {
                let _ = events.send(StateEvent::VersionMismatch {
                    service: self.service,
                    ours: VERSION.to_owned(),
                    theirs: theirs.clone(),
                });
                Verdict::Mismatch(theirs)
            });
        }
        match verdict.as_ref() {
            Some(Verdict::Mismatch(theirs)) => Err(self.mismatch(theirs)),
            _ => Ok(()),
        }
    }

    /// A mismatch already known, without asking: what the mirror reads
    /// respect. A check in flight counts as not known yet.
    pub(crate) fn known_mismatch(&self) -> Option<Error> {
        match self.verdict.try_lock().ok()?.as_ref() {
            Some(Verdict::Mismatch(theirs)) => Some(self.mismatch(theirs)),
            _ => None,
        }
    }

    /// The service went away: its next lifetime gets a fresh check.
    pub(crate) async fn reset(&self) {
        *self.verdict.lock().await = None;
    }

    fn mismatch(&self, theirs: &str) -> Error {
        Error::VersionMismatch {
            service: self.service,
            ours: VERSION.to_owned(),
            theirs: theirs.to_owned(),
        }
    }
}

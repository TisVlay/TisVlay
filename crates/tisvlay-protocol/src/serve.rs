//! The server half of the endpoint contract: a query's body, the
//! typed [`Responder`] each decoded rpc carries, and refusals. Reply
//! failures (the querier gone) are logged, never fatal — the caller's
//! own timeout reports them.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserialize;
use zenoh::bytes::Encoding;
use zenoh::query::Query;

use crate::endpoint::{Rpc, Streaming};
use crate::error::WireError;

/// The reply owed to one query of rpc `R`, handed out with the decoded
/// request. Answering consumes it, so a handler replies once and only
/// with `R`'s response type; one dropped unanswered logs a warning,
/// since its caller is left to time out.
pub struct Responder<R> {
    query: Option<Query>,
    rpc: PhantomData<fn() -> R>,
}

impl<R: Rpc> Responder<R> {
    pub(crate) fn new(query: Query) -> Self {
        Self {
            query: Some(query),
            rpc: PhantomData,
        }
    }

    /// Answer with `R`'s response.
    pub async fn ok(mut self, response: &R::Response) {
        send(&self.take(), encode(response)).await;
    }

    /// Refuse the request.
    pub async fn err(mut self, error: &WireError) {
        reply_err(&self.take(), error).await;
    }

    /// Answer with a handler's outcome: the response, or the refusal.
    pub async fn answer(self, outcome: Result<R::Response, WireError>) {
        match outcome {
            Ok(response) => self.ok(&response).await,
            Err(error) => self.err(&error).await,
        }
    }

    /// Set the reply aside for a server that answers later through its
    /// own correlation: one map can hold the replies it owes across rpcs.
    /// The daemon files queries for its world, which answers with JSON
    /// typed there by the same `R`. The parked reply remembers `R`, and
    /// checks the JSON it's answered with against `R`'s response type.
    pub fn park(mut self) -> ParkedReply {
        ParkedReply {
            query: Some(self.take()),
            rpc: R::NAME,
            check: check_response::<R>,
        }
    }

    fn take(&mut self) -> Query {
        self.query
            .take()
            .expect("a responder holds its query until answered, and answering consumes it")
    }
}

impl<R: Streaming> Responder<R> {
    /// One streamed reply; the query stays open until [`Self::finish`].
    pub async fn send(&self, item: &R::Response) {
        if let Some(query) = &self.query {
            send(query, encode(item)).await;
        }
    }

    /// Close the stream: the caller's collection ends with the replies
    /// sent so far.
    pub fn finish(mut self) {
        drop(self.take());
    }
}

impl<R> Drop for Responder<R> {
    fn drop(&mut self) {
        if let Some(query) = &self.query {
            tracing::warn!(key = %query.key_expr(), "rpc dropped without a reply");
        }
    }
}

impl<R> fmt::Debug for Responder<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.query.as_ref().map(|query| query.key_expr().as_str());
        f.debug_tuple("Responder").field(&key).finish()
    }
}

/// A [`Responder`] set aside with [`Responder::park`], its rpc type
/// erased so one map can hold replies of every rpc. Answering consumes
/// it, and one dropped unanswered logs a warning, as a responder does.
pub struct ParkedReply {
    query: Option<Query>,
    rpc: &'static str,
    check: fn(&serde_json::Value) -> Result<(), serde_json::Error>,
}

impl ParkedReply {
    /// Answer with a response already encoded as JSON, or refuse. JSON
    /// that doesn't decode as the parked rpc's response type isn't sent:
    /// the server's bug is logged here and the caller gets an
    /// [`ErrorCode::Internal`](crate::error::ErrorCode::Internal) refusal,
    /// rather than a reply it can't decode.
    pub async fn answer(mut self, outcome: Result<serde_json::Value, WireError>) {
        let query = self
            .query
            .take()
            .expect("a parked reply holds its query until answered, and answering consumes it");
        match outcome {
            Ok(response) => match (self.check)(&response) {
                Ok(()) => {
                    let bytes =
                        serde_json::to_vec(&response).expect("a JSON value always encodes as JSON");
                    send(&query, bytes).await;
                }
                Err(error) => {
                    tracing::error!(
                        rpc = self.rpc,
                        key = %query.key_expr(),
                        %error,
                        "parked reply isn't the rpc's response type; refusing instead"
                    );
                    let refusal = WireError::internal(format!(
                        "the service built a malformed {} reply",
                        self.rpc
                    ));
                    reply_err(&query, &refusal).await;
                }
            },
            Err(error) => reply_err(&query, &error).await,
        }
    }
}

/// Whether `value` decodes as `R`'s response: what a [`ParkedReply`]
/// checks before sending.
fn check_response<R: Rpc>(value: &serde_json::Value) -> Result<(), serde_json::Error> {
    R::Response::deserialize(value).map(drop)
}

impl Drop for ParkedReply {
    fn drop(&mut self) {
        if let Some(query) = &self.query {
            tracing::warn!(key = %query.key_expr(), "parked rpc dropped without a reply");
        }
    }
}

impl fmt::Debug for ParkedReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.query.as_ref().map(|query| query.key_expr().as_str());
        f.debug_tuple("ParkedReply").field(&key).finish()
    }
}

fn encode<T: serde::Serialize>(response: &T) -> Vec<u8> {
    serde_json::to_vec(response).expect(
            "rpc responses are serde structs, string-keyed maps and lists of them, which always encode as JSON",
        )
}

/// A query's body bytes, empty when it carried none.
pub(crate) fn payload(query: &Query) -> Vec<u8> {
    query
        .payload()
        .map(|payload| payload.to_bytes().into_owned())
        .unwrap_or_default()
}

/// Refuse `query` with a typed error: a query no family decoded, or a
/// parked reply's refusal.
pub(crate) async fn reply_err(query: &Query, error: &WireError) {
    let bytes = serde_json::to_vec(error).expect("WireError always encodes as JSON");
    if let Err(e) = query
        .reply_err(bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        tracing::warn!(key = %query.key_expr(), %e, "rpc refusal failed");
    }
}

async fn send(query: &Query, bytes: Vec<u8>) {
    let key = query.key_expr().clone().into_owned();
    if let Err(e) = query
        .reply(key, bytes)
        .encoding(Encoding::APPLICATION_JSON)
        .await
    {
        tracing::warn!(key = %query.key_expr(), %e, "rpc reply failed");
    }
}

#[cfg(test)]
mod tests {
    use crate::bootstrap;
    use crate::caller::{Caller, RpcError};
    use crate::endpoint::manager;
    use crate::error::ErrorCode;
    use crate::keys;

    /// A parked reply answered with JSON of the wrong shape refuses with
    /// `internal` instead of sending what the caller can't decode, and
    /// one of the right shape goes through.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_parked_reply_checks_its_json_against_the_response_type() {
        // Under /tmp, for the same reason as bootstrap's socket test.
        let dir = tempfile::Builder::new()
            .prefix("tv")
            .tempdir_in("/tmp")
            .expect("create a temp dir under /tmp");
        let socket = dir.path().join("hub.sock");
        let server = zenoh::open(bootstrap::peer_config(&socket).unwrap())
            .await
            .unwrap();
        let queryable = server
            .declare_queryable(keys::manager_rpc_wild("t"))
            .await
            .unwrap();
        tokio::spawn(async move {
            while let Ok(query) = queryable.recv_async().await {
                match manager::Incoming::receive("t", query).await {
                    Some(((), manager::Incoming::Info((), responder))) => {
                        let wrong = serde_json::json!({ "not": "a ServiceInfo" });
                        responder.park().answer(Ok(wrong)).await;
                    }
                    Some(((), manager::Incoming::Reload((), responder))) => {
                        let right = serde_json::json!({ "changed": true });
                        responder.park().answer(Ok(right)).await;
                    }
                    _ => {}
                }
            }
        });

        let client = zenoh::open(bootstrap::client_config(&socket).unwrap())
            .await
            .unwrap();
        let caller = Caller::new(client, "t");
        match caller.call::<manager::Info>(&(), &()).await {
            Err(RpcError::Service(error)) => assert_eq!(error.code, ErrorCode::Internal),
            other => panic!("expected an internal refusal, got {other:?}"),
        }
        assert!(
            caller
                .call::<manager::Reload>(&(), &())
                .await
                .unwrap()
                .changed
        );
    }
}

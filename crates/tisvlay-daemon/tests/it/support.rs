//! Typed rpc helpers shared by the daemon's wire suites.

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tisvlay_protocol::caller::{Caller, RpcError};
use tisvlay_protocol::endpoint::Unary;
use tisvlay_protocol::error::WireError;
use zenoh::Session;
use zenoh::bytes::Encoding;

/// One typed rpc round-trip through the matching-gated caller.
pub(crate) async fn call<R: Unary>(
    caller: &Caller,
    target: &R::Target,
    request: &R::Request,
) -> Result<R::Response, RpcError> {
    caller.call::<R>(target, request).await
}

/// A request from a JSON literal, decoded into the rpc's request type.
pub(crate) fn body<T: DeserializeOwned>(json: Value) -> T {
    serde_json::from_value(json).expect("the literal fits the request type")
}

/// The service's refusal behind a failed call.
pub(crate) fn refusal(error: &RpcError) -> &WireError {
    match error {
        RpcError::Service(refusal) => refusal,
        other => panic!("expected a service refusal, got {other:?}"),
    }
}

/// A raw rpc with a body or verb no typed call can send, for the decode
/// refusals; returns the error payload.
pub(crate) async fn raw_refusal(client: &Session, key: &str, body: Value) -> Value {
    let replies = client
        .get(key.to_string())
        .payload(serde_json::to_vec(&body).unwrap())
        .encoding(Encoding::APPLICATION_JSON)
        .timeout(Duration::from_secs(5))
        .await
        .unwrap();
    let reply = replies.recv_async().await.expect("one reply");
    let error = reply.result().expect_err("the service refuses the request");
    serde_json::from_slice(&error.payload().to_bytes()).unwrap()
}

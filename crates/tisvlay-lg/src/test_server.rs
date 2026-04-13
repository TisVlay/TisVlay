//! Fake SSAP server for testing the LG backend.
//!
//! A lightweight `tokio-tungstenite` WebSocket server that speaks enough
//! SSAP to drive the LG client through its real code paths: connection,
//! registration, commands, and subscriptions. No external mock crate
//! needed — SSAP's stateful, ID-correlated protocol is simpler to
//! implement directly.
//!
//! # Architecture
//!
//! [`FakeSsapServer::start`] binds a TCP listener on `127.0.0.1:0`
//! (OS-assigned port) and spawns a background task that accepts one
//! WebSocket connection at a time. Each incoming message is parsed as
//! an SSAP [`Request`], dispatched by type (register / subscribe /
//! command), and answered with the appropriate [`Response`].
//!
//! Tests drive the server through [`FakeSsapHandle`], which controls
//! registration outcomes and injects subscription pushes.

use crate::protocol::{Request, RequestType, Response, ResponseType};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Registration outcome that the server will produce.
#[derive(Debug, Clone)]
pub(crate) enum RegistrationOutcome {
    /// Two-phase accept: Response (prompt shown) then Registered (key).
    Accept { client_key: String },
    /// Single-phase accept: Registered immediately (returning client).
    AcceptImmediate { client_key: String },
    /// Reject with an error response.
    Reject,
}

impl Default for RegistrationOutcome {
    fn default() -> Self {
        Self::Accept {
            client_key: "test-client-key".to_string(),
        }
    }
}

/// Shared state between the server task and the test handle.
struct ServerState {
    registration: RegistrationOutcome,
    /// Subscription push messages queued by the test, keyed by request ID.
    /// The server task drains these after sending the initial subscribe response.
    push_queue: Vec<(String, Value)>,
}

/// Test control surface for the fake SSAP server.
pub(crate) struct FakeSsapHandle {
    addr: SocketAddr,
    state: Arc<Mutex<ServerState>>,
    /// Channel to push subscription updates into active connections.
    push_tx: mpsc::UnboundedSender<(String, Value)>,
    _server_task: tokio::task::JoinHandle<()>,
}

impl FakeSsapHandle {
    /// The address the server is listening on (`127.0.0.1:{port}`).
    pub(crate) fn addr(&self) -> &SocketAddr {
        &self.addr
    }

    /// The port the server is listening on.
    pub(crate) fn port(&self) -> u16 {
        self.addr.port()
    }

    /// WebSocket URL for the LG client to connect to.
    pub(crate) fn ws_url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Set the registration outcome for the next connection.
    pub(crate) fn set_registration_outcome(&self, outcome: RegistrationOutcome) {
        self.state.lock().unwrap().registration = outcome;
    }

    /// Push a subscription update into the active connection.
    ///
    /// The `request_id` must match the ID the client sent when subscribing.
    /// The `payload` is the SSAP response payload.
    pub(crate) fn push_subscription_update(&self, request_id: &str, payload: Value) {
        let _ = self.push_tx.send((request_id.to_string(), payload));
    }
}

/// The fake SSAP server.
pub(crate) struct FakeSsapServer;

impl FakeSsapServer {
    /// Start a fake SSAP server and return a handle for driving it.
    pub(crate) async fn start() -> FakeSsapHandle {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind to ephemeral port");
        let addr = listener.local_addr().unwrap();

        let state = Arc::new(Mutex::new(ServerState {
            registration: RegistrationOutcome::default(),
            push_queue: Vec::new(),
        }));

        let (push_tx, push_rx) = mpsc::unbounded_channel();
        let server_state = Arc::clone(&state);

        let server_task = tokio::spawn(async move {
            Self::accept_loop(listener, server_state, push_rx).await;
        });

        FakeSsapHandle {
            addr,
            state,
            push_tx,
            _server_task: server_task,
        }
    }

    async fn accept_loop(
        listener: TcpListener,
        state: Arc<Mutex<ServerState>>,
        mut push_rx: mpsc::UnboundedReceiver<(String, Value)>,
    ) {
        // Accept connections in a loop to support reconnection tests.
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };

            let ws = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(_) => continue,
            };

            let (mut sink, mut stream) = ws.split();

            loop {
                tokio::select! {
                    // Handle incoming messages from the client.
                    msg = stream.next() => {
                        let msg = match msg {
                            Some(Ok(Message::Text(text))) => text,
                            Some(Ok(Message::Close(_))) | None => break,
                            Some(Ok(Message::Ping(data))) => {
                                let _ = sink.send(Message::Pong(data)).await;
                                continue;
                            }
                            _ => continue,
                        };

                        let request: Request = match serde_json::from_str(&msg) {
                            Ok(r) => r,
                            Err(_) => continue,
                        };

                        let responses = Self::handle_request(&request, &state);
                        for resp in responses {
                            let json = serde_json::to_string(&resp).unwrap();
                            if sink.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }

                    // Handle push subscription updates from the test.
                    Some((req_id, payload)) = push_rx.recv() => {
                        let resp = json!({
                            "id": req_id,
                            "type": "response",
                            "payload": payload,
                        });
                        let json = serde_json::to_string(&resp).unwrap();
                        if sink.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }

    fn handle_request(request: &Request, state: &Arc<Mutex<ServerState>>) -> Vec<Value> {
        let id = &request.id;

        match request.msg_type {
            RequestType::Register => {
                let outcome = state.lock().unwrap().registration.clone();
                match outcome {
                    RegistrationOutcome::Accept { client_key } => {
                        // Two-phase: Response then Registered.
                        vec![
                            json!({
                                "id": id,
                                "type": "response",
                                "payload": {
                                    "pairingType": "PROMPT",
                                    "returnValue": true
                                }
                            }),
                            json!({
                                "id": id,
                                "type": "registered",
                                "payload": {
                                    "client-key": client_key
                                }
                            }),
                        ]
                    }
                    RegistrationOutcome::AcceptImmediate { client_key } => {
                        // Single-phase: Registered immediately.
                        vec![json!({
                            "id": id,
                            "type": "registered",
                            "payload": {
                                "client-key": client_key
                            }
                        })]
                    }
                    RegistrationOutcome::Reject => {
                        vec![json!({
                            "id": id,
                            "type": "error",
                            "error": "rejected",
                            "payload": {}
                        })]
                    }
                }
            }

            RequestType::Subscribe => {
                // Echo a success response. The subscription is now "active" —
                // future pushes with this request ID will be delivered as
                // subscription updates.
                let uri = request.uri.as_deref().unwrap_or("");
                let payload = Self::initial_subscribe_payload(uri, &request.payload);
                vec![json!({
                    "id": id,
                    "type": "response",
                    "payload": payload,
                })]
            }

            RequestType::Request => {
                // Generic command — echo success with returnValue: true.
                let uri = request.uri.as_deref().unwrap_or("");
                let payload = Self::command_response_payload(uri, &request.payload);
                vec![json!({
                    "id": id,
                    "type": "response",
                    "payload": payload,
                })]
            }
        }
    }

    /// Generate an initial subscribe response payload based on the endpoint.
    fn initial_subscribe_payload(uri: &str, _request_payload: &Option<Value>) -> Value {
        use crate::endpoints::{audio, input, picture, power};

        match uri {
            power::GET_POWER_STATE => json!({
                "returnValue": true,
                "state": "Active",
                "processing": "None"
            }),
            audio::GET_VOLUME => json!({
                "returnValue": true,
                "volumeStatus": { "volume": 30, "muted": false }
            }),
            audio::GET_MUTE => json!({
                "returnValue": true,
                "mute": false
            }),
            input::GET_EXTERNAL_INPUT_LIST => json!({
                "returnValue": true,
                "devices": [
                    { "id": "HDMI_1", "label": "HDMI 1", "connected": true, "appId": "com.webos.app.hdmi1" },
                    { "id": "HDMI_2", "label": "HDMI 2", "connected": true, "appId": "com.webos.app.hdmi2" },
                ]
            }),
            input::GET_CURRENT_INPUT => json!({
                "returnValue": true,
                "appId": "com.webos.app.hdmi1"
            }),
            picture::GET_SYSTEM_SETTINGS => json!({
                "returnValue": true,
                "category": "picture",
                "settings": { "backlight": 80 }
            }),
            _ => json!({ "returnValue": true }),
        }
    }

    /// Generate a command response payload based on the endpoint.
    fn command_response_payload(uri: &str, _request_payload: &Option<Value>) -> Value {
        use crate::endpoints::{audio, input, notification, picture, power};

        match uri {
            power::GET_POWER_STATE => json!({
                "returnValue": true,
                "state": "Active",
                "processing": "None"
            }),
            audio::GET_VOLUME => json!({
                "returnValue": true,
                "volumeStatus": { "volume": 30, "muted": false }
            }),
            audio::GET_MUTE => json!({
                "returnValue": true,
                "mute": false
            }),
            input::GET_EXTERNAL_INPUT_LIST => json!({
                "returnValue": true,
                "devices": [
                    { "id": "HDMI_1", "label": "HDMI 1", "connected": true, "appId": "com.webos.app.hdmi1" },
                    { "id": "HDMI_2", "label": "HDMI 2", "connected": true, "appId": "com.webos.app.hdmi2" },
                ]
            }),
            picture::GET_SYSTEM_SETTINGS => json!({
                "returnValue": true,
                "category": "picture",
                "settings": { "backlight": 80 }
            }),
            notification::CREATE_ALERT => json!({
                "returnValue": true,
                "alertId": "com.webos.service.apiadapter-fake-12345"
            }),
            _ => json!({ "returnValue": true }),
        }
    }
}

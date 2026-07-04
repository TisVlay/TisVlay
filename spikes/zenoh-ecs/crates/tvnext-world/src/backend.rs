//! Backend bridge tasks: each live TV connection is owned by one tokio task
//! (the `&mut self` trait surface makes single ownership natural — no lock).
//! The world talks to it through a command channel; facts flow back through
//! the reactor inbox.

use std::sync::Arc;
use tisvlay_core::TvConnector;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::events::{BackendCmd, BackendEvent, InEvent};

pub(crate) async fn connect_flow(
    tv: Uuid,
    connector: Arc<dyn TvConnector>,
    config: toml::Value,
    inbox: mpsc::UnboundedSender<InEvent>,
) {
    let supports_wake = connector.supports_wake(&config);
    let mut connection = match connector.connect(&config).await {
        Ok(connection) => connection,
        Err(error) => {
            let _ = inbox.send(InEvent::Backend {
                tv,
                event: BackendEvent::ConnectFailed {
                    error: error.to_string(),
                },
            });
            return;
        }
    };
    let state = match connection.get_state().await {
        Ok(state) => state,
        Err(error) => {
            let _ = inbox.send(InEvent::Backend {
                tv,
                event: BackendEvent::ConnectFailed {
                    error: format!("initial state read failed: {error}"),
                },
            });
            return;
        }
    };
    // Push-capable backends stream deltas; poll-only backends (subscribe
    // refused) still work — the connection serves commands, state refreshes
    // only on reconnect.
    let subscription = connection.subscribe_state().await.ok();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let _ = inbox.send(InEvent::Backend {
        tv,
        event: BackendEvent::Connected {
            cmd_tx,
            state: Box::new(state),
            supports_wake,
        },
    });

    let mut subscription = subscription;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                None => {
                    // World dropped the handle (removal / disconnect / despawn):
                    // close and exit without an event — the entity is already gone
                    // or going.
                    debug!(%tv, "command channel closed, shutting connection down");
                    let _ = connection.close().await;
                    return;
                }
                Some(cmd) => {
                    let result = match cmd {
                        BackendCmd::SetVolume(level) => connection.set_volume(level).await,
                        BackendCmd::SetMuted(muted) => connection.set_mute(muted).await,
                        BackendCmd::PowerOn => connection.power_on().await,
                        BackendCmd::PowerOff => connection.power_off().await,
                    };
                    if let Err(error) = result {
                        warn!(%tv, ?cmd, %error, "backend command failed");
                    }
                    if !connection.is_connected() {
                        // Network-style transports drop on power_off.
                        let _ = inbox.send(InEvent::Backend { tv, event: BackendEvent::Disconnected });
                        return;
                    }
                }
            },
            update = recv_update(&mut subscription) => match update {
                Some(state) => {
                    let _ = inbox.send(InEvent::Backend { tv, event: BackendEvent::State(Box::new(state)) });
                }
                None => {
                    let _ = inbox.send(InEvent::Backend { tv, event: BackendEvent::Disconnected });
                    return;
                }
            },
        }
    }
}

/// Receive the next push update, or pend forever for poll-only backends so
/// the select stays command-driven.
async fn recv_update(
    subscription: &mut Option<mpsc::UnboundedReceiver<tisvlay_core::TvState>>,
) -> Option<tisvlay_core::TvState> {
    match subscription {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

//! Backend bridge tasks: each live TV connection is owned by one tokio task
//! (the `&mut self` trait surface makes single ownership natural — no lock).
//! The world talks to it through a command channel; facts flow back through
//! the reactor inbox.

use std::sync::Arc;
use tisvlay_core::{
    PairingError, PairingErrorCode, PairingSession, PairingState, TvConnector, VendorConfig,
};
use tokio::sync::mpsc;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::events::{BackendCmd, BackendEvent, InEvent, PairingUpdate};

/// Ceiling on the pair-time link-MAC read, which sits between a pairing
/// dial's connect and the pairing's `complete`.
const LINK_MAC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

#[expect(
    clippy::too_many_arguments,
    reason = "a spawned task: one dial's inputs (the TV, its connector, config and policy flags) and the inbox it reports to, moved in once by its only caller"
)]
pub(crate) async fn connect_flow(
    tv: Uuid,
    dial: u64,
    pairing: bool,
    connector: Arc<dyn TvConnector>,
    config: VendorConfig,
    wake: bool,
    retry: tisvlay_core::RetryConfig,
    traits: crate::events::TransportInfo,
    inbox: mpsc::UnboundedSender<InEvent>,
) {
    debug!(%tv, wake, "connect_flow started");
    let dialed = if wake {
        let _ = inbox.send(InEvent::Backend {
            tv,
            dial,
            event: BackendEvent::PowerPhase(tisvlay_protocol::payload::PowerPhase::WolSent),
        });
        let _ = inbox.send(InEvent::Backend {
            tv,
            dial,
            event: BackendEvent::PowerPhase(tisvlay_protocol::payload::PowerPhase::Connecting),
        });
        connector.wake_and_connect(&config).await
    } else {
        connector.connect(&config).await
    };
    debug!(%tv, dialed = dialed.is_ok(), "dial finished");
    let mut connection = match dialed {
        Ok(connection) => connection,
        Err(error) => {
            let _ = inbox.send(InEvent::Backend {
                tv,
                dial,
                event: BackendEvent::ConnectFailed {
                    reason: crate::events::DialFailure::from_error(&error),
                },
            });
            return;
        }
    };
    let state = match connection.get_state().await {
        Ok(state) => state,
        Err(error) => {
            let mut reason = crate::events::DialFailure::from_error(&error);
            reason.message = format!("initial state read failed: {}", reason.message);
            let _ = inbox.send(InEvent::Backend {
                tv,
                dial,
                event: BackendEvent::ConnectFailed { reason },
            });
            return;
        }
    };
    let brand = connection.brand();
    let hardware = connection.hardware_id();
    // Pair-time read, bounded: a slow or silent device costs at most the
    // timeout before the pairing completes, never its request budget.
    let link_mac = if pairing {
        tokio::time::timeout(LINK_MAC_TIMEOUT, connection.get_link_mac())
            .await
            .unwrap_or_else(|_| {
                debug!(%tv, "link MAC read timed out");
                None
            })
    } else {
        None
    };
    let capabilities: Vec<String> = connection
        .capabilities()
        .iter_names()
        .map(|(name, _)| name.to_lowercase())
        .collect();
    // Push-capable backends stream deltas; poll-only backends (subscribe
    // refused) still work — the connection serves commands, state refreshes
    // only on reconnect.
    let subscription = connection.subscribe_state().await.ok();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let _ = inbox.send(InEvent::Backend {
        tv,
        dial,
        event: BackendEvent::Connected {
            cmd_tx,
            state: Box::new(state),
            brand,
            hardware,
            capabilities,
            retry,
            link_mac,
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
                    if matches!(cmd, BackendCmd::PowerOff(_) | BackendCmd::ScreenOff) {
                        let _ = inbox.send(InEvent::Backend {
                            tv,
                            dial,
                            event: BackendEvent::SendingOff,
                        });
                    }
                    let (result, reply) = match &cmd {
                        BackendCmd::SetVolume(level, reply) => {
                            (connection.set_volume(*level).await, *reply)
                        }
                        BackendCmd::SetMuted(muted, reply) => {
                            (connection.set_mute(*muted).await, *reply)
                        }
                        BackendCmd::SwitchInput(input, reply) => {
                            (connection.switch_input(input).await, *reply)
                        }
                        BackendCmd::SetBacklight(level, reply) => {
                            (connection.set_backlight(*level).await, *reply)
                        }
                        BackendCmd::PowerOff(reply) => (connection.power_off().await, *reply),
                        BackendCmd::PowerOn => (connection.power_on().await, None),
                        BackendCmd::ScreenOn => (connection.screen_on().await, None),
                        BackendCmd::ScreenOff => (connection.screen_off().await, None),
                        // Everything queued before it has run: dropping the
                        // token (with `cmd`) is the signal.
                        BackendCmd::Drain(_) => continue,
                    };
                    if let Err(error) = &result {
                        warn!(%tv, ?cmd, %error, "backend command failed");
                    }
                    // Deferred rpc release: the caller hears back at device
                    // speed — the backpressure that keeps rapid absolute
                    // writes (volume scroll) from queuing stale values.
                    if let Some(reply) = reply {
                        let _ = inbox.send(InEvent::Backend {
                            tv,
                            dial,
                            event: BackendEvent::CommandDone {
                                reply,
                                result: result.map_err(|error| error.to_string()),
                            },
                        });
                    }
                    // Standby-reachable transports (CEC) have no push for
                    // their own command effects — refresh state so the
                    // world's cache reflects the transition it just caused.
                    if traits.reachable_in_standby
                        && matches!(
                            cmd,
                            BackendCmd::PowerOn
                                | BackendCmd::PowerOff(_)
                                | BackendCmd::ScreenOn
                                | BackendCmd::ScreenOff
                        )
                        && let Ok(state) = connection.get_state().await
                    {
                        let _ = inbox.send(InEvent::Backend {
                            tv,
                            dial,
                            event: BackendEvent::State(Box::new(state)),
                        });
                    }
                    if !connection.is_connected() {
                        // Network-style transports drop on power_off. Drain
                        // state the command itself pushed (the standby
                        // snapshot) so last-known state reflects it before
                        // the link goes down.
                        if let Some(rx) = subscription.as_mut() {
                            while let Ok(state) = rx.try_recv() {
                                let _ = inbox.send(InEvent::Backend {
                                    tv,
                                    dial,
                                    event: BackendEvent::State(Box::new(state)),
                                });
                            }
                        }
                        let _ = inbox.send(InEvent::Backend { tv, dial, event: BackendEvent::Disconnected });
                        return;
                    }
                }
            },
            update = recv_update(&mut subscription) => match update {
                Some(state) => {
                    let _ = inbox.send(InEvent::Backend { tv, dial, event: BackendEvent::State(Box::new(state)) });
                }
                None => {
                    let _ = inbox.send(InEvent::Backend { tv, dial, event: BackendEvent::Disconnected });
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

/// Stream a pairing session's state transitions into the world. Sends the
/// cancel handle first so `CancelPair` can always reach the session, then
/// every state (watch semantics coalesce intermediate transitions) until a
/// terminal one.
pub(crate) async fn pairing_task(
    tv: Uuid,
    session: PairingSession,
    inbox: mpsc::UnboundedSender<InEvent>,
) {
    let _ = inbox.send(InEvent::PairingUpdate {
        id: tv,
        update: PairingUpdate::Handle(session.command_sender()),
    });
    let mut rx = session.subscribe();
    loop {
        let state = rx.borrow_and_update().clone();
        let terminal = state.is_terminal();
        let _ = inbox.send(InEvent::PairingUpdate {
            id: tv,
            update: PairingUpdate::State(Box::new(state)),
        });
        if terminal {
            return;
        }
        if rx.changed().await.is_err() {
            let _ = inbox.send(InEvent::PairingUpdate {
                id: tv,
                update: PairingUpdate::State(Box::new(PairingState::failed(PairingError::new(
                    PairingErrorCode::Unknown,
                    "pairing session closed unexpectedly",
                )))),
            });
            return;
        }
    }
}

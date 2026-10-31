//! A Unix-socket relay between a zenoh client and the service it dials,
//! so a test can break the link the way the OS does while the service
//! keeps running.
//!
//! The client dials [`Relay::front`]; each connection is forwarded to
//! the service's socket. The relay understands only zenoh's stream-link
//! framing: every batch on a stream link carries a little-endian `u16`
//! length prefix, so the relay forwards whole batches and can drop one.
//!
//! - [`Relay::cut`] closes every forwarded connection, as when a link's
//!   lease expires after the PC wakes.
//! - [`Relay::stall`] stops forwarding without closing anything, as when
//!   systemd freezes the client's slice before a sleep: the peer stops
//!   reading, and the other side's writes back up.
//! - [`Relay::drop_batch_containing`] drops the next batch that carries a
//!   marker, losing a sample within a link's lifetime. zenoh accepts a
//!   forward gap in a reliable channel's sequence numbers, so the link
//!   stays up.
//!
//! While stalled or cut, the relay accepts and closes new connections at
//! once instead of leaving them unanswered: a connect that never hears
//! back holds a zenoh session's close until its open timeout.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// What the relay does with its connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Open,
    Stalled,
    Cut,
}

/// The relay's mode, plus how many times it has cut: a connection
/// accepted before a cut ends even if the relay opens again before the
/// connection's pumps see the cut.
#[derive(Debug, Clone, Copy)]
struct State {
    mode: Mode,
    cuts: u64,
}

/// The shared controls of one relay.
#[derive(Debug)]
struct Controls {
    state: watch::Sender<State>,
    /// The marker of the next batch to drop, in either direction.
    drop_marker: Mutex<Option<Vec<u8>>>,
}

/// A running relay. Dropping it stops accepting; forwarded connections
/// end at their next read or write.
#[must_use = "dropping the relay stops it accepting connections"]
#[derive(Debug)]
pub struct Relay {
    front: PathBuf,
    controls: Arc<Controls>,
    accept: JoinHandle<()>,
}

impl Relay {
    /// Listen on `front` and forward each connection to `back`. `back`
    /// need not be listening yet: a connection made while it isn't is
    /// closed, and the client dials again.
    ///
    /// # Panics
    ///
    /// If `front` can't be bound.
    #[expect(
        clippy::panic,
        reason = "test infrastructure: a relay that can't bind fails the test with a clear message"
    )]
    pub fn start(front: &Path, back: &Path) -> Self {
        let listener = UnixListener::bind(front)
            .unwrap_or_else(|e| panic!("bind the relay at {}: {e}", front.display()));
        let (state, _) = watch::channel(State {
            mode: Mode::Open,
            cuts: 0,
        });
        let controls = Arc::new(Controls {
            state,
            drop_marker: Mutex::new(None),
        });
        let accept = tokio::spawn(accept_loop(listener, back.to_path_buf(), controls.clone()));
        Self {
            front: front.to_path_buf(),
            controls,
            accept,
        }
    }

    /// The socket a client dials.
    pub fn front(&self) -> &Path {
        &self.front
    }

    /// Close every forwarded connection and refuse new ones until
    /// [`restore`](Self::restore).
    pub fn cut(&self) {
        self.controls.state.send_modify(|state| {
            state.mode = Mode::Cut;
            state.cuts += 1;
        });
    }

    /// Stop forwarding without closing anything, and refuse new
    /// connections, until [`restore`](Self::restore) or [`cut`](Self::cut).
    pub fn stall(&self) {
        self.controls
            .state
            .send_modify(|state| state.mode = Mode::Stalled);
    }

    /// Forward again: stalled connections resume, and new ones are
    /// accepted.
    pub fn restore(&self) {
        self.controls
            .state
            .send_modify(|state| state.mode = Mode::Open);
    }

    /// Drop the next batch, in either direction, whose bytes contain
    /// `marker`. Payloads travel uncompressed, so a sample's JSON body
    /// makes a good marker.
    pub fn drop_batch_containing(&self, marker: &[u8]) {
        *self
            .controls
            .drop_marker
            .lock()
            .expect("nothing panics while the drop marker is held") = Some(marker.to_vec());
    }

    /// Whether the marker set by
    /// [`drop_batch_containing`](Self::drop_batch_containing) has been
    /// used.
    pub fn dropped(&self) -> bool {
        self.controls
            .drop_marker
            .lock()
            .expect("nothing panics while the drop marker is held")
            .is_none()
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut();
    }
}

async fn accept_loop(listener: UnixListener, back: PathBuf, controls: Arc<Controls>) {
    while let Ok((client, _)) = listener.accept().await {
        let state = *controls.state.borrow();
        if state.mode != Mode::Open {
            drop(client);
            continue;
        }
        let Ok(service) = UnixStream::connect(&back).await else {
            continue;
        };
        let (client_rd, client_wr) = client.into_split();
        let (service_rd, service_wr) = service.into_split();
        tokio::spawn(pump(client_rd, service_wr, controls.clone(), state.cuts));
        tokio::spawn(pump(service_rd, client_wr, controls.clone(), state.cuts));
    }
}

/// Forward batches from `from` to `to` until either side closes or the
/// relay cuts. Returning drops both halves, which closes the connection
/// in this direction; the other pump sees the close at its next read.
async fn pump(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, controls: Arc<Controls>, cuts: u64) {
    let mut state = controls.state.subscribe();
    loop {
        if !wait_open(&mut state, cuts).await {
            return;
        }
        let batch = tokio::select! {
            batch = read_batch(&mut from) => match batch {
                Some(batch) => batch,
                None => return,
            },
            () = wait_cut(&mut state, cuts) => return,
        };
        if take_marker(&controls, &batch) {
            continue;
        }
        if !wait_open(&mut state, cuts).await || to.write_all(&batch).await.is_err() {
            return;
        }
    }
}

/// Wait while the relay is stalled. `false` once this connection is cut.
async fn wait_open(state: &mut watch::Receiver<State>, cuts: u64) -> bool {
    loop {
        let current = *state.borrow_and_update();
        if current.cuts != cuts || current.mode == Mode::Cut {
            return false;
        }
        if current.mode == Mode::Open {
            return true;
        }
        if state.changed().await.is_err() {
            return false;
        }
    }
}

/// Resolve once this connection is cut.
async fn wait_cut(state: &mut watch::Receiver<State>, cuts: u64) {
    let _ = state
        .wait_for(|current| current.cuts != cuts || current.mode == Mode::Cut)
        .await;
}

/// One length-prefixed batch, prefix included. `None` at a close.
async fn read_batch(from: &mut OwnedReadHalf) -> Option<Vec<u8>> {
    let mut len = [0u8; 2];
    from.read_exact(&mut len).await.ok()?;
    let mut batch = vec![0u8; 2 + usize::from(u16::from_le_bytes(len))];
    batch[..2].copy_from_slice(&len);
    from.read_exact(&mut batch[2..]).await.ok()?;
    Some(batch)
}

/// Whether `batch` carries the drop marker, clearing it if so.
fn take_marker(controls: &Controls, batch: &[u8]) -> bool {
    let mut marker = controls
        .drop_marker
        .lock()
        .expect("nothing panics while the drop marker is held");
    let hit = marker.as_deref().is_some_and(|marker| {
        !marker.is_empty() && batch.windows(marker.len()).any(|window| window == marker)
    });
    if hit {
        *marker = None;
    }
    hit
}

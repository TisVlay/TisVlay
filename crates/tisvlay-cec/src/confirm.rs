//! The send-and-poll loop behind a CEC wake, shared by the connector's
//! dial path (`wake_and_connect`) and the connection's in-place
//! `power_on`.
//!
//! Image View On is fire-and-forget: a freshly opened adapter may still be
//! claiming its logical address, and a just-standby TV can ignore the
//! first frame. So the loop re-sends it every [`RESEND_INTERVAL`] (One
//! Touch Play is idempotent for an already-on TV) and polls the power
//! status between sends, early at first and backing off, until the TV
//! reports on or the window ends. It returns either way: the daemon judges
//! the wake from the power state it leaves.

use std::time::Duration;

use linux_cec::PhysicalAddress;
use tisvlay_core::{Error, PowerClass, Result};
use tokio::time::Instant;
use tracing::debug;

use crate::connection::map_power;
use crate::link::{CecLink, CecLinkOpener, Claim};

/// How long the dial path's wake may take to land.
pub(crate) const DIAL_WINDOW: Duration = Duration::from_secs(6);

/// How long an in-place wake may take to land. Shorter than the dial's,
/// since it holds other commands to the TV while it runs.
pub(crate) const IN_PLACE_WINDOW: Duration = Duration::from_secs(4);

/// Delay between Image View On re-sends.
const RESEND_INTERVAL: Duration = Duration::from_millis(1500);

/// Delay before the first power poll; each later one doubles, up to
/// [`RESEND_INTERVAL`]. A TV that takes the first frame answers on soon.
const FIRST_POLL: Duration = Duration::from_millis(300);

/// What a send that fails (other than on a closed link) does next.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OnSendError<'a> {
    /// Re-open the adapter to re-claim its logical address before the next
    /// send, keeping the old link if the re-open fails: the dial path,
    /// which holds no connection state the re-open would lose.
    Reclaim {
        opener: &'a dyn CecLinkOpener,
        device: &'a str,
        physical_address: PhysicalAddress,
        claim: Claim<'a>,
    },
    /// Keep re-sending on the held link: in place, where a re-claim would
    /// drop the claim's RC passthrough behind the connection's back.
    Resend,
}

/// Re-send Image View On and poll power until the TV reports on (`true`)
/// or `window` ends (`false`), sending nothing once it has. A link that
/// closes, on a send or a poll, returns its error at once: a dead adapter
/// won't recover by retrying, and the daemon re-arms the power-on as a
/// wake on it.
pub(crate) async fn wake_until_on(
    link: &mut Box<dyn CecLink>,
    window: Duration,
    on_send_error: OnSendError<'_>,
) -> Result<bool> {
    let end = Instant::now() + window;
    let mut next_send = Instant::now();
    let mut poll_delay = FIRST_POLL;
    loop {
        let now = Instant::now();
        if now >= end {
            return Ok(false);
        }
        if now >= next_send {
            match link.power_on().await {
                Ok(()) => {}
                Err(e @ Error::ConnectionClosed) => return Err(e),
                Err(error) => {
                    debug!(%error, "Image View On failed");
                    if let OnSendError::Reclaim {
                        opener,
                        device,
                        physical_address,
                        claim,
                    } = on_send_error
                        && let Ok(fresh) = opener.open(device, physical_address, claim).await
                    {
                        *link = fresh;
                    }
                }
            }
            next_send = Instant::now() + RESEND_INTERVAL;
        }
        tokio::time::sleep(poll_delay.min(end.saturating_duration_since(Instant::now()))).await;
        poll_delay = (poll_delay * 2).min(RESEND_INTERVAL);
        match link.query_power().await {
            Ok(status) if map_power(status).class() == PowerClass::On => return Ok(true),
            Err(e @ Error::ConnectionClosed) => return Err(e),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use linux_cec::operand::PowerStatus;
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// A TV that never takes the frame, recording when each one was sent;
    /// `closes` makes every poll find the link closed.
    #[derive(Debug, Default)]
    struct Deaf {
        sends: Arc<Mutex<Vec<Instant>>>,
        closes: bool,
    }

    #[async_trait]
    impl CecLink for Deaf {
        async fn query_power(&self) -> Result<PowerStatus> {
            if self.closes {
                Err(Error::ConnectionClosed)
            } else {
                Ok(PowerStatus::Standby)
            }
        }
        async fn power_on(&self) -> Result<()> {
            self.sends.lock().push(Instant::now());
            Ok(())
        }
        async fn power_off(&self) -> Result<()> {
            Ok(())
        }
        async fn set_stream_path(&self, _address: PhysicalAddress) -> Result<()> {
            Ok(())
        }
        async fn query_osd_name(&self) -> Result<String> {
            Ok(String::new())
        }
        async fn query_vendor_id(&self) -> Result<Option<linux_cec::VendorId>> {
            Ok(None)
        }
        fn has_rc(&self) -> bool {
            false
        }
        async fn set_rc_passthrough(&self, _allow: bool) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_sent_once_the_window_ends() {
        let sends = Arc::new(Mutex::new(Vec::new()));
        let mut link: Box<dyn CecLink> = Box::new(Deaf {
            sends: sends.clone(),
            closes: false,
        });
        let t0 = Instant::now();
        let woke = wake_until_on(&mut link, IN_PLACE_WINDOW, OnSendError::Resend)
            .await
            .expect("the link stays open");
        assert!(!woke);
        let sends = sends.lock();
        assert!(sends.len() >= 2, "re-sent while waiting: {}", sends.len());
        assert!(
            sends.iter().all(|sent| *sent < t0 + IN_PLACE_WINDOW),
            "{:?}",
            sends.iter().map(|sent| *sent - t0).collect::<Vec<_>>()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_poll_that_finds_the_link_closed_ends_the_wake() {
        let mut link: Box<dyn CecLink> = Box::new(Deaf {
            closes: true,
            ..Deaf::default()
        });
        let t0 = Instant::now();
        let error = wake_until_on(&mut link, IN_PLACE_WINDOW, OnSendError::Resend)
            .await
            .expect_err("the link closed");
        assert!(matches!(error, Error::ConnectionClosed));
        assert!(t0.elapsed() < RESEND_INTERVAL, "{:?}", t0.elapsed());
    }
}

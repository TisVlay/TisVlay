//! The wake-and-confirm loop behind
//! [`LgConnector::wake_and_connect`](crate::connector::LgConnector): dial
//! until the TV answers, then keep resending Wake-on-LAN and polling its
//! power state until it reports on, or the confirm window ends.
//!
//! An LG in `ActiveStandby` (for a few seconds after a remote-off) still
//! accepts SSAP, so an answering socket doesn't mean the TV woke: the
//! confirm window is what makes the wake land. The loop hands over a live
//! link either way; the world judges the wake from the power state it
//! leaves. It's generic over [`WakeSteps`] so tests drive it under paused
//! time without a socket or a real WoL broadcast.

use std::time::Duration;

use tisvlay_core::{Error, PowerClass, PowerState, Result};
use tokio::time::Instant;
use tracing::debug;

/// How long a wake may take in all, counted from the first WoL send: the
/// daemon doesn't time a wake out, so the loop bounds itself.
const WAKE_DEADLINE: Duration = Duration::from_secs(30);

/// How long a connected TV gets to report on after each connect. It
/// outlasts the remote-off latch, during which the TV ignores a network
/// wake (`docs/lg/firmware-quirks.md`).
const CONFIRM_WINDOW: Duration = Duration::from_secs(8);

/// Delay between connect attempts, and between power-state polls.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Delay between WoL resends while confirming: twice the poll interval,
/// since each packet crosses a broadcast reflector on a routed TV's
/// network.
const RESEND_INTERVAL: Duration = Duration::from_secs(1);

/// One power-state poll's reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reading {
    /// The TV reports on.
    On,
    /// Not on yet: an off, transitional or unreadable state, a TV error,
    /// or no answer in time.
    NotYet,
    /// The link dropped: dial again.
    LinkLost,
}

/// Classify one power-state read, `None` when it didn't answer in time. A
/// waking TV doesn't answer the power query until it's on, and an
/// unexpected label or error mid-wake mustn't end the window early: only
/// an on report or a dead link changes course.
pub(crate) fn classify(read: Option<Result<PowerState>>) -> Reading {
    match read {
        Some(Ok(state)) if state.class() == PowerClass::On => Reading::On,
        Some(Err(Error::ConnectionClosed | Error::Connection(_) | Error::Io(_))) => {
            Reading::LinkLost
        }
        _ => Reading::NotYet,
    }
}

/// The transport steps the loop drives.
pub(crate) trait WakeSteps {
    /// A connected, registered link to the TV.
    type Link: Send;

    /// Make one connect attempt.
    fn connect(&mut self) -> impl Future<Output = Result<Self::Link>> + Send;

    /// Read the TV's power state on `link`, giving up at `until`.
    fn poll(
        &mut self,
        link: &mut Self::Link,
        until: Instant,
    ) -> impl Future<Output = Reading> + Send;

    /// Send one Wake-on-LAN packet, best effort.
    fn resend(&mut self);
}

/// The link a wake ended with, and whether the TV reported on.
#[derive(Debug)]
pub(crate) struct Woke<L> {
    pub(crate) link: L,
    pub(crate) confirmed: bool,
}

/// Dial until the TV answers, then confirm it woke within
/// [`CONFIRM_WINDOW`], resending WoL as it goes, all within
/// [`WAKE_DEADLINE`]. A link that drops is dialed again after a pause, so
/// only a live link is returned. Fails when the deadline passes with no
/// live link: a TV that never answers, or one that keeps dropping it.
pub(crate) async fn wake_and_confirm<S: WakeSteps>(steps: &mut S) -> Result<Woke<S::Link>> {
    let deadline = Instant::now() + WAKE_DEADLINE;
    'dial: loop {
        let mut link = match steps.connect().await {
            Ok(link) => link,
            Err(error) => {
                debug!(%error, "LG TV not ready yet");
                if Instant::now() + POLL_INTERVAL >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(POLL_INTERVAL).await;
                // Earlier packets may have been lost while the desktop's
                // network came back after a resume.
                steps.resend();
                continue 'dial;
            }
        };
        // A connect near the deadline still gets one poll, not a full
        // window past it.
        let now = Instant::now();
        let window_end = (now + CONFIRM_WINDOW).min(deadline.max(now + POLL_INTERVAL));
        let mut next_resend = now + RESEND_INTERVAL;
        loop {
            let tick = (Instant::now() + POLL_INTERVAL).min(window_end);
            match steps.poll(&mut link, tick).await {
                Reading::On => {
                    return Ok(Woke {
                        link,
                        confirmed: true,
                    });
                }
                Reading::LinkLost => {
                    debug!("LG TV dropped the link while waking");
                    if Instant::now() + POLL_INTERVAL >= deadline {
                        return Err(Error::ConnectionClosed);
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                    steps.resend();
                    continue 'dial;
                }
                Reading::NotYet => {}
            }
            let now = Instant::now();
            if now >= window_end {
                return Ok(Woke {
                    link,
                    confirmed: false,
                });
            }
            if now >= next_resend {
                steps.resend();
                next_resend = now + RESEND_INTERVAL;
            }
            tokio::time::sleep_until(tick).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted steps: each connect and poll takes the next scripted
    /// outcome, repeating the last one once the script runs out.
    struct Script {
        connects: VecDeque<bool>,
        polls: VecDeque<Reading>,
        /// A poll that waits out its deadline before answering `NotYet`,
        /// like a TV that doesn't answer in time.
        slow_polls: bool,
        connect_calls: usize,
        poll_calls: usize,
        resends: usize,
    }

    impl Script {
        fn new(connects: &[bool], polls: &[Reading]) -> Self {
            Self {
                connects: connects.iter().copied().collect(),
                polls: polls.iter().copied().collect(),
                slow_polls: false,
                connect_calls: 0,
                poll_calls: 0,
                resends: 0,
            }
        }
    }

    fn next<T: Copy>(script: &mut VecDeque<T>) -> T {
        if script.len() > 1 {
            script
                .pop_front()
                .expect("the script has more than one entry")
        } else {
            *script.front().expect("the script isn't empty")
        }
    }

    impl WakeSteps for Script {
        type Link = usize;

        async fn connect(&mut self) -> Result<usize> {
            self.connect_calls += 1;
            if next(&mut self.connects) {
                Ok(self.connect_calls)
            } else {
                Err(Error::ConnectionClosed)
            }
        }

        async fn poll(&mut self, _link: &mut usize, until: Instant) -> Reading {
            self.poll_calls += 1;
            if self.slow_polls {
                tokio::time::sleep_until(until).await;
                return Reading::NotYet;
            }
            next(&mut self.polls)
        }

        fn resend(&mut self) {
            self.resends += 1;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_tv_already_on_returns_at_once_without_resending() {
        let mut script = Script::new(&[true], &[Reading::On]);
        let t0 = Instant::now();
        let woke = wake_and_confirm(&mut script).await.expect("the TV answers");
        assert!(woke.confirmed);
        assert_eq!(script.resends, 0);
        assert!(t0.elapsed() < POLL_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tv_that_wakes_late_is_polled_until_on_with_wakes_between() {
        let mut script = Script::new(
            &[true],
            &[
                Reading::NotYet,
                Reading::NotYet,
                Reading::NotYet,
                Reading::NotYet,
                Reading::On,
            ],
        );
        let woke = wake_and_confirm(&mut script).await.expect("the TV answers");
        assert!(woke.confirmed);
        assert_eq!(script.poll_calls, 5);
        assert!(
            script.resends >= 1,
            "WoL resent while waiting: {}",
            script.resends
        );
        assert_eq!(script.connect_calls, 1, "the link was held");
    }

    #[tokio::test(start_paused = true)]
    async fn a_tv_that_never_wakes_hands_over_the_link_when_the_window_ends() {
        let mut script = Script::new(&[true], &[Reading::NotYet]);
        let t0 = Instant::now();
        let woke = wake_and_confirm(&mut script).await.expect("the TV answers");
        assert!(!woke.confirmed);
        assert!(t0.elapsed() >= CONFIRM_WINDOW && t0.elapsed() < CONFIRM_WINDOW + POLL_INTERVAL);
        let resends = CONFIRM_WINDOW.as_millis() / RESEND_INTERVAL.as_millis();
        assert!(
            script.resends as u128 >= resends - 1,
            "{} resends",
            script.resends
        );
    }

    #[tokio::test(start_paused = true)]
    async fn polls_that_time_out_keep_the_window_going() {
        let mut script = Script::new(&[true], &[Reading::NotYet]);
        script.slow_polls = true;
        let t0 = Instant::now();
        let woke = wake_and_confirm(&mut script).await.expect("the TV answers");
        assert!(!woke.confirmed);
        assert!(t0.elapsed() >= CONFIRM_WINDOW && t0.elapsed() < CONFIRM_WINDOW + POLL_INTERVAL);
    }

    #[tokio::test(start_paused = true)]
    async fn a_link_lost_while_confirming_is_dialed_again() {
        let mut script = Script::new(&[true], &[Reading::NotYet, Reading::LinkLost, Reading::On]);
        let woke = wake_and_confirm(&mut script).await.expect("the TV answers");
        assert!(woke.confirmed);
        assert_eq!(script.connect_calls, 2);
        assert_eq!(woke.link, 2, "the link handed over is the live one");
    }

    #[tokio::test(start_paused = true)]
    async fn a_tv_that_never_answers_fails_at_the_deadline() {
        let mut script = Script::new(&[false], &[Reading::On]);
        let t0 = Instant::now();
        let error = wake_and_confirm(&mut script)
            .await
            .expect_err("nothing answers");
        assert!(matches!(error, Error::ConnectionClosed));
        assert!(t0.elapsed() <= WAKE_DEADLINE && t0.elapsed() + POLL_INTERVAL >= WAKE_DEADLINE);
        assert!(script.resends >= 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tv_that_keeps_dropping_the_link_fails_at_the_deadline() {
        // Two clients fighting over one key kick each other off: every
        // connect succeeds and every link drops. The deadline still ends it.
        let mut script = Script::new(&[true], &[Reading::LinkLost]);
        let t0 = Instant::now();
        let error = wake_and_confirm(&mut script)
            .await
            .expect_err("no link survives");
        assert!(matches!(error, Error::ConnectionClosed));
        assert!(t0.elapsed() <= WAKE_DEADLINE);
        assert!(
            script.connect_calls < 100,
            "redials pace themselves: {}",
            script.connect_calls
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_near_the_deadline_gets_one_poll_not_a_full_window() {
        let mut script = Script::new(
            &[false; 59]
                .iter()
                .copied()
                .chain([true])
                .collect::<Vec<_>>(),
            &[Reading::NotYet],
        );
        let t0 = Instant::now();
        let woke = wake_and_confirm(&mut script)
            .await
            .expect("the TV answers late");
        assert!(!woke.confirmed);
        assert!(
            t0.elapsed() <= WAKE_DEADLINE + POLL_INTERVAL,
            "{:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn a_reading_ends_the_wait_only_when_on_or_the_link_is_gone() {
        assert_eq!(classify(Some(Ok(PowerState::Active))), Reading::On);
        assert_eq!(classify(Some(Ok(PowerState::ScreenOff))), Reading::On);
        for state in [
            PowerState::ActiveStandby,
            PowerState::Standby,
            PowerState::WarmingUp,
            PowerState::Unknown,
        ] {
            assert_eq!(classify(Some(Ok(state))), Reading::NotYet, "{state:?}");
        }
        assert_eq!(classify(None), Reading::NotYet, "no answer in time");
        assert_eq!(classify(Some(Err(Error::RequestTimeout))), Reading::NotYet);
        assert_eq!(
            classify(Some(Err(Error::TvError("no such service".into())))),
            Reading::NotYet
        );
        assert_eq!(
            classify(Some(Err(Error::ConnectionClosed))),
            Reading::LinkLost
        );
        assert_eq!(
            classify(Some(Err(Error::Io(std::io::Error::other("reset"))))),
            Reading::LinkLost
        );
    }
}

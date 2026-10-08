//! Render deadlines are independent of event-loop wakeups and buffer releases.
//!
//! `next` is a scheduled invalidation, rather than a periodic event-loop timeout.
//! Frame callbacks grant permission to draw but never invalidate static content.
//! The cycle deadline is independent so a throttled surface still expires.
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
pub(super) const ELEMENT_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 30);

#[derive(Default)]
pub(super) struct Pacing {
    next: Option<Instant>,
    last: Option<Instant>,
    pending: bool,
}

impl Pacing {
    pub(super) fn request(&mut self, now: Instant) {
        let earliest = self.last.map_or(now, |last| now.max(last + FRAME_INTERVAL));
        self.next = Some(self.next.map_or(earliest, |next| next.min(earliest)));
    }

    pub(super) fn ready(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|deadline| now >= deadline)
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        if self.pending { None } else { self.next }
    }

    pub(super) fn timeout(&self, now: Instant, end: Instant, configured: bool) -> Duration {
        let deadline = if configured {
            self.deadline().map_or(end, |frame| frame.min(end))
        } else {
            end
        };
        deadline.saturating_duration_since(now)
    }

    pub(super) fn submitted(&mut self, now: Instant, next: Instant) {
        self.last = Some(now);
        self.next = Some(next);
        self.pending = true;
    }

    pub(super) const fn frame_done(&mut self) {
        self.pending = false;
    }

    pub(super) fn retry(&mut self, now: Instant) {
        self.next = Some(now + FRAME_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_wakeups_do_not_advance_a_frame() {
        let now = Instant::now();
        let mut pacing = Pacing::default();
        pacing.request(now);
        assert!(pacing.ready(now));
        pacing.submitted(now, now + Duration::from_secs(3));
        pacing.frame_done();
        for millis in 1..3000 {
            assert!(!pacing.ready(now + Duration::from_millis(millis)));
        }
        assert!(pacing.ready(now + Duration::from_secs(3)));
    }

    #[test]
    fn animation_needs_both_deadline_and_callback() {
        let now = Instant::now();
        let mut pacing = Pacing::default();
        pacing.submitted(now, now + FRAME_INTERVAL);
        assert!(!pacing.ready(now + Duration::from_secs(1)));
        assert_eq!(pacing.deadline(), None);
        pacing.frame_done();
        assert!(!pacing.ready(now));
        assert!(pacing.ready(now + FRAME_INTERVAL));
    }

    #[test]
    fn repeated_sends_are_coalesced_until_the_next_frame() {
        let now = Instant::now();
        let mut pacing = Pacing::default();
        pacing.submitted(now, now + Duration::from_secs(3));
        pacing.frame_done();
        for millis in 1..10 {
            pacing.request(now + Duration::from_millis(millis));
            assert!(!pacing.ready(now + Duration::from_millis(millis)));
        }
        assert_eq!(pacing.deadline(), Some(now + FRAME_INTERVAL));
        assert!(pacing.ready(now + FRAME_INTERVAL));
    }

    #[test]
    fn failed_draws_retry_without_spinning() {
        let now = Instant::now();
        let mut pacing = Pacing::default();
        pacing.request(now);
        pacing.retry(now);
        assert!(!pacing.ready(now));
        assert!(pacing.ready(now + FRAME_INTERVAL));
    }
    #[test]
    fn expiry_stays_live_without_callbacks_or_configuration() {
        let now = Instant::now();
        let end = now + Duration::from_secs(3);
        let mut pacing = Pacing::default();
        pacing.submitted(now, now + FRAME_INTERVAL);
        for configured in [true, false] {
            assert_eq!(pacing.timeout(now, end, configured), Duration::from_secs(3));
            assert_eq!(pacing.timeout(end, end, configured), Duration::ZERO);
        }
    }

    #[test]
    fn reconfiguration_requests_one_frame_after_callback() {
        let now = Instant::now();
        let mut pacing = Pacing::default();
        pacing.submitted(now, now + Duration::from_secs(3));
        let configured = now + Duration::from_millis(100);
        pacing.request(configured);
        assert!(!pacing.ready(configured));
        pacing.frame_done();
        assert!(pacing.ready(configured));
    }
}

//! Backpressure policy: what the shell does when a dockapp stops
//! keeping up, in either direction.
//!
//! Both halves exist to answer the same question — *what gets dropped?*
//! — because the one answer that is not available is "the shell waits".
//! A dockapp is an untrusted process on the other end of a socket. It
//! can stop calling `recv()` by crashing, by deadlocking, by being
//! `SIGSTOP`ped, or on purpose. If the shell's response to any of those
//! is a blocking `write()`, the desktop stops drawing, stops reading
//! input, and stops collecting page-flip completions — the exact
//! failure this whole architecture was written after (see the workspace
//! `clippy.toml`).
//!
//! Outbound (shell -> dockapp), [`SendQueue`]: a bounded queue that
//! drops its *oldest* entry when full, and asks to be disconnected if
//! it stays full. Dropping the oldest is right for this traffic: every
//! message here is either a pointer event (a stale one is worse than
//! useless) or a state update whose newest value supersedes the rest.
//!
//! Inbound (dockapp -> shell), [`FrameLimiter`]: a token bucket that
//! *coalesces* rather than queues. A dockapp pushing 1000 frames per
//! second is asking the compositor to do 1000 blits per second; it gets
//! 30, and the 970 it does not get are the ones it already overwrote.
//! Queuing them would mean spending the compositor's repaint budget
//! drawing frames that were obsolete before they were read.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

/// How many outbound messages may be in flight to one dockapp.
///
/// Sized against what a *human* can generate: pointer events arrive at
/// most a few dozen per second and the shell's event loop drains this
/// queue on every pass (16 ms), so a healthy dockapp never sees more
/// than one or two entries here. 64 is therefore already deep into
/// "this peer is not reading" territory, while being small enough that
/// a hundred wedged dockapps cost kilobytes, not megabytes.
pub const SEND_QUEUE_CAPACITY: usize = 64;

/// How long the queue may stay full before the dockapp is declared
/// unreachable.
///
/// Two seconds because the shell also pings every 2 s and calls a tile
/// hung after three unanswered pings: a peer that is merely slow gets
/// noticed by the liveness path with its gentler, user-visible
/// treatment, and this harsher path only fires for one that is
/// genuinely not draining bytes. Not zero, because a momentary full
/// queue is normal during a burst; not a minute, because until it fires
/// every event for this tile is being thrown away.
pub const SUSTAINED_OVERFLOW: Duration = Duration::from_secs(2);

/// Frames per second the shell will accept from one dockapp.
///
/// Above a display's refresh rate the extra frames cannot be seen, and
/// a dock tile is a 56-pixel square showing a number. Instruments
/// update at 1 Hz; 30 is chosen as "generously more than anything
/// reasonable" rather than as a target.
pub const DEFAULT_FRAME_RATE_HZ: f64 = 30.0;

/// What [`SendQueue::push`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// Accepted with room to spare.
    Queued,
    /// Accepted, but the oldest queued message was thrown away to make
    /// room. Worth a `debug` log, not a `warn`: one of these during a
    /// burst is the policy working.
    DroppedOldest,
    /// The queue has been full for [`SUSTAINED_OVERFLOW`]. The caller
    /// should send `Goodbye { Overflow }` best-effort and close the
    /// connection. Note the message was still queued — the disconnect
    /// is the caller's decision to act on, not something this type does
    /// behind its back.
    Disconnect,
}

/// A bounded outbound queue for one dockapp connection.
///
/// The queue exists because sends are non-blocking: `send()` returns
/// `EAGAIN` the moment the peer's socket buffer is full, and something
/// has to hold the message until the fd is writable again. That
/// something must be bounded, or "peer stopped reading" becomes
/// "compositor allocates until the OOM killer picks a winner" — a
/// slower version of the same freeze.
#[derive(Debug)]
pub struct SendQueue {
    queue: VecDeque<Vec<u8>>,
    capacity: usize,
    /// When the queue first became full and *stayed* full. Cleared the
    /// moment it drains completely, so a peer that catches its breath
    /// resets its own clock.
    overflow_since: Option<Instant>,
    dropped: u64,
}

impl SendQueue {
    pub fn new() -> Self {
        Self::with_capacity(SEND_QUEUE_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self { queue: VecDeque::new(), capacity: capacity.max(1), overflow_since: None, dropped: 0 }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Total messages thrown away over this connection's life. Reported
    /// once at disconnect rather than logged per drop: a wedged peer
    /// would otherwise fill the journal at the queue's own rate.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn push(&mut self, message: Vec<u8>, now: Instant) -> SendOutcome {
        if self.queue.len() < self.capacity {
            self.queue.push_back(message);
            self.overflow_since = None;
            return SendOutcome::Queued;
        }
        self.queue.pop_front();
        self.queue.push_back(message);
        self.dropped += 1;
        let since = *self.overflow_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= SUSTAINED_OVERFLOW {
            SendOutcome::Disconnect
        } else {
            SendOutcome::DroppedOldest
        }
    }

    /// Sends as much as the socket will take, stopping cleanly at the
    /// first `WouldBlock`.
    ///
    /// `send` is expected to be a non-blocking, `MSG_NOSIGNAL`
    /// datagram send — see [`crate::transport::Seqpacket::send`], which
    /// is why the front message is only removed once it has actually
    /// left: a partially-attempted `EAGAIN` datagram was not sent at
    /// all, and re-sending it later is correct precisely because
    /// SEQPACKET is all-or-nothing per message.
    ///
    /// Returns how many messages left. A non-`WouldBlock` error is
    /// propagated: the caller closes the connection.
    pub fn flush(&mut self, mut send: impl FnMut(&[u8]) -> io::Result<usize>) -> io::Result<usize> {
        let mut sent = 0usize;
        while let Some(front) = self.queue.front() {
            match send(front) {
                Ok(_) => {
                    self.queue.pop_front();
                    sent += 1;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        if self.queue.is_empty() {
            self.overflow_since = None;
        }
        Ok(sent)
    }
}

impl Default for SendQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// A token bucket that holds at most one item: the newest.
///
/// Generic over the item so the shell can hold a decoded frame and a
/// test can hold a counter. `T` is whatever "one update" means to the
/// caller; the invariant is that a newer `T` completely supersedes an
/// older one, which is true of tile pixels and would not be true of,
/// say, log lines — do not reuse this for those.
#[derive(Debug)]
pub struct FrameLimiter<T> {
    rate_per_sec: f64,
    burst: f64,
    tokens: f64,
    last_refill: Instant,
    pending: Option<T>,
    coalesced: u64,
}

impl<T> FrameLimiter<T> {
    pub fn new(now: Instant) -> Self {
        Self::with_rate(DEFAULT_FRAME_RATE_HZ, now)
    }

    pub fn with_rate(rate_per_sec: f64, now: Instant) -> Self {
        let rate = if rate_per_sec.is_finite() && rate_per_sec > 0.0 { rate_per_sec } else { DEFAULT_FRAME_RATE_HZ };
        Self {
            rate_per_sec: rate,
            burst: rate,
            // Starts full so the very first frame after a handshake is
            // delivered immediately. A dockapp's first frame is the
            // difference between a tile and a dead-tile placeholder;
            // making the user wait 33 ms for it to earn a token would
            // be a rate limit applied to the one frame that is never
            // spam.
            tokens: rate,
            last_refill: now,
            pending: None,
            coalesced: 0,
        }
    }

    /// Frames superseded before they were ever processed. A high count
    /// is the signal that this dockapp should be moved to the v2
    /// shared-memory transport, or told to slow down.
    pub fn coalesced(&self) -> u64 {
        self.coalesced
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Offers a freshly-received frame. Returns it if there is budget
    /// to process it now, otherwise parks it — replacing (not queuing
    /// behind) any frame already parked.
    pub fn offer(&mut self, item: T, now: Instant) -> Option<T> {
        if self.pending.replace(item).is_some() {
            self.coalesced += 1;
        }
        self.take_ready(now)
    }

    /// Picks up a parked frame once its token has refilled. The event
    /// loop calls this on the pass after [`Self::next_ready_in`]'s
    /// deadline; nothing is lost if it is called late, only delayed.
    pub fn take_ready(&mut self, now: Instant) -> Option<T> {
        self.refill(now);
        if self.pending.is_some() && self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return self.pending.take();
        }
        None
    }

    /// How long until a parked frame can be processed, for the event
    /// loop's poll timeout. `None` when nothing is parked.
    pub fn next_ready_in(&self, now: Instant) -> Option<Duration> {
        self.pending.as_ref()?;
        let tokens = self.tokens_at(now);
        if tokens >= 1.0 {
            return Some(Duration::ZERO);
        }
        Some(Duration::from_secs_f64((1.0 - tokens) / self.rate_per_sec))
    }

    fn tokens_at(&self, now: Instant) -> f64 {
        let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
        (self.tokens + elapsed * self.rate_per_sec).min(self.burst)
    }

    fn refill(&mut self, now: Instant) {
        self.tokens = self.tokens_at(now);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(tag: u8) -> Vec<u8> {
        vec![tag; 8]
    }

    fn would_block() -> io::Error {
        io::Error::from(io::ErrorKind::WouldBlock)
    }

    #[test]
    fn a_healthy_peer_never_loses_a_message() {
        let now = Instant::now();
        let mut queue = SendQueue::with_capacity(4);
        for tag in 0..4 {
            assert_eq!(queue.push(message(tag), now), SendOutcome::Queued);
        }
        let mut seen = Vec::new();
        let sent = queue
            .flush(|bytes| {
                seen.push(bytes[0]);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(sent, 4);
        assert_eq!(seen, vec![0, 1, 2, 3], "order is preserved");
        assert_eq!(queue.dropped(), 0);
        assert!(queue.is_empty());
    }

    #[test]
    fn overflow_drops_the_oldest_message_not_the_newest() {
        // The newest event is the one the user just made. Dropping it
        // to preserve a stale one would be exactly backwards.
        let now = Instant::now();
        let mut queue = SendQueue::with_capacity(2);
        queue.push(message(1), now);
        queue.push(message(2), now);
        assert_eq!(queue.push(message(3), now), SendOutcome::DroppedOldest);
        let mut seen = Vec::new();
        queue
            .flush(|bytes| {
                seen.push(bytes[0]);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(seen, vec![2, 3]);
        assert_eq!(queue.dropped(), 1);
    }

    #[test]
    fn a_full_queue_asks_for_a_disconnect_only_after_it_stays_full() {
        let start = Instant::now();
        let mut queue = SendQueue::with_capacity(1);
        assert_eq!(queue.push(message(0), start), SendOutcome::Queued);
        assert_eq!(queue.push(message(1), start), SendOutcome::DroppedOldest, "one drop is a burst");
        assert_eq!(
            queue.push(message(2), start + SUSTAINED_OVERFLOW - Duration::from_millis(1)),
            SendOutcome::DroppedOldest,
            "still inside the grace window"
        );
        assert_eq!(
            queue.push(message(3), start + SUSTAINED_OVERFLOW),
            SendOutcome::Disconnect,
            "full for the whole window: this peer is not reading"
        );
    }

    #[test]
    fn a_peer_that_catches_up_resets_the_overflow_clock() {
        let start = Instant::now();
        let mut queue = SendQueue::with_capacity(1);
        queue.push(message(0), start);
        queue.push(message(1), start);
        queue.flush(|b| Ok(b.len())).unwrap();
        // Draining emptied it, so the next full stretch starts over
        // rather than inheriting the earlier one's age.
        queue.push(message(2), start + Duration::from_secs(10));
        assert_eq!(queue.push(message(3), start + Duration::from_secs(10)), SendOutcome::DroppedOldest);
    }

    #[test]
    fn a_wouldblock_leaves_the_message_at_the_front_to_be_retried() {
        // SEQPACKET sends are all-or-nothing, so an EAGAIN means the
        // datagram was not sent at all and re-sending it is correct.
        let now = Instant::now();
        let mut queue = SendQueue::with_capacity(4);
        queue.push(message(1), now);
        queue.push(message(2), now);
        let sent = queue.flush(|_| Err(would_block())).unwrap();
        assert_eq!(sent, 0);
        assert_eq!(queue.len(), 2, "nothing was consumed");

        let mut seen = Vec::new();
        queue
            .flush(|bytes| {
                seen.push(bytes[0]);
                Ok(bytes.len())
            })
            .unwrap();
        assert_eq!(seen, vec![1, 2], "the same messages, in the same order");
    }

    #[test]
    fn flush_stops_at_the_first_wouldblock_and_keeps_the_rest() {
        let now = Instant::now();
        let mut queue = SendQueue::with_capacity(4);
        for tag in 0..4 {
            queue.push(message(tag), now);
        }
        let mut budget = 2;
        let sent = queue
            .flush(|_| {
                if budget == 0 {
                    return Err(would_block());
                }
                budget -= 1;
                Ok(8)
            })
            .unwrap();
        assert_eq!(sent, 2);
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn a_real_send_error_is_propagated_so_the_caller_can_close() {
        let now = Instant::now();
        let mut queue = SendQueue::with_capacity(2);
        queue.push(message(0), now);
        let err = queue.flush(|_| Err(io::Error::from(io::ErrorKind::BrokenPipe))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn the_first_frame_after_a_handshake_is_never_rate_limited() {
        let now = Instant::now();
        let mut limiter = FrameLimiter::new(now);
        assert_eq!(limiter.offer(1u32, now), Some(1), "a dead tile until 33ms have passed is worse than the limit");
    }

    #[test]
    fn a_flood_is_coalesced_to_the_newest_frame_not_queued() {
        let start = Instant::now();
        let mut limiter = FrameLimiter::with_rate(30.0, start);
        // Drain the burst so the bucket is genuinely empty.
        for tag in 0..30u32 {
            assert!(limiter.offer(tag, start).is_some());
        }
        for tag in 100..1000u32 {
            assert_eq!(limiter.offer(tag, start), None, "no budget, so nothing is processed");
        }
        assert_eq!(limiter.coalesced(), 899, "899 frames were superseded before anyone looked at them");

        // One token's worth of time later, exactly one frame comes out,
        // and it is the newest one — not the oldest of a backlog.
        let later = start + Duration::from_millis(34);
        assert_eq!(limiter.take_ready(later), Some(999));
        assert_eq!(limiter.take_ready(later), None, "nothing is left behind it");
    }

    #[test]
    fn the_long_run_rate_is_the_configured_rate() {
        let start = Instant::now();
        let mut limiter = FrameLimiter::with_rate(30.0, start);
        for tag in 0..30u32 {
            limiter.offer(tag, start);
        }
        // Offer one frame every millisecond for a second and count how
        // many actually get through.
        let mut delivered = 0;
        for ms in 0..1000u64 {
            if limiter.offer(ms as u32, start + Duration::from_millis(ms)).is_some() {
                delivered += 1;
            }
        }
        assert!((29..=31).contains(&delivered), "about 30 frames in a second, got {delivered}");
    }

    #[test]
    fn tokens_do_not_accumulate_without_bound_while_idle() {
        // Otherwise a tile that sat still for an hour could then blit
        // 108000 frames back-to-back, which is the flood the limiter
        // exists to prevent, merely deferred.
        let start = Instant::now();
        let mut limiter = FrameLimiter::with_rate(30.0, start);
        let hour_later = start + Duration::from_secs(3600);
        let mut delivered = 0;
        for tag in 0..1000u32 {
            if limiter.offer(tag, hour_later).is_some() {
                delivered += 1;
            }
        }
        assert_eq!(delivered, 30, "the bucket is capped at one second's worth");
    }

    #[test]
    fn next_ready_in_tells_the_event_loop_when_to_come_back() {
        let start = Instant::now();
        let mut limiter = FrameLimiter::with_rate(30.0, start);
        assert_eq!(limiter.next_ready_in(start), None, "nothing parked");
        for tag in 0..30u32 {
            limiter.offer(tag, start);
        }
        assert_eq!(limiter.offer(99u32, start), None);
        let wait = limiter.next_ready_in(start).expect("a frame is parked");
        assert!(wait > Duration::ZERO && wait <= Duration::from_millis(34), "waited {wait:?}");
        // A millisecond past the computed deadline, not exactly on it:
        // `Duration::from_secs_f64` rounds to whole nanoseconds, so the
        // round trip through it can land a fraction of a token short.
        assert_eq!(limiter.next_ready_in(start + wait + Duration::from_millis(1)), Some(Duration::ZERO));
    }

    #[test]
    fn a_nonsense_rate_falls_back_to_the_default_instead_of_dividing_by_zero() {
        let now = Instant::now();
        for rate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut limiter = FrameLimiter::<u32>::with_rate(rate, now);
            assert!(limiter.offer(1, now).is_some());
            assert!(limiter.next_ready_in(now).is_none());
        }
    }
}

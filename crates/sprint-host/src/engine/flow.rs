//! The protocol core of the engine: line numbering, the resend ring, the
//! in-flight window, and recovery from resend requests and lost `ok`s. Pure
//! state, no I/O or threads, so it is tested against a model firmware.
//!
//! # Model
//!
//! Every transmitted command (except `M112`) produces exactly one `ok`
//! when the firmware has processed it, in transmission order. A rejected
//! numbered line produces `Error:...`, `Resend: <n>` and an `ok` instead;
//! that `ok` is the line right after the `Resend:` and is swallowed. Marlin
//! also *flushes its receive buffer* when it rejects a line, silently
//! dropping everything that arrived behind it, and rejects every later
//! line that is not `last + 1` with another `Resend` for the same number.
//!
//! The firmware accepts a numbered line only when its number is exactly
//! the last accepted number plus one, so a line is never executed twice
//! and never skipped, whatever the host does; the host's job is to keep
//! the conversation live and cheap:
//!
//! * `Resend: n` rewinds the transmit cursor to `n`: line `n` and every
//!   later line go out again, in order, from the ring of framed lines.
//!   In-flight lines before `n` were accepted and keep waiting for their
//!   `ok`; in-flight lines from `n` on are dropped from the window (they
//!   were rejected or flushed).
//! * Lines that were in flight after `n` ("stale" lines) may still reach
//!   the firmware and each cause another `Resend: n`. Those duplicates are
//!   ignored: always before `n` has been retransmitted, and afterwards
//!   while fewer duplicates than stale lines have been seen (unless the
//!   error says the checksum of the expected line was bad, which a stale
//!   line cannot cause). A line is therefore only sent again because of a
//!   resend request that is not one of these duplicates.
//! * When there were stale lines, retransmission waits until the accepted
//!   lines are acknowledged (the firmware reads its input again once its
//!   queue drains) and either every stale line has been answered or the
//!   link has been quiet for a short settle time (a few round trips),
//!   so the retransmitted lines are not flushed by a late stale rejection.
//! * After a rewind the window shrinks to one command (slow start) until a
//!   run of commands has been acknowledged without errors: errors come in
//!   bursts on noisy links, and with a single command in flight there are
//!   no stale lines to wait for.
//! * `ok N<n>` (ADVANCED_OK, Repetier) acknowledges line `n` and every
//!   older in-flight command, whose `ok`s were evidently lost; an `ok` for
//!   a line no longer in flight is ignored.
//! * When no `ok` arrives for the timeout (busy messages extend it), every
//!   in-flight command is assumed done (its `ok` was lost). The engine then
//!   sends a numbered `M105`: if a line was in fact lost, the firmware
//!   asks for it to be resent.
//! * `M110` resets the line counter. It is a barrier: it is only sent with
//!   nothing in flight, nothing else is sent until its `ok`, and any resend
//!   request meanwhile repeats the `M110` itself.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use sprint_proto::frame_numbered;

/// Framed lines kept for resends.
pub(crate) const RING_CAPACITY: usize = 1000;
/// Line numbers are reset with M110 before they reach this (Marlin keeps
/// them in a signed 32-bit integer).
pub(crate) const LINE_NUMBER_LIMIT: u32 = 1 << 30;
/// Bounds of the settle time before retransmitting after stale lines.
const SETTLE_MIN: Duration = Duration::from_millis(5);
const SETTLE_MAX: Duration = Duration::from_millis(200);
/// Clean acknowledgements needed after a rewind before the full window is
/// used again (at least this many, or four windows).
const SLOW_START_MIN: usize = 16;

/// What an `Error:` line before a `Resend:` complained about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineError {
    /// The expected line arrived corrupted ("checksum mismatch", "No
    /// Checksum with line number").
    Checksum,
    /// A line with an unexpected number arrived ("Line Number is not Last
    /// Line Number+1").
    LineNumber,
    /// Another line-protocol error.
    Other,
}

/// Classify an `Error:` message; `None` when it is not about line framing.
pub(crate) fn classify_line_error(message: &str) -> Option<LineError> {
    let m = message.to_ascii_lowercase();
    if m.contains("line number is not") || m.contains("line number is out of sequence") {
        Some(LineError::LineNumber)
    } else if m.contains("checksum") && !m.contains("no line number") {
        Some(LineError::Checksum)
    } else if m.contains("last line") || m.contains("line number") {
        Some(LineError::Other)
    } else {
        None
    }
}

/// A command awaiting its `ok`.
#[derive(Clone, Debug)]
pub(crate) struct InFlight<M> {
    /// Line number it was sent with.
    pub number: Option<u32>,
    /// When it was (last) transmitted.
    pub sent_at: Instant,
    /// Caller data.
    pub meta: M,
    /// This is the M110 barrier.
    barrier: bool,
}

/// A line to write.
#[derive(Clone, Debug)]
pub(crate) struct Wire<M> {
    /// The text to write (without the line terminator).
    pub text: String,
    /// Its line number.
    pub number: Option<u32>,
    /// Caller data.
    pub meta: M,
}

/// An acknowledged command.
#[derive(Clone, Debug)]
pub(crate) struct Acked<M> {
    /// Caller data.
    pub meta: M,
    /// Time from (last) transmission to its `ok`.
    pub rtt: Duration,
}

/// Result of [`Flow::on_ok`].
#[derive(Debug)]
pub(crate) enum OkOutcome<M> {
    /// These commands are done, oldest first (more than one when older
    /// `ok`s were lost).
    Acked(Vec<Acked<M>>),
    /// The `ok` that follows a resend request.
    Swallowed,
    /// Nothing was waiting for it.
    Spurious,
}

/// Result of [`Flow::on_resend`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResendAction {
    /// The cursor went back: `from` and everything after it is sent again.
    /// `stale` lines that were in flight after it were dropped from the
    /// window.
    Rewind { from: u32, stale: usize },
    /// A duplicate request caused by a stale line; ignored.
    Duplicate,
    /// The M110 barrier is sent again.
    Barrier,
    /// Every line we sent was accepted; nothing to resend.
    NothingToResend,
    /// The firmware expects a line number we never sent: its counter was
    /// changed behind our back. The numbered lines in flight were dropped
    /// (resending them could execute some twice); the counter must be reset
    /// with M110.
    Desync { expected: u32 },
    /// The requested line cannot be resent safely: it is no longer in the
    /// ring, or it was already acknowledged (the firmware lost its state,
    /// e.g. an unnoticed reset). Handled like [`ResendAction::Desync`].
    Unavailable { line: u32 },
}

struct RingEntry<M> {
    number: u32,
    wire: String,
    meta: M,
}

/// Recovery state after a rewind.
#[derive(Clone, Copy, Debug)]
struct Rewind {
    /// The line the firmware asked for.
    line: u32,
    /// `line` has been retransmitted since.
    resent: bool,
    /// Transmissions of later lines that were in flight at the rewind.
    stale: usize,
    /// Duplicate requests seen since.
    stale_seen: usize,
    /// Last rewind / duplicate.
    since: Instant,
}

struct Barrier<M> {
    wire: String,
    number: Option<u32>,
    meta: M,
    resend_needed: bool,
}

/// Flow control state. `M` is caller data carried with every command.
pub(crate) struct Flow<M> {
    window: usize,
    /// Clean acknowledgements still needed before the full window is used
    /// again (0 = full window).
    slow_start: usize,
    /// Next line number to assign.
    next_line: u32,
    /// Next line number to transmit; below `next_line` while rewinding.
    cursor: u32,
    ring: VecDeque<RingEntry<M>>,
    in_flight: VecDeque<InFlight<M>>,
    rewind: Option<Rewind>,
    barrier: Option<Barrier<M>>,
    /// Set by a `Resend:` line: the next line's `ok` is swallowed.
    swallow_next: bool,
    swallow_ok: bool,
    /// Set by an `Error:` line: the kind the next `Resend:` refers to.
    pending_error: Option<LineError>,
    error_for_resend: Option<LineError>,
    last_activity: Instant,
    rtt_avg: Option<Duration>,
    /// Highest line number acknowledged by a real `ok` (since the last
    /// M110).
    acked_upto: Option<u32>,
}

impl<M: Clone> Flow<M> {
    /// Empty flow state allowing `window` commands in flight.
    pub(crate) fn new(window: usize, now: Instant) -> Flow<M> {
        Flow {
            window: window.max(1),
            slow_start: 0,
            next_line: 1,
            cursor: 1,
            ring: VecDeque::new(),
            in_flight: VecDeque::new(),
            rewind: None,
            barrier: None,
            swallow_next: false,
            swallow_ok: false,
            pending_error: None,
            error_for_resend: None,
            last_activity: now,
            rtt_avg: None,
            acked_upto: None,
        }
    }

    /// The next line number to be assigned.
    pub(crate) fn next_line(&self) -> u32 {
        self.next_line
    }

    /// Commands awaiting their `ok`.
    pub(crate) fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Lines waiting to be retransmitted.
    pub(crate) fn backlog_len(&self) -> usize {
        (self.next_line.saturating_sub(self.cursor)) as usize
    }

    /// Nothing in flight, nothing to retransmit, no barrier.
    pub(crate) fn is_idle(&self) -> bool {
        self.in_flight.is_empty() && self.cursor >= self.next_line && self.barrier.is_none()
    }

    /// An M110 is awaiting its `ok`.
    pub(crate) fn barrier_active(&self) -> bool {
        self.barrier.is_some()
    }

    /// The window in force: one command while slow-starting after a rewind.
    pub(crate) fn effective_window(&self) -> usize {
        if self.slow_start > 0 { 1 } else { self.window }
    }

    /// Whether a new command may be sent now.
    pub(crate) fn can_send_new(&self) -> bool {
        self.barrier.is_none()
            && self.cursor >= self.next_line
            && self.in_flight.len() < self.effective_window()
    }

    /// The oldest command awaiting its `ok`.
    pub(crate) fn front(&self) -> Option<&InFlight<M>> {
        self.in_flight.front()
    }

    /// Commands awaiting their `ok`, oldest first.
    pub(crate) fn in_flight(&self) -> impl Iterator<Item = &InFlight<M>> + '_ {
        self.in_flight.iter()
    }

    /// Caller data of the lines waiting to be retransmitted.
    pub(crate) fn backlog(&self) -> impl Iterator<Item = &M> + '_ {
        let base = self.ring.front().map_or(self.cursor, |e| e.number);
        let skip = self.cursor.saturating_sub(base) as usize;
        self.ring.iter().skip(skip).map(|e| &e.meta)
    }

    /// Smoothed send-to-ok time.
    #[cfg(test)]
    pub(crate) fn rtt_avg(&self) -> Option<Duration> {
        self.rtt_avg
    }

    /// Call at the start of every received line (before its replies): ages
    /// the "ok after Resend" and "error before Resend" markers.
    pub(crate) fn begin_line(&mut self) {
        self.swallow_ok = std::mem::take(&mut self.swallow_next);
        self.error_for_resend = self.pending_error.take();
    }

    /// The firmware is alive and working on our commands (`busy:`, heat-wait
    /// reports): extends the `ok` timeout.
    pub(crate) fn note_activity(&mut self, now: Instant) {
        self.last_activity = now;
    }

    fn settle(&self) -> Duration {
        self.rtt_avg
            .map_or(SETTLE_MIN, |r| r.saturating_mul(3))
            .clamp(SETTLE_MIN, SETTLE_MAX)
    }

    fn ring_get(&self, number: u32) -> Option<&RingEntry<M>> {
        let base = self.ring.front()?.number;
        let entry = self.ring.get(number.checked_sub(base)? as usize)?;
        (entry.number == number).then_some(entry)
    }

    fn push_ring(&mut self, number: u32, wire: String, meta: M) {
        let oldest_needed = self
            .in_flight
            .iter()
            .filter_map(|e| e.number)
            .min()
            .unwrap_or(u32::MAX)
            .min(self.cursor);
        while self.ring.len() >= RING_CAPACITY
            && self.ring.front().is_some_and(|e| e.number < oldest_needed)
        {
            self.ring.pop_front();
        }
        self.ring.push_back(RingEntry { number, wire, meta });
    }

    /// When a retransmission held back for the settle time becomes due (the
    /// engine wakes up then instead of waiting for its tick).
    pub(crate) fn retransmit_due_at(&self) -> Option<Instant> {
        let rw = self.rewind.as_ref()?;
        let gated = !rw.resent
            && rw.line == self.cursor
            && rw.stale > rw.stale_seen
            && self.in_flight.is_empty()
            && self.barrier.is_none();
        gated.then(|| rw.since.max(self.last_activity) + self.settle())
    }

    /// The next line to retransmit (or the M110 barrier to repeat), if one
    /// is due now.
    pub(crate) fn poll_retransmit(&mut self, now: Instant) -> Option<Wire<M>> {
        if let Some(b) = &mut self.barrier {
            if !b.resend_needed {
                return None;
            }
            b.resend_needed = false;
            let wire = Wire {
                text: b.wire.clone(),
                number: b.number,
                meta: b.meta.clone(),
            };
            match self.in_flight.iter_mut().find(|e| e.barrier) {
                Some(e) => e.sent_at = now,
                None => self.in_flight.push_back(InFlight {
                    number: b.number,
                    sent_at: now,
                    meta: b.meta.clone(),
                    barrier: true,
                }),
            }
            return Some(wire);
        }
        if self.cursor >= self.next_line || self.in_flight.len() >= self.effective_window() {
            return None;
        }
        if let Some(rw) = &self.rewind
            && !rw.resent
            && rw.line == self.cursor
            && rw.stale > rw.stale_seen
        {
            // Let stale lines drain before sending what they could flush.
            if !self.in_flight.is_empty() {
                return None;
            }
            let quiet = rw.since.max(self.last_activity);
            if now < quiet + self.settle() {
                return None;
            }
        }
        let number = self.cursor;
        let Some(entry) = self.ring_get(number) else {
            // Cannot happen (the ring keeps every line from the cursor on),
            // but never stall on it: skip to the end.
            self.cursor = self.next_line;
            return None;
        };
        let wire = Wire {
            text: entry.wire.clone(),
            number: Some(number),
            meta: entry.meta.clone(),
        };
        self.in_flight.push_back(InFlight {
            number: Some(number),
            sent_at: now,
            meta: wire.meta.clone(),
            barrier: false,
        });
        if let Some(rw) = &mut self.rewind
            && rw.line == number
        {
            rw.resent = true;
        }
        self.cursor += 1;
        Some(wire)
    }

    /// Transmit a new command, framed with the next line number when
    /// `numbered`. Call only when [`Flow::can_send_new`].
    pub(crate) fn send(&mut self, command: &str, numbered: bool, meta: M, now: Instant) -> Wire<M> {
        debug_assert!(self.cursor >= self.next_line, "backlog must be sent first");
        let (text, number) = if numbered {
            let n = self.next_line;
            let text = frame_numbered(n, command);
            self.push_ring(n, text.clone(), meta.clone());
            self.next_line = n.saturating_add(1);
            self.cursor = self.next_line;
            (text, Some(n))
        } else {
            (command.trim().to_string(), None)
        };
        self.in_flight.push_back(InFlight {
            number,
            sent_at: now,
            meta: meta.clone(),
            barrier: false,
        });
        Wire { text, number, meta }
    }

    /// Transmit `M110 N<value>` (line counter reset) as a barrier. Call only
    /// when [`Flow::is_idle`].
    pub(crate) fn send_line_reset(
        &mut self,
        value: u32,
        numbered: bool,
        meta: M,
        now: Instant,
    ) -> Wire<M> {
        let command = format!("M110 N{value}");
        let (text, number) = if numbered {
            (frame_numbered(value, &command), Some(value))
        } else {
            (command, None)
        };
        self.acked_upto = None;
        self.ring.clear();
        self.rewind = None;
        self.next_line = value.saturating_add(1);
        self.cursor = self.next_line;
        self.in_flight.push_back(InFlight {
            number,
            sent_at: now,
            meta: meta.clone(),
            barrier: true,
        });
        self.barrier = Some(Barrier {
            wire: text.clone(),
            number,
            meta: meta.clone(),
            resend_needed: false,
        });
        Wire { text, number, meta }
    }

    /// Repeat the M110 barrier (handshake retries).
    pub(crate) fn repeat_barrier(&mut self, now: Instant) -> Option<Wire<M>> {
        self.barrier.as_mut()?.resend_needed = true;
        self.poll_retransmit(now)
    }

    /// Transmit outside the window and the queue (emergency commands),
    /// unnumbered. `expects_ok` adds it to the in-flight list (every
    /// command but M112 gets an `ok`).
    pub(crate) fn send_immediate(
        &mut self,
        command: &str,
        expects_ok: bool,
        meta: M,
        now: Instant,
    ) -> Wire<M> {
        if expects_ok {
            self.in_flight.push_back(InFlight {
                number: None,
                sent_at: now,
                meta: meta.clone(),
                barrier: false,
            });
        }
        Wire {
            text: command.trim().to_string(),
            number: None,
            meta,
        }
    }

    /// An `ok`, with the line number it carried (if any).
    pub(crate) fn on_ok(&mut self, line: Option<u32>, now: Instant) -> OkOutcome<M> {
        self.last_activity = now;
        if std::mem::take(&mut self.swallow_ok) {
            return OkOutcome::Swallowed;
        }
        let index = match line {
            Some(n) => match self.in_flight.iter().position(|e| e.number == Some(n)) {
                Some(i) => i,
                None => return OkOutcome::Spurious,
            },
            None if self.in_flight.is_empty() => return OkOutcome::Spurious,
            None => 0,
        };
        let mut acked = Vec::with_capacity(index + 1);
        for e in self.in_flight.drain(..=index) {
            let rtt = now.saturating_duration_since(e.sent_at);
            if e.barrier {
                self.barrier = None;
            }
            if let (Some(rw), Some(n)) = (&self.rewind, e.number)
                && rw.line == n
            {
                self.rewind = None;
            }
            if let Some(n) = e.number {
                self.slow_start = self.slow_start.saturating_sub(1);
                self.acked_upto = Some(self.acked_upto.map_or(n, |a| a.max(n)));
            }
            acked.push(Acked { meta: e.meta, rtt });
        }
        if let Some(last) = acked.last() {
            let rtt = last.rtt;
            self.rtt_avg = Some(match self.rtt_avg {
                None => rtt,
                Some(avg) => (avg * 9 + rtt) / 10,
            });
        }
        OkOutcome::Acked(acked)
    }

    /// An `Error:` line about line framing (the `Resend:` follows).
    pub(crate) fn on_error(&mut self, kind: LineError) {
        self.pending_error = Some(kind);
    }

    /// A `Resend: n` request.
    pub(crate) fn on_resend(&mut self, n: u32, now: Instant) -> ResendAction {
        self.last_activity = now;
        self.swallow_next = true;
        let error = self.error_for_resend.take();
        if let Some(b) = &mut self.barrier {
            b.resend_needed = true;
            return ResendAction::Barrier;
        }
        if n > self.next_line {
            self.abandon_numbered();
            return ResendAction::Desync { expected: n };
        }
        let acked = self.acked_upto.is_some_and(|a| n <= a);
        if acked || (n < self.next_line && self.ring_get(n).is_none()) {
            self.abandon_numbered();
            return ResendAction::Unavailable { line: n };
        }
        if let Some(rw) = &mut self.rewind
            && rw.line == n
        {
            let duplicate =
                !rw.resent || (error != Some(LineError::Checksum) && rw.stale_seen < rw.stale);
            if duplicate {
                rw.stale_seen += 1;
                rw.since = now;
                return ResendAction::Duplicate;
            }
        }
        // A genuine request: everything from n on is dead.
        let mut stale = 0;
        self.in_flight.retain(|e| match e.number {
            Some(k) if k >= n => {
                if k > n {
                    stale += 1;
                }
                false
            }
            _ => true,
        });
        // A line number is accepted at most once: keep one entry per number.
        let mut seen = HashSet::new();
        self.in_flight
            .retain(|e| e.number.is_none_or(|k| seen.insert(k)));
        if n < self.cursor {
            self.cursor = n;
        } else if n > self.cursor {
            // The firmware accepted lines we were about to retransmit
            // (their first transmission got through): they will be
            // acknowledged.
            for k in self.cursor..n {
                if let Some(entry) = self.ring_get(k) {
                    let meta = entry.meta.clone();
                    self.in_flight.push_back(InFlight {
                        number: Some(k),
                        sent_at: now,
                        meta,
                        barrier: false,
                    });
                }
            }
            self.cursor = n;
        }
        if n >= self.next_line {
            self.rewind = None;
            return ResendAction::NothingToResend;
        }
        self.rewind = Some(Rewind {
            line: n,
            resent: false,
            stale,
            stale_seen: 0,
            since: now,
        });
        if self.window > 1 {
            self.slow_start = SLOW_START_MIN.max(self.window * 4);
        }
        ResendAction::Rewind { from: n, stale }
    }

    /// Whether the oldest command has waited too long: nothing (no `ok`,
    /// resend or busy) arrived for the longest timeout among the commands
    /// in flight.
    pub(crate) fn timed_out(&self, now: Instant, timeout: impl Fn(&M) -> Duration) -> bool {
        let Some(front) = self.in_flight.front() else {
            return false;
        };
        let limit = self
            .in_flight
            .iter()
            .map(|e| timeout(&e.meta))
            .max()
            .unwrap_or(Duration::MAX);
        let since = front.sent_at.max(self.last_activity);
        now.saturating_duration_since(since) > limit
    }

    /// Timeout recovery: assume every in-flight command is done (its `ok`
    /// was lost) and return them, oldest first. Stale lines have certainly
    /// been processed by now, so later resend requests are genuine.
    pub(crate) fn release_all(&mut self, now: Instant) -> Vec<M> {
        self.barrier = None;
        if let Some(rw) = &mut self.rewind {
            rw.stale_seen = rw.stale;
        }
        self.last_activity = now;
        self.in_flight.drain(..).map(|e| e.meta).collect()
    }

    /// The firmware's line counter can no longer be trusted: forget the
    /// numbered lines in flight and waiting for retransmission (they must
    /// not be resent). The next numbered command must be an M110.
    fn abandon_numbered(&mut self) {
        self.in_flight.retain(|e| e.number.is_none());
        self.cursor = self.next_line;
        self.rewind = None;
    }

    /// Forget everything (board reset, halt). The next numbered command
    /// must be an M110 ([`Flow::send_line_reset`]).
    pub(crate) fn reset(&mut self, now: Instant) {
        self.in_flight.clear();
        self.ring.clear();
        self.acked_upto = None;
        self.cursor = self.next_line;
        self.rewind = None;
        self.barrier = None;
        self.swallow_next = false;
        self.swallow_ok = false;
        self.pending_error = None;
        self.error_for_resend = None;
        self.last_activity = now;
    }
}

#[cfg(test)]
mod tests;

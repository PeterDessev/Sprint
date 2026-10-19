//! The console log: every command sent, every line received and the host's
//! own messages, in a bounded ring with monotonically increasing sequence
//! numbers so UIs can fetch only what is new
//! ([`Printer::console_since`](crate::Printer::console_since)).

use std::collections::VecDeque;

/// One console line.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleEntry {
    /// Sequence number, starting at 1 and increasing by one per entry for
    /// the lifetime of the [`Printer`](crate::Printer) (not reset by
    /// clearing).
    pub seq: u64,
    /// Seconds since the `Printer` was created.
    pub time: f64,
    /// What kind of line this is.
    pub kind: ConsoleKind,
    /// The text: the clean command for sent lines (without line number and
    /// checksum), the raw line for received ones.
    pub text: String,
    /// Automatic traffic (temperature polling/auto-reports and their oks).
    /// UIs hide these by default.
    pub auto: bool,
    /// A line of a streamed job: a sent file, preamble or progress line
    /// (not a pause/resume/cancel script line) or the `ok` that answers
    /// one. UIs may hide these during prints.
    pub stream: bool,
    /// Line number the command was sent with (`N<n>`), for sent lines.
    pub line: Option<u32>,
}

/// Kind of a [`ConsoleEntry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleKind {
    /// A command sent to the printer.
    Sent,
    /// A line received from the printer.
    Received,
    /// Host information.
    Info,
    /// Host warning (resends, timeouts, resets).
    Warning,
    /// Host error.
    Error,
}

/// Bounded console log.
#[derive(Clone, Debug)]
pub struct Console {
    entries: VecDeque<ConsoleEntry>,
    capacity: usize,
    next_seq: u64,
}

impl Console {
    /// An empty console keeping at most `capacity` entries (at least 1).
    pub fn new(capacity: usize) -> Console {
        let capacity = capacity.max(1);
        Console {
            entries: VecDeque::with_capacity(capacity.min(8192)),
            capacity,
            next_seq: 1,
        }
    }

    /// Append an entry, dropping the oldest when full, and return a copy.
    pub fn push(
        &mut self,
        time: f64,
        kind: ConsoleKind,
        text: impl Into<String>,
        auto: bool,
        line: Option<u32>,
    ) -> ConsoleEntry {
        self.push_entry(time, kind, text.into(), auto, false, line)
    }

    /// [`Console::push`] for a line of a streamed job
    /// ([`ConsoleEntry::stream`]).
    pub fn push_stream(
        &mut self,
        time: f64,
        kind: ConsoleKind,
        text: impl Into<String>,
        line: Option<u32>,
    ) -> ConsoleEntry {
        self.push_entry(time, kind, text.into(), false, true, line)
    }

    fn push_entry(
        &mut self,
        time: f64,
        kind: ConsoleKind,
        text: String,
        auto: bool,
        stream: bool,
        line: Option<u32>,
    ) -> ConsoleEntry {
        let entry = ConsoleEntry {
            seq: self.next_seq,
            time,
            kind,
            text,
            auto,
            stream,
            line,
        };
        self.next_seq += 1;
        while self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry.clone());
        entry
    }

    /// Entries with `seq > after`, oldest first, at most `max`.
    pub fn since(&self, after: u64, max: usize) -> Vec<ConsoleEntry> {
        let Some(first) = self.entries.front().map(|e| e.seq) else {
            return Vec::new();
        };
        let skip = after.saturating_add(1).saturating_sub(first);
        let skip = usize::try_from(skip).unwrap_or(usize::MAX);
        self.entries.iter().skip(skip).take(max).cloned().collect()
    }

    /// Remove every entry (sequence numbers keep increasing).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Change the capacity, dropping the oldest entries if needed.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    /// Most entries kept.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Entries currently kept.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// No entries kept.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Sequence number of the newest entry (0 before the first).
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Iterate over the kept entries, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = &ConsoleEntry> + '_ {
        self.entries.iter()
    }
}

impl Default for Console {
    fn default() -> Self {
        Console::new(crate::config::DEFAULT_CONSOLE_LINES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(c: &mut Console, text: &str) -> u64 {
        c.push(0.0, ConsoleKind::Info, text, false, None).seq
    }

    #[test]
    fn sequence_numbers_increase_and_survive_clear() {
        let mut c = Console::new(10);
        assert_eq!(c.last_seq(), 0);
        assert_eq!(push(&mut c, "a"), 1);
        assert_eq!(push(&mut c, "b"), 2);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(push(&mut c, "c"), 3);
        assert_eq!(c.last_seq(), 3);
        assert_eq!(c.since(0, 10).len(), 1);
    }

    #[test]
    fn stream_lines_are_marked() {
        let mut c = Console::new(10);
        let plain = c.push(0.0, ConsoleKind::Sent, "G28", false, Some(3));
        let stream = c.push_stream(0.0, ConsoleKind::Sent, "G1 X1", Some(4));
        assert!(!plain.stream && stream.stream && !stream.auto);
        assert_eq!((stream.seq, stream.line), (2, Some(4)));
    }

    #[test]
    fn ring_drops_oldest() {
        let mut c = Console::new(3);
        for t in ["a", "b", "c", "d", "e"] {
            push(&mut c, t);
        }
        let texts: Vec<String> = c.since(0, 100).into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["c", "d", "e"]);
        assert_eq!(c.len(), 3);
        c.set_capacity(1);
        assert_eq!(c.since(0, 100)[0].text, "e");
        assert_eq!(Console::new(0).capacity(), 1);
    }

    #[test]
    fn since_is_exclusive_and_bounded() {
        let mut c = Console::new(100);
        for i in 0..10 {
            push(&mut c, &i.to_string());
        }
        let seqs = |v: Vec<ConsoleEntry>| v.into_iter().map(|e| e.seq).collect::<Vec<_>>();
        assert_eq!(seqs(c.since(7, 100)), [8, 9, 10]);
        assert_eq!(seqs(c.since(0, 2)), [1, 2]);
        assert!(c.since(10, 5).is_empty());
        assert!(c.since(u64::MAX, 5).is_empty());
        assert!(c.since(3, 0).is_empty());
        assert!(Console::new(5).since(0, 5).is_empty());
    }
}

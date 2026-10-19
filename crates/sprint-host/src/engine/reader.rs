//! The reader thread: bytes from the link -> lines -> engine.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crossbeam_channel::Sender;

use crate::transport::is_no_data;

/// A line longer than this without a newline is delivered in pieces, so
/// garbage cannot grow memory without bound.
pub(crate) const MAX_LINE: usize = 16 * 1024;

/// Messages from the reader thread.
#[derive(Debug)]
pub(crate) enum ReaderMsg {
    /// A received line (without line terminator, lossy UTF-8).
    Line(String),
    /// The link closed or failed; the reader has stopped.
    Lost(String),
}

/// Splits a byte stream into lines.
#[derive(Default)]
pub(crate) struct LineSplitter {
    pending: Vec<u8>,
}

impl LineSplitter {
    /// Add bytes; complete lines are passed to `out`.
    pub(crate) fn push(&mut self, bytes: &[u8], out: &mut impl FnMut(String)) {
        for chunk in bytes.split_inclusive(|&b| b == b'\n') {
            let complete = chunk.last() == Some(&b'\n');
            let body = if complete {
                &chunk[..chunk.len() - 1]
            } else {
                chunk
            };
            self.pending.extend_from_slice(body);
            while self.pending.len() > MAX_LINE || (self.pending.len() == MAX_LINE && !complete) {
                let rest = self.pending.split_off(MAX_LINE);
                out(self.take());
                self.pending = rest;
            }
            if complete {
                out(self.take());
            }
        }
    }

    fn take(&mut self) -> String {
        let mut end = self.pending.len();
        while end > 0 && self.pending[end - 1] == b'\r' {
            end -= 1;
        }
        let line = String::from_utf8_lossy(&self.pending[..end]).into_owned();
        self.pending.clear();
        line
    }
}

/// Read until `stop` is set, the link closes or the engine goes away.
pub(crate) fn run(
    mut reader: Box<dyn Read + Send>,
    tx: Sender<ReaderMsg>,
    stop: Arc<AtomicBool>,
    bytes_received: Arc<AtomicU64>,
) {
    let mut buf = [0u8; 4096];
    let mut splitter = LineSplitter::default();
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(ReaderMsg::Lost("the connection was closed".to_string()));
                return;
            }
            Ok(n) => {
                bytes_received.fetch_add(n as u64, Ordering::Relaxed);
                let mut alive = true;
                splitter.push(&buf[..n], &mut |line| {
                    if alive && tx.send(ReaderMsg::Line(line)).is_err() {
                        alive = false;
                    }
                });
                if !alive {
                    return;
                }
            }
            Err(e) if is_no_data(&e) => {}
            Err(e) => {
                let _ = tx.send(ReaderMsg::Lost(e.to_string()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<String> {
        let mut s = LineSplitter::default();
        let mut out = Vec::new();
        for c in chunks {
            s.push(c, &mut |l| out.push(l));
        }
        out
    }

    #[test]
    fn splits_lines_across_chunks() {
        assert_eq!(
            split(&[b"ok\nstart\r\nT:2", b"0.0 /0\n", b"\n", b"x\r\r\n"]),
            ["ok", "start", "T:20.0 /0", "", "x"]
        );
        assert!(split(&[b"no newline"]).is_empty());
    }

    #[test]
    fn invalid_utf8_and_long_lines() {
        assert_eq!(split(&[b"caf\xe9\n"]), ["caf\u{FFFD}"]);
        let long = vec![b'a'; MAX_LINE * 2 + 10];
        let lines = split(&[&long, b"\n"]);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), MAX_LINE);
        assert_eq!(lines[2].len(), 10);
    }
}

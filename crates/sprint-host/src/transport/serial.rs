//! Serial port links and baud rate auto-detection.
//!
//! **The functions that open ports talk to real hardware and are never run
//! by the test suite.** Auto-detection is generic over a link factory so
//! its logic is tested with in-memory fakes.

use std::io::{self, Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use super::link::{Link, LinkControl, READ_TIMEOUT, is_no_data};
use crate::error::HostError;

/// How long auto-detection waits for a reply at each baud rate.
pub(crate) const AUTO_BAUD_WAIT: Duration = Duration::from_millis(2500);
/// DTR low time when resetting a board.
const DTR_PULSE: Duration = Duration::from_millis(100);
/// The probe sent at each baud rate.
const PROBE: &[u8] = b"M110 N0\nM105\n";
/// How long a write may keep timing out (the port timeout also applies to
/// writes) before the link counts as broken.
const WRITE_PATIENCE: Duration = Duration::from_secs(5);

/// Open `port` at `baud`, 8N1, no flow control, ~100 ms read timeout;
/// optionally pulse DTR to reset the board.
pub(crate) fn open(port: &str, baud: u32, reset: bool) -> Result<Link, HostError> {
    let mut writer = serialport::new(port, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(READ_TIMEOUT)
        .open()?;
    if reset {
        pulse_dtr(writer.as_mut())?;
    }
    // Drop whatever a previous session left in the buffers.
    let _ = writer.clear(ClearBuffer::All);
    let reader = writer.try_clone()?;
    Ok(Link {
        reader: Box::new(reader),
        writer: Box::new(RetryWriter::new(writer, WRITE_PATIENCE)),
        control: LinkControl::None,
    })
}

/// A writer that retries writes that time out (a serial port's timeout
/// also applies to writes, and a large batch at a low baud rate can take
/// longer than it) until `patience` has passed without progress.
pub(crate) struct RetryWriter<W> {
    inner: W,
    patience: Duration,
}

impl<W: Write> RetryWriter<W> {
    pub(crate) fn new(inner: W, patience: Duration) -> RetryWriter<W> {
        RetryWriter { inner, patience }
    }
}

impl<W: Write> Write for RetryWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let start = Instant::now();
        loop {
            match self.inner.write(buf) {
                Err(e) if is_no_data(&e) && start.elapsed() < self.patience => {}
                other => return other,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let start = Instant::now();
        loop {
            match self.inner.flush() {
                Err(e) if is_no_data(&e) && start.elapsed() < self.patience => {}
                other => return other,
            }
        }
    }
}

/// Reset the board: DTR low for a moment, then high again.
fn pulse_dtr(port: &mut dyn SerialPort) -> Result<(), HostError> {
    port.write_data_terminal_ready(false)?;
    thread::sleep(DTR_PULSE);
    port.write_data_terminal_ready(true)?;
    Ok(())
}

/// Whether a received line looks like it came from a printer at the right
/// baud rate (garbage at a wrong rate almost never does).
pub(crate) fn looks_valid(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() || line.chars().any(|c| c.is_control() || c == '\u{FFFD}') {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    lower == "ok"
        || lower.starts_with("ok ")
        || lower == "start"
        || lower.starts_with("echo:")
        || line.contains("T:")
        || line.contains("Marlin")
}

/// Try each rate in `rates`: open a link with `open`, send `M110 N0` and
/// `M105`, and wait up to `wait` for a [valid-looking](looks_valid) reply
/// line. Returns the first rate that answered with its link (the other
/// links are closed). Attempts are reported through `log`.
///
/// An error from `open` aborts the search (the port itself is unusable).
pub(crate) fn detect_baud<F>(
    rates: &[u32],
    mut open: F,
    wait: Duration,
    log: &mut dyn FnMut(&str),
) -> Result<(u32, Link), HostError>
where
    F: FnMut(u32) -> Result<Link, HostError>,
{
    for &baud in rates {
        log(&format!("Auto-baud: trying {baud}"));
        let mut link = open(baud)?;
        if let Err(e) = link
            .writer
            .write_all(PROBE)
            .and_then(|()| link.writer.flush())
        {
            log(&format!("Auto-baud: {baud}: write failed: {e}"));
            continue;
        }
        match wait_for_reply(&mut link, wait) {
            Some(line) => {
                log(&format!("Auto-baud: {baud} answered \"{line}\""));
                return Ok((baud, link));
            }
            None => log(&format!("Auto-baud: no valid reply at {baud}")),
        }
    }
    Err(HostError::Timeout(
        "auto-baud: the printer did not answer at any baud rate".to_string(),
    ))
}

/// Read lines until one looks valid or `wait` passes.
fn wait_for_reply(link: &mut Link, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        match link.reader.read(&mut buf) {
            Ok(0) => return None,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(pos) = pending.iter().position(|&b| b == b'\n') {
                    let raw: Vec<u8> = pending.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&raw[..raw.len() - 1])
                        .trim_end_matches('\r')
                        .to_string();
                    if looks_valid(&line) {
                        return Some(line);
                    }
                }
                // Garbage without newlines must not grow without bound.
                if pending.len() > 4096 {
                    pending.clear();
                }
            }
            Err(e) if is_no_data(&e) => {}
            Err(_) => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Read};
    use std::sync::{Arc, Mutex};

    /// A fake port that answers `reply` once the probe was written.
    struct Fake {
        reply: Vec<u8>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl Read for Fake {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let probed = self.written.lock().unwrap().ends_with(b"M105\n");
            if !probed || self.reply.is_empty() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
            }
            let n = out.len().min(self.reply.len());
            out[..n].copy_from_slice(&self.reply[..n]);
            self.reply.drain(..n);
            Ok(n)
        }
    }

    struct FakeWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for FakeWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn fake(reply: &[u8]) -> (Link, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let link = Link::new(
            Fake {
                reply: reply.to_vec(),
                written: Arc::clone(&written),
            },
            FakeWriter(Arc::clone(&written)),
        );
        (link, written)
    }

    #[test]
    fn valid_lines() {
        for l in [
            "ok",
            "ok T:20 /0",
            "start",
            "echo:Marlin 2.1",
            " T:21.3 /0.0 B:20 /0 @:0",
            "Marlin",
        ] {
            assert!(looks_valid(l), "{l}");
        }
        for l in [
            "",
            "  ",
            "\u{FFFD}\u{FFFD}x",
            "x\u{1}ok",
            "okay",
            "garbage",
            "o",
        ] {
            assert!(!looks_valid(l), "{l:?}");
        }
    }

    #[test]
    fn picks_the_first_rate_that_answers() {
        let mut tried = Vec::new();
        let mut logs = Vec::new();
        let mut probes = Vec::new();
        let result = detect_baud(
            &[250_000, 115_200, 57_600],
            |baud| {
                tried.push(baud);
                let reply: &[u8] = match baud {
                    250_000 => b"\xf0\x0f\x80garbage\n\xff\xfe\n",
                    115_200 => b"start\necho:Marlin\nok\n",
                    _ => b"ok\n",
                };
                let (link, written) = fake(reply);
                probes.push(written);
                Ok(link)
            },
            Duration::from_millis(200),
            &mut |m| logs.push(m.to_string()),
        );
        let (baud, _link) = result.unwrap();
        assert_eq!(baud, 115_200);
        assert_eq!(tried, [250_000, 115_200]);
        for p in &probes {
            assert_eq!(&*p.lock().unwrap(), PROBE);
        }
        assert!(
            logs.iter().any(|l| l.contains("no valid reply at 250000")),
            "{logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.contains("115200 answered \"start\"")),
            "{logs:?}"
        );
    }

    #[test]
    fn silence_everywhere_is_a_timeout() {
        let mut logs = Vec::new();
        let result = detect_baud(
            &[9600, 19_200],
            |_| Ok(fake(b"").0),
            Duration::from_millis(50),
            &mut |m| logs.push(m.to_string()),
        );
        assert!(matches!(result, Err(HostError::Timeout(_))));
        assert_eq!(logs.len(), 4, "{logs:?}");
    }

    #[test]
    fn an_unopenable_port_aborts_the_search() {
        let mut calls = 0;
        let result = detect_baud(
            &[9600, 19_200],
            |_| {
                calls += 1;
                Err(HostError::Serial("access denied".into()))
            },
            Duration::from_millis(50),
            &mut |_| {},
        );
        assert!(matches!(result, Err(HostError::Serial(_))));
        assert_eq!(calls, 1);
    }

    #[test]
    fn endless_garbage_without_newlines_is_bounded() {
        let garbage = vec![b'x'; 20_000];
        let result = detect_baud(
            &[9600],
            |_| Ok(fake(&garbage).0),
            Duration::from_millis(100),
            &mut |_| {},
        );
        assert!(result.is_err());
    }

    /// Times out `timeouts` times, then accepts everything.
    struct Slow {
        timeouts: usize,
        written: Vec<u8>,
        fail_with: Option<io::ErrorKind>,
    }

    impl Write for Slow {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(kind) = self.fail_with {
                return Err(io::Error::new(kind, "broken"));
            }
            if self.timeouts > 0 {
                self.timeouts -= 1;
                return Err(io::Error::new(io::ErrorKind::TimedOut, "busy"));
            }
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_timeouts_are_retried() {
        let mut w = RetryWriter::new(
            Slow {
                timeouts: 3,
                written: Vec::new(),
                fail_with: None,
            },
            Duration::from_secs(5),
        );
        w.write_all(b"N1 G28*18\n").unwrap();
        w.flush().unwrap();
        assert_eq!(w.inner.written, b"N1 G28*18\n");
    }

    #[test]
    fn real_errors_and_endless_timeouts_fail() {
        let mut w = RetryWriter::new(
            Slow {
                timeouts: 0,
                written: Vec::new(),
                fail_with: Some(io::ErrorKind::BrokenPipe),
            },
            Duration::from_secs(5),
        );
        assert_eq!(w.write(b"x").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        let mut w = RetryWriter::new(
            Slow {
                timeouts: usize::MAX,
                written: Vec::new(),
                fail_with: None,
            },
            Duration::from_millis(20),
        );
        assert_eq!(w.write(b"x").unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
}

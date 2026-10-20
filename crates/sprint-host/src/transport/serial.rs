//! Serial port links and baud rate auto-detection.
//!
//! **The functions that open ports talk to real hardware and are never run
//! by the test suite.** The port I/O loop and auto-detection are generic
//! over the port and the link factory, so their logic is tested with
//! in-memory fakes.
//!
//! # One thread per port
//!
//! A port is driven by a single I/O thread that both writes and reads; the
//! reader and writer halves of the [`Link`] talk to it through channels.
//! Two threads sharing the port (a reader on a `try_clone` and the engine
//! writing) do not work on Windows: serialport opens ports for synchronous
//! I/O, and Windows serialises synchronous I/O on a handle and on handles
//! cloned from it, so a read waiting for data holds back every write until
//! its timeout. In ping-pong the printer waits for the next command while
//! the reader waits for the printer, so every command would be delayed by
//! the read timeout. The I/O thread writes what is queued between reads,
//! and when a complete line arrives it waits a moment for the engine's
//! answer (usually the next command) before it reads again.
//!
//! That wait polls the channel: on Windows a timed channel wait lasts at
//! least a scheduler tick (about 15.6 ms) however short its timeout, which
//! made every round trip on real hardware take 15 ms instead of under one
//! (USB serial bridges deliver `ok` and its newline in separate reads, and
//! the engine cannot answer half a line).

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use super::link::{Link, LinkControl, READ_TIMEOUT, is_no_data};
use crate::error::HostError;

/// How long auto-detection waits for a reply at each baud rate.
pub(crate) const AUTO_BAUD_WAIT: Duration = Duration::from_millis(2500);
/// DTR low time when resetting a board.
const DTR_PULSE: Duration = Duration::from_millis(100);
/// The probe sent at each baud rate.
const PROBE: &[u8] = b"M110 N0\nM105\n";
/// How long a write may make no progress (the port timeout also applies to
/// writes) before the link counts as broken.
const WRITE_PATIENCE: Duration = Duration::from_secs(5);
/// The port's read timeout: the longest a write queued while the I/O
/// thread waits for data is held back.
const PORT_TIMEOUT: Duration = Duration::from_millis(20);
/// After a complete line arrived, how long the I/O thread waits for the
/// engine's answer before it reads again.
const ANSWER_WAIT: Duration = Duration::from_millis(5);
/// How long the answer wait yields before it sleeps between checks (the
/// engine usually answers within a few dozen microseconds).
const ANSWER_SPIN: Duration = Duration::from_micros(200);
/// The answer wait's sleep between checks after that.
const ANSWER_POLL: Duration = Duration::from_micros(200);
/// How long dropping a link waits for its I/O thread to close the port
/// (so it can be opened again at once, e.g. at the next baud rate).
const CLOSE_WAIT: Duration = Duration::from_secs(1);

/// Open `port` at `baud`, 8N1, no flow control; optionally pulse DTR to
/// reset the board. The port is then driven by its own I/O thread.
pub(crate) fn open(port: &str, baud: u32, reset: bool) -> Result<Link, HostError> {
    let mut port = serialport::new(port, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(PORT_TIMEOUT)
        .open()?;
    if reset {
        pulse_dtr(port.as_mut())?;
    }
    // Drop whatever a previous session left in the buffers.
    let _ = port.clear(ClearBuffer::All);
    spawn_io(port, IoTiming::new(PORT_TIMEOUT))
}

/// Bytes read from the port (empty: end of input), or the error that
/// ended the I/O thread.
type Chunk = io::Result<Vec<u8>>;

/// The timing of an I/O thread.
#[derive(Clone, Copy, Debug)]
pub(crate) struct IoTiming {
    /// The port's reads return within about this when no data arrives.
    pub(crate) port_timeout: Duration,
    /// After a complete line arrived, how long to wait for the engine's
    /// answer before reading again.
    pub(crate) answer_wait: Duration,
    /// How long a write may make no progress before the link counts as
    /// broken.
    pub(crate) patience: Duration,
}

impl IoTiming {
    /// The default timing for a port whose reads time out after
    /// `port_timeout`.
    pub(crate) fn new(port_timeout: Duration) -> IoTiming {
        IoTiming {
            port_timeout,
            answer_wait: ANSWER_WAIT,
            patience: WRITE_PATIENCE,
        }
    }
}

/// Start the I/O thread for `port` and return a link to it.
pub(crate) fn spawn_io<P>(port: P, timing: IoTiming) -> Result<Link, HostError>
where
    P: Read + Write + Send + 'static,
{
    let (to_port, outgoing) = crossbeam_channel::unbounded::<Vec<u8>>();
    let (incoming, from_port) = crossbeam_channel::unbounded::<Chunk>();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("sprint-host-serial".to_string())
        .spawn(move || io_loop(port, &outgoing, &incoming, &thread_stop, timing))?;
    Ok(Link {
        reader: Box::new(PortReader {
            rx: from_port,
            pending: Vec::new(),
            offset: 0,
            stop: Arc::clone(&stop),
            thread: Some(thread),
            close_wait: CLOSE_WAIT.max(timing.port_timeout.saturating_mul(4)),
        }),
        writer: Box::new(PortWriter { tx: to_port }),
        control: LinkControl::Serial(stop),
    })
}

/// The I/O thread: write what is queued, read what arrives, until told to
/// stop, the reader half is gone or the port fails.
fn io_loop<P: Read + Write>(
    mut port: P,
    outgoing: &Receiver<Vec<u8>>,
    incoming: &Sender<Chunk>,
    stop: &AtomicBool,
    timing: IoTiming,
) {
    let mut buf = [0u8; 4096];
    loop {
        // Checked before writing: what was queued before the link was
        // closed (an M112 right before disconnecting) still goes out.
        let stopping = stop.load(Ordering::Acquire);
        while let Ok(bytes) = outgoing.try_recv() {
            if let Err(e) = write_patiently(&mut port, &bytes, timing.patience) {
                let _ = incoming.send(Err(e));
                return;
            }
        }
        if stopping {
            return;
        }
        match port.read(&mut buf) {
            Ok(0) => {
                let _ = incoming.send(Ok(Vec::new()));
                return;
            }
            Ok(n) => {
                // The engine acts on complete lines: after part of one the
                // rest is on its way, so read again at once.
                let complete = buf[n - 1] == b'\n';
                if incoming.send(Ok(buf[..n].to_vec())).is_err() {
                    return;
                }
                // The engine usually answers at once (the next command):
                // write it now rather than after another read.
                if complete
                    && let Some(bytes) = wait_answer(outgoing, timing.answer_wait)
                    && let Err(e) = write_patiently(&mut port, &bytes, timing.patience)
                {
                    let _ = incoming.send(Err(e));
                    return;
                }
            }
            Err(e) if is_no_data(&e) => {}
            Err(e) => {
                let _ = incoming.send(Err(e));
                return;
            }
        }
    }
}

/// The engine's answer (the next bytes to write) if it comes within
/// `wait`.
///
/// Polls instead of blocking in `recv_timeout`: on Windows a timed channel
/// wait lasts at least a scheduler tick (about 15.6 ms) however short its
/// timeout, so every line the engine does not answer (a temperature report,
/// an `echo:`) would hold back reading the next one that long, while
/// `thread::sleep` uses a high-resolution timer there.
fn wait_answer(outgoing: &Receiver<Vec<u8>>, wait: Duration) -> Option<Vec<u8>> {
    let start = Instant::now();
    loop {
        match outgoing.try_recv() {
            Ok(bytes) => return Some(bytes),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        let waited = start.elapsed();
        if waited >= wait {
            return None;
        }
        if waited < ANSWER_SPIN {
            thread::yield_now();
        } else {
            thread::sleep(ANSWER_POLL.min(wait - waited));
        }
    }
}

/// Write all of `data`, retrying writes that time out until `patience`
/// passes without progress. A serial port's timeout also applies to
/// writes; on Windows a write that timed out returns `Ok(0)` (or a short
/// count) rather than an error.
pub(crate) fn write_patiently(
    port: &mut impl Write,
    mut data: &[u8],
    patience: Duration,
) -> io::Result<()> {
    let mut progress = Instant::now();
    while !data.is_empty() {
        match port.write(data) {
            Ok(n) if n > 0 => {
                data = &data[n.min(data.len())..];
                progress = Instant::now();
                continue;
            }
            Ok(_) => {}
            Err(e) if is_no_data(&e) => {}
            Err(e) => return Err(e),
        }
        if progress.elapsed() >= patience {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "the serial port accepted no data for {} s",
                    patience.as_secs_f32()
                ),
            ));
        }
        // Not a hot loop if a port reports no progress without waiting.
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

/// The reader half of a serial link. Dropping it stops the I/O thread and
/// waits (briefly) until the port is closed.
struct PortReader {
    rx: Receiver<Chunk>,
    pending: Vec<u8>,
    offset: usize,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    close_wait: Duration,
}

impl Read for PortReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.offset >= self.pending.len() {
            match self.rx.recv_timeout(READ_TIMEOUT) {
                Ok(Ok(bytes)) if bytes.is_empty() => return Ok(0),
                Ok(Ok(bytes)) => {
                    self.pending = bytes;
                    self.offset = 0;
                }
                Ok(Err(e)) => return Err(e),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
            }
        }
        let available = &self.pending[self.offset..];
        let n = out.len().min(available.len());
        out[..n].copy_from_slice(&available[..n]);
        self.offset += n;
        Ok(n)
    }
}

impl Drop for PortReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(thread) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + self.close_wait;
        while !thread.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        if thread.is_finished() {
            let _ = thread.join();
        } else {
            log::warn!("the serial I/O thread did not stop in time; detaching it");
        }
    }
}

/// The writer half of a serial link: queues bytes for the I/O thread.
/// Write failures end the link through the reader half.
struct PortWriter {
    tx: Sender<Vec<u8>>,
}

impl Write for PortWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.tx
            .send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the serial port is closed"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
#[cfg(test)]
pub(crate) fn detect_baud<F>(
    rates: &[u32],
    open: F,
    wait: Duration,
    log: &mut dyn FnMut(&str),
) -> Result<(u32, Link), HostError>
where
    F: FnMut(u32) -> Result<Link, HostError>,
{
    detect_baud_until(rates, open, wait, log, &|| false)
}

/// [`detect_baud`] that gives up with [`HostError::Cancelled`] as soon as
/// `cancelled` says so (a disconnect during the search).
pub(crate) fn detect_baud_until<F>(
    rates: &[u32],
    mut open: F,
    wait: Duration,
    log: &mut dyn FnMut(&str),
    cancelled: &dyn Fn() -> bool,
) -> Result<(u32, Link), HostError>
where
    F: FnMut(u32) -> Result<Link, HostError>,
{
    for &baud in rates {
        if cancelled() {
            return Err(HostError::Cancelled);
        }
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
        match wait_for_reply(&mut link, wait, cancelled) {
            Some(line) => {
                log(&format!("Auto-baud: {baud} answered \"{line}\""));
                return Ok((baud, link));
            }
            None if cancelled() => return Err(HostError::Cancelled),
            None => log(&format!("Auto-baud: no valid reply at {baud}")),
        }
    }
    Err(HostError::Timeout(
        "auto-baud: the printer did not answer at any baud rate".to_string(),
    ))
}

/// Read lines until one looks valid or `wait` passes (or `cancelled`).
fn wait_for_reply(link: &mut Link, wait: Duration, cancelled: &dyn Fn() -> bool) -> Option<String> {
    let deadline = Instant::now() + wait;
    let mut pending: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    while Instant::now() < deadline && !cancelled() {
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
    fn a_cancelled_search_stops_at_once() {
        use std::cell::Cell;
        let opened = Cell::new(0);
        let started = Instant::now();
        let result = detect_baud_until(
            &[9600, 19_200, 38_400],
            |_| {
                opened.set(opened.get() + 1);
                Ok(fake(b"").0)
            },
            Duration::from_secs(5),
            &mut |_| {},
            &|| opened.get() >= 1 && started.elapsed() > Duration::from_millis(100),
        );
        assert!(matches!(result, Err(HostError::Cancelled)));
        assert_eq!(opened.get(), 1);
        assert!(started.elapsed() < Duration::from_secs(2));
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

    /// A writer that stalls: `timeouts` TimedOut errors, then `zeros`
    /// writes of nothing (Windows' timed-out WriteFile), then one byte per
    /// write.
    struct Slow {
        timeouts: usize,
        zeros: usize,
        written: Vec<u8>,
        fail_with: Option<io::ErrorKind>,
    }

    impl Slow {
        fn new(timeouts: usize, zeros: usize) -> Slow {
            Slow {
                timeouts,
                zeros,
                written: Vec::new(),
                fail_with: None,
            }
        }
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
            if self.zeros > 0 {
                self.zeros -= 1;
                return Ok(0);
            }
            self.written.extend_from_slice(&buf[..1]);
            Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stalled_and_short_writes_are_retried() {
        let mut w = Slow::new(3, 4);
        write_patiently(&mut w, b"N1 G28*18\n", Duration::from_secs(5)).unwrap();
        assert_eq!(w.written, b"N1 G28*18\n");
    }

    #[test]
    fn real_errors_and_endless_stalls_fail() {
        let mut w = Slow::new(0, 0);
        w.fail_with = Some(io::ErrorKind::BrokenPipe);
        let e = write_patiently(&mut w, b"x", Duration::from_secs(5)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::BrokenPipe);
        for mut w in [Slow::new(usize::MAX, 0), Slow::new(0, usize::MAX)] {
            let e = write_patiently(&mut w, b"x", Duration::from_millis(20)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        }
    }

    /// State shared by a [`SyncPort`] and the test.
    #[derive(Default)]
    struct PortState {
        /// Bytes for the host.
        to_host: Vec<u8>,
        /// Partial line from the host.
        line: Vec<u8>,
        /// Complete lines from the host.
        lines: Vec<String>,
        /// Answer "ok" to every line.
        answer: bool,
        /// Fail reads with this error.
        fail: Option<io::ErrorKind>,
        /// Ports currently open (the port is exclusive, as on Windows).
        open: usize,
        /// Deliver at most this many bytes per read (USB serial bridges
        /// split replies).
        max_read: Option<usize>,
    }

    /// A fake serial port with the semantics of a synchronous Windows
    /// handle: one lock covers reads and writes, and a read without data
    /// holds it until its timeout.
    struct SyncPort {
        state: Arc<(Mutex<PortState>, std::sync::Condvar)>,
        io: Arc<Mutex<()>>,
        timeout: Duration,
    }

    impl SyncPort {
        fn new(state: &Arc<(Mutex<PortState>, std::sync::Condvar)>, timeout: Duration) -> SyncPort {
            let mut s = state.0.lock().unwrap();
            assert_eq!(s.open, 0, "the port is still open elsewhere");
            s.open += 1;
            drop(s);
            SyncPort {
                state: Arc::clone(state),
                io: Arc::new(Mutex::new(())),
                timeout,
            }
        }
    }

    impl Drop for SyncPort {
        fn drop(&mut self) {
            self.state.0.lock().unwrap().open -= 1;
        }
    }

    impl Read for SyncPort {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let _io = self.io.lock().unwrap();
            let (lock, data) = &*self.state;
            let deadline = Instant::now() + self.timeout;
            let mut s = lock.lock().unwrap();
            loop {
                if let Some(kind) = s.fail {
                    return Err(io::Error::new(kind, "device removed"));
                }
                if !s.to_host.is_empty() {
                    let n = out
                        .len()
                        .min(s.to_host.len())
                        .min(s.max_read.unwrap_or(usize::MAX));
                    out[..n].copy_from_slice(&s.to_host[..n]);
                    s.to_host.drain(..n);
                    return Ok(n);
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
                }
                s = data.wait_timeout(s, deadline - now).unwrap().0;
            }
        }
    }

    impl Write for SyncPort {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _io = self.io.lock().unwrap();
            let (lock, data) = &*self.state;
            let mut s = lock.lock().unwrap();
            for &b in buf {
                if b == b'\n' {
                    let line = String::from_utf8_lossy(&std::mem::take(&mut s.line)).into_owned();
                    s.lines.push(line);
                    if s.answer {
                        s.to_host.extend_from_slice(b"ok\n");
                    }
                } else {
                    s.line.push(b);
                }
            }
            data.notify_all();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn port_state(answer: bool) -> Arc<(Mutex<PortState>, std::sync::Condvar)> {
        Arc::new((
            Mutex::new(PortState {
                answer,
                ..PortState::default()
            }),
            std::sync::Condvar::new(),
        ))
    }

    /// Read from a link until `want` arrived.
    fn read_until(link: &mut Link, want: &[u8], patience: Duration) -> Vec<u8> {
        let deadline = Instant::now() + patience;
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        while !got.ends_with(want) {
            assert!(Instant::now() < deadline, "no {want:?}, got {got:?}");
            match link.reader.read(&mut buf) {
                Ok(0) => panic!("the link closed"),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(e) => assert!(is_no_data(&e), "{e}"),
            }
        }
        got
    }

    #[test]
    fn ping_pong_is_not_held_back_by_reads() {
        // Every read without data blocks the port for 50 ms. With a
        // reader thread and a writer sharing the port, each command would
        // wait for that; the I/O thread answers within its answer window.
        let state = port_state(true);
        let timeout = Duration::from_millis(50);
        let mut link = spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout)).unwrap();
        let start = Instant::now();
        let rounds = 100u32;
        for i in 0..rounds {
            link.writer
                .write_all(format!("N{i} G1 X1\n").as_bytes())
                .unwrap();
            link.writer.flush().unwrap();
            read_until(&mut link, b"ok\n", Duration::from_secs(5));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < timeout * rounds / 4,
            "{rounds} round trips took {elapsed:?}"
        );
        let lines = state.0.lock().unwrap().lines.clone();
        assert_eq!(lines.len(), 100);
        assert_eq!(lines[99], "N99 G1 X1");
    }

    #[test]
    fn a_reply_split_across_reads_is_not_held_back() {
        // USB serial bridges deliver `ok` and its newline in separate
        // reads. The engine cannot answer half a line, so waiting for its
        // answer after one cost the whole answer wait on every command.
        let state = port_state(true);
        state.0.lock().unwrap().max_read = Some(2);
        let timeout = Duration::from_millis(50);
        let timing = IoTiming {
            answer_wait: Duration::from_millis(200),
            ..IoTiming::new(timeout)
        };
        let mut link = spawn_io(SyncPort::new(&state, timeout), timing).unwrap();
        let start = Instant::now();
        let rounds = 20u32;
        for i in 0..rounds {
            link.writer
                .write_all(format!("N{i} M400\n").as_bytes())
                .unwrap();
            read_until(&mut link, b"ok\n", Duration::from_secs(5));
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < timing.answer_wait * rounds / 4,
            "{rounds} round trips took {elapsed:?}"
        );
        assert_eq!(state.0.lock().unwrap().lines.len(), 20);
    }

    #[test]
    fn the_answer_wait_lasts_as_long_as_asked() {
        // On Windows a timed channel wait lasts at least a scheduler tick
        // (15.6 ms) however short its timeout; the answer wait must not,
        // or every line the engine does not answer holds back the next.
        let (tx, rx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let mut waits: Vec<Duration> = (0..21)
            .map(|_| {
                let start = Instant::now();
                assert!(wait_answer(&rx, ANSWER_WAIT).is_none());
                start.elapsed()
            })
            .collect();
        waits.sort();
        let median = waits[waits.len() / 2];
        assert!(
            median >= ANSWER_WAIT && median < ANSWER_WAIT * 2,
            "median wait {median:?}"
        );

        // An answer that comes during the wait ends it.
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(2));
            tx.send(b"N2 M105\n".to_vec()).unwrap();
        });
        let start = Instant::now();
        let answer = wait_answer(&rx, Duration::from_secs(5));
        assert_eq!(answer.as_deref(), Some(&b"N2 M105\n"[..]));
        assert!(start.elapsed() < Duration::from_secs(1));
        sender.join().unwrap();
        // So does the engine going away.
        let start = Instant::now();
        assert!(wait_answer(&rx, Duration::from_secs(5)).is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn port_errors_end_the_link_and_closing_releases_the_port() {
        let state = port_state(false);
        let timeout = Duration::from_millis(10);
        let mut link = spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout)).unwrap();
        state
            .0
            .lock()
            .unwrap()
            .to_host
            .extend_from_slice(b"start\n");
        read_until(&mut link, b"start\n", Duration::from_secs(5));
        // The device goes away: the error reaches the reader, then the
        // link reads as closed and writes fail.
        state.0.lock().unwrap().fail = Some(io::ErrorKind::PermissionDenied);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buf = [0u8; 16];
        let error = loop {
            assert!(Instant::now() < deadline, "the error never arrived");
            match link.reader.read(&mut buf) {
                Err(e) if is_no_data(&e) => {}
                other => break other,
            }
        };
        assert_eq!(error.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        loop {
            assert!(Instant::now() < deadline, "the link did not close");
            match link.reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => panic!("data after the error"),
                Err(e) => assert!(is_no_data(&e), "{e}"),
            }
        }
        assert_eq!(
            state.0.lock().unwrap().open,
            0,
            "the thread closed the port"
        );
        assert_eq!(
            link.writer.write(b"M105\n").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );

        // A healthy link: closing it through the handle stops the thread;
        // dropping it releases the port at once, so it can be reopened.
        let state = port_state(true);
        let link = spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout)).unwrap();
        let (mut reader, _writer, handle) = link.split();
        handle.close();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "the link did not close");
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => assert!(is_no_data(&e), "{e}"),
            }
        }
        drop(reader);
        assert_eq!(state.0.lock().unwrap().open, 0);
        let again = spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout)).unwrap();
        drop(again);
        assert_eq!(state.0.lock().unwrap().open, 0);
    }

    #[test]
    fn what_was_written_before_closing_still_goes_out() {
        let state = port_state(false);
        let timeout = Duration::from_millis(30);
        let link = spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout)).unwrap();
        let (reader, mut writer, handle) = link.split();
        // Emergency stop, then disconnect at once.
        writer.write_all(b"M112\n").unwrap();
        handle.close();
        drop(reader);
        assert_eq!(state.0.lock().unwrap().lines, ["M112"]);
        assert_eq!(state.0.lock().unwrap().open, 0);
    }

    #[test]
    fn auto_baud_releases_each_port_before_the_next_rate() {
        // Like a Windows COM port, the fake can be open only once: the
        // link of a rate that did not answer must be closed before the
        // next rate opens the port again.
        let state = port_state(false);
        let timeout = Duration::from_millis(10);
        let mut opened = Vec::new();
        let result = detect_baud(
            &[250_000, 115_200, 57_600],
            |baud| {
                opened.push(baud);
                state.0.lock().unwrap().answer = baud == 115_200;
                spawn_io(SyncPort::new(&state, timeout), IoTiming::new(timeout))
            },
            Duration::from_millis(150),
            &mut |_| {},
        );
        let (baud, link) = result.unwrap();
        assert_eq!(baud, 115_200);
        assert_eq!(opened, [250_000, 115_200]);
        let lines = state.0.lock().unwrap().lines.clone();
        assert_eq!(lines, ["M110 N0", "M105", "M110 N0", "M105"]);
        drop(link);
        assert_eq!(state.0.lock().unwrap().open, 0);
    }
}

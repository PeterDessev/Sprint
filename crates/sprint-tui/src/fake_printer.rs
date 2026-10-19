//! A minimal fake Marlin on a loopback TCP port, for what the simulator
//! does not do: wait for the user without a host prompt, as stock Marlin
//! builds without HOST_PROMPT_SUPPORT do after M0.
//!
//! It answers every line with `ok` (M105 with temperatures, M115 with a
//! firmware name). After `M0`/`M1` it sends `echo:busy: paused for user`
//! every 200 ms and holds the `ok` (and the commands that follow, as
//! Marlin's command queue does) until `M108` arrives. Started with
//! [`FakePrinter::start_announcing_pauses`], it also says
//! `//action:paused` when the wait begins (Marlin with host action
//! commands but without host prompts).

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use sprint_host::ConnectTarget;

/// How often the busy keepalive is sent while waiting for the user.
const KEEPALIVE: Duration = Duration::from_millis(200);

/// The fake printer; stops when dropped.
pub(crate) struct FakePrinter {
    port: u16,
    stop: Arc<AtomicBool>,
    received: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl FakePrinter {
    /// Listen on a free loopback port and serve the first client.
    pub(crate) fn start() -> FakePrinter {
        FakePrinter::serving(false)
    }

    /// [`FakePrinter::start`], announcing each wait for the user with
    /// `//action:paused` (the host then pauses its print).
    pub(crate) fn start_announcing_pauses() -> FakePrinter {
        FakePrinter::serving(true)
    }

    fn serving(announce_pauses: bool) -> FakePrinter {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
        let port = listener.local_addr().expect("local address").port();
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let stop = Arc::new(AtomicBool::new(false));
        let received = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let stop = Arc::clone(&stop);
            let received = Arc::clone(&received);
            std::thread::spawn(move || serve(&listener, &stop, &received, announce_pauses))
        };
        FakePrinter {
            port,
            stop,
            received,
            thread: Some(thread),
        }
    }

    /// Where to connect.
    pub(crate) fn target(&self) -> ConnectTarget {
        ConnectTarget::parse(&format!("tcp://127.0.0.1:{}", self.port)).expect("target")
    }

    /// The commands received so far (without line numbers and checksums).
    pub(crate) fn received(&self) -> Vec<String> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakePrinter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    listener: &TcpListener,
    stop: &AtomicBool,
    received: &Mutex<Vec<String>>,
    announce_pauses: bool,
) {
    let mut stream = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    };
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let mut pending = Vec::new();
    // Commands not run yet: those that came during a wait for the user.
    let mut queued: VecDeque<String> = VecDeque::new();
    let mut waiting_since: Option<Instant> = None;
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => pending.extend_from_slice(&buf[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            let command = command_of(&String::from_utf8_lossy(&line));
            if command.is_empty() {
                continue;
            }
            received
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.clone());
            queued.push_back(command);
        }
        // M108 is handled at once (Marlin's emergency parser); the other
        // commands wait while the firmware waits for the user.
        if waiting_since.is_some()
            && let Some(at) = queued.iter().position(|c| word_of(c) == "M108")
        {
            queued.remove(at);
            waiting_since = None;
            // The held ok of M0, then M108's own.
            if stream.write_all(b"ok\nok\n").is_err() {
                return;
            }
        }
        while waiting_since.is_none()
            && let Some(command) = queued.pop_front()
        {
            let reply = match word_of(&command).as_str() {
                "M0" | "M1" => {
                    waiting_since = Some(Instant::now());
                    if announce_pauses {
                        "//action:paused\n".to_string()
                    } else {
                        String::new()
                    }
                }
                "M105" => "ok T:20.0 /0.0 B:20.0 /0.0\n".to_string(),
                "M115" => "FIRMWARE_NAME:Marlin 2.1.2 (Fake)\nok\n".to_string(),
                _ => "ok\n".to_string(),
            };
            if stream.write_all(reply.as_bytes()).is_err() {
                return;
            }
        }
        if let Some(since) = waiting_since
            && since.elapsed() >= KEEPALIVE
        {
            waiting_since = Some(Instant::now());
            if stream.write_all(b"echo:busy: paused for user\n").is_err() {
                return;
            }
        }
    }
}

/// The first word of a command, upper-case.
fn word_of(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// The command of a line as sent: `N12 M0*34` gives `M0`.
fn command_of(line: &str) -> String {
    let line = line.trim();
    let line = line.split('*').next().unwrap_or_default();
    let mut words = line.split_whitespace().peekable();
    if words
        .peek()
        .is_some_and(|w| w.starts_with('N') && w[1..].parse::<i64>().is_ok())
    {
        words.next();
    }
    words.collect::<Vec<_>>().join(" ")
}

//! A tiny in-test host for driving the simulator through its handles.
#![allow(dead_code)]

use sprint_sim::{SimConfig, SimControl, SimLink, spawn};
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// Generous upper bound for anything a test waits for (real time).
pub const PATIENCE: Duration = Duration::from_secs(10);

/// XOR checksum.
pub fn checksum(s: &str) -> u8 {
    s.bytes().fold(0, |a, b| a ^ b)
}

/// "N<n> <cmd>*<cs>".
pub fn frame(n: u32, cmd: &str) -> String {
    let payload = format!("N{n} {cmd}");
    format!("{payload}*{}", checksum(&payload))
}

/// A fast-booting configuration for tests.
pub fn fast(time_scale: f32) -> SimConfig {
    SimConfig {
        time_scale,
        boot_delay_ms: 20,
        ..SimConfig::default()
    }
}

pub struct Host {
    pub link: SimLink,
    pending: Vec<u8>,
    /// Every line received so far.
    pub log: Vec<String>,
    /// Next line number to send.
    pub next_line: u32,
}

impl Host {
    pub fn new(config: SimConfig) -> Host {
        let mut link = spawn(config);
        link.reader.set_timeout(Duration::from_millis(20));
        Host {
            link,
            pending: Vec::new(),
            log: Vec::new(),
            next_line: 1,
        }
    }

    /// Spawn and wait for the boot banner.
    pub fn booted(config: SimConfig) -> Host {
        let mut h = Host::new(config);
        h.boot();
        h
    }

    pub fn control(&self) -> SimControl {
        self.link.control.clone()
    }

    /// The next line, or None on timeout / shutdown.
    pub fn line(&mut self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self.pending.iter().position(|&b| b == b'\n') {
                let raw: Vec<u8> = self.pending.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&raw[..raw.len() - 1])
                    .trim_end_matches('\r')
                    .to_string();
                self.log.push(line.clone());
                return Some(line);
            }
            if Instant::now() >= deadline {
                return None;
            }
            let mut buf = [0u8; 4096];
            match self.link.reader.read(&mut buf) {
                Ok(0) => return None,
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => panic!("read failed: {e}"),
            }
        }
    }

    /// Lines up to and including the first one matching `pred`.
    pub fn until(&mut self, timeout: Duration, pred: impl Fn(&str) -> bool) -> Vec<String> {
        let deadline = Instant::now() + timeout;
        let mut lines = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.line(left) {
                Some(l) => {
                    let done = pred(&l);
                    lines.push(l);
                    if done {
                        return lines;
                    }
                }
                None => panic!("timed out; got {lines:?}"),
            }
        }
    }

    /// Collect everything that arrives within `window`.
    pub fn drain(&mut self, window: Duration) -> Vec<String> {
        let deadline = Instant::now() + window;
        let mut lines = Vec::new();
        while let Some(l) = self.line(deadline.saturating_duration_since(Instant::now())) {
            lines.push(l);
        }
        lines
    }

    pub fn write(&mut self, line: &str) {
        self.link
            .writer
            .write_all(format!("{line}\n").as_bytes())
            .expect("write to simulator");
    }

    pub fn boot(&mut self) -> Vec<String> {
        self.until(PATIENCE, |l| l == "echo:SD card ok")
    }

    /// Send a bare command and wait for its ok.
    pub fn command(&mut self, cmd: &str) -> Vec<String> {
        self.write(cmd);
        self.until(PATIENCE, |l| l.starts_with("ok"))
    }

    /// Send a numbered command and wait for its ok.
    pub fn numbered(&mut self, cmd: &str) -> Vec<String> {
        let n = self.next_line;
        self.next_line += 1;
        self.write(&frame(n, cmd));
        self.until(PATIENCE, |l| l.starts_with("ok"))
    }

    /// Ping-pong streaming with resend handling and lost-ok recovery
    /// (after `ok_timeout` without a reply, send M105 and move on, like
    /// sprint-host). Returns (resends, timeouts).
    pub fn stream(&mut self, cmds: &[String], ok_timeout: Duration) -> (usize, usize) {
        let base = self.next_line;
        let mut i = 0usize;
        let mut resends = 0;
        let mut timeouts = 0;
        let started = Instant::now();
        while i < cmds.len() {
            assert!(started.elapsed() < Duration::from_secs(60), "stream stuck");
            let n = base + i as u32;
            self.write(&frame(n, &cmds[i]));
            let mut resend_to = None;
            loop {
                match self.line(ok_timeout) {
                    Some(l) => {
                        if let Some(r) = l.strip_prefix("Resend: ") {
                            resend_to = r.trim().parse::<u32>().ok();
                        } else if l.starts_with("ok") {
                            break;
                        }
                    }
                    None => {
                        // Lost ok: re-synchronise with M105 and continue.
                        timeouts += 1;
                        self.write("M105");
                        self.until(PATIENCE, |l| l.starts_with("ok"));
                        break;
                    }
                }
            }
            match resend_to {
                Some(r) => {
                    resends += 1;
                    i = r.saturating_sub(base) as usize;
                }
                None => i += 1,
            }
        }
        self.next_line = base + cmds.len() as u32;
        (resends, timeouts)
    }
}

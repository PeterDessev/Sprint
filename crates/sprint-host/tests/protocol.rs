//! Protocol edge cases with hand-written firmware behind a custom link
//! ([`Printer::connect_link`]) and special simulator settings.

mod common;

use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use common::*;
use crossbeam_channel::{Receiver, Sender, unbounded};
use sprint_host::sprint_gcode::GcodeFile;
use sprint_host::sprint_proto::parse_framed;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    ConnectOptions, ConnectionStatus, ConsoleKind, Event, JobStatus, Link, PrintOptions, Printer,
};

/// Bytes from the fake printer.
struct FakeReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
}

impl Read for FakeReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buf.is_empty() {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(b) => self.buf = b,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no data"));
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        self.buf.drain(..n);
        Ok(n)
    }
}

/// Lines to the fake printer.
struct FakeWriter {
    tx: Sender<String>,
    pending: Vec<u8>,
}

impl Write for FakeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            if b == b'\n' {
                let line = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                self.tx
                    .send(line)
                    .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "gone"))?;
            } else {
                self.pending.push(b);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Replies of the fake printer.
#[derive(Clone)]
struct Out(Sender<Vec<u8>>);

impl Out {
    fn line(&self, line: &str) {
        let _ = self.0.send(format!("{line}\n").into_bytes());
    }
    fn raw(&self, bytes: &[u8]) {
        let _ = self.0.send(bytes.to_vec());
    }
}

/// A fake firmware: `handler` gets every line the host sends.
fn fake(
    greeting: impl FnOnce(&Out) + Send + 'static,
    mut handler: impl FnMut(&str, &Out) + Send + 'static,
) -> (Link, Out) {
    let (to_host, from_fake) = unbounded();
    let (to_fake, from_host) = unbounded::<String>();
    let out = Out(to_host);
    let fw_out = out.clone();
    thread::spawn(move || {
        greeting(&fw_out);
        while let Ok(line) = from_host.recv() {
            handler(&line, &fw_out);
        }
    });
    let link = Link::new(
        FakeReader {
            rx: from_fake,
            buf: Vec::new(),
        },
        FakeWriter {
            tx: to_fake,
            pending: Vec::new(),
        },
    );
    (link, out)
}

/// The command of a host line (frame removed).
fn command(line: &str) -> String {
    match parse_framed(line) {
        Ok(f) => f.command.to_string(),
        Err(_) => line.trim().to_string(),
    }
}

/// A minimal Marlin-like firmware: validates line numbers, answers ok.
struct Mini {
    last: i64,
    log: Arc<Mutex<Vec<String>>>,
}

impl Mini {
    fn new(log: Arc<Mutex<Vec<String>>>) -> Mini {
        Mini { last: 0, log }
    }

    /// Returns the accepted command, or None when it asked for a resend.
    fn accept(&mut self, line: &str, out: &Out) -> Option<String> {
        if let Ok(f) = parse_framed(line) {
            let n = i64::from(f.line_number);
            if let Some(v) = f.command.strip_prefix("M110 N") {
                self.last = v.trim().parse().unwrap_or(0);
            } else if n != self.last + 1 {
                out.line(&format!(
                    "Error:Line Number is not Last Line Number+1, Last Line: {}",
                    self.last
                ));
                out.line(&format!("Resend: {}", self.last + 1));
                out.line("ok");
                return None;
            } else {
                self.last = n;
            }
            self.log.lock().unwrap().push(f.command.to_string());
            Some(f.command.to_string())
        } else {
            let cmd = command(line);
            self.log.lock().unwrap().push(cmd.clone());
            Some(cmd)
        }
    }
}

fn connect_fake(link: Link, options: ConnectOptions) -> Printer {
    let printer = Printer::new();
    printer.connect_link(link, "fake printer", options).unwrap();
    printer
}

#[test]
fn a_silent_printer_fails_the_handshake() {
    let (link, _out) = fake(|_| {}, |_, _| {});
    let printer = connect_fake(
        link,
        ConnectOptions {
            handshake_timeout_secs: 1.5,
            ..options()
        },
    );
    assert_eq!(
        printer.with_state(|s| s.target.clone()).as_deref(),
        Some("fake printer")
    );
    wait_for(&printer, "the handshake to fail", |s| {
        matches!(s.status, ConnectionStatus::Failed(_))
    });
    let status = printer.with_state(|s| s.status.clone());
    assert!(
        matches!(&status, ConnectionStatus::Failed(m) if m.contains("Handshake")),
        "{status:?}"
    );
    // M110 went out after a second of silence.
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Sent && e.text == "M110 N0")
    );
    assert!(!printer.is_connected());
}

#[test]
fn a_board_that_does_not_reset_is_found_by_asking() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |_| {},
        move |line, out| {
            if fw.accept(line, out).is_some() {
                out.line("ok");
            }
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.send("G28\nG1 X5");
    wait_idle(&printer);
    assert_eq!(*log.lock().unwrap(), ["M110 N0", "G28", "G1 X5"]);
}

#[test]
fn garbage_from_the_printer_is_harmless() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |out| {
            out.raw(b"\xff\xfe\x00garbage\r\n\x1b[2Jok?\n");
            out.raw(&vec![b'x'; 40_000]);
            out.raw(b"\nstart\n");
        },
        move |line, out| {
            if fw.accept(line, out).is_some() {
                out.raw(b"\xc3\x28 nonsense T:abc B:\n");
                out.line("ok");
            }
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.send("M114");
    wait_idle(&printer);
    let entries = console(&printer);
    assert!(entries.iter().any(|e| e.text.contains('\u{FFFD}')));
    assert!(entries.iter().all(|e| e.text.len() <= 16 * 1024));
}

#[test]
fn fatal_errors_halt_and_m999_restarts() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, out) = fake(
        |out| out.line("start"),
        move |line, out| {
            if command(line) == "M999" {
                // Marlin: resend request + ok instead of an ok of its own.
                out.line(&format!("Resend: {}", fw.last + 1));
                out.line("ok");
                fw.log.lock().unwrap().push("M999".into());
                return;
            }
            if fw.accept(line, out).is_some() {
                out.line("ok");
            }
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    let events = printer.subscribe();
    out.line("Error:Printer stopped due to errors. Fix the error and use M999 to restart.");
    wait_for(&printer, "halted", |s| s.halted);
    assert!(events.try_iter().any(|e| matches!(e, Event::Error(_))));
    assert!(
        printer
            .with_state(|s| s.last_error.clone())
            .is_some_and(|e| e.contains("stopped"))
    );
    printer.firmware_restart();
    wait_for(&printer, "running again", |s| !s.halted);
    printer.send("G28");
    wait_idle(&printer);
    let log = log.lock().unwrap().clone();
    // After the restart the line counter is reset before G28.
    let m999 = log.iter().position(|c| c == "M999").unwrap();
    assert_eq!(&log[m999 + 1..], ["M110 N0", "G28"], "{log:?}");
}

#[test]
fn resend_requests_for_unknown_lines_reset_the_counter() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut seen_g4 = false;
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            if command(line) == "G4 P0" && !seen_g4 {
                // Something moved the counter: the line is rejected and a
                // line far in the future is requested.
                seen_g4 = true;
                fw.last = 9998;
                out.line("Error:Line Number is not Last Line Number+1, Last Line: 9998");
                out.line("Resend: 9999");
                out.line("ok");
                return;
            }
            if fw.accept(line, out).is_some() {
                out.line("ok");
            }
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.send("G4 P0");
    wait_for(&printer, "the counter reset", |_| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|c| *c == "M110 N0")
            .count()
            == 2
    });
    // The rejected G4 is dropped, not resent; later commands work.
    printer.send("G28");
    wait_idle(&printer);
    let log = log.lock().unwrap().clone();
    assert_eq!(log, ["M110 N0", "M110 N0", "G28"]);
    assert_eq!(printer.with_state(|s| s.link.timeouts), 0);
    assert!(
        console(&printer)
            .iter()
            .any(|e| e.kind == ConsoleKind::Error && e.text.contains("9999"))
    );
}

#[test]
fn wait_with_commands_in_flight_recovers_without_the_timeout() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, out) = fake(
        |out| out.line("start"),
        move |line, out| {
            if let Some(cmd) = fw.accept(line, out)
                && cmd != "G28"
            {
                out.line("ok");
            }
            // G28's ok is "lost".
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ok_timeout_secs: 120.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.send("G28");
    wait_for(&printer, "G28 in flight", |s| s.link.in_flight == 1);
    // Marlin prints "wait" when its queue has been empty for a while.
    thread::sleep(Duration::from_millis(1100));
    let start = Instant::now();
    out.line("wait");
    wait_for(&printer, "recovery", |s| {
        s.link.in_flight == 0 && s.link.timeouts == 1
    });
    assert!(start.elapsed() < Duration::from_secs(5));
    wait_idle(&printer);
    assert!(log.lock().unwrap().iter().any(|c| c == "M105"));
}

#[test]
fn klipper_is_not_asked_for_m503_and_reports_meshes_its_way() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |_| {},
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd == "M115" {
                out.line("ok FIRMWARE_NAME:Klipper FIRMWARE_VERSION:v0.12.0");
                return;
            }
            out.line("ok");
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "the firmware", |s| {
        s.firmware.is_some() && s.position.is_none() && s.is_connected()
    });
    wait_idle(&printer);
    printer.request_bed_mesh();
    wait_idle(&printer);
    let log = log.lock().unwrap().clone();
    assert!(!log.iter().any(|c| c == "M503"), "{log:?}");
    assert!(log.iter().any(|c| c == "BED_MESH_OUTPUT"), "{log:?}");
}

#[test]
fn advanced_ok_line_numbers_heal_lost_oks() {
    let sim = SimConfig {
        advanced_ok: true,
        drop_ok_rate: 0.05,
        seed: 5,
        ..fast_sim(100.0)
    };
    let printer = connect(
        sim,
        ConnectOptions {
            max_in_flight: 4,
            ok_timeout_secs: 0.5,
            ..options()
        },
    );
    let control = printer.sim_control().unwrap();
    let mut text = String::from("G28\nG90\n");
    for i in 0..800 {
        text.push_str(&format!("G1 X{} Y{} F6000\n", i % 200, i % 100));
    }
    let file = Arc::new(GcodeFile::from_text("advanced.gcode", text));
    let want: Vec<String> = (0..file.len())
        .filter_map(|i| file.command(i).map(|c| c.into_owned()))
        .collect();
    control.clear_received();
    printer.start_print(file, PrintOptions::default()).unwrap();
    wait_for(&printer, "completed", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Completed)
    );
    let got: Vec<String> = control
        .received_commands()
        .into_iter()
        .filter(|c| c != "M105")
        .collect();
    assert_eq!(got, want);
    // About 40 oks were lost; ok N<n> recovered nearly all of them.
    assert!(printer.with_state(|s| s.link.timeouts) <= 2);
}

#[test]
fn injected_fatal_errors_fail_the_job() {
    let sim = SimConfig {
        instant_moves: false,
        ..fast_sim(20.0)
    };
    let printer = connect(sim, options());
    let control = printer.sim_control().unwrap();
    let mut text = String::new();
    for i in 0..2000 {
        text.push_str(&format!("G1 X{} F600\n", i % 100));
    }
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("long.gcode", text)),
            PrintOptions::default(),
        )
        .unwrap();
    wait_for(&printer, "printing", |s| {
        s.job.as_ref().is_some_and(|j| j.current_line > 5)
    });
    control.inject_line("Error:Thermal Runaway, system stopped! Heater_ID: 0");
    wait_for(&printer, "halted", |s| s.halted);
    let status = printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()));
    assert!(
        matches!(&status, Some(JobStatus::Failed(m)) if m.contains("Thermal Runaway")),
        "{status:?}"
    );
    // The link stays open so the user can see what happened.
    assert!(printer.with_state(|s| s.status.is_connected()));
}

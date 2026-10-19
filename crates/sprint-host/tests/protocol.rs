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
    // While halted, commands go out unnumbered (the counter is unknown).
    let seq = last_seq(&printer);
    printer.send("M105");
    wait_for(&printer, "M105 while halted", |_| {
        log.lock().unwrap().iter().any(|c| c == "M105")
    });
    let sent = printer
        .console_since(seq, usize::MAX)
        .into_iter()
        .find(|e| e.kind == ConsoleKind::Sent)
        .unwrap();
    assert_eq!((sent.text.as_str(), sent.line), ("M105", None));
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
    // Waiting for an idle link could look before the request arrived.
    wait_for(&printer, "BED_MESH_OUTPUT", |_| {
        log.lock().unwrap().iter().any(|c| c == "BED_MESH_OUTPUT")
    });
    let log = log.lock().unwrap().clone();
    assert!(!log.iter().any(|c| c == "M503"), "{log:?}");
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

/// A print the engine has to give up on: one M117 longer than the
/// simulator's line buffer is rejected again and again.
fn print_with_an_unsendable_line(printer: &Printer) -> JobStatus {
    let mut text = String::from("M104 S200\nM140 S60\nG28\n");
    for i in 0..50 {
        text.push_str(&format!("G1 X{} F3000\n", i % 100));
    }
    text.push_str(&format!("M117 {}\n", "x".repeat(600)));
    for i in 0..50 {
        text.push_str(&format!("G1 Y{} F3000\n", i % 100));
    }
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("unsendable.gcode", text)),
            PrintOptions::default(),
        )
        .unwrap();
    wait_for(printer, "the job to fail", |s| !s.job_active());
    wait_idle(printer);
    printer.with_state(|s| s.job.as_ref().unwrap().status.clone())
}

#[test]
fn a_print_given_up_on_switches_the_heaters_off() {
    // The printer keeps running after the host gives up on a print: it
    // must not be left holding print temperatures.
    let printer = connect(fast_sim(50.0), options());
    let status = print_with_an_unsendable_line(&printer);
    assert!(
        matches!(&status, JobStatus::Failed(m) if m.contains("rejected")),
        "{status:?}"
    );
    let snap = printer.sim_control().unwrap().snapshot();
    assert_eq!(snap.hotend_targets, [0.0]);
    assert_eq!(snap.bed_target, 0.0);
    let got = printer.sim_control().unwrap().received_commands();
    assert_eq!(&got[got.len() - 3..], ["M104 S0", "M140 S0", "M107"]);
    // The link stays usable.
    printer.send("M105");
    wait_idle(&printer);
}

#[test]
fn a_profile_can_keep_the_heaters_on_after_a_failure() {
    let printer = connect(fast_sim(50.0), options());
    printer.set_profile(sprint_host::PrinterProfile {
        keep_heaters_on_failure: true,
        ..sprint_host::PrinterProfile::default()
    });
    let status = print_with_an_unsendable_line(&printer);
    assert!(matches!(status, JobStatus::Failed(_)), "{status:?}");
    let snap = printer.sim_control().unwrap().snapshot();
    assert_eq!(snap.hotend_targets, [200.0]);
    assert_eq!(snap.bed_target, 60.0);
}

#[test]
fn heaters_off_after_a_failure_survive_a_disconnect() {
    // `sprint print` disconnects as soon as the job fails: the heaters-off
    // commands still waiting for the line counter reset must go out.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut desynced = false;
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            if desynced {
                // After the desync it only records what arrives.
                fw.log.lock().unwrap().push(line.to_string());
                return;
            }
            if command(line) == "G1 X3" {
                desynced = true;
                out.line("Resend: 999");
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
    let file = GcodeFile::from_text("f.gcode", "M104 S210\nG1 X1\nG1 X2\nG1 X3\nG1 X4\n");
    printer
        .start_print(Arc::new(file), PrintOptions::default())
        .unwrap();
    wait_for(&printer, "the job to fail", |s| !s.job_active());
    // The counter reset is never acknowledged, so the heaters-off commands
    // are still queued when the connection closes.
    std::thread::sleep(Duration::from_millis(100));
    printer.disconnect();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let got = log.lock().unwrap().clone();
        if got.iter().any(|l| l == "M107") {
            let tail: Vec<&str> = got.iter().rev().take(3).rev().map(String::as_str).collect();
            assert_eq!(tail, ["M104 S0", "M140 S0", "M107"], "{got:?}");
            break;
        }
        assert!(Instant::now() < deadline, "{got:?}");
        thread::sleep(Duration::from_millis(5));
    }
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

#[test]
fn a_reset_during_the_handshake_repeats_m110_at_once() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut booted = false;
    let (link, _) = fake(
        |_| {},
        move |line, out| {
            if !booted {
                // Opening the port reset the board: the first command is
                // lost while it boots.
                booted = true;
                out.line("start");
                return;
            }
            if fw.accept(line, out).is_some() {
                out.line("ok");
            }
        },
    );
    let start = Instant::now();
    let printer = connect_fake(
        link,
        ConnectOptions {
            query_on_connect: false,
            temp_poll_secs: 0.0,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    // The first M110 goes out after a second of silence; the retry would
    // come two seconds later, the reaction to "start" comes at once.
    assert!(
        start.elapsed() < Duration::from_millis(2500),
        "{:?}",
        start.elapsed()
    );
}

/// A writer that fails the way a buggy custom transport might.
struct PanickingWriter;

impl Write for PanickingWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("transport bug (expected by the test)");
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn an_engine_panic_leaves_a_consistent_state() {
    let (to_host, from_fake) = unbounded();
    let _keep = to_host.clone();
    let link = Link::new(
        FakeReader {
            rx: from_fake,
            buf: Vec::new(),
        },
        PanickingWriter,
    );
    let printer = connect_fake(link, options());
    // A line makes the engine send M110, which panics in the writer.
    let _ = to_host.send(b"start\n".to_vec());
    wait_for(&printer, "the failure", |s| {
        matches!(s.status, ConnectionStatus::Failed(_))
    });
    let status = printer.with_state(|s| s.status.clone());
    assert!(
        matches!(&status, ConnectionStatus::Failed(m) if m.contains("Internal error")),
        "{status:?}"
    );
    // The printer can connect again.
    printer
        .connect(
            sprint_host::ConnectTarget::Simulator(fast_sim(20.0)),
            options(),
        )
        .unwrap();
    wait_for(&printer, "connected again", |s| s.is_connected());
}

#[test]
fn marlin_1_settings_report_headers_are_not_messages() {
    // Marlin 1.1 and Prusa print report headers without the ';'.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd == "M503" {
                out.line("echo:Steps per unit:");
                out.line("echo:  M92 X100.00 Y100.00 Z400.00 E280.00");
                out.line("echo:Maximum feedrates (mm/s):");
                out.line("echo:  M203 X200.00 Y200.00 Z12.00 E120.00");
                out.line("echo:Filament settings: Disabled");
            }
            if cmd == "M118 E1 hello" {
                out.line("echo:hello");
            }
            out.line("ok");
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            temp_poll_secs: 0.0,
            query_on_connect: false,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.request_settings();
    printer.send("M118 E1 hello");
    wait_for(&printer, "the settings", |s| {
        s.settings_loaded && s.firmware_messages.iter().any(|m| m == "hello")
    });
    let s = printer.snapshot();
    assert!(s.settings.steps_per_mm().is_some());
    assert_eq!(s.firmware_messages, ["hello"], "{:?}", s.firmware_messages);
}

#[test]
fn an_upload_completes_when_m29_is_acknowledged_without_its_message() {
    // "Done saving file." got lost (or the firmware does not print it).
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd == "M28 up.gco" {
                out.line("Writing to file: up.gco");
            }
            out.line("ok");
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            temp_poll_secs: 0.0,
            query_on_connect: false,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    let file = Arc::new(GcodeFile::from_text("up.gcode", "G28\nG1 X10\nG1 Y10\n"));
    printer.upload_to_sd(file, "up.gco").unwrap();
    wait_for_within(&printer, "the upload to end", Duration::from_secs(5), |s| {
        !s.job_active()
    });
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Completed)
    );
    wait_idle(&printer);
    let log = log.lock().unwrap().clone();
    assert_eq!(
        &log[1..],
        ["M28 up.gco", "G28", "G1 X10", "G1 Y10", "M29", "M20"],
        "{log:?}"
    );
}

#[test]
fn an_autotune_the_firmware_does_not_run_does_not_stay_running() {
    // Firmware built without PID autotune: "Unknown command" and ok.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd.starts_with("M303") {
                out.line(&format!("echo:Unknown command: \"{cmd}\""));
            }
            out.line("ok");
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            temp_poll_secs: 0.0,
            query_on_connect: false,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    let events = printer.subscribe();
    printer.start_pid_autotune(sprint_host::Heater::Hotend(0), 200.0, 3);
    wait_for_within(
        &printer,
        "the autotune to end",
        Duration::from_secs(5),
        |s| s.autotune.as_ref().is_some_and(|a| !a.running),
    );
    let at = printer.with_state(|s| s.autotune.clone()).unwrap();
    assert!(at.result.is_none() && at.error.is_some(), "{at:?}");
    assert!(
        events
            .try_iter()
            .any(|e| matches!(e, Event::AutotuneFailed(_)))
    );
}

/// An SD print on firmware that reports `polls` progress lines and then
/// "Not SD printing" (its "Done printing file" was lost, or the print was
/// stopped at the printer).
fn sd_print_that_stops_reporting(progress: &'static [u64]) -> Option<JobStatus> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut polls = 0usize;
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            match cmd.as_str() {
                "M23 part.gco" => {
                    out.line("File opened: PART.GCO Size: 1000");
                    out.line("File selected");
                }
                "M27" => {
                    match progress.get(polls) {
                        Some(done) => out.line(&format!("SD printing byte {done}/1000")),
                        None => out.line("Not SD printing"),
                    }
                    polls += 1;
                }
                _ => {}
            }
            out.line("ok");
        },
    );
    let printer = connect_fake(
        link,
        ConnectOptions {
            temp_poll_secs: 0.0,
            query_on_connect: false,
            ..options()
        },
    );
    wait_for(&printer, "connected", |s| s.is_connected());
    printer.start_sd_print("part.gco").unwrap();
    wait_for_within(&printer, "the job to end", Duration::from_secs(10), |s| {
        !s.job_active()
    });
    printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone()))
}

#[test]
fn an_sd_print_that_is_no_longer_running_ends() {
    // Everything was read: the "Done printing file" line was lost.
    assert_eq!(
        sd_print_that_stops_reporting(&[400, 1000]),
        Some(JobStatus::Completed)
    );
    // Stopped halfway (at the printer): not a success.
    let status = sd_print_that_stops_reporting(&[400]);
    assert!(
        matches!(&status, Some(JobStatus::Failed(m)) if m.contains("not printing")),
        "{status:?}"
    );
}

#[test]
fn sd_status_is_polled_without_auto_reports() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut polls = 0u64;
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            match cmd.as_str() {
                "M115" => {
                    out.line("FIRMWARE_NAME:Marlin 2.0.9 (old) EXTRUDER_COUNT:1");
                    out.line("Cap:SDCARD:1");
                    out.line("Cap:AUTOREPORT_SD_STATUS:0");
                }
                "M23 part.gco" => {
                    out.line("echo:Now fresh file: part.gco");
                    out.line("File opened: PART.GCO Size: 1000");
                    out.line("File selected");
                }
                "M27" => {
                    polls += 1;
                    if polls < 3 {
                        out.line(&format!("SD printing byte {}/1000", polls * 400));
                    } else {
                        out.line("Done printing file");
                    }
                }
                _ => {}
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
    wait_for(&printer, "connected", |s| {
        s.is_connected() && !s.capabilities.is_empty()
    });
    wait_idle(&printer);
    printer.start_sd_print("part.gco").unwrap();
    wait_for(&printer, "progress from a poll", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.bytes_done == 400 && j.bytes_total == 1000)
    });
    wait_for(&printer, "the SD print to complete", |s| !s.job_active());
    assert_eq!(
        printer.with_state(|s| s.job.as_ref().map(|j| j.status.clone())),
        Some(JobStatus::Completed)
    );
    let log = log.lock().unwrap().clone();
    assert!(log.iter().filter(|c| *c == "M27").count() >= 3, "{log:?}");
    assert!(!log.iter().any(|c| c.starts_with("M27 S")), "{log:?}");
    // Polls are automatic traffic.
    assert!(
        console(&printer)
            .iter()
            .filter(|e| e.kind == ConsoleKind::Sent && e.text == "M27")
            .all(|e| e.auto)
    );
}

#[test]
fn homing_and_probing_do_not_inflate_the_eta() {
    // Estimates count G28 and G29 as zero; their real time must not make
    // the print look three times slower than estimated. The firmware takes
    // 2 s for each and exactly the estimated time for every move.
    use sprint_host::sprint_gcode::{AnalyzeOptions, analyze_simple};
    let mut text = String::from("G28\nG29\n");
    for i in 0..30 {
        text.push_str(&format!("G1 X{} F6000\n", if i % 2 == 0 { 15 } else { 10 }));
    }
    let file = Arc::new(GcodeFile::from_text("probe-first.gcode", text));
    let toolpath = Arc::new(analyze_simple(&file, &AnalyzeOptions::default()));
    let moves: Vec<f64> = (2..file.len())
        .map(|i| f64::from(toolpath.time_at_line(i as u32) - toolpath.time_at_line(i as u32 - 1)))
        .collect();
    assert!(moves.iter().all(|&t| t > 0.01), "{moves:?}");
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let mut done_moves = 0;
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd == "G28" || cmd == "G29" {
                thread::sleep(Duration::from_secs(2));
            } else if cmd.starts_with("G1") {
                let t = moves.get(done_moves).copied().unwrap_or(0.0);
                done_moves += 1;
                thread::sleep(Duration::from_secs_f64(t));
            }
            out.line("ok");
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
    let options = PrintOptions {
        toolpath: Some(Arc::clone(&toolpath)),
        ..PrintOptions::default()
    };
    printer.start_print(Arc::clone(&file), options).unwrap();
    // Half of the moves.
    let stop = 2 + 15;
    printer.pause_at_lines(&[stop]);
    wait_for(&printer, "half of the moves", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    let remaining =
        f64::from(toolpath.stats.estimated_time - toolpath.time_at_line(stop as u32 - 1));
    let ratio = job.eta.unwrap().as_secs_f64() / remaining;
    assert!(
        ratio < 1.8,
        "ETA {:?} for {remaining:.2} s of estimated printing (ratio {ratio:.2}), elapsed {:?}",
        job.eta,
        job.elapsed
    );
    printer.cancel();
}

#[test]
fn a_failed_print_records_where_to_recover_from() {
    let printer = connect(fast_sim(50.0), options());
    let status = print_with_an_unsendable_line(&printer);
    assert!(matches!(status, JobStatus::Failed(_)), "{status:?}");
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    // Lines 0..53 went through; line 53 (the long M117) never did.
    assert_eq!(job.stopped_at, Some(53), "{job:?}");
    assert_eq!(job.current_line, 53);
    // The next job starts without it.
    printer
        .start_print(
            Arc::new(GcodeFile::from_text("next.gcode", "G28\nG1 X5\n")),
            PrintOptions::default(),
        )
        .unwrap();
    wait_for(&printer, "the next job", |s| !s.job_active());
    let job = printer.with_state(|s| s.job.clone()).unwrap();
    assert_eq!((job.status, job.stopped_at), (JobStatus::Completed, None));
}

#[test]
fn a_pause_does_not_flood_a_heat_up_the_firmware_cannot_end() {
    // Without EMERGENCY_PARSER, Marlin reads nothing while it waits for a
    // temperature: an M108 cannot end the wait, it only waits in the
    // receive buffer (128 bytes), and one every second overflows it.
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, _) = fake(
        |out| out.line("start"),
        move |line, out| {
            let Some(cmd) = fw.accept(line, out) else {
                return;
            };
            if cmd == "M115" {
                out.line("FIRMWARE_NAME:Marlin 2.1.2 EXTRUDER_COUNT:1");
                out.line("Cap:EMERGENCY_PARSER:0");
            } else if cmd.starts_with("M190") {
                // Reports temperatures while it heats for 4 s.
                for _ in 0..8 {
                    out.line(" T:200.00 /200.00 B:40.00 /60.00");
                    thread::sleep(Duration::from_millis(500));
                }
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
    wait_for(&printer, "connected", |s| {
        s.is_connected() && s.firmware.is_some()
    });
    wait_idle(&printer);
    let file = Arc::new(GcodeFile::from_text(
        "heat.gcode",
        "M190 S60\nG28\nG1 X10 F3000\n",
    ));
    printer.start_print(file, PrintOptions::default()).unwrap();
    wait_for(&printer, "the heat-up", |_| {
        log.lock().unwrap().iter().any(|c| c == "M190 S60")
    });
    printer.pause();
    wait_for(&printer, "paused", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let m108 = log.lock().unwrap().iter().filter(|c| *c == "M108").count();
    assert_eq!(m108, 0, "{:?}", log.lock().unwrap());
    printer.cancel();
}

#[test]
fn klipper_restarts_its_firmware_only_from_a_shutdown() {
    // FIRMWARE_RESTART reboots the MCUs: on a printer that is not shut
    // down it would abort the print ("Restart firmware" is in the GUI's
    // menu whenever the link is open, and a typed M999 restarts too).
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut fw = Mini::new(Arc::clone(&log));
    let (link, out) = fake(
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
        s.firmware.is_some() && s.is_connected()
    });
    wait_idle(&printer);
    printer.firmware_restart();
    printer.send("M999\nM117 after");
    wait_for(&printer, "the commands", |_| {
        log.lock().unwrap().iter().any(|c| c == "M117 after")
    });
    assert!(
        !log.lock().unwrap().iter().any(|c| c == "FIRMWARE_RESTART"),
        "{:?}",
        log.lock().unwrap()
    );
    out.line("!! Shutdown due to M112 command");
    wait_for(&printer, "shut down", |s| s.halted && s.restartable);
    printer.firmware_restart();
    wait_for(&printer, "FIRMWARE_RESTART", |_| {
        log.lock().unwrap().iter().any(|c| c == "FIRMWARE_RESTART")
    });
}

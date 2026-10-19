//! App tests: key handling against the simulator, and rendering of the
//! whole screen with a test backend.

use std::time::{Duration, Instant};

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{AppConfig, ConnectOptions, ConnectTarget, Printer, PrinterState};

use super::*;

/// Generous bound for anything a test waits for.
const PATIENCE: Duration = Duration::from_secs(30);

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// A fast virtual printer.
fn fast_sim() -> SimConfig {
    SimConfig {
        boot_delay_ms: 20,
        time_scale: 100.0,
        instant_moves: true,
        ..SimConfig::default()
    }
}

/// An app on a disconnected printer, with the default configuration and
/// no config file.
fn app() -> App {
    App::new(Printer::new(), AppConfig::default(), None, Theme::dark())
}

/// An app connected to a fast simulator, after the setup queries.
fn connected_app() -> App {
    let printer = Printer::new();
    let options = ConnectOptions {
        ok_timeout_secs: 3.0,
        ..ConnectOptions::default()
    };
    printer
        .connect(ConnectTarget::Simulator(fast_sim()), options)
        .expect("connect to the simulator");
    let mut app = App::new(printer, AppConfig::default(), None, Theme::dark());
    wait(&mut app, "the setup queries", |s| {
        s.is_connected() && s.settings_loaded && s.position.is_some()
    });
    wait_idle(&mut app);
    app
}

/// Sync the app until `cond` holds for the printer state.
fn wait(app: &mut App, what: &str, cond: impl Fn(&PrinterState) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        app.sync();
        app.tick(Instant::now());
        if app.printer.with_state(&cond) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; console:\n{}",
            console_text(app)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Sync the app until `cond` holds for the app itself.
fn wait_app(app: &mut App, what: &str, cond: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        app.sync();
        app.tick(Instant::now());
        if cond(app) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_idle(app: &mut App) {
    let mut quiet = 0;
    let deadline = Instant::now() + PATIENCE;
    while quiet < 3 {
        let idle = app
            .printer
            .with_state(|s| s.link.in_flight == 0 && s.link.queue_len == 0);
        quiet = if idle { quiet + 1 } else { 0 };
        assert!(Instant::now() < deadline, "the link never became idle");
        std::thread::sleep(Duration::from_millis(25));
    }
    app.sync();
}

fn console_text(app: &App) -> String {
    app.console
        .iter()
        .map(|l| format!("{:?} {}", l.kind, l.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Commands the simulator received since the last call.
fn received(app: &App) -> Vec<String> {
    let control = app.printer.sim_control().expect("simulator");
    let got = control.received_commands();
    control.clear_received();
    got
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_event(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn ctrl(app: &mut App, c: char) {
    app.handle_event(TermEvent::Key(KeyEvent::new(
        KeyCode::Char(c),
        KeyModifiers::CONTROL,
    )));
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draw");
    terminal.backend().buffer().clone()
}

fn screen_text(buf: &Buffer) -> String {
    let area = buf.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut row = String::new();
            let mut skip = 0;
            for x in area.left()..area.right() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let symbol = buf[(x, y)].symbol();
                skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
                row.push_str(symbol);
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    screen_text(&render(app, width, height))
}

/// Load a sample and wait for the analysis.
fn load(app: &mut App, name: &str) {
    app.open_file(sample(name));
    wait_app(app, "the analysis", |a| a.loader.is_none());
    assert!(app.file.is_some(), "loaded {name}: {:?}", app.toast);
}

#[test]
fn starts_disconnected_and_draws() {
    let mut app = app();
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("Sprint"), "{text}");
    assert!(text.contains("Disconnected"), "{text}");
    assert!(text.contains("F3"), "{text}");
    assert!(text.contains("Temperatures"), "{text}");
    assert!(text.contains("Console"), "{text}");
}

// ----- synthetic screens -----

/// The cube loaded and a print at layer 42 of 100, without a printer:
/// every value is fixed so screens are reproducible.
fn printing_app() -> App {
    use sprint_host::sprint_proto::{HeaterReading, PositionReport, TempReport};
    use sprint_host::{ConnectionStatus, JobInfo, JobKind, JobStatus};

    let mut app = app();
    load(&mut app, "calibration-cube.gcode");
    let file = app.file.clone().expect("loaded");
    let layer = &file.toolpath.layers[41];
    let current_line = (layer.lines.start + (layer.lines.end - layer.lines.start) / 2) as usize;
    let mut job = JobInfo::new(
        JobKind::HostPrint,
        file.name(),
        file.file.len(),
        file.file.byte_len() as u64,
    );
    job.status = JobStatus::Printing;
    job.current_line = current_line;
    job.progress = 0.42;
    job.layer = Some((41, file.toolpath.layers.len()));
    job.elapsed = Duration::from_secs(23 * 60 + 15);
    job.eta = Some(Duration::from_secs(31 * 60 + 40));
    app.view = PrinterView {
        status: ConnectionStatus::Connected,
        target: Some("Virtual printer".into()),
        firmware: Some("Marlin 2.1.2.1 (Sprint Virtual Printer)".into()),
        extruders: 1,
        temps: TempReport {
            hotends: vec![HeaterReading {
                current: 214.6,
                target: Some(215.0),
                power: Some(0.42),
            }],
            bed: Some(HeaterReading {
                current: 60.1,
                target: Some(60.0),
                power: Some(0.18),
            }),
            ..TempReport::default()
        },
        position: Some(PositionReport {
            x: 104.3,
            y: 98.7,
            z: 8.4,
            e: 1234.56,
            stepper_counts: None,
        }),
        job: Some(job),
        fan: 255,
        feedrate_pct: 100.0,
        flow_pct: 100.0,
        ..PrinterView::default()
    };
    // Ten minutes of heating and holding, one sample a second.
    app.temps.points = (0..600)
        .map(|i| {
            let t = f64::from(i);
            let rise = |target: f32, rate: f64| {
                let x = (1.0 - (-t / rate).exp()) as f32;
                22.0 + (target - 22.0) * x + (t / 9.0).sin() as f32 * 0.4
            };
            crate::widgets::TempPoint {
                t,
                hotends: vec![(rise(215.0, 60.0), 215.0)],
                bed: Some((rise(60.0, 90.0), 60.0)),
                chamber: None,
            }
        })
        .collect();
    app.console = vec![
        ConsoleLine::info("Connecting to Virtual printer"),
        ConsoleLine::received("start"),
        ConsoleLine::received(
            "FIRMWARE_NAME:Marlin 2.1.2.1 (Github) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Sprint Virtual Printer EXTRUDER_COUNT:1",
        ),
        ConsoleLine::sent("G28"),
        ConsoleLine::received("ok"),
        ConsoleLine::sent("M105").with_auto(true),
        ConsoleLine::received("ok T:214.6 /215.0 B:60.1 /60.0 @:54 B@:23").with_auto(true),
        ConsoleLine::info("Printing calibration-cube.gcode"),
        ConsoleLine::warning("Resend: 1832 (checksum mismatch)"),
        ConsoleLine::sent("M117 Layer 42/100"),
        ConsoleLine::received("ok"),
    ];
    app.toast = None;
    app
}

#[test]
fn macros_ask_first_while_a_job_runs() {
    let mut app = printing_app();
    app.run_line("macro home");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("no question: {:?}", app.toast);
    };
    assert_eq!(confirm.action, dialogs::Action::RunMacro("Home".into()));
    assert!(confirm.danger, "asked as a dangerous action");
    assert!(!confirm.on_yes, "the safe answer is the default");
    assert!(
        confirm.message[0].contains("A job is running"),
        "{:?}",
        confirm.message
    );
    assert!(confirm.message.contains(&"G28".to_string()));
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    // A long macro shows its first lines.
    let long: String = (0..20)
        .map(|i| {
            format!(
                "G4 P{i}
"
            )
        })
        .collect();
    let preview = macro_preview(&long);
    assert_eq!(preview.len(), MACRO_PREVIEW_LINES + 1);
    assert_eq!(preview.last().map(String::as_str), Some("… 8 more lines"));
}

#[test]
fn turning_heaters_off_while_printing_asks_first() {
    let mut app = printing_app();
    // :cooldown during the print.
    app.run_line("cooldown");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("no question: {:?}", app.toast);
    };
    assert!(confirm.danger && !confirm.on_yes);
    assert!(
        confirm.message[0].starts_with("Hotend T0 and Bed off while calibration-cube.gcode"),
        "{:?}",
        confirm.message
    );
    assert!(matches!(&confirm.action, dialogs::Action::SetTempsAnyway(t) if t.len() == 2));
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    // The temperature dialog: Up from the first row is "All off".
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&app.modal, Some(Modal::Confirm(c)) if c.title == "Turn heaters off while printing?")
    );
    press(&mut app, KeyCode::Esc);
    // Raising a target or setting one that is already off does not ask.
    app.run_line("temp 220");
    assert!(app.modal.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text == "Hotend T0 220 °C")
    );
    app.view.temps.bed = None;
    app.run_line("bedtemp off");
    assert!(app.modal.is_none(), "the bed was not heating");
}

#[test]
fn printing_screen_at_120x40() {
    let mut app = printing_app();
    let buf = render(&mut app, 120, 40);
    crate::snapshot::save(&buf, "tui-printing-120x40");
    let text = screen_text(&buf);
    for fact in [
        "◆ Sprint",
        "● Connected",
        "Virtual printer",
        "Marlin 2.1.2.1",
        "▶ Printing 42.0%",
        "layer 42/100",
        "Temperatures",
        "T0",
        "Bed",
        "Position",
        "104.30",
        "Job",
        "calibration-cube.gcode",
        "Layer 42/100",
        "following the print",
        "Console",
        "M117 Layer 42/100",
        "G-code",
        "F1 Help",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
    // Automatic traffic is hidden by default.
    assert!(!text.contains("@:54"), "{text}");
}

#[test]
fn printing_screen_at_80x24() {
    let mut app = printing_app();
    let buf = render(&mut app, 80, 24);
    crate::snapshot::save(&buf, "tui-printing-80x24");
    let text = screen_text(&buf);
    for fact in [
        "Sprint",
        "Connected",
        "T0",
        "Bed",
        "Layer 42/100",
        "Console",
        "F1",
    ] {
        assert!(text.contains(fact), "missing {fact:?} in\n{text}");
    }
}

#[test]
fn printing_screen_wide() {
    let mut app = printing_app();
    let buf = render(&mut app, 170, 48);
    crate::snapshot::save(&buf, "tui-printing-170x48");
    let text = screen_text(&buf);
    assert!(
        text.contains("Console") && text.contains("Layer 42/100"),
        "{text}"
    );
}

#[test]
fn welcome_screens() {
    let mut app = app();
    let buf = render(&mut app, 120, 40);
    crate::snapshot::save(&buf, "tui-welcome-120x40");
    let text = screen_text(&buf);
    assert!(text.contains("S P R I N T"), "{text}");
    assert!(text.contains("open a G-code file"), "{text}");
    let buf = render(&mut app, 80, 24);
    crate::snapshot::save(&buf, "tui-welcome-80x24");
}

/// Every dialog and the jog overlay, at sizes down to a single cell.
#[test]
fn dialogs_draw_at_any_size() {
    use sprint_host::{HostPrompt, PortInfo, PrinterProfile, TempPreset};

    let profile = PrinterProfile::default();
    let port = PortInfo {
        name: "COM7".into(),
        description: "A rather long USB serial adapter description".into(),
        vid: Some(0x1A86),
        pid: Some(0x7523),
        manufacturer: None,
        serial_number: None,
        likely_printer: true,
    };
    let modals: Vec<Box<dyn Fn() -> Modal>> = vec![
        Box::new(|| Modal::Help { scroll: 3 }),
        Box::new(move || {
            Modal::Connect(dialogs::ConnectDialog::new(
                vec![port.clone()],
                true,
                &profile,
            ))
        }),
        Box::new(|| {
            Modal::Open(dialogs::OpenDialog::new(
                sample(""),
                vec![sample("calibration-cube.gcode")],
            ))
        }),
        Box::new(|| {
            Modal::Temps(dialogs::TempDialog::new(
                vec![
                    (Heater::Hotend(0), 21.0, 215.0),
                    (Heater::Hotend(1), f32::NAN, 0.0),
                    (Heater::Bed, 60.0, 60.0),
                ],
                vec![TempPreset {
                    name: "A very long preset name".into(),
                    hotend: 999.0,
                    bed: f32::INFINITY,
                }],
            ))
        }),
        Box::new(|| {
            Modal::Confirm(dialogs::Confirm::new(
                "Cancel the print?",
                vec!["x".repeat(200), String::new()],
                "Cancel print",
                "Keep printing",
                true,
                dialogs::Action::CancelJob,
            ))
        }),
        Box::new(|| {
            let lines = (0..300)
                .map(|i| ReportLine::plain(format!("line {i} {}", "w".repeat(i % 150))))
                .collect();
            let mut report = dialogs::ReportView::new("Report", lines);
            report.scroll = usize::MAX;
            report.pending = Some(dialogs::Pending::new(REPORT_TIMEOUT, false));
            Modal::Report(report)
        }),
        Box::new(|| {
            let mut prompt = dialogs::PromptDialog::new(HostPrompt {
                message: "m".repeat(300),
                buttons: (0..9).map(|i| format!("Button {i}")).collect(),
            });
            prompt.selected = 8;
            Modal::Prompt(prompt)
        }),
    ];
    let mut app = printing_app();
    let sizes = [
        (1, 1),
        (2, 2),
        (10, 3),
        (24, 8),
        (36, 10),
        (38, 12),
        (40, 12),
        (64, 15),
        (80, 24),
        (120, 40),
    ];
    for make in &modals {
        for (w, h) in sizes {
            app.modal = Some(make());
            let _ = render(&mut app, w, h);
        }
    }
    app.modal = None;
    app.jog = Some(JOG_STEPS.len() - 1);
    for (w, h) in sizes {
        let _ = render(&mut app, w, h);
    }
    // Narrow terminals have no left column: the layer view starts at x 0.
    for w in 30..=45 {
        let _ = render(&mut app, w, 20);
    }
}

#[test]
fn any_size_draws_without_panicking() {
    let mut app = printing_app();
    for (w, h) in [
        (1, 1),
        (10, 3),
        (23, 7),
        (24, 8),
        (40, 12),
        (63, 20),
        (64, 15),
        (200, 60),
        (300, 100),
    ] {
        let _ = render(&mut app, w, h);
    }
    app.modal = Some(Modal::Help { scroll: 0 });
    for (w, h) in [(1, 1), (30, 10), (120, 40)] {
        let _ = render(&mut app, w, h);
    }
}

// ----- key handling against the simulator -----

/// A slow job: many short real-time moves, so it is still running while
/// keys are pressed, and pausing (which waits for the planned moves) is
/// quick.
fn slow_file(dir: &std::path::Path) -> PathBuf {
    let mut gcode = String::from("G28\nG90\nM83\nG1 Z0.3 F600\n");
    for i in 0..400 {
        let x = 20 + (i % 2) * 5;
        gcode.push_str(&format!("G1 X{x} Y{} E0.2 F600\n", 20 + i % 5));
    }
    let path = dir.join("slow.gcode");
    std::fs::write(&path, gcode).expect("write slow file");
    path
}

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("sprint-tui-app-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An app connected to a simulator that moves in (fast) real time.
fn timed_app() -> App {
    let printer = Printer::new();
    let sim = SimConfig {
        boot_delay_ms: 20,
        time_scale: 5.0,
        instant_moves: false,
        ..SimConfig::default()
    };
    printer
        .connect(ConnectTarget::Simulator(sim), ConnectOptions::default())
        .expect("connect to the simulator");
    let mut app = App::new(printer, AppConfig::default(), None, Theme::dark());
    wait(&mut app, "the setup queries", |s| {
        s.is_connected() && s.settings_loaded && s.position.is_some()
    });
    wait_idle(&mut app);
    app
}

fn job_status(app: &App) -> Option<JobStatus> {
    app.printer
        .with_state(|s| s.job.as_ref().map(|j| j.status.clone()))
}

#[test]
fn f5_prints_the_loaded_file_to_completion() {
    let mut app = connected_app();
    // Without a file, F5 only explains.
    press(&mut app, KeyCode::F(5));
    assert!(app.modal.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("No file loaded"))
    );
    load(&mut app, "calibration-cube.gcode");
    received(&app);
    press(&mut app, KeyCode::F(5));
    assert!(
        matches!(app.modal, Some(Modal::Confirm(_))),
        "F5 asks first"
    );
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Start print?") && text.contains("calibration-cube.gcode"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-confirm-print-120x40");
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    wait(&mut app, "the print to start", |s| s.job.is_some());
    // The layer view follows the job.
    wait_app(&mut app, "the printing layer", |a| {
        a.printing_layer().is_some()
    });
    wait(&mut app, "the print to finish", |s| {
        s.job.as_ref().is_some_and(|j| j.status.is_finished())
    });
    assert_eq!(
        job_status(&app),
        Some(JobStatus::Completed),
        "{}",
        console_text(&app)
    );
    let got = received(&app);
    assert!(got.iter().any(|c| c == "G28"), "{got:?}");
    assert!(got.iter().any(|c| c.starts_with("G1 ")), "{got:?}");
    app.sync();
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.starts_with("Finished calibration-cube.gcode")),
        "{:?}",
        app.toast
    );
    // Once finished, the whole layer is drawn as printed.
    assert_eq!(app.progress_line(), None);
}

#[test]
fn f6_pauses_and_resumes_and_f7_cancels_after_asking() {
    let scratch = Scratch::new("pause");
    let mut app = timed_app();
    app.open_file(slow_file(&scratch.0));
    wait_app(&mut app, "the analysis", |a| a.loader.is_none());
    press(&mut app, KeyCode::F(5));
    press(&mut app, KeyCode::Char('y'));
    wait(&mut app, "printing", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 10)
    });
    // The layer view follows the job.
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("following the print"), "{text}");
    press(&mut app, KeyCode::F(6));
    wait(&mut app, "the pause", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Paused)
    });
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("‖ Paused"), "{text}");
    assert!(text.contains("F6 Resume"), "{text}");
    // A move typed while paused, still running when F6 is pressed: the
    // resume waits for it (the resume script would go first otherwise).
    wait_idle(&mut app);
    received(&app);
    app.send_gcode("G1 X60 F300");
    press(&mut app, KeyCode::F(6));
    assert!(app.resume_pending.is_some(), "{:?}", app.toast);
    wait(&mut app, "the resume", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing)
    });
    assert!(app.resume_pending.is_none());
    let got = received(&app);
    let at = |want: &str| got.iter().position(|c| c == want);
    assert!(
        at("G1 X60 F300").is_some_and(|m| at("G1 E2 F2700").is_some_and(|r| m < r)),
        "{got:?}"
    );
    // F7 asks, defaulting to "keep printing".
    press(&mut app, KeyCode::F(7));
    assert!(matches!(app.modal, Some(Modal::Confirm(_))));
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-confirm-cancel-120x40");
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    app.sync();
    assert_eq!(job_status(&app), Some(JobStatus::Printing));
    press(&mut app, KeyCode::F(7));
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Enter);
    wait(&mut app, "the cancel", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Cancelled)
    });
    // Connecting elsewhere or disconnecting stops a job: asked first.
    press(&mut app, KeyCode::F(5));
    press(&mut app, KeyCode::Char('y'));
    wait(&mut app, "printing again", |s| s.job_active());
    app.sync();
    app.run_line("disconnect");
    assert!(
        matches!(&app.modal, Some(Modal::Confirm(c)) if c.action == dialogs::Action::DisconnectAnyway)
    );
    press(&mut app, KeyCode::Enter);
    app.run_line("connect sim");
    assert!(
        matches!(&app.modal, Some(Modal::Confirm(c)) if matches!(c.action, dialogs::Action::ConnectAnyway(_)))
    );
    press(&mut app, KeyCode::Esc);
    app.sync();
    assert!(app.view.job_active(), "the job kept running");
    app.printer.cancel();
    wait(&mut app, "the second cancel", |s| !s.job_active());
    // Jogging is refused during a job.
    press(&mut app, KeyCode::F(5));
    press(&mut app, KeyCode::Char('y'));
    wait(&mut app, "printing again", |s| s.job_active());
    app.sync();
    press(&mut app, KeyCode::Char('j'));
    assert!(app.jog.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("disabled"))
    );
    // Quitting during the job asks; Enter keeps the app open.
    press(&mut app, KeyCode::F(10));
    assert!(matches!(app.modal, Some(Modal::Confirm(_))));
    press(&mut app, KeyCode::Enter);
    assert!(!app.quit);
    ctrl(&mut app, 'q');
    press(&mut app, KeyCode::Char('y'));
    assert!(app.quit);
    app.printer.cancel();
}

#[test]
fn jog_mode_ends_when_a_print_starts() {
    let mut app = connected_app();
    load(&mut app, "calibration-cube.gcode");
    press(&mut app, KeyCode::Char('j'));
    assert!(app.jog.is_some());
    // F5 works from jog mode; the job ends jog mode.
    press(&mut app, KeyCode::F(5));
    press(&mut app, KeyCode::Char('y'));
    wait_app(&mut app, "the job", |a| a.view.job.is_some());
    assert!(
        app.jog.is_none(),
        "jog mode outlived the start of the print"
    );
    assert!(!screen(&mut app, 120, 40).contains("step 10 mm"));
    app.printer.cancel();
    wait(&mut app, "the cancel", |s| !s.job_active());
}

#[test]
fn jog_mode_moves_and_homes() {
    let mut app = connected_app();
    received(&app);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.jog, Some(DEFAULT_JOG_STEP));
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Jog") && text.contains("step 10 mm"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-jog-120x40");
    press(&mut app, KeyCode::Right);
    press(&mut app, KeyCode::Char('+'));
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::PageUp);
    for _ in 0..4 {
        press(&mut app, KeyCode::Char('-'));
    }
    press(&mut app, KeyCode::PageDown);
    press(&mut app, KeyCode::Char('h'));
    press(&mut app, KeyCode::Esc);
    assert!(app.jog.is_none());
    wait_idle(&mut app);
    let got = received(&app);
    for want in [
        "G1 X10 F3000",
        "G1 Y50 F3000",
        "G1 Z10 F600",
        "G1 Z-0.1 F600",
        "G28",
    ] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    assert!(got.iter().filter(|c| *c == "G91").count() >= 4, "{got:?}");
    // Without a connection, jog mode does not start.
    let mut offline = super::tests::app();
    press(&mut offline, KeyCode::Char('j'));
    assert!(offline.jog.is_none());
}

#[test]
fn temperature_dialog_sets_targets() {
    let mut app = connected_app();
    received(&app);
    press(&mut app, KeyCode::Char('t'));
    assert!(matches!(app.modal, Some(Modal::Temps(_))));
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-temps-120x40");
    type_text(&mut app, "200");
    press(&mut app, KeyCode::Down);
    type_text(&mut app, "55");
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    wait(&mut app, "the targets", |s| {
        s.temps.hotends.first().and_then(|h| h.target) == Some(200.0)
            && s.temps.bed.as_ref().and_then(|b| b.target) == Some(55.0)
    });
    // The PLA preset (third row).
    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    wait(&mut app, "the preset", |s| {
        s.temps.hotends.first().and_then(|h| h.target) == Some(205.0)
            && s.temps.bed.as_ref().and_then(|b| b.target) == Some(60.0)
    });
    let got = received(&app);
    assert!(got.iter().any(|c| c == "M104 T0 S200"), "{got:?}");
    assert!(got.iter().any(|c| c == "M140 S55"), "{got:?}");
    assert!(got.iter().any(|c| c == "M104 T0 S205"), "{got:?}");
}

#[test]
fn typed_gcode_is_sent_and_completed() {
    let mut app = connected_app();
    received(&app);
    // An upper-case letter starts the G-code line.
    press(&mut app, KeyCode::Char('M'));
    assert_eq!(app.focus, Focus::Input);
    type_text(&mut app, "115");
    press(&mut app, KeyCode::Enter);
    wait_idle(&mut app);
    assert!(received(&app).iter().any(|c| c == "M115"));
    assert!(app.console.iter().any(|l| l.text.contains("FIRMWARE_NAME")));
    // Tab opens the completion popup.
    type_text(&mut app, "m10");
    press(&mut app, KeyCode::Tab);
    assert!(app.gcode_input.is_completing());
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("M104") && text.contains("Set hotend temperature"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-completion-120x40");
    press(&mut app, KeyCode::Esc);
    ctrl(&mut app, 'c');
    assert!(app.gcode_input.is_empty());
    // Dangerous codes ask first.
    type_text(&mut app, "M502");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Confirm(_))));
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("Send M502?"), "{text}");
    press(&mut app, KeyCode::Char('n'));
    wait_idle(&mut app);
    assert!(!received(&app).iter().any(|c| c == "M502"));
    // Esc leaves the line; Tab on an empty line moves the focus.
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Layers);
    press(&mut app, KeyCode::Char('i'));
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Layers);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Console);
}

/// Open the palette, type a line and run it.
fn run_palette(app: &mut App, line: &str) {
    press(app, KeyCode::Char(':'));
    assert_eq!(app.input_mode, InputMode::Command);
    type_text(app, line);
    press(app, KeyCode::Enter);
}

/// Wait until the open report has its data.
fn wait_report(app: &mut App, what: &str) {
    wait_app(
        app,
        what,
        |a| matches!(&a.modal, Some(Modal::Report(r)) if r.pending.is_none()),
    );
}

#[test]
fn command_palette_runs_shell_commands() {
    let mut app = connected_app();
    received(&app);
    for line in [
        "temp 205",
        "bedtemp 50",
        "fan 50",
        "home x",
        "speed 120",
        "set M92 E415",
    ] {
        run_palette(&mut app, line);
    }
    wait_idle(&mut app);
    let got = received(&app);
    for want in [
        "M104 T0 S205",
        "M140 S50",
        "M106 S128",
        "G28 X",
        "M220 S120",
    ] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    assert!(
        got.iter()
            .any(|c| c.starts_with("M92 ") && c.ends_with("E415")),
        "{got:?}"
    );
    // Reports open in a dialog and fill in when the printer answers.
    run_palette(&mut app, "settings");
    wait_report(&mut app, "the settings");
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("Steps per unit"), "{text}");
    assert!(text.contains("E415"), "{text}");
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-settings-120x40");
    press(&mut app, KeyCode::Esc);
    // Before probing there is no mesh; the report says so once the printer
    // has answered, without waiting for the timeout.
    let asked = Instant::now();
    run_palette(&mut app, "mesh");
    wait_report(&mut app, "the empty mesh");
    assert!(asked.elapsed() < REPORT_TIMEOUT);
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("No bed mesh reported"), "{text}");
    assert!(!text.contains("did not answer"), "{text}");
    press(&mut app, KeyCode::Esc);
    run_palette(&mut app, "mesh probe");
    wait_report(&mut app, "the probed mesh");
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Bed mesh") && text.contains("range"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-mesh-120x40");
    press(&mut app, KeyCode::Esc);
    run_palette(&mut app, "status");
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("● Connected"), "{text}");
    press(&mut app, KeyCode::Esc);
    run_palette(&mut app, "endstops");
    wait_report(&mut app, "the endstops");
    assert!(screen(&mut app, 120, 40).contains("x_min"));
    press(&mut app, KeyCode::Esc);
    // Errors show the usage.
    run_palette(&mut app, "temp 2hot");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.tone == Tone::Bad && t.text.contains("usage: temp"))
    );
    run_palette(&mut app, "hoem");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("did you mean 'home'"))
    );
    // Raw G-code goes through the palette too.
    run_palette(&mut app, "M114");
    wait_idle(&mut app);
    assert!(received(&app).iter().any(|c| c == "M114"));
    // Palette completion offers commands.
    press(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "se");
    press(&mut app, KeyCode::Tab);
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("settings") && text.contains("Firmware settings"),
        "{text}"
    );
}

#[test]
fn emergency_stop_without_a_printer_says_so() {
    let mut app = app();
    press(&mut app, KeyCode::F(8));
    press(&mut app, KeyCode::F(8));
    assert!(app.estop_armed.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.starts_with("Not connected")),
        "{:?}",
        app.toast
    );
    assert!(!screen(&mut app, 120, 40).contains("EMERGENCY"));
}

#[test]
fn emergency_stop_needs_a_second_press() {
    let mut app = connected_app();
    press(&mut app, KeyCode::F(8));
    assert!(app.estop_armed.is_some());
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("EMERGENCY STOP"), "{text}");
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-estop-120x40");
    // Any other key disarms.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.estop_armed.is_none());
    app.sync();
    assert!(!app.view.halted);
    // The arm expires.
    press(&mut app, KeyCode::F(8));
    app.tick(Instant::now() + ESTOP_WINDOW + Duration::from_millis(1));
    assert!(app.estop_armed.is_none());
    // Two presses stop the printer.
    press(&mut app, KeyCode::F(8));
    press(&mut app, KeyCode::F(8));
    wait(&mut app, "the halt", |s| s.halted);
    assert!(screen(&mut app, 120, 40).contains("HALTED"));
}

#[test]
fn host_prompts_are_answered() {
    let mut app = connected_app();
    received(&app);
    app.printer
        .sim_control()
        .expect("simulator")
        .trigger_filament_runout();
    wait_app(&mut app, "the prompt dialog", |a| {
        matches!(a.modal, Some(Modal::Prompt(_)))
    });
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("The printer asks"), "{text}");
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-prompt-120x40");
    // Someone typing G-code as the prompt appears does not answer it.
    type_text(&mut app, "G1 X10");
    press(&mut app, KeyCode::Enter);
    wait_idle(&mut app);
    assert!(matches!(app.modal, Some(Modal::Prompt(_))));
    assert!(!received(&app).iter().any(|c| c.starts_with("M876")));
    // A moment later Enter answers.
    if let Some(Modal::Prompt(p)) = &mut app.modal {
        p.shown -= dialogs::PROMPT_GRACE;
    }
    press(&mut app, KeyCode::Enter);
    wait_idle(&mut app);
    let got = received(&app);
    assert!(got.iter().any(|c| c.starts_with("M876 S")), "{got:?}");
}

#[test]
fn connect_dialog_reaches_the_simulator() {
    let mut app = app();
    press(&mut app, KeyCode::F(2));
    let Some(Modal::Connect(dialog)) = &mut app.modal else {
        panic!("no connect dialog");
    };
    // Real serial ports may be listed (enumeration only): pick the
    // virtual printer explicitly, never a port.
    let sim = dialog
        .items
        .iter()
        .position(|i| *i == dialogs::ConnectItem::Simulator)
        .expect("simulator entry");
    dialog.selected = sim;
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Connect") && text.contains("Virtual"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-connect-120x40");
    let Some(Modal::Connect(dialog)) = &app.modal else {
        panic!("no connect dialog");
    };
    assert_eq!(
        dialog.items[dialog.selected],
        dialogs::ConnectItem::Simulator
    );
    press(&mut app, KeyCode::Enter);
    wait(&mut app, "the connection", |s| s.is_connected());
    wait_app(&mut app, "the connect thread", |a| a.connecting.is_none());
    assert!(app.printer.sim_control().is_some());
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("● Connected"), "{text}");
    // F2 again offers Disconnect first.
    press(&mut app, KeyCode::F(2));
    let Some(Modal::Connect(dialog)) = &mut app.modal else {
        panic!("no connect dialog");
    };
    assert_eq!(dialog.items[0], dialogs::ConnectItem::Disconnect);
    dialog.selected = 0;
    press(&mut app, KeyCode::Enter);
    wait(&mut app, "the disconnect", |s| !s.status.is_open());
}

#[test]
fn a_connect_thread_that_dies_does_not_block_connecting() {
    let mut app = app();
    let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    app.connecting = Some(rx);
    drop(tx);
    app.tick(Instant::now());
    assert!(app.connecting.is_none());
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text == "Connecting stopped unexpectedly")
    );
}

#[test]
fn open_dialog_completes_and_loads() {
    let mut app = app();
    press(&mut app, KeyCode::F(3));
    assert!(matches!(app.modal, Some(Modal::Open(_))));
    let dir = sample("");
    let mut prefix = dir.display().to_string();
    if !prefix.ends_with(['/', '\\']) {
        prefix.push(std::path::MAIN_SEPARATOR);
    }
    app.on_paste(&format!("{prefix}calib"));
    press(&mut app, KeyCode::Tab);
    let Some(Modal::Open(dialog)) = &app.modal else {
        panic!("no open dialog");
    };
    assert!(
        dialog.input.text().ends_with("calibration-cube.gcode"),
        "{}",
        dialog.input.text()
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-open-120x40");
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    wait_app(&mut app, "the load", |a| a.loader.is_none());
    assert_eq!(
        app.file.as_ref().map(|f| f.name()),
        Some("calibration-cube.gcode")
    );
    // A missing file is reported, not loaded.
    press(&mut app, KeyCode::F(3));
    app.on_paste("no-such-file-77.gcode");
    press(&mut app, KeyCode::Enter);
    wait_app(&mut app, "the failed load", |a| a.loader.is_none());
    assert!(
        app.toast.as_ref().is_some_and(|t| t.tone == Tone::Bad),
        "{:?}",
        app.toast
    );
    assert_eq!(
        app.file.as_ref().map(|f| f.name()),
        Some("calibration-cube.gcode")
    );
}

#[test]
fn layer_keys_browse_and_follow() {
    let mut app = printing_app();
    assert_eq!(app.shown_layer(), 41);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.layer_view, Some(40));
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.layer_view, Some(50));
    press(&mut app, KeyCode::End);
    assert_eq!(app.shown_layer(), 99);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.shown_layer(), 99, "clamped");
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Layer 100/100") && text.contains("f follows the print"),
        "{text}"
    );
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(app.layer_view, None);
    assert_eq!(app.shown_layer(), 41);
    press(&mut app, KeyCode::Char('b'));
    assert_eq!(app.fit, LayerFit::Bed);
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-bed-fit-120x40");
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.center, Center::Stats);
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Statistics") && text.contains("Print time"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-stats-120x40");
    press(&mut app, KeyCode::Char('s'));
    assert_eq!(app.center, Center::Layers);
}

#[test]
fn console_keys_scroll_filter_and_search() {
    let mut app = printing_app();
    for i in 0..200 {
        app.console.push(ConsoleLine::received(format!("line {i}")));
    }
    let _ = render(&mut app, 120, 40);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Console);
    press(&mut app, KeyCode::PageUp);
    let _ = render(&mut app, 120, 40);
    assert!(!app.console_state.is_following());
    press(&mut app, KeyCode::End);
    assert!(app.console_state.is_following());
    press(&mut app, KeyCode::Char('a'));
    assert!(app.show_auto);
    assert!(screen(&mut app, 120, 40).contains("all traffic"));
    press(&mut app, KeyCode::Char('/'));
    type_text(&mut app, "line 19");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.search.as_deref(), Some("line 19"));
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("matches"), "{text}");
    press(&mut app, KeyCode::Char('n'));
    assert!(app.search.is_none());
    // The mouse wheel scrolls the console where it is drawn.
    let _ = render(&mut app, 120, 40);
    let area = app.console_area;
    app.handle_event(TermEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: area.x + 2,
        row: area.y + 2,
        modifiers: KeyModifiers::NONE,
    }));
    let _ = render(&mut app, 120, 40);
    assert!(!app.console_state.is_following());
    press(&mut app, KeyCode::Char('c'));
    assert!(app.console.is_empty());
}

#[test]
fn help_and_quit() {
    let mut app = app();
    press(&mut app, KeyCode::F(1));
    assert!(matches!(app.modal, Some(Modal::Help { .. })));
    let text = screen(&mut app, 120, 40);
    assert!(
        text.contains("Sprint keys and commands") && text.contains("EMERGENCY STOP"),
        "{text}"
    );
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-help-120x40");
    for _ in 0..200 {
        press(&mut app, KeyCode::Down);
    }
    let _ = screen(&mut app, 120, 40);
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    press(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.modal, Some(Modal::Help { .. })));
    press(&mut app, KeyCode::Char('?'));
    // Without a job, quitting needs no confirmation.
    press(&mut app, KeyCode::F(10));
    assert!(app.quit);
    let mut app = super::tests::app();
    ctrl(&mut app, 'c');
    assert!(app.quit);
}

#[test]
fn offline_commands_explain_instead_of_failing() {
    let mut app = app();
    for key in [
        KeyCode::F(5),
        KeyCode::F(6),
        KeyCode::F(7),
        KeyCode::Char('t'),
    ] {
        press(&mut app, key);
        assert!(app.modal.is_none(), "{key:?}");
        assert!(app.toast.is_some(), "{key:?}");
    }
    app.run_line("temp 200");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Not connected"))
    );
    // (Never a serial port here: the line must fail before connecting.)
    app.run_line("connect sim 9600 extra");
    assert!(app.toast.as_ref().is_some_and(|t| t.tone == Tone::Bad));
    app.run_line("connect sim 115200");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("only applies to serial ports"))
    );
    app.run_line("help G28");
    assert!(matches!(&app.modal, Some(Modal::Report(r)) if r.title == "G-code G28"));
    app.modal = None;
    app.run_line("help nothing-here");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("No help"))
    );
    // Listing the presets needs no printer (as in the shell).
    app.run_line("preheat");
    assert!(
        matches!(&app.modal, Some(Modal::Report(r)) if r.title == "Presets"),
        "{:?}",
        app.toast
    );
}

#[test]
fn palette_reports_open_as_dialogs() {
    let mut app = connected_app();
    load(&mut app, "calibration-cube.gcode");
    for (line, title, fact) in [
        ("info", "calibration-cube.gcode", "Slicer"),
        ("status", "Status", "Loaded"),
        ("firmware", "Firmware", "Marlin"),
        ("stats", "Statistics", "Round trip"),
        ("sd ls", "SD card", "card"),
        ("macro", "Macros", "Bed mesh"),
        ("preheat", "Presets", "PETG"),
        ("help temp", "Help: temp", "usage: temp"),
        ("help M92", "G-code M92", "Set steps per unit"),
        ("settings motion", "Settings", "Maximum feedrate"),
    ] {
        run_palette(&mut app, line);
        assert_eq!(app.focus, Focus::Layers, "the palette closes after {line}");
        wait_report(&mut app, line);
        let Some(Modal::Report(report)) = &app.modal else {
            panic!("{line}: no report");
        };
        assert_eq!(report.title, title, "{line}");
        let text = screen(&mut app, 120, 40);
        assert!(text.contains(fact), "{line}: missing {fact:?} in\n{text}");
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none(), "{line}");
    }
    // Serial ports are listed (enumeration only).
    run_palette(&mut app, "ports");
    assert!(matches!(&app.modal, Some(Modal::Report(r)) if r.title == "Serial ports"));
    press(&mut app, KeyCode::Esc);
    // A macro that asks first.
    received(&app);
    run_palette(&mut app, "macro bed");
    assert!(matches!(&app.modal, Some(Modal::Confirm(c)) if c.title == "Run Bed mesh?"));
    press(&mut app, KeyCode::Char('n'));
    run_palette(&mut app, "macro home");
    wait_idle(&mut app);
    let got = received(&app);
    assert!(got.iter().any(|c| c == "G28"), "{got:?}");
    assert!(!got.iter().any(|c| c == "G29"), "{got:?}");
}

/// Wait until the SD upload job ends and return what the printer got.
fn finish_upload(app: &mut App) -> Vec<String> {
    wait(app, "the upload", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.kind == JobKind::SdUpload && j.status.is_finished())
    });
    wait_idle(app);
    received(app)
}

#[test]
fn sd_uploads_read_files_off_the_ui_thread() {
    let mut app = connected_app();
    received(&app);
    // A file named on the line is read by a helper thread.
    let path = sample("calibration-cube.gcode");
    app.run_line(&format!("sd upload \"{}\" cube.gco", path.display()));
    assert_eq!(app.background.len(), 1, "{:?}", app.toast);
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.starts_with("Reading calibration-cube"))
    );
    wait_app(&mut app, "the upload message", |a| {
        a.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Uploading to the SD card as cube.gco"))
    });
    assert!(app.background.is_empty());
    let got = finish_upload(&mut app);
    assert!(
        got.iter().any(|c| c == "M28 cube.gco"),
        "{:?}",
        &got[..got.len().min(5)]
    );
    assert!(got.iter().any(|c| c.starts_with("M29")), "no M29");
    // A missing file fails with a message, not a freeze.
    app.run_line("sd upload no-such-file.gcode");
    wait_app(&mut app, "the failure", |a| {
        a.background.is_empty()
            && a.toast
                .as_ref()
                .is_some_and(|t| t.text.contains("no such file"))
    });
    // Without a file: the loaded one, named after it.
    load(&mut app, "calibration-cube.gcode");
    app.run_line("sd upload");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text == "Uploading to the SD card as CALIBRAT.GCO"),
        "{:?}",
        app.toast
    );
    let got = finish_upload(&mut app);
    assert!(
        got.iter().any(|c| c == "M28 CALIBRAT.GCO"),
        "{:?}",
        &got[..got.len().min(5)]
    );
}

#[test]
fn terminal_events_resize_paste_and_cancel_typing() {
    let mut app = app();
    app.dirty = false;
    app.handle_event(TermEvent::Resize(80, 24));
    assert!(app.dirty);
    // A paste lands in the G-code line.
    app.handle_event(TermEvent::Paste("G28 X".into()));
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.gcode_input.text(), "G28 X");
    // Ctrl+C clears a typed line instead of quitting.
    ctrl(&mut app, 'c');
    assert!(app.gcode_input.is_empty());
    assert!(!app.quit);
    // Search: Esc goes back to the panel the search started from.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, Focus::Console);
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.input_mode, InputMode::Search);
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.focus, Focus::Console);
    assert_eq!(app.input_mode, InputMode::Gcode);
    // Key releases are ignored.
    let mut release = KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE);
    release.kind = crossterm::event::KeyEventKind::Release;
    app.handle_event(TermEvent::Key(release));
    assert!(!app.quit);
    // An empty palette closes on Enter.
    press(&mut app, KeyCode::Char(':'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.focus, Focus::Console);
}

#[test]
fn the_print_confirmation_starts_the_file_it_named() {
    let mut app = connected_app();
    load(&mut app, "calibration-cube.gcode");
    press(&mut app, KeyCode::F(5));
    assert!(matches!(app.modal, Some(Modal::Confirm(_))));
    // Another file finishes loading while the question is open.
    load(&mut app, "arcs-test.gcode");
    press(&mut app, KeyCode::Enter);
    app.sync();
    assert!(
        app.printer.with_state(|s| s.job.is_none()),
        "printed a file the question did not name: {:?}",
        app.view.job.as_ref().map(|j| j.name.clone())
    );
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("press F5 again")),
        "{:?}",
        app.toast
    );
}

#[test]
fn a_pasted_block_sends_one_command_per_line_after_asking() {
    let mut app = connected_app();
    received(&app);
    // Several lines (as copied from a file or a web page): asked first,
    // then each line is its own command, never "G28 X M105".
    app.handle_event(TermEvent::Paste("G28 X\r\n; a comment\nM105\n".into()));
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("a pasted block asks first: {:?}", app.toast);
    };
    assert_eq!(confirm.title, "Send 2 pasted lines?");
    assert!(!confirm.danger);
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-confirm-paste-120x40");
    press(&mut app, KeyCode::Char('y'));
    wait_idle(&mut app);
    let got = received(&app);
    assert!(got.iter().any(|c| c == "G28 X"), "{got:?}");
    assert!(got.iter().any(|c| c == "M105"), "{got:?}");
    assert!(
        !got.iter().any(|c| c.contains("M105") && c.contains("G28")),
        "{got:?}"
    );
    assert!(app.gcode_input.is_empty(), "nothing was left on the line");
    // A block with a dangerous line starts on "Don't send".
    app.handle_event(TermEvent::Paste("G28\nM502\n".into()));
    assert!(matches!(&app.modal, Some(Modal::Confirm(c)) if c.danger));
    press(&mut app, KeyCode::Enter);
    wait_idle(&mut app);
    let got = received(&app);
    assert!(
        !got.iter().any(|c| c.starts_with("G28") || c == "M502"),
        "nothing was sent: {got:?}"
    );
    // One line with its line break still lands on the G-code line.
    app.handle_event(TermEvent::Paste("; header\nM115\r\n".into()));
    assert!(app.modal.is_none());
    assert_eq!(app.gcode_input.text(), "M115");
}

#[test]
fn the_statistics_use_the_bounds_measured_at_load() {
    // Measuring the part walks every segment: millions in a big file, too
    // slow for every frame. The panel shows what the load measured.
    let mut app = app();
    load(&mut app, "calibration-cube.gcode");
    let file = app.file.as_mut().expect("loaded");
    assert_eq!(file.part_bounds, crate::part_bounds(&file.toolpath));
    file.part_bounds = sprint_host::sprint_gcode::Bounds {
        min: [0.0, 0.0, 0.0],
        max: [123.0, 45.0, 6.0],
    };
    press(&mut app, KeyCode::Char('s'));
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("123.0 x 45.0 x 6.0 mm"), "{text}");
}

/// The question F10 asks while the view's job is of `kind`.
fn quit_question(kind: JobKind) -> Vec<String> {
    let mut app = printing_app();
    if let Some(job) = app.view.job.as_mut() {
        job.kind = kind;
        job.name = "CUBE.GCO".into();
    }
    press(&mut app, KeyCode::F(10));
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("quitting during a job asks first: {:?}", app.toast);
    };
    assert_eq!(confirm.action, dialogs::Action::Quit);
    let mut text = confirm.message.clone();
    // Disconnecting tells the same story.
    app.modal = None;
    app.run_line("disconnect");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("disconnecting during a job asks first: {:?}", app.toast);
    };
    text.extend(confirm.message.iter().cloned());
    text
}

#[test]
fn quitting_says_what_happens_to_the_job() {
    let host = quit_question(JobKind::HostPrint).join(" ");
    assert!(host.contains("cancels it first"), "{host}");
    // The printer prints from its card without Sprint: quitting does not
    // stop that print, and saying so would leave it running unwatched.
    let sd = quit_question(JobKind::SdPrint).join(" ");
    assert!(!sd.contains("stops"), "{sd}");
    assert!(sd.contains("SD card") && sd.contains("goes on"), "{sd}");
    let upload = quit_question(JobKind::SdUpload).join(" ");
    assert!(upload.contains("upload"), "{upload}");
}

#[test]
fn typed_moves_during_a_print_ask_first() {
    let mut app = printing_app();
    // Homing in the middle of the print would ruin it.
    app.send_gcode("G28");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("G28 during a print asks first: {:?}", app.toast);
    };
    assert!(confirm.danger && !confirm.on_yes);
    assert_eq!(confirm.title, "Send G28 during the job?");
    assert_eq!(confirm.action, dialogs::Action::Send("G28".into()));
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-confirm-move-120x40");
    press(&mut app, KeyCode::Esc);
    // What a print is tuned with goes straight out.
    app.send_gcode("M220 S110");
    assert!(app.modal.is_none());
    // While paused a move is fine, homing Z is not.
    if let Some(job) = app.view.job.as_mut() {
        job.status = JobStatus::Paused;
    }
    app.send_gcode("G1 Z5 F600");
    assert!(app.modal.is_none());
    app.send_gcode("G28");
    assert!(matches!(&app.modal, Some(Modal::Confirm(c)) if c.danger));
}

#[test]
fn loading_a_file_keeps_changes_made_to_the_configuration_meanwhile() {
    let scratch = Scratch::new("config");
    let path = scratch.0.join("config.toml");
    let mut app = App::new(
        Printer::new(),
        AppConfig::default(),
        Some(path.clone()),
        Theme::dark(),
    );
    // While the TUI runs, the file gains a macro (the GUI, a hand edit).
    let mut edited = AppConfig::default();
    edited.macros.push(sprint_host::Macro {
        name: "Added elsewhere".into(),
        gcode: "M117 hi".into(),
        color: None,
        confirm: false,
    });
    edited.save_to(&path).expect("save");
    load(&mut app, "arcs-test.gcode");
    let on_disk = AppConfig::load_from(&path).expect("load");
    assert!(
        on_disk.macros.iter().any(|m| m.name == "Added elsewhere"),
        "the recent file replaced the edited configuration"
    );
    assert_eq!(
        on_disk.recent_files.first(),
        Some(&sample("arcs-test.gcode"))
    );
    assert!(
        app.config
            .macros
            .iter()
            .any(|m| m.name == "Added elsewhere")
    );
}

#[test]
fn short_screens_keep_the_position_before_link_statistics() {
    let mut app = printing_app();
    // 18 rows: the temperatures, the position and the job fit; the link
    // statistics (and the overrides) do not.
    let text = screen(&mut app, 64, 18);
    assert!(
        text.contains("Position") && text.contains("X 104.30"),
        "{text}"
    );
    assert!(text.contains("Job"), "{text}");
    assert!(!text.contains("Link"), "{text}");
    // Taller screens keep the layout they had.
    for (w, h) in [(80, 24), (120, 40)] {
        let text = screen(&mut app, w, h);
        for panel in ["Temperatures", "Position", "Job", "Fan · Speed · Flow"] {
            assert!(
                text.contains(panel),
                "{panel} missing at {w}x{h}:
{text}"
            );
        }
    }
    assert!(screen(&mut app, 80, 24).contains("Link"));
}

#[test]
fn a_hidden_prompt_can_be_shown_again() {
    let mut app = printing_app();
    app.view.prompt = Some(sprint_host::HostPrompt {
        message: "Nozzle Parked".into(),
        buttons: vec!["Continue".into()],
    });
    app.sync_prompt();
    let Some(Modal::Prompt(prompt)) = &mut app.modal else {
        panic!("the prompt shows");
    };
    prompt.shown -= dialogs::PROMPT_GRACE;
    // Esc hides it; the printer still waits for an answer, so the screen
    // says so and p brings the question back.
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    app.sync_prompt();
    assert!(app.modal.is_none(), "a hidden prompt stays hidden");
    let text = screen(&mut app, 120, 40);
    assert!(text.contains("The printer asks"), "{text}");
    crate::snapshot::save(&render(&mut app, 120, 40), "tui-prompt-hidden-120x40");
    press(&mut app, KeyCode::Char('p'));
    assert!(
        matches!(&app.modal, Some(Modal::Prompt(p)) if p.prompt.message == "Nozzle Parked"),
        "{:?}",
        app.toast
    );
}

#[test]
fn key_hints_during_a_job_lead_with_its_controls() {
    let mut app = printing_app();
    let hints = app.hints();
    // Pause, cancel and the emergency stop first (narrow terminals show
    // only the first few), and nothing the job refuses.
    assert_eq!(
        &hints[..3],
        &[("F6", "Pause"), ("F7", "Cancel"), ("F8", "E-stop")]
    );
    for refused in [("F5", "Print"), ("j", "Jog")] {
        assert!(!hints.contains(&refused), "{hints:?}");
    }
    assert!(screen(&mut app, 64, 18).contains("F8 E-stop"));
    // Idle: printing and jogging are offered again.
    app.view.job = None;
    let hints = app.hints();
    assert!(hints.contains(&("F5", "Print")) && hints.contains(&("j", "Jog")));
}

#[test]
fn a_relative_startup_file_is_remembered_with_its_full_path() {
    // `sprint tui cube.gcode`: the recent file must not depend on the
    // directory the next session starts in.
    let mut app = app();
    app.base_dir = sample("");
    app.open_file(PathBuf::from("arcs-test.gcode"));
    wait_app(&mut app, "the analysis", |a| a.loader.is_none());
    let file = app.file.as_ref().expect("loaded");
    assert!(file.path.is_absolute(), "{}", file.path.display());
    assert_eq!(app.config.recent_files.first(), Some(&file.path));
}

#[test]
fn disconnecting_during_a_print_cancels_it_first() {
    let scratch = Scratch::new("disconnect-cancel");
    let mut app = timed_app();
    app.open_file(slow_file(&scratch.0));
    wait_app(&mut app, "the analysis", |a| a.loader.is_none());
    press(&mut app, KeyCode::F(5));
    press(&mut app, KeyCode::Char('y'));
    wait(&mut app, "printing", |s| {
        s.job
            .as_ref()
            .is_some_and(|j| j.status == JobStatus::Printing && j.current_line > 10)
    });
    let control = app.printer.sim_control().expect("simulator");
    control.clear_received();
    app.run_line("disconnect");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("disconnecting during a print asks first");
    };
    let text = confirm.message.join(" ");
    assert!(text.contains("heaters off"), "{text}");
    press(&mut app, KeyCode::Char('y'));
    // Closing the link alone would leave the printer heating: the print
    // is cancelled (its script turns the heaters off) before it closes.
    wait(&mut app, "the disconnect", |s| !s.status.is_open());
    assert_eq!(job_status(&app), Some(JobStatus::Cancelled));
    let got = control.received_commands();
    assert!(got.iter().any(|c| c == "M104 S0"), "{got:?}");
}

#[test]
fn a_held_f8_is_not_a_second_press() {
    use crossterm::event::KeyEventKind;
    let key = |kind: KeyEventKind| {
        let mut k = KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE);
        k.kind = kind;
        TermEvent::Key(k)
    };
    let mut app = connected_app();
    // Repeats reported as such (kitty keyboard protocol) never fire.
    app.handle_event(key(KeyEventKind::Press));
    app.handle_event(key(KeyEventKind::Repeat));
    app.sync();
    assert!(!app.view.halted && app.estop_armed.is_some());
    // The Windows console reports a held key as more presses, and every
    // release: once releases are seen, the second press needs one first.
    app.handle_event(key(KeyEventKind::Release));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.estop_armed.is_none());
    app.handle_event(key(KeyEventKind::Press));
    app.handle_event(key(KeyEventKind::Press));
    app.handle_event(key(KeyEventKind::Press));
    app.sync();
    assert!(!app.view.halted, "holding F8 sent the emergency stop");
    assert!(app.estop_armed.is_some());
    app.handle_event(key(KeyEventKind::Release));
    app.handle_event(key(KeyEventKind::Press));
    wait(&mut app, "the halt", |s| s.halted);
}

#[test]
fn the_palette_lists_and_switches_profiles() {
    let dir = std::env::temp_dir().join(format!("sprint-tui-profiles-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("config.toml");
    let mut config = AppConfig::default();
    // Loopback with nothing listening: never a serial port.
    config.profiles.push(sprint_host::PrinterProfile {
        name: "Bench".into(),
        connection: ConnectTarget::parse("tcp://127.0.0.1:1").expect("target"),
        ..sprint_host::PrinterProfile::default()
    });
    config.save_to(&path).expect("save");
    let mut app = App::new(Printer::new(), config, Some(path.clone()), Theme::dark());
    app.run_line("profile");
    let Some(Modal::Report(report)) = &app.modal else {
        panic!("no list: {:?}", app.toast);
    };
    assert_eq!(report.title, "Printer profiles");
    let text: Vec<String> = report.lines.iter().map(ReportLine::text).collect();
    assert!(
        text[1].contains("Bench") && text[1].contains("tcp://127.0.0.1:1"),
        "{text:?}"
    );
    app.modal = None;
    app.run_line("profile bench");
    assert_eq!(app.profile.name, "Bench");
    assert_eq!(app.printer.profile().name, "Bench");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("Using the profile Bench")),
        "{:?}",
        app.toast
    );
    assert_eq!(AppConfig::load_from(&path).expect("load").active_profile, 1);
    // --save stores the target in the profile before connecting.
    app.run_line("connect sim://?boot_delay_ms=20&time_scale=100&instant_moves=true --save");
    let on_disk = AppConfig::load_from(&path).expect("load");
    assert!(on_disk.profiles[1].connection.is_simulator());
    wait(&mut app, "the connection", PrinterState::is_connected);
    app.run_line("profile nobody");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("no profile 'nobody'")),
        "{:?}",
        app.toast
    );
    app.printer.disconnect();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_palette_runs_pronsole_commands() {
    let mut app = connected_app();
    received(&app);
    app.run_line("settemp pla");
    app.run_line("home e");
    app.run_line("tool 0");
    app.run_line("gettemp");
    let Some(Modal::Report(report)) = &app.modal else {
        panic!("no report: {:?}", app.toast);
    };
    assert_eq!(report.title, "Temperatures");
    app.modal = None;
    app.run_line("eta");
    assert!(
        app.toast
            .as_ref()
            .is_some_and(|t| t.text.contains("No job is running")),
        "{:?}",
        app.toast
    );
    app.run_line("ls");
    assert!(matches!(&app.modal, Some(Modal::Report(r)) if r.title == "SD card"));
    app.modal = None;
    // source reads the file off the UI thread, then asks.
    let dir = std::env::temp_dir().join(format!("sprint-tui-source-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("lines.txt");
    std::fs::write(&file, "; warm up\nbedtemp 55\nM105\n").expect("write");
    app.run_line(&format!("source {}", file.display()));
    wait_app(&mut app, "the question", |a| {
        matches!(&a.modal, Some(Modal::Confirm(_)))
    });
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("no question");
    };
    assert_eq!(
        confirm.action,
        dialogs::Action::RunLines(vec!["bedtemp 55".into(), "M105".into()])
    );
    let action = confirm.action.clone();
    app.modal = None;
    app.run_action(action);
    app.run_line("off");
    wait_idle(&mut app);
    let got = received(&app);
    for want in [
        "M104 T0 S205",
        "G92 E0",
        "T0",
        "M140 S55",
        "M105",
        "M140 S0",
        "M84",
    ] {
        assert!(got.iter().any(|c| c == want), "missing {want} in {got:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_cancel_question_fits_the_job() {
    let mut app = printing_app();
    app.run_line("cancel");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("no question: {:?}", app.toast);
    };
    assert_eq!(confirm.title, "Cancel the print?");
    assert_eq!(confirm.yes, "Cancel print");
    let text = confirm.message.join(" ");
    assert!(text.contains("The cancel script runs: M104 S0"), "{text}");
    assert!(text.contains("printed again from a line"), "{text}");
    assert!(!text.contains("cannot be resumed"), "{text}");
    // An upload: the partial file goes, no cancel script.
    app.modal = None;
    if let Some(job) = &mut app.view.job {
        job.kind = sprint_host::JobKind::SdUpload;
        job.name = "CUBE.GCO".into();
    }
    app.run_line("cancel");
    let Some(Modal::Confirm(confirm)) = &app.modal else {
        panic!("no question: {:?}", app.toast);
    };
    assert_eq!(confirm.title, "Cancel the upload?");
    assert_eq!(
        (confirm.yes.as_str(), confirm.no.as_str()),
        ("Cancel upload", "Keep uploading")
    );
    assert!(
        confirm
            .message
            .join(" ")
            .contains("partial file CUBE.GCO is deleted")
    );
    assert_eq!(confirm.action, dialogs::Action::CancelJob);
}

//! Shared helpers for the insight tab tests: a host that lends an
//! [`AppCtx`] to a tab every frame (from a simulator-backed printer or a
//! synthetic state), a realistic demo state and simulator helpers.
//!
//! Never connects to anything but the built-in simulator.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use sprint_gui::context::{AppAction, AppCtx, LoadedFile};
use sprint_gui::panels::{BedMeshTab, CalibrationTab, PrinterInfoTab, SdCardTab, TuningTab};
use sprint_gui::widgets::Toasts;
use sprint_host::sprint_proto::{
    BedMesh, PositionReport, SdFile, SettingCommand, StatsReport, TempReport, parse_firmware_line,
};
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    AppConfig, ConnectOptions, ConnectTarget, ConnectionStatus, LinkStats, Printer, PrinterState,
    TempSample,
};

/// Anything with the panel signature.
pub trait Panel {
    fn show(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>);
}

macro_rules! panel {
    ($($t:ty),*) => {$(
        impl Panel for $t {
            fn show(&mut self, ui: &mut egui::Ui, ctx: &mut AppCtx<'_>) {
                self.ui(ui, ctx);
            }
        }
    )*};
}
panel!(
    PrinterInfoTab,
    TuningTab,
    CalibrationTab,
    BedMeshTab,
    SdCardTab
);

/// Owns everything an [`AppCtx`] borrows.
pub struct Host<T> {
    pub tab: T,
    pub printer: Printer,
    /// A synthetic state shown instead of the printer's (for deterministic
    /// snapshots); `None` = the printer's live snapshot each frame.
    pub state: Option<PrinterState>,
    pub config: AppConfig,
    pub config_dirty: bool,
    pub toasts: Toasts,
    pub actions: Vec<AppAction>,
    pub file: Option<LoadedFile>,
    pub selected_line: Option<u32>,
    /// The state of the last frame.
    pub last_state: PrinterState,
}

impl<T: Panel + Default> Host<T> {
    /// A host with a disconnected printer and a synthetic state.
    pub fn synthetic(state: PrinterState) -> Host<T> {
        Host::new(Printer::new(), Some(state))
    }

    /// A host showing a (simulator) printer's live state.
    pub fn live(printer: Printer) -> Host<T> {
        Host::new(printer, None)
    }

    fn new(printer: Printer, state: Option<PrinterState>) -> Host<T> {
        Host {
            tab: T::default(),
            printer,
            state,
            config: AppConfig::default(),
            config_dirty: false,
            toasts: Toasts::default(),
            actions: Vec::new(),
            file: None,
            selected_line: None,
            last_state: PrinterState::default(),
        }
    }
}

impl<T: Panel> Host<T> {
    /// Draw the tab for one frame.
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        let state = match &self.state {
            Some(s) => s.clone(),
            None => self.printer.snapshot(),
        };
        let mut ctx = AppCtx {
            printer: &self.printer,
            state: &state,
            config: &mut self.config,
            config_dirty: &mut self.config_dirty,
            file: self.file.as_ref(),
            toasts: &mut self.toasts,
            actions: &mut self.actions,
            selected_line: self.selected_line,
        };
        self.tab.show(ui, &mut ctx);
        self.last_state = state;
    }
}

/// A fast simulator configuration.
pub fn fast_sim() -> SimConfig {
    SimConfig {
        boot_delay_ms: 20,
        time_scale: 40.0,
        instant_moves: true,
        ..SimConfig::default()
    }
}

/// Connect a printer to the simulator and wait for the setup queries.
pub fn sim_printer(config: SimConfig) -> Printer {
    let printer = Printer::new();
    printer
        .connect(
            ConnectTarget::Simulator(config),
            ConnectOptions {
                ok_timeout_secs: 3.0,
                handshake_timeout_secs: 10.0,
                ..ConnectOptions::default()
            },
        )
        .expect("connect to the simulator");
    wait_for(&printer, "the setup queries", |s| {
        s.is_connected() && s.settings_loaded && s.position.is_some()
    });
    printer
}

/// Poll the printer until `cond` holds (30 s at most).
pub fn wait_for(printer: &Printer, what: &str, cond: impl Fn(&PrinterState) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !printer.with_state(&cond) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run harness frames until `cond` holds on the printer (30 s at most).
pub fn run_until<T: Panel>(
    h: &mut Harness<'_, Host<T>>,
    what: &str,
    cond: impl Fn(&PrinterState) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        h.step();
        if h.state().printer.with_state(&cond) {
            h.step();
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait until the simulator received a command starting with `prefix`.
pub fn wait_for_command(printer: &Printer, prefix: &str) -> String {
    let sim = printer.sim_control().expect("simulator");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(cmd) = sim
            .received_commands()
            .into_iter()
            .find(|c| c.starts_with(prefix))
        {
            return cmd;
        }
        assert!(
            Instant::now() < deadline,
            "the simulator never received {prefix:?}; got {:?}",
            sim.received_commands()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Run harness frames until the simulator received a command starting
/// with `prefix` (for commands a tab sends from a later frame, like a
/// poll); 30 s at most.
pub fn step_until_command<T: Panel>(h: &mut Harness<'_, Host<T>>, prefix: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        h.step();
        let found = h.state().printer.sim_control().and_then(|sim| {
            sim.received_commands()
                .into_iter()
                .find(|c| c.starts_with(prefix))
        });
        if let Some(cmd) = found {
            return cmd;
        }
        assert!(
            Instant::now() < deadline,
            "the simulator never received {prefix:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the simulator received a command starting with `prefix`.
pub fn received(printer: &Printer, prefix: &str) -> bool {
    printer.sim_control().is_some_and(|sim| {
        sim.received_commands()
            .iter()
            .any(|c| c.starts_with(prefix))
    })
}

/// Forget the commands the simulator received so far.
pub fn clear_received(printer: &Printer) {
    if let Some(sim) = printer.sim_control() {
        sim.clear_received();
    }
}

/// Marlin 2.1's M503 report for the demo printer.
pub const DEMO_M503: &[&str] = &[
    "G21",
    "M149 C",
    "M200 S0 D1.75",
    "M92 X80.00 Y80.00 Z400.00 E93.00",
    "M203 X300.00 Y300.00 Z5.00 E25.00",
    "M201 X3000.00 Y3000.00 Z100.00 E10000.00",
    "M204 P3000.00 R3000.00 T3000.00",
    "M205 B20000.00 S0.00 T0.00 J0.013",
    "M206 X0.00 Y0.00 Z0.00",
    "M420 S1 Z10.00",
    "M145 S0 H180.00 B70.00 F0",
    "M145 S1 H240.00 B110.00 F0",
    "M301 P22.20 I1.08 D114.00",
    "M304 P92.03 I3.47 D608.21",
    "M413 S1",
    "M207 S3.00 W13.00 F2700.00 Z0.00",
    "M208 S0.00 W0.00 F480.00",
    "M209 S0",
    "M851 X10.00 Y10.00 Z-1.52",
    "M900 K0.22",
    "M906 X800 Y800 Z800",
    "M906 T0 E800",
    "M913 X100 Y100 Z3",
    "M913 T0 E30",
    "M569 S1 X Y Z",
    "M593 X F40.00 D0.15",
    "M593 Y F40.00 D0.15",
];

/// The demo settings.
pub fn demo_settings() -> sprint_host::sprint_proto::PrinterSettings {
    DEMO_M503
        .iter()
        .filter_map(|l| SettingCommand::parse(l))
        .collect()
}

/// A 5x5 bilinear mesh of a slightly domed bed, tilted up towards the
/// front right, with a little probing noise.
pub fn demo_mesh() -> BedMesh {
    let mut rows = Vec::new();
    for r in 0..5 {
        let mut row = Vec::new();
        for c in 0..5 {
            let (x, y) = (c as f32 / 4.0, r as f32 / 4.0);
            let tilt = 0.10 * x - 0.06 * y;
            let dome = 0.07 * (1.0 - 4.0 * ((x - 0.55).powi(2) + (y - 0.45).powi(2)));
            let noise = 0.006 * (13.0 * x + 7.0 * y).sin();
            row.push(((tilt + dome + noise - 0.05) * 1000.0).round() / 1000.0);
        }
        rows.push(row);
    }
    BedMesh::from_rows(rows, "bilinear")
}

/// Temperature history: a hotend heating to 210 °C and holding it, a bed
/// at 60 °C. `bed_swing` is the bed's oscillation amplitude (°C).
pub fn demo_history(bed_swing: f32) -> VecDeque<TempSample> {
    let mut history = VecDeque::new();
    for i in 0..=240 {
        let t = f64::from(i);
        let (hotend, target, power) = if i < 20 {
            (24.0, 0.0, 0.0)
        } else {
            let s = (i - 20) as f32;
            // First-order rise with a small overshoot, then a tiny ripple.
            let rise = 24.0 + 186.0 * (1.0 - (-s / 22.0).exp());
            let overshoot = 3.2 * (s / 70.0) * (-(s - 70.0).abs() / 18.0).exp();
            let ripple = 0.25 * (s / 9.0).sin();
            let temp = if s < 70.0 {
                rise + overshoot
            } else {
                210.0 + overshoot + ripple
            };
            let power = if s < 60.0 {
                1.0
            } else {
                0.42 + 0.05 * (s / 9.0).cos()
            };
            (temp.min(214.0), 210.0, power)
        };
        let bed = 60.0 + bed_swing * (t as f32 / 24.0 * std::f32::consts::TAU).sin();
        history.push_back(TempSample {
            t,
            hotends: vec![(hotend, target)],
            bed: Some((bed, 60.0)),
            chamber: None,
            hotend_power: vec![power],
            bed_power: Some(0.36 + 0.04 * (t as f32 / 24.0).sin()),
        });
    }
    history
}

/// A connected Marlin printer with everything reported.
pub fn demo_state() -> PrinterState {
    let firmware = parse_firmware_line(
        "FIRMWARE_NAME:Marlin 2.1.2.1 (Sprint Virtual) SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 MACHINE_TYPE:Sprint Virtual Printer EXTRUDER_COUNT:1 UUID:7f3a9c2e-5b1d-4e8a-9c3f-2d6b8e1a4f70",
    );
    let caps = [
        ("SERIAL_XON_XOFF", false),
        ("BINARY_FILE_TRANSFER", false),
        ("EEPROM", true),
        ("VOLUMETRIC", true),
        ("AUTOREPORT_POS", true),
        ("AUTOREPORT_TEMP", true),
        ("PROGRESS", false),
        ("PRINT_JOB", true),
        ("AUTOLEVEL", true),
        ("RUNOUT", true),
        ("Z_PROBE", true),
        ("LEVELING_DATA", true),
        ("BUILD_PERCENT", true),
        ("SOFTWARE_POWER", false),
        ("TOGGLE_LIGHTS", false),
        ("CASE_LIGHT_BRIGHTNESS", false),
        ("EMERGENCY_PARSER", true),
        ("HOST_ACTION_COMMANDS", true),
        ("PROMPT_SUPPORT", true),
        ("SDCARD", true),
        ("MULTI_VOLUME", false),
        ("SD_WRITE", true),
        ("AUTOREPORT_SD_STATUS", true),
        ("LONG_FILENAME", true),
        ("EXTENDED_M20", true),
        ("THERMAL_PROTECTION", true),
        ("ARCS", true),
        ("BABYSTEPPING", true),
        ("CHAMBER_TEMPERATURE", false),
        ("ADVANCED_OK", false),
    ];
    let history = demo_history(0.12);
    let last = history.back().cloned().unwrap_or_default();
    let temps = TempReport {
        hotends: vec![sprint_host::sprint_proto::HeaterReading {
            current: last.hotends[0].0,
            target: Some(210.0),
            power: Some(0.42),
        }],
        bed: Some(sprint_host::sprint_proto::HeaterReading {
            current: last.bed.map_or(60.0, |b| b.0),
            target: Some(60.0),
            power: Some(0.36),
        }),
        ..TempReport::default()
    };
    let mut mesh = demo_mesh();
    mesh.x_positions = None;
    PrinterState {
        status: ConnectionStatus::Connected,
        target: Some("Virtual printer".to_owned()),
        firmware,
        capabilities: caps.iter().map(|(n, on)| (n.to_string(), *on)).collect(),
        temps,
        temp_history: history,
        position: Some(PositionReport {
            x: 110.0,
            y: 105.5,
            z: 0.28,
            e: 1842.35,
            stepper_counts: Some(vec![8800, 8440, 112, 171_339]),
        }),
        settings: demo_settings(),
        settings_loaded: true,
        endstops: vec![
            ("x_min".to_owned(), false),
            ("y_min".to_owned(), false),
            ("z_min".to_owned(), true),
            ("z_probe".to_owned(), false),
            ("filament".to_owned(), false),
        ],
        mesh: Some(mesh),
        stats: Some(StatsReport {
            prints: Some(148),
            finished: Some(131),
            failed: Some(14),
            total_time: Some(1_131_000),
            longest_job: Some(68_400),
            filament_used_mm: Some(412_873.4),
        }),
        link: LinkStats {
            connected_secs: 4985.0,
            lines_sent: 48_211,
            bytes_sent: 1_261_820,
            bytes_received: 2_412_337,
            oks: 48_190,
            resends: 3,
            errors: 3,
            timeouts: 0,
            rtt_last_ms: 3.1,
            rtt_avg_ms: 2.8,
            rtt_max_ms: 41.0,
            queue_len: 0,
            in_flight: 1,
            lines_per_sec: 212.4,
        },
        sd: sprint_host::SdState {
            card_present: Some(true),
            files: vec![
                SdFile {
                    name: "CALIBR~1.GCO".to_owned(),
                    long_name: Some("calibration_cube.gcode".to_owned()),
                    size: Some(48_213),
                },
                SdFile {
                    name: "BENCHY~1.GCO".to_owned(),
                    long_name: Some("benchy_0.2mm_PLA.gcode".to_owned()),
                    size: Some(1_843_271),
                },
                SdFile {
                    name: "FIRSTL~1.GCO".to_owned(),
                    long_name: Some("first layer test.gcode".to_owned()),
                    size: Some(12_630),
                },
                SdFile {
                    name: "VASE.GCO".to_owned(),
                    long_name: None,
                    size: Some(734_120),
                },
            ],
            ..sprint_host::SdState::default()
        },
        firmware_messages: [
            "Marlin 2.1.2.1",
            "Last Updated: 2023-07-29 | Author: (Sprint, virtual)",
            "Compiled: Jul 29 2023",
            "Free Memory: 4813  PlannerBufferBytes: 1232",
            "SD card ok",
            "Bed Leveling ON",
            "Fade Height 10.00",
            "Settings Stored (712 bytes; crc 42391)",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        ..PrinterState::default()
    }
}

/// Snapshot name with the theme: "insight_printer_dark".
pub fn themed(base: &str, theme: sprint_gui::theme::Theme) -> String {
    format!("{base}_{}", theme.name().to_lowercase())
}

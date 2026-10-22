//! Shared helpers for the controls sidebar and temperature graph tests: a
//! rig that owns everything an [`AppCtx`] borrows, synthetic printer
//! states, and a simulator connection.
//!
//! Only the simulator is ever connected: a real printer is attached to the
//! development machine and must never be opened by tests.

#![allow(dead_code)]

use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Rect, Vec2};
use egui_kittest::{Harness, kittest::By};
use sprint_gui::{
    context::{AppAction, AppCtx},
    panels::{ControlsSidebar, TemperatureGraphTab},
    theme::Theme,
    widgets::Toasts,
};
use sprint_host::sprint_proto::{
    FirmwareFlavor, FirmwareInfo, HeaterReading, PositionReport, TempReport,
};
use sprint_host::sprint_sim::{SimConfig, SimControl};
use sprint_host::{
    AppConfig, Axes, ConnectOptions, ConnectTarget, ConnectionStatus, JobInfo, JobKind, JobStatus,
    Printer, PrinterState, TempSample,
};

#[path = "../common/mod.rs"]
mod common;

#[allow(unused_imports)]
pub use common::{click_at, hover, settle, snapshot};

/// Generous upper bound for anything a test waits for.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// Everything the panels borrow through an [`AppCtx`].
pub struct Rig {
    pub sidebar: ControlsSidebar,
    pub graph: TemperatureGraphTab,
    pub printer: Printer,
    /// A fixed state to show instead of the printer's snapshot.
    pub state: Option<PrinterState>,
    /// Applied to every snapshot of the printer's state (e.g. a fake job).
    pub tweak: Option<fn(&mut PrinterState)>,
    pub config: AppConfig,
    pub dirty: bool,
    pub toasts: Toasts,
    pub actions: Vec<AppAction>,
    /// Where the last frame's content ended (for fitting the window).
    pub content: Rect,
}

impl Rig {
    /// A rig with a printer that is not connected.
    pub fn offline() -> Rig {
        Rig {
            sidebar: ControlsSidebar::default(),
            graph: TemperatureGraphTab::default(),
            printer: Printer::new(),
            state: None,
            tweak: None,
            config: AppConfig::default(),
            dirty: false,
            toasts: Toasts::default(),
            actions: Vec::new(),
            content: Rect::NOTHING,
        }
    }

    /// A rig showing a fixed state (its printer is not connected, so
    /// nothing is ever sent).
    pub fn synthetic(state: PrinterState) -> Rig {
        Rig {
            state: Some(state),
            ..Rig::offline()
        }
    }

    /// A rig connected to a fast simulator, idle and with the setup
    /// traffic cleared.
    pub fn simulator() -> Rig {
        Rig::simulator_with(fast_sim())
    }

    /// Like [`Rig::simulator`] with a custom simulator.
    pub fn simulator_with(config: SimConfig) -> Rig {
        let printer = Printer::new();
        printer
            .connect(ConnectTarget::Simulator(config), ConnectOptions::default())
            .expect("connect to the simulator");
        wait_for(&printer, "the setup queries", |s| {
            s.is_connected() && s.settings_loaded && s.position.is_some()
        });
        wait_idle(&printer);
        sim(&printer).clear_received();
        Rig {
            printer,
            ..Rig::offline()
        }
    }

    /// The state the panels see this frame.
    pub fn frame_state(&self) -> PrinterState {
        let mut state = match &self.state {
            Some(s) => s.clone(),
            None => self.printer.snapshot(),
        };
        if let Some(tweak) = self.tweak {
            tweak(&mut state);
        }
        state
    }

    fn with_ctx(
        &mut self,
        f: impl FnOnce(&mut AppCtx<'_>, &mut ControlsSidebar, &mut TemperatureGraphTab),
    ) {
        let state = self.frame_state();
        let mut ctx = AppCtx {
            printer: &self.printer,
            state: &state,
            config: &mut self.config,
            config_dirty: &mut self.dirty,
            file: None,
            toasts: &mut self.toasts,
            actions: &mut self.actions,
            selected_line: None,
        };
        f(&mut ctx, &mut self.sidebar, &mut self.graph);
    }

    /// Draw the sidebar (and the toasts) into `ui`.
    pub fn draw_sidebar(&mut self, ui: &mut egui::Ui) {
        self.with_ctx(|ctx, sidebar, _| sidebar.ui(ui, ctx));
        self.content = ui.min_rect();
        self.toasts.show(ui.ctx());
    }

    /// Draw the temperature graph tab into `ui`.
    pub fn draw_graph(&mut self, ui: &mut egui::Ui) {
        self.with_ctx(|ctx, _, graph| graph.ui(ui, ctx));
        self.content = ui.min_rect();
        self.toasts.show(ui.ctx());
    }

    /// The simulator's control handle.
    pub fn sim(&self) -> SimControl {
        sim(&self.printer)
    }
}

/// A sidebar harness `width` points wide whose height fits the content.
pub fn sidebar(theme: Theme, width: f32, rig: Rig) -> Harness<'static, Rig> {
    let mut h = common::harness_state(theme, (width, 4000.0), rig, |ui, rig| {
        rig.draw_sidebar(ui);
    });
    fit_height(&mut h, width);
    h
}

/// A sidebar harness of a fixed size (tall enough for interaction tests).
pub fn sidebar_fixed(theme: Theme, size: impl Into<Vec2>, rig: Rig) -> Harness<'static, Rig> {
    common::harness_state(theme, size, rig, |ui, rig| rig.draw_sidebar(ui))
}

/// A graph tab harness of a fixed size.
pub fn graph(theme: Theme, size: impl Into<Vec2>, rig: Rig) -> Harness<'static, Rig> {
    common::harness_state(theme, size, rig, |ui, rig| rig.draw_graph(ui))
}

/// Fit the window to the drawn content (twice: wrapping may change). The
/// window first grows tall: the sidebar scrolls, so content taller than the
/// window would be measured at the window's height.
pub fn fit_height(h: &mut Harness<'_, Rig>, width: f32) {
    h.set_size(Vec2::new(width, 4000.0));
    settle(h);
    for _ in 0..2 {
        let content = h.state().content;
        if content.is_positive() {
            h.set_size(Vec2::new(width, (content.max.y + 24.0).ceil()));
            settle(h);
        }
    }
}

/// Run frames until `cond` holds for the harness state (or panic).
pub fn run_until(h: &mut Harness<'_, Rig>, what: &str, cond: impl Fn(&Rig) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !cond(h.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A query for the node named `label` that never asks a text field for its
/// name. Narrow heater cards label their target field by a node that is
/// not in the AccessKit tree (a design-system widget detail), and
/// `accesskit_consumer` panics computing that field's name, which plain
/// label queries do for every node.
pub fn by_label(label: &str) -> By<'_> {
    By::new().predicate(move |n| {
        let name = match n.role() {
            Role::TextInput | Role::MultilineTextInput => return false,
            Role::Label => n.value(),
            _ => n.label(),
        };
        name.as_deref() == Some(label)
    })
}

/// Like [`by_label`], matching names that contain `text`.
pub fn by_label_contains(text: &str) -> By<'_> {
    By::new().predicate(move |n| {
        let name = match n.role() {
            Role::TextInput | Role::MultilineTextInput => return false,
            Role::Label => n.value(),
            _ => n.label(),
        };
        name.is_some_and(|name| name.contains(text))
    })
}

/// Take the pointer off the window (no hover highlight or cursor in a
/// snapshot) and let the UI settle.
pub fn pointer_away(h: &mut Harness<'_, Rig>) {
    h.event(egui::Event::PointerGone);
    settle(h);
}

/// A fast simulator: short boot, instant moves, accelerated heating.
pub fn fast_sim() -> SimConfig {
    SimConfig {
        boot_delay_ms: 20,
        time_scale: 20.0,
        instant_moves: true,
        ..SimConfig::default()
    }
}

/// The simulator's control handle.
pub fn sim(printer: &Printer) -> SimControl {
    printer.sim_control().expect("connected to the simulator")
}

/// Poll the printer until `cond` holds.
pub fn wait_for(printer: &Printer, what: &str, cond: impl Fn(&PrinterState) -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !printer.with_state(&cond) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Home every axis of the (simulated) printer and forget the traffic:
/// moves and babysteps then go out without asking first.
pub fn home(printer: &Printer) {
    printer.home(Axes::ALL);
    wait_for(printer, "the homing", |s| s.homed == Some(Axes::ALL));
    wait_idle(printer);
    sim(printer).clear_received();
}

/// Wait until nothing is queued or in flight.
pub fn wait_idle(printer: &Printer) {
    let deadline = Instant::now() + PATIENCE;
    let mut quiet = 0;
    while quiet < 3 {
        let idle = printer.with_state(|s| s.link.in_flight == 0 && s.link.queue_len == 0);
        quiet = if idle { quiet + 1 } else { 0 };
        assert!(Instant::now() < deadline, "the link never became idle");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Run `f` with the panels showing the printer's state as it is now, held
/// still (and laid out once) until `f` returns. Live readouts change the
/// layout as the printer reports, and a report arriving between finding a
/// widget and clicking it would move the widget from under the pointer:
/// find and click inside `f`. Commands still go to the live printer. A
/// fixed state stays as it is.
pub fn frozen<R>(h: &mut Harness<'_, Rig>, f: impl FnOnce(&mut Harness<'_, Rig>) -> R) -> R {
    if h.state().state.is_some() {
        return f(h);
    }
    let now = h.state().printer.snapshot();
    h.state_mut().state = Some(now);
    h.step();
    let result = f(h);
    h.state_mut().state = None;
    result
}

/// Click where `find` says, on one layout ([`frozen`]).
pub fn click_found(h: &mut Harness<'_, Rig>, find: impl FnOnce(&Harness<'_, Rig>) -> egui::Pos2) {
    frozen(h, |h| {
        let pos = find(h);
        click_at(h, pos);
    });
}

/// Commands the simulator executed, without automatic temperature traffic.
pub fn received(printer: &Printer) -> Vec<String> {
    sim(printer)
        .received_commands()
        .into_iter()
        .filter(|c| !c.starts_with("M105") && !c.starts_with("M155"))
        .collect()
}

/// Wait until the simulator executed `expected` (in order, as a
/// contiguous run) and return everything it received.
pub fn wait_for_commands(printer: &Printer, expected: &[&str]) -> Vec<String> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let got = received(printer);
        if contains_run(&got, expected) {
            return got;
        }
        assert!(
            Instant::now() < deadline,
            "the simulator never received {expected:?}; got {got:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// `expected` appears in `got` as a contiguous run.
pub fn contains_run(got: &[String], expected: &[&str]) -> bool {
    expected.is_empty()
        || got
            .windows(expected.len())
            .any(|w| w.iter().zip(expected).all(|(a, b)| a == b))
}

/// A heating curve sample: first-order approach from `start` to `target`.
fn approach(start: f32, target: f32, t: f32, tau: f32) -> f32 {
    target + (start - target) * (-t / tau).exp()
}

/// A connected, homed Marlin printer mid-way through heating up for PLA,
/// with a few minutes of history, a known position and a running part fan.
pub fn heating_state() -> PrinterState {
    let mut state = PrinterState {
        status: ConnectionStatus::Connected,
        target: Some("sim://".into()),
        firmware: Some(FirmwareInfo {
            name: "Marlin 2.1.2.1".into(),
            version: Some("2.1.2.1".into()),
            flavor: FirmwareFlavor::Marlin,
            extruder_count: Some(1),
            ..FirmwareInfo::default()
        }),
        capabilities: vec![
            ("EEPROM".into(), true),
            ("AUTOREPORT_TEMP".into(), true),
            ("BABYSTEPPING".into(), true),
        ],
        position: Some(PositionReport {
            x: 110.0,
            y: 110.0,
            z: 5.2,
            e: 0.0,
            stepper_counts: None,
        }),
        homed: Some(Axes::ALL),
        feedrate_pct: 100.0,
        flow_pct: vec![100.0],
        fan_speeds: vec![128],
        ..PrinterState::default()
    };
    for i in 0..=240 {
        let t = i as f32;
        let hot = approach(24.0, 215.0, t, 38.0) + 0.4 * (t * 0.35).sin();
        let bed = approach(22.0, 60.0, t, 70.0);
        state.temp_history.push_back(TempSample {
            t: f64::from(t),
            hotends: vec![(hot, 215.0)],
            bed: Some((bed, 60.0)),
            chamber: None,
            hotend_power: vec![((215.0 - hot) / 60.0).clamp(0.08, 1.0)],
            bed_power: Some(((60.0 - bed) / 20.0).clamp(0.05, 1.0)),
        });
    }
    state.temps = TempReport {
        hotends: vec![HeaterReading {
            current: 212.4,
            target: Some(215.0),
            power: Some(0.64),
        }],
        bed: Some(HeaterReading {
            current: 59.8,
            target: Some(60.0),
            power: Some(0.12),
        }),
        ..TempReport::default()
    };
    state
}

/// A cold, idle, connected printer (fresh after connecting: whether it
/// is homed is not known).
pub fn cold_state() -> PrinterState {
    let mut state = heating_state();
    state.homed = None;
    state.temp_history.clear();
    for i in 0..60 {
        state.temp_history.push_back(TempSample {
            t: f64::from(i),
            hotends: vec![(24.0, 0.0)],
            bed: Some((22.5, 0.0)),
            hotend_power: vec![0.0],
            bed_power: Some(0.0),
            ..TempSample::default()
        });
    }
    state.temps = TempReport {
        hotends: vec![HeaterReading {
            current: 24.0,
            target: Some(0.0),
            power: Some(0.0),
        }],
        bed: Some(HeaterReading {
            current: 22.5,
            target: Some(0.0),
            power: Some(0.0),
        }),
        ..TempReport::default()
    };
    state.fan_speeds = vec![0];
    state
}

/// A PID-like heat-up: approach `target` from `start` from time `t0` with
/// a damped overshoot.
fn heat(start: f32, target: f32, t: f32, t0: f32, tau: f32) -> f32 {
    if t < t0 {
        return start;
    }
    let dt = t - t0;
    let base = approach(start, target, dt, tau);
    let overshoot = (target - start) * 0.035 * (dt / tau).min(6.0) * (-dt / (tau * 1.6)).exp();
    base + overshoot * (dt / (tau * 0.9)).sin().max(0.0)
}

/// Ten minutes of history: the bed heats to 60, the hotend to 215 with a
/// little overshoot, and later to 230; optionally a second hotend and a
/// chamber. Deterministic (a fixed pseudo-noise).
pub fn graph_state(two_hotends: bool, chamber: bool) -> PrinterState {
    let mut state = heating_state();
    state.temp_history.clear();
    let noise = |i: u32| ((i.wrapping_mul(2_654_435_761) >> 16) % 1000) as f32 / 1000.0 - 0.5;
    for i in 0..=600u32 {
        let t = i as f32;
        let (hot_target, hot) = if t < 420.0 {
            (
                if t >= 30.0 { 215.0 } else { 0.0 },
                heat(24.0, 215.0, t, 30.0, 34.0),
            )
        } else {
            (230.0, heat(215.0, 230.0, t, 420.0, 20.0))
        };
        let hot = hot + 0.25 * noise(i);
        let bed = heat(22.0, 60.0, t, 0.0, 75.0) + 0.1 * noise(i + 7);
        let mut hotends = vec![(hot, hot_target)];
        let mut power = vec![
            ((hot_target - hot) / 50.0 + 0.32).clamp(0.0, 1.0)
                * f32::from(u8::from(hot_target > 0.0)),
        ];
        if two_hotends {
            let target = if t >= 200.0 { 240.0 } else { 0.0 };
            let second = heat(25.0, 240.0, t, 200.0, 40.0) + 0.25 * noise(i + 3);
            hotends.push((second, target));
            power.push(
                ((target - second) / 50.0 + 0.35).clamp(0.0, 1.0)
                    * f32::from(u8::from(target > 0.0)),
            );
        }
        state.temp_history.push_back(TempSample {
            t: f64::from(t),
            hotends,
            bed: Some((bed, 60.0)),
            chamber: chamber.then(|| (heat(21.0, 40.0, t, 60.0, 160.0), 40.0)),
            hotend_power: power,
            bed_power: Some(((60.0 - bed) / 15.0 + 0.18).clamp(0.0, 1.0)),
        });
    }
    if let Some(last) = state.temp_history.back() {
        state.temps.hotends = last
            .hotends
            .iter()
            .zip(&last.hotend_power)
            .map(|(&(current, target), &power)| HeaterReading {
                current,
                target: Some(target),
                power: Some(power),
            })
            .collect();
        state.temps.chamber = last.chamber.map(|(current, target)| HeaterReading {
            current,
            target: Some(target),
            power: None,
        });
    }
    state
}

/// Mark `state` as printing a host job.
pub fn printing(state: &mut PrinterState) {
    let mut job = JobInfo::new(JobKind::HostPrint, "benchy.gcode", 120_000, 4_000_000);
    job.status = JobStatus::Printing;
    job.current_line = 30_000;
    job.progress = 0.25;
    state.job = Some(job);
    // Its start G-code homed.
    state.homed = Some(Axes::ALL);
}

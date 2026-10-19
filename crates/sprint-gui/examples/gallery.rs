//! The Sprint widget gallery: every widget of the design system, in the
//! dark and light themes side by side, wired to live demo state.
//!
//! ```text
//! cargo run -p sprint-gui --example gallery
//! ```
//!
//! Set `SPRINT_GALLERY_FRAMES=N` to close the window by itself after N
//! frames (for smoke tests). The gallery never touches a printer.

use eframe::egui;
use sprint_gui::{
    icons,
    theme::{self, HeaterKind, Theme, Tone},
    widgets::*,
};

/// Which theme(s) the gallery shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GalleryView {
    /// Dark only.
    Dark,
    /// Light only.
    Light,
    /// Both, side by side.
    Both,
}

/// A simulated heater for the temperature cards.
#[derive(Clone, Debug)]
pub struct DemoHeater {
    current: f32,
    target: f32,
    power: f32,
    history: Vec<f32>,
}

impl DemoHeater {
    fn new(start: f32, target: f32, settled: f32) -> Self {
        // A first-order warm-up from `start` that has reached `settled`.
        let tau = 18.0;
        let history: Vec<f32> = (0..90)
            .map(|i| target - (target - start) * (-(i as f32) / tau).exp())
            .map(|v| v.min(settled.max(start)))
            .collect();
        let current = history.last().copied().unwrap_or(start);
        DemoHeater {
            current,
            target,
            power: ((target - current) / 25.0).clamp(0.08, 1.0),
            history,
        }
    }

    /// Approach the target with a ~6 s time constant.
    fn tick(&mut self, dt: f32, sample: bool) {
        let ambient = 24.0;
        let goal = if self.target > 0.0 {
            self.target
        } else {
            ambient
        };
        self.current += (goal - self.current) * (1.0 - (-dt / 6.0).exp());
        self.power = if self.target > 0.0 {
            ((self.target - self.current) / 25.0).clamp(0.08, 1.0)
        } else {
            0.0
        };
        if sample {
            self.history.push(self.current);
            if self.history.len() > 120 {
                self.history.remove(0);
            }
        }
    }

    fn apply(&mut self, action: TemperatureAction) {
        match action {
            TemperatureAction::SetTarget(t) => self.target = t,
            TemperatureAction::Off => self.target = 0.0,
        }
    }
}

/// Which confirmation dialog is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenDialog {
    Reset,
    Print,
}

/// The gallery state.
pub struct Gallery {
    /// The theme layout.
    pub view: GalleryView,
    autoscroll: bool,
    travel: bool,
    view3d: u8,
    length: f32,
    speed: f32,
    steps_x: f64,
    steps_z: f64,
    feedrate: f64,
    z_offset: f64,
    fan: u8,
    hotend: DemoHeater,
    bed: DemoHeater,
    progress: f32,
    mesh: sprint_proto::BedMesh,
    toasts: Toasts,
    dialog: Option<OpenDialog>,
    log: Vec<String>,
    sample_clock: f32,
    frames_left: Option<u32>,
}

impl Default for Gallery {
    fn default() -> Self {
        Gallery::new()
    }
}

impl Gallery {
    /// The gallery with deterministic demo data (no animation until
    /// [`Gallery::tick`] runs).
    pub fn new() -> Self {
        Gallery {
            view: GalleryView::Both,
            autoscroll: true,
            travel: false,
            view3d: 0,
            length: 10.0,
            speed: 5.0,
            steps_x: 80.0,
            steps_z: 415.5,
            feedrate: 1500.0,
            z_offset: f64::NAN,
            fan: 0,
            hotend: DemoHeater::new(24.0, 215.0, 193.0),
            bed: DemoHeater::new(22.0, 60.0, 59.6),
            progress: 0.42,
            mesh: demo_mesh(),
            toasts: Toasts::new().anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0)),
            dialog: None,
            log: vec!["Ready".to_owned()],
            sample_clock: 0.0,
            frames_left: None,
        }
    }

    /// Advance the simulated heaters and the job by `dt` seconds.
    pub fn tick(&mut self, dt: f32) {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, 0.1)
        } else {
            0.0
        };
        self.sample_clock += dt;
        let sample = self.sample_clock >= 0.5;
        if sample {
            self.sample_clock = 0.0;
        }
        self.hotend.tick(dt, sample);
        self.bed.tick(dt, sample);
        self.progress = (self.progress + dt / 600.0) % 1.0;
    }

    fn record(&mut self, text: impl Into<String>) {
        self.log.push(text.into());
        if self.log.len() > 6 {
            self.log.remove(0);
        }
    }

    /// The whole gallery (toolbar, both themes, dialogs and toasts).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.toolbar(ui);
        ui.add_space(12.0);
        match self.view {
            GalleryView::Both => {
                ui.columns(2, |cols| {
                    let [left, right] = cols else { return };
                    theme::scope(left, Theme::Dark, |ui| self.showcase(ui));
                    theme::scope(right, Theme::Light, |ui| self.showcase(ui));
                });
            }
            GalleryView::Dark => {
                theme::scope(ui, Theme::Dark, |ui| self.showcase(ui));
            }
            GalleryView::Light => {
                theme::scope(ui, Theme::Light, |ui| self.showcase(ui));
            }
        }
        self.dialogs(ui.ctx());
        self.toasts.show(ui.ctx());
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(icons::with_text(icons::PRINTER, "Sprint widgets"))
                    .font(theme::builtin_font(ui, egui::TextStyle::Heading)),
            );
            ui.add_space(16.0);
            ui.add(SegmentedControl::new(
                &mut self.view,
                &[
                    (GalleryView::Dark, "Dark"),
                    (GalleryView::Light, "Light"),
                    (GalleryView::Both, "Side by side"),
                ],
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(EStopButton::new()).changed() {
                    self.toasts
                        .error("Emergency stop")
                        .message("M112 would be sent to the printer.");
                    self.record("E-STOP");
                }
                ui.add(StatusPill::new("Simulator", Tone::Info).icon(icons::SIMULATOR));
                if let Some(last) = self.log.last() {
                    ui.label(egui::RichText::new(last.as_str()).weak());
                }
            });
        });
    }

    /// Every widget, once, in the current theme.
    pub fn showcase(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 8.0;
        self.buttons(ui);
        self.status(ui);
        self.selection(ui);
        self.motion(ui);
        self.temperatures(ui);
        self.charts(ui);
        self.forms(ui);
        self.mesh(ui);
        self.feedback(ui);
    }

    fn buttons(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "Buttons");
        ui.horizontal(|ui| {
            for (kind, icon, tip) in [
                (ButtonKind::Ghost, icons::REFRESH, "Ghost"),
                (ButtonKind::Normal, icons::HOME, "Normal"),
                (ButtonKind::Primary, icons::PLAY, "Primary"),
                (ButtonKind::Danger, icons::DELETE, "Danger"),
            ] {
                for size in [ButtonSize::Small, ButtonSize::Regular, ButtonSize::Large] {
                    if ui
                        .add(IconButton::new(icon).kind(kind).size(size).tooltip(tip))
                        .clicked()
                    {
                        self.record(format!("{tip} icon button"));
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            if ui
                .add(
                    ActionButton::new("Start print")
                        .icon(icons::PLAY)
                        .primary()
                        .large(),
                )
                .clicked()
            {
                self.dialog = Some(OpenDialog::Print);
            }
            ui.add(ActionButton::new("Pause").icon(icons::PAUSE).large());
            if ui
                .add(
                    ActionButton::new("Cancel")
                        .icon(icons::STOP)
                        .danger()
                        .large(),
                )
                .clicked()
            {
                self.record("Cancel");
            }
            ui.add(
                EStopButton::new()
                    .label("STOP")
                    .style(EStopStyle::Octagon(52.0)),
            );
        });
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "Status");
        ui.horizontal_wrapped(|ui| {
            ui.add(StatusPill::new("Connected", Tone::Success));
            ui.add(StatusPill::new("Connecting", Tone::Warning).pulse(true));
            ui.add(StatusPill::new("Offline", Tone::Neutral));
            ui.add(StatusPill::new("Halted", Tone::Danger));
            ui.add(Badge::new("MARLIN").tone(Tone::Accent));
            ui.add(Badge::new("3").tone(Tone::Danger).filled(true));
        });
    }

    fn selection(&mut self, ui: &mut egui::Ui) {
        section_header(ui, "Selection");
        ui.horizontal_wrapped(|ui| {
            ui.add(Toggle::new(&mut self.autoscroll).label("Autoscroll"));
            ui.add_space(8.0);
            if ui.add(Chip::new("Travel").selected(self.travel)).clicked() {
                self.travel = !self.travel;
            }
            ui.add(Chip::new("Perimeters").swatch(theme::hex(0xFF8C3C)));
            ui.add(SegmentedControl::new(
                &mut self.view3d,
                &[(0, "3D"), (1, "Layer"), (2, "G-code")],
            ));
        });
    }

    fn motion(&mut self, ui: &mut egui::Ui) {
        let mut log = None;
        card(
            ui,
            icons::MOVE,
            "Motion",
            |_| {},
            |ui| {
                ui.horizontal(|ui| {
                    if let Some(a) = JogPad::new().size(220.0).show(ui).action {
                        log = Some(a.describe());
                    }
                    ui.add_space(6.0);
                    if let Some(a) = ZJog::new().height(220.0).width(60.0).show(ui).action {
                        log = Some(a.describe());
                    }
                });
                ui.add_space(4.0);
                let cold = self.hotend.current < 170.0;
                let r = ExtruderControl::new(&mut self.length, &mut self.speed)
                    .blocked(cold.then_some("Hotend below 170 °C"))
                    .show(ui);
                match r.action {
                    Some(ExtruderAction::Extrude { length, .. }) => {
                        log = Some(format!("Extrude {length} mm"));
                    }
                    Some(ExtruderAction::Retract { length, .. }) => {
                        log = Some(format!("Retract {length} mm"));
                    }
                    None => {}
                }
            },
        );
        if let Some(text) = log {
            self.record(text);
        }
    }

    fn temperatures(&mut self, ui: &mut egui::Ui) {
        let presets = [("PLA", 210.0), ("PETG", 240.0), ("ABS", 250.0)];
        let r = TemperatureCard::new("Hotend", HeaterKind::Hotend)
            .current(self.hotend.current)
            .target(self.hotend.target)
            .power(Some(self.hotend.power))
            .history(&self.hotend.history)
            .presets(&presets)
            .show(ui);
        if let Some(a) = r.action {
            self.hotend.apply(a);
            self.record(format!("Hotend {a:?}"));
        }
        let r = TemperatureCard::new("Bed", HeaterKind::Bed)
            .current(self.bed.current)
            .target(self.bed.target)
            .power(Some(self.bed.power))
            .history(&self.bed.history)
            .presets(&[("PLA", 60.0), ("PETG", 80.0)])
            .show(ui);
        if let Some(a) = r.action {
            self.bed.apply(a);
            self.record(format!("Bed {a:?}"));
        }
    }

    fn charts(&mut self, ui: &mut egui::Ui) {
        let p = theme::ui_palette(ui);
        card(
            ui,
            icons::CHART,
            "Charts & progress",
            |_| {},
            |ui| {
                let temps: Vec<[f64; 2]> = self
                    .hotend
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, v)| [i as f64 * 0.5, f64::from(*v)])
                    .collect();
                let beds: Vec<[f64; 2]> = self
                    .bed
                    .history
                    .iter()
                    .enumerate()
                    .map(|(i, v)| [i as f64 * 0.5, f64::from(*v)])
                    .collect();
                let end = temps.last().map_or(1.0, |p| p[0]);
                let target = [
                    [0.0, f64::from(self.hotend.target)],
                    [end, f64::from(self.hotend.target)],
                ];
                MiniChart::new()
                    .series(ChartSeries::new("Hotend", &temps, p.hotend).fill(true))
                    .series(ChartSeries::new("Target", &target, p.hotend).dashed(true))
                    .series(ChartSeries::new("Bed", &beds, p.bed))
                    .unit("°C")
                    .height(110.0)
                    .show(ui);
                ui.horizontal(|ui| {
                    ui.add(
                        ProgressRing::new(self.progress)
                            .size(84.0)
                            .sublabel("Layer 42"),
                    );
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        ui.add(
                            LinearProgress::new(self.progress)
                                .label("benchy.gcode")
                                .show_percentage(true),
                        );
                        ui.add(LinearProgress::indeterminate().label("Analyzing toolpath…"));
                    });
                });
            },
        );
    }

    fn forms(&mut self, ui: &mut egui::Ui) {
        card(
            ui,
            icons::TUNING,
            "Settings",
            |_| {},
            |ui| {
                let w = 120.0;
                ui.add(
                    NumericField::new(&mut self.steps_x)
                        .label("X steps")
                        .unit("steps/mm")
                        .range(1.0..=10_000.0)
                        .original(80.0)
                        .label_width(w),
                );
                ui.add(
                    NumericField::new(&mut self.steps_z)
                        .label("Z steps")
                        .unit("steps/mm")
                        .range(1.0..=10_000.0)
                        .original(400.0)
                        .label_width(w),
                );
                ui.add(
                    NumericField::new(&mut self.feedrate)
                        .label("Max feedrate X")
                        .unit("mm/s")
                        .range(1.0..=1000.0)
                        .decimals(0)
                        .original(1500.0)
                        .label_width(w),
                );
                ui.add(
                    NumericField::new(&mut self.z_offset)
                        .label("Probe Z offset")
                        .unit("mm")
                        .original(-1.25)
                        .label_width(w),
                );
                ui.add(
                    NumericField::new(&mut self.fan)
                        .label("Fan index")
                        .label_width(w),
                );
                ui.add_space(4.0);
                KeyValueGrid::new("gallery-kv").key_width(w).show(ui, |kv| {
                    kv.row("Firmware", "Marlin 2.1.2.4");
                    kv.row_mono("UUID", "cede2a2f-41a2-4748-9b12-c55c62f367ff");
                    kv.row_tone("Thermal", "Stable", Tone::Success);
                });
            },
        );
    }

    fn mesh(&mut self, ui: &mut egui::Ui) {
        card(
            ui,
            icons::MESH,
            "Bed mesh",
            |_| {},
            |ui| {
                let r = Heatmap::from_mesh(&self.mesh).max_cell(56.0).show(ui);
                if let Some(cell) = r.clicked {
                    self.record(format!("Mesh cell {} / {}", cell.row, cell.col));
                }
            },
        );
    }

    fn feedback(&mut self, ui: &mut egui::Ui) {
        Card::new("Feedback")
            .icon(icons::BELL)
            .subtitle("toasts and dialogs")
            .collapsible(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add(ActionButton::new("Info").small()).clicked() {
                        self.toasts.info("Connected to the virtual printer");
                    }
                    if ui.add(ActionButton::new("Success").small()).clicked() {
                        self.toasts
                            .success("Print finished")
                            .message("benchy.gcode · 1 h 42 min");
                    }
                    if ui.add(ActionButton::new("Warning").small()).clicked() {
                        self.toasts.warning("Resend requested");
                    }
                    if ui.add(ActionButton::new("Error").small()).clicked() {
                        self.toasts
                            .error("Thermal runaway")
                            .message("Heater 0 stopped heating.")
                            .sticky();
                    }
                    if ui
                        .add(ActionButton::new("Reset EEPROM…").danger().small())
                        .clicked()
                    {
                        self.dialog = Some(OpenDialog::Reset);
                    }
                });
                EmptyState::new(icons::SD_CARD, "No SD card")
                    .message("Insert a card in the printer and refresh.")
                    .action("Refresh")
                    .action_icon(icons::REFRESH)
                    .show(ui);
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(open) = self.dialog else { return };
        let answer = match open {
            OpenDialog::Reset => ConfirmDialog::new("gallery-reset", "Reset all settings?")
                .message("M502 restores the firmware defaults. Unsaved tuning is lost.")
                .details("M502")
                .confirm_label("Reset")
                .danger(true)
                .show(ctx),
            OpenDialog::Print => {
                ConfirmDialog::new("gallery-print", "Start printing benchy.gcode?")
                    .message("Estimated 1 h 42 min, 11.3 m of filament.")
                    .confirm_label("Start print")
                    .icon(icons::PLAY)
                    .show(ctx)
            }
        };
        if let Some(answer) = answer {
            self.dialog = None;
            self.record(format!("{open:?}: {answer:?}"));
        }
    }
}

/// A plausible warped bed with one point that failed to probe.
fn demo_mesh() -> sprint_proto::BedMesh {
    let n = 5;
    let mut points: Vec<Vec<f32>> = (0..n)
        .map(|r| {
            (0..n)
                .map(|c| {
                    let x = c as f32 / (n - 1) as f32 - 0.5;
                    let y = r as f32 / (n - 1) as f32 - 0.5;
                    0.12 * x - 0.08 * y + 0.22 * (0.25 - x * x - y * y)
                })
                .collect()
        })
        .collect();
    if let Some(row) = points.get_mut(3)
        && let Some(v) = row.get_mut(1)
    {
        *v = f32::NAN;
    }
    let positions = vec![10.0, 62.5, 115.0, 167.5, 220.0];
    sprint_proto::BedMesh {
        points,
        x_positions: Some(positions.clone()),
        y_positions: Some(positions),
        kind: "bilinear".to_owned(),
    }
}

impl eframe::App for Gallery {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let dt = ctx.input(|i| i.stable_dt);
        self.tick(dt);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if let Some(left) = &mut self.frames_left {
            if *left == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                *left -= 1;
                ctx.request_repaint();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.ui(ui));
        });
    }
}

fn main() -> eframe::Result {
    let frames_left = std::env::var("SPRINT_GALLERY_FRAMES")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Sprint widget gallery")
            .with_inner_size([1180.0, 900.0])
            .with_min_inner_size([520.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Sprint widget gallery",
        options,
        Box::new(move |cc| {
            let report = theme::setup(&cc.egui_ctx, Theme::Dark);
            log::info!("fonts: {report:?}");
            let mut gallery = Gallery::new();
            gallery.frames_left = frames_left;
            Ok(Box::new(gallery))
        }),
    )
}

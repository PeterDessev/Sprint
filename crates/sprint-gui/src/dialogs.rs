//! The shell's dialogs: start print (from the beginning, a layer or a
//! line, with an editable resume preamble), the firmware's host prompts,
//! the halted banner, about and keyboard shortcuts.

use std::sync::Arc;

use egui::{
    Align, Color32, CornerRadius, Frame, Id, Key, Layout, Margin, Modal, RichText, Stroke, Ui, vec2,
};
use sprint_gcode::{ResumeOptions, Toolpath, resume_preamble};
use sprint_host::{HostPrompt, PrintOptions};

use crate::context::{AppAction, AppCtx, LoadedFile};
use crate::panels::job_bar::print_blocker;
use crate::theme::{self, Palette, TextRole, Tone};
use crate::widgets::{ActionButton, Badge, NumericField, SegmentedControl, Toggle};
use crate::{format, icons};

/// The frame shared by Sprint's modal dialogs.
pub fn modal_frame(ctx: &egui::Context) -> Frame {
    let p = theme::palette(ctx);
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(
            1.0,
            if p.dark { p.border_strong } else { p.border },
        ))
        .corner_radius(CornerRadius::same(theme::RADIUS_XL))
        .inner_margin(Margin::same(20))
        .shadow(ctx.global_style().visuals.window_shadow)
}

/// The backdrop behind modal dialogs.
pub fn backdrop(ctx: &egui::Context) -> Color32 {
    if theme::palette(ctx).dark {
        Color32::from_black_alpha(150)
    } else {
        Color32::from_black_alpha(90)
    }
}

/// A dialog title row with an icon.
pub fn dialog_title(ui: &mut Ui, icon: &str, title: &str, tone: Color32) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(20.0).color(tone));
        ui.label(
            RichText::new(title)
                .font(egui::FontId::new(
                    17.0,
                    theme::font(ui, TextRole::Title).family,
                ))
                .color(theme::ui_palette(ui).text),
        );
    });
}

/// Where a print starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMode {
    /// Line 1.
    Beginning,
    /// The first line of a layer.
    Layer,
    /// Any line.
    Line,
}

/// How the start-print dialog ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartOutcome {
    /// The print started.
    Started,
    /// The dialog was closed.
    Closed,
}

/// Files with more lines than this get their resume preamble computed on
/// a worker thread (walking millions of lines takes a moment).
pub const ASYNC_PREAMBLE_LINES: usize = 200_000;

/// The preamble for one start line and set of options.
type PreambleKey = (usize, ResumeOptions);

/// A preamble being computed on a worker thread.
struct PendingPreamble {
    key: PreambleKey,
    rx: crossbeam_channel::Receiver<String>,
}

/// The start-print dialog.
pub struct StartPrintDialog {
    mode: StartMode,
    layer: usize,
    line: usize,
    line_text: String,
    resume: ResumeOptions,
    preamble: String,
    preamble_for: Option<PreambleKey>,
    pending: Option<PendingPreamble>,
    edited: bool,
    emit_m73: bool,
    error: Option<String>,
}

impl StartPrintDialog {
    /// A dialog starting at 0-based `start_line` of `file`.
    pub fn new(start_line: usize, file: &LoadedFile) -> StartPrintDialog {
        let lines = file.file.len();
        let line = start_line.min(lines.saturating_sub(1));
        let layer = file
            .toolpath
            .as_ref()
            .and_then(|tp| tp.layer_at_line(line as u32))
            .unwrap_or(0);
        let mode = if line == 0 {
            StartMode::Beginning
        } else {
            StartMode::Line
        };
        StartPrintDialog {
            mode,
            layer,
            line,
            line_text: (line + 1).to_string(),
            resume: ResumeOptions::default(),
            preamble: String::new(),
            preamble_for: None,
            pending: None,
            edited: false,
            emit_m73: false,
            error: None,
        }
    }

    /// The 0-based line the print would start at.
    pub fn start_line(&self, toolpath: Option<&Toolpath>) -> usize {
        match self.mode {
            StartMode::Beginning => 0,
            StartMode::Layer => toolpath
                .and_then(|tp| tp.layers.get(self.layer))
                .map_or(0, |l| l.lines.start as usize),
            StartMode::Line => self.line,
        }
    }

    /// Choose where to start (tests and keyboard users).
    pub fn set_mode(&mut self, mode: StartMode) {
        self.mode = mode;
    }

    /// Choose the layer for [`StartMode::Layer`].
    pub fn set_layer(&mut self, layer: usize) {
        self.layer = layer;
    }

    /// Send M73 progress to the printer's display.
    pub fn set_emit_progress(&mut self, on: bool) {
        self.emit_m73 = on;
    }

    /// The preamble that would be sent (after [`StartPrintDialog::refresh_preamble`]).
    pub fn preamble(&self) -> &str {
        &self.preamble
    }

    /// True while the preamble is computed in the background.
    pub fn is_preparing(&self) -> bool {
        self.pending.is_some()
    }

    /// Regenerate the preamble for the current start line unless the user
    /// edited it (here and now, see [`StartPrintDialog::refresh_preamble_in_background`]).
    pub fn refresh_preamble(&mut self, file: &LoadedFile) {
        let line = self.start_line(file.toolpath.as_deref());
        let key = (line, self.resume.clone());
        if self.edited || self.preamble_for.as_ref() == Some(&key) {
            return;
        }
        self.pending = None;
        self.preamble = if line == 0 {
            String::new()
        } else {
            resume_preamble(&file.file, line, &self.resume).join("\n")
        };
        self.preamble_for = Some(key);
    }

    /// Like [`StartPrintDialog::refresh_preamble`], but for files over
    /// [`ASYNC_PREAMBLE_LINES`] lines the work runs on a worker thread
    /// (`repaint` is asked for a frame when it is done).
    pub fn refresh_preamble_in_background(&mut self, file: &LoadedFile, repaint: &egui::Context) {
        let line = self.start_line(file.toolpath.as_deref());
        let key = (line, self.resume.clone());
        if let Some(pending) = &self.pending {
            match pending.rx.try_recv() {
                Ok(text) => {
                    if !self.edited {
                        self.preamble = text;
                        self.preamble_for = Some(pending.key.clone());
                    }
                    self.pending = None;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.pending = None,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
        let wanted = |k: &PreambleKey| *k == key;
        if self.edited
            || self.preamble_for.as_ref().is_some_and(wanted)
            || self.pending.as_ref().is_some_and(|p| wanted(&p.key))
        {
            return;
        }
        if line == 0 || file.file.len() < ASYNC_PREAMBLE_LINES {
            self.refresh_preamble(file);
            return;
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        let gcode = Arc::clone(&file.file);
        let options = self.resume.clone();
        let ctx = repaint.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-gui-preamble".to_owned())
            .spawn(move || {
                let _ = tx.send(resume_preamble(&gcode, line, &options).join("\n"));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.pending = Some(PendingPreamble { key, rx }),
            Err(_) => self.refresh_preamble(file),
        }
    }

    /// Try to start the print.
    pub fn start(&mut self, ctx: &mut AppCtx<'_>, file: &LoadedFile) -> bool {
        if let Some(reason) = print_blocker(ctx.state, true) {
            self.error = Some(reason.to_owned());
            return false;
        }
        self.refresh_preamble(file);
        let start_line = self.start_line(file.toolpath.as_deref());
        let profile = ctx.config.active();
        let options = PrintOptions {
            start_line,
            preamble: if start_line > 0 {
                crate::panels::console::command_lines(&self.preamble)
            } else {
                Vec::new()
            },
            toolpath: file.toolpath.clone(),
            pause_script: Some(profile.pause_script.clone()),
            resume_script: Some(profile.resume_script.clone()),
            cancel_script: Some(profile.cancel_script.clone()),
            emit_progress: self.emit_m73,
        };
        match ctx.printer.start_print(Arc::clone(&file.file), options) {
            Ok(()) => {
                let name = format::sanitize(file.file.name(), 60);
                let toast = ctx.toasts.success("Print started");
                if start_line > 0 {
                    toast.message(format!(
                        "{name} from line {}",
                        format::count(start_line as u64 + 1)
                    ));
                } else {
                    toast.message(name);
                }
                true
            }
            Err(e) => {
                self.error = Some(format!("Could not start the print: {e}"));
                false
            }
        }
    }

    /// Show the dialog; `Some` once it is done.
    pub fn show(
        &mut self,
        egui_ctx: &egui::Context,
        ctx: &mut AppCtx<'_>,
        file: &LoadedFile,
    ) -> Option<StartOutcome> {
        let p = theme::palette(egui_ctx);
        let mut outcome = None;
        let dragging = egui_ctx.input(|i| i.pointer.any_down());
        if !dragging {
            self.refresh_preamble_in_background(file, egui_ctx);
        }
        let toolpath = file.toolpath.clone();
        let modal = Modal::new(Id::new("sprint-start-print"))
            .frame(modal_frame(egui_ctx))
            .backdrop_color(backdrop(egui_ctx))
            .show(egui_ctx, |ui| {
                ui.set_width(520.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                dialog_title(ui, icons::PLAY, "Start print", p.accent);
                ui.label(
                    RichText::new(format::sanitize(file.file.name(), 70))
                        .color(p.text_dim),
                );
                if let Some(tp) = &toolpath {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 16.0;
                        let stat = |ui: &mut Ui, icon: &str, text: String| {
                            ui.label(RichText::new(icons::with_text(icon, &text)).color(p.text));
                        };
                        stat(ui, icons::TIMER, format::duration(f64::from(tp.stats.estimated_time)));
                        stat(ui, icons::FILAMENT, format::length_mm(f64::from(tp.stats.filament_length)));
                        stat(ui, icons::LAYERS, format!("{} layers", format::count(tp.layers.len() as u64)));
                    });
                } else {
                    ui.label(
                        RichText::new("The analysis has not finished: progress will be counted in lines and layers cannot be picked.")
                            .size(12.5)
                            .color(p.warning),
                    );
                }
                ui.separator();
                self.start_point(ui, file, toolpath.as_deref(), &p);
                if self.start_line(toolpath.as_deref()) > 0 {
                    self.preamble_editor(ui, file, &p);
                }
                ui.add(Toggle::new(&mut self.emit_m73).label("Show progress on the printer's display (M73)"));
                if let Some(error) = &self.error {
                    ui.label(RichText::new(icons::with_text(icons::ERROR, error)).color(p.danger));
                }
                let blocker = print_blocker(ctx.state, true).or_else(|| {
                    (self.is_preparing() && self.start_line(toolpath.as_deref()) > 0)
                        .then_some("Preparing the resume preamble…")
                });
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let start = ui
                        .add_enabled(
                            blocker.is_none(),
                            ActionButton::new("Start print").icon(icons::PLAY).primary(),
                        )
                        .on_disabled_hover_text(blocker.unwrap_or(""));
                    if start.clicked() && self.start(ctx, file) {
                        outcome = Some(StartOutcome::Started);
                    }
                    if ui.add(ActionButton::new("Cancel")).clicked() {
                        outcome = Some(StartOutcome::Closed);
                    }
                    if let Some(reason) = blocker {
                        ui.label(RichText::new(reason).size(12.0).color(p.text_dim));
                    }
                });
            });
        if outcome.is_none() && modal.should_close() {
            outcome = Some(StartOutcome::Closed);
        }
        outcome
    }

    fn start_point(&mut self, ui: &mut Ui, file: &LoadedFile, tp: Option<&Toolpath>, p: &Palette) {
        let mut options: Vec<(StartMode, &str)> = vec![(StartMode::Beginning, "Beginning")];
        if tp.is_some_and(|t| !t.layers.is_empty()) {
            options.push((StartMode::Layer, "Layer"));
        }
        options.push((StartMode::Line, "Line"));
        if self.mode == StartMode::Layer && tp.is_none() {
            self.mode = StartMode::Line;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Start from").color(p.text_dim));
            ui.add(SegmentedControl::new(&mut self.mode, &options).id_salt("start-mode"));
        });
        let lines = file.file.len();
        match self.mode {
            StartMode::Beginning => {}
            StartMode::Layer => {
                let Some(tp) = tp else { return };
                let n = tp.layers.len();
                let mut shown = self.layer.min(n.saturating_sub(1)) + 1;
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 300.0;
                    ui.add(egui::Slider::new(&mut shown, 1..=n.max(1)).text("layer"));
                });
                self.layer = shown - 1;
                if let Some(layer) = tp.layers.get(self.layer) {
                    ui.label(
                        RichText::new(format!(
                            "Z {} mm · starts at line {} · about {} into the print",
                            format::mm(layer.z, 2),
                            format::count(u64::from(layer.lines.start) + 1),
                            format::duration(f64::from(layer.time_start))
                        ))
                        .size(12.5)
                        .color(p.text_dim),
                    );
                }
            }
            StartMode::Line => {
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.line_text)
                            .desired_width(100.0)
                            .hint_text("line"),
                    );
                    if edit.changed()
                        && let Some(line) =
                            crate::panels::gcode_tab::parse_goto(&self.line_text, lines)
                    {
                        self.line = line as usize;
                    }
                    ui.label(
                        RichText::new(format!("of {}", format::count(lines as u64)))
                            .color(p.text_dim),
                    );
                });
                let text = format::sanitize(file.file.line(self.line).trim(), 60);
                let mut info = format!("Line {}: {text}", format::count(self.line as u64 + 1));
                if let Some(tp) = tp
                    && let Some(layer) = tp
                        .layer_at_line(self.line as u32)
                        .and_then(|i| tp.layers.get(i).map(|l| (i, l)))
                {
                    info.push_str(&format!(
                        " · layer {} at Z {} mm",
                        layer.0 + 1,
                        format::mm(layer.1.z, 2)
                    ));
                }
                ui.label(RichText::new(info).size(12.5).monospace().color(p.text_dim));
            }
        }
    }

    fn preamble_editor(&mut self, ui: &mut Ui, file: &LoadedFile, p: &Palette) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("RESUME PREAMBLE")
                    .font(theme::font(ui, TextRole::Section))
                    .color(p.text_dim),
            );
            if self.edited {
                ui.add(Badge::new("EDITED").tone(Tone::Warning));
            }
            if self.is_preparing() {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(RichText::new("Preparing…").size(12.0).color(p.text_dim));
            }
        });
        let before = self.resume.clone();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.add(Toggle::new(&mut self.resume.home_xy).label("Home X/Y"));
            ui.add(Toggle::new(&mut self.resume.home_z).label("Home Z"))
                .on_hover_text(
                    "Only when nothing is on the bed: homing Z drives the nozzle into the part",
                );
            ui.add(Toggle::new(&mut self.resume.wait_for_temps).label("Wait for temperatures"));
            ui.add(Toggle::new(&mut self.resume.restore_z_position).label("Board was reset"))
                .on_hover_text(
                    "G92 Z to the resume height (the printer lost its position but did not move)",
                );
        });
        ui.add(
            NumericField::new(&mut self.resume.z_clearance)
                .label("Z clearance")
                .unit("mm")
                .range(0.5..=50.0)
                .decimals(1),
        );
        if self.resume != before {
            self.edited = false;
        }
        if self.resume.home_z {
            ui.label(
                RichText::new(icons::with_text(
                    icons::WARNING,
                    "Homing Z with a part on the bed crashes the nozzle into it.",
                ))
                .size(12.0)
                .color(p.warning),
            );
        }
        let edited = egui::ScrollArea::vertical()
            .id_salt("sprint-preamble-scroll")
            .max_height(170.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.preamble)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(7)
                        .desired_width(f32::INFINITY),
                )
                .changed()
            })
            .inner;
        if edited {
            self.edited = true;
        }
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(
                    "Sent before the first line; it heats, homes and moves the nozzle. Check it.",
                )
                .size(12.0)
                .color(p.text_faint),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.edited,
                        ActionButton::new("Regenerate").icon(icons::RESET).small(),
                    )
                    .clicked()
                {
                    self.edited = false;
                    self.preamble_for = None;
                    self.refresh_preamble_in_background(file, ui.ctx());
                }
            });
        });
    }
}

/// Show the firmware's host prompt; returns the chosen button (or
/// `usize::MAX` when the user hid the dialog without answering).
pub fn prompt_dialog(egui_ctx: &egui::Context, prompt: &HostPrompt) -> Option<usize> {
    let p = theme::palette(egui_ctx);
    let mut answer = None;
    let modal = Modal::new(Id::new("sprint-host-prompt"))
        .frame(modal_frame(egui_ctx))
        .backdrop_color(backdrop(egui_ctx))
        .show(egui_ctx, |ui| {
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing.y = 10.0;
            dialog_title(ui, icons::BELL, "The printer is asking", p.accent);
            let message = if prompt.message.trim().is_empty() {
                "The printer is waiting for you.".to_owned()
            } else {
                format::sanitize(&prompt.message, 300)
            };
            ui.label(RichText::new(message).color(p.text).size(15.0));
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if prompt.buttons.is_empty() {
                    if ui.add(ActionButton::new("Continue").primary()).clicked() {
                        answer = Some(0);
                    }
                } else {
                    // Right to left: the first choice ends up rightmost.
                    for (i, label) in prompt.buttons.iter().enumerate() {
                        let label = format::sanitize(label, 30);
                        let mut button = ActionButton::new(&label);
                        if i == 0 {
                            button = button.primary();
                        }
                        if ui.add(button).clicked() {
                            answer = Some(i);
                        }
                    }
                }
                if ui
                    .add(ActionButton::new("Hide").ghost())
                    .on_hover_text("Close this without answering (the printer keeps waiting)")
                    .clicked()
                {
                    answer = Some(usize::MAX);
                }
            });
        });
    // Escape or a click beside the dialog hides it, like "Hide".
    if answer.is_none() && modal.should_close() {
        answer = Some(usize::MAX);
    }
    answer
}

/// The red banner shown while the firmware is halted.
pub fn halted_banner(ui: &mut Ui, ctx: &mut AppCtx<'_>) {
    let p = theme::ui_palette(ui);
    // Buttons first, from the right; the message takes what is left (and
    // is cut short there instead of running under them).
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if ui
            .add(ActionButton::new("Reconnect").icon(icons::CONNECT))
            .on_hover_text("Close and reopen the connection (resets most boards)")
            .clicked()
        {
            ctx.actions.push(AppAction::Reconnect);
        }
        if ui
            .add(
                ActionButton::new("Restart firmware (M999)")
                    .icon(icons::RESTART)
                    .danger(),
            )
            .on_hover_text("Ask the firmware to leave the halted state")
            .clicked()
        {
            ctx.printer.firmware_restart();
            ctx.toasts.info("Restarting the firmware (M999)");
        }
        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.label(RichText::new(icons::ERROR).size(18.0).color(p.danger));
            ui.label(
                RichText::new("Printer halted")
                    .font(theme::font(ui, TextRole::Title))
                    .color(p.danger),
            );
            let reason = ctx.state.last_error.as_deref().map_or(
                "The firmware stopped (emergency stop, thermal protection or a kill).".to_owned(),
                |e| format::sanitize(e, 400),
            );
            // Cut to the room left; the full text shows on hover.
            ui.add(egui::Label::new(RichText::new(reason).color(p.text)).truncate());
        });
    });
}

/// Keyboard shortcuts, for the shortcuts dialog and menus.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+O", "Open a G-code file"),
    ("F5", "Reload the file from disk"),
    ("Ctrl+P", "Start print…"),
    ("Space", "Pause / resume the print"),
    ("Ctrl+F", "Find in the G-code"),
    ("Ctrl+G", "Go to line"),
    ("Ctrl+,", "Settings and printer profiles"),
    (
        "1 – 7",
        "View presets: iso, top, bottom, front, back, left, right",
    ),
    ("Z", "Fit the 3D view (pointer over the view)"),
    ("↑ / ↓", "Top layer up / down (3D view)"),
    ("PgUp / PgDn", "Top layer ±10"),
    ("Home / End", "First / last layer"),
    ("Shift + layer keys", "Move the lower layer handle"),
    (
        "Enter / Shift+Enter",
        "Console: send / new line; Find: next / previous",
    ),
    ("↑ / ↓ in the console", "Command history (or suggestions)"),
    ("Tab", "Accept a G-code suggestion"),
    ("Esc", "Close dialogs, find and go to line"),
];

/// Show the keyboard shortcuts; true once closed.
pub fn shortcuts_dialog(egui_ctx: &egui::Context) -> bool {
    let p = theme::palette(egui_ctx);
    let mut close = false;
    let modal = Modal::new(Id::new("sprint-shortcuts"))
        .frame(modal_frame(egui_ctx))
        .backdrop_color(backdrop(egui_ctx))
        .show(egui_ctx, |ui| {
            ui.set_width(480.0);
            dialog_title(
                ui,
                icons::phosphor::KEYBOARD,
                "Keyboard shortcuts",
                p.accent,
            );
            ui.add_space(6.0);
            egui::Grid::new("sprint-shortcut-grid")
                .num_columns(2)
                .spacing(vec2(18.0, 7.0))
                .show(ui, |ui| {
                    for (keys, action) in SHORTCUTS {
                        keycap(ui, keys, &p);
                        ui.label(RichText::new(*action).color(p.text));
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(ActionButton::new("Close").primary()).clicked() {
                    close = true;
                }
            });
        });
    close || modal.should_close()
}

/// A key combination drawn as caps.
fn keycap(ui: &mut Ui, keys: &str, p: &Palette) {
    egui::Frame::new()
        .fill(p.raised)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(CornerRadius::same(theme::RADIUS_SM))
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(keys).monospace().size(12.0).color(p.text));
        });
}

/// Show the about dialog; true once closed.
pub fn about_dialog(egui_ctx: &egui::Context, icon: Option<&egui::TextureHandle>) -> bool {
    let p = theme::palette(egui_ctx);
    let mut close = false;
    let modal = Modal::new(Id::new("sprint-about"))
        .frame(modal_frame(egui_ctx))
        .backdrop_color(backdrop(egui_ctx))
        .show(egui_ctx, |ui| {
            ui.set_width(420.0);
            ui.vertical_centered(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if let Some(icon) = icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(84.0, 84.0)));
                }
                ui.label(
                    RichText::new("Sprint")
                        .font(egui::FontId::new(26.0, theme::font(ui, TextRole::Display).family))
                        .color(p.text),
                );
                ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(p.text_dim));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("A fast, modern 3D-printer host: G-code preview, printing, firmware insight, tuning and a console that always lets you talk to the printer.")
                        .color(p.text),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("CREDITS").font(theme::font(ui, TextRole::Section)).color(p.text_dim));
                ui.label(
                    RichText::new("Inspired by Printrun (Pronterface / pronsole) by Kliment and contributors.\nBuilt with Rust, egui, eframe, wgpu and Phosphor icons.")
                        .size(12.5)
                        .color(p.text_dim),
                );
                ui.label(RichText::new("Licensed under MIT or Apache-2.0").size(12.0).color(p.text_faint));
            });
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(ActionButton::new("Close").primary()).clicked() {
                    close = true;
                }
            });
        });
    close || modal.should_close() || egui_ctx.input(|i| i.key_pressed(Key::Escape))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sprint_gcode::{AnalyzeOptions, GcodeFile, analyze_simple};

    fn loaded(text: &str) -> LoadedFile {
        let file = Arc::new(GcodeFile::from_text("t.gcode", text));
        let toolpath = Arc::new(analyze_simple(&file, &AnalyzeOptions::default()));
        LoadedFile {
            path: None,
            file,
            toolpath: Some(toolpath),
            analysis_progress: None,
        }
    }

    const TWO_LAYERS: &str = "M104 S200\nG28\nG1 Z0.2 F600\nG1 X10 Y0 E1 F1200\nG1 X10 Y10 E2\nG1 Z0.4\nG1 X0 Y10 E3\nG1 X0 Y0 E4\n";

    #[test]
    fn start_lines_follow_the_mode() {
        let file = loaded(TWO_LAYERS);
        let tp = file.toolpath.as_deref();
        let mut d = StartPrintDialog::new(0, &file);
        assert_eq!(d.mode, StartMode::Beginning);
        assert_eq!(d.start_line(tp), 0);
        d.set_mode(StartMode::Layer);
        d.set_layer(1);
        let second = tp
            .and_then(|t| t.layers.get(1))
            .map(|l| l.lines.start as usize);
        assert_eq!(Some(d.start_line(tp)), second);
        let d = StartPrintDialog::new(6, &file);
        assert_eq!(d.mode, StartMode::Line);
        assert_eq!(d.start_line(tp), 6);
        assert_eq!(d.layer, 1);
        // Out-of-range starts are clamped.
        let d = StartPrintDialog::new(10_000, &file);
        assert_eq!(d.start_line(tp), file.file.len() - 1);
    }

    #[test]
    fn preamble_regenerates_until_edited() {
        let file = loaded(TWO_LAYERS);
        let mut d = StartPrintDialog::new(6, &file);
        d.refresh_preamble(&file);
        assert!(
            d.preamble().contains("M109") || d.preamble().contains("M104"),
            "{}",
            d.preamble()
        );
        assert!(d.preamble().contains("G28"));
        d.preamble = "G28 X\n".into();
        d.edited = true;
        d.line = 5;
        d.refresh_preamble(&file);
        assert_eq!(d.preamble(), "G28 X\n");
        d.edited = false;
        d.refresh_preamble(&file);
        assert_ne!(d.preamble(), "G28 X\n");
        let mut d = StartPrintDialog::new(0, &file);
        d.refresh_preamble(&file);
        assert!(d.preamble().is_empty());
    }

    #[test]
    fn huge_files_prepare_the_preamble_in_the_background() {
        let mut text = String::from("M104 S200\nG28\nG1 Z0.2 F600\n");
        for i in 0..ASYNC_PREAMBLE_LINES {
            text.push_str(&format!("G1 X{} Y{} E{}\n", i % 100, (i / 100) % 100, i));
        }
        let file = LoadedFile {
            path: None,
            file: Arc::new(GcodeFile::from_text("big.gcode", text)),
            toolpath: None,
            analysis_progress: None,
        };
        let ctx = egui::Context::default();
        let mut d = StartPrintDialog::new(ASYNC_PREAMBLE_LINES, &file);
        d.refresh_preamble_in_background(&file, &ctx);
        assert!(d.is_preparing());
        assert!(d.preamble().is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while d.is_preparing() {
            assert!(
                std::time::Instant::now() < deadline,
                "preamble never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            d.refresh_preamble_in_background(&file, &ctx);
        }
        assert!(d.preamble().contains("G28"), "{}", d.preamble());
        // Small files are prepared at once.
        let small = loaded(TWO_LAYERS);
        let mut d = StartPrintDialog::new(6, &small);
        d.refresh_preamble_in_background(&small, &ctx);
        assert!(!d.is_preparing());
        assert!(d.preamble().contains("G28"));
    }

    #[test]
    fn shortcuts_are_listed() {
        for key in [
            "Ctrl+O", "F5", "Ctrl+P", "Space", "Ctrl+F", "Ctrl+G", "Ctrl+,", "Esc",
        ] {
            assert!(SHORTCUTS.iter().any(|(k, _)| *k == key), "{key}");
        }
    }
}

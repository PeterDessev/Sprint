//! The settings dialog: printer profiles (add, duplicate, delete, rename
//! and every [`PrinterProfile`] field) and app preferences (theme, console
//! length). Edits go to a draft that replaces the live configuration only
//! on "Save".

use egui::{Align, Id, Layout, Margin, Modal, RichText, Stroke, Ui, vec2};
use sprint_gcode::KinematicLimits;
use sprint_host::sprint_sim::SimConfig;
use sprint_host::{
    AppConfig, BAUD_RATES, BedOrigin, CONFIG_ENV, ChecksumMode, ConfigReport, ConnectTarget,
    DEFAULT_BAUD, DEFAULT_CANCEL_SCRIPT, DEFAULT_PAUSE_SCRIPT, DEFAULT_RESUME_SCRIPT,
    MAX_IN_FLIGHT_LIMIT, PrinterProfile, TempPreset,
};

use crate::dialogs::{backdrop, dialog_title, modal_frame};
use crate::panels::connection_bar::{TargetKind, host_port_text, parse_host_port};
use crate::theme::{self, Palette, TextRole, Theme, Tone};
use crate::widgets::{
    ActionButton, Badge, ConfirmDialog, DialogResult, IconButton, NumericField, SegmentedControl,
    Toggle,
};
use crate::{format, icons};

/// Most problems of the configuration load listed on the preferences page.
const MAX_REPORT_LINES: usize = 12;

/// Placeholders the pause / resume / cancel scripts understand.
pub const SCRIPT_PLACEHOLDERS: &str = "{x} {y} {z} {e} {f} — position and feedrate when paused\n{tool} {hotend} {bed} {fan} — tool, targets and fan\n{max_x} {max_y} {max_z} {park_x} {park_y} — from the bed size\n{heaters_off} — every heater off (each hotend, the bed and the chamber)";

/// What the print event commands are and what they get.
pub const HOOK_HELP: &str = "Shell commands run when a print (from Sprint or from the SD card) starts, finishes, fails or is cancelled, like Printrun's start, final and error commands: on Windows with cmd /C, elsewhere with sh -c, stopped after 2 minutes, their output in the console. Quote the variables (\"%SPRINT_FILE%\" for cmd, \"$SPRINT_FILE\" for sh): a file name may hold characters the shell acts on. Leave one empty to run nothing.";

/// The environment the print event commands get.
pub const HOOK_ENVIRONMENT: &str = "SPRINT_FILE — the file or SD name\nSPRINT_STATUS — started, completed, failed or cancelled\nSPRINT_ELAPSED_SECS — print time so far\nSPRINT_PRINTER — the printer profile's name\nSPRINT_REASON — why it failed (failures only)";

/// Which page the dialog shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    /// A printer profile.
    Profile(usize),
    /// App preferences.
    Preferences,
}

/// How the dialog ended.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsOutcome {
    /// Apply the draft.
    Saved,
    /// Discard it.
    Cancelled,
}

/// Clamp every profile value into a usable range (fields allow typing
/// anything; this runs on save).
pub fn sanitize_profile(p: &mut PrinterProfile) {
    fn fix(v: &mut f32, lo: f32, hi: f32, default: f32) {
        *v = if v.is_finite() {
            v.clamp(lo, hi)
        } else {
            default
        };
    }
    let d = PrinterProfile::default();
    if p.name.trim().is_empty() {
        p.name = "Printer".to_owned();
    }
    p.name = p.name.trim().chars().take(60).collect();
    fix(&mut p.bed_size[0], 10.0, 10_000.0, d.bed_size[0]);
    fix(&mut p.bed_size[1], 10.0, 10_000.0, d.bed_size[1]);
    fix(&mut p.max_z, 1.0, 10_000.0, d.max_z);
    p.extruders = p.extruders.clamp(1, 16);
    fix(&mut p.filament_diameter, 0.5, 5.0, d.filament_diameter);
    fix(&mut p.nozzle_diameter, 0.05, 5.0, d.nozzle_diameter);
    fix(&mut p.jog_feedrate_xy, 1.0, 100_000.0, d.jog_feedrate_xy);
    fix(&mut p.jog_feedrate_z, 1.0, 100_000.0, d.jog_feedrate_z);
    fix(&mut p.extrude_feedrate, 1.0, 100_000.0, d.extrude_feedrate);
    p.temp_presets.retain(|t| !t.name.trim().is_empty());
    for t in &mut p.temp_presets {
        fix(&mut t.hotend, 0.0, 500.0, 0.0);
        fix(&mut t.bed, 0.0, 200.0, 0.0);
    }
    p.options = p.options.sanitized();
    if let Some(k) = &mut p.kinematics {
        let dk = KinematicLimits::default();
        for i in 0..4 {
            fix(&mut k.max_feedrate[i], 0.1, 10_000.0, dk.max_feedrate[i]);
            fix(&mut k.max_accel[i], 1.0, 1_000_000.0, dk.max_accel[i]);
            fix(&mut k.jerk[i], 0.0, 1000.0, dk.jerk[i]);
        }
        fix(&mut k.print_accel, 1.0, 1_000_000.0, dk.print_accel);
        fix(&mut k.retract_accel, 1.0, 1_000_000.0, dk.retract_accel);
        fix(&mut k.travel_accel, 1.0, 1_000_000.0, dk.travel_accel);
        fix(&mut k.min_feedrate, 0.0, 1000.0, dk.min_feedrate);
        if let Some(j) = &mut k.junction_deviation {
            fix(j, 0.001, 1.0, 0.013);
        }
    }
    if let ConnectTarget::Simulator(sim) = &mut p.connection {
        fix(&mut sim.time_scale, 0.1, 1000.0, 1.0);
        fix(&mut sim.checksum_error_rate, 0.0, 0.5, 0.0);
        fix(&mut sim.drop_ok_rate, 0.0, 0.5, 0.0);
        sim.extruders = sim.extruders.clamp(1, 8);
    }
}

/// A name not used by any profile: `base`, `base 2`, `base 3`, ...
pub fn unique_name(profiles: &[PrinterProfile], base: &str) -> String {
    let taken = |n: &str| profiles.iter().any(|p| p.name == n);
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| base.to_owned())
}

/// The settings dialog.
pub struct SettingsDialog {
    /// The configuration being edited.
    pub draft: AppConfig,
    /// The theme being edited.
    pub theme: Theme,
    page: Page,
    confirm_delete: Option<usize>,
    tcp_text: String,
    /// The profile page `tcp_text` was typed on.
    tcp_page: Option<usize>,
    ports: Vec<String>,
    config_path: Option<std::path::PathBuf>,
    /// What loading the configuration left out.
    config_report: Option<ConfigReport>,
}

impl SettingsDialog {
    /// Edit a copy of `config`; `ports` are port names to suggest.
    pub fn new(config: &AppConfig, theme: Theme, ports: Vec<String>) -> SettingsDialog {
        let page = Page::Profile(
            config
                .active_profile
                .min(config.profiles.len().saturating_sub(1)),
        );
        SettingsDialog {
            draft: config.clone(),
            theme,
            page,
            confirm_delete: None,
            tcp_text: String::new(),
            tcp_page: None,
            ports,
            config_path: Some(AppConfig::path()),
            config_report: None,
        }
    }

    /// Where the configuration is saved (`None`: kept in memory only).
    #[must_use]
    pub fn with_config_path(mut self, path: Option<std::path::PathBuf>) -> SettingsDialog {
        self.config_path = path;
        self
    }

    /// What loading the configuration left out, listed on the preferences
    /// page.
    #[must_use]
    pub fn with_config_report(mut self, report: Option<ConfigReport>) -> SettingsDialog {
        self.config_report = report.filter(|r| !r.is_clean());
        self
    }

    fn location_text(&self) -> String {
        match &self.config_path {
            Some(path) => format!("Saved to {}", path.display()),
            None => "Kept in memory for this session (not saved to disk)".to_owned(),
        }
    }

    /// Whether the configuration file is the one `SPRINT_CONFIG` names.
    fn named_by_environment(&self) -> bool {
        let named = std::env::var_os(CONFIG_ENV).filter(|v| !v.to_string_lossy().trim().is_empty());
        match (&self.config_path, named) {
            (Some(path), Some(named)) => {
                crate::loader::same_path(path, std::path::Path::new(&named))
            }
            _ => false,
        }
    }

    /// The page shown.
    pub fn page(&self) -> Page {
        self.page
    }

    /// Show a page.
    pub fn set_page(&mut self, page: Page) {
        self.page = page;
    }

    /// Add a default profile and show it.
    pub fn add_profile(&mut self) -> usize {
        let name = unique_name(&self.draft.profiles, "New printer");
        self.draft.profiles.push(PrinterProfile {
            name,
            ..PrinterProfile::default()
        });
        let i = self.draft.profiles.len() - 1;
        self.page = Page::Profile(i);
        i
    }

    /// Copy profile `i` and show the copy.
    pub fn duplicate_profile(&mut self, i: usize) -> Option<usize> {
        let mut copy = self.draft.profiles.get(i)?.clone();
        copy.name = unique_name(&self.draft.profiles, &format!("{} copy", copy.name));
        copy.auto_connect = false;
        self.draft.profiles.insert(i + 1, copy);
        if self.draft.active_profile > i {
            self.draft.active_profile += 1;
        }
        self.page = Page::Profile(i + 1);
        Some(i + 1)
    }

    /// Delete profile `i` (the last one cannot be deleted).
    pub fn delete_profile(&mut self, i: usize) -> bool {
        if self.draft.profiles.len() <= 1 || i >= self.draft.profiles.len() {
            return false;
        }
        self.draft.profiles.remove(i);
        // The page index may now show another profile.
        self.tcp_page = None;
        let active = self.draft.active_profile;
        self.draft.active_profile = if active > i {
            active - 1
        } else {
            active.min(self.draft.profiles.len() - 1)
        };
        self.page = Page::Profile(i.min(self.draft.profiles.len() - 1));
        true
    }

    /// Finish editing: sanitize the draft.
    pub fn finish(&mut self) {
        for p in &mut self.draft.profiles {
            sanitize_profile(p);
        }
        if self.draft.profiles.is_empty() {
            self.draft.profiles.push(PrinterProfile::default());
        }
        self.draft.active_profile = self.draft.active_profile.min(self.draft.profiles.len() - 1);
        self.draft.console_max_lines = self.draft.console_max_lines.clamp(100, 1_000_000);
    }

    /// Show the dialog; `Some` once saved or cancelled. While `connected`
    /// (a connection is open) the active profile cannot be switched or
    /// deleted, and connection changes apply on the next connect.
    pub fn show(&mut self, egui_ctx: &egui::Context, connected: bool) -> Option<SettingsOutcome> {
        let p = theme::palette(egui_ctx);
        let mut outcome = None;
        let screen = egui_ctx.content_rect();
        let height = (screen.height() - 120.0).clamp(360.0, 640.0);
        let modal = Modal::new(Id::new("sprint-settings"))
            .frame(modal_frame(egui_ctx).inner_margin(Margin::same(0)))
            .backdrop_color(backdrop(egui_ctx))
            .show(egui_ctx, |ui| {
                ui.set_width((screen.width() - 80.0).clamp(560.0, 880.0));
                ui.set_height(height);
                egui::Frame::new()
                    .inner_margin(Margin::symmetric(20, 16))
                    .show(ui, |ui| {
                        dialog_title(ui, icons::SETTINGS, "Settings", p.accent);
                    });
                ui.separator();
                let body_h = height - 120.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        vec2(210.0, body_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            egui::Frame::new()
                                .inner_margin(Margin::symmetric(12, 4))
                                .show(ui, |ui| {
                                    ui.set_width(186.0);
                                    self.navigation(ui, &p);
                                });
                        },
                    );
                    // As tall as the body: a plain separator takes all the
                    // height left on screen and stretches the dialog.
                    ui.add_sized(
                        vec2(ui.spacing().item_spacing.x, body_h),
                        egui::Separator::default().vertical(),
                    );
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), body_h),
                        Layout::top_down(Align::Min),
                        |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(("sprint-settings-page", self.page_key()))
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    egui::Frame::new()
                                        .inner_margin(Margin {
                                            left: 8,
                                            right: 18,
                                            top: 4,
                                            bottom: 12,
                                        })
                                        .show(ui, |ui| match self.page {
                                            Page::Profile(i) => {
                                                self.profile_page(ui, i, connected, &p)
                                            }
                                            Page::Preferences => self.preferences_page(ui, &p),
                                        });
                                });
                        },
                    );
                });
                ui.separator();
                egui::Frame::new()
                    .inner_margin(Margin::symmetric(20, 10))
                    .show(ui, |ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(ActionButton::new("Save").icon(icons::SAVE).primary())
                                .clicked()
                            {
                                self.finish();
                                outcome = Some(SettingsOutcome::Saved);
                            }
                            if ui.add(ActionButton::new("Cancel")).clicked() {
                                outcome = Some(SettingsOutcome::Cancelled);
                            }
                            ui.label(
                                RichText::new(self.location_text())
                                    .size(11.5)
                                    .color(p.text_faint),
                            );
                        });
                    });
            });
        if let Some(i) = self.confirm_delete {
            let name = self
                .draft
                .profiles
                .get(i)
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let title = format!("Delete \"{}\"?", format::sanitize(&name, 40));
            match ConfirmDialog::new("sprint-delete-profile", &title)
                .message("The profile and its settings are removed when you save.")
                .confirm_label("Delete")
                .danger(true)
                .show(egui_ctx)
            {
                Some(DialogResult::Confirmed) => {
                    self.delete_profile(i);
                    self.confirm_delete = None;
                }
                Some(DialogResult::Cancelled) => self.confirm_delete = None,
                None => {}
            }
        } else if outcome.is_none()
            // Escape cancels; a stray click beside the dialog does not throw
            // the edits away (unlike `should_close`).
            && modal.is_top_modal
            && !modal.any_popup_open
            && egui_ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            outcome = Some(SettingsOutcome::Cancelled);
        }
        outcome
    }

    fn page_key(&self) -> usize {
        match self.page {
            Page::Profile(i) => i,
            Page::Preferences => usize::MAX,
        }
    }

    fn navigation(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(
            RichText::new("PRINTER PROFILES")
                .font(theme::font(ui, TextRole::Section))
                .color(p.text_dim),
        );
        ui.add_space(4.0);
        let active = self.draft.active_profile;
        for (i, profile) in self.draft.profiles.iter().enumerate() {
            let selected = self.page == Page::Profile(i);
            let mut text = format::sanitize(&profile.name, 26);
            if i == active {
                text = format!("{text}  {}", icons::CHECK);
            }
            let r = nav_button(ui, selected, text);
            if r.clicked() {
                self.page = Page::Profile(i);
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .add(ActionButton::new("Add").icon(icons::ADD).small())
                .clicked()
            {
                self.add_profile();
            }
            if let Page::Profile(i) = self.page
                && ui
                    .add(ActionButton::new("Duplicate").icon(icons::COPY).small())
                    .clicked()
            {
                self.duplicate_profile(i);
            }
        });
        ui.add_space(14.0);
        ui.label(
            RichText::new("APP")
                .font(theme::font(ui, TextRole::Section))
                .color(p.text_dim),
        );
        ui.add_space(4.0);
        let r = nav_button(
            ui,
            self.page == Page::Preferences,
            icons::with_text(icons::SUN, "Preferences"),
        );
        if r.clicked() {
            self.page = Page::Preferences;
        }
    }

    fn preferences_page(&mut self, ui: &mut Ui, p: &Palette) {
        section(ui, "Appearance", p);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Theme").color(p.text_dim));
            ui.add(
                SegmentedControl::new(
                    &mut self.theme,
                    &[(Theme::Dark, "Dark"), (Theme::Light, "Light")],
                )
                .id_salt("prefs-theme"),
            );
        });
        section(ui, "Console", p);
        ui.add(
            NumericField::new(&mut self.draft.console_max_lines)
                .label("Lines kept")
                .range(100..=1_000_000)
                .hint("Older lines are dropped; more lines use more memory")
                .label_width(140.0),
        );
        section(ui, "Configuration file", p);
        let path = self.location_text();
        ui.add(
            egui::Label::new(
                RichText::new(&path)
                    .monospace()
                    .size(12.0)
                    .color(p.text_dim),
            )
            .wrap(),
        );
        if self.named_by_environment() {
            note(
                ui,
                &format!("Chosen with the {CONFIG_ENV} environment variable."),
                p,
            );
        } else if self.config_path.is_some() {
            note(
                ui,
                &format!("Set {CONFIG_ENV} to use another configuration file."),
                p,
            );
        }
        ui.label(
            RichText::new("Window layout, the console history and view options are remembered separately by the app.")
                .size(12.0)
                .color(p.text_faint),
        );
        if let Some(report) = &self.config_report {
            ui.add_space(8.0);
            warn(
                ui,
                "Some lines or values of the file could not be read and were left out (their defaults apply); everything else loaded. Saving writes the file again without them.",
                p,
            );
            ui.add_space(4.0);
            for line in report.warnings.iter().take(MAX_REPORT_LINES) {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("• {}", format::sanitize(line, 300)))
                            .monospace()
                            .size(11.5)
                            .color(p.text_dim),
                    )
                    .wrap(),
                );
            }
            let more = report.warnings.len().saturating_sub(MAX_REPORT_LINES);
            if more > 0 {
                note(ui, &format!("… and {more} more."), p);
            }
            if let Some(backup) = &report.backup {
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "The file as it was is kept in {}",
                            backup.display()
                        ))
                        .size(12.0)
                        .color(p.text),
                    )
                    .wrap(),
                );
            }
        }
    }

    fn profile_page(&mut self, ui: &mut Ui, i: usize, connected: bool, p: &Palette) {
        let count = self.draft.profiles.len();
        let active = self.draft.active_profile == i;
        let Some(profile) = self.draft.profiles.get_mut(i) else {
            self.page = Page::Profile(0);
            return;
        };
        // Name and actions.
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut profile.name)
                    .font(egui::FontId::new(
                        16.0,
                        theme::font(ui, TextRole::Title).family,
                    ))
                    .desired_width(260.0)
                    .hint_text("Profile name"),
            );
            if active {
                ui.add(Badge::new("ACTIVE").tone(Tone::Success));
            }
            // The open connection keeps its profile: its bed and parking
            // position drive the scripts of the printer that is attached.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (can_delete, why) = if count <= 1 {
                    (false, "The last profile cannot be deleted")
                } else if active && connected {
                    (false, "Disconnect to delete the printer in use")
                } else {
                    (true, "")
                };
                if ui
                    .add_enabled(
                        can_delete,
                        IconButton::new(icons::DELETE)
                            .danger()
                            .tooltip("Delete this profile"),
                    )
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    self.confirm_delete = Some(i);
                }
                if !active
                    && ui
                        .add_enabled(!connected, ActionButton::new("Use this printer").small())
                        .on_disabled_hover_text("Disconnect to switch printers")
                        .clicked()
                {
                    self.draft.active_profile = i;
                }
            });
        });
        let Some(profile) = self.draft.profiles.get_mut(i) else {
            return;
        };

        section(ui, "Connection", p);
        if connected && active {
            note(ui, "Connection changes apply the next time you connect.", p);
        }
        if self.tcp_page != Some(i) {
            // Another page: what was typed on the last one does not apply
            // (empty shows this profile's address).
            self.tcp_text.clear();
            self.tcp_page = Some(i);
        }
        connection_editor(ui, profile, &mut self.tcp_text, &self.ports, p);
        ui.add_space(4.0);
        ui.add(Toggle::new(&mut profile.auto_connect).label("Connect when Sprint starts"));
        if profile.auto_connect {
            warn(
                ui,
                "Sprint opens this connection at every start. Many boards reset when the port opens, which stops a print started from the printer's own screen.",
                p,
            );
        }

        section(ui, "Protocol", p);
        let o = &mut profile.options;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Line numbers and checksums").color(p.text_dim));
            ui.add(
                SegmentedControl::new(
                    &mut o.checksum,
                    &[
                        (ChecksumMode::Always, "Always"),
                        (ChecksumMode::PrintOnly, "While printing"),
                        (ChecksumMode::Never, "Never"),
                    ],
                )
                .id_salt("profile-checksum"),
            );
        });
        let w = 190.0;
        ui.add(
            NumericField::new(&mut o.ok_timeout_secs)
                .label("Reply timeout")
                .unit("s")
                .range(0.05..=3600.0)
                .decimals(1)
                .label_width(w)
                .hint("No reply for this long counts as a lost \"ok\""),
        );
        ui.add(
            NumericField::new(&mut o.handshake_timeout_secs)
                .label("Handshake timeout")
                .unit("s")
                .range(0.1..=3600.0)
                .decimals(1)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut o.temp_poll_secs)
                .label("Temperature polling")
                .unit("s")
                .range(0.0..=3600.0)
                .decimals(1)
                .label_width(w)
                .hint(
                    "M105 interval when the firmware cannot report by itself; 0 turns polling off",
                ),
        );
        ui.add(
            NumericField::new(&mut o.autoreport_interval_secs)
                .label("Auto-report interval")
                .unit("s")
                .range(0..=60)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut o.max_in_flight)
                .label("Commands in flight")
                .range(1..=MAX_IN_FLIGHT_LIMIT)
                .label_width(w)
                .hint(
                    "1 is strict ping-pong; more fills the firmware's buffer for faster streaming",
                ),
        );
        ui.add(
            Toggle::new(&mut o.query_on_connect)
                .label("Read firmware info and settings after connecting"),
        );
        ui.add(
            Toggle::new(&mut o.reset_on_connect)
                .label("Reset the board when connecting (pulse DTR)"),
        );

        section(ui, "Machine", p);
        ui.add(
            NumericField::new(&mut profile.bed_size[0])
                .label("Bed width (X)")
                .unit("mm")
                .range(10.0..=10_000.0)
                .decimals(1)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut profile.bed_size[1])
                .label("Bed depth (Y)")
                .unit("mm")
                .range(10.0..=10_000.0)
                .decimals(1)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut profile.max_z)
                .label("Max height (Z)")
                .unit("mm")
                .range(1.0..=10_000.0)
                .decimals(1)
                .label_width(w),
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                vec2(w, 20.0),
                egui::Label::new(RichText::new("Origin").color(p.text_dim)),
            );
            ui.add(
                SegmentedControl::new(
                    &mut profile.origin,
                    &[
                        (BedOrigin::FrontLeft, "Front left"),
                        (BedOrigin::Center, "Bed center"),
                    ],
                )
                .id_salt("profile-origin"),
            );
        });
        ui.add(
            NumericField::new(&mut profile.extruders)
                .label("Extruders")
                .range(1..=16)
                .label_width(w),
        );
        ui.add(Toggle::new(&mut profile.heated_bed).label("Heated bed"));
        ui.add(Toggle::new(&mut profile.heated_chamber).label("Heated chamber"));

        section(ui, "Filament and nozzle", p);
        ui.add(
            NumericField::new(&mut profile.filament_diameter)
                .label("Filament diameter")
                .unit("mm")
                .range(0.5..=5.0)
                .decimals(2)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut profile.nozzle_diameter)
                .label("Nozzle diameter")
                .unit("mm")
                .range(0.05..=5.0)
                .decimals(2)
                .label_width(w),
        );

        section(ui, "Temperature presets", p);
        let mut remove = None;
        egui::Grid::new(("sprint-presets", i))
            .num_columns(4)
            .spacing(vec2(10.0, 6.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Name").size(12.0).color(p.text_dim));
                ui.label(RichText::new("Hotend").size(12.0).color(p.text_dim));
                ui.label(RichText::new("Bed").size(12.0).color(p.text_dim));
                ui.end_row();
                for (n, preset) in profile.temp_presets.iter_mut().enumerate() {
                    ui.add(egui::TextEdit::singleline(&mut preset.name).desired_width(110.0));
                    ui.add(
                        NumericField::new(&mut preset.hotend)
                            .unit("°C")
                            .range(0.0..=500.0)
                            .decimals(0)
                            .field_width(80.0),
                    );
                    ui.add(
                        NumericField::new(&mut preset.bed)
                            .unit("°C")
                            .range(0.0..=200.0)
                            .decimals(0)
                            .field_width(80.0),
                    );
                    if ui
                        .add(
                            IconButton::new(icons::REMOVE)
                                .ghost()
                                .small()
                                .tooltip("Remove"),
                        )
                        .clicked()
                    {
                        remove = Some(n);
                    }
                    ui.end_row();
                }
            });
        if let Some(n) = remove {
            profile.temp_presets.remove(n);
        }
        if ui
            .add(ActionButton::new("Add preset").icon(icons::ADD).small())
            .clicked()
        {
            profile.temp_presets.push(TempPreset {
                name: "New".to_owned(),
                hotend: 200.0,
                bed: 60.0,
            });
        }

        section(ui, "Jogging", p);
        ui.add(
            NumericField::new(&mut profile.jog_feedrate_xy)
                .label("X/Y jog speed")
                .unit("mm/min")
                .range(1.0..=100_000.0)
                .decimals(0)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut profile.jog_feedrate_z)
                .label("Z jog speed")
                .unit("mm/min")
                .range(1.0..=100_000.0)
                .decimals(0)
                .label_width(w),
        );
        ui.add(
            NumericField::new(&mut profile.extrude_feedrate)
                .label("Extrude speed")
                .unit("mm/min")
                .range(1.0..=100_000.0)
                .decimals(0)
                .label_width(w),
        );

        section(ui, "Pause, resume and cancel scripts", p);
        note(ui, SCRIPT_PLACEHOLDERS, p);
        script_editor(
            ui,
            "Pause",
            &mut profile.pause_script,
            DEFAULT_PAUSE_SCRIPT,
            p,
        );
        script_editor(
            ui,
            "Resume",
            &mut profile.resume_script,
            DEFAULT_RESUME_SCRIPT,
            p,
        );
        script_editor(
            ui,
            "Cancel",
            &mut profile.cancel_script,
            DEFAULT_CANCEL_SCRIPT,
            p,
        );

        section(ui, "Printing", p);
        ui.add(
            Toggle::new(&mut profile.emit_progress)
                .label("Show progress on the printer's display (M73)"),
        )
        .on_hover_text(
            "M73 P<percent> R<minutes> while printing from Sprint; the start dialog starts \
             with this and can change it for one print",
        );
        ui.add(
            Toggle::new(&mut profile.keep_heaters_on_failure)
                .label("Keep the heaters on when a print fails"),
        );
        if profile.keep_heaters_on_failure {
            warn(
                ui,
                "A print the link gives up on leaves the printer hot with nobody watching it. \
                 Only for printers you stay with.",
                p,
            );
        } else {
            note(
                ui,
                "When Sprint gives up on a print (a lost link, a firmware that stopped \
                 answering), it switches every heater and the fan off.",
                p,
            );
        }

        section(ui, "Print event commands", p);
        note(ui, HOOK_HELP, p);
        ui.label(
            RichText::new(HOOK_ENVIRONMENT)
                .monospace()
                .size(11.5)
                .color(p.text_dim),
        );
        ui.add_space(4.0);
        hook_editor(ui, "When a print starts", &mut profile.on_start_command, p);
        hook_editor(
            ui,
            "When a print finishes",
            &mut profile.on_finish_command,
            p,
        );
        hook_editor(ui, "When a print fails", &mut profile.on_fail_command, p);
        hook_editor(
            ui,
            "When a print is cancelled",
            &mut profile.on_cancel_command,
            p,
        );

        section(ui, "Print time estimation", p);
        let mut custom = profile.kinematics.is_some();
        if ui
            .add(Toggle::new(&mut custom).label("Use custom motion limits"))
            .changed()
        {
            profile.kinematics = custom.then(KinematicLimits::default);
        }
        match &mut profile.kinematics {
            None => note(
                ui,
                "Sprint uses the limits the firmware reports (M503) when connected, otherwise typical Marlin defaults.",
                p,
            ),
            Some(k) => kinematics_editor(ui, k, w, p),
        }
    }
}

/// A full-width, left-aligned navigation entry.
fn nav_button(ui: &mut Ui, selected: bool, text: String) -> egui::Response {
    ui.with_layout(Layout::top_down_justified(Align::Min), |ui| {
        ui.add(egui::Button::selectable(selected, text).min_size(vec2(0.0, 28.0)))
    })
    .inner
}

fn section(ui: &mut Ui, title: &str, p: &Palette) {
    ui.add_space(14.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .font(theme::font(ui, TextRole::Section))
            .color(p.text_dim),
    );
    let rect = ui.available_rect_before_wrap();
    ui.painter()
        .hline(rect.x_range(), rect.top() + 1.0, Stroke::new(1.0, p.border));
    ui.add_space(8.0);
}

fn note(ui: &mut Ui, text: &str, p: &Palette) {
    ui.label(RichText::new(text).size(12.0).color(p.text_faint));
}

fn warn(ui: &mut Ui, text: &str, p: &Palette) {
    egui::Frame::new()
        .fill(p.tone_soft(Tone::Warning))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.label(
                RichText::new(icons::with_text(icons::WARNING, text))
                    .size(12.5)
                    .color(p.warning),
            );
        });
}

fn connection_editor(
    ui: &mut Ui,
    profile: &mut PrinterProfile,
    tcp_text: &mut String,
    ports: &[String],
    p: &Palette,
) {
    let mut kind = TargetKind::of(&profile.connection);
    let before = kind;
    ui.add(
        SegmentedControl::new(
            &mut kind,
            &[
                (TargetKind::Serial, "Serial port"),
                (TargetKind::Network, "Network (TCP)"),
                (TargetKind::Simulator, "Virtual printer"),
            ],
        )
        .id_salt("profile-kind"),
    );
    if kind != before {
        profile.connection = match kind {
            TargetKind::Serial => ConnectTarget::Serial {
                port: ports.first().cloned().unwrap_or_else(|| {
                    if cfg!(windows) {
                        "COM3".to_owned()
                    } else {
                        "/dev/ttyUSB0".to_owned()
                    }
                }),
                baud: DEFAULT_BAUD,
            },
            TargetKind::Network => ConnectTarget::Tcp {
                host: "localhost".to_owned(),
                port: 23,
            },
            TargetKind::Simulator => ConnectTarget::Simulator(SimConfig::default()),
        };
        *tcp_text = host_port_text(&profile.connection);
    }
    ui.add_space(4.0);
    match &mut profile.connection {
        ConnectTarget::Serial { port, baud } => {
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(80.0, 20.0),
                    egui::Label::new(RichText::new("Port").color(p.text_dim)),
                );
                ui.add(egui::TextEdit::singleline(port).desired_width(160.0));
                if !ports.is_empty() {
                    egui::ComboBox::from_id_salt("profile-port-pick")
                        .selected_text("Found ports")
                        .show_ui(ui, |ui| {
                            for name in ports {
                                if ui.selectable_label(name == port, name).clicked() {
                                    *port = name.clone();
                                }
                            }
                        });
                }
            });
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(80.0, 20.0),
                    egui::Label::new(RichText::new("Baud").color(p.text_dim)),
                );
                egui::ComboBox::from_id_salt("profile-baud")
                    .selected_text(if *baud == 0 {
                        "Auto".to_owned()
                    } else {
                        baud.to_string()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(baud, 0, "Auto");
                        for &rate in BAUD_RATES {
                            ui.selectable_value(baud, rate, rate.to_string());
                        }
                    });
            });
        }
        ConnectTarget::Tcp { .. } => {
            if tcp_text.is_empty()
                || parse_host_port(tcp_text).as_ref() != Some(&profile.connection)
                    && parse_host_port(tcp_text).is_some()
            {
                *tcp_text = host_port_text(&profile.connection);
            }
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(80.0, 20.0),
                    egui::Label::new(RichText::new("Address").color(p.text_dim)),
                );
                let valid = parse_host_port(tcp_text).is_some();
                let r = ui.add(
                    egui::TextEdit::singleline(tcp_text)
                        .desired_width(220.0)
                        .hint_text("host:port")
                        .text_color_opt((!valid).then_some(p.danger)),
                );
                if r.changed()
                    && let Some(target) = parse_host_port(tcp_text)
                {
                    profile.connection = target;
                }
                if !valid {
                    ui.label(RichText::new("host:port").size(12.0).color(p.danger));
                }
            });
        }
        ConnectTarget::Simulator(sim) => {
            note(
                ui,
                "Sprint's built-in simulated Marlin printer: safe for trying things out.",
                p,
            );
            ui.add(
                NumericField::new(&mut sim.time_scale)
                    .label("Speed")
                    .unit("× real time")
                    .range(0.1..=1000.0)
                    .decimals(1)
                    .label_width(140.0),
            );
            ui.add(
                NumericField::new(&mut sim.extruders)
                    .label("Extruders")
                    .range(1..=8)
                    .label_width(140.0),
            );
            ui.add(Toggle::new(&mut sim.heated_bed).label("Heated bed"));
            ui.add(Toggle::new(&mut sim.instant_moves).label("Instant moves"));
            ui.add(
                NumericField::new(&mut sim.checksum_error_rate)
                    .label("Line errors")
                    .range(0.0..=0.5)
                    .decimals(3)
                    .label_width(140.0)
                    .hint("Probability that a line arrives corrupted (tests resends)"),
            );
            ui.add(
                NumericField::new(&mut sim.drop_ok_rate)
                    .label("Lost replies")
                    .range(0.0..=0.5)
                    .decimals(3)
                    .label_width(140.0)
                    .hint("Probability that an \"ok\" is lost (tests timeouts)"),
            );
        }
    }
}

/// One print event command: a labelled single-line field.
fn hook_editor(ui: &mut Ui, title: &str, command: &mut String, p: &Palette) {
    ui.add_space(4.0);
    let label = ui.label(RichText::new(title).strong().color(p.text));
    ui.add(
        egui::TextEdit::singleline(command)
            .font(egui::TextStyle::Monospace)
            .hint_text(if cfg!(windows) {
                "e.g. msg * \"Sprint: %SPRINT_FILE% %SPRINT_STATUS%\""
            } else {
                "e.g. notify-send Sprint \"$SPRINT_FILE $SPRINT_STATUS\""
            })
            .desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}

fn script_editor(ui: &mut Ui, title: &str, script: &mut String, default: &str, p: &Palette) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().color(p.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui
                .add_enabled(
                    script != default,
                    ActionButton::new("Default")
                        .icon(icons::RESET)
                        .small()
                        .ghost(),
                )
                .clicked()
            {
                *script = default.to_owned();
            }
        });
    });
    ui.add(
        egui::TextEdit::multiline(script)
            .font(egui::TextStyle::Monospace)
            .desired_rows(4)
            .desired_width(f32::INFINITY),
    );
}

fn kinematics_editor(ui: &mut Ui, k: &mut KinematicLimits, w: f32, p: &Palette) {
    let axes = ["X", "Y", "Z", "E"];
    egui::Grid::new("sprint-kinematics")
        .num_columns(5)
        .spacing(vec2(8.0, 6.0))
        .show(ui, |ui| {
            ui.label("");
            for a in axes {
                ui.label(RichText::new(a).size(12.0).color(p.text_dim));
            }
            ui.end_row();
            ui.label(RichText::new("Max speed (mm/s)").color(p.text_dim));
            for v in &mut k.max_feedrate {
                ui.add(
                    NumericField::new(v)
                        .range(0.1..=10_000.0)
                        .decimals(1)
                        .field_width(70.0),
                );
            }
            ui.end_row();
            ui.label(RichText::new("Max accel (mm/s²)").color(p.text_dim));
            for v in &mut k.max_accel {
                ui.add(
                    NumericField::new(v)
                        .range(1.0..=1_000_000.0)
                        .decimals(0)
                        .field_width(70.0),
                );
            }
            ui.end_row();
            ui.label(RichText::new("Jerk (mm/s)").color(p.text_dim));
            for v in &mut k.jerk {
                ui.add(
                    NumericField::new(v)
                        .range(0.0..=1000.0)
                        .decimals(2)
                        .field_width(70.0),
                );
            }
            ui.end_row();
        });
    ui.add(
        NumericField::new(&mut k.print_accel)
            .label("Print acceleration")
            .unit("mm/s²")
            .range(1.0..=1_000_000.0)
            .decimals(0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut k.retract_accel)
            .label("Retract acceleration")
            .unit("mm/s²")
            .range(1.0..=1_000_000.0)
            .decimals(0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut k.travel_accel)
            .label("Travel acceleration")
            .unit("mm/s²")
            .range(1.0..=1_000_000.0)
            .decimals(0)
            .label_width(w),
    );
    ui.add(
        NumericField::new(&mut k.min_feedrate)
            .label("Minimum speed")
            .unit("mm/s")
            .range(0.0..=1000.0)
            .decimals(1)
            .label_width(w),
    );
    let mut jd = k.junction_deviation.is_some();
    if ui
        .add(Toggle::new(&mut jd).label("Junction deviation (off: classic jerk)"))
        .changed()
    {
        k.junction_deviation = jd.then_some(0.013);
    }
    if let Some(j) = &mut k.junction_deviation {
        ui.add(
            NumericField::new(j)
                .label("Junction deviation")
                .unit("mm")
                .range(0.001..=1.0)
                .decimals(3)
                .label_width(w),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> SettingsDialog {
        SettingsDialog::new(&AppConfig::default(), Theme::Dark, vec!["COM5".into()])
    }

    #[test]
    fn add_duplicate_delete_keep_the_active_profile() {
        let mut d = dialog();
        assert_eq!(d.page(), Page::Profile(0));
        let added = d.add_profile();
        assert_eq!(added, 1);
        assert_eq!(d.draft.profiles[1].name, "New printer");
        assert_eq!(d.add_profile(), 2);
        assert_eq!(d.draft.profiles[2].name, "New printer 2");
        d.draft.active_profile = 2;
        d.draft.profiles[0].auto_connect = true;
        let copy = d.duplicate_profile(0).expect("copy");
        assert_eq!(copy, 1);
        assert_eq!(d.draft.profiles[1].name, "Virtual printer copy");
        assert!(
            !d.draft.profiles[1].auto_connect,
            "copies never auto-connect"
        );
        assert_eq!(d.draft.active_profile, 3, "active index follows the insert");
        assert!(d.delete_profile(0));
        assert_eq!(d.draft.active_profile, 2);
        assert!(d.delete_profile(2));
        assert_eq!(d.draft.active_profile, 1);
        while d.draft.profiles.len() > 1 {
            assert!(d.delete_profile(0));
        }
        assert!(!d.delete_profile(0), "the last profile stays");
        assert!(!d.delete_profile(5));
        assert!(d.duplicate_profile(9).is_none());
    }

    #[test]
    fn sanitizing_repairs_every_field() {
        let mut p = PrinterProfile {
            name: "   ".into(),
            bed_size: [f32::NAN, -5.0],
            max_z: f32::INFINITY,
            extruders: 0,
            filament_diameter: 99.0,
            nozzle_diameter: f32::NAN,
            jog_feedrate_xy: -1.0,
            temp_presets: vec![
                TempPreset {
                    name: "".into(),
                    hotend: 1.0,
                    bed: 1.0,
                },
                TempPreset {
                    name: "Hot".into(),
                    hotend: 9000.0,
                    bed: f32::NAN,
                },
            ],
            kinematics: Some(KinematicLimits {
                max_feedrate: [f32::NAN; 4],
                junction_deviation: Some(-3.0),
                ..KinematicLimits::default()
            }),
            connection: ConnectTarget::Simulator(SimConfig {
                time_scale: f32::NAN,
                checksum_error_rate: 2.0,
                ..SimConfig::default()
            }),
            ..PrinterProfile::default()
        };
        p.options.max_in_flight = 999;
        sanitize_profile(&mut p);
        assert_eq!(p.name, "Printer");
        assert_eq!(p.bed_size, [220.0, 10.0]);
        assert_eq!(p.max_z, 250.0);
        assert_eq!(p.extruders, 1);
        assert_eq!(p.filament_diameter, 5.0);
        assert_eq!(p.nozzle_diameter, 0.4);
        assert_eq!(p.jog_feedrate_xy, 1.0);
        assert_eq!(p.temp_presets.len(), 1);
        assert_eq!(p.temp_presets[0].hotend, 500.0);
        assert_eq!(p.temp_presets[0].bed, 0.0);
        assert_eq!(p.options.max_in_flight, MAX_IN_FLIGHT_LIMIT);
        let k = p.kinematics.expect("kept");
        assert_eq!(k.max_feedrate, KinematicLimits::default().max_feedrate);
        assert_eq!(k.junction_deviation, Some(0.001));
        let ConnectTarget::Simulator(sim) = &p.connection else {
            panic!("simulator");
        };
        assert_eq!(sim.time_scale, 1.0);
        assert_eq!(sim.checksum_error_rate, 0.5);
    }

    #[test]
    fn finish_clamps_the_draft() {
        let mut d = dialog();
        d.draft.console_max_lines = 3;
        d.draft.active_profile = 42;
        d.finish();
        assert_eq!(d.draft.console_max_lines, 100);
        assert_eq!(d.draft.active_profile, 0);
        assert_eq!(unique_name(&d.draft.profiles, "X"), "X");
        assert_eq!(
            unique_name(&d.draft.profiles, "Virtual printer"),
            "Virtual printer 2"
        );
    }
}

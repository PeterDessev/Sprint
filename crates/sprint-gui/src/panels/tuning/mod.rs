//! Tuning tab: a catalog-driven editor for the firmware's EEPROM settings.
//!
//! The settings the firmware reported (M503) are copied into a [`Draft`].
//! Every known setting gets a card with one field per parameter (units,
//! ranges, decimals and descriptions from the settings catalog), one row
//! per instance (`M301 E0`/`E1`, `M906` per driver, ...) and modified
//! markers; settings the catalog does not know are edited as raw G-code.
//! Edits are only sent on "Apply changes"; EEPROM actions (save, load,
//! factory reset), copy, export and import (with a preview) sit in the
//! header and toolbar.

mod cards;
pub mod model;

use egui::{Align, Layout, Rect, RichText, Ui, UiBuilder, pos2, vec2};
use sprint_proto::setting_def;

use self::cards::Filter;
use self::model::{Draft, Import, ImportDiff, diff_import, export_text, parse_import};
use crate::context::AppCtx;
use crate::icons;
use crate::insight::ui::{self as kit, FileOutcome, FileTask};
use crate::theme::{self, TextRole, Tone};
use crate::widgets::{
    ActionButton, Badge, ConfirmDialog, DialogResult, EmptyState, IconButton, card_frame,
};

/// Shown for firmware without M503 (Klipper).
const NO_M503_HELP: &str = "This firmware keeps its configuration in a file (Klipper: \
     printer.cfg) and does not report settings with M503.";
/// Width of the category list.
const LIST_WIDTH: f32 = 196.0;
/// Below this width the category list is hidden.
const LIST_MIN_TOTAL: f32 = 640.0;

/// A question waiting for an answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Save,
    Load,
    FactoryReset,
}

/// An import waiting for the user's review.
#[derive(Clone, Debug)]
struct ImportPreview {
    /// File name shown in the title.
    name: String,
    import: Import,
    diffs: Vec<(sprint_proto::SettingCommand, ImportDiff)>,
}

impl ImportPreview {
    fn changed(&self) -> usize {
        self.diffs
            .iter()
            .filter(|(_, d)| !matches!(d, ImportDiff::Unchanged))
            .count()
    }
}

/// What a running file dialog is for.
enum FileJob {
    Export(FileTask),
    Import(FileTask),
}

/// Tuning tab: catalog-driven EEPROM settings editor.
#[derive(Default)]
pub struct TuningTab {
    draft: Draft,
    /// The draft follows a report (false until the first one arrives).
    synced: bool,
    search: String,
    filter: Filter,
    /// M503 was requested automatically for this connection.
    auto_requested: bool,
    connection: kit::ConnectionWatch,
    show_changes: bool,
    confirm: Option<Confirm>,
    import: Option<ImportPreview>,
    file: Option<FileJob>,
}

impl TuningTab {
    /// Draw the panel.
    pub fn ui(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        self.sync(ctx);
        self.poll_file(ctx);
        let connected = ctx.connected();
        let no_m503 = ctx
            .state
            .firmware
            .as_ref()
            .is_some_and(|f| !f.flavor.supports_m503());
        if no_m503 && self.draft.entries().is_empty() {
            ui.add_space(48.0);
            let _ = EmptyState::new(icons::TUNING, "No EEPROM settings")
                .message(NO_M503_HELP)
                .show(ui);
            return;
        }
        if !connected && self.draft.entries().is_empty() {
            ui.add_space(48.0);
            let _ = EmptyState::new(icons::TUNING, "Not connected")
                .message("Connect to a printer to load, edit and save its firmware settings.")
                .show(ui);
            return;
        }
        self.header(ui, ctx);
        ui.add_space(theme::GAP);
        self.toolbar(ui, ctx);
        ui.add_space(theme::GAP);
        if ctx.job_active() {
            kit::notice(
                ui,
                Tone::Warning,
                icons::WARNING,
                "A job is running",
                Some("Applied changes take effect immediately and affect the print in progress."),
                None,
            );
            ui.add_space(theme::GAP);
        }
        if self.draft.entries().is_empty() {
            ui.add_space(32.0);
            let loading = connected && !ctx.state.settings_loaded;
            let r = EmptyState::new(
                icons::TUNING,
                if loading {
                    "Loading settings…"
                } else {
                    "No settings"
                },
            )
            .message("The firmware reports its active settings with M503.")
            .action("Load from printer")
            .action_icon(icons::REFRESH)
            .show(ui);
            if r.action_clicked() {
                ctx.printer.request_settings();
            }
        } else {
            self.body(ui, ctx);
        }
        self.dialogs(ui, ctx);
    }

    /// Number of edited settings not applied yet.
    pub fn pending_changes(&self) -> usize {
        self.draft.modified_count()
    }

    /// The editor's draft.
    pub fn draft(&self) -> &Draft {
        &self.draft
    }

    /// Stage a setting as if it had been edited (shown as a pending change
    /// until applied).
    pub fn stage(&mut self, cmd: sprint_proto::SettingCommand) {
        self.draft.stage(cmd);
    }

    /// Open the import preview for a file's text (what "Import…" does after
    /// reading the file).
    pub fn import_text(&mut self, name: &str, text: &str) {
        let known: Vec<String> = self
            .draft
            .entries()
            .iter()
            .map(|e| e.draft.code.clone())
            .collect();
        let import = parse_import(text, &known);
        let diffs = diff_import(&self.draft, &import);
        self.import = Some(ImportPreview {
            name: name.to_owned(),
            import,
            diffs,
        });
    }

    /// The text "Export…" writes.
    pub fn export_text(&self, ctx: &AppCtx<'_>) -> String {
        let fw = ctx.state.firmware.as_ref();
        export_text(
            &self.draft,
            fw.map(|f| f.name.as_str()),
            fw.and_then(|f| f.machine_type.as_deref()),
        )
    }

    /// Follow the printer's settings: request them once per connection and
    /// rebase the draft on every new report.
    fn sync(&mut self, ctx: &AppCtx<'_>) {
        let state = ctx.state;
        if self.connection.is_new(state) {
            self.auto_requested = false;
        }
        if state.is_connected()
            && !state.settings_loaded
            && !self.auto_requested
            && state
                .firmware
                .as_ref()
                .is_none_or(|f| f.flavor.supports_m503())
        {
            ctx.printer.request_settings();
            self.auto_requested = true;
        }
        if !self.synced || self.draft.base() != &state.settings {
            if self.synced {
                self.draft.rebase(&state.settings);
            } else {
                self.draft = Draft::new(&state.settings);
            }
            self.synced = true;
        }
    }

    fn poll_file(&mut self, ctx: &mut AppCtx<'_>) {
        let Some(job) = &self.file else {
            return;
        };
        let (outcome, import) = match job {
            FileJob::Export(task) => (task.poll(), false),
            FileJob::Import(task) => (task.poll(), true),
        };
        let Some(outcome) = outcome else {
            return;
        };
        self.file = None;
        match outcome {
            FileOutcome::Saved(path) => {
                ctx.toasts
                    .success("Settings exported")
                    .message(path.display().to_string());
            }
            FileOutcome::Loaded(path, text) if import => {
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                self.import_text(&name, &text);
            }
            FileOutcome::Loaded(..) | FileOutcome::Cancelled => {}
            FileOutcome::Failed(err) => {
                let title = if import {
                    "Import failed"
                } else {
                    "Export failed"
                };
                ctx.toasts.error(title).message(err);
            }
        }
    }

    fn header(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let connected = ctx.connected();
        let n = self.draft.entries().len();
        let subtitle = if n == 0 {
            "Firmware settings".to_owned()
        } else {
            format!("{n} settings reported by the firmware")
        };
        let mut confirm = None;
        // Narrow tabs get icon buttons so the title keeps its room.
        let compact = ui.available_width() < 780.0;
        kit::page_header(ui, icons::TUNING, "Tuning", Some(&subtitle), |ui| {
            ui.add_enabled_ui(connected, |ui| {
                let actions = [
                    (
                        Confirm::Save,
                        "Save to EEPROM",
                        icons::SAVE,
                        "M500: keep the active settings after a restart",
                    ),
                    (
                        Confirm::Load,
                        "Load from EEPROM",
                        icons::DOWNLOAD,
                        "M501: restore the settings last saved to EEPROM",
                    ),
                    (
                        Confirm::FactoryReset,
                        "Factory reset",
                        icons::RESTART,
                        "M502: firmware defaults (not saved until M500)",
                    ),
                ];
                for (which, label, icon, tip) in actions {
                    let clicked = if compact {
                        let button = IconButton::new(icon).tooltip(format!("{label} ({tip})"));
                        let button = match which {
                            Confirm::Save => button.primary(),
                            Confirm::FactoryReset => button.ghost(),
                            Confirm::Load => button,
                        };
                        ui.add(button).clicked()
                    } else {
                        let button = ActionButton::new(label).icon(icon).tooltip(tip);
                        let button = match which {
                            Confirm::Save => button.primary(),
                            Confirm::FactoryReset => button.ghost(),
                            Confirm::Load => button,
                        };
                        ui.add(button).clicked()
                    };
                    if clicked {
                        confirm = Some(which);
                    }
                }
            });
        });
        if confirm.is_some() {
            self.confirm = confirm;
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let p = theme::ui_palette(ui);
        let connected = ctx.connected();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let search_w = (ui.available_width() - 4.0 * 36.0 - 24.0).clamp(140.0, 320.0);
            ui.scope(|ui| {
                ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.border);
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text(format!("{}  Search settings", icons::SEARCH))
                        .desired_width(search_w)
                        .min_size(vec2(0.0, 28.0))
                        .margin(egui::Margin::symmetric(8, 6)),
                );
            });
            if !self.search.is_empty()
                && ui
                    .add(
                        IconButton::new(icons::CLOSE)
                            .ghost()
                            .small()
                            .tooltip("Clear the search"),
                    )
                    .clicked()
            {
                self.search.clear();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let busy = self.file.is_some();
                if ui
                    .add_enabled(
                        !busy,
                        IconButton::new(icons::UPLOAD).tooltip("Import from a file…"),
                    )
                    .clicked()
                {
                    self.file = Some(FileJob::Import(FileTask::open_text(
                        ui.ctx(),
                        "Import settings",
                        ("Settings / G-code", &["gcode", "gco", "g", "txt", "cfg"]),
                    )));
                }
                if ui
                    .add_enabled(
                        !busy && !self.draft.entries().is_empty(),
                        IconButton::new(icons::DOWNLOAD).tooltip("Export to a file…"),
                    )
                    .clicked()
                {
                    let text = self.export_text(ctx);
                    self.file = Some(FileJob::Export(FileTask::save_text(
                        ui.ctx(),
                        "Export settings",
                        "printer-settings.gcode",
                        ("G-code", &["gcode"]),
                        text,
                    )));
                }
                if ui
                    .add_enabled(
                        !self.draft.entries().is_empty(),
                        IconButton::new(icons::COPY).tooltip("Copy all as G-code"),
                    )
                    .clicked()
                {
                    ui.ctx().copy_text(self.draft.to_gcode());
                    ctx.toasts
                        .success("Settings copied")
                        .message(format!("{} lines of G-code", self.draft.entries().len()));
                }
                if ui
                    .add_enabled(
                        connected,
                        IconButton::new(icons::REFRESH).tooltip("Reload from the printer (M503)"),
                    )
                    .clicked()
                {
                    ctx.printer.request_settings();
                }
            });
        });
    }

    fn body(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let avail = ui.available_rect_before_wrap();
        let modified = self.draft.modified_count();
        let invalid = self.draft.invalid_count();
        if modified == 0 {
            self.show_changes = false;
        }
        let bar_h = if modified > 0 {
            let list_h = if self.show_changes {
                (self.draft.changes().len() as f32 * 44.0 + 12.0).min(220.0)
            } else {
                0.0
            };
            let rows = if avail.width() < 584.0 { 2.0 } else { 1.0 };
            20.0 + 36.0 * rows + list_h + if invalid > 0 { 18.0 } else { 0.0 }
        } else {
            0.0
        };
        let gap = theme::GAP_LG;
        let body_bottom =
            (avail.max.y - if bar_h > 0.0 { bar_h + gap } else { 0.0 }).max(avail.min.y + 80.0);
        let body = Rect::from_min_max(avail.min, pos2(avail.max.x, body_bottom));
        let show_list = avail.width() >= LIST_MIN_TOTAL;
        let cards_rect = if show_list {
            let list_rect = Rect::from_min_max(body.min, pos2(body.min.x + LIST_WIDTH, body.max.y));
            let mut list = ui.new_child(
                UiBuilder::new()
                    .id_salt("tuning-categories")
                    .max_rect(list_rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            egui::ScrollArea::vertical()
                .id_salt("tuning-category-scroll")
                .auto_shrink([false, false])
                .show(&mut list, |ui| {
                    cards::category_list(ui, &self.draft, &mut self.filter);
                });
            Rect::from_min_max(pos2(list_rect.max.x + gap, body.min.y), body.max)
        } else {
            body
        };
        let mut right = ui.new_child(
            UiBuilder::new()
                .id_salt("tuning-cards")
                .max_rect(cards_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        if !show_list {
            self.compact_filter(&mut right);
        }
        if self.filter == Filter::Modified && modified == 0 {
            self.filter = Filter::All;
        }
        let groups = cards::groups(&self.draft, self.filter, &self.search);
        egui::ScrollArea::vertical()
            .id_salt("tuning-cards-scroll")
            .auto_shrink([false, false])
            .show(&mut right, |ui| {
                if groups.is_empty() {
                    ui.add_space(24.0);
                    let _ = EmptyState::new(icons::SEARCH, "Nothing matches")
                        .message(
                            "Try another word: a code (M92), a name (steps) or a parameter (jerk).",
                        )
                        .show(ui);
                    return;
                }
                for (i, (code, indices)) in groups.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(theme::GAP_LG);
                    }
                    match code.as_deref().and_then(setting_def) {
                        Some(def) => cards::setting_card(ui, def, &mut self.draft, indices),
                        None => cards::raw_card(ui, &mut self.draft, indices),
                    }
                }
                ui.add_space(theme::GAP);
            });
        if modified > 0 {
            let bar = Rect::from_min_max(pos2(avail.min.x, body.max.y + gap), avail.max);
            let mut bar_ui = ui.new_child(
                UiBuilder::new()
                    .id_salt("tuning-changes")
                    .max_rect(bar)
                    .layout(Layout::top_down(Align::Min)),
            );
            self.changes_bar(&mut bar_ui, ctx, modified, invalid);
        }
        ui.advance_cursor_after_rect(avail);
    }

    /// A category picker for narrow layouts.
    fn compact_filter(&mut self, ui: &mut Ui) {
        let label = match self.filter {
            Filter::All => "All settings".to_owned(),
            Filter::Modified => "Modified".to_owned(),
            Filter::Category(c) => c.name().to_owned(),
        };
        egui::ComboBox::from_id_salt("tuning-category-combo")
            .selected_text(label)
            .width(ui.available_width().min(260.0))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut self.filter, Filter::All, "All settings");
                if self.draft.modified_count() > 0 {
                    ui.selectable_value(&mut self.filter, Filter::Modified, "Modified");
                }
                for (cat, count, _) in self.draft.categories() {
                    ui.selectable_value(
                        &mut self.filter,
                        Filter::Category(cat),
                        format!("{} ({count})", cat.name()),
                    );
                }
            });
        ui.add_space(theme::GAP);
    }

    fn changes_bar(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>, modified: usize, invalid: usize) {
        let p = theme::ui_palette(ui);
        let connected = ctx.connected();
        let changes = self.draft.changes();
        let mut apply = false;
        let mut revert = false;
        card_frame(&p)
            .inner_margin(egui::Margin::symmetric(12, 8))
            .stroke(egui::Stroke::new(1.0, theme::with_alpha(p.accent, 0.45)))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                if self.show_changes {
                    egui::ScrollArea::vertical()
                        .id_salt("tuning-diff")
                        .max_height(208.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for change in &changes {
                                change_row(ui, change);
                            }
                        });
                    ui.separator();
                }
                let narrow = ui.available_width() < 560.0;
                let mut summary = |ui: &mut Ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(10.0, 24.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 4.0, p.accent);
                    ui.label(
                        RichText::new(format!(
                            "{modified} unapplied change{}",
                            if modified == 1 { "" } else { "s" }
                        ))
                        .font(theme::font(ui, TextRole::Title))
                        .color(p.text),
                    );
                    let toggle = if self.show_changes {
                        "Hide changes"
                    } else {
                        "Show changes"
                    };
                    if ui
                        .add(
                            ActionButton::new(toggle)
                                .icon(if self.show_changes {
                                    icons::CARET_DOWN
                                } else {
                                    icons::CARET_UP
                                })
                                .ghost()
                                .small(),
                        )
                        .clicked()
                    {
                        self.show_changes = !self.show_changes;
                    }
                };
                let mut buttons = |ui: &mut Ui| {
                    let label = if changes.len() == 1 {
                        "Apply change".to_owned()
                    } else {
                        format!("Apply {} changes", changes.len())
                    };
                    let tip = if connected {
                        "Send the changed settings to the printer, then reload them (M503)"
                    } else {
                        "Connect to apply"
                    };
                    if ui
                        .add_enabled(
                            connected && !changes.is_empty(),
                            ActionButton::new(&label)
                                .icon(icons::CHECK)
                                .primary()
                                .tooltip(tip),
                        )
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui
                        .add(
                            ActionButton::new("Revert")
                                .icon(icons::RESET)
                                .tooltip("Discard every edit"),
                        )
                        .clicked()
                    {
                        revert = true;
                    }
                };
                if narrow {
                    ui.horizontal(|ui| summary(ui));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.set_height(30.0);
                        buttons(ui);
                    });
                } else {
                    ui.horizontal(|ui| {
                        summary(ui);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| buttons(ui));
                    });
                }
                if invalid > 0 {
                    ui.label(
                        RichText::new(format!(
                            "{invalid} raw line{} cannot be parsed and will not be applied.",
                            if invalid == 1 { "" } else { "s" }
                        ))
                        .size(12.0)
                        .color(p.danger),
                    );
                }
            });
        if revert {
            self.draft.revert_all();
        }
        if apply {
            for change in &changes {
                ctx.printer.apply_setting(&change.command);
            }
            ctx.printer.request_settings();
            ctx.toasts
                .success(format!(
                    "Applied {} setting{}",
                    changes.len(),
                    if changes.len() == 1 { "" } else { "s" }
                ))
                .message("Active until the next restart. Save to EEPROM to keep them.");
            self.show_changes = false;
        }
    }

    fn dialogs(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        if let Some(confirm) = self.confirm {
            let pending = self.draft.modified_count();
            let pending_note = if pending > 0 {
                format!(
                    "\n\n{pending} edit{} in the editor {} not applied yet.",
                    if pending == 1 { "" } else { "s" },
                    if pending == 1 { "is" } else { "are" }
                )
            } else {
                String::new()
            };
            let answer = match confirm {
                Confirm::Save => {
                    let message = format!(
                        "M500 writes the printer's active settings to its EEPROM, so they survive \
                         a restart or power cycle.{pending_note}"
                    );
                    ConfirmDialog::new("tuning-save", "Save settings to EEPROM?")
                        .message(&message)
                        .details("M500")
                        .icon(icons::SAVE)
                        .confirm_label("Save to EEPROM")
                        .show(ui.ctx())
                }
                Confirm::Load => {
                    let message = format!(
                        "M501 replaces the active settings with the ones last saved to EEPROM. \
                         Settings applied since the last save are lost.{pending_note}"
                    );
                    ConfirmDialog::new("tuning-load", "Load settings from EEPROM?")
                        .message(&message)
                        .details("M501\nM503")
                        .icon(icons::DOWNLOAD)
                        .confirm_label("Load from EEPROM")
                        .show(ui.ctx())
                }
                Confirm::FactoryReset => {
                    ConfirmDialog::new("tuning-reset", "Reset to factory defaults?")
                        .message(
                            "M502 replaces the active settings with the firmware's compiled-in \
                         defaults: steps/mm, PID gains, offsets and everything else you tuned. \
                         Nothing is permanent until you save to EEPROM (M500); until then, Load \
                         from EEPROM or a restart brings your saved settings back.",
                        )
                        .details("M502\nM503")
                        .confirm_label("Reset to defaults")
                        .danger(true)
                        .show(ui.ctx())
                }
            };
            match answer {
                Some(DialogResult::Confirmed) => {
                    self.confirm = None;
                    match confirm {
                        Confirm::Save => {
                            ctx.printer.save_settings();
                            ctx.toasts.success("Settings saved to EEPROM");
                        }
                        Confirm::Load => {
                            ctx.printer.load_settings();
                            ctx.toasts.info("Settings loaded from EEPROM");
                        }
                        Confirm::FactoryReset => {
                            ctx.printer.factory_reset_settings();
                            ctx.toasts.warning("Factory defaults active").message(
                                "Save to EEPROM to keep them, or load from EEPROM to undo.",
                            );
                        }
                    }
                }
                Some(DialogResult::Cancelled) => self.confirm = None,
                None => {}
            }
        }
        if self.import.is_some() {
            self.import_dialog(ui, ctx);
        }
    }

    fn import_dialog(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let Some(preview) = &self.import else {
            return;
        };
        let p = theme::palette(ui.ctx());
        let changed = preview.changed();
        let unchanged = preview.diffs.len() - changed;
        let mut stage = false;
        let mut close = false;
        let frame = egui::Frame::new()
            .fill(p.surface)
            .stroke(egui::Stroke::new(
                1.0,
                if p.dark { p.border_strong } else { p.border },
            ))
            .corner_radius(egui::CornerRadius::same(theme::RADIUS_XL))
            .inner_margin(egui::Margin::same(20))
            .shadow(ui.ctx().global_style().visuals.window_shadow);
        let modal = egui::Modal::new(egui::Id::new("tuning-import-preview"))
            .frame(frame)
            .show(ui.ctx(), |ui| {
                ui.set_width(520.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icons::UPLOAD)
                            .font(kit::icon_font(ui, 20.0))
                            .color(p.accent),
                    );
                    ui.label(
                        RichText::new("Import settings")
                            .font(theme::builtin_font(ui, egui::TextStyle::Heading))
                            .color(p.text),
                    );
                });
                ui.add_space(2.0);
                kit::dim(
                    ui,
                    &format!(
                        "{}: {} change{}, {unchanged} unchanged{}.",
                        preview.name,
                        changed,
                        if changed == 1 { "" } else { "s" },
                        if preview.import.skipped.is_empty() {
                            String::new()
                        } else {
                            format!(
                                ", {} line{} skipped",
                                preview.import.skipped.len(),
                                if preview.import.skipped.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                }
                            )
                        }
                    ),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .id_salt("import-diff")
                    .max_height(300.0)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (cmd, diff) in &preview.diffs {
                            import_row(ui, cmd, diff);
                        }
                        if !preview.import.skipped.is_empty() {
                            ui.add_space(6.0);
                            kit::caption(ui, "SKIPPED LINES");
                            for s in preview.import.skipped.iter().take(50) {
                                ui.label(
                                    RichText::new(format!(
                                        "{:>4}  {}  ({})",
                                        s.line, s.text, s.reason
                                    ))
                                    .monospace()
                                    .size(11.5)
                                    .color(p.text_dim),
                                );
                            }
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if changed == 1 {
                            "Stage 1 change".to_owned()
                        } else {
                            format!("Stage {changed} changes")
                        };
                        if ui
                            .add_enabled(
                                changed > 0,
                                ActionButton::new(&label).primary().tooltip(
                                    "Load the changes into the editor; review and apply them there",
                                ),
                            )
                            .clicked()
                        {
                            stage = true;
                        }
                        if ui.add(ActionButton::new("Cancel")).clicked() {
                            close = true;
                        }
                    });
                });
            });
        if modal.should_close() {
            close = true;
        }
        if stage && let Some(preview) = self.import.take() {
            let mut staged = 0;
            for (cmd, diff) in &preview.diffs {
                if !matches!(diff, ImportDiff::Unchanged) {
                    self.draft.stage_import(cmd);
                    staged += 1;
                }
            }
            self.show_changes = true;
            ctx.toasts
                .info(format!(
                    "Staged {staged} imported setting{}",
                    if staged == 1 { "" } else { "s" }
                ))
                .message("Review them below and apply them to the printer.");
        } else if close {
            self.import = None;
        }
    }
}

/// One row of the pending-changes list.
fn change_row(ui: &mut Ui, change: &model::Change) {
    let p = theme::ui_palette(ui);
    ui.horizontal_top(|ui| {
        ui.add(Badge::new(&change.command.code).tone(Tone::Accent));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut title = change.name.clone();
            if !change.instance.is_empty() {
                title.push_str(" · ");
                title.push_str(&change.instance);
            }
            ui.label(RichText::new(title).size(13.0).color(p.text));
            for line in change.lines() {
                ui.label(RichText::new(line).size(12.0).color(p.text_dim));
            }
        });
    });
    ui.add_space(4.0);
}

/// One row of the import preview.
fn import_row(ui: &mut Ui, cmd: &sprint_proto::SettingCommand, diff: &ImportDiff) {
    let p = theme::ui_palette(ui);
    let name = setting_def(&cmd.code).map_or(cmd.code.as_str(), |d| d.name);
    ui.horizontal_top(|ui| {
        let (text, tone) = match diff {
            ImportDiff::Unchanged => ("SAME", Tone::Neutral),
            ImportDiff::Changed(_) => ("CHANGE", Tone::Accent),
            ImportDiff::New => ("NEW", Tone::Info),
        };
        ui.add(Badge::new(text).tone(tone));
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(
                RichText::new(format!("{name} · {}", cmd.key()))
                    .size(13.0)
                    .color(if matches!(diff, ImportDiff::Unchanged) {
                        p.text_dim
                    } else {
                        p.text
                    }),
            );
            match diff {
                ImportDiff::Changed(params) if !params.is_empty() => {
                    for c in params {
                        ui.label(
                            RichText::new(format!("{}: {}", c.label, c.describe()))
                                .size(12.0)
                                .color(p.text_dim),
                        );
                    }
                }
                ImportDiff::Changed(_) | ImportDiff::New => {
                    ui.label(
                        RichText::new(cmd.to_gcode())
                            .monospace()
                            .size(12.0)
                            .color(p.text_dim),
                    );
                }
                ImportDiff::Unchanged => {}
            }
        });
    });
    ui.add_space(3.0);
}

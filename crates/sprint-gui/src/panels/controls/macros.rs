//! Macros: a grid of the user's G-code buttons (from the config), with an
//! editor dialog, a context menu to edit, duplicate, reorder and delete,
//! and a set of useful defaults.
//!
//! Running a macro: its arguments (`{0}`, `{1=200}`, `{args}`, see
//! [`crate::macro_args`]) are asked for in a small dialog, never sent as
//! placeholders; the G-code it sends, with `{x}`, `{heaters_off}` and the
//! other script placeholders rendered, then goes through the shared
//! command-safety classifier ([`crate::safety`]), which decides whether it
//! runs at once, asks first (also when the macro asks for confirmation)
//! or is refused.

use egui::color_picker::{self, Alpha};
use egui::ecolor::Hsva;
use egui::{
    Align, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Mesh, Modal,
    Popup, PopupCloseBehavior, Response, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit,
    Ui, WidgetInfo, WidgetType, text::LayoutJob, vec2,
};
use sprint_host::{Macro, PrinterProfile, PrinterState};
use sprint_proto::JobContext;

use super::motion::refresh_position;
use super::{caption, section};
use crate::context::AppCtx;
use crate::macro_args::{self, MacroParams};
use crate::safety::{self, Assessment};
use crate::theme::{self, Palette, TextRole, mix, readable_text_on, with_alpha};
use crate::widgets::{ActionButton, ConfirmDialog, DialogResult, IconButton, Toggle};
use crate::{format, icons};

/// Colors offered in the editor (besides "none" and a custom picker).
const SWATCHES: [[u8; 3]; 8] = [
    [0xFF, 0x7A, 0x1A], // orange
    [0xE5, 0x48, 0x4D], // red
    [0xF5, 0xB8, 0x3D], // amber
    [0x3F, 0xCF, 0x8E], // green
    [0x3C, 0xC6, 0xC6], // teal
    [0x4D, 0xA3, 0xFF], // blue
    [0xB1, 0x8C, 0xFF], // purple
    [0xFF, 0x6F, 0xAE], // pink
];

/// Height of a macro button.
const TILE_HEIGHT: f32 = 34.0;
/// Lines of G-code shown in tooltips and confirmations.
const PREVIEW_LINES: usize = 12;
/// Preview lines longer than this many characters are cut.
const PREVIEW_LINE_CHARS: usize = 72;
/// Height of everything in the macro editor but the G-code field, plus a
/// margin to the window edges (points).
const EDITOR_CHROME: f32 = 440.0;
/// The G-code field scrolls beyond this height (points).
const MAX_CODE_HEIGHT: f32 = 360.0;
/// ... and never gets shorter than this, even in a tiny window.
const MIN_CODE_HEIGHT: f32 = 96.0;

/// The commands a macro sends, as the printer will see them (comments and
/// blank lines dropped; `{placeholders}` are filled in when it runs).
pub(super) fn macro_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(sprint_host::sprint_gcode::clean_line)
        .map(|line| line.into_owned())
        .collect()
}

/// Why the classifier would ask before a macro with `text` runs on an idle
/// printer (dangerous commands such as M502), for the editor.
pub(super) fn idle_reasons(text: &str) -> Vec<String> {
    safety::assess(&macro_lines(text), JobContext::Idle).confirm
}

/// Whether a macro with `text` holds an emergency stop (M112): it goes out
/// at once when the macro runs, without the macro's own question (see
/// [`Macros::check_and_run`]).
pub(super) fn stops_at_once(text: &str) -> bool {
    safety::assess(&macro_lines(text), JobContext::Idle).emergency_stop
}

/// What a macro sends now: its G-code (arguments filled in) with the script
/// placeholders rendered from the printer's state and the profile, one
/// command per line.
pub(super) fn rendered(gcode: &str, state: &PrinterState, profile: &PrinterProfile) -> String {
    macro_args::rendered(gcode, state, profile).join("\n")
}

/// A short preview of a macro's G-code (first lines, "…" when cut).
fn preview(text: &str) -> String {
    let mut lines = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty());
    let mut out = lines
        .by_ref()
        .take(PREVIEW_LINES)
        .map(|line| {
            if line.chars().count() > PREVIEW_LINE_CHARS {
                let mut cut: String = line.chars().take(PREVIEW_LINE_CHARS - 1).collect();
                cut.push('…');
                cut
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let more = lines.count();
    if more > 0 {
        out.push_str(&format!("\n… {more} more lines"));
    }
    out
}

fn new_macro(name: &str, gcode: &str, color: Option<[u8; 3]>, confirm: bool) -> Macro {
    Macro {
        name: name.to_owned(),
        gcode: gcode.to_owned(),
        color,
        confirm,
    }
}

/// Useful starter macros for `profile`: preheat with its PLA preset, level
/// corners (home, then each corner at Z0.2 with a pause), filament change
/// and a settings report.
pub(super) fn default_macros(profile: &PrinterProfile) -> Vec<Macro> {
    let usable = |t: f32| t.is_finite() && t > 0.0;
    let preset = profile
        .temp_presets
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case("PLA") && usable(p.hotend))
        .or_else(|| profile.temp_presets.iter().find(|p| usable(p.hotend)));
    let (name, hotend, bed) =
        preset.map_or(("PLA", 205.0, 60.0), |p| (p.name.as_str(), p.hotend, p.bed));
    let num = |v: f32| sprint_host::format_number(f64::from(v), 1);
    let mut preheat = String::new();
    if profile.heated_bed && usable(bed) {
        preheat.push_str(&format!("M140 S{}\n", num(bed)));
    }
    preheat.push_str(&format!("M104 S{}", num(hotend)));

    let (min, max) = profile.build_volume();
    let inset = |lo: f32, hi: f32| ((hi - lo) * 0.15).clamp(5.0, 30.0).min((hi - lo) / 2.0);
    let (ix, iy) = (inset(min[0], max[0]), inset(min[1], max[1]));
    let (x0, x1, y0, y1) = (min[0] + ix, max[0] - ix, min[1] + iy, max[1] - iy);
    let mut level = String::from(
        "; Home, then visit each corner at Z0.2 and wait (M0) while you\n\
         ; adjust its screw with a sheet of paper under the nozzle.\nG28\nG90\n",
    );
    for (corner, x, y) in [
        ("front left", x0, y0),
        ("front right", x1, y0),
        ("back right", x1, y1),
        ("back left", x0, y1),
    ] {
        level.push_str(&format!(
            "G1 Z5 F600\nG1 X{} Y{} F3000\nG1 Z0.2 F600\nM0 Adjust the {corner} corner\n",
            num(x),
            num(y)
        ));
    }
    level.push_str("G1 Z10 F600");

    vec![
        new_macro(
            &format!("Preheat {name}"),
            &preheat,
            Some(SWATCHES[0]),
            false,
        ),
        new_macro("Level corners", &level, Some(SWATCHES[5]), true),
        new_macro("Filament change", "M600", Some(SWATCHES[6]), true),
        new_macro("Report settings", "M503", None, false),
    ]
}

/// Send a macro, then ask for the position it may have changed (see
/// [`refresh_position`]).
fn run_macro(ctx: &AppCtx<'_>, m: &Macro) {
    ctx.printer.run_macro(m);
    refresh_position(ctx.printer, ctx.state);
}

/// [`run_macro`]; a `test` send from the editor says so in a toast.
fn run_now(ctx: &mut AppCtx<'_>, m: &Macro, test: bool) {
    run_macro(ctx, m);
    if test {
        let name = m.name.trim();
        let shown = if name.is_empty() { "macro" } else { name };
        ctx.toasts.info(format!("Sent {shown}"));
    }
}

/// A change to the macro list, applied after the grid is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListOp {
    Edit(usize),
    Duplicate(usize),
    MoveEarlier(usize),
    MoveLater(usize),
    Delete(usize),
}

/// Apply a list operation; returns whether the list changed.
fn apply_op(macros: &mut Vec<Macro>, op: ListOp) -> bool {
    match op {
        ListOp::Edit(_) | ListOp::Delete(_) => false,
        ListOp::Duplicate(i) => {
            let Some(m) = macros.get(i) else {
                return false;
            };
            let mut copy = m.clone();
            copy.name = format!("{} (copy)", copy.name);
            macros.insert(i + 1, copy);
            true
        }
        ListOp::MoveEarlier(i) if i > 0 && i < macros.len() => {
            macros.swap(i, i - 1);
            true
        }
        ListOp::MoveLater(i) if i + 1 < macros.len() => {
            macros.swap(i, i + 1);
            true
        }
        ListOp::MoveEarlier(_) | ListOp::MoveLater(_) => false,
    }
}

/// The macro editor dialog's state.
#[derive(Clone, Debug)]
struct Editor {
    /// Index of the edited macro; `None` for a new one.
    index: Option<usize>,
    draft: Macro,
    /// The macro as it was when the editor opened.
    original: Macro,
    /// The custom color picker's value.
    custom: [u8; 3],
    /// Focus the name field on the first frame.
    focus_name: bool,
}

impl Editor {
    fn new(index: Option<usize>, draft: Macro) -> Editor {
        let custom = draft.color.unwrap_or([0x8A, 0x93, 0xA0]);
        Editor {
            index,
            original: draft.clone(),
            draft,
            custom,
            focus_name: true,
        }
    }

    /// Whether the draft differs from what the editor opened with.
    fn is_modified(&self) -> bool {
        self.draft != self.original
    }
}

/// A macro waiting for "Run?" confirmation.
#[derive(Clone, Debug)]
struct PendingRun {
    /// The macro with its arguments filled in.
    run: Macro,
    /// Why it asks (empty when only the macro itself asks).
    check: Assessment,
    /// What the job was doing then.
    job: JobContext,
    /// A test send from the editor (a toast says it was sent).
    test: bool,
}

/// A macro waiting for its arguments.
#[derive(Clone, Debug)]
struct ArgsDialog {
    run: Macro,
    params: MacroParams,
    fields: Vec<String>,
    /// Focus the first field on the first frame.
    focus: bool,
    /// A test send from the editor.
    test: bool,
}

/// UI state of the macros section.
#[derive(Debug, Default)]
pub(super) struct Macros {
    editor: Option<Editor>,
    /// A macro waiting for "Run?" confirmation.
    pending_run: Option<PendingRun>,
    /// A macro waiting for "Delete?" confirmation.
    pending_delete: Option<usize>,
    /// A macro waiting for its arguments.
    args: Option<ArgsDialog>,
}

impl Macros {
    /// Run `m` as the user asked: its arguments first, when it takes any
    /// (see [`crate::macro_args`]), then [`Macros::check_and_run`]. `test`:
    /// a test send from the editor.
    fn request_run(&mut self, ctx: &mut AppCtx<'_>, m: &Macro, test: bool) {
        let params = macro_args::params(&m.gcode);
        if params.any() {
            self.args = Some(ArgsDialog {
                run: m.clone(),
                fields: params.initial_fields(),
                params,
                focus: true,
                test,
            });
            return;
        }
        self.check_and_run(ctx, m.clone(), test);
    }

    /// Run `m` (its arguments filled in) through the command-safety
    /// classifier: at once, after a question (also when the macro asks
    /// for one), or not at all. A macro holding an M112 sends the
    /// emergency stop at once, never after a question, and nothing else
    /// (the firmware halts), as the console does.
    fn check_and_run(&mut self, ctx: &mut AppCtx<'_>, m: Macro, test: bool) {
        let lines = macro_args::rendered(&m.gcode, ctx.state, ctx.config.active());
        let job = safety::job_context(ctx.state);
        let check = safety::assess(&lines, job);
        if check.emergency_stop {
            ctx.printer.emergency_stop();
            let toast = ctx.toasts.warning("Emergency stop sent");
            if check.commands > 1 {
                toast.message(format!(
                    "M112 went out at once; the rest of “{}” was not sent (the firmware halts).",
                    m.name.trim()
                ));
            } else {
                toast.message(crate::job_text::ESTOP_MESSAGE);
            }
        } else if !check.blocked.is_empty() {
            ctx.toasts
                .warning(format!("“{}” not run", m.name.trim()))
                .message(check.blocked.join("\n"));
        } else if m.confirm || !check.confirm.is_empty() {
            self.pending_run = Some(PendingRun {
                run: m,
                check,
                job,
                test,
            });
        } else {
            run_now(ctx, &m, test);
        }
    }

    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let connected = ctx.connected();
        let mut op = None;
        let mut run = None;
        let mut add_header = false;
        let mut add = false;
        let mut defaults = false;
        let macros = &ctx.config.macros;
        section(
            ui,
            icons::MACRO,
            "Macros",
            |ui| {
                add_header |= ui
                    .add(
                        IconButton::new(icons::ADD)
                            .ghost()
                            .small()
                            .tooltip("New macro"),
                    )
                    .clicked();
            },
            |ui| {
                if macros.is_empty() {
                    caption(
                        ui,
                        "No macros yet. Macros are buttons that send your own G-code, \
                         with {x} {y} {z} {hotend} {bed} … filled in when they run.",
                    );
                    ui.columns(2, |cols| {
                        defaults |= cols[0]
                            .add(
                                ActionButton::new("Add defaults")
                                    .icon(icons::MACRO)
                                    .primary()
                                    .fill_width(true)
                                    .tooltip(
                                        "Preheat PLA, Level corners, Filament change (M600) \
                                         and Report settings (M503)",
                                    ),
                            )
                            .clicked();
                        add |= cols[1]
                            .add(
                                ActionButton::new("New macro")
                                    .icon(icons::ADD)
                                    .fill_width(true),
                            )
                            .clicked();
                    });
                    return;
                }
                let gap = 6.0;
                let width = ui.available_width();
                let columns = if width >= 330.0 { 3 } else { 2 };
                let tile_w = ((width - gap * (columns - 1) as f32) / columns as f32).max(40.0);
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                let preview_with = (ctx.state, &*ctx.config);
                for (row, chunk) in macros.chunks(columns).enumerate() {
                    ui.horizontal(|ui| {
                        for (col, m) in chunk.iter().enumerate() {
                            let index = row * columns + col;
                            let r = macro_tile(ui, m, tile_w, connected, index, preview_with);
                            let r = if connected {
                                r
                            } else {
                                r.on_hover_text("Connect to run macros. Right-click to edit.")
                            };
                            if r.clicked() && connected {
                                run = Some(index);
                            }
                            r.context_menu(|ui| {
                                ui.set_min_width(170.0);
                                if ui.button(icons::with_text(icons::EDIT, "Edit…")).clicked() {
                                    op = Some(ListOp::Edit(index));
                                }
                                if ui
                                    .button(icons::with_text(icons::COPY, "Duplicate"))
                                    .clicked()
                                {
                                    op = Some(ListOp::Duplicate(index));
                                }
                                if ui
                                    .add_enabled(
                                        index > 0,
                                        egui::Button::new(icons::with_text(
                                            icons::CARET_LEFT,
                                            "Move earlier",
                                        )),
                                    )
                                    .clicked()
                                {
                                    op = Some(ListOp::MoveEarlier(index));
                                }
                                if ui
                                    .add_enabled(
                                        index + 1 < macros.len(),
                                        egui::Button::new(icons::with_text(
                                            icons::CARET_RIGHT,
                                            "Move later",
                                        )),
                                    )
                                    .clicked()
                                {
                                    op = Some(ListOp::MoveLater(index));
                                }
                                ui.separator();
                                let p = theme::ui_palette(ui);
                                if ui
                                    .button(
                                        RichText::new(icons::with_text(icons::DELETE, "Delete…"))
                                            .color(p.danger),
                                    )
                                    .clicked()
                                {
                                    op = Some(ListOp::Delete(index));
                                }
                            });
                        }
                    });
                }
                caption(ui, "Right-click a macro to edit, reorder or delete it.");
            },
        );
        if let Some(index) = run
            && let Some(m) = ctx.config.macros.get(index).cloned()
        {
            self.request_run(ctx, &m, false);
        }
        if add || add_header {
            self.editor = Some(Editor::new(None, new_macro("", "", None, false)));
        }
        if defaults {
            let profile = ctx.config.active().clone();
            ctx.config.macros.extend(default_macros(&profile));
            *ctx.config_dirty = true;
        }
        match op {
            Some(ListOp::Edit(i)) => {
                if let Some(m) = ctx.config.macros.get(i) {
                    self.editor = Some(Editor::new(Some(i), m.clone()));
                }
            }
            Some(ListOp::Delete(i)) => self.pending_delete = Some(i),
            Some(other) => *ctx.config_dirty |= apply_op(&mut ctx.config.macros, other),
            None => {}
        }
    }

    /// Show the open dialogs (run confirmation, delete confirmation, the
    /// editor, the arguments). Called once per frame after the sidebar.
    pub(super) fn dialogs(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        let egui_ctx = ui.ctx().clone();
        // The editor first: a confirmation it opens draws above it.
        let mut test_send = None;
        if let Some(editor) = &mut self.editor {
            match editor_dialog(&egui_ctx, editor, ctx) {
                EditorOutcome::Open => {}
                EditorOutcome::TestSend => {
                    let mut draft = editor.draft.clone();
                    // The draft's own question is about running it from
                    // its button; testing it is what the user just asked.
                    draft.confirm = false;
                    test_send = Some(draft);
                }
                EditorOutcome::Cancel => self.editor = None,
                EditorOutcome::Save => {
                    let mut saved = editor.draft.clone();
                    saved.name = saved.name.trim().to_owned();
                    match editor.index {
                        Some(i) if i < ctx.config.macros.len() => ctx.config.macros[i] = saved,
                        _ => ctx.config.macros.push(saved),
                    }
                    *ctx.config_dirty = true;
                    self.editor = None;
                }
            }
        }
        if let Some(draft) = test_send {
            self.request_run(ctx, &draft, true);
        }
        self.args_dialog(&egui_ctx, ctx);
        if let Some(pending) = &self.pending_run {
            let title = format!("Run “{}”?", pending.run.name.trim());
            let message = if pending.check.confirm.is_empty() {
                "This macro asks for confirmation before it runs.".to_owned()
            } else {
                pending.check.message(pending.job)
            };
            // What it sends now, its placeholders rendered.
            let details = preview(&rendered(
                &pending.run.gcode,
                ctx.state,
                ctx.config.active(),
            ));
            let danger = !pending.check.confirm.is_empty();
            let answer = ConfirmDialog::new("sprint-controls-macro-run", &title)
                .message(&message)
                .details(&details)
                .confirm_label("Run")
                .icon(icons::MACRO)
                .danger(danger)
                .show(&egui_ctx);
            match answer {
                Some(DialogResult::Confirmed) => {
                    if let Some(pending) = self.pending_run.take() {
                        // The job may have moved on meanwhile: what was
                        // agreed to must still be all there is.
                        let lines = macro_args::rendered(
                            &pending.run.gcode,
                            ctx.state,
                            ctx.config.active(),
                        );
                        let job = safety::job_context(ctx.state);
                        let check = safety::assess(&lines, job);
                        if !ctx.connected() || ctx.state.halted {
                            ctx.toasts
                                .warning(format!("“{}” not run", pending.run.name.trim()))
                                .message("The printer is not ready for it any more.");
                        } else if !check.blocked.is_empty() {
                            ctx.toasts
                                .warning(format!("“{}” not run", pending.run.name.trim()))
                                .message(check.blocked.join("\n"));
                        } else if job != pending.job && !check.is_clear() {
                            self.pending_run = Some(PendingRun {
                                run: pending.run,
                                check,
                                job,
                                test: pending.test,
                            });
                        } else {
                            run_now(ctx, &pending.run, pending.test);
                        }
                    }
                }
                Some(DialogResult::Cancelled) => self.pending_run = None,
                None => {}
            }
        }
        if let Some(index) = self.pending_delete {
            match ctx.config.macros.get(index).map(|m| m.name.clone()) {
                None => self.pending_delete = None,
                Some(name) => {
                    let title = format!("Delete macro “{name}”?");
                    let answer = ConfirmDialog::new("sprint-controls-macro-delete", &title)
                        .message("The button and its G-code are removed.")
                        .confirm_label("Delete")
                        .icon(icons::DELETE)
                        .danger(true)
                        .show(&egui_ctx);
                    match answer {
                        Some(DialogResult::Confirmed) => {
                            ctx.config.macros.remove(index);
                            *ctx.config_dirty = true;
                            self.pending_delete = None;
                        }
                        Some(DialogResult::Cancelled) => self.pending_delete = None,
                        None => {}
                    }
                }
            }
        }
    }

    /// The "arguments of the macro" dialog, while one waits.
    fn args_dialog(&mut self, egui_ctx: &egui::Context, ctx: &mut AppCtx<'_>) {
        let Some(dialog) = &mut self.args else {
            return;
        };
        let p = theme::palette(egui_ctx);
        let args = dialog.params.args(&dialog.fields);
        let filled = macro_args::fill(&dialog.run.gcode, &args);
        let missing = dialog.params.missing(&dialog.fields);
        let mut outcome = None;
        let modal = Modal::new(Id::new("sprint-controls-macro-args"))
            .frame(crate::dialogs::modal_frame(egui_ctx))
            .backdrop_color(crate::dialogs::backdrop(egui_ctx))
            .show(egui_ctx, |ui| {
                ui.set_width(380.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                crate::dialogs::dialog_title(
                    ui,
                    icons::MACRO,
                    &format!("Run “{}”", format::sanitize(dialog.run.name.trim(), 40)),
                    p.accent,
                );
                caption(ui, "This macro takes arguments: fill them in.");
                let numbered = !dialog.params.numbered.is_empty();
                for (i, field) in dialog.fields.iter_mut().enumerate() {
                    let label = if numbered {
                        format!("Argument {{{i}}}")
                    } else {
                        "Arguments (separated by spaces)".to_owned()
                    };
                    field_label(ui, &p, &label);
                    let hint = dialog
                        .params
                        .numbered
                        .get(i)
                        .cloned()
                        .flatten()
                        .map_or_else(String::new, |d| format!("default {d}"));
                    let edit = ui.add(
                        TextEdit::singleline(field)
                            .id(Id::new(("sprint-controls-macro-arg", i)))
                            .hint_text(hint)
                            .desired_width(f32::INFINITY),
                    );
                    if dialog.focus && i == 0 {
                        edit.request_focus();
                        dialog.focus = false;
                    }
                }
                field_label(ui, &p, "Sends");
                let text = match &filled {
                    Ok(gcode) => preview(&rendered(gcode, ctx.state, ctx.config.active())),
                    Err(e) => e.clone(),
                };
                ui.label(
                    RichText::new(text)
                        .font(theme::builtin_font(ui, egui::TextStyle::Monospace))
                        .color(if filled.is_ok() {
                            p.text_dim
                        } else {
                            p.warning
                        }),
                );
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ready = filled.is_ok() && missing.is_empty() && ctx.connected();
                    let run = ui
                        .add_enabled(ready, ActionButton::new("Run").icon(icons::SEND).primary())
                        .on_disabled_hover_text(if ctx.connected() {
                            "Fill in every argument without a default"
                        } else {
                            "Connect to a printer first"
                        });
                    if run.clicked() {
                        outcome = Some(true);
                    }
                    if ui.add(ActionButton::new("Cancel")).clicked() {
                        outcome = Some(false);
                    }
                });
            });
        let enter = modal.is_top_modal
            && filled.is_ok()
            && missing.is_empty()
            && egui_ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
        if enter {
            outcome = Some(true);
        }
        if outcome.is_none() && modal.should_close() {
            outcome = Some(false);
        }
        match (outcome, filled) {
            (Some(true), Ok(gcode)) => {
                if let Some(dialog) = self.args.take() {
                    let run = Macro {
                        gcode,
                        ..dialog.run
                    };
                    self.check_and_run(ctx, run, dialog.test);
                }
            }
            (Some(_), _) => self.args = None,
            (None, _) => {}
        }
    }
}

/// One macro button: its color (or a neutral fill), name truncated to the
/// width, "…" when it asks before running (not with an emergency stop,
/// which never asks) or for arguments.
fn macro_tile(
    ui: &mut Ui,
    m: &Macro,
    width: f32,
    runnable: bool,
    index: usize,
    (state, config): (&PrinterState, &sprint_host::AppConfig),
) -> Response {
    let p = theme::ui_palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::click());
    let id = Id::new(("sprint-controls-macro", index));
    let asks = (m.confirm && !stops_at_once(&m.gcode)) || macro_args::params(&m.gcode).any();
    let label = if asks {
        format!("{}…", m.name.trim())
    } else {
        m.name.trim().to_owned()
    };
    let label = if label.is_empty() {
        "Untitled".to_owned()
    } else {
        label
    };
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, runnable, &label));
    if ui.is_rect_visible(rect) {
        let hover = ui
            .ctx()
            .animate_bool_with_time(id.with("hover"), response.hovered(), 0.08);
        let pressed = response.is_pointer_button_down_on();
        let (base, fg, border) = match m.color {
            Some([r, g, b]) => {
                let c = Color32::from_rgb(r, g, b);
                (c, readable_text_on(c), mix(c, Color32::BLACK, 0.18))
            }
            None => (p.raised, p.text, p.border),
        };
        let lift = if p.dark {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        let fill = if pressed {
            mix(base, Color32::BLACK, 0.18)
        } else {
            mix(base, lift, 0.08 * hover)
        };
        let alpha = if runnable { 1.0 } else { 0.45 };
        let radius = CornerRadius::same(theme::RADIUS);
        let painter = ui.painter_at(rect);
        painter.rect(
            rect,
            radius,
            with_alpha(fill, alpha),
            Stroke::new(
                1.0,
                with_alpha(mix(border, p.border_strong, hover * 0.5), alpha),
            ),
            StrokeKind::Inside,
        );
        let font = FontId::new(13.0, theme::font(ui, TextRole::Title).family);
        let mut job = LayoutJob::simple_singleline(label.clone(), font, with_alpha(fg, alpha));
        job.wrap = egui::text::TextWrapping::truncate_at_width((width - 14.0).max(8.0));
        let galley = ui.painter().layout_job(job);
        painter.galley(
            rect.center() - galley.size() / 2.0,
            galley,
            with_alpha(fg, alpha),
        );
    }
    let name = label;
    response
        .on_hover_cursor(if runnable {
            CursorIcon::PointingHand
        } else {
            CursorIcon::Default
        })
        .on_hover_ui(|ui| {
            // Built only while hovered: a macro can be long. What it sends
            // now, with the placeholders rendered; a macro taking
            // arguments shows its G-code as written.
            let params = macro_args::params(&m.gcode);
            let gcode = if params.any() {
                preview(&m.gcode)
            } else {
                preview(&rendered(&m.gcode, state, config.active()))
            };
            let p = theme::ui_palette(ui);
            ui.label(RichText::new(name).font(theme::font(ui, TextRole::Title)));
            if gcode.is_empty() {
                ui.label(RichText::new("No G-code").color(p.text_dim));
            } else {
                ui.label(
                    RichText::new(gcode)
                        .font(theme::builtin_font(ui, egui::TextStyle::Monospace))
                        .color(p.text_dim),
                );
            }
            if params.any() {
                ui.label(
                    RichText::new("Asks for its arguments when it runs.")
                        .size(12.0)
                        .color(p.text_faint),
                );
            }
        })
}

/// What the editor dialog wants this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorOutcome {
    Open,
    /// "Test send" was clicked (the editor stays open).
    TestSend,
    Cancel,
    Save,
}

fn editor_dialog(ctx: &egui::Context, editor: &mut Editor, app: &mut AppCtx<'_>) -> EditorOutcome {
    let p = theme::palette(ctx);
    let frame = Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(
            1.0,
            if p.dark { p.border_strong } else { p.border },
        ))
        .corner_radius(CornerRadius::same(theme::RADIUS_XL))
        .inner_margin(Margin::same(20))
        .shadow(ctx.global_style().visuals.window_shadow);
    let backdrop = Color32::from_black_alpha(if p.dark { 150 } else { 90 });
    let mut outcome = EditorOutcome::Open;
    let screen = ctx.content_rect();
    let width = (screen.width() - 48.0).clamp(280.0, 480.0);
    let code_height = (screen.height() - EDITOR_CHROME).clamp(MIN_CODE_HEIGHT, MAX_CODE_HEIGHT);
    let lines = macro_lines(&editor.draft.gcode);
    let dangerous = idle_reasons(&editor.draft.gcode);
    let params = macro_args::params(&editor.draft.gcode);
    let name_ok = !editor.draft.name.trim().is_empty();
    let can_save = name_ok && !lines.is_empty();
    // A test run is for an idle printer: during a print the macro runs
    // from its button, through the classifier's questions.
    let can_test = app.command_blocker().is_none() && !app.printer_busy() && !lines.is_empty();
    let modal = Modal::new(Id::new("sprint-controls-macro-editor"))
        .frame(frame)
        .backdrop_color(backdrop)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;
            let title = if editor.index.is_some() {
                "Edit macro"
            } else {
                "New macro"
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(icons::MACRO).color(p.accent).size(18.0));
                ui.label(
                    RichText::new(title)
                        .font(FontId::new(16.0, theme::font(ui, TextRole::Title).family))
                        .color(p.text),
                );
            });
            ui.add_space(2.0);
            field_label(ui, &p, "Name");
            let name = ui.add(
                TextEdit::singleline(&mut editor.draft.name)
                    .id(Id::new("sprint-controls-macro-name"))
                    .hint_text("e.g. Preheat PETG")
                    .desired_width(f32::INFINITY)
                    .margin(vec2(8.0, 5.0)),
            );
            if editor.focus_name {
                name.request_focus();
                editor.focus_name = false;
            }
            field_label(ui, &p, "G-code");
            // A long macro scrolls inside the field: the dialog itself
            // must stay on screen with its buttons.
            ScrollArea::vertical()
                .id_salt("sprint-controls-macro-gcode-scroll")
                .max_height(code_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        TextEdit::multiline(&mut editor.draft.gcode)
                            .id(Id::new("sprint-controls-macro-gcode"))
                            .code_editor()
                            .hint_text("G28\nG1 Z10 F600")
                            .desired_rows(8)
                            .desired_width(f32::INFINITY)
                            .lock_focus(true),
                    );
                });
            let summary = match lines.len() {
                0 => "No commands yet.".to_owned(),
                1 => "1 command.".to_owned(),
                n => format!("{n} commands."),
            };
            caption(
                ui,
                &format!(
                    "{summary} Comments after ; are ignored; {{x}} {{y}} {{z}} {{hotend}} {{bed}} \
                     {{fan}} {{park_x}} {{heaters_off}} … are filled in when it runs; {{0}}, \
                     {{1=200}} and {{args}} are arguments it asks for."
                ),
            );
            if params.any() {
                let fields = params.fields();
                caption(
                    ui,
                    &format!(
                        "Asks for {fields} argument{} when it runs.",
                        if fields == 1 { "" } else { "s" }
                    ),
                );
            }
            if !dangerous.is_empty() {
                super::notice(
                    ui,
                    icons::WARNING,
                    &format!("Sprint asks before sending it. {}", dangerous.join(" ")),
                    theme::Tone::Warning,
                );
            }
            field_label(ui, &p, "Color");
            color_row(ui, &p, editor);
            ui.add(Toggle::new(&mut editor.draft.confirm).label("Ask before running"));
            if editor.draft.confirm && stops_at_once(&editor.draft.gcode) {
                caption(
                    ui,
                    "Its emergency stop (M112) goes out at once, without this question, and \
                     nothing else is sent.",
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let test = ui.add_enabled(
                    can_test,
                    ActionButton::new("Test send")
                        .icon(icons::SEND)
                        .tooltip("Send the G-code now without saving"),
                );
                let test = match app.command_blocker() {
                    Some(reason) => test.on_disabled_hover_text(reason),
                    None if app.printer_busy() => {
                        test.on_disabled_hover_text("Not while a print runs")
                    }
                    None => test.on_disabled_hover_text("Type some G-code first"),
                };
                if test.clicked() {
                    outcome = EditorOutcome::TestSend;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let save = ui.add_enabled(
                        can_save,
                        ActionButton::new("Save").primary().min_width(84.0),
                    );
                    let save = save.on_disabled_hover_text(if name_ok {
                        "Add at least one G-code command"
                    } else {
                        "Give the macro a name"
                    });
                    if save.clicked() {
                        outcome = EditorOutcome::Save;
                    }
                    if ui
                        .add(ActionButton::new("Cancel").min_width(84.0))
                        .clicked()
                    {
                        outcome = EditorOutcome::Cancel;
                    }
                });
            });
        });
    // Escape and Cancel discard the edits; a click outside the dialog only
    // closes it while nothing was changed (a stray click must not throw
    // away a macro being written).
    if outcome == EditorOutcome::Open
        && modal.should_close()
        && !(modal.backdrop_response.clicked() && editor.is_modified())
    {
        outcome = EditorOutcome::Cancel;
    }
    // Ctrl+Enter saves from anywhere in the dialog.
    if outcome == EditorOutcome::Open
        && can_save
        && modal.is_top_modal
        && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Enter))
    {
        outcome = EditorOutcome::Save;
    }
    outcome
}

fn field_label(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).color(p.text_dim).size(12.5));
}

/// "None", the swatches and a custom color picker.
fn color_row(ui: &mut Ui, p: &Palette, editor: &mut Editor) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let none = swatch(ui, p, None, editor.draft.color.is_none(), "No color");
        if none.clicked() {
            editor.draft.color = None;
        }
        for rgb in SWATCHES {
            let r = swatch(ui, p, Some(rgb), editor.draft.color == Some(rgb), "Color");
            if r.clicked() {
                editor.draft.color = Some(rgb);
            }
        }
        custom_swatch(ui, p, editor);
    });
}

/// The custom color choice: a hue wheel (holding the custom color once it
/// is picked) that opens egui's color picker.
fn custom_swatch(ui: &mut Ui, p: &Palette, editor: &mut Editor) {
    let selected = editor.draft.color.is_some_and(|c| !SWATCHES.contains(&c));
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(WidgetType::RadioButton, true, selected, "Custom color")
    });
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        let segments = 24;
        let mut wheel = Mesh::default();
        wheel.colored_vertex(c, if p.dark { p.surface } else { Color32::WHITE });
        for i in 0..=segments {
            let turn = i as f32 / segments as f32;
            let angle = turn * std::f32::consts::TAU;
            wheel.colored_vertex(
                c + 9.0 * vec2(angle.cos(), angle.sin()),
                Hsva::new(turn, 0.7, 1.0, 1.0).into(),
            );
        }
        for i in 0..segments {
            wheel.add_triangle(0, i + 1, i + 2);
        }
        let painter = ui.painter();
        painter.add(wheel);
        if selected {
            let [r, g, b] = editor.custom;
            painter.circle(
                c,
                5.0,
                Color32::from_rgb(r, g, b),
                Stroke::new(1.5, p.surface),
            );
        }
        if selected {
            painter.circle_stroke(c, 11.5, Stroke::new(2.0, p.accent));
        } else if response.hovered() {
            painter.circle_stroke(c, 11.5, Stroke::new(1.0, p.border_strong));
        }
    }
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Custom color");
    if response.clicked() {
        editor.draft.color = Some(editor.custom);
    }
    Popup::menu(&response)
        .id(Id::new("sprint-controls-macro-custom-color"))
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let [r, g, b] = editor.custom;
            let mut color = Color32::from_rgb(r, g, b);
            if color_picker::color_picker_color32(ui, &mut color, Alpha::Opaque) {
                editor.custom = [color.r(), color.g(), color.b()];
                editor.draft.color = Some(editor.custom);
            }
        });
}

/// A round color choice; `None` is the "no color" swatch.
fn swatch(ui: &mut Ui, p: &Palette, rgb: Option<[u8; 3]>, selected: bool, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let label = match rgb {
        Some([r, g, b]) => format!("Color #{r:02X}{g:02X}{b:02X}"),
        None => "No color".to_owned(),
    };
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, true, selected, &label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let c = rect.center();
        match rgb {
            Some([r, g, b]) => {
                painter.circle_filled(c, 9.0, Color32::from_rgb(r, g, b));
            }
            None => {
                painter.circle(c, 9.0, p.raised, Stroke::new(1.0, p.border_strong));
                painter.line_segment(
                    [c + vec2(-5.5, 5.5), c + vec2(5.5, -5.5)],
                    Stroke::new(1.5, p.text_faint),
                );
            }
        }
        if selected {
            painter.circle_stroke(c, 11.5, Stroke::new(2.0, p.accent));
        } else if response.hovered() {
            painter.circle_stroke(c, 11.5, Stroke::new(1.0, p.border_strong));
        }
    }
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macro_text_becomes_clean_lines() {
        assert_eq!(
            macro_lines("G28 ; home\n\n  g1 z10 f600\r\n; just a comment\nM117 Hi there"),
            ["G28", "G1 Z10 F600", "M117 Hi there"]
        );
        assert!(macro_lines("").is_empty());
        assert!(macro_lines("   \n;\n(comment)\n").is_empty());
        // Placeholders survive for the host to fill in.
        assert_eq!(macro_lines("G1 Z{z}"), ["G1 Z{z}"]);
    }

    #[test]
    fn the_editor_names_what_asks_on_an_idle_printer() {
        let reasons = idle_reasons("M502\nM500\nm502 ; again");
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(reasons[0].starts_with("M502"), "{reasons:?}");
        assert!(idle_reasons("G28\nM503\n{heaters_off}").is_empty());
        // An emergency stop never asks.
        assert!(idle_reasons("M112").is_empty());
        assert!(stops_at_once("G28\nm112 ; stop"));
        assert!(!stops_at_once("M108\nM410\n; M112"));
    }

    #[test]
    fn previews_render_the_placeholders() {
        let state = PrinterState::default();
        let profile = PrinterProfile::default();
        assert_eq!(
            rendered("{heaters_off}\nM107 ; fan", &state, &profile),
            "M104 S0\nM140 S0\nM107"
        );
    }

    #[test]
    fn defaults_follow_the_profile() {
        let mut profile = PrinterProfile::default();
        let defaults = default_macros(&profile);
        let names: Vec<_> = defaults.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Preheat PLA",
                "Level corners",
                "Filament change",
                "Report settings"
            ]
        );
        assert_eq!(macro_lines(&defaults[0].gcode), ["M140 S60", "M104 S205"]);
        let level = macro_lines(&defaults[1].gcode);
        assert_eq!(level[0], "G28");
        assert_eq!(level.iter().filter(|l| l.starts_with("M0")).count(), 4);
        assert_eq!(level.iter().filter(|l| *l == "G1 Z0.2 F600").count(), 4);
        // Corners sit inside a 220 mm bed.
        assert!(level.contains(&"G1 X30 Y30 F3000".to_owned()));
        assert!(level.contains(&"G1 X190 Y190 F3000".to_owned()));
        assert!(defaults[1].confirm && defaults[2].confirm);
        assert_eq!(defaults[2].gcode, "M600");
        assert_eq!(defaults[3].gcode, "M503");
        // No heated bed, no PLA preset, centered origin.
        profile.heated_bed = false;
        profile.temp_presets.retain(|p| p.name != "PLA");
        profile.origin = sprint_host::BedOrigin::Center;
        let defaults = default_macros(&profile);
        assert_eq!(defaults[0].name, "Preheat PETG");
        assert_eq!(macro_lines(&defaults[0].gcode), ["M104 S240"]);
        assert!(macro_lines(&defaults[1].gcode).contains(&"G1 X-80 Y-80 F3000".to_owned()));
        // A tiny bed keeps the corners on the bed.
        profile.bed_size = [8.0, 8.0];
        let level = macro_lines(&default_macros(&profile)[1].gcode);
        assert!(level.contains(&"G1 X0 Y0 F3000".to_owned()));
    }

    #[test]
    fn list_operations() {
        let mut list = vec![
            new_macro("A", "G28", None, false),
            new_macro("B", "M84", None, false),
        ];
        assert!(apply_op(&mut list, ListOp::Duplicate(0)));
        assert_eq!(list[1].name, "A (copy)");
        assert!(apply_op(&mut list, ListOp::MoveLater(0)));
        assert_eq!(list[0].name, "A (copy)");
        assert!(apply_op(&mut list, ListOp::MoveEarlier(2)));
        assert_eq!(list[1].name, "B");
        // Out of range or at the edges: nothing happens.
        assert!(!apply_op(&mut list, ListOp::MoveEarlier(0)));
        assert!(!apply_op(&mut list, ListOp::MoveLater(2)));
        assert!(!apply_op(&mut list, ListOp::Duplicate(9)));
        assert!(!apply_op(&mut list, ListOp::MoveLater(99)));
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn previews_are_bounded() {
        let long: String = (0..30).map(|i| format!("G1 X{i}\n")).collect();
        let p = preview(&long);
        assert_eq!(p.lines().count(), PREVIEW_LINES + 1);
        assert!(p.ends_with("… 18 more lines"));
        assert_eq!(preview("G28\n\n  \nM84"), "G28\nM84");
        // A huge line is cut, so a tooltip or confirmation stays small.
        let wide = format!("M117 {}", "é".repeat(10_000));
        let p = preview(&wide);
        assert_eq!(p.chars().count(), PREVIEW_LINE_CHARS);
        assert!(p.starts_with("M117 éé") && p.ends_with('…'));
        let exact = "x".repeat(PREVIEW_LINE_CHARS);
        assert_eq!(preview(&exact), exact);
    }
}

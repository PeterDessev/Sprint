//! Macros: a grid of the user's G-code buttons (from the config), with an
//! editor dialog, confirmation where asked for, a context menu to edit,
//! duplicate, reorder and delete, and a set of useful defaults.

use egui::color_picker::{self, Alpha};
use egui::ecolor::Hsva;
use egui::{
    Align, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Key, Layout, Margin, Mesh, Modal,
    Popup, PopupCloseBehavior, Response, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui,
    WidgetInfo, WidgetType, text::LayoutJob, vec2,
};
use sprint_host::{Macro, PrinterProfile};

use super::{caption, section};
use crate::context::AppCtx;
use crate::icons;
use crate::theme::{self, Palette, TextRole, mix, readable_text_on, with_alpha};
use crate::widgets::{ActionButton, ConfirmDialog, DialogResult, IconButton, Toggle};

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

/// The commands a macro sends, as the printer will see them (comments and
/// blank lines dropped; `{placeholders}` are filled in when it runs).
pub(super) fn macro_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(sprint_host::sprint_gcode::clean_line)
        .map(|line| line.into_owned())
        .collect()
}

/// Commands in `text` the G-code reference marks as dangerous (M502,
/// M112, ...), each listed once, in order.
pub(super) fn dangerous_commands(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for line in macro_lines(text) {
        if let Some(doc) = sprint_host::sprint_proto::gcode_doc(&line)
            && doc.dangerous
            && !found.contains(&doc.code)
        {
            found.push(doc.code);
        }
    }
    found
}

/// A short preview of a macro's G-code (first lines, "…" when cut).
fn preview(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let mut out = lines
        .iter()
        .take(PREVIEW_LINES)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > PREVIEW_LINES {
        out.push_str(&format!("\n… {} more lines", lines.len() - PREVIEW_LINES));
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
            draft,
            custom,
            focus_name: true,
        }
    }
}

/// UI state of the macros section.
#[derive(Debug, Default)]
pub(super) struct Macros {
    editor: Option<Editor>,
    /// A macro waiting for "Run?" confirmation.
    pending_run: Option<Macro>,
    /// A macro waiting for "Delete?" confirmation.
    pending_delete: Option<usize>,
}

impl Macros {
    /// Draw the section.
    pub(super) fn show(&mut self, ui: &mut Ui, ctx: &mut AppCtx<'_>) {
        let connected = ctx.connected();
        let job_active = ctx.job_active();
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
                for (row, chunk) in macros.chunks(columns).enumerate() {
                    ui.horizontal(|ui| {
                        for (col, m) in chunk.iter().enumerate() {
                            let index = row * columns + col;
                            let r = macro_tile(ui, m, tile_w, connected, index);
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
            && let Some(m) = ctx.config.macros.get(index)
        {
            if m.confirm || job_active {
                self.pending_run = Some(m.clone());
            } else {
                ctx.printer.run_macro(m);
            }
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
    /// editor). Called once per frame after the sidebar.
    pub(super) fn dialogs(&mut self, ui: &Ui, ctx: &mut AppCtx<'_>) {
        let egui_ctx = ui.ctx().clone();
        if let Some(m) = &self.pending_run {
            let title = format!("Run “{}”?", m.name);
            let message = if ctx.job_active() {
                "A job is running: these commands are sent between the print's lines."
            } else {
                "This macro asks for confirmation before it runs."
            };
            let details = preview(&m.gcode);
            let danger = ctx.job_active() || !dangerous_commands(&m.gcode).is_empty();
            let answer = ConfirmDialog::new("sprint-controls-macro-run", &title)
                .message(message)
                .details(&details)
                .confirm_label("Run")
                .icon(icons::MACRO)
                .danger(danger)
                .show(&egui_ctx);
            match answer {
                Some(DialogResult::Confirmed) => {
                    if ctx.connected() {
                        ctx.printer.run_macro(m);
                    }
                    self.pending_run = None;
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
        if let Some(editor) = &mut self.editor {
            match editor_dialog(&egui_ctx, editor, ctx) {
                EditorOutcome::Open => {}
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
    }
}

/// One macro button: its color (or a neutral fill), name truncated to the
/// width, "…" when it asks before running.
fn macro_tile(ui: &mut Ui, m: &Macro, width: f32, runnable: bool, index: usize) -> Response {
    let p = theme::ui_palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(width, TILE_HEIGHT), Sense::click());
    let id = Id::new(("sprint-controls-macro", index));
    let label = if m.confirm {
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
    let gcode = preview(&m.gcode);
    let name = label;
    response
        .on_hover_cursor(if runnable {
            CursorIcon::PointingHand
        } else {
            CursorIcon::Default
        })
        .on_hover_ui(|ui| {
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
        })
}

/// What the editor dialog wants this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorOutcome {
    Open,
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
    let width = (ctx.content_rect().width() - 48.0).clamp(280.0, 480.0);
    let lines = macro_lines(&editor.draft.gcode);
    let dangerous = dangerous_commands(&editor.draft.gcode);
    let name_ok = !editor.draft.name.trim().is_empty();
    let can_save = name_ok && !lines.is_empty();
    let can_test = app.connected() && !app.job_active() && !lines.is_empty();
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
            ui.add(
                TextEdit::multiline(&mut editor.draft.gcode)
                    .id(Id::new("sprint-controls-macro-gcode"))
                    .code_editor()
                    .hint_text("G28\nG1 Z10 F600")
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .lock_focus(true),
            );
            let summary = match lines.len() {
                0 => "No commands yet.".to_owned(),
                1 => "1 command.".to_owned(),
                n => format!("{n} commands."),
            };
            caption(
                ui,
                &format!(
                    "{summary} Comments after ; are ignored; {{x}} {{y}} {{z}} {{hotend}} {{bed}} \
                     {{fan}} {{park_x}} … are filled in when it runs."
                ),
            );
            if !dangerous.is_empty() {
                super::notice(
                    ui,
                    icons::WARNING,
                    &format!(
                        "Contains {}: consider asking before it runs.",
                        dangerous.join(", ")
                    ),
                    theme::Tone::Warning,
                );
            }
            field_label(ui, &p, "Color");
            color_row(ui, &p, editor);
            ui.add(Toggle::new(&mut editor.draft.confirm).label("Ask before running"));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let test = ui.add_enabled(
                    can_test,
                    ActionButton::new("Test send")
                        .icon(icons::SEND)
                        .tooltip("Send the G-code now without saving"),
                );
                let test = if app.job_active() {
                    test.on_disabled_hover_text("Not while a job runs")
                } else if !app.connected() {
                    test.on_disabled_hover_text("Connect to a printer first")
                } else {
                    test.on_disabled_hover_text("Type some G-code first")
                };
                if test.clicked() {
                    app.printer.run_macro(&editor.draft);
                    let shown = if name_ok {
                        editor.draft.name.trim().to_owned()
                    } else {
                        "macro".to_owned()
                    };
                    app.toasts.info(format!("Sent {shown}"));
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
    if outcome == EditorOutcome::Open && modal.should_close() {
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
    fn dangerous_commands_are_found_once() {
        assert_eq!(dangerous_commands("M502\nM500\nm502"), ["M502"]);
        assert!(dangerous_commands("G28\nM503").is_empty());
        assert_eq!(dangerous_commands("M112"), ["M112"]);
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
    }
}

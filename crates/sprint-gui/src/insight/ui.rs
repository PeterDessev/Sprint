//! Drawing helpers shared by the insight tabs (Printer, Tuning,
//! Calibration, Bed mesh, SD card). The only part of [`crate::insight`]
//! that touches egui; built on the design system, which it extends with
//! layout pieces the tabs need: a page header, a masonry card grid, metric
//! tiles and inline notices.

use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, Response, Sense, Stroke, Ui,
    UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::theme::{self, GAP_LG, TextRole, Tone, with_alpha};
use crate::widgets::ActionButton;

/// Font for an icon glyph (the Phosphor family when Sprint's fonts are
/// installed, so it centres well; never panics).
pub(crate) fn icon_font(ui: &Ui, size: f32) -> FontId {
    if crate::fonts::family_is_active(ui.ctx(), &crate::fonts::icon_family()) {
        FontId::new(size, crate::fonts::icon_family())
    } else {
        FontId::proportional(size)
    }
}

/// Room the title of a [`page_header`] keeps next to its controls (points,
/// after the icon tile); with less, the controls go on a row below.
const HEADER_TITLE_ROOM: f32 = 150.0;

/// The header of a tab: an icon tile, a title, an optional dim subtitle
/// and controls on the right. When the controls leave the title less than
/// [`HEADER_TITLE_ROOM`], they move to a row of their own under it (wrapping
/// if need be) instead of being drawn over the title.
pub(crate) fn page_header(
    ui: &mut Ui,
    icon: &str,
    title: &str,
    subtitle: Option<&str>,
    right: impl FnOnce(&mut Ui),
) {
    let p = theme::ui_palette(ui);
    let height = 40.0;
    let width = ui.available_width();
    // The controls' width from the last frame decides the layout.
    let memory = ui.id().with(("insight-page-header", title));
    let last_controls: f32 = ui.data(|d| d.get_temp(memory)).unwrap_or(0.0);
    let fits =
        |controls: f32| controls <= 0.0 || controls + 12.0 + 48.0 + HEADER_TITLE_ROOM <= width;
    let stacked = !fits(last_controls);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, title));
    let controls = if stacked {
        ui.add_space(6.0);
        let mut row = ui.new_child(
            UiBuilder::new()
                // One button high: the controls centre on that row (and
                // wrap onto more rows below it).
                .max_rect(Rect::from_min_size(ui.cursor().min, vec2(width, 30.0)))
                .layout(Layout::left_to_right(Align::Center).with_main_wrap(true)),
        );
        row.spacing_mut().item_spacing = vec2(6.0, 6.0);
        right(&mut row);
        let used = row.min_rect();
        ui.advance_cursor_after_rect(used);
        used
    } else {
        let mut right_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        right_ui.spacing_mut().item_spacing.x = 6.0;
        right(&mut right_ui);
        right_ui.min_rect()
    };
    let controls_width = controls.width().max(0.0);
    ui.data_mut(|d| d.insert_temp(memory, controls_width));
    if fits(controls_width) == stacked {
        // The layout changes next frame: do not show this one.
        ui.ctx().request_discard("page header layout");
    }
    let text_end = if stacked || controls_width <= 0.0 {
        rect.max.x
    } else {
        controls.min.x - 12.0
    };
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let tile = Rect::from_center_size(pos2(rect.min.x + 18.0, rect.center().y), Vec2::splat(36.0));
    painter.rect_filled(tile, CornerRadius::same(theme::RADIUS_LG), p.accent_soft);
    painter.text(
        tile.center(),
        Align2::CENTER_CENTER,
        icon,
        icon_font(ui, 20.0),
        p.accent,
    );
    let x = tile.max.x + 12.0;
    let title_font = theme::builtin_font(ui, egui::TextStyle::Heading);
    let max = (text_end - x).max(0.0);
    let title_galley = truncated(ui, title, title_font, p.text, max);
    let sub_galley =
        subtitle.map(|s| truncated(ui, s, FontId::proportional(12.5), p.text_dim, max));
    let total = title_galley.size().y + sub_galley.as_ref().map_or(0.0, |g| g.size().y + 1.0);
    let mut y = rect.center().y - total / 2.0;
    painter.galley(pos2(x, y), title_galley.clone(), p.text);
    y += title_galley.size().y + 1.0;
    if let Some(g) = sub_galley {
        painter.galley(pos2(x, y), g, p.text_dim);
    }
}

/// The width a [`crate::widgets::SegmentedControl`] with these labels
/// takes: every segment is as wide as the widest label plus its padding
/// (13 pt text, 24 points of padding, at least 36 points, 3 points of
/// inset on each side), so a narrow tab can pick shorter labels or another
/// control before it overflows.
pub(crate) fn segmented_width(ui: &Ui, labels: &[&str]) -> f32 {
    let widest = labels
        .iter()
        .map(|label| {
            ui.painter()
                .layout_no_wrap(
                    (*label).to_owned(),
                    FontId::proportional(13.0),
                    Color32::PLACEHOLDER,
                )
                .size()
                .x
                + 24.0
        })
        .fold(36.0, f32::max);
    widest * labels.len().max(1) as f32 + 6.0
}

/// `text` on one line, cut with "…" to `max_width`.
pub(crate) fn truncated(
    ui: &Ui,
    text: &str,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_width.max(0.0));
    ui.painter().layout_job(job)
}

/// Number of masonry columns for `width`: as many as fit `min_col` wide,
/// between 1 and `max_cols`.
pub(crate) fn column_count(width: f32, min_col: f32, max_cols: usize) -> usize {
    if !(width.is_finite() && min_col.is_finite() && min_col > 0.0) {
        return 1;
    }
    (((width + GAP_LG) / (min_col + GAP_LG)).floor() as usize).clamp(1, max_cols.max(1))
}

/// Lay out `count` cards in columns (1 to `max_cols`, each at least
/// `min_col` wide), each card going into the currently shortest column.
/// `draw(ui, index)` draws card `index` into a `Ui` of the column's width;
/// its widget ids do not depend on the column it lands in, and content
/// wider than the column is clipped instead of pushing the next cards out.
pub(crate) fn masonry(
    ui: &mut Ui,
    min_col: f32,
    max_cols: usize,
    count: usize,
    mut draw: impl FnMut(&mut Ui, usize),
) {
    let gap = GAP_LG;
    let width = ui.available_width();
    let cols = column_count(width, min_col, max_cols);
    let col_w = ((width - gap * (cols - 1) as f32) / cols as f32).max(1.0);
    let top_left = ui.cursor().min;
    let base = ui.id().with("insight-masonry");
    let mut heights = vec![0.0f32; cols];
    for index in 0..count {
        let c = heights
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(i, _)| i);
        let x = top_left.x + c as f32 * (col_w + gap);
        let y = top_left.y + heights[c];
        // Like `Ui::columns`: down to the parent's bottom (inside a scroll
        // area that is the viewport; the content may grow past it).
        let bottom = ui.max_rect().bottom().max(y + 1.0);
        let rect = Rect::from_min_max(pos2(x, y), pos2(x + col_w, bottom));
        let mut card = ui.new_child(
            UiBuilder::new()
                .id(base.with(("card", index)))
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        card.set_width(col_w);
        let clip = ui.clip_rect().intersect(Rect::from_x_y_ranges(
            (x - 2.0)..=(x + col_w + 2.0),
            ui.clip_rect().y_range(),
        ));
        card.set_clip_rect(clip);
        draw(&mut card, index);
        let used = card.min_rect().height();
        if used > 0.0 {
            heights[c] += used + gap;
        }
    }
    let height = (heights.iter().copied().fold(0.0, f32::max) - gap).max(0.0);
    ui.advance_cursor_after_rect(Rect::from_min_size(top_left, vec2(width, height)));
}

/// A small labelled value: a dim caption above a value.
pub(crate) fn metric(ui: &mut Ui, label: &str, value: &str, color: Option<Color32>) -> Response {
    let p = theme::ui_palette(ui);
    let caption = theme::font(ui, TextRole::Caption);
    let value_font = FontId::new(15.0, theme::font(ui, TextRole::Title).family);
    let width = ui.available_width().max(40.0);
    let label_g = truncated(ui, label, caption, p.text_dim, width);
    let value_g = truncated(ui, value, value_font, color.unwrap_or(p.text), width);
    let size = vec2(
        label_g.size().x.max(value_g.size().x),
        label_g.size().y + 2.0 + value_g.size().y,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response
        .widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, format!("{label}: {value}")));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        painter.galley(rect.min, label_g.clone(), p.text_dim);
        painter.galley(
            pos2(rect.min.x, rect.min.y + label_g.size().y + 2.0),
            value_g,
            p.text,
        );
    }
    response
}

/// A grid of [`metric`]s, `columns` per row, equally wide.
pub(crate) fn metric_grid(ui: &mut Ui, columns: usize, items: &[(&str, String, Option<Color32>)]) {
    let columns = columns.max(1);
    let gap = 12.0;
    let width = ui.available_width();
    let cell = ((width - gap * (columns - 1) as f32) / columns as f32).max(40.0);
    for row in items.chunks(columns) {
        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (label, value, color) in row {
                ui.allocate_ui_with_layout(vec2(cell, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(cell);
                    metric(ui, label, value, *color);
                });
            }
        });
        ui.add_space(4.0);
    }
}

/// An inline notice: a tinted box with an icon, a bold title, an optional
/// body and an optional action button. Returns whether the action was
/// clicked.
pub(crate) fn notice(
    ui: &mut Ui,
    tone: Tone,
    icon: &str,
    title: &str,
    body: Option<&str>,
    action: Option<&str>,
) -> bool {
    let p = theme::ui_palette(ui);
    let color = p.tone(tone);
    let mut clicked = false;
    let frame = egui::Frame::new()
        .fill(p.tone_soft(tone))
        .stroke(Stroke::new(1.0, with_alpha(color, 0.35)))
        .corner_radius(CornerRadius::same(theme::RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 8));
    frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(
                egui::RichText::new(icon)
                    .font(icon_font(ui, 16.0))
                    .color(color),
            );
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.label(
                    egui::RichText::new(title)
                        .font(FontId::new(13.0, theme::font(ui, TextRole::Title).family))
                        .color(p.text),
                );
                if let Some(body) = body {
                    ui.add(
                        egui::Label::new(egui::RichText::new(body).size(12.5).color(p.text_dim))
                            .wrap(),
                    );
                }
                if let Some(action) = action {
                    ui.add_space(4.0);
                    clicked = ui.add(ActionButton::new(action).small()).clicked();
                }
            });
        });
    });
    clicked
}

/// A thin horizontal bar showing `fraction` (0..=1) in `color` on a track.
pub(crate) fn bar(ui: &mut Ui, fraction: f32, color: Color32, width: f32, height: f32) -> Response {
    let p = theme::ui_palette(ui);
    let (rect, response) =
        ui.allocate_exact_size(vec2(width.max(4.0), height.max(2.0)), Sense::hover());
    if ui.is_rect_visible(rect) {
        let radius = CornerRadius::same(255);
        ui.painter().rect_filled(rect, radius, p.raised);
        let f = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if f > 0.0 {
            let fill = Rect::from_min_size(rect.min, vec2(rect.width() * f, rect.height()));
            ui.painter().rect_filled(fill, radius, color);
        }
    }
    response
}

/// A row label styled like [`crate::widgets::NumericField`]'s, `width`
/// wide, so other controls line up with a column of numeric fields.
pub(crate) fn field_label(ui: &mut Ui, text: &str, width: f32) -> Response {
    let p = theme::ui_palette(ui);
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), FontId::proportional(13.0), p.text_dim);
    let width = width.max(12.0 + galley.size().x);
    let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        ui.painter().galley(
            pos2(rect.min.x + 12.0, rect.center().y - galley.size().y / 2.0),
            galley,
            p.text_dim,
        );
    }
    response
}

/// The frame of the tabs' own modal dialogs, matching
/// [`crate::widgets::ConfirmDialog`].
pub(crate) fn modal_frame(ctx: &egui::Context) -> egui::Frame {
    let p = theme::palette(ctx);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(
            1.0,
            if p.dark { p.border_strong } else { p.border },
        ))
        .corner_radius(CornerRadius::same(theme::RADIUS_XL))
        .inner_margin(egui::Margin::same(20))
        .shadow(ctx.global_style().visuals.window_shadow)
}

/// A dialog title: an icon in a tinted circle and the title text.
pub(crate) fn dialog_title(ui: &mut Ui, icon: &str, title: &str, tone: Color32) {
    let p = theme::ui_palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let (rect, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
        ui.painter().circle_filled(
            rect.center(),
            18.0,
            with_alpha(tone, if p.dark { 0.18 } else { 0.12 }),
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            icon,
            icon_font(ui, 19.0),
            tone,
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(title)
                    .font(FontId::new(16.0, theme::font(ui, TextRole::Title).family))
                    .color(p.text),
            )
            .wrap(),
        );
    });
}

/// A dim caption line.
pub(crate) fn caption(ui: &mut Ui, text: &str) -> Response {
    let p = theme::ui_palette(ui);
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::font(ui, TextRole::Caption))
                .color(p.text_dim),
        )
        .wrap(),
    )
}

/// Body text in the dim color, wrapped.
pub(crate) fn dim(ui: &mut Ui, text: &str) -> Response {
    let p = theme::ui_palette(ui);
    ui.add(egui::Label::new(egui::RichText::new(text).size(13.0).color(p.text_dim)).wrap())
}

/// A colored dot followed by a label, for legends and heater names.
pub(crate) fn dot_label(ui: &mut Ui, color: Color32, text: &str, strong: bool) -> Response {
    let p = theme::ui_palette(ui);
    let font = if strong {
        FontId::new(13.5, theme::font(ui, TextRole::Title).family)
    } else {
        FontId::proportional(13.0)
    };
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, p.text);
    let size = vec2(14.0 + galley.size().x, galley.size().y.max(16.0));
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, text));
    if ui.is_rect_visible(rect) {
        ui.painter()
            .circle_filled(pos2(rect.min.x + 4.0, rect.center().y), 4.0, color);
        ui.painter().galley(
            pos2(rect.min.x + 14.0, rect.center().y - galley.size().y / 2.0),
            galley,
            p.text,
        );
    }
    response
}

/// Where a request to calibrate a heater's PID waits for the Calibration
/// tab (panels cannot add app actions; this hand-over goes through egui's
/// memory next to the `ShowTab` action).
fn pid_request_id() -> egui::Id {
    egui::Id::new("insight-pid-calibration-request")
}

/// Ask the Calibration tab to open its PID wizard for `heater` the next
/// time it is shown.
pub(crate) fn request_pid_calibration(ctx: &egui::Context, heater: sprint_host::Heater) {
    ctx.data_mut(|d| d.insert_temp(pid_request_id(), heater));
}

/// The pending PID calibration request, if any (taken: asked once).
pub(crate) fn take_pid_calibration(ctx: &egui::Context) -> Option<sprint_host::Heater> {
    ctx.data_mut(|d| {
        let heater = d.get_temp::<sprint_host::Heater>(pid_request_id());
        d.remove::<sprint_host::Heater>(pid_request_id());
        heater
    })
}

/// Why the EEPROM buttons are disabled on firmware without one.
pub(crate) const NO_EEPROM: &str = concat!(
    "The firmware reports no EEPROM (Cap:EEPROM:0): settings cannot be saved and ",
    "last until the printer restarts.",
);

/// The toasts of an EEPROM save ([`Printer::save_settings`]): `done` once
/// the printer has saved, a note while it still finishes earlier commands
/// (a heat wait, a long move), an error when it will not save.
///
/// [`Printer::save_settings`]: sprint_host::Printer::save_settings
pub(crate) fn eeprom_followup(done: &str) -> crate::widgets::Followup {
    crate::widgets::Followup::new(
        crate::widgets::Notice::new(done),
        "Saving to EEPROM when the printer is ready",
        "Not saved to EEPROM",
    )
}

/// The toasts of a changed setting ([`Printer::apply_setting`]): `done`
/// once the printer took it, a note while it still finishes earlier
/// commands, an error titled `failed` when it rejects it or will not
/// take it.
///
/// [`Printer::apply_setting`]: sprint_host::Printer::apply_setting
pub(crate) fn applied_followup(
    done: crate::widgets::Notice,
    failed: &str,
) -> crate::widgets::Followup {
    crate::widgets::Followup::new(done, "Applying it when the printer is ready", failed)
}

/// Whether the printer is a delta: its firmware says so (Marlin's M115
/// `KINEMATICS:DELTA`), it reports a delta's geometry (`M665` with its rod
/// length and radius), or its profile is a delta's.
pub(crate) fn is_delta(
    state: &sprint_host::PrinterState,
    profile: &sprint_host::PrinterProfile,
) -> bool {
    let firmware = state.firmware.as_ref().is_some_and(firmware_says_delta);
    let geometry =
        state.settings.value("M665", 'L').is_some() && state.settings.value("M665", 'R').is_some();
    firmware || geometry || profile.is_delta()
}

/// The firmware names delta kinematics (Marlin's M115 `KINEMATICS:DELTA`).
pub(crate) fn firmware_says_delta(info: &sprint_host::sprint_proto::FirmwareInfo) -> bool {
    info.get("KINEMATICS")
        .is_some_and(|k| k.trim().eq_ignore_ascii_case("DELTA"))
}

/// The firmware said it has no EEPROM (`Cap:EEPROM:0`); M500 and M501 do
/// nothing there. Firmware that does not report it counts as having one.
pub(crate) fn lacks_eeprom(state: &sprint_host::PrinterState) -> bool {
    lacks_capability(state, "EEPROM")
}

/// The firmware reported capability `name` as missing (`Cap:<name>:0`).
/// Capabilities it does not report count as present.
pub(crate) fn lacks_capability(state: &sprint_host::PrinterState, name: &str) -> bool {
    state
        .capabilities
        .iter()
        .any(|(cap, on)| cap.eq_ignore_ascii_case(name) && !on)
}

/// Tells connections apart, for work done once per connection (loading
/// the settings, the mesh, the file list). A tab only runs while it is
/// shown, so it may never see the printer disconnected between two
/// connections. The connection clock (`connected_secs`, wall-clock seconds
/// since the link opened) gives a new one away: it restarted, or it moved
/// on less than the time that passed since the tab last looked.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConnectionWatch {
    /// `connected_secs` and the wall clock when last seen connected.
    last: Option<(f64, std::time::Instant)>,
}

/// How far (seconds) the connection clock may lag behind the wall clock
/// before the connection counts as a new one (the host publishes it every
/// 20 ms; a reconnect takes longer than this).
const CLOCK_SLACK: f64 = 1.0;

impl ConnectionWatch {
    /// Whether the printer is connected and this is not the connection
    /// seen on the previous call (the first one seen, or a new one).
    pub(crate) fn is_new(&mut self, state: &sprint_host::PrinterState) -> bool {
        self.is_new_at(state, std::time::Instant::now())
    }

    fn is_new_at(&mut self, state: &sprint_host::PrinterState, now: std::time::Instant) -> bool {
        if !state.is_connected() {
            self.last = None;
            return false;
        }
        let secs = state.link.connected_secs;
        let secs = if secs.is_finite() { secs } else { 0.0 };
        let new = self.last.is_none_or(|(last, seen)| {
            let passed = now.saturating_duration_since(seen).as_secs_f64();
            secs + 0.5 < last || secs - last < passed - CLOCK_SLACK
        });
        self.last = Some((secs, now));
        new
    }
}

/// The result of a [`FileTask`].
#[derive(Debug)]
pub(crate) enum FileOutcome {
    /// The file was written.
    Saved(std::path::PathBuf),
    /// The file was read (lossy UTF-8).
    Loaded(std::path::PathBuf, String),
    /// The user closed the dialog.
    Cancelled,
    /// Writing or reading failed.
    Failed(String),
}

/// A native file dialog (and the file I/O after it) running on its own
/// thread, so the window keeps painting while it is open.
pub(crate) struct FileTask {
    rx: std::sync::mpsc::Receiver<FileOutcome>,
}

/// Largest text file read by [`FileTask::open_text`] (bytes).
pub(crate) const MAX_TEXT_FILE: u64 = 4 * 1024 * 1024;

impl FileTask {
    fn spawn(ctx: &egui::Context, job: impl FnOnce() -> FileOutcome + Send + 'static) -> FileTask {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("sprint-file-dialog".into())
            .spawn(move || {
                let outcome = job();
                let _ = tx.send(outcome);
                ctx.request_repaint();
            });
        if let Err(err) = spawned {
            // Without a thread there is no dialog; report it right away.
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = tx.send(FileOutcome::Failed(format!(
                "cannot open a file dialog: {err}"
            )));
            return FileTask { rx };
        }
        FileTask { rx }
    }

    /// Ask where to save `contents` and write it there.
    pub(crate) fn save_text(
        ctx: &egui::Context,
        title: &str,
        file_name: &str,
        filter: (&str, &[&str]),
        contents: String,
    ) -> FileTask {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title(title)
            .set_file_name(file_name)
            .add_filter(filter.0, filter.1);
        FileTask::spawn(ctx, move || match block_on(dialog.save_file()) {
            None => FileOutcome::Cancelled,
            Some(handle) => {
                let path = handle.path().to_path_buf();
                match crate::fsutil::write_atomic(&path, contents) {
                    Ok(()) => FileOutcome::Saved(path),
                    Err(err) => FileOutcome::Failed(format!("{}: {err}", path.display())),
                }
            }
        })
    }

    /// Ask for a text file and read it (at most [`MAX_TEXT_FILE`] bytes).
    pub(crate) fn open_text(ctx: &egui::Context, title: &str, filter: (&str, &[&str])) -> FileTask {
        let dialog = rfd::AsyncFileDialog::new()
            .set_title(title)
            .add_filter(filter.0, filter.1)
            .add_filter("All files", &["*"]);
        FileTask::spawn(ctx, move || match block_on(dialog.pick_file()) {
            None => FileOutcome::Cancelled,
            Some(handle) => {
                let path = handle.path().to_path_buf();
                read_text(&path)
            }
        })
    }

    /// The outcome, once the dialog closed.
    pub(crate) fn poll(&self) -> Option<FileOutcome> {
        match self.rx.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(FileOutcome::Failed(
                "the file dialog closed unexpectedly".into(),
            )),
        }
    }
}

/// Read a text file of at most [`MAX_TEXT_FILE`] bytes (lossy UTF-8).
pub(crate) fn read_text(path: &std::path::Path) -> FileOutcome {
    let too_big = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_TEXT_FILE);
    if too_big {
        return FileOutcome::Failed(format!(
            "{} is larger than {} MB",
            path.display(),
            MAX_TEXT_FILE / (1024 * 1024)
        ));
    }
    match std::fs::read(path) {
        Ok(bytes) => FileOutcome::Loaded(
            path.to_path_buf(),
            String::from_utf8_lossy(&bytes).into_owned(),
        ),
        Err(err) => FileOutcome::Failed(format!("{}: {err}", path.display())),
    }
}

/// Run a future to completion on the current thread (for the dialog
/// thread; rfd's futures are driven by its own backend).
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl std::task::Wake for ThreadWaker {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => return value,
            std::task::Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_delta_is_known_by_its_firmware_geometry_or_profile() {
        use sprint_host::sprint_proto::{SettingCommand, parse_firmware_line};
        // A real delta's M115.
        let delta = parse_firmware_line(
            "FIRMWARE_NAME:Marlin bugfix-2.1.x (Oct 21 2026 00:06:13) \
             SOURCE_CODE_URL:github.com/MarlinFirmware/Marlin PROTOCOL_VERSION:1.0 \
             MACHINE_TYPE:Space Construction Vehicle KINEMATICS:DELTA EXTRUDER_COUNT:1",
        )
        .expect("M115");
        assert!(firmware_says_delta(&delta));
        let cartesian = parse_firmware_line(
            "FIRMWARE_NAME:Marlin 2.1.2.1 PROTOCOL_VERSION:1.0 MACHINE_TYPE:Ender EXTRUDER_COUNT:1",
        )
        .expect("M115");
        assert!(!firmware_says_delta(&cartesian));
        let profile = sprint_host::PrinterProfile::default();
        let mut state = sprint_host::PrinterState::default();
        assert!(!is_delta(&state, &profile));
        state.firmware = Some(delta);
        assert!(is_delta(&state, &profile));
        // Its geometry in the settings.
        let mut state = sprint_host::PrinterState::default();
        state
            .settings
            .insert(SettingCommand::parse("M665 L218 R102 H342.5").expect("M665"));
        assert!(is_delta(&state, &profile));
        // Or its profile.
        let delta_profile = sprint_host::PrinterProfile {
            machine: sprint_host::MachineKind::Delta,
            ..sprint_host::PrinterProfile::default()
        };
        assert!(is_delta(
            &sprint_host::PrinterState::default(),
            &delta_profile
        ));
    }

    #[test]
    fn column_count_fits_width() {
        assert_eq!(column_count(300.0, 340.0, 3), 1);
        assert_eq!(column_count(700.0, 340.0, 3), 2);
        assert_eq!(column_count(1100.0, 340.0, 3), 3);
        assert_eq!(column_count(5000.0, 340.0, 3), 3);
        assert_eq!(column_count(f32::NAN, 340.0, 3), 1);
        assert_eq!(column_count(1000.0, 0.0, 3), 1);
        assert_eq!(column_count(1000.0, 100.0, 0), 1);
    }

    #[test]
    fn connections_are_told_apart() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let at = |secs: f64| t0 + Duration::from_secs_f64(secs);
        let mut watch = ConnectionWatch::default();
        let mut state = sprint_host::PrinterState {
            status: sprint_host::ConnectionStatus::Connected,
            ..Default::default()
        };
        state.link.connected_secs = 3.0;
        assert!(watch.is_new_at(&state, at(0.0)));
        // The same connection: its clock moves with the wall clock.
        state.link.connected_secs = 600.0;
        assert!(!watch.is_new_at(&state, at(597.2)));
        // Reconnected while the tab was hidden: the clock restarted...
        state.link.connected_secs = 12.0;
        assert!(watch.is_new_at(&state, at(700.0)));
        assert!(!watch.is_new_at(&state, at(700.1)));
        // ...or, seen only briefly, it is behind the time that passed.
        state.link.connected_secs = 20.0;
        assert!(watch.is_new_at(&state, at(800.0)));
        // Seen disconnected in between.
        let disconnected = sprint_host::PrinterState::default();
        assert!(!watch.is_new_at(&disconnected, at(801.0)));
        state.link.connected_secs = f64::NAN;
        assert!(watch.is_new_at(&state, at(802.0)));
        assert!(!watch.is_new_at(&state, at(802.5)));
    }

    #[test]
    fn block_on_runs_ready_and_woken_futures() {
        assert_eq!(block_on(async { 7 }), 7);
        // A future that wakes itself once before completing.
        let mut polled = false;
        let value = block_on(std::future::poll_fn(move |cx| {
            if polled {
                std::task::Poll::Ready(42)
            } else {
                polled = true;
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
        }));
        assert_eq!(value, 42);
    }

    #[test]
    fn reading_missing_or_huge_files_fails_cleanly() {
        let missing = std::env::temp_dir().join("sprint-insight-definitely-missing.txt");
        assert!(matches!(read_text(&missing), FileOutcome::Failed(_)));
        let path =
            std::env::temp_dir().join(format!("sprint-insight-read-{}.gcode", std::process::id()));
        std::fs::write(&path, b"M92 E415\n\xff").expect("write");
        match read_text(&path) {
            FileOutcome::Loaded(p, text) => {
                assert_eq!(p, path);
                assert!(text.starts_with("M92 E415"));
            }
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }
}

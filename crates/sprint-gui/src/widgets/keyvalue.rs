//! [`KeyValueGrid`]: two-column info tables with dim labels, bright values
//! and click-to-copy.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Grid, Id, InnerResponse, Pos2, Rect,
    Response, Sense, Ui, WidgetInfo, WidgetType, vec2,
};

use crate::icons;
use crate::theme::{self, Palette, Tone, with_alpha};

use super::icon_font;

/// How long the "Copied" confirmation stays, in seconds.
const COPIED_SECS: f64 = 1.2;

/// A two-column table of labelled values (firmware info, statistics, link
/// details). Values are copied to the clipboard on click by default.
///
/// ```no_run
/// # use sprint_gui::{theme::Tone, widgets::KeyValueGrid};
/// # fn demo(ui: &mut egui::Ui) {
/// KeyValueGrid::new("firmware").show(ui, |kv| {
///     kv.row("Firmware", "Marlin 2.1.2.4");
///     kv.row_mono("UUID", "cede2a2f-41a2-4748-9b12-c55c62f367ff");
///     kv.row_tone("Endstops", "Open", Tone::Success);
///     kv.row_ui("Extruders", |ui| { ui.label("2"); });
/// });
/// # }
/// ```
#[must_use = "call `.show(ui, ...)`"]
pub struct KeyValueGrid {
    id: Id,
    key_width: Option<f32>,
    copyable: bool,
    row_spacing: f32,
}

impl KeyValueGrid {
    /// A grid with an id salt unique within the parent.
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        KeyValueGrid {
            id: Id::new(id_salt),
            key_width: None,
            copyable: true,
            row_spacing: 6.0,
        }
    }

    /// Minimum width of the label column.
    pub fn key_width(mut self, width: f32) -> Self {
        if let Some(width) = super::finite_clamped(width, 0.0, super::MAX_EXTENT) {
            self.key_width = Some(width);
        }
        self
    }

    /// Click a value to copy it (default `true`).
    pub fn copyable(mut self, copyable: bool) -> Self {
        self.copyable = copyable;
        self
    }

    /// Vertical gap between rows (default 6).
    pub fn row_spacing(mut self, spacing: f32) -> Self {
        if let Some(spacing) = super::finite_clamped(spacing, 0.0, 64.0) {
            self.row_spacing = spacing;
        }
        self
    }

    /// Show the rows added by `add_rows`.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        add_rows: impl FnOnce(&mut KeyValueRows<'_>) -> R,
    ) -> InnerResponse<R> {
        let p = theme::ui_palette(ui);
        // Grid ids derive from the parent's stable id, which sibling Uis
        // share; salt with the unique id so side-by-side grids differ.
        let mut grid = Grid::new(super::widget_id(ui, self.id))
            .num_columns(2)
            .spacing(vec2(16.0, self.row_spacing));
        if let Some(w) = self.key_width {
            grid = grid.min_col_width(w);
        }
        grid.show(ui, |ui| {
            let mut rows = KeyValueRows {
                ui,
                p,
                copyable: self.copyable,
                copied: None,
            };
            add_rows(&mut rows)
        })
    }
}

/// Row builder passed to [`KeyValueGrid::show`].
pub struct KeyValueRows<'u> {
    ui: &'u mut Ui,
    p: Palette,
    copyable: bool,
    copied: Option<String>,
}

impl KeyValueRows<'_> {
    /// A label and a text value.
    pub fn row(&mut self, key: &str, value: &str) -> Response {
        let font = FontId::proportional(13.0);
        let color = self.p.text;
        self.value_row(key, value, font, color)
    }

    /// A label and a monospace value (ids, G-code, hex).
    pub fn row_mono(&mut self, key: &str, value: &str) -> Response {
        let font = FontId::monospace(12.5);
        let color = self.p.text;
        self.value_row(key, value, font, color)
    }

    /// A label and a value in a semantic color.
    pub fn row_tone(&mut self, key: &str, value: &str, tone: Tone) -> Response {
        let color = if tone == Tone::Neutral {
            self.p.text
        } else {
            self.p.tone(tone)
        };
        self.value_row(key, value, FontId::proportional(13.0), color)
    }

    /// A label and a value in an explicit color.
    pub fn row_colored(&mut self, key: &str, value: &str, color: Color32) -> Response {
        self.value_row(key, value, FontId::proportional(13.0), color)
    }

    /// A label and custom content in the value column.
    pub fn row_ui<R>(&mut self, key: &str, add_value: impl FnOnce(&mut Ui) -> R) -> R {
        key_label(self.ui, &self.p, key);
        let inner = add_value(self.ui);
        self.ui.end_row();
        inner
    }

    /// The value copied by a click this frame, if any.
    pub fn copied(&self) -> Option<&str> {
        self.copied.as_deref()
    }

    /// The `Ui` of the grid (for anything the helpers do not cover; call
    /// `end_row` yourself).
    pub fn ui(&mut self) -> &mut Ui {
        self.ui
    }

    fn value_row(&mut self, key: &str, value: &str, font: FontId, color: Color32) -> Response {
        key_label(self.ui, &self.p, key);
        let response = value_cell(self.ui, &self.p, value, font, color, self.copyable);
        if response.clicked() && self.copyable && !value.is_empty() {
            self.copied = Some(value.to_owned());
        }
        self.ui.end_row();
        response
    }
}

fn key_label(ui: &mut Ui, p: &Palette, key: &str) {
    ui.label(egui::RichText::new(key).size(13.0).color(p.text_dim));
}

fn value_cell(
    ui: &mut Ui,
    p: &Palette,
    value: &str,
    font: FontId,
    color: Color32,
    copyable: bool,
) -> Response {
    let empty = value.trim().is_empty();
    let (text, color) = if empty {
        ("—".to_owned(), p.text_faint)
    } else {
        (value.to_owned(), color)
    };
    let icon_w = if copyable && !empty { 18.0 } else { 0.0 };
    let wrap = (ui.available_width() - icon_w).max(60.0);
    let galley = ui.painter().layout(text, font, color, wrap);
    let size = vec2(galley.size().x + icon_w, galley.size().y.max(18.0));
    let sense = if copyable && !empty {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), value));
    let copied_at = ui.data(|d| d.get_temp::<f64>(response.id));
    let now = ui.input(|i| i.time);
    if response.clicked() && copyable && !empty {
        ui.ctx().copy_text(value.to_owned());
        ui.data_mut(|d| d.insert_temp(response.id, now));
    }
    let show_copied = copied_at.is_some_and(|t| now - t < COPIED_SECS && now >= t)
        || (response.clicked() && copyable && !empty);
    if show_copied {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(COPIED_SECS));
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if response.hovered() && copyable && !empty {
            painter.rect_filled(
                rect.expand2(vec2(4.0, 1.0)),
                CornerRadius::same(theme::RADIUS_SM),
                with_alpha(p.text_dim, if p.dark { 0.10 } else { 0.08 }),
            );
        }
        let text_pos = Pos2::new(rect.min.x, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, color);
        if copyable && !empty && (response.hovered() || show_copied) {
            let icon_rect =
                Rect::from_min_max(Pos2::new(rect.max.x - icon_w, rect.min.y), rect.max);
            let (icon, icon_color) = if show_copied {
                (icons::CHECK, p.success)
            } else {
                (icons::COPY, p.text_faint)
            };
            painter.text(
                Pos2::new(icon_rect.center().x + 2.0, rect.min.y + 9.0),
                Align2::CENTER_CENTER,
                icon,
                icon_font(ui, 13.0),
                icon_color,
            );
        }
    }
    if copyable && !empty {
        let tip = if show_copied {
            "Copied"
        } else {
            "Click to copy"
        };
        response
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(tip)
    } else {
        response
    }
}

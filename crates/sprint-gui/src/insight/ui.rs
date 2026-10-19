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

/// The header of a tab: an icon tile, a title, an optional dim subtitle
/// and controls on the right.
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
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, title));
    let mut right_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::right_to_left(Align::Center)),
    );
    right_ui.spacing_mut().item_spacing.x = 6.0;
    right(&mut right_ui);
    let used = right_ui.min_rect();
    let text_end = if used.width() > 0.0 {
        used.min.x - 12.0
    } else {
        rect.max.x
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

//! The category list and the setting cards of the Tuning tab.

use egui::{Align2, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use sprint_proto::{ParamDef, SettingCategory, SettingDef};

use super::model::{Draft, Entry};
use crate::icons;
use crate::insight::ui as kit;
use crate::theme::{self, TextRole, Tone, with_alpha};
use crate::widgets::{Badge, Card, IconButton, NumericField, Toggle};

/// What the right-hand side shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Filter {
    /// Everything.
    #[default]
    All,
    /// One category.
    Category(SettingCategory),
    /// Only modified settings.
    Modified,
}

/// One row of the category list; returns whether it was clicked.
fn list_row(ui: &mut Ui, label: &str, count: usize, modified: usize, selected: bool) -> bool {
    let p = theme::ui_palette(ui);
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 30.0), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = CornerRadius::same(theme::RADIUS);
        if selected {
            painter.rect_filled(rect, radius, p.accent_soft);
            painter.rect_filled(
                Rect::from_min_size(
                    pos2(rect.min.x, rect.min.y + 7.0),
                    vec2(3.0, rect.height() - 14.0),
                ),
                CornerRadius::same(2),
                p.accent,
            );
        } else if response.hovered() {
            painter.rect_filled(rect, radius, p.raised);
        }
        let text_color = if selected { p.text } else { p.text_dim };
        let font = if selected {
            FontId::new(13.5, theme::font(ui, TextRole::Title).family)
        } else {
            FontId::proportional(13.5)
        };
        painter.text(
            pos2(rect.min.x + 12.0, rect.center().y),
            Align2::LEFT_CENTER,
            label,
            font,
            text_color,
        );
        let mut x = rect.max.x - 8.0;
        let count_text = count.to_string();
        let g = painter.layout_no_wrap(count_text, FontId::proportional(12.0), p.text_faint);
        painter.galley(
            pos2(x - g.size().x, rect.center().y - g.size().y / 2.0),
            g.clone(),
            p.text_faint,
        );
        x -= g.size().x + 8.0;
        if modified > 0 {
            painter.circle_filled(pos2(x - 3.0, rect.center().y), 3.5, p.accent);
        }
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// The category list on the left.
pub(super) fn category_list(ui: &mut Ui, draft: &Draft, filter: &mut Filter) {
    let total = draft.entries().len();
    let modified = draft.modified_count();
    ui.spacing_mut().item_spacing.y = 2.0;
    if list_row(ui, "All settings", total, modified, *filter == Filter::All) {
        *filter = Filter::All;
    }
    if modified > 0 && list_row(ui, "Modified", modified, 0, *filter == Filter::Modified) {
        *filter = Filter::Modified;
    }
    ui.add_space(6.0);
    kit::caption(ui, "CATEGORIES");
    ui.add_space(2.0);
    for (cat, count, changed) in draft.categories() {
        if list_row(
            ui,
            cat.name(),
            count,
            changed,
            *filter == Filter::Category(cat),
        ) {
            *filter = Filter::Category(cat);
        }
    }
}

/// Whether an entry is shown with the filter and search query.
pub(super) fn visible(entry: &Entry, filter: Filter, query: &str) -> bool {
    let in_filter = match filter {
        Filter::All => true,
        Filter::Category(cat) => entry.category() == cat,
        Filter::Modified => entry.is_modified(),
    };
    // A search looks through everything.
    (in_filter || !query.trim().is_empty()) && entry.matches(query)
}

/// Indices of the visible entries grouped by command code (catalog
/// settings) in order of first appearance; unknown commands come last as
/// one group with code `None`.
pub(super) fn groups(
    draft: &Draft,
    filter: Filter,
    query: &str,
) -> Vec<(Option<String>, Vec<usize>)> {
    let mut out: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    let mut unknown = Vec::new();
    for (i, e) in draft.entries().iter().enumerate() {
        if !visible(e, filter, query) {
            continue;
        }
        if e.raw.is_some() {
            unknown.push(i);
            continue;
        }
        let code = e.draft.code.clone();
        match out
            .iter_mut()
            .find(|(c, _)| c.as_deref() == Some(code.as_str()))
        {
            Some((_, v)) => v.push(i),
            None => out.push((Some(code), vec![i])),
        }
    }
    // Catalog order: related settings stay together (steps, feedrates,
    // accelerations, ...) whatever order the firmware reports them in.
    let rank = |code: &Option<String>| {
        code.as_deref()
            .and_then(|c| {
                sprint_proto::setting_defs()
                    .iter()
                    .position(|d| d.code == c)
            })
            .unwrap_or(usize::MAX)
    };
    out.sort_by_key(|(code, _)| rank(code));
    if !unknown.is_empty() {
        out.push((None, unknown));
    }
    out
}

/// A 0/1 parameter shown as a switch.
fn is_switch(param: &ParamDef) -> bool {
    !param.flag && param.min == 0.0 && param.max == 1.0 && param.decimals == 0
}

/// The card of one setting code with all its instances.
pub(super) fn setting_card(
    ui: &mut Ui,
    def: &'static SettingDef,
    draft: &mut Draft,
    indices: &[usize],
) {
    let modified = indices
        .iter()
        .any(|&i| draft.entries().get(i).is_some_and(Entry::is_modified));
    let subtitle = def.code;
    let mut revert = false;
    Card::new(def.name)
        .subtitle(subtitle)
        .id_salt(("setting", def.code))
        .show_with_header(
            ui,
            |ui| {
                ui.add(
                    IconButton::new(icons::INFO)
                        .ghost()
                        .small()
                        .tooltip(format!(
                            "{} ({})\n\n{}",
                            def.name, def.code, def.description
                        )),
                );
                if modified {
                    if ui
                        .add(
                            IconButton::new(icons::RESET)
                                .ghost()
                                .small()
                                .tooltip(format!("Revert {}", def.name)),
                        )
                        .clicked()
                    {
                        revert = true;
                    }
                    ui.add(Badge::new("MODIFIED").tone(Tone::Accent));
                }
            },
            |ui| {
                let many = indices.len() > 1;
                for (n, &i) in indices.iter().enumerate() {
                    let Some(entry) = draft.entries_mut().get_mut(i) else {
                        continue;
                    };
                    let label = entry.instance_label();
                    if many || !label.is_empty() {
                        if n > 0 {
                            ui.add_space(6.0);
                        }
                        instance_header(ui, &label, entry.is_modified());
                    }
                    ui.push_id(("instance", entry.key.clone()), |ui| {
                        instance_fields(ui, def, entry);
                    });
                }
            },
        );
    if revert {
        for &i in indices {
            if let Some(e) = draft.entries_mut().get_mut(i) {
                e.revert();
            }
        }
    }
}

fn instance_header(ui: &mut Ui, label: &str, modified: bool) {
    let p = theme::ui_palette(ui);
    let text = if label.is_empty() { "Default" } else { label };
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(text.to_uppercase())
                .font(theme::font(ui, TextRole::Section))
                .color(if modified { p.accent } else { p.text_dim }),
        );
    });
}

/// The fields of one instance, in one or two columns.
fn instance_fields(ui: &mut Ui, def: &'static SettingDef, entry: &mut Entry) {
    // Catalog parameters the printer reported (flags always: they are
    // on/off), then parameters the catalog does not know.
    let mut params: Vec<(char, Option<&'static ParamDef>)> = def
        .params
        .iter()
        .filter(|p| !def.is_selector(p.letter))
        .filter(|p| {
            p.flag
                || entry.draft.has(p.letter)
                || entry.original.as_ref().is_some_and(|o| o.has(p.letter))
        })
        .map(|p| (p.letter, Some(p)))
        .collect();
    for (letter, _) in &entry.draft.params {
        if def.param(*letter).is_none() && !params.iter().any(|(l, _)| l == letter) {
            params.push((*letter, None));
        }
    }
    if params.is_empty() {
        kit::caption(ui, "No values reported.");
        return;
    }
    let width = ui.available_width();
    let cols = if width >= 600.0 { 2 } else { 1 };
    let gap = 16.0;
    let col_w = (width - gap * (cols as f32 - 1.0)) / cols as f32;
    let field_w = 128.0;
    let label_w = (col_w - field_w - 24.0 - 12.0).clamp(60.0, 220.0);
    for row in params.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for &(letter, param) in row {
                ui.allocate_ui_with_layout(
                    vec2(col_w, 28.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(col_w);
                        param_field(ui, def, entry, letter, param, label_w, field_w);
                    },
                );
            }
        });
    }
}

fn param_field(
    ui: &mut Ui,
    def: &'static SettingDef,
    entry: &mut Entry,
    letter: char,
    param: Option<&'static ParamDef>,
    label_w: f32,
    field_w: f32,
) {
    let original = entry.original_value(letter);
    let hint = format!(
        "{} {letter}{}",
        def.code,
        param.map_or(String::new(), |p| if p.unit.is_empty() {
            String::new()
        } else {
            format!(" · {}", p.unit)
        })
    );
    match param {
        Some(p) if p.flag => {
            let mut on = entry.draft.has(letter);
            let was = entry.original.as_ref().is_some_and(|o| o.has(letter));
            let response = flag_row(ui, p.label, &mut on, was, label_w).on_hover_text(&hint);
            if response.changed() {
                if on {
                    entry.draft.set(letter, f64::NAN);
                } else {
                    entry.draft.remove(letter);
                }
            }
        }
        Some(p) if is_switch(p) => {
            let Some(value) = entry.value_mut(letter) else {
                return;
            };
            let mut on = value.is_finite() && *value != 0.0;
            let was = original.is_some_and(|v| v.is_finite() && v != 0.0);
            let response = flag_row(ui, p.label, &mut on, was, label_w).on_hover_text(&hint);
            if response.changed() {
                *value = if on { 1.0 } else { 0.0 };
            }
        }
        Some(p) => {
            if let Some(value) = entry.value_mut(letter) {
                ui.add(
                    NumericField::from_param(p, value)
                        .original_opt(original)
                        .label_width(label_w)
                        .field_width(field_w)
                        .hint(&hint),
                );
            }
        }
        None => {
            let label = letter.to_string();
            if let Some(value) = entry.value_mut(letter) {
                ui.add(
                    NumericField::new(value)
                        .label(&label)
                        .decimals(3)
                        .original_opt(original)
                        .label_width(label_w)
                        .field_width(field_w)
                        .hint(&hint),
                );
            }
        }
    }
}

/// A label column (with the modified dot) and a switch.
fn flag_row(
    ui: &mut Ui,
    label: &str,
    on: &mut bool,
    original: bool,
    label_w: f32,
) -> egui::Response {
    let p = theme::ui_palette(ui);
    let modified = *on != original;
    let (rect, _) = ui.allocate_exact_size(vec2(label_w, 26.0), Sense::hover());
    if ui.is_rect_visible(rect) {
        if modified {
            ui.painter()
                .circle_filled(pos2(rect.min.x + 3.5, rect.center().y), 3.0, p.accent);
        }
        let color = if modified { p.text } else { p.text_dim };
        let g = kit::truncated(ui, label, FontId::proportional(13.0), color, label_w - 12.0);
        ui.painter().galley(
            pos2(rect.min.x + 12.0, rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
    }
    let response = ui.add(Toggle::new(on));
    let state = *on;
    // The switch has no visible label of its own: name it for screen
    // readers (and tests) after the setting.
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), state, label)
    });
    response
}

/// The card of commands the catalog does not know, edited as raw G-code.
pub(super) fn raw_card(ui: &mut Ui, draft: &mut Draft, indices: &[usize]) {
    let p = theme::ui_palette(ui);
    Card::new("Other reported settings")
        .subtitle("raw G-code")
        .id_salt("raw-settings")
        .show_with_header(
            ui,
            |ui| {
                ui.add(
                    IconButton::new(icons::INFO).ghost().small().tooltip(
                        "Commands the firmware reported that Sprint has no description for. Edit them as G-code; they are sent as written.",
                    ),
                );
            },
            |ui| {
                for &i in indices {
                    let Some(entry) = draft.entries_mut().get_mut(i) else {
                        continue;
                    };
                    let modified = entry.is_modified();
                    let invalid = entry.is_invalid();
                    let original = entry
                        .original
                        .as_ref()
                        .map(sprint_proto::SettingCommand::to_gcode);
                    let key = entry.key.clone();
                    let Some(text) = entry.raw.as_mut() else {
                        continue;
                    };
                    let mut reset = false;
                    ui.push_id(("raw", key.clone()), |ui| {
                        ui.horizontal(|ui| {
                            let (dot, _) = ui.allocate_exact_size(vec2(8.0, 26.0), Sense::hover());
                            if modified {
                                ui.painter().circle_filled(dot.center(), 3.0, p.accent);
                            }
                            let width = (ui.available_width() - 32.0).max(80.0);
                            ui.scope(|ui| {
                                let stroke = if invalid {
                                    p.danger
                                } else if modified {
                                    with_alpha(p.accent, 0.75)
                                } else {
                                    p.border
                                };
                                ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(1.0, stroke);
                                ui.visuals_mut().widgets.hovered.bg_stroke = Stroke::new(1.0, stroke);
                                ui.add(
                                    egui::TextEdit::singleline(text)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(width)
                                        .hint_text(original.as_deref().unwrap_or("G-code")),
                                )
                                .on_hover_text(format!("Reported as {key}"));
                            });
                            if modified
                                && ui
                                    .add(
                                        IconButton::new(icons::RESET)
                                            .ghost()
                                            .small()
                                            .tooltip(format!("Reset to {}", original.as_deref().unwrap_or(""))),
                                    )
                                    .clicked()
                            {
                                reset = true;
                            }
                        });
                        if invalid {
                            ui.label(
                                RichText::new("Not a valid setting command: it will not be applied.")
                                    .size(12.0)
                                    .color(p.danger),
                            );
                        }
                    });
                    if reset {
                        entry.revert();
                    }
                }
            },
        );
}
